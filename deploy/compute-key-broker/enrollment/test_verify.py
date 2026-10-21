"""Synthetic PKI tests exercise validation; they do not qualify AWS hardware."""

import base64
import copy
import hashlib
import json
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest.mock import patch

import cbor2
from cryptography import x509
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import ec, utils
from cryptography.hazmat.primitives.serialization import Encoding
from cryptography.x509.oid import NameOID

import verify


def b58(raw):
    num = int.from_bytes(raw, "big")
    out = ""
    while num:
        num, digit = divmod(num, 58)
        out = verify.BASE58[digit] + out
    return "1" * (len(raw) - len(raw.lstrip(b"\0"))) + out


def cert(
    key,
    name,
    issuer_key,
    issuer,
    now,
    ca,
    path_length=None,
    expired=False,
    unknown_critical=False,
):
    builder = (
        x509.CertificateBuilder()
        .subject_name(name)
        .issuer_name(issuer)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - timedelta(days=2))
        .not_valid_after(
            now - timedelta(seconds=1) if expired else now + timedelta(days=1)
        )
    )
    builder = (
        builder.add_extension(
            x509.BasicConstraints(ca=ca, path_length=path_length), critical=True
        )
        .add_extension(
            x509.KeyUsage(
                digital_signature=True,
                content_commitment=False,
                key_encipherment=False,
                data_encipherment=False,
                key_agreement=False,
                key_cert_sign=ca,
                crl_sign=ca,
                encipher_only=False,
                decipher_only=False,
            ),
            critical=True,
        )
        .add_extension(
            x509.SubjectKeyIdentifier.from_public_key(key.public_key()), critical=False
        )
        .add_extension(
            x509.AuthorityKeyIdentifier.from_issuer_public_key(issuer_key.public_key()),
            critical=False,
        )
    )
    if unknown_critical:
        builder = builder.add_extension(
            x509.UnrecognizedExtension(
                x509.ObjectIdentifier("1.2.3.4.5"), b"unsupported"
            ),
            critical=True,
        )
    return builder.sign(issuer_key, hashes.SHA384())


class VerifyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.now_ms = 1800000000000
        self.now = datetime.fromtimestamp(self.now_ms / 1000, timezone.utc)
        self.root_key = ec.generate_private_key(ec.SECP384R1())
        self.ca_key = ec.generate_private_key(ec.SECP384R1())
        self.leaf_key = ec.generate_private_key(ec.SECP384R1())
        self.root_name = x509.Name(
            [x509.NameAttribute(NameOID.COMMON_NAME, "synthetic test root")]
        )
        self.ca_name = x509.Name(
            [x509.NameAttribute(NameOID.COMMON_NAME, "synthetic intermediate")]
        )
        self.leaf_name = x509.Name(
            [x509.NameAttribute(NameOID.COMMON_NAME, "synthetic Nitro leaf")]
        )
        self.root = cert(
            self.root_key,
            self.root_name,
            self.root_key,
            self.root_name,
            self.now,
            True,
            1,
        )
        self.ca = cert(
            self.ca_key, self.ca_name, self.root_key, self.root_name, self.now, True, 0
        )
        self.leaf = cert(
            self.leaf_key, self.leaf_name, self.ca_key, self.ca_name, self.now, False
        )
        self.root_file = Path(self.temp.name) / "synthetic-root.pem"
        self.root_file.write_bytes(self.root.public_bytes(Encoding.PEM))
        # Unit tests only: production CLI has no custom-root or custom-time flags.
        for item in (
            patch.object(verify, "ROOT_PATH", self.root_file),
            patch.object(
                verify, "ROOT_FINGERPRINT", self.root.fingerprint(hashes.SHA256()).hex()
            ),
        ):
            item.start()
            self.addCleanup(item.stop)
        self.enrollment = {
            "protocol_version": 1,
            "broker_id": "test-broker",
            "coordinator_pubkey_b58": b58(bytes(range(1, 33))),
            "kms_key_arn": "arn:aws:kms:eu-north-1:999988887777:key/12345678-1234-1234-1234-123456789abc",
            "wrapped_key_sha256": hashlib.sha256(b"synthetic wrapped seed").hexdigest(),
            "public_key_b58": b58(bytes(range(32))),
            "nonce_hex": hashlib.sha256(b"test nonce").hexdigest(),
        }
        self.expected = {
            k: v for k, v in self.enrollment.items() if k != "public_key_b58"
        }
        self.expected.update(
            approved_pcr0=hashlib.sha384(b"approved image").hexdigest(),
            approved_pcr3=hashlib.sha384(b"approved parent").hexdigest(),
            challenge_created_at_ms=self.now_ms - 1000,
        )
        self.document = {
            "module_id": "synthetic-nitro-module",
            "timestamp": self.now_ms,
            "digest": "SHA384",
            "pcrs": {
                0: bytes.fromhex(self.expected["approved_pcr0"]),
                3: bytes.fromhex(self.expected["approved_pcr3"]),
            },
            "certificate": self.leaf.public_bytes(Encoding.DER),
            "cabundle": [
                self.root.public_bytes(Encoding.DER),
                self.ca.public_bytes(Encoding.DER),
            ],
            "public_key": verify.public_key(self.enrollment["public_key_b58"]),
            "nonce": bytes.fromhex(self.enrollment["nonce_hex"]),
            "user_data": hashlib.sha384(
                verify.DOMAIN + verify.canonical_enrollment(self.enrollment)
            ).digest(),
        }

    def envelope(self, document=None, protected=None, tagged=False):
        protected = cbor2.dumps({1: -35}) if protected is None else protected
        payload = cbor2.dumps(document or self.document)
        signature_der = self.leaf_key.sign(
            cbor2.dumps(["Signature1", protected, b"", payload]),
            ec.ECDSA(hashes.SHA384()),
        )
        r, s = utils.decode_dss_signature(signature_der)
        value = [protected, {}, payload, r.to_bytes(48, "big") + s.to_bytes(48, "big")]
        if tagged:
            value = cbor2.CBORTag(18, value)
        return {
            "enrollment": copy.deepcopy(self.enrollment),
            "attestation_document_base64": base64.b64encode(
                cbor2.dumps(value)
            ).decode(),
        }

    def run_verify(self, envelope=None, expected=None):
        return verify.verify(
            envelope or self.envelope(), expected or self.expected, self.now_ms
        )

    def test_valid_chain_cose_and_binding(self):
        result = self.run_verify()
        self.assertEqual(
            result["status"], "nitro-enrollment-verified-not-production-qualified"
        )
        self.assertEqual(result["enrollment"], self.enrollment)
        self.run_verify(self.envelope(tagged=True))

    def test_aws_root_pin_is_correct(self):
        root = x509.load_pem_x509_certificate(
            Path(verify.__file__).with_name("aws-nitro-root-g1.pem").read_bytes()
        )
        self.assertEqual(
            root.fingerprint(hashes.SHA256()).hex(),
            "641a0321a3e244efe456463195d606317ed7cdcc3c1756e09893f3c68f79bb5b",
        )

    def test_wrong_expected_fields_rejected(self):
        for field in (
            "broker_id",
            "kms_key_arn",
            "coordinator_pubkey_b58",
            "wrapped_key_sha256",
            "nonce_hex",
        ):
            with self.subTest(field=field):
                expected = dict(self.expected)
                expected[field] += "x"
                with self.assertRaises(ValueError):
                    self.run_verify(expected=expected)

    def test_wrong_missing_zero_pcrs(self):
        for register in (0, 3):
            for bad in (None, bytes(48), b"x" * 48):
                with self.subTest(register=register, bad=bad):
                    document = copy.deepcopy(self.document)
                    if bad is None:
                        document["pcrs"].pop(register)
                    else:
                        document["pcrs"][register] = bad
                    with self.assertRaises(ValueError):
                        self.run_verify(self.envelope(document))

    def test_nonce_key_user_data_tampering(self):
        for field in ("nonce", "public_key", "user_data"):
            document = copy.deepcopy(self.document)
            document[field] = b"wrong"
            with self.assertRaises(ValueError):
                self.run_verify(self.envelope(document))

    def test_unattested_enrollment_public_key_rejected(self):
        envelope = self.envelope()
        envelope["enrollment"]["public_key_b58"] = b58(b"x" * 32)
        with self.assertRaises(ValueError):
            self.run_verify(envelope)

    def test_stale_future_prechallenge_timestamp(self):
        for timestamp in (
            self.now_ms - 120001,
            self.now_ms + 5001,
            self.now_ms - 1001,
            True,
        ):
            document = copy.deepcopy(self.document)
            document["timestamp"] = timestamp
            with self.assertRaises(ValueError):
                self.run_verify(self.envelope(document))

    def test_expired_future_challenge(self):
        for created in (self.now_ms - 120001, self.now_ms + 1, True):
            expected = dict(self.expected, challenge_created_at_ms=created)
            with self.assertRaises(ValueError):
                self.run_verify(expected=expected)

    def test_bad_signature(self):
        envelope = self.envelope()
        blob = bytearray(base64.b64decode(envelope["attestation_document_base64"]))
        blob[-1] ^= 1
        envelope["attestation_document_base64"] = base64.b64encode(blob).decode()
        with self.assertRaises(Exception):
            self.run_verify(envelope)

    def test_untrusted_or_missing_chain(self):
        document = copy.deepcopy(self.document)
        document["cabundle"] = document["cabundle"][1:]
        with self.assertRaises(ValueError):
            self.run_verify(self.envelope(document))

    def test_expired_leaf(self):
        document = copy.deepcopy(self.document)
        document["certificate"] = cert(
            self.leaf_key,
            self.leaf_name,
            self.ca_key,
            self.ca_name,
            self.now,
            False,
            expired=True,
        ).public_bytes(Encoding.DER)
        with self.assertRaises(ValueError):
            self.run_verify(self.envelope(document))

    def test_wrong_issuer_signature(self):
        document = copy.deepcopy(self.document)
        document["certificate"] = cert(
            self.leaf_key, self.leaf_name, self.root_key, self.ca_name, self.now, False
        ).public_bytes(Encoding.DER)
        with self.assertRaises(Exception):
            self.run_verify(self.envelope(document))

    def test_unknown_critical_extension(self):
        document = copy.deepcopy(self.document)
        document["certificate"] = cert(
            self.leaf_key,
            self.leaf_name,
            self.ca_key,
            self.ca_name,
            self.now,
            False,
            unknown_critical=True,
        ).public_bytes(Encoding.DER)
        with self.assertRaises(Exception):
            self.run_verify(self.envelope(document))

    def test_non_ca_intermediate_rejected(self):
        document = copy.deepcopy(self.document)
        document["cabundle"][1] = cert(
            self.ca_key, self.ca_name, self.root_key, self.root_name, self.now, False
        ).public_bytes(Encoding.DER)
        with self.assertRaises(Exception):
            self.run_verify(self.envelope(document))

    def test_ca_leaf_rejected(self):
        document = copy.deepcopy(self.document)
        document["certificate"] = cert(
            self.leaf_key, self.leaf_name, self.ca_key, self.ca_name, self.now, True
        ).public_bytes(Encoding.DER)
        with self.assertRaises(ValueError):
            self.run_verify(self.envelope(document))

    def test_cose_algorithm_confusion(self):
        with self.assertRaises(ValueError):
            self.run_verify(self.envelope(protected=cbor2.dumps({1: -7})))

    def test_duplicate_or_indefinite_cbor_rejected(self):
        for blob in (
            bytes.fromhex("a2013822013822"),
            bytes.fromhex("bf01ff"),
            bytes.fromhex("82f5f4"),
            cbor2.dumps({"x": 1}) + b"\x00",
        ):
            with self.subTest(blob=blob):
                with self.assertRaises(ValueError):
                    verify.StrictCbor(blob).decode()

    def test_duplicate_json_rejected(self):
        with self.assertRaises(ValueError):
            json.loads('{"a":1,"a":2}', object_pairs_hook=verify.no_duplicate_json)

    def test_cli_consumes_challenge_and_refuses_replay(self):
        folder = Path(self.temp.name)
        proof = folder / "proof.json"
        challenge = folder / "challenge.json"
        receipt = folder / "receipt.json"
        proof.write_text(json.dumps(self.envelope()))
        challenge.write_text(json.dumps(self.expected))
        args = [
            "verify.py",
            "--proof",
            str(proof),
            "--challenge",
            str(challenge),
            "--receipt",
            str(receipt),
        ]
        with patch("sys.argv", args), patch.object(
            verify.time, "time", return_value=self.now_ms / 1000
        ):
            verify.main()
        self.assertTrue(challenge.with_name("challenge.json.consumed").exists())
        self.assertTrue(receipt.exists())
        args[-1] = str(folder / "second-receipt.json")
        with patch("sys.argv", args), self.assertRaises(SystemExit) as error:
            verify.main()
        self.assertEqual(error.exception.code, 1)
        self.assertFalse((folder / "second-receipt.json").exists())

    def test_size_and_extra_field_limits(self):
        with self.assertRaises(ValueError):
            verify.StrictCbor(b"x" * 32769)
        expected = dict(self.expected, ignored="evil")
        with self.assertRaises(ValueError):
            self.run_verify(expected=expected)


if __name__ == "__main__":
    unittest.main()
