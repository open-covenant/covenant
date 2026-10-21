#!/usr/bin/env python3
"""Untrusted, loopback-only HTTP to Nitro relay. No application plaintext.

Deploy behind separately qualified authenticated HTTPS ingress. The gateway
cannot authorize a key release: enclave policy, buyer/coordinator signatures and
hardware evidence do that. Its session IDs are routing capabilities only.
"""
import argparse
import http.server
import json
import re
import socket
import threading
import time
import urllib.request
import uuid

MAX_FRAME = 256 * 1024
SESSION_SECONDS = 65
MAX_SESSIONS = 2  # Matches the enclave process/concurrency budget.


def strict_json(raw):
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:
                raise ValueError("duplicate field")
            result[key] = value
        return result
    return json.loads(raw, object_pairs_hook=pairs,
                      parse_constant=lambda _: (_ for _ in ()).throw(ValueError("nonfinite")))


def encode(value):
    raw = json.dumps(value, allow_nan=False, separators=(",", ":")).encode() + b"\n"
    if len(raw) > MAX_FRAME:
        raise ValueError("oversized frame")
    return raw


def imds_credentials():
    """Only the fixed IMDSv2 endpoint; no redirects, proxies or caller URL."""
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *_args, **_kwargs):
            raise ValueError("metadata redirect")
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

    def read(path, *, token=None, method="GET"):
        headers = {"X-aws-ec2-metadata-token": token} if token else {
            "X-aws-ec2-metadata-token-ttl-seconds": "60"}
        request = urllib.request.Request("http://169.254.169.254" + path,
                                         headers=headers, method=method)
        with opener.open(request, timeout=3) as response:
            raw = response.read(16385)
        if len(raw) > 16384:
            raise ValueError("metadata too large")
        return raw

    token = read("/latest/api/token", method="PUT").decode("ascii")
    role = read("/latest/meta-data/iam/security-credentials/", token=token).decode("ascii")
    if not re.fullmatch(r"[A-Za-z0-9+=,.@_-]{1,128}", role):
        raise ValueError("invalid role")
    value = strict_json(read("/latest/meta-data/iam/security-credentials/" + role, token=token))
    if value.get("Code") != "Success":
        raise ValueError("metadata credentials unavailable")
    result = {target: value[source] for target, source in (
        ("access_key_id", "AccessKeyId"), ("secret_access_key", "SecretAccessKey"),
        ("session_token", "Token"))}
    if any(type(v) is not str or not 1 <= len(v) <= 8192 for v in result.values()):
        raise ValueError("invalid credentials")
    return result


class EnclaveConnection:
    def __init__(self, cid, credentials):
        self.socket = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
        self.socket.settimeout(35)
        self.stream = None
        try:
            self.socket.connect((cid, 5000))
            self.stream = self.socket.makefile("rwb")
            value = self.command("credentials", credentials)
            if value != {"ok": True}:
                raise ValueError("credential initialization failed")
        except Exception:
            self.close()
            raise

    def command(self, operation, request):
        self.stream.write(encode({"operation": operation, "request": request}))
        self.stream.flush()
        raw = self.stream.readline(MAX_FRAME + 1)
        if len(raw) > MAX_FRAME or not raw.endswith(b"\n"):
            raise ValueError("invalid enclave frame")
        value = strict_json(raw)
        if type(value) is not dict or value.get("ok") is not True:
            raise ValueError("enclave rejected request")
        return value

    def close(self):
        if self.stream is not None:
            self.stream.close()
        self.socket.close()


class Sessions:
    def __init__(self, connect, *, clock=time.monotonic):
        self.connect = connect
        self.clock = clock
        self.lock = threading.Lock()
        self.active = {}
        self.opening = 0

    def prune(self):
        with self.lock:
            stale = [key for key, (_, expires) in self.active.items() if expires <= self.clock()]
            connections = [self.active.pop(key)[0] for key in stale]
        for connection in connections:
            connection.close()

    def challenge(self, request):
        if type(request) is not dict or set(request) - {
                "binding", "recipient_spki_der_base64", "signed_job_envelope"} or not {
                "binding", "recipient_spki_der_base64"} <= set(request):
            raise ValueError("invalid challenge request")
        self.prune()
        with self.lock:
            if len(self.active) + self.opening >= MAX_SESSIONS:
                raise ValueError("capacity")
            self.opening += 1
        connection = None
        try:
            connection = self.connect()
            value = connection.command("challenge", request)
            challenge = value["result"]
            session_id = str(uuid.uuid4())
            with self.lock:
                self.active[session_id] = (connection, self.clock() + SESSION_SECONDS)
            connection = None
            return {"session_id": session_id, "challenge": challenge}
        finally:
            with self.lock:
                self.opening -= 1
            if connection is not None:
                connection.close()

    def release(self, body):
        if type(body) is not dict or set(body) != {"session_id", "request"}:
            raise ValueError("invalid release")
        session_id = body["session_id"]
        if type(session_id) is not str or str(uuid.UUID(session_id)) != session_id:
            raise ValueError("invalid session")
        self.prune()
        # Consume before any I/O. A disconnect/timeout never restores or retries.
        with self.lock:
            connection, expires = self.active.pop(session_id)
            self.opening += 1  # In-flight releases still occupy enclave capacity.
        try:
            if self.clock() >= expires:
                raise ValueError("expired session")
            return connection.command("release", body["request"])["result"]
        finally:
            connection.close()
            with self.lock:
                self.opening -= 1

    def close(self):
        with self.lock:
            connections = [value[0] for value in self.active.values()]
            self.active.clear()
        for connection in connections:
            connection.close()


class Gateway(http.server.ThreadingHTTPServer):
    daemon_threads = True
    request_queue_size = 8

    def __init__(self, address, sessions):
        if address[0] != "127.0.0.1":
            raise ValueError("gateway must remain loopback-only")
        self.sessions = sessions
        self.slots = threading.BoundedSemaphore(8)
        super().__init__(address, Handler)

    def process_request(self, request, client_address):
        if not self.slots.acquire(blocking=False):
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, client_address)
        except Exception:
            self.slots.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self.slots.release()

    def handle_error(self, _request, _address):
        pass  # No request bodies, credentials or exception details in host logs.

    def service_actions(self):
        self.sessions.prune()


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def setup(self):
        self.request.settimeout(35)
        # Socket idle timeouts alone do not stop a byte-at-a-time slow client.
        def expire():
            try:
                self.request.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
        self.deadline = threading.Timer(45, expire)
        self.deadline.daemon = True
        self.deadline.start()
        super().setup()

    def finish(self):
        try:
            super().finish()
        finally:
            self.deadline.cancel()

    def log_message(self, *_args):
        pass

    def do_POST(self):
        try:
            if self.path not in ("/v1/challenge", "/v1/release"):
                raise ValueError("unknown operation")
            lengths = self.headers.get_all("Content-Length", [])
            if len(lengths) != 1 or not re.fullmatch(r"[1-9][0-9]{0,5}", lengths[0]) or self.headers.get("Transfer-Encoding"):
                raise ValueError("invalid framing")
            length = int(lengths[0])
            if length > MAX_FRAME - 1 or self.headers.get_content_type() != "application/json":
                raise ValueError("invalid content")
            raw = self.rfile.read(length)
            if len(raw) != length:
                raise ValueError("truncated body")
            body = strict_json(raw)
            result = self.server.sessions.challenge(body) if self.path == "/v1/challenge" else self.server.sessions.release(body)
            payload = encode(result)
            status = 200
        except Exception:
            status, payload = 400, b'{"error":"rejected_or_unknown_outcome_do_not_retry"}\n'
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(payload)
        self.close_connection = True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--enclave-cid", type=int, required=True)
    parser.add_argument("--port", type=int, default=8088)
    args = parser.parse_args()
    if not 3 < args.enclave_cid < 2**32 - 1 or not 1024 <= args.port <= 65535:
        parser.error("invalid enclave CID or unprivileged port")
    sessions = Sessions(lambda: EnclaveConnection(args.enclave_cid, imds_credentials()))
    try:
        with Gateway(("127.0.0.1", args.port), sessions) as server:
            server.serve_forever(poll_interval=0.5)
    finally:
        sessions.close()


if __name__ == "__main__":
    main()
