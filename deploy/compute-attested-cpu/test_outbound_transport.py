#!/usr/bin/env python3
"""Offline transport tests. Fake evidence is explicitly not hardware acceptance."""
import base64
import contextlib
import copy
import io
import json
import socket
import threading
import time
import unittest
import uuid
from unittest.mock import patch

from outbound_relay import RelayState, serve_relay
from outbound_worker import MAX_IDENTITY_BYTES, OutboundWorker
from relay_client import RelayClient
from transport_protocol import (
    MAX_BUFFER, MAX_CHUNK, HttpTransport, ProtocolError, encoded, endpoint_origin, now_ms, strict_json,
)


def uid():
    return str(uuid.uuid4())


class RelayStateTests(unittest.TestCase):
    def setUp(self):
        self.now, self.mono = 2_000_000_000_000, 100
        self.session, self.buyer = uid(), "b" * 64
        self.state = RelayState(self.session, self.now + 600_000, self.buyer,
                                clock=lambda: self.now, monotonic=lambda: self.mono)
        self.instance = "a" * 40
        self.registration = {"registration_id": uid(), "instance_id": self.instance}
        self.registered = self.state.dispatch("guest_register", self.registration)
        self.guest = self.registered["guest_token"]

    def pin(self):
        return self.state.dispatch("buyer_pin_instance", {"instance_id": self.instance}, self.buyer)

    def submit(self, kind, payload, request_id=None):
        arguments = {"request_id": request_id or uid(), "kind": kind, "payload": payload}
        self.state.dispatch("buyer_submit", arguments, self.buyer)
        return arguments

    def stream(self):
        self.pin()
        stream_id = uid()
        submitted = self.submit("open_stream", {"stream_id": stream_id, "challenge_id": uid()})
        self.state.dispatch("guest_reply", {"request_id": submitted["request_id"],
                            "result": {"ok": True, "value": {"stream_id": stream_id}}}, self.guest)
        return stream_id

    def exchange(self, role, stream_id, data=b"", send_offset=0, receive_ack=0, eof=False, operation_id=None):
        arguments = {"stream_id": stream_id, "operation_id": operation_id or uid(),
                     "send_offset": send_offset, "receive_ack": receive_ack,
                     "data": base64.b64encode(data).decode(), "eof": eof}
        result = self.state.dispatch(role + "_exchange", arguments,
                                     self.buyer if role == "buyer" else self.guest)
        return result, arguments

    def test_registration_is_unverified_and_idempotent(self):
        self.assertFalse(self.registered["instance_pinned"])
        self.assertEqual(self.state.dispatch("guest_register", self.registration), self.registered)
        with self.assertRaisesRegex(ProtocolError, "guest_replacement_forbidden"):
            self.state.dispatch("guest_register", {"registration_id": uid(), "instance_id": self.instance})

    def test_stream_requires_instance_pin(self):
        with self.assertRaisesRegex(ProtocolError, "verified_instance_pin_required"):
            self.submit("open_stream", {"stream_id": uid(), "challenge_id": uid()})
        self.assertEqual(len(self.state.operations), 0)

    def test_mismatched_pin_rejects_session_permanently(self):
        with self.assertRaisesRegex(ProtocolError, "candidate_instance_mismatch"):
            self.state.dispatch("buyer_pin_instance", {"instance_id": "c" * 40}, self.buyer)
        with self.assertRaisesRegex(ProtocolError, "session_rejected"):
            self.state.dispatch("guest_register", self.registration)

    def test_authenticated_buyer_and_guest_roles(self):
        for operation, arguments, token in (("buyer_pin_instance", {"instance_id": self.instance}, self.guest),
                                            ("guest_poll", {}, self.buyer), ("guest_poll", {}, None)):
            with self.subTest(operation=operation), self.assertRaisesRegex(ProtocolError, "unauthorized"):
                self.state.dispatch(operation, arguments, token)

    def test_operation_identity_and_reply_reconciliation(self):
        submitted = self.submit("identity", {})
        self.state.dispatch("buyer_submit", submitted, self.buyer)
        changed = dict(submitted, kind="quote")
        with self.assertRaisesRegex(ProtocolError, "operation_id_collision"):
            self.state.dispatch("buyer_submit", changed, self.buyer)
        reply = {"request_id": submitted["request_id"], "result": {"ok": True, "value": {"public": "identity"}}}
        self.state.dispatch("guest_reply", reply, self.guest)
        self.state.dispatch("guest_reply", reply, self.guest)
        with self.assertRaisesRegex(ProtocolError, "result_collision"):
            self.state.dispatch("guest_reply", dict(reply, result={"ok": False, "error": "changed"}), self.guest)

    def test_exchange_reconciliation_and_delivered_ack(self):
        stream_id = self.stream()
        first, arguments = self.exchange("buyer", stream_id, b"encrypted bytes")
        self.assertEqual(first["send_ack"], 15)
        self.assertEqual(self.state.dispatch("buyer_exchange", arguments, self.buyer), first)
        received, _ = self.exchange("guest", stream_id)
        self.assertEqual(base64.b64decode(received["data"]), b"encrypted bytes")
        received_again, _ = self.exchange("guest", stream_id)
        self.assertEqual(received_again["data"], received["data"])
        acked, _ = self.exchange("guest", stream_id, receive_ack=15)
        self.assertEqual(acked["data"], "")
        with self.assertRaisesRegex(ProtocolError, "exchange_id_collision"):
            self.state.dispatch("buyer_exchange", dict(arguments, eof=True), self.buyer)

    def test_backpressure_caps_buffer_without_losing_bytes(self):
        stream_id = self.stream()
        sent = 0
        for _ in range(MAX_BUFFER // MAX_CHUNK):
            result, _ = self.exchange("buyer", stream_id, b"x" * MAX_CHUNK, send_offset=sent)
            sent = result["send_ack"]
        result, _ = self.exchange("buyer", stream_id, b"pending", send_offset=sent, eof=True)
        self.assertEqual(result["send_ack"], MAX_BUFFER)
        self.assertFalse(result["send_eof"])
        self.assertEqual(len(self.state.stream["buyer"].buffer), MAX_BUFFER)
        self.exchange("guest", stream_id, receive_ack=MAX_CHUNK)
        result, _ = self.exchange("buyer", stream_id, b"pending", send_offset=sent, eof=True)
        self.assertEqual(result["send_ack"], MAX_BUFFER + 7)
        self.assertTrue(result["send_eof"])

    def test_offsets_and_data_after_eof_rejected(self):
        stream_id = self.stream()
        self.exchange("buyer", stream_id, b"x", eof=True)
        with self.assertRaisesRegex(ProtocolError, "wrong_send_offset"):
            self.exchange("buyer", stream_id, send_offset=0)
        with self.assertRaisesRegex(ProtocolError, "data_after_eof"):
            self.exchange("buyer", stream_id, b"y", send_offset=1)
        with self.assertRaises(ProtocolError):
            self.exchange("guest", stream_id, receive_ack=2)

    def test_explicit_close_is_idempotent_and_terminal(self):
        stream_id = self.stream()
        for _ in range(2):
            self.assertEqual(self.state.dispatch("buyer_close_stream", {}, self.buyer), {"closed": True})
        with self.assertRaisesRegex(ProtocolError, "stream_closed"):
            self.exchange("guest", stream_id)

    def test_expiry_and_clock_rollback(self):
        self.now -= 1
        with self.assertRaisesRegex(ProtocolError, "lease_expired"):
            self.state.dispatch("guest_register", self.registration)
        self.now += 1
        self.mono += 600
        with self.assertRaisesRegex(ProtocolError, "lease_expired"):
            self.state.dispatch("guest_register", self.registration)

    def test_endpoint_and_json_boundaries(self):
        self.assertEqual(endpoint_origin("https://relay.example.com"), ("https", "relay.example.com", 443))
        self.assertEqual(endpoint_origin("http://127.0.0.1:1234", True), ("http", "127.0.0.1", 1234))
        for endpoint in ("http://relay.example.com", "https://relay.example.com:8443",
                         "https://user:pass@relay.example.com", "https://relay.example.com/path",
                         "https://127.0.0.1", "http://127.0.0.1:1234"):
            with self.subTest(endpoint=endpoint), self.assertRaises(ProtocolError):
                endpoint_origin(endpoint)
        with self.assertRaises(ProtocolError):
            strict_json('{"a":1,"a":2}')
        with self.assertRaises(ProtocolError):
            strict_json('{"a":1.5}')


class Evidence:
    def __init__(self):
        self.calls = 0
        self.fail = False

    def identity(self):
        return {"host": "relay.example.com", "port": 443, "username": "covenant",
                "transport": {"kind": "outbound_relay"},
                "guest_channel_key": {"algorithm": "ssh-ed25519", "key_base64": "TEST_ONLY"},
                "guest_info": {"instance_id": "a" * 40, "compose_hash": "b" * 64}}

    def collect(self, challenge):
        self.calls += 1
        if self.fail:
            raise TimeoutError("synthetic quote failure")
        return {"schema_version": 1, "verification_status": "unverified",
                "identity": self.identity(), "challenge": copy.deepcopy(challenge),
                "dstack_quote": {"fake_for_transport_tests_only": True}}

    def check_stream(self, identity):
        if self.identity() != identity:
            raise ValueError("synthetic identity changed")


class FullInfoEvidence(Evidence):
    """Synthetic public Info of realistic size; no real quote or trust claim."""
    def __init__(self, tcb_bytes=36_445):
        super().__init__()
        self.tcb_bytes = tcb_bytes

    def identity(self):
        identity = super().identity()
        # Structural lengths from retained Info only; no real cert/evidence.
        tcb = {"event_log": "", "measurement": "b" * 96}
        tcb["event_log"] = "a" * (self.tcb_bytes - len(json.dumps(tcb)))
        identity["guest_info"]["tcb_info"] = json.dumps(tcb)
        prefix = "SYNTHETIC PUBLIC CERTIFICATE:"
        identity["guest_info"]["app_cert"] = prefix + "c" * (14_542 - len(prefix))
        identity["guest_info"]["vm_config"] = json.dumps({"synthetic_padding": "v" * 5_000})
        return identity


class WorkerTests(unittest.TestCase):
    def test_full_dstack_info_above_old_cap_is_preserved(self):
        evidence = FullInfoEvidence()
        worker = OutboundWorker("https://relay.example.com", uid(), evidence, now_ms() + 120_000)
        expected = evidence.identity()
        self.assertEqual(len(expected["guest_info"]["tcb_info"]), 36_445)
        self.assertEqual(len(expected["guest_info"]["app_cert"]), 14_542)
        self.assertGreater(len(encoded(expected)), 32 * 1024)
        self.assertLess(len(encoded(expected)), MAX_IDENTITY_BYTES)
        self.assertEqual(worker.identity(), expected)
        result = worker.execute({"request_id": uid(), "kind": "identity", "payload": {}})
        self.assertEqual(result, {"ok": True, "value": expected})

    def test_oversized_identity_is_rejected_without_truncation(self):
        evidence = FullInfoEvidence(MAX_IDENTITY_BYTES)
        worker = OutboundWorker("https://relay.example.com", uid(), evidence, now_ms() + 120_000)
        original = evidence.identity()
        self.assertGreater(len(encoded(original)), MAX_IDENTITY_BYTES)
        with self.assertRaisesRegex(ProtocolError, "identity_too_large"):
            worker.identity()
        self.assertEqual(evidence.identity(), original)

    def test_registration_invalid_identity_fails_promptly_with_sanitized_diagnostic(self):
        for oversized in (False, True):
            with self.subTest(oversized=oversized):
                evidence = FullInfoEvidence(MAX_IDENTITY_BYTES) if oversized else Evidence()
                if not oversized:
                    evidence.identity = lambda: (_ for _ in ()).throw(ValueError("SECRET_PROVIDER_VALUE"))
                worker = OutboundWorker("https://relay.example.com", uid(), evidence, now_ms() + 120_000)
                log = io.StringIO()
                with contextlib.redirect_stderr(log), patch("outbound_worker.time.sleep") as sleep:
                    with self.assertRaises(ProtocolError if oversized else ValueError):
                        worker.register()
                    sleep.assert_not_called()
                records = [json.loads(line) for line in log.getvalue().splitlines()]
                expected = {"event": "outbound_registration_identity_failed",
                            "error_class": "ProtocolError" if oversized else "ValueError"}
                if oversized:
                    expected["error_code"] = "identity_too_large"
                self.assertEqual(records, [expected])
                self.assertNotIn("SECRET_PROVIDER_VALUE", log.getvalue())
                self.assertNotIn("SYNTHETIC PUBLIC CERTIFICATE", log.getvalue())

    def test_transient_registration_identity_retry_is_bounded_and_log_rate_limited(self):
        evidence = Evidence()
        evidence.identity = lambda: (_ for _ in ()).throw(TimeoutError("SECRET_PROVIDER_VALUE"))
        worker = OutboundWorker("https://relay.example.com", uid(), evidence, now_ms() + 120_000)
        elapsed = [0]
        def advance(_):
            elapsed[0] += 1 if elapsed[0] == 0 else 180
        log = io.StringIO()
        with contextlib.redirect_stderr(log), \
                patch("outbound_worker.time.monotonic", side_effect=lambda: elapsed[0]), \
                patch("outbound_worker.time.sleep", side_effect=advance):
            with self.assertRaisesRegex(ProtocolError, "guest_registration_timeout"):
                worker.register()
        self.assertEqual([json.loads(line) for line in log.getvalue().splitlines()], [{
            "event": "outbound_registration_identity_failed", "error_class": "TimeoutError"}])

    def test_failed_quote_not_reexecuted(self):
        evidence = Evidence()
        evidence.fail = True
        worker = OutboundWorker("https://relay.example.com", uid(), evidence, now_ms() + 120_000)
        request_id = uid()
        operation = {"request_id": request_id, "kind": "quote", "payload": {"challenge_id": request_id}}
        first = worker.execute(operation)
        self.assertFalse(first["ok"])
        evidence.fail = False
        self.assertEqual(worker.execute(operation), first)
        self.assertEqual(evidence.calls, 1)

    def test_stream_without_fresh_quote_never_connects(self):
        def forbidden(*_, **__):
            self.fail("unverified stream connected")
        worker = OutboundWorker("https://relay.example.com", uid(), Evidence(), now_ms() + 120_000,
                                ssh_connector=forbidden)
        result = worker.execute({"request_id": uid(), "kind": "open_stream",
                                 "payload": {"stream_id": uid(), "challenge_id": uid()}})
        self.assertFalse(result["ok"])

    def test_loopback_http_control_and_duplex_bytes(self):
        session, token, expiry = uid(), "c" * 64, now_ms() + 120_000
        state = RelayState(session, expiry, token)
        server = serve_relay(state)
        endpoint = "http://127.0.0.1:%d" % server.server_port
        server_thread = threading.Thread(target=server.serve_forever, daemon=True)
        server_thread.start()
        errors = []
        worker_sock, fake_ssh = socket.socketpair()
        buyer_sock, application = socket.socketpair()
        application.settimeout(15)
        def connector(address, timeout):
            self.assertEqual(address, ("ssh", 1022))
            self.assertEqual(timeout, 5)
            return worker_sock
        evidence = FullInfoEvidence()
        worker = OutboundWorker("https://relay.example.com", session, evidence, expiry,
                                transport=HttpTransport(endpoint, session, allow_loopback=True),
                                ssh_connector=connector)
        def run_worker():
            try:
                worker.run()
            except Exception as error:
                errors.append(error)
        def echo():
            try:
                data = bytearray()
                while True:
                    chunk = fake_ssh.recv(8192)
                    if not chunk:
                        break
                    data.extend(chunk)
                fake_ssh.sendall(b"response:" + data)
                fake_ssh.shutdown(socket.SHUT_WR)
            except Exception as error:
                errors.append(error)
        worker_thread = threading.Thread(target=run_worker, daemon=True)
        echo_thread = threading.Thread(target=echo, daemon=True)
        worker_thread.start()
        echo_thread.start()
        client = RelayClient(endpoint, session, token)
        try:
            identity = client.identity()
            self.assertGreater(len(encoded(identity)), 32 * 1024)
            self.assertEqual(identity, evidence.identity())
            self.assertEqual(identity["guest_info"]["instance_id"], "a" * 40)
            challenge = {"challenge_id": uid(), "expires_at_ms": now_ms() + 90_000}
            envelope = client.quote(challenge)
            self.assertEqual(envelope["identity"], identity)
            self.assertTrue(envelope["dstack_quote"]["fake_for_transport_tests_only"])
            client.pin_instance("a" * 40)
            def proxy():
                try:
                    client.proxy_stdio(challenge["challenge_id"], buyer_sock.fileno(), buyer_sock.fileno())
                except Exception as error:
                    errors.append(error)
                finally:
                    buyer_sock.close()
            proxy_thread = threading.Thread(target=proxy, daemon=True)
            proxy_thread.start()
            payload = b"transport-only-test\x00" * 4096
            application.sendall(payload)
            application.shutdown(socket.SHUT_WR)
            received = bytearray()
            while True:
                chunk = application.recv(16384)
                if not chunk:
                    break
                received.extend(chunk)
            self.assertEqual(received, b"response:" + payload)
            proxy_thread.join(5)
            worker_thread.join(5)
            self.assertFalse(proxy_thread.is_alive())
            self.assertFalse(worker_thread.is_alive())
            self.assertEqual(errors, [])
        finally:
            server.shutdown()
            server.server_close()
            for connection in (application, buyer_sock, worker_sock, fake_ssh):
                connection.close()
            server_thread.join(2)


if __name__ == "__main__":
    unittest.main()
