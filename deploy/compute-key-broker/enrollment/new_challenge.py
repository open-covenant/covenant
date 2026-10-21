#!/usr/bin/env python3
"""Create a local one-time enrollment challenge from independently reviewed inputs."""
import argparse
import json
import os
import secrets
import time
from pathlib import Path
from verify import EXPECTED_FIELDS, hex_bytes, public_key, read_json


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    try:
        value = read_json(args.manifest)
        if set(value) != EXPECTED_FIELDS - {"nonce_hex", "challenge_created_at_ms"}:
            raise ValueError(
                "manifest must contain exactly the independent enrollment expectations"
            )
        if type(value["protocol_version"]) is not int or value["protocol_version"] != 1:
            raise ValueError("unsupported protocol version")
        public_key(value["coordinator_pubkey_b58"])
        for field, size in (
            ("approved_pcr0", 48),
            ("approved_pcr3", 48),
            ("wrapped_key_sha256", 32),
        ):
            hex_bytes(value[field], size, field)
        value["nonce_hex"] = secrets.token_hex(32)
        value["challenge_created_at_ms"] = int(time.time() * 1000)
        fd = os.open(args.output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w") as handle:
            json.dump(value, handle, indent=2)
            handle.write("\n")
        print("One-time challenge created; it expires in 120 seconds. No AWS calls.")
    except Exception as exc:
        parser.exit(1, "Challenge creation rejected: " + str(exc) + "\n")


if __name__ == "__main__":
    main()
