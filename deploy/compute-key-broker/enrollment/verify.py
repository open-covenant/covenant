#!/usr/bin/env python3
"""Offline Nitro signing-key enrollment verifier. Never calls AWS or accepts a supplied trust root."""
import argparse
import base64
import hashlib
import json
import os
import re
import time
from datetime import datetime, timezone
from pathlib import Path

import cbor2
from OpenSSL import crypto
from cryptography import x509
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import ec, utils

ROOT_FINGERPRINT = "641a0321a3e244efe456463195d606317ed7cdcc3c1756e09893f3c68f79bb5b"
ROOT_PATH = Path(__file__).with_name("aws-nitro-root-g1.pem")
DOMAIN = b"covenant.compute.key-broker.signing-enrollment.v1\0"
ENROLLMENT_FIELDS = {
    "protocol_version",
    "broker_id",
    "coordinator_pubkey_b58",
    "kms_key_arn",
    "wrapped_key_sha256",
    "public_key_b58",
    "nonce_hex",
}
EXPECTED_FIELDS = (ENROLLMENT_FIELDS - {"public_key_b58"}) | {
    "approved_pcr0",
    "approved_pcr3",
    "challenge_created_at_ms",
}
BASE58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def fail(message):
    raise ValueError(message)


def no_duplicate_json(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            fail("duplicate JSON key")
        result[key] = value
    return result


def read_json(path):
    with path.open("rb") as handle:
        data = handle.read(100001)
    if len(data) > 100000:
        fail("JSON exceeds size limit")
    return json.loads(
        data,
        object_pairs_hook=no_duplicate_json,
        parse_constant=lambda _: fail("non-finite JSON number"),
    )


class StrictCbor:
    """Bounded definite-length CBOR subset used by Nitro, preserving duplicate-key rejection.

    This syntax gate rejects aliases/tags/floats/indefinite forms outside the
    attestation schema and bounds every container before allocating; cbor2 is
    only used to encode the exact COSE Sig_structure, not to parse hostile maps.
    """

    def __init__(self, data):
        if type(data) is not bytes or len(data) > 32768:
            fail("CBOR exceeds size/type limit")
        self.data, self.offset = data, 0

    def take(self, size):
        if size < 0 or self.offset + size > len(self.data):
            fail("truncated CBOR")
        result = self.data[self.offset : self.offset + size]
        self.offset += size
        return result

    def item(self, depth=0):
        if depth > 12:
            fail("CBOR nesting limit")
        first = self.take(1)[0]
        major, additional = first >> 5, first & 31
        if additional < 24:
            size = additional
        elif additional in (24, 25, 26, 27):
            size = int.from_bytes(self.take(1 << (additional - 24)), "big")
        else:
            fail("indefinite/reserved CBOR is forbidden")
        if major == 0:
            return size
        if major == 1:
            return -1 - size
        if major in (2, 3):
            value = self.take(size)
            return value if major == 2 else value.decode("utf-8", "strict")
        if major == 4:
            if size > 64:
                fail("CBOR array size limit")
            return [self.item(depth + 1) for _ in range(size)]
        if major == 5:
            if size > 64:
                fail("CBOR map size limit")
            result = {}
            for _ in range(size):
                key = self.item(depth + 1)
                if type(key) not in (str, int) or key in result:
                    fail("invalid or duplicate CBOR map key")
                result[key] = self.item(depth + 1)
            return result
        if major == 6 and size == 18 and depth == 0:
            return self.item(depth + 1)
        fail("unsupported CBOR type/tag")

    def decode(self):
        result = self.item()
        if self.offset != len(self.data):
            fail("trailing CBOR bytes")
        return result


def public_key(value):
    if (
        not isinstance(value, str)
        or not 32 <= len(value) <= 44
        or any(c not in BASE58 for c in value)
    ):
        fail("invalid base58 public key")
    number = 0
    for char in value:
        number = number * 58 + BASE58.index(char)
    decoded = b"\0" * (len(value) - len(value.lstrip("1"))) + (
        number.to_bytes((number.bit_length() + 7) // 8, "big") if number else b""
    )
    if len(decoded) != 32 or decoded == bytes(32):
        fail("public key must be nonzero 32 bytes")
    return decoded


def hex_bytes(value, size, label):
    if (
        not isinstance(value, str)
        or re.fullmatch("[0-9a-f]{" + str(size * 2) + "}", value) is None
    ):
        fail("invalid " + label)
    result = bytes.fromhex(value)
    if result == bytes(size):
        fail("zero " + label)
    return result


def canonical_enrollment(enrollment):
    if not isinstance(enrollment, dict) or set(enrollment) != ENROLLMENT_FIELDS:
        fail("unexpected enrollment fields")
    if (
        type(enrollment["protocol_version"]) is not int
        or enrollment["protocol_version"] != 1
    ):
        fail("unsupported enrollment protocol")
    for key, value in enrollment.items():
        if key != "protocol_version" and (
            not isinstance(value, str)
            or not 1 <= len(value) <= 512
            or not value.isascii()
            or any(ord(char) < 32 for char in value)
        ):
            fail("enrollment strings must be bounded ASCII")
    public_key(enrollment["coordinator_pubkey_b58"])
    public_key(enrollment["public_key_b58"])
    hex_bytes(enrollment["nonce_hex"], 32, "nonce")
    hex_bytes(enrollment["wrapped_key_sha256"], 32, "ciphertext hash")
    if (
        re.fullmatch(
            r"arn:aws:kms:[a-z]{2}-[a-z]+-[1-9][0-9]*:[0-9]{12}:key/[0-9a-f-]{36}",
            enrollment["kms_key_arn"],
        )
        is None
    ):
        fail("an immutable commercial-partition KMS key ARN is required")
    # Every key/value is ASCII and the only number is integer 1, so this is JCS.
    return json.dumps(
        enrollment, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    ).encode("utf-8")


def certificate_chain(document, now_ms):
    root = x509.load_pem_x509_certificate(ROOT_PATH.read_bytes())
    if root.fingerprint(hashes.SHA256()).hex() != ROOT_FINGERPRINT:
        fail("bundled AWS trust root does not match pinned DER fingerprint")
    bundle = document["cabundle"]
    if not isinstance(bundle, list) or not 1 <= len(bundle) <= 8:
        fail("invalid certificate bundle length")
    cert_bytes = [document["certificate"]] + bundle
    if any(
        type(item) is not bytes or not 1 <= len(item) <= 4096 for item in cert_bytes
    ) or len(set(cert_bytes)) != len(cert_bytes):
        fail("invalid or duplicate certificate")
    certs = [x509.load_der_x509_certificate(item) for item in cert_bytes]
    if certs[1].fingerprint(hashes.SHA256()).hex() != ROOT_FINGERPRINT:
        fail("cabundle must start with pinned AWS root")
    current = datetime.fromtimestamp(now_ms / 1000, timezone.utc)
    for cert in certs:
        if current < cert.not_valid_before_utc or current > cert.not_valid_after_utc:
            fail("certificate outside current validity period")
    leaf = certs[0]
    if leaf.extensions.get_extension_for_class(x509.BasicConstraints).value.ca:
        fail("attestation leaf must not be a CA")
    usage = leaf.extensions.get_extension_for_class(x509.KeyUsage).value
    if not usage.digital_signature or usage.key_cert_sign:
        fail("attestation leaf key usage invalid")
    leaf_key = leaf.public_key()
    if not isinstance(leaf_key, ec.EllipticCurvePublicKey) or not isinstance(
        leaf_key.curve, ec.SECP384R1
    ):
        fail("attestation leaf must use P-384")
    store = crypto.X509Store()
    store.add_cert(crypto.X509.from_cryptography(root))
    store.set_flags(
        crypto.X509StoreFlags.X509_STRICT | crypto.X509StoreFlags.CHECK_SS_SIGNATURE
    )
    store.set_time(current)
    context = crypto.X509StoreContext(
        store,
        crypto.X509.from_cryptography(leaf),
        [crypto.X509.from_cryptography(cert) for cert in certs[2:]],
    )
    verified = context.get_verified_chain()
    if verified[-1].to_cryptography().fingerprint(
        hashes.SHA256()
    ).hex() != ROOT_FINGERPRINT or len(verified) != len(certs):
        fail("certificate path did not consume the expected pinned chain")
    return leaf_key


def verify(envelope, expected, now_ms=None):
    # now_ms is injectable for deterministic unit tests; CLI always uses its own clock.
    now_ms = int(time.time() * 1000) if now_ms is None else now_ms
    if not isinstance(expected, dict) or set(expected) != EXPECTED_FIELDS:
        fail("unexpected independently supplied challenge fields")
    created = expected["challenge_created_at_ms"]
    if type(created) is not int or not now_ms - 120000 <= created <= now_ms:
        fail("challenge expired or created in the future")
    if not isinstance(envelope, dict) or set(envelope) != {
        "enrollment",
        "attestation_document_base64",
    }:
        fail("unexpected enrollment envelope")
    enrollment = envelope["enrollment"]
    encoded = canonical_enrollment(enrollment)
    for key in ENROLLMENT_FIELDS - {"public_key_b58"}:
        if (
            type(expected[key]) is not type(enrollment[key])
            or expected[key] != enrollment[key]
        ):
            fail("enrollment does not match independent expected " + key)
    pcr0 = hex_bytes(expected["approved_pcr0"], 48, "approved PCR0")
    pcr3 = hex_bytes(expected["approved_pcr3"], 48, "approved PCR3")
    text = envelope["attestation_document_base64"]
    if not isinstance(text, str) or len(text) > 44000:
        fail("attestation exceeds size/type limit")
    blob = base64.b64decode(text, validate=True)
    cose = StrictCbor(blob).decode()
    if not isinstance(cose, list) or len(cose) != 4:
        fail("COSE_Sign1 must have four fields")
    protected, unprotected, payload, signature = cose
    if (
        type(protected) is not bytes
        or StrictCbor(protected).decode() != {1: -35}
        or unprotected != {}
    ):
        fail("COSE headers must require only protected ES384")
    if (
        type(payload) is not bytes
        or type(signature) is not bytes
        or len(signature) != 96
    ):
        fail("invalid COSE payload/signature")
    document = StrictCbor(payload).decode()
    fields = {
        "module_id",
        "timestamp",
        "digest",
        "pcrs",
        "certificate",
        "cabundle",
        "public_key",
        "user_data",
        "nonce",
    }
    if not isinstance(document, dict) or set(document) != fields:
        fail("unexpected Nitro attestation fields")
    if (
        not isinstance(document["module_id"], str)
        or not 1 <= len(document["module_id"]) <= 512
        or document["digest"] != "SHA384"
    ):
        fail("invalid Nitro module/digest")
    timestamp = document["timestamp"]
    if (
        type(timestamp) is not int
        or timestamp < created
        or timestamp < now_ms - 120000
        or timestamp > now_ms + 5000
    ):
        fail("stale, pre-challenge or future attestation")
    pcrs = document["pcrs"]
    if (
        not isinstance(pcrs, dict)
        or not 2 <= len(pcrs) <= 32
        or any(
            type(k) is not int
            or not 0 <= k <= 31
            or type(v) is not bytes
            or len(v) != 48
            for k, v in pcrs.items()
        )
    ):
        fail("invalid Nitro PCRs")
    if pcrs.get(0) != pcr0 or pcrs.get(3) != pcr3:
        fail("Nitro measurement or parent role mismatch")
    if (
        document["nonce"] != bytes.fromhex(enrollment["nonce_hex"])
        or document["public_key"] != public_key(enrollment["public_key_b58"])
        or document["user_data"] != hashlib.sha384(DOMAIN + encoded).digest()
    ):
        fail("Nitro attestation is not bound to the expected enrollment")
    signer = certificate_chain(document, now_ms)
    signed = cbor2.dumps(["Signature1", protected, b"", payload])
    r = int.from_bytes(signature[:48], "big")
    s = int.from_bytes(signature[48:], "big")
    signer.verify(utils.encode_dss_signature(r, s), signed, ec.ECDSA(hashes.SHA384()))
    return {
        "status": "nitro-enrollment-verified-not-production-qualified",
        "verified_at_ms": now_ms,
        "root_fingerprint_sha256": ROOT_FINGERPRINT,
        "attestation_sha256": hashlib.sha256(blob).hexdigest(),
        "approved_pcr0": expected["approved_pcr0"],
        "approved_pcr3": expected["approved_pcr3"],
        "enrollment": enrollment,
    }


def write_exclusive_json(path, value):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w") as handle:
        json.dump(value, handle, indent=2)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    directory = os.open(path.parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--proof", required=True, type=Path)
    parser.add_argument("--challenge", required=True, type=Path)
    parser.add_argument("--receipt", required=True, type=Path)
    args = parser.parse_args()
    try:
        if args.receipt.exists():
            fail("receipt already exists")
        marker = args.challenge.with_name(args.challenge.name + ".consumed")
        if marker.exists():
            fail("challenge already consumed")
        result = verify(read_json(args.proof), read_json(args.challenge))
        # Exclusive marker consumes a successful challenge before receipt writing.
        # If writing fails, preserve the consumed state and create a fresh challenge.
        write_exclusive_json(
            marker,
            {
                "attestation_sha256": result["attestation_sha256"],
                "verified_at_ms": result["verified_at_ms"],
            },
        )
        write_exclusive_json(args.receipt, result)
        print(
            "Nitro enrollment verified and challenge consumed. Production qualification remains separate."
        )
    except Exception as exc:
        parser.exit(1, "Enrollment rejected: " + str(exc) + "\n")


if __name__ == "__main__":
    main()
