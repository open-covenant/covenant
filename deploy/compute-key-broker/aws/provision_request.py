#!/usr/bin/env python3
"""Prepare one ciphertext-only key creation request. Does not call AWS."""
import argparse
import json
from pathlib import Path
from render import SIGNING_PURPOSE, validate, require

FIELDS = {"tenant_id", "key_id", "key_version", "owner_pubkey_b58", "kms_key_arn"}


def request(config, metadata, *, key_kind="application"):
    validate(config)
    if key_kind not in ("application", "response-signing"):
        raise ValueError("invalid key kind")
    signing = key_kind == "response-signing"
    if config["policy_phase"] == "signing_enrollment" and not signing:
        raise ValueError("signing_enrollment permits only the reserved signing context")
    if set(metadata) != FIELDS:
        raise ValueError(
            "metadata must contain exactly tenant_id/key_id/key_version/owner_pubkey_b58/kms_key_arn"
        )
    for field in ("tenant_id", "key_id"):
        require(r"[A-Za-z0-9._:/-]{1,128}", metadata[field], field)
    version = metadata["key_version"]
    if type(version) is not int or not 1 <= version <= 9007199254740991:
        raise ValueError("key_version must be a positive interoperable JSON integer")
    require(
        r"[1-9A-HJ-NP-Za-km-z]{32,44}", metadata["owner_pubkey_b58"], "owner_pubkey_b58"
    )
    require(
        r"arn:aws:kms:"
        + config["region"]
        + ":"
        + config["account_id"]
        + r":key/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
        metadata["kms_key_arn"],
        "kms_key_arn (aliases forbidden)",
    )
    if signing and (
        metadata["tenant_id"] != config["broker_id"]
        or metadata["key_id"] != "broker-response-signing"
        or version != 1
    ):
        raise ValueError(
            "signing enrollment requires reserved broker tenant/key/version"
        )
    if not signing and metadata["key_id"] == SIGNING_PURPOSE:
        raise ValueError("reserved signing key ID is not an application key")
    return {
        "KeyId": metadata["kms_key_arn"],
        "NumberOfBytes": 32,
        "EncryptionContext": {
            "covenant:protocol": "application-key-v1",
            "covenant:broker_id": config["broker_id"],
            "covenant:purpose": SIGNING_PURPOSE if signing else config["purpose"],
            "covenant:tenant_id": metadata["tenant_id"],
            "covenant:key_id": metadata["key_id"],
            "covenant:key_version": str(version),
            "covenant:owner_pubkey_b58": metadata["owner_pubkey_b58"],
        },
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("--metadata", required=True, type=Path)
    parser.add_argument(
        "--key-kind", choices=("application", "response-signing"), default="application"
    )
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    try:
        result = request(
            json.loads(args.config.read_text()),
            json.loads(args.metadata.read_text()),
            key_kind=args.key_kind,
        )
        with args.output.open("x") as handle:
            json.dump(result, handle, indent=2)
            handle.write("\n")
        print(
            "Prepared GenerateDataKeyWithoutPlaintext request; no AWS call or key created."
        )
    except (ValueError, OSError) as exc:
        parser.error(str(exc))


if __name__ == "__main__":
    main()
