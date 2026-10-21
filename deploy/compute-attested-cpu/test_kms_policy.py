import datetime
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import kms_policy as k

try:
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.x509.oid import ExtensionOID, NameOID, ObjectIdentifier
    CRYPTO = True
except ImportError:
    CRYPTO = False

NOW = datetime.datetime(2026, 10, 20, tzinfo=datetime.timezone.utc)


class PlanTests(unittest.TestCase):
    def test_plan_cannot_choose_network_destination_or_coerce_version(self):
        plan = {"schema_version": 1, "app_id": "a" * 40,
                "workload_sha256": "b" * 64, "expected_ca_spki_hex": "c" * 182}
        self.assertEqual(k.validate_plan(plan), plan)
        for change in ({"host": "evil.example"}, {"schema_version": True},
                       {"app_id": "A" * 40}, {"expected_ca_spki_hex": "c" * 180}):
            with self.subTest(change=change), self.assertRaises(k.ReviewError):
                k.validate_plan({**plan, **change})


@unittest.skipUnless(CRYPTO, "optional KMS operator tests require cryptography>=42")
class CryptoTests(unittest.TestCase):
    def setUp(self):
        self.root = ec.generate_private_key(ec.SECP256R1())
        self.leaf = ec.generate_private_key(ec.SECP256R1())
        self.name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "test KMS root")])
        self.pin = self.root.public_key().public_bytes(
            serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo).hex()
        self.plan = {"schema_version": 1, "app_id": "a" * 40,
                     "workload_sha256": "b" * 64, "expected_ca_spki_hex": self.pin}

    def certificate(self, signer=None, host=k.HOST, role=k.KMS_ROLE_DER,
                    end=None, ca=False, unknown_critical=False):
        builder = (x509.CertificateBuilder()
                   .subject_name(x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, host)]))
                   .issuer_name(self.name).public_key(self.leaf.public_key())
                   .serial_number(1).not_valid_before(NOW - datetime.timedelta(days=1))
                   .not_valid_after(end or NOW + datetime.timedelta(days=1))
                   .add_extension(x509.SubjectAlternativeName([x509.DNSName(host)]), False)
                   .add_extension(x509.KeyUsage(True, False, False, False, False,
                                               ca, ca, None, None), True)
                   .add_extension(x509.BasicConstraints(ca, None), True))
        if role is not None:
            builder = builder.add_extension(
                x509.UnrecognizedExtension(ObjectIdentifier(k.USAGE_OID), role), False)
        if unknown_critical:
            builder = builder.add_extension(
                x509.UnrecognizedExtension(ObjectIdentifier("1.2.3.4"), b"x"), True)
        return builder.sign(signer or self.root, hashes.SHA256()).public_bytes(
            serialization.Encoding.DER)

    def root_pem(self, key=None):
        key = key or self.root
        return (x509.CertificateBuilder().subject_name(self.name).issuer_name(self.name)
                .public_key(key.public_key()).serial_number(2)
                .not_valid_before(NOW - datetime.timedelta(days=1))
                .not_valid_after(NOW + datetime.timedelta(days=1))
                .add_extension(x509.BasicConstraints(True, None), True)
                .sign(key, hashes.SHA256()).public_bytes(serialization.Encoding.PEM).decode())

    def test_direct_pin_and_role_authenticate_identity_without_hardware_claim(self):
        result = k.authenticate_peer(self.certificate(), self.pin, k.HOST, NOW)
        self.assertTrue(result["direct_ca_signature_verified"])
        self.assertTrue(result["kms_rpc_role_verified"])
        self.assertFalse(result["hardware_attestation_verified"])

    def test_rejects_other_signer_app_role_hostname_expiry_and_ca(self):
        cases = [
            {"signer": ec.generate_private_key(ec.SECP256R1())},
            {"role": b"\x04\x0aapp:custom"}, {"role": None},
            {"host": "other.example"}, {"host": "*.phala.network"},
            {"end": NOW}, {"ca": True}, {"unknown_critical": True},
        ]
        for arguments in cases:
            with self.subTest(arguments=arguments), self.assertRaises(Exception):
                k.authenticate_peer(self.certificate(**arguments), self.pin, k.HOST, NOW)

    def test_managed_metadata_records_disabled_image_check_and_remains_unqualified(self):
        meta = {"ca_cert": self.root_pem(), "is_dev": False,
                "allow_any_upgrade": False, "os_image_verification": False,
                "chain_id": 0, "kms_contract_address": ""}
        result = k.assess_metadata(meta, self.pin)
        self.assertEqual(result["observed_backend"], "managed_offchain")
        self.assertIn("kms_os_image_verification_disabled", result["findings"])
        self.assertFalse(result["production_policy_qualified"])
        self.assertFalse(result["authorization_policy_verified"])
        # Even positive self-reported flags cannot establish external policy.
        meta.update(os_image_verification=True, chain_id=8453,
                    kms_contract_address="0x" + "1" * 40)
        self.assertFalse(k.assess_metadata(meta, self.pin)["production_policy_qualified"])
        del meta["os_image_verification"]
        self.assertIn("kms_os_image_verification_unreported",
                      k.assess_metadata(meta, self.pin)["findings"])
        meta["os_image_verification"] = "false"
        with self.assertRaises(k.ReviewError):
            k.assess_metadata(meta, self.pin)

    def test_metadata_cannot_substitute_root_or_claim_boolean_with_integer(self):
        meta = {"ca_cert": self.root_pem(), "is_dev": False, "allow_any_upgrade": False}
        for update in ({"ca_cert": self.root_pem(ec.generate_private_key(ec.SECP256R1()))},
                       {"is_dev": 0}, {"allow_any_upgrade": "false"}, {"chain_id": False}):
            with self.subTest(update=list(update)), self.assertRaises(k.ReviewError):
                k.assess_metadata({**meta, **update}, self.pin)

    def test_failed_peer_verification_sends_no_http_and_preserves_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            connection = mock.Mock()
            connection.sock.getpeercert.return_value = self.certificate(role=None)
            output = Path(directory) / "meta.json"
            with mock.patch.object(k.http.client, "HTTPSConnection", return_value=connection), \
                    self.assertRaises(k.ReviewError):
                k.collect(self.plan, output)
            connection.request.assert_not_called()
            evidence = json.loads(Path(str(output) + ".metadata.json").read_text())
            self.assertFalse(evidence["kms_identity_authenticated"])
            self.assertFalse(evidence["production_policy_qualified"])
            self.assertEqual(output.read_bytes(), b"")

    def test_redirect_is_not_followed_after_authenticated_connection(self):
        with tempfile.TemporaryDirectory() as directory:
            connection = mock.Mock()
            connection.sock.getpeercert.return_value = self.certificate()
            response = connection.getresponse.return_value
            response.status = 302
            response.read.return_value = b"redirect"
            response.getheader.return_value = "identity"
            output = Path(directory) / "meta.json"
            with mock.patch.object(k.http.client, "HTTPSConnection", return_value=connection), \
                    mock.patch.object(k, "authenticate_peer", return_value={"test": True}), \
                    self.assertRaises(k.ReviewError):
                k.collect(self.plan, output)
            self.assertEqual(connection.request.call_count, 1)
            args, kwargs = connection.request.call_args
            self.assertEqual(args, ("GET", "/prpc/KMS.GetMeta"))
            self.assertNotIn("Authorization", kwargs["headers"])
            evidence = json.loads(Path(str(output) + ".metadata.json").read_text())
            self.assertTrue(evidence["kms_identity_authenticated"])
            self.assertEqual(evidence["http_status"], 302)
            self.assertEqual(evidence["outcome"], "review_failed")

    def test_successful_capture_stays_unqualified_and_oversize_is_bounded(self):
        body = json.dumps({"ca_cert": self.root_pem(), "is_dev": False,
                           "allow_any_upgrade": False, "os_image_verification": False,
                           "chain_id": 0, "kms_contract_address": ""}).encode()
        for payload, succeeds in ((body, True), (b"x" * (k.MAX_BODY + 1), False)):
            with self.subTest(succeeds=succeeds), tempfile.TemporaryDirectory() as directory:
                connection = mock.Mock()
                connection.sock.getpeercert.return_value = self.certificate()
                response = connection.getresponse.return_value
                response.status = 200
                response.read.return_value = payload
                response.getheader.return_value = "identity"
                output = Path(directory) / "meta.json"
                with mock.patch.object(k.http.client, "HTTPSConnection", return_value=connection), \
                        mock.patch.object(k, "authenticate_peer", return_value={"test": True}):
                    if succeeds:
                        result = k.collect(self.plan, output)
                        self.assertFalse(result["production_policy_qualified"])
                        self.assertFalse(result["assessment"]["production_policy_qualified"])
                        self.assertEqual(result["outcome"], "collected_policy_unqualified")
                    else:
                        with self.assertRaises(k.ReviewError):
                            k.collect(self.plan, output)
                response.read.assert_called_once_with(k.MAX_BODY + 1)
                self.assertEqual(connection.request.call_count, 1)
                self.assertEqual(output.read_bytes(), payload)


if __name__ == "__main__":
    unittest.main()
