import copy
from pathlib import Path

import pytest
import httpx2 as httpx
from solders.keypair import Keypair
from solders.signature import Signature
from solders.message import to_bytes_versioned
from solders.transaction import VersionedTransaction

from covenant_compute_x402 import CovenantError
from covenant_compute_x402.batch import verify_batch_result, verify_batch_proof, validate_batch_pricing
from covenant_compute_x402.payment import decode64, short_memo, solana_quote
from covenant_compute_x402.proof import compact, sha256, stable, strict_json
from batch_fixture import BatchFixture, batch_request, build_result
from fixture import request, wrapper


def test_rust_proof_and_js_signed_mixed_tariff_fixtures():
    root = Path(__file__).parents[3]
    local = Path(__file__).parent / "fixtures"
    # Keep source distributions self-contained while detecting shared fixture
    # drift when running from the complete repository.
    for name, shared in [("native-batch-proof.json", root / "fixtures/compute/inference-batch-v1.json"),
                         ("native-batch-response.json", root / "sdk/compute-x402-client/test/fixtures/signed-batch-response.json")]:
        if shared.is_file():
            assert (local / name).read_bytes() == shared.read_bytes()
    vector = strict_json((local / "native-batch-proof.json").read_bytes())
    verify_batch_proof(vector["result"])
    fixture = strict_json((local / "native-batch-response.json").read_bytes())
    quote = fixture["quote"]
    validate_batch_pricing(quote["extra"], fixture["request"]["requests"], int(quote["amount"]))
    result = verify_batch_result(fixture["response"], request=fixture["request"],
                                request_sha256=sha256(stable(fixture["request"])),
                                quote=quote,
                                gateway_buyer=fixture["gatewayBuyer"], operators=fixture["operators"])
    assert len(result["texts"]) == 2


@pytest.mark.parametrize("field", ["caller_digest", "quote_digest"])
def test_standalone_verifier_derives_request_digest_and_requires_quote_binding(field):
    fixture = strict_json((Path(__file__).parent / "fixtures/native-batch-response.json").read_bytes())
    digest = sha256(stable(fixture["request"]))
    if field == "caller_digest":
        digest = "ab" * 32
    else:
        fixture["quote"]["extra"]["requestSha256"] = "ab" * 32
    with pytest.raises(CovenantError, match="batch_request_mismatch"):
        verify_batch_result(fixture["response"], request=fixture["request"], request_sha256=digest,
                            quote=fixture["quote"], gateway_buyer=fixture["gatewayBuyer"],
                            operators=fixture["operators"])


@pytest.mark.parametrize("mutation", [
    lambda extra: extra.update(memo="0" * 32),
    lambda extra: extra.update(quoteId="11111111-1111-1111-1111-111111111111"),
    lambda extra: extra.update(memo=extra["memo"].upper()),
])
def test_standalone_batch_verifier_checks_quote_memo_binding(mutation):
    fixture = strict_json((Path(__file__).parent / "fixtures/native-batch-response.json").read_bytes())
    mutation(fixture["quote"]["extra"])
    with pytest.raises(CovenantError, match="quote_memo_mismatch"):
        verify_batch_result(fixture["response"], request=fixture["request"],
                            request_sha256=sha256(stable(fixture["request"])),
                            quote=fixture["quote"], gateway_buyer=fixture["gatewayBuyer"],
                            operators=fixture["operators"])


def test_proof_verifier_all_supported_tree_shapes():
    for size in range(1, 33):
        outputs = [{"covenant": {"job_id": f"00000000-0000-0000-0000-{i+1:012x}"}, "value": i} for i in range(size)]
        result = build_result("11111111-1111-1111-1111-111111111111", "ab"*32, outputs)
        verify_batch_proof(result)
        result["items"][-1]["audit_path"].append("bb"*32)
        with pytest.raises(CovenantError, match="invalid_batch_proof"):
            verify_batch_proof(result)


@pytest.mark.parametrize("size", [1, 2])
async def test_batch_complete_all_leaves_and_native_tariffs_with_one_payment(tmp_path, size):
    f, item = BatchFixture(tmp_path), batch_request(size)
    async with f.client() as client:
        result = await client.purchase_batch(**item)
        assert result["status"] == "completed" and len(result["texts"]) == size
        assert result["amountMicroUsdc"] == size * 1000
        assert f.wallet.signatures == f.quotes == f.sends == 1
    calls = len(f.calls)
    async with f.client() as client:
        assert await client.purchase_batch(**item) == result
    assert len(f.calls) == calls


@pytest.mark.parametrize("legacy_memo", [False, True])
async def test_batch_memo_uses_standard_20k_or_legacy_60k_and_freezes_recovery(tmp_path, legacy_memo, monkeypatch):
    from covenant_compute_x402 import payment
    built = []
    original = payment._SnapshotScheme.create_payment_payload

    def capture(self, *args, **kwargs):
        result = original(self, *args, **kwargs)
        built.append(result)
        return result
    monkeypatch.setattr(payment._SnapshotScheme, "create_payment_payload", capture)
    # A non-Solana offer first must not change selection or the saved quote.
    f = BatchFixture(tmp_path, legacy_memo=legacy_memo, first_status=202,
                     offers=lambda q: [{"network": "eip155:8453", "amount": "999999999999"}, q])
    item = batch_request()
    async with f.client() as client:
        assert (await client.purchase_batch(**item))["status"] == "pending"
        before = client.journal.get(item["request_id"])
    quote = solana_quote(strict_json(before["quote"]))
    payload = strict_json(decode64(before["payment"]))
    tx = VersionedTransaction.from_bytes(decode64(payload["payload"]["transaction"]))
    assert payload["accepted"] == quote
    assert int.from_bytes(bytes(tx.message.instructions[0].data)[1:5], "little") == (60_000 if legacy_memo else 20_000)
    if not legacy_memo:
        unsigned = VersionedTransaction.from_bytes(decode64(built[0]["transaction"]))
        assert to_bytes_versioned(tx.message) == to_bytes_versioned(unsigned.message)
        assert quote["extra"]["memo"] == short_memo(item["request_id"], before["digest"])
    # Gateway defaults and other-network offers may change after signing.
    f.legacy_memo = not legacy_memo
    f.offers = lambda q: [q]
    async with f.client() as client:
        assert (await client.purchase_batch(**item))["status"] == "completed"
        after = client.journal.get(item["request_id"])
        assert all(before[key] == after[key] for key in ("body", "digest", "quote", "payment", "amount"))
    assert f.quotes == f.wallet.signatures == 1 and f.sends == 2
    assert f.payments[0] == f.payments[1]


@pytest.mark.parametrize("offers,code", [
    (lambda q: [{"network": "eip155:8453"}], "untrusted_payment_rail"),
    (lambda q: [q, copy.deepcopy(q)], "ambiguous_payment_requirements"),
])
async def test_batch_without_one_solana_offer_stops_before_reservation(tmp_path, offers, code):
    f, item = BatchFixture(tmp_path, offers=offers), batch_request()
    async with f.client() as client:
        with pytest.raises(CovenantError, match=code):
            await client.purchase_batch(**item)
        assert client.journal.get(item["request_id"])["quote"] is None
    assert f.wallet.signatures == f.sends == 0


@pytest.mark.parametrize("status", [202, 503, 402])
async def test_batch_unknown_restart_keeps_original_authorization_and_frozen_prices(tmp_path, status):
    price = 1000
    f, item = BatchFixture(tmp_path, first_status=status, price=lambda _: price), batch_request()
    async with f.client() as client:
        if status == 202:
            assert (await client.purchase_batch(**item))["status"] == "pending"
        else:
            with pytest.raises(CovenantError, match="authorization_retained"):
                await client.purchase_batch(**item)
        before = client.journal.get(item["request_id"])
    price = 9000
    async with f.client() as client:
        result = await client.purchase_batch(**item)
        assert result["amountMicroUsdc"] == 2000
        after = client.journal.get(item["request_id"])
        assert all(before[key] == after[key] for key in ("quote", "payment", "amount", "body", "digest"))
    assert f.wallet.signatures == f.quotes == 1 and f.payments[0] == f.payments[1]


async def test_batch_order_or_body_change_never_requotes_or_repays(tmp_path):
    f, item = BatchFixture(tmp_path, first_status=202), batch_request()
    async with f.client() as client:
        await client.purchase_batch(**item)
        for children in [list(reversed(item["requests"])), item["requests"][:-1],
                         [{**item["requests"][0], "max_tokens": 1}, *item["requests"][1:]]]:
            with pytest.raises(CovenantError, match="idempotency_request_mismatch"):
                await client.purchase_batch(**{**item, "requests": children})
        with pytest.raises(CovenantError, match="idempotency_request_mismatch"):
            await client.purchase(**request(item["request_id"]))
    assert f.quotes == f.wallet.signatures == f.sends == 1


@pytest.mark.parametrize("change,error", [
    (lambda result: result["items"].reverse(), "batch_item_binding_mismatch"),
    (lambda result: result["items"].pop(), "invalid_or_incomplete_batch"),
    (lambda result: result.update(merkle_root="a"*64), "invalid_batch_proof"),
    (lambda result: result.update(request_sha256="a"*64), "batch_request_mismatch"),
    (lambda result: result.update(batch_id="11111111-1111-1111-1111-111111111111"), "batch_identity_mismatch"),
    (lambda result: result["items"][0]["leaf"].update(index=True), "batch_item_binding_mismatch"),
    (lambda result: result["items"][0]["audit_path"].append("b"*64), "invalid_batch_proof"),
    (lambda result: result.update(unknown=True), "invalid_or_incomplete_batch"),
    (lambda result: result["items"][0].update(unknown=True), "invalid_batch_item"),
    (lambda result: result["items"][0]["output"]["choices"][0]["message"].update(content="substitution"), "batch_output_hash_mismatch"),
])
async def test_batch_proof_tampering_retains_payment(tmp_path, change, error):
    f, item = BatchFixture(tmp_path, bad_result=change), batch_request()
    async with f.client() as client:
        with pytest.raises(CovenantError, match=error):
            await client.purchase_batch(**item)
        saved = client.journal.get(item["request_id"])
        assert saved["payment"] and saved["amount"] == 2000 and not saved["result"]
    assert f.wallet.signatures == f.sends == 1


@pytest.mark.parametrize("mutation,error", [
    ("signature", "invalid_signature"), ("partial", "compute_job_not_completed"),
    ("request", "job_messages_mismatch"), ("job_id", "batch_item_binding_mismatch"),
    ("tariff", "inference_tariff_required"),
])
async def test_valid_root_cannot_replace_native_receipt_or_original_child(tmp_path, mutation, error):
    f = BatchFixture(tmp_path)
    def change(result):
        output = result["items"][1]["output"]
        proof = output["covenant"]
        if mutation == "signature":
            proof["receipt"]["signature_b58"] = str(Signature.default())
        elif mutation == "partial":
            receipt = proof["receipt"]["receipt"]
            receipt["status"] = "failed"
            proof["receipt"] = wrapper(f.operator, receipt, "receipt", "covenant.compute.receipt.v1\n", "operator")
        else:
            job = proof["envelope"]["payload"]
            if mutation == "request":
                job["input"][0]["value"]["messages"][0]["content"] = "another request"
            elif mutation == "job_id":
                job["job_id"] = proof["job_id"] = "11111111-1111-1111-1111-111111111111"
                proof["receipt"]["receipt"]["job_id"] = job["job_id"]
            else:
                job["input"].pop()
                del job["capability_requirement"]["inference_tariff"]
            proof["envelope"] = wrapper(f.buyer, job, "payload", "covenant.compute.job.v1\n", "buyer")
            receipt = proof["receipt"]["receipt"]
            receipt["job_hash_hex"] = sha256(proof["envelope"]["payload_json"])
            proof["receipt"] = wrapper(f.operator, receipt, "receipt", "covenant.compute.receipt.v1\n", "operator")
        result.update(build_result(result["batch_id"], result["request_sha256"], [item["output"] for item in result["items"]]))
    f.bad_result = change
    async with f.client() as client:
        with pytest.raises(CovenantError, match=error):
            await client.purchase_batch(**batch_request())


@pytest.mark.parametrize("stream", [True, None, 0, "false"])
async def test_batch_signed_native_stream_must_be_absent_or_exact_false(tmp_path, stream):
    f = BatchFixture(tmp_path, bad_job=lambda job: job.update(stream=stream))
    async with f.client() as client:
        with pytest.raises(CovenantError, match="batch_native_request_mismatch"):
            await client.purchase_batch(**batch_request())


@pytest.mark.parametrize("change,error", [
    (lambda m: m.update(version=True), "invalid_batch_pricing"),
    (lambda m: m.update(batch_id="00000000-0000-0000-0000-000000000000"), "invalid_batch_pricing"),
    (lambda m: m["children"].pop(), "batch_pricing_size_mismatch"),
    (lambda m: m["children"][0].update(model="changed"), "batch_pricing_request_mismatch"),
    (lambda m: m["children"][0].update(max_output_tokens=1), "batch_pricing_request_mismatch"),
    (lambda m: m["children"][1].pop("pricing"), "batch_pricing_request_mismatch"),
    (lambda m: m["children"][0].update(amount_atomic=1), "batch_pricing_total_mismatch"),
    (lambda m: m["children"][0].update(amount_atomic=True), "invalid_batch_pricing_amount"),
    (lambda m: m["children"][0].update(amount_atomic=9007199254740992), "invalid_batch_pricing_amount"),
    (lambda m: m["children"][1].update(job_id=m["children"][0]["job_id"]), "invalid_batch_pricing_child"),
])
async def test_quote_manifest_refused_before_signing(tmp_path, change, error):
    f = BatchFixture(tmp_path, bad_quote=lambda q: change(q["extra"]["batchPricing"]))
    async with f.client() as client:
        with pytest.raises(CovenantError, match=error):
            await client.purchase_batch(**batch_request())
    assert f.wallet.signatures == f.sends == 0


@pytest.mark.parametrize("change", [
    lambda r: r.update(version=True), lambda r: r.update(version=2),
    lambda r: r.update(requests=[]), lambda r: r.update(requests=r["requests"]*17),
    lambda r: r["requests"][0].update(stream=True), lambda r: r["requests"][0].update(tools=[]),
    lambda r: r["requests"][0].update(requests=[]), lambda r: r["requests"][0].update(pricing="other"),
    lambda r: r["requests"][0].update(messages=[{"role": "user", "content": "x"*4096}]),
])
async def test_invalid_batch_before_reservation_or_network(tmp_path, change):
    f, item = BatchFixture(tmp_path), batch_request()
    change(item)
    async with f.client() as client:
        with pytest.raises(CovenantError):
            await client.purchase_batch(**item)
        assert client.journal.get(item["request_id"]) is None
    assert not f.calls


@pytest.mark.parametrize("size", [3, 32])
async def test_fresh_batch_capacity_cannot_be_bypassed_by_an_unsigned_journal_row(tmp_path, size):
    f, item = BatchFixture(tmp_path), batch_request(size)
    async with f.client() as client:
        with pytest.raises(CovenantError, match="fresh_batch_size_exceeded"):
            await client.purchase_batch(**item)
        assert client.journal.get(item["request_id"]) is None
        body = stable({"version": 1, "requests": item["requests"]})
        row = client.journal.claim(item["request_id"], body, sha256(body))
        client.journal.release(item["request_id"], row["owner"])
        with pytest.raises(CovenantError, match="fresh_batch_size_exceeded"):
            await client.purchase_batch(**item)
        assert client.journal.get(item["request_id"])["amount"] == 0
    assert not f.calls and f.wallet.signatures == 0


async def test_single_and_batch_share_persisted_budget(tmp_path):
    f = BatchFixture(tmp_path)
    async with f.client(max_total_micro_usdc=3000, max_payment_micro_usdc=3000) as client:
        await client.purchase(**request())
        await client.purchase_batch(**batch_request())
    async with f.client(max_total_micro_usdc=3000, max_payment_micro_usdc=3000) as client:
        with pytest.raises(CovenantError, match="cumulative_spend_cap_exceeded"):
            await client.purchase_batch(**batch_request(1))
    assert f.wallet.signatures == f.sends == 2


async def test_batch_cached_result_rechecks_signatures_texts_and_current_trust(tmp_path):
    f, item = BatchFixture(tmp_path), batch_request()
    async with f.client() as client:
        complete = await client.purchase_batch(**item)
        changed = copy.deepcopy(complete)
        changed["texts"].reverse()
        client.journal.db.execute("UPDATE purchases SET result=? WHERE id=?", (compact(changed), item["request_id"]))
        calls = len(f.calls)
        with pytest.raises(CovenantError, match="cached_text_mismatch"):
            await client.purchase_batch(**item)
        client.journal.db.execute("UPDATE purchases SET result=? WHERE id=?", (compact(complete), item["request_id"]))
    async with f.client(operators=[str(Keypair().pubkey())]) as client:
        with pytest.raises(CovenantError, match="unexpected_operator"):
            await client.purchase_batch(**item)
    assert len(f.calls) == calls


async def test_batch_response_bound_matches_gateway_and_single_limit_stays_unchanged(tmp_path):
    size = 10 * 1024 * 1024
    f = BatchFixture(tmp_path)
    async def handle(_):
        return httpx.Response(200, content=b"x" * size)
    async with f.client(transport=httpx.MockTransport(handle)) as client:
        with pytest.raises(CovenantError, match="response_too_large"):
            await client.post(batch_request()["request_id"], "{}")
        status, _, body = await client.post(batch_request()["request_id"], "{}", batch=True)
        assert status == 200 and len(body) == size
        size += 1
        with pytest.raises(CovenantError, match="response_too_large"):
            await client.post(batch_request()["request_id"], "{}", batch=True)
