"""RFC 8785 JSON Canonicalization, matching the Rust ``serde_jcs`` the
``covenant-blockrun`` crate hashes with and the ``@covenant-org/blockrun`` TS
package, so a receipt's digest is identical across all three.

Object keys are sorted by UTF-16 code unit, arrays keep their order, and there
is no insignificant whitespace. For the ASCII strings and simple numbers a
BlockRun receipt carries, this coincides with the strict spec.
"""

from __future__ import annotations

import json
from typing import Any


def canonicalize(value: Any) -> str:
    return _encode(value)


def _encode(value: Any) -> str:
    if value is None:
        return "null"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, str):
        return json.dumps(value, ensure_ascii=False)
    if isinstance(value, int):
        return str(value)
    if isinstance(value, float):
        if value != value or value in (float("inf"), float("-inf")):
            raise ValueError("cannot canonicalize a non-finite number")
        # RFC 8785 uses ECMAScript number-to-string: an integral float is an
        # integer (78.0 -> "78", -0.0 -> "0"), which json.dumps does not do
        # (it keeps "78.0"). Rust serde_jcs and JS JSON.stringify both drop the
        # ".0", so Python must too or the digest diverges. For non-integral
        # values in the range a receipt carries, json.dumps already matches the
        # ECMAScript shortest round-trip.
        if value.is_integer():
            return str(int(value))
        return json.dumps(value)
    if isinstance(value, (list, tuple)):
        return "[" + ",".join(_encode(v) for v in value) + "]"
    if isinstance(value, dict):
        items = [(k, v) for k, v in value.items() if v is not None]
        # Code-point sort; coincides with the JS/Rust UTF-16 code-unit sort for
        # the BMP keys a receipt uses.
        items.sort(key=lambda kv: kv[0])
        return (
            "{"
            + ",".join(
                json.dumps(k, ensure_ascii=False) + ":" + _encode(v) for k, v in items
            )
            + "}"
        )
    raise TypeError(f"cannot canonicalize value of type {type(value).__name__}")
