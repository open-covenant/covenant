"""Ordered RFC6962 batch proofs and individually signed native inference results."""
from __future__ import annotations

import hashlib
import re
from uuid import UUID

from .payment import short_memo
from .proof import CovenantError, require, safe_integer, sha256, stable, verify_compute_result

LEAF_DOMAIN = b"covenant.compute.inference-batch.leaf.v1\n"


def _uuid(value):
    try:
        return isinstance(value, str) and str(UUID(value)) == value and UUID(value).int != 0
    except (ValueError, TypeError, AttributeError):
        return False


def _digest(value):
    require(isinstance(value, str) and re.fullmatch(r"[a-f0-9]{64}", value), "invalid_batch_digest")
    return bytes.fromhex(value)


def _hash(value):
    return hashlib.sha256(value).digest()


def validate_batch_pricing(extra: dict, requests: list[dict], amount: int) -> None:
    """The retail manifest is part of the exact accepted payment authorization.

    Native operator tariffs may differ from retail prices. Verify the manifest
    against the buyer's request and total, never substitute native prices here.
    """
    manifest = extra.get("batchPricing")
    require(isinstance(manifest, dict) and set(manifest) == {"version", "batch_id", "children"}
            and safe_integer(manifest.get("version")) and manifest["version"] == 1 and _uuid(manifest["batch_id"]),
            "invalid_batch_pricing")
    children = manifest["children"]
    require(isinstance(children, list) and len(children) == len(requests), "batch_pricing_size_mismatch")
    total, identities = 0, set()
    for child, request in zip(children, requests):
        require(isinstance(child, dict) and {"job_id", "model", "max_output_tokens", "amount_atomic"} <= set(child)
                and set(child) <= {"job_id", "model", "max_output_tokens", "pricing", "amount_atomic"}
                and _uuid(child["job_id"]) and child["job_id"] not in identities, "invalid_batch_pricing_child")
        identities.add(child["job_id"])
        require(child["model"] == request["model"]
                and safe_integer(child["max_output_tokens"], 1)
                and child["max_output_tokens"] == request["max_tokens"]
                and child.get("pricing") == request.get("pricing"), "batch_pricing_request_mismatch")
        require(safe_integer(child["amount_atomic"], 1), "invalid_batch_pricing_amount")
        total += int(child["amount_atomic"])
        require(safe_integer(total, 1), "invalid_batch_pricing_amount")
    require(total == amount, "batch_pricing_total_mismatch")


def verify_batch_proof(result: dict, *, request_sha256=None, tree_size=None, batch_pricing=None) -> dict:
    """Require every ordered leaf in the version 1 protocol shape.

    The batch root proves membership and order, not output correctness or a
    settlement on its own. The client separately verifies the one USDC payment.
    """
    try:
        require(isinstance(result, dict) and set(result) == {"version", "batch_id", "request_sha256", "tree_size", "merkle_root", "items"}
                and safe_integer(result.get("version")) and result["version"] == 1
                and safe_integer(result.get("tree_size"), 1) and result["tree_size"] <= 32
                and isinstance(result.get("items"), list) and len(result["items"]) == result["tree_size"]
                and _uuid(result.get("batch_id")), "invalid_or_incomplete_batch")
        _digest(result["request_sha256"])
        root = _digest(result["merkle_root"])
        if request_sha256 is not None:
            require(result["request_sha256"] == request_sha256, "batch_request_mismatch")
        if tree_size is not None:
            require(tree_size == result["tree_size"], "batch_size_mismatch")
        if batch_pricing is not None:
            require(result["batch_id"] == batch_pricing["batch_id"], "batch_identity_mismatch")
        identities = set()
        for index, item in enumerate(result["items"]):
            require(isinstance(item, dict) and set(item) == {"leaf", "output", "audit_path"}, "invalid_batch_item")
            leaf = item["leaf"]
            require(isinstance(leaf, dict) and set(leaf) == {"version", "batch_id", "request_sha256", "index", "job_id", "result_sha256"}
                    and safe_integer(leaf.get("version")) and leaf["version"] == 1
                    and safe_integer(leaf.get("index")) and leaf["index"] == index
                    and leaf["batch_id"] == result["batch_id"] and leaf["request_sha256"] == result["request_sha256"]
                    and (batch_pricing is None or leaf["job_id"] == batch_pricing["children"][index]["job_id"])
                    and _uuid(leaf["job_id"]) and leaf["job_id"] not in identities, "batch_item_binding_mismatch")
            identities.add(leaf["job_id"])
            require(leaf["result_sha256"] == sha256(stable(item["output"])), "batch_output_hash_mismatch")
            value = _hash(b"\0" + LEAF_DOMAIN + stable(leaf).encode())
            position, last = index, int(result["tree_size"]) - 1
            require(isinstance(item["audit_path"], list) and len(item["audit_path"]) <= 5, "invalid_batch_proof")
            for encoded in item["audit_path"]:
                require(last != 0, "invalid_batch_proof")
                sibling = _digest(encoded)
                if position & 1 or position == last:
                    value = _hash(b"\1" + sibling + value)
                    if not position & 1:
                        while not position & 1 and position != 0:
                            position >>= 1
                            last >>= 1
                else:
                    value = _hash(b"\1" + value + sibling)
                position >>= 1
                last >>= 1
            require(last == 0 and value == root, "invalid_batch_proof")
        return result
    except (KeyError, TypeError, IndexError, AttributeError) as exc:
        raise CovenantError("malformed_batch_result") from exc


def verify_batch_result(result: dict, *, request: dict, request_sha256: str,
                        gateway_buyer: str, operators: list[str], quote: dict) -> dict:
    """Verify both ordered membership and independently signed child results."""
    try:
        digest = sha256(stable(request))
        require(request_sha256 == digest and quote["extra"].get("requestSha256") == digest,
                "batch_request_mismatch")
        quote_id = quote["extra"].get("quoteId")
        require(_uuid(quote_id) and quote["extra"].get("memo") in (
            short_memo(quote_id, digest), f"covenant.x402.v2:{quote_id}:{digest}"), "quote_memo_mismatch")
        require(isinstance(quote.get("amount"), str) and re.fullmatch(r"[1-9][0-9]{0,15}", quote["amount"]),
                "invalid_batch_pricing_amount")
        validate_batch_pricing(quote["extra"], request["requests"], int(quote["amount"]))
        batch_pricing = quote["extra"]["batchPricing"]
        verify_batch_proof(result, request_sha256=request_sha256, tree_size=len(request["requests"]),
                           batch_pricing=batch_pricing)
        verified = []
        for index, item in enumerate(result["items"]):
            leaf = item["leaf"]
            child_request = request["requests"][index]
            child = verify_compute_result(item["output"], request=child_request,
                                          request_sha256=sha256(stable(child_request)), quote_id="",
                                          gateway_buyer=gateway_buyer, operators=operators, _batch_child=True)
            require(child["job"]["job_id"] == leaf["job_id"]
                    and child["job"]["capability_requirement"].get("kind") == "inference_call"
                    and ("stream" not in child["job"] or child["job"]["stream"] is False), "batch_native_request_mismatch")
            verified.append(child)
        return {"texts": [child["text"] for child in verified],
                "jobs": [child["job"] for child in verified],
                "receipts": [child["receipt"] for child in verified]}
    except (KeyError, TypeError, IndexError, AttributeError) as exc:
        raise CovenantError("malformed_batch_result") from exc
