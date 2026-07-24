"""The Covenant receipt for a BlockRun paid call. Field names and hashing match
the ``covenant-blockrun`` Rust crate and ``@covenant-org/blockrun``, so a receipt
built here verifies anywhere.
"""

from __future__ import annotations

import hashlib
from dataclasses import dataclass, field
from typing import Any, Optional

from .jcs import canonicalize

PROVIDER = "blockrun"

VERDICT_DELIVERED = "delivered"
VERDICT_SUBSTITUTED = "substituted"
VERDICT_UNVERIFIED = "unverified"

USDC_DECIMALS = 6


@dataclass
class PaymentInfo:
    network: str = ""
    asset: str = ""
    amount: str = "0"
    amount_usdc: float = 0.0
    pay_to: str = ""
    tx: Optional[str] = None

    def to_json(self) -> dict[str, Any]:
        out: dict[str, Any] = {
            "network": self.network,
            "asset": self.asset,
            "amount": self.amount,
            "amountUsdc": self.amount_usdc,
            "payTo": self.pay_to,
        }
        if self.tx is not None:
            out["tx"] = self.tx
        return out


@dataclass
class RoutingClaim:
    profile: Optional[str] = None
    model: Optional[str] = None
    savings_pct: Optional[float] = None

    def is_empty(self) -> bool:
        return self.profile is None and self.model is None and self.savings_pct is None

    def to_json(self) -> dict[str, Any]:
        out: dict[str, Any] = {}
        if self.profile is not None:
            out["profile"] = self.profile
        if self.model is not None:
            out["model"] = self.model
        if self.savings_pct is not None:
            out["savingsPct"] = self.savings_pct
        return out


@dataclass
class CallReceipt:
    endpoint: str
    model_requested: str
    model_served: str
    verdict: str
    input_sha256: str
    output_sha256: str
    payment: PaymentInfo
    routing: RoutingClaim = field(default_factory=RoutingClaim)
    provider: str = PROVIDER

    def to_json(self) -> dict[str, Any]:
        out: dict[str, Any] = {
            "provider": self.provider,
            "endpoint": self.endpoint,
            "modelRequested": self.model_requested,
            "modelServed": self.model_served,
            "verdict": self.verdict,
            "inputSha256": self.input_sha256,
            "outputSha256": self.output_sha256,
            "payment": self.payment.to_json(),
        }
        if not self.routing.is_empty():
            out["routing"] = self.routing.to_json()
        return out

    def digest(self) -> str:
        return canonical_sha256_hex(self.to_json())

    def expected_verdict(self) -> str:
        return verdict_for(self.model_requested, self.model_served)


def canonical_sha256_hex(value: Any) -> str:
    return hashlib.sha256(canonicalize(value).encode("utf-8")).hexdigest()


def verdict_for(requested: str, served: str) -> str:
    if not served:
        return VERDICT_UNVERIFIED
    return VERDICT_DELIVERED if _model_base(requested) == _model_base(served) else VERDICT_SUBSTITUTED


def _model_base(model: str) -> str:
    """Model name without a provider namespace: openai/gpt-4o-mini -> gpt-4o-mini."""
    return model.lower().rsplit("/", 1)[-1]


def build_receipt(
    endpoint: str,
    request: Any,
    response: Any,
    payment: PaymentInfo,
    routing: Optional[RoutingClaim] = None,
) -> CallReceipt:
    routing = routing or RoutingClaim()
    model_requested = _read_model(request) or ""
    model_served = _read_model(response) or (routing.model or "")
    return CallReceipt(
        endpoint=endpoint,
        model_requested=model_requested,
        model_served=model_served,
        verdict=verdict_for(model_requested, model_served),
        input_sha256=canonical_sha256_hex(request),
        output_sha256=canonical_sha256_hex(response),
        payment=payment,
        routing=routing,
    )


def verify_receipt(receipt: CallReceipt, expected_digest: Optional[str] = None) -> dict[str, Any]:
    """Re-check a receipt offline: recompute its digest and confirm the stated
    verdict matches the requested/served pair."""
    digest = receipt.digest()
    expected_verdict = verdict_for(receipt.model_requested, receipt.model_served)
    verdict_consistent = expected_verdict == receipt.verdict
    digest_matches = None if expected_digest is None else expected_digest == digest
    return {
        "valid": verdict_consistent and (True if digest_matches is None else digest_matches),
        "digest": digest,
        "digestMatches": digest_matches,
        "verdictConsistent": verdict_consistent,
        "statedVerdict": receipt.verdict,
        "expectedVerdict": expected_verdict,
    }


def payment_from_accept(accept: dict[str, Any], tx: Optional[str] = None) -> PaymentInfo:
    amount = str(accept.get("amount", "0"))
    try:
        usdc = int(amount) / (10**USDC_DECIMALS)
    except ValueError:
        usdc = 0.0
    return PaymentInfo(
        network=str(accept.get("network", "")),
        asset=str(accept.get("asset", "")),
        amount=amount,
        amount_usdc=usdc,
        pay_to=str(accept.get("payTo", "")),
        tx=tx,
    )


def _read_model(value: Any) -> Optional[str]:
    if isinstance(value, dict):
        m = value.get("model")
        return m if isinstance(m, str) else None
    return None
