"""Verify exact signed Covenant bytes; never reserialize bytes before hashing."""
from __future__ import annotations

import hashlib
import json
import math
from typing import Any

import jcs
from solders.pubkey import Pubkey
from solders.signature import Signature


class CovenantError(ValueError):
    """A purchase failed a validation, trust, or budget boundary."""


def require(condition: Any, message: str) -> None:
    if not condition:
        raise CovenantError(message)


def sha256(value: str | bytes) -> str:
    return hashlib.sha256(value.encode("utf-8") if isinstance(value, str) else value).hexdigest()


def _check(value: Any, depth: int = 0) -> None:
    require(depth < 128, "json_too_deep")
    if isinstance(value, str):
        try:
            value.encode("utf-8", errors="strict")
        except UnicodeError as exc:
            raise CovenantError("invalid_utf8") from exc
    elif isinstance(value, (int, float)) and not isinstance(value, bool):
        require(math.isfinite(float(value)), "invalid_json_number")
    elif isinstance(value, dict):
        for key, item in value.items():
            require(isinstance(key, str), "invalid_json_key")
            _check(key, depth + 1)
            _check(item, depth + 1)
    elif isinstance(value, list):
        for item in value:
            _check(item, depth + 1)
    else:
        require(value is None or isinstance(value, bool), "invalid_json_value")


def stable(value: Any) -> str:
    """The JS client canonical form: UTF-16 key order and ECMAScript numbers."""
    _check(value)
    return jcs.canonicalize(value).decode("utf-8")


def compact(value: Any) -> str:
    """Compact UTF-8 JSON retaining insertion order (for request byte limits)."""
    _check(value)
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), allow_nan=False)


def strict_json(source: str | bytes) -> Any:
    def pairs(items: list[tuple[str, Any]]) -> dict:
        result: dict = {}
        for key, value in items:
            require(key not in result, "duplicate_json_key")
            result[key] = value
        return result

    try:
        if isinstance(source, bytes):
            source = source.decode("utf-8", errors="strict")
        # Parse finite IEEE-754 numbers like JavaScript. Safe-integer guards below
        # retain the native signed protocol's numeric limits.
        value = json.loads(source, object_pairs_hook=pairs, parse_int=float,
                           parse_constant=lambda _: require(False, "invalid_json_number"))
        _check(value)
        return value
    except CovenantError:
        raise
    except (ValueError, UnicodeError, RecursionError, OverflowError) as exc:
        raise CovenantError("invalid_json") from exc


def safe_integer(value: Any, minimum: int = 0) -> bool:
    return (isinstance(value, (int, float)) and not isinstance(value, bool)
            and minimum <= value <= 9_007_199_254_740_991
            and math.isfinite(value) and int(value) == value)


def _signed(wrapper: dict, field: str, domain: str, actor: str) -> dict:
    require(isinstance(wrapper, dict) and isinstance(wrapper.get(field + "_json"), str), "missing_signed_bytes")
    raw = wrapper[field + "_json"]
    payload = strict_json(raw)
    require(stable(payload) == stable(wrapper.get(field)), "signed_payload_mismatch")
    require(payload.get(actor, {}).get("pubkey") == wrapper.get("signer_pubkey_b58"), "signed_identity_mismatch")
    try:
        key = Pubkey.from_string(wrapper["signer_pubkey_b58"])
        signature = Signature.from_string(wrapper["signature_b58"])
    except (ValueError, TypeError, KeyError) as exc:
        raise CovenantError("invalid_signature_encoding") from exc
    require(signature.verify(key, (domain + raw).encode("utf-8")), "invalid_signature")
    return payload


def verify_compute_result(result: dict, *, request: dict, request_sha256: str,
                          quote_id: str, gateway_buyer: str, operators: list[str]) -> dict:
    try:
        proof = result["covenant"]
        require(proof.get("request_sha256") == request_sha256 and proof.get("quote_id") == quote_id, "result_request_binding_mismatch")
        job = _signed(proof["envelope"], "payload", "covenant.compute.job.v1\n", "buyer")
        receipt = _signed(proof["receipt"], "receipt", "covenant.compute.receipt.v1\n", "operator")
        require(job["buyer"]["pubkey"] == gateway_buyer, "unexpected_gateway_buyer")
        require(receipt["operator"]["pubkey"] in operators, "unexpected_operator")
        require(job.get("kind") == "inference_call" and job.get("capability_requirement", {}).get("model_id") == request["model"], "job_model_mismatch")
        require(receipt.get("job_id") == job["job_id"] == proof.get("job_id"), "receipt_job_id_mismatch")
        require(receipt.get("job_hash_hex") == sha256(proof["envelope"]["payload_json"]), "receipt_job_hash_mismatch")
        require(isinstance(proof.get("output_json"), str) and receipt.get("result_hash_hex") == sha256(proof["output_json"]), "receipt_output_hash_mismatch")
        output = strict_json(proof["output_json"])
        require(stable(output) == stable(proof.get("output")), "output_bytes_mismatch")
        blocks = job.get("input")
        require(isinstance(blocks, list) and len(blocks) == 2, "invalid_job_input")
        chats = [b for b in blocks if b.get("type") == "json" and "messages" in b.get("value", {})]
        generations = [b for b in blocks if b.get("type") == "json" and "generation" in b.get("value", {})]
        require(len(chats) == 1 and stable(chats[0]["value"]["messages"]) == stable(request["messages"]), "job_messages_mismatch")
        require(len(generations) == 1, "job_generation_missing")
        generation = generations[0]["value"]["generation"]
        require(set(generation) <= {"max_tokens", "temperature"}, "unexpected_generation_parameters")
        require(generation.get("max_tokens") == request["max_tokens"], "job_token_limit_mismatch")
        require(generation.get("temperature") == request.get("temperature"), "job_temperature_mismatch")
        require(safe_integer(job.get("price_micro_usdc"), 1) and safe_integer(receipt.get("price_micro_usdc")) and receipt["price_micro_usdc"] <= job["price_micro_usdc"], "receipt_price_mismatch")
        require(receipt.get("status") == "ok", "compute_job_not_completed")
        text = "\n".join(b["text"] for b in output if b.get("type") == "text")
        require(text and result["choices"][0]["message"]["content"] == text, "completion_output_mismatch")
        return {"receipt": receipt, "job": job, "text": text}
    except (KeyError, TypeError, IndexError, AttributeError) as exc:
        raise CovenantError("malformed_compute_result") from exc
