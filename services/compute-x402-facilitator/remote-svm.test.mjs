import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { generateKeyPairSigner, pipe, createTransactionMessage, setTransactionMessageFeePayer, setTransactionMessageLifetimeUsingBlockhash, appendTransactionMessageInstructions, partiallySignTransactionMessageWithSigners, getBase64EncodedWireTransaction } from '@solana/kit';
import { findAssociatedTokenPda, TOKEN_PROGRAM_ADDRESS, getTransferCheckedInstruction } from '@solana-program/token';
import { getSetComputeUnitLimitInstruction, getSetComputeUnitPriceInstruction } from '@solana-program/compute-budget';
import { VerifyError } from '@x402/core/types';
import { RAILS, Rejected, shortMemo } from './bridge.mjs';
import { createRemoteSvmRail, RESUBMIT_MS } from './remote-svm.mjs';

const BLOCKHASH = '11111111111111111111111111111111';
const signature = n => 'S'.repeat(86) + String(n).padStart(2, '0');

async function setup({ feePayerOverride } = {}) {
  const payer = await generateKeyPairSigner();
  const facilitatorKey = await generateKeyPairSigner();
  const merchant = await generateKeyPairSigner();
  const network = 'solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1';
  const config = { network, asset: RAILS[network].asset, payTo: merchant.address, feePayer: facilitatorKey.address,
    resourceUrl: 'https://example.com/v1/chat/completions', priceAtomic: '1200', secret: 'test-secret-with-at-least-32-characters' };
  const quoteId = '00000000-0000-4000-8000-000000000001';
  const requestSha256 = 'a'.repeat(64);
  const terms = { scheme: 'exact', network, asset: config.asset, payTo: config.payTo, amount: '1200', maxTimeoutSeconds: 90,
    extra: { feePayer: config.feePayer, memo: shortMemo(quoteId, requestSha256), quoteId, requestSha256, expiresAtMs: 90_000 } };
  const source = (await findAssociatedTokenPda({ mint: config.asset, owner: payer.address, tokenProgram: TOKEN_PROGRAM_ADDRESS }))[0];
  const destination = (await findAssociatedTokenPda({ mint: config.asset, owner: merchant.address, tokenProgram: TOKEN_PROGRAM_ADDRESS }))[0];
  async function payload(amount = 1200n) {
    const tx = pipe(createTransactionMessage({ version: 0 }),
      m => setTransactionMessageFeePayer(feePayerOverride ?? facilitatorKey.address, m),
      m => setTransactionMessageLifetimeUsingBlockhash({ blockhash: BLOCKHASH, lastValidBlockHeight: 1000n }, m),
      m => appendTransactionMessageInstructions([
        getSetComputeUnitLimitInstruction({ units: 20_000 }),
        getSetComputeUnitPriceInstruction({ microLamports: 1n }),
        getTransferCheckedInstruction({ source, mint: config.asset, destination, authority: payer, amount, decimals: 6 }),
        { programAddress: 'MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr', accounts: [], data: new TextEncoder().encode(terms.extra.memo) },
      ], m));
    const signed = await partiallySignTransactionMessageWithSigners(tx);
    return { x402Version: 2, resource: { url: config.resourceUrl }, accepted: structuredClone(terms), payload: { transaction: getBase64EncodedWireTransaction(signed) } };
  }
  const state = { localVerify: null, remoteVerify: null, localCalls: 0, remoteCalls: 0, settleCalls: 0, settleReply: null,
    signatures: [], transactions: new Map(), statuses: new Map(), blockhashValid: true, rpcCalls: [], hold: null };
  const verifier = { async verify() { state.localCalls++; return state.localVerify ?? { isValid: true, payer: payer.address }; } };
  const facilitator = {
    async verify() {
      state.remoteCalls++;
      if (state.remoteVerify instanceof Error) throw state.remoteVerify;
      return state.remoteVerify ?? { isValid: true, payer: payer.address };
    },
    async settle() { state.settleCalls++; return state.settleReply ?? { success: false, errorReason: 'pending' }; },
  };
  const rpc = async (method, params) => {
    state.rpcCalls.push(method);
    if (state.hold) await state.hold;
    if (method === 'getGenesisHash') return RAILS[network].genesis;
    if (method === 'getSignaturesForAddress') {
      assert.equal(params[0], source);
      const visible = state.signatures.filter(s => params[1].commitment !== 'finalized' || state.statuses.get(s.signature)?.confirmationStatus === 'finalized');
      const start = params[1].before ? visible.findIndex(s => s.signature === params[1].before) + 1 : 0;
      return visible.slice(start, start + params[1].limit);
    }
    if (method === 'getTransaction') return state.transactions.get(params[0]) ?? null;
    if (method === 'getSignatureStatuses') return { value: [state.statuses.get(params[0][0]) ?? null] };
    if (method === 'isBlockhashValid') { assert.equal(params[1].commitment, 'finalized'); return { context: { slot: 77 }, value: state.blockhashValid }; }
    throw new Error(`unexpected RPC ${method}`);
  };
  let clock = 1000;
  const rail = createRemoteSvmRail({ config, rpc, facilitator, verifier, now: () => clock });
  const verified = async (p) => rail.verify({ paymentPayload: p ?? await payload(), paymentRequirements: terms, resourceUrl: config.resourceUrl });
  const land = (sig, wire, { err = null, confirmation = 'finalized', readable = true } = {}) => {
    state.signatures.unshift({ signature: sig, blockTime: 1, err });
    if (readable) state.transactions.set(sig, { transaction: [wire, 'base64'], meta: { err } });
    state.statuses.set(sig, { confirmationStatus: confirmation, err });
  };
  return { rail, state, terms, payload, verified, land, tick: ms => { clock += ms; }, config };
}

const settle = (t, payment) => t.rail.settle({ payment, paymentRequirements: t.terms });
const flush = () => new Promise(resolve => setImmediate(resolve));

test('verification runs here and at the facilitator, and broadcasts nothing', async () => {
  const t = await setup();
  const p = await t.payload();
  const payment = await t.verified(p);
  const authorizationId = createHash('sha256').update(Buffer.from(p.payload.transaction, 'base64')).digest('hex');
  assert.equal(payment.authorization_id, authorizationId);
  assert.equal(payment.transaction_id, authorizationId);
  assert.equal(t.state.localCalls, 1);
  assert.equal(t.state.remoteCalls, 1);
  assert.equal(t.state.settleCalls, 0);
  assert.deepEqual(t.state.rpcCalls, ['getGenesisHash']);
});

test('either verifier refusing, a payer disagreement or another fee payer is rejected', async () => {
  const other = await generateKeyPairSigner();
  const t = await setup({ feePayerOverride: other.address });
  await assert.rejects(t.verified(), Rejected);
  const u = await setup();
  u.state.localVerify = { isValid: false, invalidReason: 'invalid_exact_svm_payload_amount_mismatch' };
  await assert.rejects(u.verified(), /amount_mismatch/);
  assert.equal(u.state.remoteCalls, 0);
  u.state.localVerify = null;
  u.state.remoteVerify = new VerifyError(400, { isValid: false, invalidReason: 'self_send_not_allowed' });
  await assert.rejects(u.verified(), /self_send_not_allowed/);
  u.state.remoteVerify = { isValid: true, payer: other.address };
  await assert.rejects(u.verified(), /invalid payment/);
});

test('an unseen payment is submitted once per interval without waiting on a search', async () => {
  const t = await setup();
  const payment = await t.verified();
  assert.deepEqual(await settle(t, payment), { status: 'pending' });
  assert.deepEqual(await settle(t, payment), { status: 'pending' });
  await flush();
  assert.equal(t.state.settleCalls, 1);
  t.tick(RESUBMIT_MS);
  await settle(t, payment);
  await flush();
  assert.equal(t.state.settleCalls, 2);
});

test('the landed transaction with the same message decides, at finalized only', async () => {
  const t = await setup();
  const p = await t.payload();
  const payment = await t.verified(p);
  t.land(signature(1), (await t.payload(1300n)).payload.transaction);
  t.land(signature(2), p.payload.transaction, { confirmation: 'confirmed', err: { InstructionError: [2, 'Custom'] } });
  // An error seen before finality is not yet a failure.
  assert.deepEqual(await settle(t, payment), { status: 'pending' });
  t.state.statuses.set(signature(2), { confirmationStatus: 'finalized', err: null });
  assert.deepEqual(await settle(t, payment), { status: 'finalized', transaction_id: signature(2) });
});

test('a finalized on-chain error fails the payment', async () => {
  const t = await setup();
  const p = await t.payload();
  const payment = await t.verified(p);
  t.land(signature(3), p.payload.transaction, { err: { InstructionError: [2, 'Custom'] } });
  assert.deepEqual(await settle(t, payment), { status: 'failed' });
});

test('after expiry, only a complete search that finds nothing fails the payment', async () => {
  const t = await setup();
  const payment = await t.verified();
  t.land(signature(4), (await t.payload(1300n)).payload.transaction);
  t.state.blockhashValid = false;
  assert.deepEqual(await settle(t, payment), { status: 'failed' });
  assert.equal(t.state.settleCalls, 0);

  const u = await setup();
  const p = await u.payload();
  const landed = await u.verified(p);
  u.state.blockhashValid = false;
  u.land(signature(5), p.payload.transaction);
  assert.deepEqual(await settle(u, landed), { status: 'finalized', transaction_id: signature(5) });

  const v = await setup();
  const q = await v.payload();
  const unreadable = await v.verified(q);
  v.state.blockhashValid = false;
  v.land(signature(6), q.payload.transaction, { readable: false });
  assert.deepEqual(await settle(v, unreadable), { status: 'pending' });
});

test('a reported signature that does not check out falls through to the search', async () => {
  const t = await setup();
  const p = await t.payload();
  const payment = await t.verified(p);
  t.state.settleReply = { success: true, transaction: signature(7) };
  await settle(t, payment);
  await flush();
  t.land(signature(7), (await t.payload(1300n)).payload.transaction);
  t.land(signature(8), p.payload.transaction);
  t.state.blockhashValid = false;
  assert.deepEqual(await settle(t, payment), { status: 'finalized', transaction_id: signature(8) });
});

test('a search that cannot reach back to the quote stays pending, even after expiry', async () => {
  const t = await setup();
  const p = await t.payload();
  const payment = await t.verified(p);
  const other = (await t.payload(1300n)).payload.transaction;
  t.land(signature(9), p.payload.transaction);
  for (let n = 0; n < 150; n++) t.land(`U${String(n).padStart(87, '0')}`, other);
  t.state.blockhashValid = false;
  // Each call reads a bounded number of transactions and remembers the rest.
  let result = { status: 'pending' };
  for (let call = 0; call < 6 && result.status === 'pending'; call++) result = await settle(t, payment);
  assert.deepEqual(result, { status: 'finalized', transaction_id: signature(9) });
});

test('one settle per payment runs at a time', async () => {
  const t = await setup();
  const payment = await t.verified();
  let release;
  t.state.hold = new Promise(resolve => { release = resolve; });
  const first = settle(t, payment);
  await flush();
  const calls = t.state.rpcCalls.length;
  assert.deepEqual(await settle(t, payment), { status: 'pending' });
  assert.equal(t.state.rpcCalls.length, calls);
  t.state.hold = null;
  release();
  await first;
});

test('tampered evidence or a changed binding fails closed', async () => {
  const t = await setup();
  const payment = await t.verified();
  const saved = JSON.parse(payment.encoded_transaction);
  saved.evidence.payer = 'someone else';
  await assert.rejects(settle(t, { ...payment, encoded_transaction: JSON.stringify(saved) }), /stored authorization changed/);
  await assert.rejects(settle(t, { ...payment, transaction_id: signature(10) }), /binding changed/);
});

test('readiness reports the configured terms without calling the facilitator', async () => {
  const t = await setup();
  const ready = t.rail.readiness();
  assert.equal(ready.feePayer, t.config.feePayer);
  assert.equal(t.state.remoteCalls + t.state.rpcCalls.length, 0);
});
