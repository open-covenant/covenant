"""Offline protocol tests: real RSA/OAEP, MOCK TDX and runtime guard.

No fixture here is Intel hardware evidence or a successful live key release.
"""
import base64
import copy
import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest

from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ed25519, padding, rsa

import recipient as r


# Public test-only signer; never load a fixture seed in a deployed broker.
BROKER_SIGNER = ed25519.Ed25519PrivateKey.from_private_bytes(b"s" * 32)


def b58encode(raw):
    alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
    value = int.from_bytes(raw, "big")
    result = ""
    while value:
        value, remainder = divmod(value, 58)
        result = alphabet[remainder] + result
    return "1" * (len(raw) - len(raw.lstrip(b"\0"))) + result


BROKER_PUBLIC = b58encode(BROKER_SIGNER.public_key().public_bytes(
    serialization.Encoding.Raw, serialization.PublicFormat.Raw))


class Clock:
    wall = 1_800_000_000_000
    mono = 100.0


class MockGuard:
    fail = False
    calls = 0

    def check(self, role):
        assert role == "ssh"
        self.calls += 1
        r.collector.require(not self.fail, "mock unsafe memory runtime")


class MockDstack:
    def __init__(self):
        self.calls = []
        self.info = {"os_image_hash": r.collector.OS_IMAGE_HASH,
                     "compose_hash": "ab" * 32, "app_id": "cd" * 20,
                     "instance_id": "ef" * 20,
                     "vm_config": '{"key_provider":{"name":"none","id":""}}'}
        self.wrong_report = False
        self.fail_quote = False

    def request(self, method, payload=None):
        self.calls.append((method, payload))
        if method == "Info":
            return self.info.copy()
        assert method == "GetQuote"
        if self.fail_quote:
            raise OSError("mock quote failure")
        return {"quote": "00112233", "event_log": '[{"mock_tdx":1}]',
                "report_data": "00" * 64 if self.wrong_report else payload["report_data"],
                "vm_config": self.info["vm_config"]}


def policies():
    job = "ddff04e5-aeb5-44ee-a137-4164c24d6597"
    def public(seed):
        return b58encode(ed25519.Ed25519PrivateKey.from_private_bytes(seed).public_key().public_bytes(
            serialization.Encoding.Raw, serialization.PublicFormat.Raw))
    lease = {"schema_version": 1, "profile": "phala_ephemeral_dstack061",
             "job_id": job, "operator_pubkey_b58": public(b"o" * 32),
             "buyer_pubkey_b58": public(b"b" * 32),
             "lease_generation": "phala-ephemeral-" + job + "-1",
             "lease_expires_at_ms": Clock.wall + 300_000,
             "relay_endpoint": "https://relay.example.com", "relay_session_id": job,
             "buyer_ssh_public_key": "ssh-ed25519 " + base64.b64encode(
                 b"\0\0\0\x0bssh-ed25519\0\0\0\x20" + b"x" * 32).decode(),
             "max_challenges": 4}
    policy = {"protocol_version": 1, "broker_id": "covenant-broker-test",
              "tenant_id": "tenant-1", "key_id": "application-1", "key_version": 1,
              "purpose": "workspace", "lease_id": job,
              "broker_response_pubkey_b58": BROKER_PUBLIC}
    return policy, lease


def make_recipient():
    clock, guard, guest = Clock(), MockGuard(), MockDstack()
    policy, lease = policies()
    value = r.Recipient(policy, lease, guest, guard=guard,
                        clock=lambda: clock.wall, monotonic=lambda: clock.mono)
    return value, clock, guard, guest


def challenge(value):
    return {"protocol_version": 1, "broker_id": value.policy["broker_id"],
            "challenge_id": "ba888f79-2103-4a51-afad-4516d7c51f34",
            "nonce_hex": "01" * 32, "ttl_ms": 60_000,
            "binding": copy.deepcopy(value.binding),
            "recipient_spki_der_base64": value.public}


def response(value, key=b"k" * 32, *, label=None):
    challenge_digest = r.digest(value.challenge)
    public = serialization.load_der_public_key(base64.b64decode(value.public))
    ciphertext = public.encrypt(key, padding.OAEP(
        mgf=padding.MGF1(hashes.SHA256()), algorithm=hashes.SHA256(),
        label=(r.RESPONSE_LABEL + challenge_digest.encode()) if label is None else label))
    result = {"protocol_version": 1, "challenge_id": value.challenge["challenge_id"],
            "challenge_sha256": challenge_digest, "key_id": value.binding["key_id"],
            "key_version": value.binding["key_version"], "algorithm": "RSA_OAEP_SHA256",
            "ciphertext_base64": base64.b64encode(ciphertext).decode()}
    return {"response": result, "broker_pubkey_b58": BROKER_PUBLIC,
            "signature_b58": b58encode(BROKER_SIGNER.sign(
                r.RESPONSE_SIGNATURE_DOMAIN + r.collector.canonical(result)))}


class RecipientTests(unittest.TestCase):
    def test_real_rsa3072_spki_and_exact_raw64_mock_quote(self):
        value, _, _, guest = make_recipient()
        public = serialization.load_der_public_key(base64.b64decode(value.public))
        self.assertIsInstance(public, rsa.RSAPublicKey)
        self.assertEqual(public.key_size, 3072)
        self.assertEqual(public.public_numbers().e, 65537)
        request = challenge(value)
        bundle = value.collect(request)
        calls = [payload for method, payload in guest.calls if method == "GetQuote"]
        self.assertEqual(calls, [{"report_data": r.report_data(request).hex()}])
        self.assertEqual(len(bytes.fromhex(calls[0]["report_data"])), 64)
        self.assertEqual(bundle["evidence"]["quote_base64"], "ABEiMw==")
        self.assertEqual(value.decrypt(response(value)), b"k" * 32)
        self.assertIsNone(value._key)
        with self.assertRaises(r.collector.Rejected):
            value.decrypt(response(value))

    def test_every_binding_mutation_is_rejected_before_quote(self):
        value, _, _, guest = make_recipient()
        original = challenge(value)
        for name in original["binding"]:
            changed = copy.deepcopy(original)
            changed["binding"][name] = (2 if name == "key_version" else "other")
            with self.subTest(field=name), self.assertRaises(r.collector.Rejected):
                value.collect(changed)
        self.assertFalse(any(method == "GetQuote" for method, _ in guest.calls))

    def test_no_arbitrary_recipient_quote_oracle(self):
        value, _, _, guest = make_recipient()
        second, _, _, _ = make_recipient()
        request = challenge(value)
        request["recipient_spki_der_base64"] = second.public
        with self.assertRaises(r.collector.Rejected):
            value.collect(request)
        self.assertFalse(any(method == "GetQuote" for method, _ in guest.calls))

    def test_protocol_types_unknown_fields_ttl_broker_and_nonce_rejected(self):
        value, _, _, guest = make_recipient()
        for name, replacement in [("protocol_version", True), ("ttl_ms", True),
                                  ("ttl_ms", 0), ("ttl_ms", 60_001),
                                  ("broker_id", "other"), ("nonce_hex", "AA" * 32),
                                  ("challenge_id", "00000000-0000-0000-0000-000000000000")]:
            request = challenge(value)
            request[name] = replacement
            with self.subTest(field=name, value=replacement), self.assertRaises(r.collector.Rejected):
                value.collect(request)
        request = challenge(value)
        request["extra"] = "field"
        with self.assertRaises(r.collector.Rejected):
            value.collect(request)
        self.assertFalse(any(method == "GetQuote" for method, _ in guest.calls))

    def test_runtime_failure_prevents_quote(self):
        value, _, guard, guest = make_recipient()
        guard.fail = True
        with self.assertRaises(r.collector.Rejected):
            value.collect(challenge(value))
        self.assertFalse(any(method == "GetQuote" for method, _ in guest.calls))

    def test_guest_identity_changes_and_managed_kms_refused(self):
        for name, replacement in [("compose_hash", "bb" * 32), ("instance_id", "bb" * 20),
                                  ("vm_config", '{"key_provider":{"name":"kms","id":"1234"}}')]:
            value, _, _, guest = make_recipient()
            guest.info[name] = replacement
            with self.subTest(field=name), self.assertRaises(r.collector.Rejected):
                value.collect(challenge(value))
            self.assertFalse(any(method == "GetQuote" for method, _ in guest.calls))

    def test_failed_external_quote_consumes_recipient(self):
        value, _, _, guest = make_recipient()
        guest.fail_quote = True
        with self.assertRaises(OSError):
            value.collect(challenge(value))
        guest.fail_quote = False
        with self.assertRaises(r.collector.Rejected):
            value.collect(challenge(value))

    def test_guest_report_data_mismatch_refused(self):
        value, _, _, guest = make_recipient()
        guest.wrong_report = True
        with self.assertRaises(r.collector.Rejected):
            value.collect(challenge(value))

    def test_monotonic_deadline_survives_wallclock_rollback(self):
        value, clock, _, _ = make_recipient()
        value.collect(challenge(value))
        reply = response(value)
        clock.wall -= 60_000
        clock.mono += 60
        with self.assertRaises(r.collector.Rejected):
            value.decrypt(reply)

    def test_lease_expiry_prevents_release(self):
        value, clock, _, _ = make_recipient()
        value.collect(challenge(value))
        reply = response(value)
        clock.wall += 300_000
        with self.assertRaises(r.collector.Rejected):
            value.decrypt(reply)

    def test_response_binding_mutations_rejected_before_decryption(self):
        value, _, _, _ = make_recipient()
        value.collect(challenge(value))
        original = response(value)
        for name in original["response"].keys() - {"ciphertext_base64"}:
            changed = copy.deepcopy(original)
            changed["response"][name] = 2 if name in {"protocol_version", "key_version"} else "other"
            with self.subTest(field=name), self.assertRaises(r.collector.Rejected):
                value.decrypt(changed)
        self.assertIsNotNone(value._key)

    def test_untrusted_parent_cannot_replace_key_with_chosen_plaintext(self):
        value, _, _, _ = make_recipient()
        value.collect(challenge(value))
        original = response(value)
        injected = response(value, b"a" * 32)
        # The parent knows the public RSA key but cannot produce a valid broker
        # signature. Retaining the real signature over different ciphertext fails.
        injected["signature_b58"] = original["signature_b58"]
        with self.assertRaises(r.collector.Rejected):
            value.decrypt(injected)
        self.assertIsNotNone(value._key)
        with self.assertRaises(r.collector.Rejected):
            value.decrypt(original["response"])
        self.assertEqual(value.decrypt(original), b"k" * 32)

    def test_untrusted_broker_signer_rejected(self):
        value, _, _, _ = make_recipient()
        value.collect(challenge(value))
        reply = response(value)
        attacker = ed25519.Ed25519PrivateKey.generate()
        reply["broker_pubkey_b58"] = b58encode(attacker.public_key().public_bytes(
            serialization.Encoding.Raw, serialization.PublicFormat.Raw))
        reply["signature_b58"] = b58encode(attacker.sign(
            r.RESPONSE_SIGNATURE_DOMAIN + r.collector.canonical(reply["response"])))
        with self.assertRaises(r.collector.Rejected):
            value.decrypt(reply)
        self.assertIsNotNone(value._key)

    def test_oaep_label_binding_and_wrong_key_length_fail(self):
        for size, label in [(32, b"wrong-request"), (31, None), (33, None)]:
            value, _, _, _ = make_recipient()
            value.collect(challenge(value))
            with self.subTest(size=size, label=label), self.assertRaises(r.collector.Rejected):
                value.decrypt(response(value, b"k" * size, label=label))
            self.assertIsNone(value._key)

    def test_strict_duplicate_float_and_unicode_refused(self):
        with self.assertRaises(r.collector.Rejected):
            r.collector.strict_json('{"ttl_ms":1,"ttl_ms":2}')
        with self.assertRaises(r.collector.Rejected):
            r.collector.strict_json('{"ttl_ms":1.0}')
        with self.assertRaises(r.collector.Rejected):
            r.collector.canonical({"tenant_id": "\u00e6"})

    def test_shared_cross_language_challenge_vector(self):
        vector = json.loads((Path(__file__).parent / "challenge-vector.json").read_text())
        self.assertEqual(r.collector.canonical(vector["challenge"]).decode(), vector["canonical_json"])
        self.assertEqual(r.digest(vector["challenge"]), vector["challenge_sha256"])
        self.assertEqual(r.report_data(vector["challenge"]).hex(), vector["report_data_hex"])


class DeliveryTests(unittest.TestCase):
    def test_delivery_mode_and_owner_and_no_overwrite(self):
        with tempfile.TemporaryDirectory() as directory:
            os.chmod(directory, 0o700)
            result = r.deliver(b"x" * 32, workspace=directory, uid=os.getuid(), gid=os.getgid())
            path = Path(directory) / r.KEY_FILENAME
            self.assertEqual(path.read_bytes(), b"x" * 32)
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(result["status"], "delivered")
            with self.assertRaises(FileExistsError):
                r.deliver(b"y" * 32, workspace=directory, uid=os.getuid(), gid=os.getgid())
            self.assertEqual(path.read_bytes(), b"x" * 32)

    def test_symlink_destination_cannot_exfiltrate_or_overwrite(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "disk-canary"
            target.write_bytes(b"safe")
            (Path(directory) / r.KEY_FILENAME).symlink_to(target)
            with self.assertRaises(FileExistsError):
                r.deliver(b"x" * 32, workspace=directory, uid=os.getuid(), gid=os.getgid())
            self.assertEqual(target.read_bytes(), b"safe")

    def test_workspace_symlink_and_unsafe_permissions_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "workspace"
            target.mkdir(mode=0o700)
            link = Path(directory) / "link"
            link.symlink_to(target)
            with self.assertRaises(OSError):
                r.deliver(b"x" * 32, workspace=link, uid=os.getuid(), gid=os.getgid())
            target.chmod(0o755)
            with self.assertRaises(r.collector.Rejected):
                r.deliver(b"x" * 32, workspace=target, uid=os.getuid(), gid=os.getgid())


if __name__ == "__main__":
    unittest.main()
