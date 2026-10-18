#!/usr/bin/env python3
"""Cloudflare email adapter for the rental canary's stdin alert contract.

Input: {"schema": 1, "type": "rental_canary_missing_or_unsuccessful" or
"rental_canary_recovered", "state": <sanitized public monitor snapshot>}.
Exit 0 means Cloudflare explicitly delivered or queued alert@opencovenant.org;
every other outcome exits 1. No provider response or input body is printed.

The runner owns durable delivery bookkeeping under monitor.lock. UUIDv5 event
IDs bind the fixed canary identity, event type and six-hour UTC window, so
retries within that window keep the same Idempotency-Key even as state changes.
A new unsuccessful window intentionally produces a new reminder. This adapter
does not claim exactly-once delivery beyond the provider's idempotency contract.

Required environment: COVENANT_CANARY_ALERT_CLOUDFLARE_API_TOKEN,
COVENANT_CANARY_ALERT_CLOUDFLARE_ACCOUNT_ID, COVENANT_CANARY_ALERT_FROM,
COVENANT_CANARY_ALERT_TO (must be alert@opencovenant.org). Only an explicit
invocation sends mail; import and tests are offline. Like the runner, requires
Unix for the five-second whole-request deadline.
"""
from __future__ import annotations

import contextlib
import http.client
import json
import os
import re
import signal
import ssl
import sys
import uuid

HOST = "api.cloudflare.com"
ORIGIN = "https://" + HOST
RECIPIENT = "alert@opencovenant.org"
WINDOW_SECONDS = 6 * 60 * 60
MAX_INPUT_BYTES = 128 * 1024
MAX_RESPONSE_BYTES = 16 * 1024
TIMEOUT_SECONDS = 5
EVENT_BASE = "https://opencovenant.org/compute-rental-canary/alerts/v1/"
MISSING = "rental_canary_missing_or_unsuccessful"
RECOVERED = "rental_canary_recovered"


class Refusal(Exception):
    """Fixed diagnostic strings only; never include input or provider data."""


def require(condition, message):
    if not condition:
        raise Refusal(message)


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate JSON field")
        result[key] = value
    return result


def parse_json(blob):
    def invalid_constant(_value):
        raise Refusal("invalid JSON number")

    try:
        return json.loads(blob.decode("utf-8"), object_pairs_hook=unique_object,
                          parse_constant=invalid_constant)
    except (ValueError, UnicodeError, RecursionError):
        raise Refusal("invalid JSON") from None


def natural(value):
    return type(value) is int and 0 <= value <= 2**53 - 1


def load_config(environ):
    token = environ.get("COVENANT_CANARY_ALERT_CLOUDFLARE_API_TOKEN")
    account = environ.get("COVENANT_CANARY_ALERT_CLOUDFLARE_ACCOUNT_ID")
    sender = environ.get("COVENANT_CANARY_ALERT_FROM")
    recipient = environ.get("COVENANT_CANARY_ALERT_TO")
    require(isinstance(token, str) and 0 < len(token) <= 4096
            and all(33 <= ord(char) <= 126 for char in token), "invalid alert token")
    require(isinstance(account, str) and re.fullmatch(r"[0-9a-fA-F]{32}", account),
            "invalid alert account")
    # A single ASCII mailbox, not an arbitrary RFC5322 header or recipient list.
    require(isinstance(sender, str) and len(sender) <= 254
            and re.fullmatch(r"[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+@[A-Za-z0-9](?:[A-Za-z0-9.-]*[A-Za-z0-9])?", sender),
            "invalid alert sender")
    require(recipient == RECIPIENT, "invalid alert recipient")
    return {"token": token, "account": account, "from": sender, "to": recipient}


def email_event(payload, config):
    require(isinstance(payload, dict) and type(payload.get("schema")) is int
            and payload["schema"] == 1, "invalid alert schema")
    kind = payload.get("type")
    require(kind in (MISSING, RECOVERED), "invalid alert type")
    state = payload.get("state")
    require(isinstance(state, dict) and type(state.get("schema")) is int
            and state["schema"] == 1 and type(state.get("cadence_seconds")) is int
            and state["cadence_seconds"] == WINDOW_SECONDS, "invalid monitor schema")
    generated = state.get("generated_at_unix")
    require(natural(generated), "invalid monitor timestamp")
    monitor = state.get("monitor")
    require(isinstance(monitor, dict), "invalid monitor state")
    count = monitor.get("consecutive_unsuccessful_windows")
    require(natural(count) and type(monitor.get("alert_due")) is bool,
            "invalid monitor counters")
    if kind == MISSING:
        require(state.get("enabled") is True and monitor["alert_due"] and count >= 3,
                "unconfirmed rental incident")
        subject = "Covenant Compute: rental checks missing or unsuccessful"
        description = ("At least three consecutive six-hour windows have no successful "
                       "end-to-end rental check. This can include missed runs, unavailable "
                       "capacity or a failed check; it does not establish lost funds or "
                       "a network-wide outage.")
    else:
        last_success = monitor.get("last_success_at_unix")
        require(not monitor["alert_due"] and count == 0 and natural(last_success)
                and 0 < last_success <= generated, "unconfirmed rental recovery")
        subject = "Covenant Compute: rental checks recovered"
        description = "A complete rental check succeeded and reset the unsuccessful-window count."
    window = generated // WINDOW_SECONDS
    # Do not hash the mutable snapshot, which changes during each retry.
    event_id = str(uuid.uuid5(uuid.NAMESPACE_URL, EVENT_BASE + kind + "/" + str(window)))
    body = (subject + "\n\n" + description + "\n\n"
            + "Consecutive unsuccessful windows: " + str(count) + "\n"
            + "Six-hour UTC window: " + str(window) + "\n"
            + "Event ID: " + event_id + "\n\n"
            + "Status: https://opencovenant.org/compute-status\n")
    return event_id, {"from": config["from"], "to": config["to"], "subject": subject,
                      "text": body, "headers": {"X-Covenant-Event-ID": event_id}}


def cloudflare_accepted(value, recipient=RECIPIENT):
    """Match the coordinator observer's strict per-recipient acceptance rule."""
    if not isinstance(value, dict) or value.get("success") is not True or value.get("errors") != []:
        return False
    result = value.get("result")
    if not isinstance(result, dict):
        return False
    for field in ("delivered", "queued", "permanent_bounces"):
        items = result.get(field)
        if not isinstance(items, list) or not all(isinstance(item, str) for item in items):
            return False
    if recipient in result["permanent_bounces"]:
        return False
    if "suppressed_recipients" in result:
        items = result["suppressed_recipients"]
        if not isinstance(items, list) or not all(isinstance(item, str) for item in items) or recipient in items:
            return False
    return recipient in result["delivered"] or recipient in result["queued"]


@contextlib.contextmanager
def request_deadline():
    # Socket timeouts alone reset for each received byte; SIGALRM also bounds
    # DNS, TLS, slow response headers and a slowly trickling response body.
    def expired(_signum, _frame):
        raise Refusal("alert request timed out")

    previous = signal.signal(signal.SIGALRM, expired)
    signal.setitimer(signal.ITIMER_REAL, TIMEOUT_SECONDS)
    try:
        yield
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous)


def deliver(config, event_id, email):
    # http.client never consults proxy environment variables and never follows
    # redirects. Neither host nor path may be supplied by the stdin payload.
    path = "/client/v4/accounts/" + config["account"] + "/email/sending/send"
    encoded = json.dumps(email, separators=(",", ":"), ensure_ascii=True).encode("utf-8")
    connection = None
    try:
        with request_deadline():
            connection = http.client.HTTPSConnection(HOST, timeout=TIMEOUT_SECONDS,
                                                    context=ssl.create_default_context())
            connection.request("POST", path, body=encoded, headers={
                "Authorization": "Bearer " + config["token"],
                "Idempotency-Key": event_id,
                "Content-Type": "application/json", "Accept": "application/json",
                "Accept-Encoding": "identity",
            })
            response = connection.getresponse()
            require(200 <= response.status < 300, "alert request rejected")
            lengths = response.headers.get_all("Content-Length", [])
            encodings = response.headers.get_all("Content-Encoding", [])
            require(len(lengths) <= 1 and len(encodings) <= 1, "ambiguous response headers")
            require(not encodings or encodings[0].lower() == "identity", "encoded alert response")
            length = None
            if lengths:
                require(re.fullmatch(r"[0-9]{1,8}", lengths[0]), "invalid response length")
                length = int(lengths[0])
                require(length <= MAX_RESPONSE_BYTES and not response.headers.get("Transfer-Encoding"),
                        "invalid response length")
            body = bytearray()
            while True:
                chunk = response.read1(MAX_RESPONSE_BYTES + 1 - len(body))
                if not chunk:
                    break
                body.extend(chunk)
                require(len(body) <= MAX_RESPONSE_BYTES, "alert response too large")
            require(length is None or len(body) == length, "truncated alert response")
            require(cloudflare_accepted(parse_json(bytes(body)), config["to"]), "alert was not accepted")
    except (OSError, http.client.HTTPException, ValueError):
        raise Refusal("alert request failed") from None
    finally:
        if connection is not None:
            connection.close()


def main():
    try:
        require(len(sys.argv) == 1, "unexpected alert arguments")
        config = load_config(os.environ)
        blob = sys.stdin.buffer.read(MAX_INPUT_BYTES + 1)
        require(len(blob) <= MAX_INPUT_BYTES, "alert input too large")
        event_id, email = email_event(parse_json(blob), config)
        deliver(config, event_id, email)
        return 0
    except (Refusal, OSError, ValueError, TypeError, KeyError):
        # Neither potentially sensitive exception text nor provider body is logged.
        print("canary alert not accepted", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
