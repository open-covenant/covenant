#!/usr/bin/env python3
"""Guest-initiated relay worker; quote identity and SSH encryption stay end to end."""
import copy
import http.client
import json
import re
import socket
import sys
import threading
import time
import uuid

from relay_client import pump_stream, reconcile
from transport_protocol import (
    MAX_OPERATIONS, MAX_STREAM_SECONDS, HttpTransport, ProtocolError, encoded,
    endpoint_origin, fields, fingerprint, hex_string, integer, now_ms, require,
    uuid_string,
)

MAX_IDENTITY_BYTES = 256 * 1024


def diagnostic(event, error):
    """Log bounded classifications only, never provider values or credentials."""
    name = type(error).__name__
    detail = {"event": event,
              "error_class": name if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]{0,79}", name) else "Exception"}
    if isinstance(error, ProtocolError) and re.fullmatch(r"[a-z_]{1,80}", error.code):
        detail["error_code"] = error.code
    print(json.dumps(detail, sort_keys=True), file=sys.stderr, flush=True)


class OutboundWorker:
    def __init__(self, endpoint, session_id, evidence_provider, expires_at_ms,
                 ssh_address=("ssh", 1022), transport=None, ssh_connector=None):
        _, self.relay_host, _ = endpoint_origin(endpoint)
        self.session_id = uuid_string(session_id)
        self.evidence = evidence_provider
        require(callable(getattr(evidence_provider, "check_stream", None)), "stream_guard_required")
        self.expires_at_ms = integer(expires_at_ms, now_ms() + 1, now_ms() + 3_600_000)
        self.deadline = time.monotonic() + (expires_at_ms - now_ms()) / 1000
        require(ssh_address == ("ssh", 1022), "fixed_ssh_service_required", 400)
        self.ssh_address = ssh_address
        # Constructor-only test seam; the measured deployment supplies neither
        # an alternate connector nor an alternate HTTP transport.
        self.ssh_connector = ssh_connector or socket.create_connection
        self.transport = transport or HttpTransport(endpoint, session_id)
        self.registration_id = str(uuid.uuid4())
        self.guest_token = None
        self.instance_id = None
        self.results = {}
        self.stream_socket = None
        self.stream_id = None
        self.stream_started = False
        self.stream_identity = None
        self.quote_count = 0

    def live(self):
        require(now_ms() < self.expires_at_ms and time.monotonic() < self.deadline,
                "lease_expired", 410)

    def identity(self):
        self.live()
        identity = self.evidence.identity()
        require(type(identity) is dict and type(identity.get("guest_info")) is dict,
                "invalid_guest_identity")
        instance = hex_string(identity["guest_info"].get("instance_id"), 40)
        require(self.instance_id in (None, instance), "guest_instance_changed")
        require(identity.get("host") == self.relay_host and identity.get("port") == 443
                and identity.get("username") == "covenant"
                and identity.get("transport") == {"kind": "outbound_relay"},
                "wrong_relay_scope")
        # Real dstack Info includes its TCB/event data and can exceed 32 KiB.
        # Preserve it intact, below both the 512 KiB result and 768 KiB HTTP caps.
        require(len(encoded(identity)) <= MAX_IDENTITY_BYTES, "identity_too_large")
        return identity

    def register(self):
        # Registration is only untrusted routing. A buyer can inspect evidence
        # before a Cloud/verified-quote instance pin; opening SSH requires it.
        # Once registered, a new identity or session is forbidden.
        registration_deadline = min(self.deadline, time.monotonic() + 180)
        last_notice = float("-inf")
        while time.monotonic() < registration_deadline:
            self.live()
            try:
                identity = self.identity()
            except (OSError, http.client.HTTPException) as error:
                if time.monotonic() - last_notice >= 10:
                    diagnostic("outbound_registration_identity_failed", error)
                    last_notice = time.monotonic()
                time.sleep(0.25)
                continue
            except Exception as error:
                # Guarded SSH readiness has already been awaited by the main
                # adapter. Malformed/binding/size failures must not look like
                # another three-minute wait for a network connection.
                diagnostic("outbound_registration_identity_failed", error)
                raise
            instance = identity["guest_info"]["instance_id"]
            if self.instance_id is None:
                self.instance_id = instance
            require(instance == self.instance_id, "guest_instance_changed")
            arguments = {"registration_id": self.registration_id, "instance_id": instance}
            try:
                result = reconcile(self.transport, "guest_register", arguments, registration_deadline)
            except Exception as error:
                if time.monotonic() - last_notice >= 10:
                    diagnostic("outbound_registration_request_failed", error)
                    last_notice = time.monotonic()
                if not isinstance(error, ProtocolError) or error.code != "instance_not_pinned":
                    raise
                time.sleep(0.5)
                continue
            fields(result, {"guest_token", "expires_at_ms", "instance_pinned"})
            require(type(result["instance_pinned"]) is bool, "invalid_pin_status")
            self.guest_token = hex_string(result["guest_token"], 64)
            require(result["expires_at_ms"] == self.expires_at_ms, "relay_expiry_changed")
            return
        raise ProtocolError("guest_registration_timeout", 504)

    def execute(self, operation):
        fields(operation, {"request_id", "kind", "payload"})
        request_id = uuid_string(operation["request_id"])
        digest = fingerprint(operation)
        if request_id in self.results:
            old_digest, result = self.results[request_id]
            require(old_digest == digest, "operation_id_collision")
            return copy.deepcopy(result)
        require(len(self.results) < MAX_OPERATIONS, "operation_budget_exhausted", 429)
        # An operation enters the spent set before any quote/socket action.
        # Exceptions replace the pending record with a permanent failed result.
        self.results[request_id] = (digest, {"ok": False, "error": "operation_outcome_unknown"})
        try:
            self.live()
            kind, payload = operation["kind"], operation["payload"]
            if kind == "identity":
                fields(payload, set())
                value = self.identity()
            elif kind == "quote":
                require(type(payload) is dict and payload.get("challenge_id") == request_id,
                        "quote_request_id_differs")
                require(self.quote_count < 8, "quote_budget_exhausted")
                self.quote_count += 1
                value = self.evidence.collect(payload)
                require(type(value) is dict and value.get("challenge") == payload,
                        "quote_challenge_changed")
                require(value.get("identity") == self.identity(), "quote_identity_changed")
            elif kind == "open_stream":
                fields(payload, {"stream_id", "challenge_id"})
                stream_id = uuid_string(payload["stream_id"])
                challenge_id = uuid_string(payload["challenge_id"])
                require(not self.stream_started and self.stream_socket is None, "only_one_stream_allowed")
                quoted = self.results.get(challenge_id)
                require(quoted is not None and quoted[1].get("ok") is True, "quote_required_before_stream")
                envelope = quoted[1]["value"]
                require(type(envelope) is dict and type(envelope.get("challenge")) is dict,
                        "quote_required_before_stream")
                require(envelope["challenge"]["challenge_id"] == challenge_id
                        and now_ms() < envelope["challenge"]["expires_at_ms"], "quote_expired")
                require(envelope["identity"] == self.identity(), "quoted_identity_changed")
                self.evidence.check_stream(envelope["identity"])
                self.stream_started = True
                self.stream_socket = self.ssh_connector(self.ssh_address, timeout=5)
                self.stream_id = stream_id
                self.stream_identity = copy.deepcopy(envelope["identity"])
                value = {"stream_id": stream_id}
            else:
                raise ProtocolError("unknown_control", 400)
            result = {"ok": True, "value": value}
            require(len(encoded(result)) <= 512 * 1024, "operation_result_too_large", 413)
        except Exception as error:
            diagnostic("outbound_operation_failed", error)
            result = {"ok": False, "error": "guest_operation_failed"}
        self.results[request_id] = (digest, result)
        return copy.deepcopy(result)

    def run(self):
        try:
            self.register()
            while time.monotonic() < self.deadline:
                self.live()
                result = reconcile(self.transport, "guest_poll", {}, self.deadline, self.guest_token)
                fields(result, {"operation"})
                operation = result["operation"]
                if operation is None:
                    continue
                reply = self.execute(operation)
                stored = reconcile(self.transport, "guest_reply",
                                   {"request_id": operation["request_id"], "result": reply},
                                   self.deadline, self.guest_token)
                require(stored == {"request_id": operation["request_id"], "stored": True},
                        "wrong_reply_ack", 502)
                if self.stream_socket is not None:
                    stream = self.stream_socket
                    stop_guard, guard_failed = threading.Event(), threading.Event()

                    def guard_stream():
                        while not stop_guard.is_set():
                            try:
                                self.live()
                                self.evidence.check_stream(self.stream_identity)
                            except Exception as error:
                                guard_failed.set()
                                print(json.dumps({"event": "outbound_stream_guard_failed",
                                                  "error_class": type(error).__name__}, sort_keys=True),
                                      file=sys.stderr, flush=True)
                                try:
                                    stream.shutdown(socket.SHUT_RDWR)
                                except OSError:
                                    pass
                                return
                            stop_guard.wait(0.25)

                    watchdog = threading.Thread(target=guard_stream, daemon=True)
                    watchdog.start()
                    try:
                        try:
                            pump_stream(self.transport, "guest_exchange", self.stream_id,
                                        stream.fileno(), stream.fileno(),
                                        min(self.deadline, time.monotonic() + MAX_STREAM_SECONDS),
                                        self.guest_token, lambda: stream.shutdown(socket.SHUT_WR))
                        except ProtocolError as error:
                            if error.code != "stream_closed":
                                raise
                        require(not guard_failed.is_set(), "stream_guard_failed", 410)
                    finally:
                        stop_guard.set()
                        watchdog.join(timeout=1)
                        stream.close()
                        self.stream_socket = None
                    # A closed stream is final. Recovery requires a new lease,
                    # fresh VM/disk, challenge, verification, and connection.
                    return
        finally:
            if self.stream_socket is not None:
                self.stream_socket.close()
                self.stream_socket = None
