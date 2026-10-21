import base64
import copy
import hashlib
import http.server
import json
from pathlib import Path
import socketserver
import tempfile
import threading
import unittest
from unittest import mock

import collector as c
import render

NOW = 1_792_540_800_000
PUBLIC = "ssh-ed25519 " + base64.b64encode(
    b"\0\0\0\x0bssh-ed25519\0\0\0\x20" + bytes([0x9E]) * 32
).decode()
POLICY = {
    "schema_version": 1,
    "job_id": "05446ce4-557a-4ee0-9e47-f47228bb703c",
    "operator_pubkey_b58": "11111111111111111111111111111111",
    "buyer_pubkey_b58": "11111111111111111111111111111112",
    "lease_generation": "cvm-acceptance-1",
    "lease_expires_at_ms": NOW + 600_000,
    "gateway_domain": "dstack-pha-prod5.phala.network",
    "buyer_ssh_public_key": PUBLIC,
    "max_challenges": 8,
}
INFO = {
    "app_id": "1a" * 20, "instance_id": "2b" * 20, "compose_hash": "3c" * 32,
    "os_image_hash": c.OS_IMAGE_HASH,
    "vm_config": '{"cpu_count":1,"memory_size":2048}',
}
EVENTS = '[{"version":2,"imr":3,"event_type":1,"event":"app-id","digest":"' + (
    "ab" * 48) + '","event_payload":"1a","preimage":"020000000100000000"}]'


class Guest:
    def __init__(self):
        self.info = copy.deepcopy(INFO)
        self.calls = []
        self.bad_quote = None

    def request(self, method, payload=None):
        self.calls.append((method, payload))
        if method == "Info":
            return copy.deepcopy(self.info)
        if self.bad_quote is not None:
            return self.bad_quote
        return {"quote": "abcd", "report_data": payload["report_data"],
                "event_log": EVENTS, "vm_config": self.info["vm_config"]}


class Tests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.key = self.directory / "key.pub"
        self.key.write_text(PUBLIC + "\n")
        self.guest = Guest()
        self.now = NOW
        self.policy = c.load_policy(copy.deepcopy(POLICY), NOW)
        self.store = c.ReplayStore(self.directory / "journal.db", self.policy["job_id"])
        # Collector protocol tests isolate guest RPC/canonicalization. Real
        # kernel-interface rejection tests live in test_memory_guard.py, and
        # runtime_smoke.py exercises both roles under actual Linux/Docker.
        self.memory_guard = mock.Mock()
        self.service = c.Collector(self.policy, self.key, self.guest, self.store,
                                   lambda: self.now, memory_guard=self.memory_guard)
        identity = self.service.identity()
        self.challenge = {
            "schema_version": 1,
            "challenge_id": "b61bf342-b2ea-4012-8946-6bd69c6f333d",
            "nonce_hex": "4d" * 32,
            "issued_at_ms": NOW, "expires_at_ms": NOW + 120_000,
            "scope": {
                key: self.policy[key] for key in (
                    "job_id", "operator_pubkey_b58", "buyer_pubkey_b58",
                    "lease_generation", "lease_expires_at_ms",
                )
            },
        }
        self.challenge["scope"].update({key: identity[key] for key in (
            "host", "port", "username", "guest_channel_key", "transport",
        )})
        self.challenge["scope"]["workload_sha256"] = INFO["compose_hash"]

    def test_raw_v2_evidence_preserved_and_exact_64_byte_binding(self):
        response = self.service.collect(self.challenge)
        self.assertEqual(response["verification_status"], "unverified")
        self.assertEqual(response["dstack_quote"]["event_log"], EVENTS)
        self.assertEqual(response["dstack_quote"]["vm_config"], INFO["vm_config"])
        expected = hashlib.sha512(c.DOMAIN + c.canonical(self.challenge)).hexdigest()
        self.assertEqual(self.guest.calls[-2], ("GetQuote", {"report_data": expected}))
        self.assertEqual(len(bytes.fromhex(expected)), 64)

    def test_actual_rust_hardware_challenge_golden_digest(self):
        # Independently produced using the actual Rust HardwareChallenge
        # deserializer and guest_report_data(), not Python's serializer.
        expected = (
            "8beb204f70fe63d2648c573860c1a03e1966e9a7084195c4ab4eb0a2d55818a0d"
            "1dc8e10aa9a163ab9663320cd4d2f213921d672a904741538b62dfab3c229de"
        )
        actual = self.service.validate(self.challenge, self.service.identity(), NOW)
        self.assertEqual(actual, expected)

    def test_all_lease_and_channel_substitutions_rejected_before_quote(self):
        changed = {
            "job_id": "a54eae37-99a6-465c-92ca-81477bd62629",
            "operator_pubkey_b58": POLICY["buyer_pubkey_b58"],
            "buyer_pubkey_b58": POLICY["operator_pubkey_b58"],
            "lease_generation": "other", "host": "other.phala.network",
            "port": 1022, "username": "root", "transport": {"kind": "direct"}, "guest_channel_key": {
                "algorithm": "ssh-ed25519", "key_base64": "AAAA"},
            "workload_sha256": "ff" * 32, "lease_expires_at_ms": NOW + 700_000,
        }
        for field, value in changed.items():
            with self.subTest(field=field):
                challenge = copy.deepcopy(self.challenge)
                challenge["scope"][field] = value
                with self.assertRaises(c.Rejected):
                    self.service.collect(challenge)
        self.assertFalse(any(call[0] == "GetQuote" for call in self.guest.calls))

    def test_stale_future_oversized_and_malformed_challenge(self):
        cases = [
            ("issued_at_ms", NOW + 1), ("expires_at_ms", NOW),
            ("expires_at_ms", NOW + 120_001), ("schema_version", True),
            ("schema_version", 2), ("nonce_hex", "4D" * 32),
            ("nonce_hex", "ff"), ("challenge_id", str("0" * 32)),
        ]
        for key, value in cases:
            with self.subTest(field=key, value=value):
                challenge = copy.deepcopy(self.challenge)
                challenge[key] = value
                with self.assertRaises(c.Rejected):
                    self.service.collect(challenge)
        challenge = copy.deepcopy(self.challenge)
        challenge["arbitrary_report_data"] = "ff" * 64
        with self.assertRaises(c.Rejected):
            self.service.collect(challenge)

    def test_replay_and_nonce_reuse_rejected_after_reopen(self):
        self.service.collect(self.challenge)
        self.service.journal = c.ReplayStore(self.directory / "journal.db", POLICY["job_id"])
        for same_nonce in (False, True):
            challenge = copy.deepcopy(self.challenge)
            if same_nonce:
                challenge["challenge_id"] = "dcd24959-d202-40a2-b615-e45b3fbeb83d"
            with self.assertRaises(c.Rejected):
                self.service.collect(challenge)

    def test_failed_guest_response_still_consumes_challenge(self):
        self.guest.bad_quote = {"quote": "ab", "report_data": "00" * 64}
        with self.assertRaises(c.Rejected):
            self.service.collect(self.challenge)
        self.guest.bad_quote = None
        with self.assertRaises(c.Rejected):
            self.service.collect(self.challenge)

    def test_clock_rollback_and_budget_are_durable(self):
        self.store.consume(self.challenge, NOW + 1, 2)
        second = copy.deepcopy(self.challenge)
        second.update(challenge_id="dcd24959-d202-40a2-b615-e45b3fbeb83d", nonce_hex="aa" * 32)
        with self.assertRaises(c.Rejected):
            self.store.consume(second, NOW, 2)
        self.store.consume(second, NOW + 2, 2)
        third = copy.deepcopy(second)
        third.update(challenge_id="a54eae37-99a6-465c-92ca-81477bd62629", nonce_hex="bb" * 32)
        with self.assertRaises(c.Rejected):
            self.store.consume(third, NOW + 3, 2)

    def test_different_job_cannot_reuse_journal(self):
        with self.assertRaises(c.Rejected):
            c.ReplayStore(self.directory / "journal.db",
                          "a54eae37-99a6-465c-92ca-81477bd62629")

    def test_expired_lease_cannot_return_identity(self):
        self.now = self.policy["lease_expires_at_ms"]
        with self.assertRaises(c.Rejected):
            self.service.identity()

    def test_os_mismatch_and_invalid_key_fail(self):
        self.guest.info["os_image_hash"] = "00" * 32
        with self.assertRaises(c.Rejected):
            self.service.identity()
        self.guest.info["os_image_hash"] = c.OS_IMAGE_HASH
        self.key.write_text("ssh-rsa AAAA\n")
        with self.assertRaises(c.Rejected):
            self.service.identity()

    def test_identity_change_during_quote_rejected(self):
        original = self.guest.request

        def changing(method, payload=None):
            result = original(method, payload)
            if method == "GetQuote":
                self.guest.info["instance_id"] = "cc" * 20
            return result
        self.guest.request = changing
        with self.assertRaises(c.Rejected):
            self.service.collect(self.challenge)

    def test_unsafe_memory_rejected_before_guest_rpc(self):
        from memory_guard import UnsafeRuntime
        self.guest.calls.clear()
        self.memory_guard.check_peer.side_effect = UnsafeRuntime("swap enabled")
        with self.assertRaises(UnsafeRuntime):
            self.service.collect(self.challenge)
        self.assertEqual(self.guest.calls, [])

    def test_memory_guard_rechecked_after_quote(self):
        from memory_guard import UnsafeRuntime
        self.memory_guard.check_peer.reset_mock()
        self.memory_guard.check_peer.side_effect = [None, None, UnsafeRuntime("stale peer")]
        with self.assertRaises(UnsafeRuntime):
            self.service.collect(self.challenge)
        self.assertEqual(self.memory_guard.check_peer.call_count, 3)
        # The failed post-quote guard cannot make the consumed nonce reusable.
        self.memory_guard.check_peer.side_effect = None
        with self.assertRaises(c.Rejected):
            self.service.collect(self.challenge)

    def test_slow_info_cannot_publish_identity_after_peer_status_invalidates(self):
        from memory_guard import UnsafeRuntime
        self.memory_guard.check_peer.reset_mock()
        original = self.guest.request

        def invalidate_during_info(method, payload=None):
            result = original(method, payload)
            if method == "Info":
                self.memory_guard.check_peer.side_effect = UnsafeRuntime("expired peer")
            return result
        self.guest.request = invalidate_during_info
        with self.assertRaises(UnsafeRuntime):
            self.service.identity()
        self.assertEqual(self.memory_guard.check_peer.call_count, 2)

    def test_quote_finishing_after_expiry_rejected(self):
        original = self.guest.request

        def slow(method, payload=None):
            result = original(method, payload)
            if method == "GetQuote":
                self.now = self.challenge["expires_at_ms"]
            return result
        self.guest.request = slow
        with self.assertRaises(c.Rejected):
            self.service.collect(self.challenge)

    def test_duplicate_float_nonfinite_unicode_and_bool_not_canonical(self):
        for raw in ('{"a":1,"a":2}', '{"number":1.5}', '{"number":NaN}'):
            with self.subTest(raw=raw), self.assertRaises(c.Rejected):
                c.strict_json(raw)
        for invalid in ({"a": True}, {"a": "\n"}, {"a": "\u00e9"}, {"a": 1 << 53}):
            with self.subTest(value=invalid), self.assertRaises(c.Rejected):
                c.canonical(invalid)
        self.assertEqual(c.canonical({"z": 1, "a": '"\\'}), b'{"a":"\\"\\\\","z":1}')

    def test_template_requires_digest_bounded_expiry_and_retains_secret_reference(self):
        template = Path(__file__).with_name("compose.template.yaml").read_text()
        rendered = render.render(template, "sha256:" + "ab" * 32, self.policy, NOW)
        self.assertNotIn("__IMAGE_DIGEST__", rendered)
        self.assertNotIn("__LEASE_POLICY_B64__", rendered)
        self.assertNotIn("__JOB_ID__", rendered)
        self.assertIn("${COVENANT_COLLECTOR_TOKEN:?missing encrypted collector token}", rendered)
        with self.assertRaises(c.Rejected):
            render.render(template, "latest", self.policy, NOW)
        policy = copy.deepcopy(self.policy)
        policy["lease_expires_at_ms"] = NOW + 3_600_001
        with self.assertRaises(c.Rejected):
            render.render(template, "sha256:" + "ab" * 32, policy, NOW)

    def test_noswap_local_volumes_are_isolated_per_job_without_image_copy(self):
        template = Path(__file__).with_name("compose.template.yaml").read_text()
        rendered = render.render(template, "sha256:" + "ab" * 32, self.policy, NOW)
        # Docker --tmpfs rejects noswap itself; local-driver options are passed
        # to the kernel. Mount verification remains an independent runtime gate.
        self.assertNotIn("    tmpfs:", rendered)
        self.assertEqual(rendered.count("      type: tmpfs"), 4)
        self.assertEqual(rendered.count("volume: {nocopy: true}"), 5)
        self.assertEqual(rendered.count("cgroup: private"), 2)
        for prefix in ("ssh-run-memory", "ssh-tmp-memory", "workspace-memory",
                       "host-public-memory-v1", "collector-state"):
            self.assertIn(f"{prefix}-{self.policy['job_id']}", rendered)
        self.assertEqual(rendered.count("noswap,size="), 4)
        self.assertIn('o: "rw,nosuid,nodev,noswap,size=128m,mode=700,uid=10001,gid=10001"', rendered)

    def test_unix_rpc_uses_raw_report_data_and_preserves_strings(self):
        captured = []
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass
            def do_POST(self):
                body = c.strict_json(self.rfile.read(int(self.headers["Content-Length"])))
                captured.append((self.path, body))
                raw = json.dumps({"quote": "abcd", "report_data": body["report_data"],
                                  "event_log": EVENTS, "vm_config": INFO["vm_config"]}).encode()
                self.send_response(200)
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)
        path = str(self.directory / "guest.sock")
        server = socketserver.UnixStreamServer(path, Handler)
        thread = threading.Thread(target=server.handle_request, daemon=True)
        thread.start()
        digest = "ab" * 64
        try:
            result = c.Dstack(path).request("GetQuote", {"report_data": digest})
            self.assertEqual(captured, [("/GetQuote", {"report_data": digest})])
            self.assertEqual(result["event_log"], EVENTS)
        finally:
            thread.join(timeout=3)
            server.server_close()


if __name__ == "__main__":
    unittest.main()
