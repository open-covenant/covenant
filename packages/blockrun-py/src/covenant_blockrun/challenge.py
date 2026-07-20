"""Decoding BlockRun's x402 402 challenge (base64 in the ``x-payment-required``
header, mirrored in ``www-authenticate: X402 requirements="…"``)."""

from __future__ import annotations

import base64
import json
from typing import Any, Optional


def decode_challenge(header_value: str) -> dict[str, Any]:
    blob = _extract_requirements(header_value)
    raw = base64.b64decode(blob.strip())
    parsed = json.loads(raw.decode("utf-8"))
    if not isinstance(parsed.get("accepts"), list):
        parsed["accepts"] = []
    return parsed


def pick_accept(challenge: dict[str, Any], network: Optional[str] = None) -> Optional[dict[str, Any]]:
    accepts = challenge.get("accepts") or []
    if not accepts:
        return None
    if network:
        for a in accepts:
            if a.get("network") == network:
                return a
    return accepts[0]


def _extract_requirements(value: str) -> str:
    idx = value.find("requirements=")
    if idx >= 0:
        return value[idx + len("requirements=") :].strip().strip('"')
    return value.strip()
