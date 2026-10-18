import asyncio
import base64
import copy
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
from uuid import uuid4

import pytest
from solders.keypair import Keypair
from solders.message import MessageV0, to_bytes_versioned
from solders.signature import Signature
from solders.transaction import VersionedTransaction

from covenant_compute_x402 import CovenantError
from covenant_compute_x402.journal import Journal
from covenant_compute_x402.payment import decode64, expected_message, validate_transaction
from covenant_compute_x402.proof import stable, strict_json, sha256, verify_compute_result, compact
from fixture import Fixture, request, encode


async def test_official_sdk_complete_restart_verified_no_calls(tmp_path):
    f, item = Fixture(tmp_path), request()
    async with f.client() as client:
        result = await client.purchase(**item)
        assert result["status"] == "completed" and result["amountMicroUsdc"] == 1000
        assert f.wallet.signatures == 1 and f.sends == 1
    before = len(f.calls)
    async with f.client() as client:
        assert await client.purchase(**item) == result
    assert len(f.calls) == before and f.wallet.signatures == 1


async def test_payment_header_preserves_rust_integer_term_types(tmp_path):
    f = Fixture(tmp_path)
    async with f.client() as client:
        await client.purchase(**request())
    # Unlike the JS-compatible proof parser, Python's ordinary JSON decoder
    # distinguishes the lexical integer/float forms checked by serde_json.
    raw = base64.b64decode(f.payments[0]).decode("utf-8")
    payload = json.loads(raw)
    assert type(payload["x402Version"]) is int
    assert type(payload["accepted"]["maxTimeoutSeconds"]) is int
    assert type(payload["accepted"]["extra"]["expiresAtMs"]) is int
    assert raw == stable(payload)


@pytest.mark.parametrize("status", [202, 503])
async def test_uncertain_restart_reuses_exact_authorization(tmp_path, status):
    f, item = Fixture(tmp_path, first_status=status), request()
    async with f.client() as client:
        if status == 202:
            assert (await client.purchase(**item))["status"] == "pending"
        else:
            with pytest.raises(CovenantError, match="authorization_retained"):
                await client.purchase(**item)
        assert client.journal.get(item["request_id"])["amount"] == 1000
    async with f.client() as client:
        assert (await client.purchase(**item))["status"] == "completed"
    assert f.quotes == 1 and f.wallet.signatures == 1 and f.payments[0] == f.payments[1]


@pytest.mark.parametrize("field,value", [("network", "solana:devnet"), ("asset", str(Keypair().pubkey())), ("payTo", str(Keypair().pubkey())), ("amount", "1001")])
async def test_wrong_quote_no_signing_dispatch(tmp_path, field, value):
    f = Fixture(tmp_path, bad_quote=lambda q: q.update({field: value}))
    async with f.client() as client:
        with pytest.raises(CovenantError):
            await client.purchase(**request())
    assert f.wallet.signatures == 0 and f.sends == 0


@pytest.mark.parametrize("field,value", [("memo", "different"), ("expiresAtMs", 0), ("requestSha256", "0" * 64)])
async def test_wrong_binding_no_signing_dispatch(tmp_path, field, value):
    f = Fixture(tmp_path, bad_quote=lambda q: q["extra"].update({field: value}))
    async with f.client() as client:
        with pytest.raises(CovenantError):
            await client.purchase(**request())
    assert f.wallet.signatures == 0 and f.sends == 0


async def test_wrong_rpc_never_quotes(tmp_path):
    f = Fixture(tmp_path, wrong_rpc=True)
    async with f.client() as client:
        with pytest.raises(CovenantError, match="rpc_network_mismatch"):
            await client.purchase(**request())
    assert f.wallet.signatures == f.quotes == f.sends == 0


async def test_insufficient_funds_no_signing_dispatch_reserve_retained(tmp_path):
    f, item = Fixture(tmp_path, balance="999"), request()
    async with f.client() as client:
        with pytest.raises(CovenantError, match="insufficient_usdc"):
            await client.purchase(**item)
        assert client.journal.get(item["request_id"])["amount"] == 1000
    assert f.wallet.signatures == 0 and f.sends == 0


async def test_cap_and_request_binding_persist(tmp_path):
    f, item = Fixture(tmp_path), request()
    async with f.client() as client:
        await client.purchase(**item)
        with pytest.raises(CovenantError, match="idempotency_request_mismatch"):
            await client.purchase(**{**item, "max_tokens": 500})
        await client.purchase(**request())
    async with f.client() as client:
        with pytest.raises(CovenantError, match="cumulative_spend_cap_exceeded"):
            await client.purchase(**request())
    assert f.wallet.signatures == 2 and f.sends == 2


async def test_result_substitution_retains_uncertain_authorization(tmp_path):
    f, item = Fixture(tmp_path, bad_result=lambda r: r["choices"][0]["message"].update(content="wrong")), request()
    async with f.client() as client:
        with pytest.raises(CovenantError, match="completion_output_mismatch"):
            await client.purchase(**item)
        row = client.journal.get(item["request_id"])
        assert row["payment"] and not row["result"] and row["amount"] == 1000


@pytest.mark.parametrize("config_key", ["pay_to", "gateway_buyer", "operators"])
async def test_cache_revalidates_current_trust(tmp_path, config_key):
    f, item = Fixture(tmp_path), request()
    async with f.client() as client:
        await client.purchase(**item)
    before = len(f.calls)
    value = str(Keypair().pubkey())
    async with f.client(**{config_key: [value] if config_key == "operators" else value}) as client:
        with pytest.raises(CovenantError):
            await client.purchase(**item)
    assert len(f.calls) == before


async def test_corrupt_cached_text_fails_offline(tmp_path):
    f, item = Fixture(tmp_path), request()
    async with f.client() as client:
        result = await client.purchase(**item)
        result["text"] = "changed"
        client.journal.db.execute("UPDATE purchases SET result=? WHERE id=?", (compact(result), item["request_id"]))
        before = len(f.calls)
        with pytest.raises(CovenantError, match="cached_text_mismatch"):
            await client.purchase(**item)
        assert len(f.calls) == before


async def test_confirmation_wrong_wire_or_failure_never_complete(tmp_path):
    f, item = Fixture(tmp_path, bad_settlement=lambda r: r["meta"].update(err="failed")), request()
    async with f.client() as client:
        with pytest.raises(CovenantError, match="settlement_not_confirmed"):
            await client.purchase(**item)
        assert not client.journal.get(item["request_id"])["result"]


@pytest.mark.parametrize("messages", [
    [{"role": "user", "content": "界" * 1400}],
    [{"role": "user", "content": "\n" * 2050}],
    [{"role": "user", "content": "x"}] * 33,
    [{"role": "user", "content": "\ud800"}],
    [{"role": "user", "content": "x", "extra": "bad"}],
])
async def test_invalid_request_before_any_reservation(tmp_path, messages):
    f, item = Fixture(tmp_path), {**request(), "messages": messages}
    async with f.client() as client:
        with pytest.raises(CovenantError):
            await client.purchase(**item)
        assert client.journal.get(item["request_id"]) is None
    assert not f.calls


async def test_exact_utf8_boundary(tmp_path):
    f = Fixture(tmp_path)
    overhead = len(compact([{"role": "user", "content": ""}]).encode())
    async with f.client() as client:
        assert (await client.purchase(**{**request(), "messages": [{"role": "user", "content": "x" * (4096 - overhead)}]}))["status"] == "completed"


def test_duplicate_utf8_and_number_rejection():
    for source in ['{"a":1,"a":2}', '{"a":1,"\\u0061":2}', 'NaN', '1e999', b'"\xff"', '"\\ud800"', '[' * 129 + '0' + ']' * 129]:
        with pytest.raises(CovenantError):
            strict_json(source)


@pytest.mark.parametrize("temperature", [0.001, 0.10000000001, 0.30000000000000004, float("nan"), True])
async def test_temperature_matches_js_decimal_steps(tmp_path, temperature):
    f = Fixture(tmp_path)
    async with f.client() as client:
        with pytest.raises(CovenantError, match="invalid_temperature"):
            await client.purchase(**{**request(), "temperature": temperature})
    assert not f.calls


def test_real_native_rust_fixture_and_exact_float_output():
    f = strict_json((Path(__file__).parent / "fixtures/native-response.json").read_bytes())
    result, request_data = f["response"], f["request"]
    proof = result["covenant"]
    args = dict(request=request_data, request_sha256=sha256(stable(request_data)), quote_id=proof["quote_id"], gateway_buyer=proof["envelope"]["signer_pubkey_b58"], operators=[proof["receipt"]["signer_pubkey_b58"]])
    assert verify_compute_result(result, **args)["text"] == "Light separates into colors.\nVerified fixture."
    tampered = copy.deepcopy(result)
    tampered["covenant"]["output_json"] = compact(tampered["covenant"]["output"])
    with pytest.raises(CovenantError, match="output_hash"):
        verify_compute_result(tampered, **args)
    tampered = copy.deepcopy(result)
    tampered["covenant"]["receipt"]["signature_b58"] = str(Signature.default())
    with pytest.raises(CovenantError, match="invalid_signature"):
        verify_compute_result(tampered, **args)


def test_claims_and_reservations_shared(tmp_path):
    a, b = Journal(tmp_path / "journal", "scope", 1000), Journal(tmp_path / "journal", "scope", 1000)
    try:
        one = a.claim("one", "body", "digest")
        with pytest.raises(CovenantError, match="purchase_in_progress"):
            b.claim("one", "body", "digest")
        a.reserve("one", one["owner"], {}, 1000)
        two = b.claim("two", "body2", "digest2")
        with pytest.raises(CovenantError, match="cumulative_spend_cap_exceeded"):
            b.reserve("two", two["owner"], {}, 1)
    finally:
        a.close(); b.close()


def test_journal_private_and_symlink(tmp_path):
    path = tmp_path / "public"; path.write_text(""); path.chmod(0o644)
    with pytest.raises(CovenantError, match="private_regular"):
        Journal(path, "scope", 1000)
    path.chmod(0o600)
    link = tmp_path / "link"; link.symlink_to(path)
    with pytest.raises(CovenantError, match="private_regular"):
        Journal(link, "scope", 1000)


async def test_complete_transaction_rejects_mutation_and_presigned_sdk(tmp_path):
    f, item = Fixture(tmp_path), request()
    async with f.client() as client:
        await client.purchase(**item)
        payload = strict_json(decode64(f.payments[0])); quote = payload["accepted"]
        tx = VersionedTransaction.from_bytes(decode64(payload["payload"]["transaction"]))
        for field, value in [("amount", "999"), ("payTo", str(Keypair().pubkey()))]:
            changed = copy.deepcopy(quote); changed[field] = value
            with pytest.raises(CovenantError, match="not_approved"):
                validate_transaction(tx, changed, f.wallet.address)
        with pytest.raises(CovenantError):
            validate_transaction(tx, quote, f.wallet.address, unsigned=True)


async def test_wallet_mutation_refused_before_dispatch(tmp_path):
    f = Fixture(tmp_path)
    original = f.wallet.sign_transaction
    # Use a valid solders Hash while changing the message.
    from solders.hash import Hash
    async def bad(tx):
        changed = MessageV0(tx.message.header, tx.message.account_keys, Hash.new_unique(), tx.message.instructions, [])
        return await original(VersionedTransaction.populate(changed, list(tx.signatures)))
    f.wallet.sign_transaction = bad
    async with f.client() as client:
        with pytest.raises(CovenantError, match="wallet_changed"):
            await client.purchase(**request())
    assert f.sends == 0


@pytest.mark.parametrize("mutation", ["amount", "fee", "memo", "recipient", "priority", "instruction", "signature"])
async def test_sdk_unsigned_mutations_fail_before_wallet_signing(tmp_path, monkeypatch, mutation):
    from covenant_compute_x402.payment import _SnapshotScheme
    from solders.instruction import CompiledInstruction
    original = _SnapshotScheme.create_payment_payload
    def changed(scheme, requirements):
        payload = original(scheme, requirements)
        tx = VersionedTransaction.from_bytes(decode64(payload["transaction"]))
        q = requirements.model_dump(by_alias=True, exclude_none=True)
        if mutation == "amount":
            q["amount"] = "999"
        elif mutation == "fee":
            q["extra"]["feePayer"] = str(Keypair().pubkey())
        elif mutation == "memo":
            q["extra"]["memo"] = "different"
        elif mutation == "recipient":
            q["payTo"] = str(Keypair().pubkey())
        message = expected_message(q, scheme._signer.address, tx.message.recent_blockhash, 20_000)
        instructions = list(message.instructions)
        if mutation == "priority":
            ix = instructions[1]
            instructions[1] = CompiledInstruction(ix.program_id_index, b"\x03" + (2).to_bytes(8, "little"), ix.accounts)
        elif mutation == "instruction":
            instructions.append(instructions[-1])
        message = MessageV0(message.header, message.account_keys, message.recent_blockhash, instructions, [])
        sigs = [Signature.default(), Signature.default()]
        if mutation == "signature":
            sigs[1] = Keypair().sign_message(to_bytes_versioned(message))
        payload["transaction"] = base64.b64encode(bytes(VersionedTransaction.populate(message, sigs))).decode()
        return payload
    monkeypatch.setattr(_SnapshotScheme, "create_payment_payload", changed)
    f = Fixture(tmp_path)
    async with f.client() as client:
        with pytest.raises(CovenantError):
            await client.purchase(**request())
    assert f.wallet.signatures == f.sends == 0
