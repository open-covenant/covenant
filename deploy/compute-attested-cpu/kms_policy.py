#!/usr/bin/env python3
"""Read-only KMS identity evidence; never releases keys or grants hardware trust.

Optional operator dependency: cryptography>=42. The collector image does not
import this module. See kms-policy-review.md for the remaining policy gate.
"""
import argparse
import base64
import datetime
import hashlib
import http.client
import json
from pathlib import Path
import re
import ssl
import time

import client
import collector

HOST = "kms.dstack-pha-prod5.phala.network"
PATH = "/prpc/KMS.GetMeta"
MAX_BODY = 64 * 1024
MAX_CERT = 128 * 1024
PLAN_FIELDS = {
    "schema_version", "app_id", "workload_sha256", "expected_ca_spki_hex",
}
USAGE_OID = "1.3.6.1.4.1.62397.1.4"
# dstack stores the special usage as a DER OCTET STRING inside extnValue.
KMS_ROLE_DER = b"\x04\x07kms:rpc"


class ReviewError(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise ReviewError(message)


def validate_plan(plan):
    require(type(plan) is dict and set(plan) == PLAN_FIELDS, "unexpected review plan")
    require(type(plan["schema_version"]) is int and plan["schema_version"] == 1,
            "unsupported review schema")
    for field, width in (("app_id", 40), ("workload_sha256", 64),
                         ("expected_ca_spki_hex", 182)):
        require(type(plan[field]) is str and
                re.fullmatch("[0-9a-f]{" + str(width) + "}", plan[field]),
                "invalid " + field)
    return plan


def authenticate_peer(der, expected_spki_hex, host, now):
    """Verify the direct KMS CA signature and role before any HTTP request.

    A standard public-Web-PKI chain is not used by dstack KMS. The CA pin must
    come from the caller's separately reviewed/measured workload, never from
    this TLS connection or GetMeta response. Root pin alone is insufficient:
    that root also signs app CAs, so require direct signing and kms:rpc usage.
    This authenticates the KMS identity, not its embedded hardware attestation.
    """
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.x509.oid import ExtensionOID, ExtendedKeyUsageOID, ObjectIdentifier

    require(type(der) is bytes and 0 < len(der) <= MAX_CERT,
            "missing or oversized TLS certificate")
    require(host == HOST, "unsupported KMS host")
    require(type(expected_spki_hex) is str and
            re.fullmatch(r"[0-9a-f]{182}", expected_spki_hex), "invalid CA pin")
    require(now.tzinfo is not None, "certificate clock must be timezone aware")
    root_der = bytes.fromhex(expected_spki_hex)
    root = serialization.load_der_public_key(root_der)
    require(isinstance(root, ec.EllipticCurvePublicKey)
            and isinstance(root.curve, ec.SECP256R1), "CA pin must be P-256")
    require(root.public_bytes(serialization.Encoding.DER,
                              serialization.PublicFormat.SubjectPublicKeyInfo) == root_der,
            "noncanonical CA pin")
    cert = x509.load_der_x509_certificate(der)
    require(cert.signature_algorithm_oid == ObjectIdentifier("1.2.840.10045.4.3.2"),
            "unsupported certificate signature")
    root.verify(cert.signature, cert.tbs_certificate_bytes, ec.ECDSA(hashes.SHA256()))
    require(cert.not_valid_before_utc <= now < cert.not_valid_after_utc,
            "TLS certificate is not current")
    names = cert.extensions.get_extension_for_oid(
        ExtensionOID.SUBJECT_ALTERNATIVE_NAME).value.get_values_for_type(x509.DNSName)
    require(host in names, "TLS certificate hostname mismatch")
    require(cert.extensions.get_extension_for_oid(
        ExtensionOID.KEY_USAGE).value.digital_signature, "TLS signing usage missing")
    require(cert.extensions.get_extension_for_oid(
        ObjectIdentifier(USAGE_OID)).value.value == KMS_ROLE_DER, "KMS RPC role missing")
    for extension in cert.extensions:
        require(not extension.critical or extension.oid in {
            ExtensionOID.SUBJECT_ALTERNATIVE_NAME, ExtensionOID.KEY_USAGE,
            ExtensionOID.BASIC_CONSTRAINTS, ExtensionOID.EXTENDED_KEY_USAGE,
        }, "unsupported critical certificate extension")
    try:
        require(not cert.extensions.get_extension_for_oid(
            ExtensionOID.BASIC_CONSTRAINTS).value.ca, "server certificate is a CA")
    except x509.ExtensionNotFound:
        pass
    try:
        require(ExtendedKeyUsageOID.SERVER_AUTH in cert.extensions.get_extension_for_oid(
            ExtensionOID.EXTENDED_KEY_USAGE).value, "server authentication usage missing")
    except x509.ExtensionNotFound:
        pass
    return {
        "tls_leaf_sha256": hashlib.sha256(der).hexdigest(),
        "expected_ca_spki_sha256": hashlib.sha256(root_der).hexdigest(),
        "direct_ca_signature_verified": True,
        "kms_rpc_role_verified": True,
        "hostname_verified": True,
        "certificate_time_verified": True,
        "hardware_attestation_verified": False,
    }


def assess_metadata(meta, expected_spki_hex):
    """Inspect authenticated metadata without treating it as policy proof."""
    from cryptography import x509
    from cryptography.hazmat.primitives import serialization

    require(type(meta) is dict, "KMS metadata must be an object")
    require(type(meta.get("ca_cert")) is str and len(meta["ca_cert"]) <= 16 * 1024,
            "missing or oversized KMS CA certificate")
    pem = meta["ca_cert"].encode("ascii")
    require(pem.count(b"-----BEGIN CERTIFICATE-----") == 1
            and pem.count(b"-----END CERTIFICATE-----") == 1,
            "expected exactly one KMS CA certificate")
    root = x509.load_pem_x509_certificate(pem)
    actual = root.public_key().public_bytes(
        serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo)
    require(actual.hex() == expected_spki_hex, "metadata CA does not match expected pin")
    for field in ("is_dev", "allow_any_upgrade"):
        require(type(meta.get(field)) is bool, "missing or malformed " + field)
    image_check = meta.get("os_image_verification")
    require(image_check is None or type(image_check) is bool,
            "malformed os_image_verification")
    chain_id = meta.get("chain_id")
    require(chain_id is None or (type(chain_id) is int and 0 <= chain_id <= 2**63 - 1),
            "malformed chain_id")
    contract = meta.get("kms_contract_address")
    require(contract is None or (type(contract) is str and len(contract) <= 128),
            "malformed kms_contract_address")
    managed = chain_id == 0 and contract in (None, "", "phala")
    findings = []
    if meta["is_dev"] or meta["allow_any_upgrade"]:
        findings.append("development_or_unrestricted_upgrade_mode")
    if image_check is not True:
        findings.append("kms_os_image_verification_disabled" if image_check is False
                        else "kms_os_image_verification_unreported")
    findings.append("managed_authorization_policy_not_independently_verified" if managed
                    else "authorization_policy_not_independently_verified")
    return {
        "observed_backend": "managed_offchain" if managed else "unqualified",
        "is_dev": meta["is_dev"],
        "allow_any_upgrade": meta["allow_any_upgrade"],
        "os_image_verification": image_check,
        "chain_id": chain_id,
        "kms_contract_address": contract,
        "metadata_ca_matches_pin": True,
        "authorization_policy_verified": False,
        "production_policy_qualified": False,
        "served_trust_ceiling": "open",
        "findings": findings,
    }


def collect(plan, output):
    validate_plan(plan)
    # Fail before network activity if this optional operator dependency is absent.
    import cryptography  # noqa: F401

    metadata = {
        "schema_version": 1,
        "purpose": "kms_policy_review",
        "url": "https://" + HOST + PATH,
        "method": "GET",
        "plan": plan,
        "started_at_ms": int(time.time() * 1000),
        "automatic_retries": 0,
        "kms_identity_authenticated": False,
        "production_policy_qualified": False,
        "served_trust_ceiling": "open",
    }
    captured = b""
    failure = None
    with client.output_files(output) as (saved_body, saved_metadata):
        connection = None
        try:
            with client.deadline(30):
                context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
                # Authenticate explicitly below against the caller's measured CA
                # key and kms:rpc role. No HTTP is sent before this succeeds.
                context.check_hostname = False
                context.verify_mode = ssl.CERT_NONE
                context.minimum_version = ssl.TLSVersion.TLSv1_2
                connection = http.client.HTTPSConnection(
                    HOST, 443, timeout=8, context=context)
                connection.connect()
                der = connection.sock.getpeercert(binary_form=True)
                metadata["peer"] = authenticate_peer(
                    der, plan["expected_ca_spki_hex"], HOST,
                    datetime.datetime.now(datetime.timezone.utc))
                metadata["kms_identity_authenticated"] = True
                metadata["tls_leaf_der_base64"] = base64.b64encode(der).decode("ascii")
                connection.request("GET", PATH, headers={
                    "Accept": "application/json", "Accept-Encoding": "identity",
                    "Connection": "close",
                })
                response = connection.getresponse()
                metadata["http_status"] = response.status
                captured = response.read(MAX_BODY + 1)
                require(len(captured) <= MAX_BODY, "KMS response exceeds limit")
                require(response.getheader("Content-Encoding", "identity") == "identity",
                        "compressed KMS response rejected")
                require(response.status == 200, "KMS HTTP response was not successful")
                metadata["assessment"] = assess_metadata(
                    collector.strict_json(captured), plan["expected_ca_spki_hex"])
                metadata["outcome"] = "collected_policy_unqualified"
        except Exception as error:
            failure = error
            metadata["outcome"] = "review_failed"
            metadata["failure_kind"] = type(error).__name__
        finally:
            if connection is not None:
                connection.close()
            saved_body.write(captured)
            metadata["captured_body_sha256"] = hashlib.sha256(captured).hexdigest()
            metadata["captured_body_bytes"] = len(captured)
            metadata["finished_at_ms"] = int(time.time() * 1000)
            saved_metadata.write((json.dumps(metadata, indent=2) + "\n").encode())
    if failure is not None:
        raise ReviewError("KMS review failed; retained evidence; no retry performed") from failure
    return metadata


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plan", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    plan = collector.strict_json(client.private_read(args.plan, 4096))
    result = collect(plan, args.output)
    print(json.dumps({
        "kms_identity_authenticated": result["kms_identity_authenticated"],
        "production_policy_qualified": False,
        "assessment": result["assessment"],
    }, indent=2))
    # A successful GET is not a passing production-policy qualification.
    raise SystemExit(2)


if __name__ == "__main__":
    main()
