"""covenant-blockrun — a trust receipt for every BlockRun x402 call.

Wrap your BlockRun client's httpx transport with :class:`ReceiptTransport` and
each paid call emits a :class:`CallReceipt` binding the requested model, the
model actually served, the input/output hashes, and the settlement — the same
receipt the Covenant daemon and ``@covenant-org/blockrun`` produce, verifiable
with :func:`verify_receipt`.
"""

from .challenge import decode_challenge, pick_accept
from .decorator import OnReceipt, ReceiptTransport
from .jcs import canonicalize
from .receipt import (
    PROVIDER,
    VERDICT_DELIVERED,
    VERDICT_SUBSTITUTED,
    VERDICT_UNVERIFIED,
    CallReceipt,
    PaymentInfo,
    RoutingClaim,
    build_receipt,
    canonical_sha256_hex,
    payment_from_accept,
    verdict_for,
    verify_receipt,
)

__version__ = "0.1.0"

__all__ = [
    "CallReceipt",
    "PaymentInfo",
    "RoutingClaim",
    "ReceiptTransport",
    "OnReceipt",
    "build_receipt",
    "verify_receipt",
    "verdict_for",
    "canonical_sha256_hex",
    "canonicalize",
    "payment_from_accept",
    "decode_challenge",
    "pick_accept",
    "PROVIDER",
    "VERDICT_DELIVERED",
    "VERDICT_SUBSTITUTED",
    "VERDICT_UNVERIFIED",
    "__version__",
]
