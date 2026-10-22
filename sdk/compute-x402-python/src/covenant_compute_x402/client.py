"""Durable bounded x402 inference, with no automatic re-signing after uncertainty."""
from __future__ import annotations

import base64
from decimal import Decimal
import math
import re
from urllib.parse import urlsplit, urlunsplit

import httpx2 as httpx
from solders.pubkey import Pubkey
from solders.signature import Signature
from solders.message import to_bytes_versioned
from solders.transaction import VersionedTransaction
from x402.mechanisms.svm.utils import derive_ata

from .journal import Journal, now_ms
from .batch import validate_batch_pricing, verify_batch_result
from .payment import (NETWORK, SOLANA_GENESIS, USDC, TOKEN_PROGRAM, create_payment,
                      decode64, short_memo, solana_quote, validate_transaction, wallet_address)
from .proof import (CovenantError, compact, require, safe_integer, sha256, stable,
                    strict_json, verify_compute_result)

UUID_PATTERN = re.compile(r"^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$")


def _url(value: str, local: bool) -> str:
    require(isinstance(value, str) and not any(ord(c) <= 32 or ord(c) == 127 for c in value), "invalid_url")
    parsed = urlsplit(value)
    require(parsed.hostname and not parsed.username and not parsed.password and not parsed.fragment, "invalid_url")
    require(parsed.scheme == "https" or local and parsed.scheme == "http" and parsed.hostname in {"localhost", "127.0.0.1", "::1"}, "https_required")
    host = parsed.hostname.lower().encode("idna").decode("ascii")
    if ":" in host:
        host = f"[{host}]"
    if parsed.port and parsed.port != (443 if parsed.scheme == "https" else 80):
        host += f":{parsed.port}"
    return urlunsplit((parsed.scheme, host, parsed.path or "/", parsed.query, ""))


def validate_request(model, messages, max_tokens, temperature=None, pricing=None) -> dict:
    def length(text):
        require(isinstance(text, str), "invalid_request_string")
        try:
            return len(text.encode("utf-16-le")) // 2
        except UnicodeError as exc:
            raise CovenantError("invalid_utf8") from exc

    require(1 <= length(model) <= 200, "invalid_model")
    require(isinstance(messages, list) and 1 <= len(messages) <= 32, "invalid_messages")
    clean = []
    for message in messages:
        require(isinstance(message, dict) and set(message) == {"role", "content"}, "invalid_message_fields")
        require(message["role"] in {"system", "user", "assistant"} and 1 <= length(message["content"]) <= 64_000, "invalid_message")
        clean.append({"role": message["role"], "content": message["content"]})
    require(safe_integer(max_tokens, 1) and max_tokens <= 8192, "invalid_max_tokens")
    request = {"model": model, "messages": clean, "max_tokens": int(max_tokens)}
    if pricing is not None:
        require(pricing == "model_tariff_v1", "invalid_pricing")
        request["pricing"] = pricing
    if temperature is not None:
        require(isinstance(temperature, (int, float)) and not isinstance(temperature, bool) and math.isfinite(temperature) and 0 <= temperature <= 2 and Decimal(str(temperature)) % Decimal("0.01") == 0, "invalid_temperature")
        request["temperature"] = temperature
    return request


class CovenantX402Client:
    def __init__(self, *, gateway_url, rpc_url, pay_to, gateway_buyer, operators, wallet,
                 journal_path, max_payment_micro_usdc, max_total_micro_usdc,
                 max_input_bytes=4096, timeout_ms=30_000, allow_localhost=False, transport=None):
        gateway = urlsplit(_url(gateway_url, allow_localhost))
        require(gateway.path == "/" and not gateway.query, "gateway_origin_required")
        self.endpoint = urlunsplit((gateway.scheme, gateway.netloc, "/v1/chat/completions", "", ""))
        self.batch_endpoint = urlunsplit((gateway.scheme, gateway.netloc, "/v1/inference/batches", "", ""))
        self.rpc_url = _url(rpc_url, allow_localhost)
        Pubkey.from_string(pay_to); Pubkey.from_string(gateway_buyer)
        require(isinstance(operators, list) and len(operators) > 0, "trusted_operator_keys_required")
        for key in operators:
            Pubkey.from_string(key)
        require(safe_integer(max_payment_micro_usdc, 1) and safe_integer(max_total_micro_usdc, 1), "invalid_spend_cap")
        require(max_payment_micro_usdc <= max_total_micro_usdc, "payment_cap_exceeds_total_cap")
        require(safe_integer(max_input_bytes, 1) and max_input_bytes <= 65_536, "invalid_input_byte_limit")
        require(safe_integer(timeout_ms, 1) and timeout_ms <= 120_000, "invalid_timeout")
        self.pay_to, self.gateway_buyer, self.operators = pay_to, gateway_buyer, list(operators)
        self.wallet, self.payer = wallet, wallet_address(wallet)
        self.max_payment, self.max_total = int(max_payment_micro_usdc), int(max_total_micro_usdc)
        self.max_input_bytes = int(max_input_bytes)
        self.http = httpx.AsyncClient(timeout=timeout_ms / 1000, follow_redirects=False, transport=transport, trust_env=False)
        self.journal = Journal(journal_path, sha256(stable({"payer": self.payer, "endpoint": self.endpoint, "network": NETWORK, "asset": USDC})), self.max_total)

    async def _response(self, url: str, headers: dict, body: str, *, max_response_bytes=4 * 1024 * 1024):
        # Stream with a bound before parsing; no redirects with payment headers.
        async with self.http.stream("POST", url, headers=headers, content=body.encode("utf-8")) as response:
            chunks, size = [], 0
            async for chunk in response.aiter_bytes():
                size += len(chunk)
                require(size <= max_response_bytes, "response_too_large")
                chunks.append(chunk)
            return response.status_code, response.headers, b"".join(chunks)

    async def rpc(self, method: str, params=None):
        status, _, body = await self._response(self.rpc_url, {"content-type": "application/json"}, compact({"jsonrpc": "2.0", "id": 1, "method": method, "params": params or []}))
        require(200 <= status < 300, "rpc_unavailable")
        parsed = strict_json(body)
        require(isinstance(parsed, dict) and not parsed.get("error") and "result" in parsed, "rpc_error")
        return parsed["result"]

    async def post(self, request_id: str, body: str, payment=None, *, batch=False):
        headers = {"content-type": "application/json", "idempotency-key": request_id}
        if payment:
            headers["PAYMENT-SIGNATURE"] = payment
        return await self._response(self.batch_endpoint if batch else self.endpoint, headers, body,
                                    max_response_bytes=(10 if batch else 4) * 1024 * 1024)

    def validate_quote(self, required: dict, digest: str, expired_allowed=False, *, batch_request=None) -> tuple[dict, int]:
        endpoint = self.batch_endpoint if batch_request is not None else self.endpoint
        require(isinstance(required, dict) and required.get("x402Version") == 2 and required.get("resource", {}).get("url") == endpoint, "untrusted_payment_resource")
        quote = solana_quote(required)
        require(quote.get("scheme") == "exact" and quote.get("network") == NETWORK and quote.get("asset") == USDC and quote.get("payTo") == self.pay_to, "untrusted_payment_rail")
        require(isinstance(quote.get("amount"), str) and re.fullmatch(r"[1-9][0-9]*", quote["amount"]) and len(quote["amount"]) <= 16, "invalid_payment_amount")
        price = int(quote["amount"])
        require(price <= self.max_payment, "per_payment_cap_exceeded")
        extra = quote.get("extra")
        require(isinstance(extra, dict) and isinstance(extra.get("quoteId"), str) and UUID_PATTERN.fullmatch(extra["quoteId"]) and extra.get("requestSha256") == digest, "quote_request_mismatch")
        require(safe_integer(extra.get("expiresAtMs")) and (expired_allowed or extra["expiresAtMs"] > now_ms() + 5000), "quote_expired")
        require(extra.get("memo") in (short_memo(extra["quoteId"], digest), f"covenant.x402.v2:{extra['quoteId']}:{digest}"), "quote_memo_mismatch")
        Pubkey.from_string(extra["feePayer"])
        require(extra["feePayer"] != self.payer, "buyer_must_not_pay_solana_fees")
        if batch_request is not None:
            validate_batch_pricing(extra, batch_request["requests"], price)
        else:
            require("batchPricing" not in extra, "unexpected_batch_pricing")
        return quote, price

    async def _create_payment(self, required: dict) -> str:
        mint = (await self.rpc("getAccountInfo", [USDC, {"encoding": "base64", "commitment": "confirmed"}])).get("value")
        require(isinstance(mint, dict) and mint.get("owner") == TOKEN_PROGRAM and mint.get("executable") is False, "untrusted_usdc_mint")
        data = decode64(mint["data"][0])
        require(len(data) == 82 and data[44] == 6 and data[45] == 1, "untrusted_usdc_mint")
        balance = (await self.rpc("getTokenAccountBalance", [derive_ata(self.payer, USDC, TOKEN_PROGRAM), {"commitment": "confirmed"}])).get("value", {})
        require(balance.get("decimals") == 6 and isinstance(balance.get("amount"), str) and balance["amount"].isdigit() and int(balance["amount"]) >= int(solana_quote(required)["amount"]), "insufficient_usdc")
        blockhash = (await self.rpc("getLatestBlockhash", [{"commitment": "confirmed"}]))["value"]["blockhash"]
        payload = await create_payment(required, self.wallet, self.payer, data, blockhash)
        # Rust compares accepted terms as serde_json::Value: an integer expiry
        # must remain a JSON integer, even after strict_json parsed IEEE numbers.
        return base64.b64encode(stable(payload).encode("utf-8")).decode("ascii")

    async def verify_settlement(self, header: str, payment: str) -> dict:
        require(isinstance(header, str) and len(header) < 8192, "missing_payment_response")
        settlement = strict_json(decode64(header))
        require(settlement.get("success") is True and settlement.get("network") == NETWORK and settlement.get("payer") == self.payer, "settlement_identity_mismatch")
        try:
            Signature.from_string(settlement["transaction"])
        except (ValueError, TypeError, KeyError) as exc:
            raise CovenantError("invalid_settlement_signature") from exc
        authorization = strict_json(decode64(payment))
        signed = VersionedTransaction.from_bytes(decode64(authorization["payload"]["transaction"]))
        landed = await self.rpc("getTransaction", [settlement["transaction"], {"commitment": "confirmed", "encoding": "base64", "maxSupportedTransactionVersion": 0}])
        require(isinstance(landed, dict) and landed.get("meta", {}).get("err", "missing") is None and isinstance(landed.get("transaction"), list), "settlement_not_confirmed")
        confirmed = VersionedTransaction.from_bytes(decode64(landed["transaction"][0]))
        require(str(confirmed.signatures[0]) == settlement["transaction"], "settlement_signature_mismatch")
        require(to_bytes_versioned(confirmed.message) == to_bytes_versioned(signed.message), "settlement_transaction_mismatch")
        require(confirmed.signatures[1:] == signed.signatures[1:] and all(confirmed.verify_with_results()), "settlement_signature_mismatch")
        return settlement

    def _authorization(self, payment: str, quote: dict) -> dict:
        authorization = strict_json(decode64(payment))
        require(authorization.get("x402Version") == 2 and stable(authorization.get("accepted")) == stable(quote), "authorization_quote_mismatch")
        validate_transaction(VersionedTransaction.from_bytes(decode64(authorization["payload"]["transaction"])), quote, self.payer, legacy_allowed=True)
        return authorization

    def _verify_result(self, result, request, digest, quote, batch):
        if batch:
            return verify_batch_result(result, request=request, request_sha256=digest,
                                       gateway_buyer=self.gateway_buyer, operators=self.operators,
                                       quote=quote)
        return verify_compute_result(result, request=request, request_sha256=digest,
                                     quote_id=quote["extra"]["quoteId"], gateway_buyer=self.gateway_buyer,
                                     operators=self.operators)

    def _cached_result(self, row: dict, request: dict, request_id: str, digest: str, *, batch=False) -> dict:
        required = strict_json(row["quote"])
        quote, price = self.validate_quote(required, digest, expired_allowed=True, batch_request=request if batch else None)
        self._authorization(row["payment"], quote)
        complete = strict_json(row["result"])
        require(complete.get("status") == "completed" and complete.get("requestId") == request_id and complete.get("requestSha256") == digest, "cached_request_mismatch")
        require(complete.get("amountMicroUsdc") == row["amount"] == price, "cached_amount_mismatch")
        verified = self._verify_result(complete["result"], request, digest, quote, batch)
        text_field = "texts" if batch else "text"
        require(complete.get(text_field) == verified[text_field], "cached_text_mismatch")
        settlement = complete.get("settlement", {})
        require(settlement.get("success") is True and settlement.get("network") == NETWORK and settlement.get("payer") == self.payer, "settlement_identity_mismatch")
        try:
            Signature.from_string(settlement["transaction"])
        except (ValueError, TypeError, KeyError) as exc:
            raise CovenantError("invalid_settlement_signature") from exc
        return complete

    async def purchase(self, *, request_id: str, model: str, messages: list[dict], max_tokens: int, temperature=None, pricing=None) -> dict:
        request = validate_request(model, messages, max_tokens, temperature, pricing)
        require(len(compact(request["messages"]).encode("utf-8")) <= self.max_input_bytes, "input_byte_limit_exceeded")
        return await self._purchase(request_id, request)

    async def purchase_batch(self, *, request_id: str, requests: list[dict], version=1) -> dict:
        """Purchase 1..2 ordered chat requests with one durable authorization.

        Retry only the original UUID and ordered body. A partial/failed batch
        retains its authorization and reservation; this method never rebuys.
        """
        require(safe_integer(version) and version == 1, "invalid_batch_version")
        require(isinstance(requests, list) and 1 <= len(requests) <= 32, "invalid_batch_size")
        clean = []
        for child in requests:
            require(isinstance(child, dict) and {"model", "messages", "max_tokens"} <= set(child)
                    and set(child) <= {"model", "messages", "max_tokens", "temperature", "pricing"}, "invalid_batch_request_fields")
            normalized = validate_request(**child)
            require(len(compact(normalized["messages"]).encode("utf-8")) <= self.max_input_bytes, "input_byte_limit_exceeded")
            clean.append(normalized)
        return await self._purchase(request_id, {"version": 1, "requests": clean}, batch=True)

    async def _purchase(self, request_id: str, request: dict, *, batch=False) -> dict:
        require(isinstance(request_id, str) and UUID_PATTERN.fullmatch(request_id), "stable_uuid_request_id_required")
        body = stable(request)
        require(len(body.encode("utf-8")) <= 128_000, "request_too_large")
        digest = sha256(body)
        if batch and len(request["requests"]) > 2:
            # Protocol proofs support 32, but fresh admission is deliberately
            # limited to qualified sequential capacity. Existing signed larger
            # batches remain recoverable without a replacement authorization.
            saved = self.journal.get(request_id)
            require(saved and saved["payment"], "fresh_batch_size_exceeded")
        row = self.journal.claim(request_id, body, digest)
        if row["result"]:
            return self._cached_result(row, request, request_id, digest, batch=batch)
        owner = row["owner"]
        try:
            require(await self.rpc("getGenesisHash") == SOLANA_GENESIS, "rpc_network_mismatch")
            if not row["quote"]:
                status, headers, _ = await self.post(request_id, body, batch=batch)
                require(status == 402, f"quote_http_{status}")
                header = headers.get("PAYMENT-REQUIRED")
                require(isinstance(header, str) and len(header) < (65_536 if batch else 16_384), "missing_payment_required")
                required = strict_json(decode64(header))
                _, price = self.validate_quote(required, digest, batch_request=request if batch else None)
                self.journal.reserve(request_id, owner, required, price)
                row = self.journal.get(request_id)
            required = strict_json(row["quote"])
            quote, price = self.validate_quote(required, digest, bool(row["payment"]), batch_request=request if batch else None)
            require(row["amount"] == price, "reserved_amount_mismatch")
            if not row["payment"]:
                payment = await self._create_payment(required)
                self.validate_quote(required, digest, batch_request=request if batch else None)
                self.journal.signed(request_id, owner, payment)
                row = self.journal.get(request_id)
            # Also validate authorizations originally saved by Node; never change
            # their compute limit, blockhash, signature, header, or spend reserve.
            self._authorization(row["payment"], quote)
            self.journal.check(request_id, owner)
            status, headers, response = await self.post(request_id, body, row["payment"], batch=batch)
            if status == 202:
                return {"status": "pending", "requestId": request_id, "requestSha256": digest}
            require(status == 200, f"paid_http_{status}_authorization_retained")
            result = strict_json(response)
            verified = self._verify_result(result, request, digest, quote, batch)
            settlement = await self.verify_settlement(headers.get("PAYMENT-RESPONSE"), row["payment"])
            text_field = "texts" if batch else "text"
            complete = {"status": "completed", "requestId": request_id, "requestSha256": digest, "amountMicroUsdc": row["amount"], "settlement": settlement, text_field: verified[text_field], "result": result}
            self.journal.complete(request_id, owner, compact(complete))
            return complete
        finally:
            self.journal.release(request_id, owner)

    async def close(self) -> None:
        await self.http.aclose()
        self.journal.close()

    async def __aenter__(self):
        return self

    async def __aexit__(self, *_):
        await self.close()
