"""Synthetic batch transport; all keys are unfunded and RPC never broadcasts."""
import base64
import copy
import hashlib
from uuid import uuid4

import httpx2 as httpx
from solders.message import to_bytes_versioned
from solders.transaction import VersionedTransaction

from covenant_compute_x402 import NETWORK, USDC
from covenant_compute_x402.journal import now_ms
from covenant_compute_x402.payment import decode64, short_memo, validate_transaction
from covenant_compute_x402.proof import compact, sha256, stable, strict_json
from fixture import Fixture, encode, wrapper


def batch_request(size=2, request_id=None):
    return {"request_id": request_id or str(uuid4()), "version": 1, "requests": [
        {"model": "covenant-test", "messages": [{"role": "user", "content": f"Question {i}: 日本語 🟨"}],
         "max_tokens": 256 + i, **({"pricing": "model_tariff_v1"} if i % 2 else {})}
        for i in range(size)]}


def build_result(batch_id, digest, outputs):
    items = [{"leaf": {"version": 1, "batch_id": batch_id, "request_sha256": digest,
                       "index": i, "job_id": output["covenant"]["job_id"],
                       "result_sha256": sha256(stable(output))}, "output": copy.deepcopy(output), "audit_path": []}
             for i, output in enumerate(outputs)]
    hashes = [hashlib.sha256(b"\0covenant.compute.inference-batch.leaf.v1\n" + stable(item["leaf"]).encode()).digest() for item in items]

    def tree(values, offset):
        if len(values) == 1:
            return values[0]
        split = 1 << ((len(values) - 1).bit_length() - 1)
        left, right = tree(values[:split], offset), tree(values[split:], offset + split)
        for item in items[offset:offset + split]:
            item["audit_path"].append(right.hex())
        for item in items[offset + split:offset + len(values)]:
            item["audit_path"].append(left.hex())
        return hashlib.sha256(b"\1" + left + right).digest()

    root = tree(hashes, 0)
    return {"version": 1, "batch_id": batch_id, "request_sha256": digest,
            "tree_size": len(items), "merkle_root": root.hex(), "items": items}


class BatchFixture(Fixture):
    def __init__(self, tmp_path, **kwargs):
        super().__init__(tmp_path, **kwargs)
        self.results = {}
        self.config.update(max_payment_micro_usdc=100_000, max_total_micro_usdc=200_000)

    async def seed_historical_authorization(self, client, item):
        """An unfunded saved older authorization, not current fresh admission."""
        body = stable({"version": item["version"], "requests": item["requests"]})
        digest, request_id = sha256(body), item["request_id"]
        row = client.journal.claim(request_id, body, digest)
        try:
            status, headers, _ = await client.post(request_id, body, batch=True)
            assert status == 402
            required = strict_json(decode64(headers["PAYMENT-REQUIRED"]))
            _, amount = client.validate_quote(required, digest, batch_request=strict_json(body))
            client.journal.reserve(request_id, row["owner"], required, amount)
            client.journal.signed(request_id, row["owner"], await client._create_payment(required))
        finally:
            client.journal.release(request_id, row["owner"])

    async def handle(self, req):
        if req.url.path != "/v1/inference/batches":
            return await super().handle(req)
        self.calls.append(str(req.url))
        body = strict_json(req.content)
        digest, quote_id = sha256(stable(body)), req.headers["idempotency-key"]
        quote = self.saved_quotes.get(quote_id)
        if quote is None:
            batch_id, outputs, children = str(uuid4()), [], []
            for index, child in enumerate(body["requests"]):
                job_id, issued = str(uuid4()), now_ms()
                output = [{"type": "text", "text": f"Answer {index}: {child['messages'][0]['content']}"}]
                generation = {key: child[key] for key in ("max_tokens", "temperature") if key in child}
                job = {"job_id": job_id, "kind": "inference_call", "issued_at_ms": issued,
                       "capability_requirement": {"kind": "inference_call", "model_id": child["model"]},
                       "input": [{"type": "json", "value": {"messages": child["messages"]}},
                                 {"type": "json", "value": {"generation": generation}}], "price_micro_usdc": 900}
                if child.get("pricing"):
                    tariff = dict(schema_version=1, id="batch-native-tariff", revision=1, model=child["model"],
                                  base_micro_usdc=500, output_cap_micro_usdc_per_million=1001, max_output_tokens=8192,
                                  valid_from_ms=issued-1000, valid_until_ms=issued+90_000)
                    native = dict(tariff=tariff, requested_output_cap=child["max_tokens"],
                                  price_micro_usdc=500+(int(child["max_tokens"])*1001+999999)//1000000)
                    job["capability_requirement"]["inference_tariff"] = copy.deepcopy(native)
                    job["input"].append({"type": "json", "value": {"inference_tariff": native}})
                    job["price_micro_usdc"] = native["price_micro_usdc"]
                if self.bad_job:
                    self.bad_job(job)
                envelope = wrapper(self.buyer, job, "payload", "covenant.compute.job.v1\n", "buyer")
                receipt = wrapper(self.operator, {"job_id": job_id, "job_hash_hex": sha256(envelope["payload_json"]),
                                  "result_hash_hex": sha256(stable(output)), "price_micro_usdc": job["price_micro_usdc"],
                                  "status": "ok"}, "receipt", "covenant.compute.receipt.v1\n", "operator")
                outputs.append({"choices": [{"message": {"role": "assistant", "content": output[0]["text"]}}],
                                "covenant": {"envelope": envelope, "receipt": receipt, "output": output,
                                             "output_json": stable(output), "job_id": job_id}})
                retail = int(self.price(child)) if self.price else 1000
                children.append({"job_id": job_id, "model": child["model"], "max_output_tokens": child["max_tokens"],
                                 **({"pricing": child["pricing"]} if "pricing" in child else {}), "amount_atomic": retail})
            quote = {"scheme": "exact", "network": NETWORK, "asset": USDC,
                     "amount": str(sum(child["amount_atomic"] for child in children)), "payTo": str(self.seller.pubkey()),
                     "maxTimeoutSeconds": 90, "extra": {"feePayer": str(self.fee.pubkey()), "quoteId": quote_id,
                     "requestSha256": digest, "expiresAtMs": now_ms()+90_000,
                     "memo": f"covenant.x402.v2:{quote_id}:{digest}" if self.legacy_memo else short_memo(quote_id, digest),
                     "batchPricing": {"version": 1, "batch_id": batch_id, "children": children}}}
            self.results[quote_id] = build_result(batch_id, digest, outputs)
        if "payment-signature" not in req.headers:
            self.quotes += 1
            if self.bad_quote:
                self.bad_quote(quote)
            self.saved_quotes[quote_id] = quote
            return httpx.Response(402, headers={"PAYMENT-REQUIRED": encode({"x402Version": 2,
                                  "resource": {"url": str(req.url)}, "accepts": self.offers(quote) if self.offers else [quote]})}, content="{}")
        self.sends += 1
        self.payments.append(req.headers["payment-signature"])
        payload = strict_json(decode64(self.payments[-1]))
        assert payload["accepted"] == quote
        tx = VersionedTransaction.from_bytes(decode64(payload["payload"]["transaction"]))
        validate_transaction(tx, payload["accepted"], self.wallet.address, legacy_allowed=True)
        signatures = list(tx.signatures)
        signatures[0] = self.fee.sign_message(to_bytes_versioned(tx.message))
        confirmed = VersionedTransaction.populate(tx.message, signatures)
        txid = str(signatures[0])
        self.confirmed[txid] = {"meta": {"err": None}, "transaction": [base64.b64encode(bytes(confirmed)).decode(), "base64"]}
        if self.first_status:
            status, self.first_status = self.first_status, None
            return httpx.Response(status, content="{}")
        result = copy.deepcopy(self.results[quote_id])
        if self.bad_result:
            self.bad_result(result)
        return httpx.Response(200, headers={"PAYMENT-RESPONSE": encode({"success": True, "network": NETWORK,
                              "payer": self.wallet.address, "transaction": txid})}, content=compact(result))
