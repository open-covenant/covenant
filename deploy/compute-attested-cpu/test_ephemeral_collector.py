import copy
import hashlib
import unittest

import collector as c
from ephemeral_collector import EphemeralCollector, PROFILE
import test_collector as fixture
from test_collector import NOW


class EphemeralTests(unittest.TestCase):
    # Reuse the isolated guest RPC fixture, not its managed-profile test methods.
    def setUp(self):
        fixture.Tests.setUp(self)
        self.policy.pop("gateway_domain")
        self.policy.update(profile=PROFILE, relay_endpoint="https://relay.example.com",
                           relay_session_id=self.policy["job_id"],
                           lease_generation="phala-ephemeral-" + self.policy["job_id"] + "-1")
        c.load_policy(self.policy, NOW)
        self.service = EphemeralCollector(self.policy, self.key, self.guest, self.store,
                                          lambda: self.now, memory_guard=self.memory_guard)
        self.challenge["scope"].update(profile=PROFILE,
                                       lease_generation=self.policy["lease_generation"],
                                       host="relay.example.com",
                                       transport={"kind": "outbound_relay"})

    def test_exact_profile_is_bound_and_raw_quote_preserved(self):
        result = self.service.collect(self.challenge)
        self.assertEqual(result["identity"]["host"], "relay.example.com")
        self.assertEqual(result["dstack_quote"]["report_data"],
                         hashlib.sha512(c.DOMAIN + c.canonical(self.challenge)).hexdigest())

    def test_profile_and_relay_substitution_reject_before_quote(self):
        for field, value in (("profile", "standard"), ("host", "evil.example.com"),
                             ("transport", {"kind": "tls"})):
            changed = copy.deepcopy(self.challenge)
            changed["scope"][field] = value
            with self.assertRaises(c.Rejected):
                self.service.collect(changed)
        self.assertFalse(any(method == "GetQuote" for method, _ in self.guest.calls))

    def test_stream_rechecks_runtime_key_and_expiry_without_guest_rpc(self):
        identity = self.service.identity()
        self.guest.calls.clear()
        self.service.check_stream(identity)
        self.assertFalse(self.guest.calls)
        self.memory_guard.check_peer.assert_called_with(self.policy, self.key.read_text())
        self.now = self.policy["lease_expires_at_ms"]
        with self.assertRaises(c.Rejected):
            self.service.check_stream(identity)
        self.now = NOW
        changed = copy.deepcopy(identity)
        changed["guest_channel_key"]["key_base64"] = "invalid"
        with self.assertRaises(c.Rejected):
            self.service.check_stream(changed)

    def test_policy_rejects_credentials_alternate_session_and_gateway(self):
        for key, value in (("relay_endpoint", "https://user@relay.example.com"),
                           ("relay_endpoint", "https://relay.example.com/path"),
                           ("relay_endpoint", "http://relay.example.com"),
                           ("relay_endpoint", "https://127.0.0.1"),
                           ("relay_session_id", "other"),
                           ("gateway_domain", "test.phala.network")):
            changed = copy.deepcopy(self.policy)
            changed[key] = value
            with self.assertRaises(c.Rejected):
                c.load_policy(changed, NOW)


if __name__ == "__main__":
    unittest.main()
