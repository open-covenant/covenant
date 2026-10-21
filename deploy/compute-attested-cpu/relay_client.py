#!/usr/bin/env python3
"""Buyer relay operations and an SSH ProxyCommand; never substitutes for quote verification."""
import argparse
import base64
import http.client
import os
from pathlib import Path
import select
import signal
import sys
import time
import uuid

from transport_protocol import (
    MAX_BUFFER, MAX_CHUNK, MAX_STREAM_SECONDS, HttpTransport, ProtocolError,
    decode_chunk, fields, integer, now_ms, require, uuid_string,
)


def reconcile(transport, operation, arguments, deadline, token=None):
    """Retry the identical idempotent operation only; never mint a replacement ID."""
    last = None
    for attempt in range(3):
        require(time.monotonic() < deadline, "local_deadline_expired", 410)
        try:
            return transport.request(operation, arguments, token=token)
        except ProtocolError as error:
            if error.status != 500:
                raise
            last = error
        except (OSError, http.client.HTTPException) as error:
            last = error
        if attempt < 2:
            time.sleep(0.1)
    raise ProtocolError("transport_outcome_unknown", 502) from last


def pump_stream(transport, operation, stream_id, input_fd, output_fd, deadline,
                token=None, half_close=None):
    """Bounded duplex bytes, explicit delivered ACKs, and no connection resumption."""
    uuid_string(stream_id)
    previous = {fd: os.get_blocking(fd) for fd in {input_fd, output_fd}}
    for fd in previous:
        os.set_blocking(fd, False)
    outgoing, incoming = bytearray(), bytearray()
    sent, delivered = 0, 0
    local_eof = remote_eof = sent_eof = closed_output = False
    try:
        while time.monotonic() < deadline:
            readable = [input_fd] if not local_eof and len(outgoing) < MAX_CHUNK else []
            writable = [output_fd] if incoming else []
            ready_read, ready_write, _ = select.select(readable, writable, [], 0.02)
            if input_fd in ready_read:
                try:
                    chunk = os.read(input_fd, MAX_CHUNK - len(outgoing))
                except BlockingIOError:
                    chunk = None
                if chunk is not None:
                    if chunk:
                        outgoing.extend(chunk)
                    else:
                        local_eof = True
            if output_fd in ready_write:
                try:
                    written = os.write(output_fd, incoming)
                except BlockingIOError:
                    written = 0
                require(written >= 0, "local_write_failed", 502)
                del incoming[:written]
                delivered += written
            arguments = {"stream_id": stream_id, "operation_id": str(uuid.uuid4()),
                         "send_offset": sent, "data": base64.b64encode(outgoing).decode(),
                         "receive_ack": delivered, "eof": local_eof}
            result = reconcile(transport, operation, arguments, deadline, token)
            fields(result, {"send_ack", "send_eof", "receive_offset", "data", "eof"})
            accepted = integer(result["send_ack"], sent, sent + len(outgoing)) - sent
            del outgoing[:accepted]
            sent += accepted
            require(type(result["send_eof"]) is bool and type(result["eof"]) is bool,
                    "invalid_stream_flags", 502)
            require(not result["send_eof"] or (local_eof and not outgoing), "unexpected_send_eof", 502)
            require(not sent_eof or result["send_eof"], "send_eof_reversed", 502)
            sent_eof = result["send_eof"]
            require(integer(result["receive_offset"]) == delivered, "receive_offset_changed", 502)
            chunk = decode_chunk(result["data"])
            overlap = min(len(incoming), len(chunk))
            require(incoming[:overlap] == chunk[:overlap], "replayed_bytes_changed", 502)
            require(len(chunk) >= len(incoming), "replayed_bytes_missing", 502)
            require(not remote_eof or (result["eof"] and len(chunk) == len(incoming)),
                    "data_after_remote_eof", 502)
            incoming.extend(chunk[len(incoming):])
            require(len(incoming) <= MAX_BUFFER, "receive_buffer_exceeded", 413)
            remote_eof = result["eof"]
            if remote_eof and not incoming and not closed_output:
                if half_close is not None:
                    half_close()
                closed_output = True
            if local_eof and sent_eof and remote_eof and not outgoing and not incoming:
                return {"sent_bytes": sent, "received_bytes": delivered}
        raise ProtocolError("stream_deadline_expired", 410)
    finally:
        for fd, blocking in previous.items():
            try:
                os.set_blocking(fd, blocking)
            except OSError:
                pass


class RelayClient:
    def __init__(self, endpoint, session_id, buyer_token, transport=None):
        self.transport = transport or HttpTransport(endpoint, session_id, buyer_token, allow_loopback=True)
        self.session_id = uuid_string(session_id)

    def pin_instance(self, instance_id):
        return reconcile(self.transport, "buyer_pin_instance", {"instance_id": instance_id}, time.monotonic() + 30)

    def control(self, kind, payload, request_id=None, timeout=120):
        request_id = uuid_string(request_id or str(uuid.uuid4()))
        deadline = time.monotonic() + timeout
        arguments = {"request_id": request_id, "kind": kind, "payload": payload}
        submitted = reconcile(self.transport, "buyer_submit", arguments, deadline)
        require(submitted == {"request_id": request_id}, "wrong_submit_ack", 502)
        while time.monotonic() < deadline:
            response = reconcile(self.transport, "buyer_result", {"request_id": request_id}, deadline)
            fields(response, {"request_id", "result"})
            require(response["request_id"] == request_id, "wrong_result_id", 502)
            result = response["result"]
            if result is None:
                continue
            require(type(result) is dict and type(result.get("ok")) is bool, "invalid_control_result", 502)
            fields(result, {"ok", "value"} if result["ok"] else {"ok", "error"})
            require(result["ok"], "guest_operation_failed", 502)
            return result["value"]
        raise ProtocolError("control_outcome_unknown", 504)

    def identity(self, request_id=None):
        return self.control("identity", {}, request_id, timeout=60)

    def quote(self, challenge):
        require(type(challenge) is dict, "invalid_challenge", 400)
        remaining = (integer(challenge["expires_at_ms"]) - now_ms()) / 1000
        require(0 < remaining <= 120, "expired_challenge", 410)
        return self.control("quote", challenge, challenge["challenge_id"], timeout=remaining)

    def proxy_stdio(self, challenge_id, input_fd=None, output_fd=None):
        """Call only after independent quote verification; SSH must pin its quoted key."""
        stream_id = str(uuid.uuid4())
        result = self.control("open_stream", {"stream_id": stream_id,
                                               "challenge_id": uuid_string(challenge_id)}, timeout=30)
        require(result == {"stream_id": stream_id}, "wrong_stream_ack", 502)
        try:
            return pump_stream(self.transport, "buyer_exchange", stream_id,
                               sys.stdin.fileno() if input_fd is None else input_fd,
                               sys.stdout.fileno() if output_fd is None else output_fd,
                               time.monotonic() + MAX_STREAM_SECONDS)
        finally:
            try:
                self.close_stream()
            except Exception:
                pass

    def close_stream(self):
        """Idempotently stop the sole stream, including a terminated ProxyCommand."""
        result = reconcile(self.transport, "buyer_close_stream", {}, time.monotonic() + 30)
        require(result == {"closed": True}, "wrong_close_ack", 502)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--endpoint", required=True)
    parser.add_argument("--session-id", required=True)
    parser.add_argument("--buyer-token-file", required=True, type=Path)
    parser.add_argument("--verified-challenge-id", required=True)
    args = parser.parse_args()
    metadata = args.buyer_token_file.stat()
    require(metadata.st_mode & 0o077 == 0 and metadata.st_size <= 65, "unsafe_buyer_token_file")
    client = RelayClient(args.endpoint, args.session_id, args.buyer_token_file.read_text().strip())
    def stop(_signal, _frame):
        raise SystemExit(0)
    signal.signal(signal.SIGHUP, stop)
    signal.signal(signal.SIGTERM, stop)
    client.proxy_stdio(args.verified_challenge_id)


if __name__ == "__main__":
    main()
