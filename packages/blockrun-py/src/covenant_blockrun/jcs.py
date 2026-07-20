"""RFC 8785 JSON Canonicalization, matching the Rust ``serde_jcs`` the
``covenant-blockrun`` crate hashes with and the ``@covenant-org/blockrun`` TS
package, so a receipt's digest is identical across all three.

Object keys are sorted by UTF-16 code unit, arrays keep their order, and there
is no insignificant whitespace. For the ASCII strings and simple numbers a
BlockRun receipt carries, this coincides with the strict spec.
"""

from __future__ import annotations

import json
from decimal import Decimal
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
        # Exact for any magnitude. JS/Rust route JSON numbers through f64 and so
        # round integers above 2**53; a receipt never carries one (ids are
        # strings, counts are small), so this latent gap is left documented.
        return str(value)
    if isinstance(value, float):
        if value != value or value in (float("inf"), float("-inf")):
            raise ValueError("cannot canonicalize a non-finite number")
        return _es_number(value)
    if isinstance(value, (list, tuple)):
        return "[" + ",".join(_encode(v) for v in value) + "]"
    if isinstance(value, dict):
        items = [(k, v) for k, v in value.items() if v is not None]
        # RFC 8785 sorts keys by UTF-16 code unit. Rust serde_jcs and JS both do
        # this; a plain code-point sort would disagree on supplementary-plane
        # keys (emoji etc.) that can appear in a hashed request/response body.
        items.sort(key=lambda kv: kv[0].encode("utf-16-be"))
        return (
            "{"
            + ",".join(
                json.dumps(k, ensure_ascii=False) + ":" + _encode(v) for k, v in items
            )
            + "}"
        )
    raise TypeError(f"cannot canonicalize value of type {type(value).__name__}")


def _es_number(value: float) -> str:
    """Format a finite float as ECMAScript Number::toString, which is what RFC
    8785 (and Rust serde_jcs via ryu_js, and JS JSON.stringify) use. Python's
    json.dumps differs on integral floats ("78.0") and on the exponent
    thresholds ("1e-05" vs "0.00001"), so those must be handled here."""
    if value == 0:  # also -0.0, which ECMAScript renders as "0"
        return "0"
    sign = "-" if value < 0 else ""
    # repr gives the shortest round-trip decimal; Decimal exposes its digits.
    _, digits, exp = Decimal(repr(abs(value))).as_tuple()
    digs = list(digits)
    while len(digs) > 1 and digs[-1] == 0:  # strip trailing zeros
        digs.pop()
        exp += 1
    while len(digs) > 1 and digs[0] == 0:  # strip leading zeros
        digs.pop(0)
    sig = "".join(str(d) for d in digs)
    k = len(sig)
    n = exp + k  # value == sig * 10**exp == 0.sig * 10**n
    if k <= n <= 21:
        return sign + sig + "0" * (n - k)
    if 0 < n <= 21:
        return sign + sig[:n] + "." + sig[n:]
    if -6 < n <= 0:
        return sign + "0." + "0" * (-n) + sig
    e = n - 1
    e_str = ("+" if e >= 0 else "-") + str(abs(e))
    mantissa = sig if k == 1 else sig[0] + "." + sig[1:]
    return sign + mantissa + "e" + e_str
