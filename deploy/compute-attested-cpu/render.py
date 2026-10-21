#!/usr/bin/env python3
"""Render local candidate Compose; no API calls, creation, signing or secrets."""
import argparse
import base64
import hashlib
import re
from pathlib import Path
import time

import collector


def render(template, digest, policy, now_ms):
    collector.require(re.fullmatch(r"sha256:[0-9a-f]{64}", digest) is not None,
                      "an immutable image digest is required")
    collector.load_policy(policy, now_ms)
    collector.require(policy["lease_expires_at_ms"] <= now_ms + 3_600_000,
                      "initial acceptance is bounded to one hour")
    encoded = base64.b64encode(collector.canonical(policy)).decode()
    result = template.replace("__IMAGE_DIGEST__", digest[7:])
    result = result.replace("__LEASE_POLICY_B64__", encoded)
    result = result.replace("__JOB_ID__", policy["job_id"])
    collector.require("__IMAGE_DIGEST__" not in result and "__LEASE_POLICY_B64__" not in result
                      and "__JOB_ID__" not in result,
                      "unresolved template")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image-digest", required=True)
    parser.add_argument("--policy", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    policy = collector.strict_json(args.policy.read_bytes())
    template = Path(__file__).with_name("compose.template.yaml").read_text()
    content = render(template, args.image_digest, policy, int(time.time() * 1000))
    # Never overwrite an already reviewed candidate.
    with args.output.open("x") as output:
        output.write(content)
    print("compose_yaml_sha256=" + hashlib.sha256(content.encode()).hexdigest())
    print("This is YAML only. Retain/hash exact provider app-compose bytes separately.")


if __name__ == "__main__":
    main()
