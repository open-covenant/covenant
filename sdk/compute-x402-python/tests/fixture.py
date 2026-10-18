"""Unfunded official-SDK wire fixture. No transaction is publicly broadcast."""
import base64
from uuid import uuid4

import httpx2 as httpx
from solders.keypair import Keypair
from solders.signature import Signature
from solders.transaction import VersionedTransaction
from solders.message import to_bytes_versioned

from covenant_compute_x402 import CovenantX402Client, NETWORK, SOLANA_GENESIS, USDC
from covenant_compute_x402.journal import now_ms
from covenant_compute_x402.payment import TOKEN_PROGRAM, decode64, validate_transaction
from covenant_compute_x402.proof import stable, compact, strict_json, sha256


def request(request_id=None):
    return dict(request_id=request_id or str(uuid4()), model="covenant-test",
                messages=[{"role": "user", "content": "Draft a useful support reply."}],
                max_tokens=512, temperature=0.25)


def encode(value):
    return base64.b64encode(compact(value).encode()).decode()


def wrapper(key, value, field, domain, actor):
    payload = {**value, actor: {"display": actor, "pubkey": str(key.pubkey())}}
    raw = stable(payload)
    return {field: payload, field + "_json": raw, "signer_pubkey_b58": str(key.pubkey()),
            "signature_b58": str(key.sign_message((domain + raw).encode()))}


class Wallet:
    def __init__(self, key):
        self.key, self.signatures = key, 0
        self.address = str(key.pubkey())

    async def sign_transaction(self, tx):
        self.signatures += 1
        return VersionedTransaction.populate(tx.message, [Signature.default(), self.key.sign_message(to_bytes_versioned(tx.message))])


class Fixture:
    def __init__(self, tmp_path, *, first_status=None, bad_quote=None, bad_result=None,
                 wrong_rpc=False, balance="1000000", bad_settlement=None):
        self.wallet = Wallet(Keypair())
        self.buyer, self.operator, self.fee, self.seller = [Keypair() for _ in range(4)]
        self.calls, self.quotes, self.sends, self.payments, self.confirmed = [], 0, 0, [], {}
        self.first_status, self.bad_quote, self.bad_result = first_status, bad_quote, bad_result
        self.wrong_rpc, self.balance, self.bad_settlement = wrong_rpc, balance, bad_settlement
        self.config = dict(gateway_url="https://compute.example", rpc_url="https://rpc.example/",
                           pay_to=str(self.seller.pubkey()), gateway_buyer=str(self.buyer.pubkey()),
                           operators=[str(self.operator.pubkey())], wallet=self.wallet,
                           journal_path=str(tmp_path / "journal.sqlite"), max_payment_micro_usdc=1000,
                           max_total_micro_usdc=2000, transport=httpx.MockTransport(self.handle))

    def client(self, **overrides):
        return CovenantX402Client(**{**self.config, **overrides})

    async def handle(self, req):
        body = strict_json(req.content)
        self.calls.append(str(req.url))
        if req.url.host == "rpc.example":
            method = body["method"]
            if method == "getGenesisHash":
                result = "wrong" if self.wrong_rpc else SOLANA_GENESIS
            elif method == "getAccountInfo":
                mint = bytearray(82); mint[44] = 6; mint[45] = 1
                result = {"value": {"owner": TOKEN_PROGRAM, "executable": False, "data": [base64.b64encode(mint).decode(), "base64"]}}
            elif method == "getTokenAccountBalance":
                result = {"value": {"amount": self.balance, "decimals": 6}}
            elif method == "getLatestBlockhash":
                result = {"value": {"blockhash": str(self.fee.pubkey()), "lastValidBlockHeight": 12345}}
            elif method == "getTransaction":
                result = self.confirmed.get(body["params"][0])
                if self.bad_settlement:
                    self.bad_settlement(result)
            else:
                raise AssertionError(method)
            return httpx.Response(200, content=compact({"jsonrpc": "2.0", "id": 1, "result": result}))
        assert str(req.url) == "https://compute.example/v1/chat/completions"
        digest, quote_id = sha256(stable(body)), req.headers["idempotency-key"]
        quote = {"scheme": "exact", "network": NETWORK, "asset": USDC,
                 "amount": "1000", "payTo": str(self.seller.pubkey()), "maxTimeoutSeconds": 90,
                 "extra": {"feePayer": str(self.fee.pubkey()), "quoteId": quote_id,
                           "requestSha256": digest, "expiresAtMs": now_ms() + 90_000,
                           "memo": f"covenant.x402.v2:{quote_id}:{digest}"}}
        if "payment-signature" not in req.headers:
            self.quotes += 1
            if self.bad_quote:
                self.bad_quote(quote)
            return httpx.Response(402, headers={"PAYMENT-REQUIRED": encode({"x402Version": 2, "resource": {"url": str(req.url)}, "accepts": [quote]})}, content="{}")
        self.sends += 1
        payment_header = req.headers["payment-signature"]
        self.payments.append(payment_header)
        payload = strict_json(decode64(payment_header))
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
        output = [{"type": "text", "text": "Please share the invoice numbers so the team can investigate."}]
        generation = {"max_tokens": body["max_tokens"]}
        if "temperature" in body:
            generation["temperature"] = body["temperature"]
        envelope = wrapper(self.buyer, {"job_id": quote_id, "kind": "inference_call", "capability_requirement": {"kind": "inference_call", "model_id": body["model"]}, "input": [{"type": "json", "value": {"messages": body["messages"]}}, {"type": "json", "value": {"generation": generation}}], "price_micro_usdc": 900}, "payload", "covenant.compute.job.v1\n", "buyer")
        receipt = wrapper(self.operator, {"job_id": quote_id, "job_hash_hex": sha256(envelope["payload_json"]), "result_hash_hex": sha256(stable(output)), "price_micro_usdc": 900, "status": "ok"}, "receipt", "covenant.compute.receipt.v1\n", "operator")
        response = {"choices": [{"message": {"role": "assistant", "content": output[0]["text"]}}], "covenant": {"envelope": envelope, "receipt": receipt, "output": output, "output_json": stable(output), "request_sha256": digest, "quote_id": quote_id, "job_id": quote_id}}
        if self.bad_result:
            self.bad_result(response)
        return httpx.Response(200, content=compact(response), headers={"PAYMENT-RESPONSE": encode({"success": True, "network": NETWORK, "payer": self.wallet.address, "transaction": txid})})
