"""Verify exact signed Covenant bytes; never reserialize bytes before hashing."""
from __future__ import annotations

import hashlib
import json
import math
import re
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


def _verify_tariff(job: dict, request: dict, blocks: list) -> None:
    tariffs = [b for b in blocks if b.get("type") == "json" and "inference_tariff" in b.get("value", {})]
    requirement = job["capability_requirement"].get("inference_tariff")
    if "pricing" not in request:
        require(not tariffs and requirement is None and len(blocks) == 2, "unexpected_inference_tariff")
        return
    require(request["pricing"] == "model_tariff_v1", "invalid_pricing")
    require(len(tariffs) == 1 and len(blocks) == 3, "inference_tariff_required")
    require(set(tariffs[0]["value"]) == {"inference_tariff"}, "invalid_inference_tariff_block")
    quote = tariffs[0]["value"]["inference_tariff"]
    require(isinstance(quote, dict) and set(quote) == {"tariff", "requested_output_cap", "price_micro_usdc"}, "invalid_inference_tariff_quote")
    require(stable(quote) == stable(requirement), "inference_tariff_requirement_mismatch")
    tariff = quote["tariff"]
    require(isinstance(tariff, dict) and set(tariff) == {"schema_version", "id", "revision", "model", "base_micro_usdc", "output_cap_micro_usdc_per_million", "max_output_tokens", "valid_from_ms", "valid_until_ms"}, "invalid_inference_tariff")
    require(safe_integer(tariff["schema_version"]) and tariff["schema_version"] == 1
            and safe_integer(tariff["revision"], 1)
            and isinstance(tariff["id"], str) and re.fullmatch(r"[A-Za-z0-9_.-]{1,128}", tariff["id"])
            and isinstance(tariff["model"], str) and tariff["model"].strip() and tariff["model"] != "any"
            and len(tariff["model"].encode("utf-8")) <= 128 and not re.search(r"[\x00-\x1f\x7f-\x9f]", tariff["model"])
            and safe_integer(tariff["base_micro_usdc"]) and safe_integer(tariff["output_cap_micro_usdc_per_million"])
            and safe_integer(tariff["max_output_tokens"], 1) and tariff["max_output_tokens"] <= 0xffff_ffff
            and safe_integer(tariff["valid_from_ms"]) and safe_integer(tariff["valid_until_ms"])
            and tariff["valid_until_ms"] > tariff["valid_from_ms"], "invalid_inference_tariff")
    require(tariff["model"].removesuffix(":latest") == request["model"].removesuffix(":latest"), "inference_tariff_model_mismatch")
    require(safe_integer(job.get("issued_at_ms")) and tariff["valid_from_ms"] <= job["issued_at_ms"] < tariff["valid_until_ms"], "inference_tariff_issue_time_mismatch")
    require(safe_integer(quote["requested_output_cap"], 1) and quote["requested_output_cap"] == request["max_tokens"] and quote["requested_output_cap"] <= tariff["max_output_tokens"], "inference_tariff_cap_mismatch")
    def price_for(cap):
        return int(tariff["base_micro_usdc"]) + (int(cap) * int(tariff["output_cap_micro_usdc_per_million"]) + 999_999) // 1_000_000
    require(price_for(tariff["max_output_tokens"]) <= 9_007_199_254_740_991
            and safe_integer(quote["price_micro_usdc"], 1)
            and price_for(quote["requested_output_cap"]) == quote["price_micro_usdc"] == job["price_micro_usdc"], "inference_tariff_price_mismatch")


def verify_compute_result(result: dict, *, request: dict, request_sha256: str,
                          quote_id: str, gateway_buyer: str, operators: list[str],
                          _batch_child: bool = False) -> dict:
    try:
        proof = result["covenant"]
        # Native batch children predate x402 metadata. Their ordered leaf, full
        # output hash and original request are checked by verify_batch_result.
        # Ordinary single requests always require their direct quote binding.
        if not _batch_child:
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
        require(isinstance(blocks, list), "invalid_job_input")
        _verify_tariff(job, request, blocks)
        chats = [b for b in blocks if b.get("type") == "json" and "messages" in b.get("value", {})]
        generations = [b for b in blocks if b.get("type") == "json" and "generation" in b.get("value", {})]
        require(len(chats) == 1 and stable(chats[0]["value"]["messages"]) == stable(request["messages"]), "job_messages_mismatch")
        require(len(generations) == 1, "job_generation_missing")
        require(chats[0] is not generations[0], "invalid_job_input")
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
