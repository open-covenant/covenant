import test from 'node:test';
import assert from 'node:assert/strict';
import { generateKeyPairSigner, pipe, createTransactionMessage, setTransactionMessageFeePayer, setTransactionMessageLifetimeUsingBlockhash, appendTransactionMessageInstructions, partiallySignTransactionMessageWithSigners, getBase64EncodedWireTransaction } from '@solana/kit';
import { findAssociatedTokenPda, TOKEN_PROGRAM_ADDRESS, getTransferCheckedInstruction } from '@solana-program/token';
import { getSetComputeUnitLimitInstruction, getSetComputeUnitPriceInstruction } from '@solana-program/compute-budget';
import { toFacilitatorSvmSigner } from '@x402/svm';
import { createBridge, RAILS, Rejected } from './bridge.mjs';

async function setup() {
  const payer = await generateKeyPairSigner();
  const facilitator = await generateKeyPairSigner();
  const merchant = await generateKeyPairSigner();
  const network = 'solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1';
  const config = { network, asset: RAILS[network].asset, payTo: merchant.address, feePayer: facilitator.address, resourceUrl: 'https://example.com/v1/chat/completions', priceAtomic: '1200', secret: 'test-secret-with-at-least-32-characters' };
  const quoteId = '00000000-0000-4000-8000-000000000001';
  const requestSha256 = 'a'.repeat(64);
  const terms = { scheme: 'exact', network, asset: config.asset, payTo: config.payTo, amount: '1200', maxTimeoutSeconds: 90, extra: { feePayer: config.feePayer, memo: `covenant.x402.v2:${quoteId}:${requestSha256}`, quoteId, requestSha256, expiresAtMs: 90_000 } };
  const source = (await findAssociatedTokenPda({ mint: config.asset, owner: payer.address, tokenProgram: TOKEN_PROGRAM_ADDRESS }))[0];
  const destination = (await findAssociatedTokenPda({ mint: config.asset, owner: merchant.address, tokenProgram: TOKEN_PROGRAM_ADDRESS }))[0];
  async function payload(overrides = {}) {
    const tx = pipe(createTransactionMessage({ version: 0 }),
      m => setTransactionMessageFeePayer(facilitator.address, m),
      m => setTransactionMessageLifetimeUsingBlockhash({ blockhash: '11111111111111111111111111111111', lastValidBlockHeight: 1000n }, m),
      m => appendTransactionMessageInstructions([
        getSetComputeUnitLimitInstruction({ units: overrides.units ?? 60_000 }),
        getSetComputeUnitPriceInstruction({ microLamports: overrides.priority ?? 1n }),
        getTransferCheckedInstruction({ source, mint: config.asset, destination, authority: payer, amount: overrides.amount ?? 1200n, decimals: 6 }),
        { programAddress: 'MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr', accounts: [], data: new TextEncoder().encode(overrides.memo ?? terms.extra.memo) },
      ], m));
    const signed = await partiallySignTransactionMessageWithSigners(tx);
    return { x402Version: 2, resource: { url: config.resourceUrl }, accepted: structuredClone(terms), payload: { transaction: getBase64EncodedWireTransaction(signed) } };
  }
  const signer = toFacilitatorSvmSigner(facilitator);
  let simulations = 0; signer.simulateTransaction = async () => { simulations++; };
  const state = { sends: [], status: null, readback: null, loseResponse: false };
  const rpc = async (method, params) => {
    if (method === 'getGenesisHash') return RAILS[network].genesis;
    if (method === 'getSignatureStatuses') return { value: [state.status] };
    if (method === 'getTransaction') return state.readback;
    if (method === 'sendTransaction') { state.sends.push(params[0]); if (state.loseResponse) throw new Error('lost RPC response'); return state.signature; }
    throw new Error(`unexpected RPC ${method}`);
  };
  const bridge = createBridge({ signer, config, rpc, now: () => 1000 });
  const verify = async (overrides = {}) => bridge.verify({ paymentPayload: await payload(overrides), paymentRequirements: terms, resourceUrl: config.resourceUrl });
  return { bridge, verify, terms, state, payload, config, simulations: () => simulations };
}

test('official verifier checks signed memo, amount and fee sponsorship before signing', async () => {
  const f = await setup();
  for (const override of [{ memo: 'copied payment for other request' }, { amount: 1201n }, { units: 60_001 }, { priority: 1001n }]) {
    await assert.rejects(f.verify(override), Rejected);
  }
  assert.equal(f.state.sends.length, 0);
  assert.equal(f.simulations(), 0, 'invalid static policies cannot reach RPC simulation');
  const payment = await f.verify({ units: 60_000, priority: 1000n });
  assert.equal(payment.authorization_id.length, 64);
  assert.ok(payment.transaction_id);
  assert.equal(f.simulations(), 1, 'the exact 60k/1000 ceiling still requires official simulation');
  assert.equal(f.state.sends.length, 0, 'verify must never broadcast');
});

test('restart and lost send reconcile the exact persisted authorization', async () => {
  const f = await setup();
  const payment = await f.verify();
  f.state.signature = payment.transaction_id;
  f.state.loseResponse = true;
  assert.equal((await f.bridge.settle({ payment, paymentRequirements: f.terms })).status, 'pending');
  const recovered = JSON.parse(JSON.stringify(payment));
  assert.equal((await f.bridge.settle({ payment: recovered, paymentRequirements: f.terms })).status, 'pending');
  assert.equal(f.state.sends.length, 2);
  assert.equal(f.state.sends[0], f.state.sends[1]);
  f.state.status = { confirmationStatus: 'finalized', err: null };
  f.state.readback = { meta: { err: null }, transaction: [f.state.sends[0], 'base64'] };
  assert.deepEqual(await f.bridge.settle({ payment: recovered, paymentRequirements: f.terms }), { status: 'finalized', transaction_id: payment.transaction_id });
  assert.equal(f.state.sends.length, 2);
});

test('tampered stored payment, request binding and final chain evidence fail closed', async () => {
  const f = await setup();
  const payment = await f.verify();
  await assert.rejects(f.bridge.settle({ payment: { ...payment, payer: 'different' }, paymentRequirements: f.terms }), Rejected);
  await assert.rejects(f.bridge.settle({ payment, paymentRequirements: { ...f.terms, amount: '1' } }), Rejected);
  const saved = JSON.parse(payment.encoded_transaction); saved.evidence.transaction = 'AAAA';
  await assert.rejects(f.bridge.settle({ payment: { ...payment, encoded_transaction: JSON.stringify(saved) }, paymentRequirements: f.terms }), Rejected);
  f.state.status = { confirmationStatus: 'finalized', err: null };
  f.state.readback = { meta: { err: null }, transaction: ['AAAA', 'base64'] };
  await assert.rejects(f.bridge.settle({ payment, paymentRequirements: f.terms }), /readback mismatch/);
  f.state.status.err = { InstructionError: [0, 'failure'] };
  assert.equal((await f.bridge.settle({ payment, paymentRequirements: f.terms })).status, 'failed');
  assert.equal(f.state.sends.length, 0);
});

test('a copied authorization cannot change quote or request digest', async () => {
  const f = await setup();
  const payload = await f.payload();
  const changed = structuredClone(f.terms);
  changed.extra.requestSha256 = 'b'.repeat(64);
  changed.extra.memo = `covenant.x402.v2:${changed.extra.quoteId}:${changed.extra.requestSha256}`;
  payload.accepted = changed;
  await assert.rejects(f.bridge.verify({ paymentPayload: payload, paymentRequirements: changed, resourceUrl: f.config.resourceUrl }), Rejected);
  assert.equal(f.state.sends.length, 0);
});
