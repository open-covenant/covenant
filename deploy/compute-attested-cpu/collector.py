#!/usr/bin/env python3
"""Bounded evidence collection only; this module never grants hardware trust."""
import base64
import contextlib
import hashlib
import hmac
import http.client
import http.server
import json
import os
from pathlib import Path
import re
import socket
import sqlite3
import time
import uuid

OS_IMAGE_HASH = "ec43cb8c5e04b6bb6c53b33b16076a01d83d01f7f4b703f2f78a0d947d6247e6"
DOMAIN = b"covenant.compute.hardware.guest.v1\0"
MAX_REQUEST = 16 * 1024
MAX_RESPONSE = 512 * 1024
MAX_SAFE_INT = (1 << 53) - 1
POLICY_FIELDS = {
    "schema_version", "job_id", "operator_pubkey_b58", "buyer_pubkey_b58",
    "lease_generation", "lease_expires_at_ms", "gateway_domain",
    "buyer_ssh_public_key", "max_challenges",
}
SCOPE_FIELDS = {
    "job_id", "operator_pubkey_b58", "buyer_pubkey_b58", "lease_generation",
    "host", "port", "username", "guest_channel_key", "workload_sha256",
    "lease_expires_at_ms", "transport",
}
CHALLENGE_FIELDS = {
    "schema_version", "challenge_id", "nonce_hex", "issued_at_ms",
    "expires_at_ms", "scope",
}


class Rejected(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise Rejected(message)


def exact_fields(value, fields):
    require(type(value) is dict and set(value) == fields, "unexpected object fields")


def integer(value, low=0, high=MAX_SAFE_INT):
    require(type(value) is int and low <= value <= high, "invalid integer")
    return value


def hex_value(value, size):
    require(
        type(value) is str and re.fullmatch(r"[0-9a-f]{%d}" % (size * 2), value),
        "invalid lowercase hex",
    )
    return value


def uuid_value(value):
    require(type(value) is str, "invalid UUID")
    try:
        parsed = uuid.UUID(value)
    except ValueError as error:
        raise Rejected("invalid UUID") from error
    require(parsed.int != 0 and str(parsed) == value, "noncanonical UUID")


def public_identity(value):
    alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
    require(type(value) is str and 32 <= len(value) <= 44, "invalid identity")
    number = 0
    for character in value:
        require(character in alphabet, "invalid identity")
        number = number * 58 + alphabet.index(character)
    leading = len(value) - len(value.lstrip("1"))
    require(leading + (number.bit_length() + 7) // 8 == 32, "invalid identity")


def ssh_key(value):
    """Only the generated Ed25519 key type is supported in this initial image."""
    require(type(value) is str and len(value) <= 256, "invalid SSH key")
    fields = value.strip().split()
    require(len(fields) == 2 and fields[0] == "ssh-ed25519", "invalid SSH key")
    try:
        wire = base64.b64decode(fields[1], validate=True)
    except (ValueError, TypeError) as error:
        raise Rejected("invalid SSH key") from error
    require(
        wire[:19] == b"\0\0\0\x0bssh-ed25519\0\0\0\x20" and len(wire) == 51
        and base64.b64encode(wire).decode() == fields[1],
        "invalid SSH key wire format",
    )
    return {"algorithm": fields[0], "key_base64": fields[1]}


def strict_json(raw):
    def pairs(items):
        result = {}
        for key, value in items:
            require(key not in result, "duplicate JSON field")
            result[key] = value
        return result

    def invalid_number(_):
        raise Rejected("noninteger JSON number")

    try:
        return json.loads(
            raw, object_pairs_hook=pairs, parse_float=invalid_number,
            parse_constant=invalid_number,
        )
    except (UnicodeError, json.JSONDecodeError, RecursionError) as error:
        raise Rejected("invalid JSON") from error


def canonical(value):
    """JCS for this deliberately restricted ASCII and safe-integer schema."""
    def validate(item):
        if type(item) is dict:
            for key, content in item.items():
                validate(key)
                validate(content)
        elif type(item) is list:
            for content in item:
                validate(content)
        elif type(item) is str:
            require(item.isascii() and all(ord(c) >= 32 for c in item),
                    "noncanonical string")
        else:
            integer(item)
    validate(value)
    return json.dumps(value, sort_keys=True, separators=(",", ":"),
                      ensure_ascii=False).encode()


def load_policy(value, now_ms):
    ephemeral = type(value) is dict and value.get("profile") == "phala_ephemeral_dstack061"
    fields = (POLICY_FIELDS - {"gateway_domain"}) | {"profile", "relay_endpoint", "relay_session_id"} if ephemeral else POLICY_FIELDS
    exact_fields(value, fields)
    integer(value["schema_version"], 1, 1)
    uuid_value(value["job_id"])
    public_identity(value["operator_pubkey_b58"])
    public_identity(value["buyer_pubkey_b58"])
    require(type(value["lease_generation"]) is str and re.fullmatch(
        r"[A-Za-z0-9_.-]{1,128}", value["lease_generation"]), "invalid generation")
    integer(value["lease_expires_at_ms"], now_ms + 1)
    integer(value["max_challenges"], 1, 128)
    if ephemeral:
        import ipaddress
        from urllib.parse import urlsplit
        endpoint = value["relay_endpoint"]
        require(type(endpoint) is str, "invalid relay endpoint")
        parsed = urlsplit(endpoint)
        require(parsed.scheme == "https" and parsed.netloc == parsed.hostname
                and not parsed.path and not parsed.query and not parsed.fragment
                and parsed.hostname is not None and "." in parsed.hostname
                and re.fullmatch(r"(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?", parsed.hostname),
                "invalid canonical relay origin")
        try:
            ipaddress.ip_address(parsed.hostname)
        except ValueError:
            pass
        else:
            raise Rejected("relay requires a DNS hostname")
        require(value["relay_session_id"] == value["job_id"], "relay session differs from job")
        require(value["lease_generation"] == "phala-ephemeral-" + value["job_id"] + "-1",
                "invalid ephemeral generation")
    domain = value.get("gateway_domain", "test.phala.network")
    require(type(domain) is str and len(domain) <= 180 and re.fullmatch(
        r"(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+phala\.network", domain),
        "invalid gateway domain")
    ssh_key(value["buyer_ssh_public_key"])
    canonical(value)
    return value


def environment_policy():
    encoded = os.environ.get("COVENANT_LEASE_POLICY_B64", "")
    require(0 < len(encoded) <= MAX_REQUEST, "missing policy")
    try:
        raw = base64.b64decode(encoded, validate=True)
    except ValueError as error:
        raise Rejected("invalid policy encoding") from error
    return load_policy(strict_json(raw), int(time.time() * 1000))


class Dstack:
    def __init__(self, path="/var/run/dstack.sock"):
        self.path = path

    def request(self, method, payload=None):
        require(method in {"Info", "GetQuote"}, "unsupported guest RPC")
        connection = http.client.HTTPConnection("dstack", timeout=8)
        transport = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        transport.settimeout(8)
        try:
            transport.connect(self.path)
            connection.sock = transport
            if payload is None:
                connection.request("GET", "/" + method)
            else:
                connection.request("POST", "/" + method, json.dumps(payload),
                                   {"Content-Type": "application/json"})
            response = connection.getresponse()
            require(response.status == 200, "guest RPC failed")
            raw = response.read(MAX_RESPONSE + 1)
            require(len(raw) <= MAX_RESPONSE, "oversized guest response")
            value = strict_json(raw)
            require(type(value) is dict, "invalid guest response")
            return value
        finally:
            connection.close()
            transport.close()


class ReplayStore:
    """Persist consumption before requesting a quote; failed requests stay spent."""
    def __init__(self, path, job_id):
        self.path = str(path)
        self.job_id = job_id
        os.umask(0o077)
        with self.connect() as database:
            database.execute("CREATE TABLE IF NOT EXISTS state "
                             "(singleton INTEGER PRIMARY KEY CHECK(singleton=1),"
                             "job_id TEXT NOT NULL, last_ms INTEGER NOT NULL)")
            database.execute("CREATE TABLE IF NOT EXISTS spent "
                             "(challenge_id TEXT PRIMARY KEY, nonce TEXT UNIQUE NOT NULL)")
            row = database.execute("SELECT job_id FROM state WHERE singleton=1").fetchone()
            if row is None:
                database.execute("INSERT INTO state VALUES(1,?,0)", (job_id,))
            else:
                require(row[0] == job_id, "journal belongs to another lease")
        os.chmod(self.path, 0o600)

    @contextlib.contextmanager
    def connect(self):
        database = sqlite3.connect(self.path, timeout=2)
        try:
            database.execute("PRAGMA synchronous=FULL")
            database.execute("PRAGMA journal_mode=DELETE")
            with database:
                yield database
        finally:
            database.close()

    def consume(self, challenge, now_ms, maximum):
        with self.connect() as database:
            database.execute("BEGIN IMMEDIATE")
            row = database.execute("SELECT job_id,last_ms FROM state WHERE singleton=1").fetchone()
            require(row is not None and row[0] == self.job_id, "invalid journal")
            require(now_ms >= row[1], "clock rollback")
            count = database.execute("SELECT count(*) FROM spent").fetchone()[0]
            require(count < maximum, "challenge budget exhausted")
            try:
                database.execute("INSERT INTO spent VALUES(?,?)",
                                 (challenge["challenge_id"], challenge["nonce_hex"]))
            except sqlite3.IntegrityError as error:
                raise Rejected("challenge already consumed") from error
            database.execute("UPDATE state SET last_ms=? WHERE singleton=1", (now_ms,))


class Collector:
    def __init__(self, policy, key_path, guest, journal, clock=None, memory_guard=None):
        from memory_guard import RuntimeGuard
        self.policy, self.key_path = policy, Path(key_path)
        self.guest, self.journal = guest, journal
        self.clock = clock or (lambda: int(time.time() * 1000))
        self.memory_guard = memory_guard if memory_guard is not None else RuntimeGuard()

    def identity(self):
        require(self.clock() < self.policy["lease_expires_at_ms"], "expired lease")
        # The directory is writable only by the measured SSH master. It contains
        # no private key; the collector mounts it read-only.
        with self.key_path.open("rb") as public_file:
            raw_key = public_file.read(257)
        require(len(raw_key) <= 256, "oversized SSH key")
        try:
            key = ssh_key(raw_key.decode("ascii"))
        except UnicodeError as error:
            raise Rejected("invalid SSH key") from error
        self.memory_guard.check_peer(self.policy, raw_key.decode("ascii"))
        info = self.guest.request("Info")
        hex_value(info.get("compose_hash"), 32)
        # This is an early consistency check, not an authenticated measurement.
        require(info.get("os_image_hash") == OS_IMAGE_HASH, "unexpected guest OS")
        for field in ("app_id", "instance_id"):
            require(type(info.get(field)) is str and re.fullmatch(
                r"[0-9a-f]{40}", info[field]), "invalid guest identity")
        require(type(info.get("vm_config")) is str and info["vm_config"], "missing VM config")
        require(type(strict_json(info["vm_config"])) is dict, "invalid VM config")
        # Info is an external RPC and may block. A status checked before that
        # call cannot authorize identity after the SSH watchdog has stopped.
        self.memory_guard.check_peer(self.policy, raw_key.decode("ascii"))
        endpoint = self.endpoint(info)
        return {"guest_channel_key": key, **endpoint,
                "username": "covenant", "guest_info": info}

    def endpoint(self, info):
        return {"host": f'{info["instance_id"]}-1022.{self.policy["gateway_domain"]}',
                "port": 443, "transport": {"kind": "tls"}}

    def scope_fields(self):
        return SCOPE_FIELDS

    def validate(self, challenge, identity, now_ms):
        exact_fields(challenge, CHALLENGE_FIELDS)
        integer(challenge["schema_version"], 1, 1)
        uuid_value(challenge["challenge_id"])
        hex_value(challenge["nonce_hex"], 32)
        issued = integer(challenge["issued_at_ms"])
        expiry = integer(challenge["expires_at_ms"])
        require(issued <= now_ms < expiry <= issued + 120_000, "stale challenge")
        require(expiry <= self.policy["lease_expires_at_ms"], "challenge exceeds lease")
        scope = challenge["scope"]
        exact_fields(scope, self.scope_fields())
        for field in (
            "job_id", "operator_pubkey_b58", "buyer_pubkey_b58",
            "lease_generation", "lease_expires_at_ms",
        ):
            require(type(scope[field]) is type(self.policy[field])
                    and scope[field] == self.policy[field], "wrong lease scope")
        for field in ("host", "port", "username", "guest_channel_key", "transport"):
            require(type(scope[field]) is type(identity[field])
                    and scope[field] == identity[field], "wrong SSH channel")
        require(scope["workload_sha256"] == identity["guest_info"]["compose_hash"],
                "wrong measured workload")
        return hashlib.sha512(DOMAIN + canonical(challenge)).hexdigest()

    def collect(self, challenge):
        before = self.identity()
        now_ms = self.clock()
        report_data = self.validate(challenge, before, now_ms)
        self.journal.consume(challenge, now_ms, self.policy["max_challenges"])
        quote = self.guest.request("GetQuote", {"report_data": report_data})
        require(type(quote.get("quote")) is str and re.fullmatch(
            r"[0-9a-fA-F]{2,262144}", quote["quote"]) and len(quote["quote"]) % 2 == 0,
            "invalid quote encoding")
        require(quote.get("report_data") == report_data, "guest report-data mismatch")
        require(type(quote.get("event_log")) is str
                and type(strict_json(quote["event_log"])) is list, "invalid event log")
        require(quote.get("vm_config") == before["guest_info"]["vm_config"],
                "VM config changed")
        after = self.identity()
        require(after == before, "guest identity or SSH key changed")
        self.validate(challenge, after, self.clock())
        # Preserve all native evidence fields, including V2 event preimages.
        # No legacy event translation, collateral shortcut or trust label.
        return {"schema_version": 1, "verification_status": "unverified",
                "challenge": challenge, "identity": before, "dstack_quote": quote}


def serve(collector, token, address=("0.0.0.0", 8090)):
    require(re.fullmatch(r"[0-9a-f]{64}", token) is not None, "missing collector token")

    class Handler(http.server.BaseHTTPRequestHandler):
        server_version = "CovenantEvidenceCollector"
        sys_version = ""

        def setup(self):
            super().setup()
            self.connection.settimeout(8)

        def log_message(self, *_):
            pass

        def reply(self, status, value):
            raw = json.dumps(value, separators=(",", ":")).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(raw)

        def authorized(self):
            values = self.headers.get_all("Authorization", [])
            expected = ("Bearer " + token).encode()
            supplied = values[0].encode("utf-8") if len(values) == 1 else b""
            if not hmac.compare_digest(supplied, expected):
                self.reply(401, {"error": "unauthorized"})
                return False
            return True

        def do_GET(self):
            if not self.authorized():
                return
            if self.path != "/identity":
                self.reply(404, {"error": "not found"})
                return
            try:
                self.reply(200, {"verification_status": "unverified",
                                 "identity": collector.identity()})
            except Exception:
                self.reply(503, {"error": "identity unavailable"})

        def do_POST(self):
            if not self.authorized():
                return
            if self.path != "/quote":
                self.reply(404, {"error": "not found"})
                return
            try:
                require(not self.headers.get_all("Transfer-Encoding")
                        and not self.headers.get_all("Content-Encoding"),
                        "unsupported request encoding")
                lengths = self.headers.get_all("Content-Length", [])
                require(len(lengths) == 1 and re.fullmatch(r"[1-9][0-9]{0,5}", lengths[0]),
                        "invalid content length")
                length = integer(int(lengths[0]), 1, MAX_REQUEST)
                require(self.headers.get("Content-Type") == "application/json",
                        "invalid content type")
                raw = self.rfile.read(length)
                require(len(raw) == length, "incomplete request")
                result = collector.collect(strict_json(raw))
                self.reply(200, result)
            except Rejected:
                self.reply(409, {"error": "collection rejected"})
            except Exception:
                self.reply(503, {"error": "collection unavailable"})

    # Deliberately serial: at most one guest RPC and bounded parsing at a time.
    server = http.server.HTTPServer(address, Handler)
    server.timeout = 1
    while collector.clock() < collector.policy["lease_expires_at_ms"]:
        server.handle_request()
    server.server_close()


def main():
    from memory_guard import RuntimeGuard
    policy = environment_policy()
    RuntimeGuard().check("collector")
    journal = ReplayStore("/state/challenges.sqlite3", policy["job_id"])
    collector = Collector(policy, "/host-public/host_key.pub", Dstack(), journal)
    serve(collector, os.environ.get("COVENANT_COLLECTOR_TOKEN", ""))


if __name__ == "__main__":
    main()
