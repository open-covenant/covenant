#!/usr/bin/env python3
"""Ephemeral bounded routing relay. SSH encryption and guest trust stay at the buyer."""
import argparse
import base64
from collections import OrderedDict
import copy
import hmac
import http.server
import json
from pathlib import Path
import secrets
import socket
import threading
import time

from transport_protocol import (
    MAX_BUFFER, MAX_CHUNK, MAX_HTTP, MAX_OPERATIONS, MAX_STREAM_BYTES,
    MAX_STREAM_SECONDS, ProtocolError, decode_chunk, encoded, fields, fingerprint,
    hex_string, integer, now_ms, require, strict_json, uuid_string,
)


class Pipe:
    def __init__(self):
        self.start, self.end, self.buffer, self.eof = 0, 0, bytearray(), False


class RelayState:
    def __init__(self, session_id, expires_at_ms, buyer_token, expected_instance_id=None,
                 clock=now_ms, monotonic=time.monotonic):
        self.session_id = uuid_string(session_id)
        self.buyer_token = hex_string(buyer_token, 64)
        self.clock, self.monotonic = clock, monotonic
        now = clock()
        integer(expires_at_ms, now + 1, now + 3_600_000)
        self.expires_at_ms, self.last_ms = expires_at_ms, now
        self.deadline = monotonic() + (expires_at_ms - now) / 1000
        self.expected_instance_id = hex_string(expected_instance_id, 40) if expected_instance_id else None
        self.guest_registration, self.guest_token = None, None
        self.rejected = False
        self.operations = OrderedDict()
        self.stream = None
        self.condition = threading.Condition()

    def live(self):
        now = self.clock()
        require(not self.rejected, "session_rejected", 410)
        require(self.last_ms <= now < self.expires_at_ms and self.monotonic() < self.deadline,
                "lease_expired", 410)
        self.last_ms = now

    def authorize(self, token, expected):
        require(type(token) is str and type(expected) is str
                and hmac.compare_digest(token.encode(), expected.encode()), "unauthorized", 401)

    def dispatch(self, operation, arguments, token=None):
        with self.condition:
            self.live()
            if operation == "guest_register":
                fields(arguments, {"registration_id", "instance_id"})
                uuid_string(arguments["registration_id"])
                hex_string(arguments["instance_id"], 40)
                require(self.expected_instance_id in (None, arguments["instance_id"]), "wrong_instance", 403)
                if self.guest_registration is None:
                    self.guest_registration = copy.deepcopy(arguments)
                    self.guest_token = secrets.token_hex(32)
                require(arguments == self.guest_registration, "guest_replacement_forbidden", 409)
                return {"guest_token": self.guest_token, "expires_at_ms": self.expires_at_ms,
                        "instance_pinned": self.expected_instance_id is not None}
            if operation.startswith("buyer_"):
                self.authorize(token, self.buyer_token)
                return self.buyer(operation, arguments)
            if operation.startswith("guest_"):
                self.authorize(token, self.guest_token)
                return self.guest(operation, arguments)
            raise ProtocolError("unknown_operation", 404)

    def buyer(self, operation, arguments):
        if operation == "buyer_pin_instance":
            fields(arguments, {"instance_id"})
            instance = hex_string(arguments["instance_id"], 40)
            require(self.expected_instance_id in (None, instance), "instance_replacement_forbidden")
            if self.guest_registration is not None and self.guest_registration["instance_id"] != instance:
                self.rejected = True
                self.condition.notify_all()
                raise ProtocolError("candidate_instance_mismatch", 403)
            self.expected_instance_id = instance
            return {"instance_id": instance}
        if operation == "buyer_submit":
            fields(arguments, {"request_id", "kind", "payload"})
            request_id = uuid_string(arguments["request_id"])
            kind = arguments["kind"]
            require(kind in ("identity", "quote", "open_stream"), "unknown_control", 400)
            require(len(encoded(arguments["payload"])) <= 16 * 1024, "control_too_large", 413)
            digest = fingerprint(arguments)
            existing = self.operations.get(request_id)
            if existing is not None:
                require(existing["fingerprint"] == digest, "operation_id_collision")
            else:
                require(len(self.operations) < MAX_OPERATIONS, "operation_budget_exhausted", 429)
                if kind == "open_stream":
                    require(self.expected_instance_id is not None and self.guest_registration is not None
                            and self.guest_registration["instance_id"] == self.expected_instance_id,
                            "verified_instance_pin_required", 403)
                    fields(arguments["payload"], {"stream_id", "challenge_id"})
                    uuid_string(arguments["payload"]["stream_id"])
                    uuid_string(arguments["payload"]["challenge_id"])
                    require(self.stream is None and not any(
                        item["operation"]["kind"] == "open_stream" for item in self.operations.values()),
                        "only_one_stream_allowed")
                self.operations[request_id] = {"fingerprint": digest,
                                               "operation": copy.deepcopy(arguments), "result": None}
                self.condition.notify_all()
            return {"request_id": request_id}
        if operation == "buyer_result":
            fields(arguments, {"request_id"})
            request_id = uuid_string(arguments["request_id"])
            require(request_id in self.operations, "unknown_request", 404)
            item = self.operations[request_id]
            if item["result"] is None:
                self.condition.wait(timeout=min(1, max(0, self.deadline - self.monotonic())))
                self.live()
            return {"request_id": request_id, "result": copy.deepcopy(item["result"])}
        if operation == "buyer_exchange":
            return self.exchange("buyer", arguments)
        if operation == "buyer_close_stream":
            fields(arguments, set())
            if self.stream is not None:
                self.stream["closed"] = True
                self.condition.notify_all()
            return {"closed": True}
        raise ProtocolError("unknown_operation", 404)

    def guest(self, operation, arguments):
        if operation == "guest_poll":
            fields(arguments, set())
            pending = next((item["operation"] for item in self.operations.values() if item["result"] is None), None)
            if pending is None:
                self.condition.wait(timeout=min(1, max(0, self.deadline - self.monotonic())))
                self.live()
                pending = next((item["operation"] for item in self.operations.values() if item["result"] is None), None)
            return {"operation": copy.deepcopy(pending)}
        if operation == "guest_reply":
            fields(arguments, {"request_id", "result"})
            request_id = uuid_string(arguments["request_id"])
            require(request_id in self.operations, "unknown_request", 404)
            result = arguments["result"]
            require(type(result) is dict and type(result.get("ok")) is bool, "invalid_result", 400)
            fields(result, {"ok", "value"} if result["ok"] else {"ok", "error"})
            require(len(encoded(result)) <= 512 * 1024, "result_too_large", 413)
            item = self.operations[request_id]
            if item["result"] is not None:
                require(item["result"] == result, "result_collision")
            else:
                if item["operation"]["kind"] == "open_stream" and result["ok"]:
                    stream_id = item["operation"]["payload"]["stream_id"]
                    require(self.stream is None, "only_one_stream_allowed")
                    require(result["value"] == {"stream_id": stream_id}, "wrong_stream_reply")
                    self.stream = {"stream_id": stream_id, "buyer": Pipe(), "guest": Pipe(),
                                   "closed": False,
                                   "bytes": 0, "deadline": min(self.deadline, self.monotonic() + MAX_STREAM_SECONDS),
                                   "history": {"buyer": OrderedDict(), "guest": OrderedDict()}}
                item["result"] = copy.deepcopy(result)
                self.condition.notify_all()
            return {"request_id": request_id, "stored": True}
        if operation == "guest_exchange":
            return self.exchange("guest", arguments)
        raise ProtocolError("unknown_operation", 404)

    def exchange(self, role, arguments):
        fields(arguments, {"stream_id", "operation_id", "send_offset", "data", "receive_ack", "eof"})
        stream = self.stream
        require(stream is not None and arguments["stream_id"] == stream["stream_id"], "wrong_stream", 404)
        require(not stream["closed"], "stream_closed", 410)
        require(self.monotonic() < stream["deadline"], "stream_expired", 410)
        operation_id = uuid_string(arguments["operation_id"])
        digest = fingerprint(arguments)
        history = stream["history"][role]
        if operation_id in history:
            prior_digest, result = history[operation_id]
            require(digest == prior_digest, "exchange_id_collision")
            return copy.deepcopy(result)
        own, peer = stream[role], stream["guest" if role == "buyer" else "buyer"]
        raw = decode_chunk(arguments["data"])
        require(type(arguments["eof"]) is bool, "invalid_eof", 400)
        require(integer(arguments["send_offset"]) == own.end, "wrong_send_offset")
        ack = integer(arguments["receive_ack"], peer.start, peer.end)
        require(not own.eof or not raw, "data_after_eof")
        capacity = MAX_BUFFER - len(own.buffer)
        accepted = min(len(raw), capacity)
        require(stream["bytes"] + accepted <= MAX_STREAM_BYTES, "stream_byte_budget_exhausted", 413)
        del peer.buffer[:ack - peer.start]
        peer.start = ack
        own.buffer.extend(raw[:accepted])
        own.end += accepted
        stream["bytes"] += accepted
        if arguments["eof"] and accepted == len(raw):
            own.eof = True
        result = {"send_ack": own.end, "send_eof": own.eof, "receive_offset": peer.start,
                  "data": base64.b64encode(bytes(peer.buffer[:MAX_CHUNK])).decode(),
                  "eof": peer.eof and len(peer.buffer) <= MAX_CHUNK}
        history[operation_id] = (digest, copy.deepcopy(result))
        while len(history) > 4:
            history.popitem(last=False)
        return result


class BoundedServer(http.server.ThreadingHTTPServer):
    daemon_threads = True
    request_queue_size = 8

    def __init__(self, address, handler):
        super().__init__(address, handler)
        self.slots = threading.BoundedSemaphore(8)

    def process_request(self, request, client_address):
        if not self.slots.acquire(blocking=False):
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, client_address)
        except BaseException:
            self.slots.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self.slots.release()


def serve_relay(state, bind="127.0.0.1", port=0):
    class Handler(http.server.BaseHTTPRequestHandler):
        server_version = "CovenantOutboundRelay"
        sys_version = ""

        def setup(self):
            super().setup()
            self.connection.settimeout(8)
            self.cutoff = threading.Timer(15, self.cut_connection)
            self.cutoff.daemon = True
            self.cutoff.start()

        def cut_connection(self):
            try:
                self.connection.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass

        def finish(self):
            self.cutoff.cancel()
            super().finish()

        def log_message(self, *_):
            pass

        def reply(self, status, value):
            body = encoded(value)
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self):
            try:
                require(self.path == "/v1/relay", "not_found", 404)
                require(not self.headers.get_all("Transfer-Encoding")
                        and not self.headers.get_all("Content-Encoding"), "unsupported_encoding", 400)
                lengths = self.headers.get_all("Content-Length", [])
                require(len(lengths) == 1 and lengths[0].isdigit(), "invalid_length", 400)
                length = integer(int(lengths[0]), 1, MAX_HTTP)
                require(self.headers.get_all("Content-Type", []) == ["application/json"], "invalid_content_type", 400)
                auth = self.headers.get_all("Authorization", [])
                require(len(auth) <= 1 and (not auth or auth[0].startswith("Bearer ")), "invalid_authorization", 401)
                token = auth[0][7:] if auth else None
                raw = self.rfile.read(length)
                require(len(raw) == length, "incomplete_request", 400)
                message = strict_json(raw)
                fields(message, {"version", "session_id", "operation", "arguments"})
                require(type(message["version"]) is int and message["version"] == 1, "wrong_version", 400)
                require(message["session_id"] == state.session_id, "wrong_session", 404)
                require(type(message["operation"]) is str, "invalid_operation", 400)
                result = state.dispatch(message["operation"], message["arguments"], token)
                self.reply(200, {"version": 1, "result": result})
            except ProtocolError as error:
                self.reply(error.status, {"error": error.code})
            except Exception:
                try:
                    self.reply(500, {"error": "relay_failed"})
                except OSError:
                    pass

    return BoundedServer((bind, port), Handler)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--session-id", required=True)
    parser.add_argument("--expires-at-ms", required=True, type=int)
    parser.add_argument("--buyer-token-file", required=True, type=Path)
    parser.add_argument("--port", required=True, type=int)
    args = parser.parse_args()
    metadata = args.buyer_token_file.stat()
    require(metadata.st_mode & 0o077 == 0 and metadata.st_size <= 65, "unsafe_buyer_token_file")
    token = args.buyer_token_file.read_text().strip()
    state = RelayState(args.session_id, args.expires_at_ms, token)
    server = serve_relay(state, port=integer(args.port, 1, 65535))
    server.timeout = 1
    try:
        while time.monotonic() < state.deadline:
            server.handle_request()
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
