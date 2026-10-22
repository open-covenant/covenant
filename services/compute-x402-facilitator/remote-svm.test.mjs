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
  const state = { verifyCalls: 0, settleCalls: 0, verifyReply: null, settleReply: null, supported: null, supportedFails: false,
    signatures: [], transactions: new Map(), statuses: new Map(), blockhashValid: true, rpcCalls: [] };
  const facilitator = {
    async getSupported() {
      if (state.supportedFails) throw new Error('facilitator unreachable');
      return state.supported ?? { kinds: [{ x402Version: 2, scheme: 'exact', network, extra: { feePayer: config.feePayer } }] };
    },
    async verify(p) {
      state.verifyCalls++;
      if (state.verifyReply instanceof Error) throw state.verifyReply;
      return state.verifyReply ?? { isValid: true, payer: payer.address };
    },
    async settle() { state.settleCalls++; return state.settleReply ?? { success: false, errorReason: 'pending' }; },
  };
  const rpc = async (method, params) => {
    state.rpcCalls.push(method);
    if (method === 'getGenesisHash') return RAILS[network].genesis;
    if (method === 'getSignaturesForAddress') {
      assert.equal(params[0], destination);
      const start = params[1].before ? state.signatures.findIndex(s => s.signature === params[1].before) + 1 : 0;
      return state.signatures.slice(start, start + params[1].limit);
    }
    if (method === 'getTransaction') return state.transactions.get(params[0]) ?? null;
    if (method === 'getSignatureStatuses') return { value: [state.statuses.get(params[0][0]) ?? null] };
    if (method === 'isBlockhashValid') { assert.equal(params[0], BLOCKHASH); return { value: state.blockhashValid }; }
    throw new Error(`unexpected RPC ${method}`);
  };
  let clock = 1000;
  const rail = createRemoteSvmRail({ config, rpc, facilitator, now: () => clock });
  const verified = async (p) => rail.verify({ paymentPayload: p ?? await payload(), paymentRequirements: terms, resourceUrl: config.resourceUrl });
  const land = (signature, wire, { err = null, confirmation = 'finalized' } = {}) => {
    state.signatures.unshift({ signature, blockTime: 1, err });
    state.transactions.set(signature, { transaction: [wire, 'base64'], meta: { err } });
    state.statuses.set(signature, { confirmationStatus: confirmation, err });
  };
  return { rail, state, terms, payload, verified, land, tick: ms => { clock += ms; }, config };
}

const settle = (t, payment) => t.rail.settle({ payment, paymentRequirements: t.terms });
const signature = n => 'S'.repeat(86) + String(n).padStart(2, '0');

test('verification goes to the facilitator and broadcasts nothing', async () => {
  const t = await setup();
  const p = await t.payload();
  const payment = await t.verified(p);
  const authorizationId = createHash('sha256').update(Buffer.from(p.payload.transaction, 'base64')).digest('hex');
  assert.equal(payment.authorization_id, authorizationId);
  assert.equal(payment.transaction_id, authorizationId);
  assert.equal(t.state.verifyCalls, 1);
  assert.equal(t.state.settleCalls, 0);
  assert.deepEqual(t.state.rpcCalls, ['getGenesisHash']);
});

test('a payment naming another fee payer or refused by the facilitator is rejected', async () => {
  const other = await generateKeyPairSigner();
  const t = await setup({ feePayerOverride: other.address });
  await assert.rejects(t.verified(), Rejected);
  const u = await setup();
  u.state.verifyReply = { isValid: false, invalidReason: 'invalid_exact_svm_payload_amount_mismatch' };
  await assert.rejects(u.verified(), /amount_mismatch/);
  u.state.verifyReply = new VerifyError(400, { isValid: false, invalidReason: 'self_send_not_allowed' });
  await assert.rejects(u.verified(), /self_send_not_allowed/);
});

test('an unseen payment is handed to the facilitator once per resubmit interval', async () => {
  const t = await setup();
  const payment = await t.verified();
  assert.deepEqual(await settle(t, payment), { status: 'pending' });
  assert.deepEqual(await settle(t, payment), { status: 'pending' });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(t.state.settleCalls, 1);
  t.tick(RESUBMIT_MS);
  await settle(t, payment);
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(t.state.settleCalls, 2);
});

test('the chain decides: the landed transaction with the same message finalizes', async () => {
  const t = await setup();
  const p = await t.payload();
  const payment = await t.verified(p);
  const unrelated = await t.payload(1300n);
  t.land(signature(1), unrelated.payload.transaction);
  t.land(signature(2), p.payload.transaction, { confirmation: 'confirmed' });
  assert.deepEqual(await settle(t, payment), { status: 'pending' });
  t.state.statuses.set(signature(2), { confirmationStatus: 'finalized', err: null });
  assert.deepEqual(await settle(t, payment), { status: 'finalized', transaction_id: signature(2) });
  assert.equal(t.state.settleCalls, 0);
});

test('a landed transaction that failed on chain fails the payment', async () => {
  const t = await setup();
  const p = await t.payload();
  const payment = await t.verified(p);
  t.land(signature(3), p.payload.transaction, { err: { InstructionError: [2, 'Custom'] } });
  assert.deepEqual(await settle(t, payment), { status: 'failed' });
});

test('an expired blockhash fails only a payment that never landed', async () => {
  const t = await setup();
  const payment = await t.verified();
  t.state.blockhashValid = false;
  assert.deepEqual(await settle(t, payment), { status: 'failed' });
  assert.equal(t.state.settleCalls, 0);

  const u = await setup();
  const p = await u.payload();
  const landedLate = await u.verified(p);
  u.state.blockhashValid = false;
  u.land(signature(4), p.payload.transaction, { confirmation: 'confirmed' });
  assert.deepEqual(await settle(u, landedLate), { status: 'pending' });
});

test('a signature the facilitator reported is checked before any search', async () => {
  const t = await setup();
  const p = await t.payload();
  const payment = await t.verified(p);
  t.state.settleReply = { success: true, transaction: signature(5) };
  await settle(t, payment);
  await new Promise(resolve => setImmediate(resolve));
  t.state.transactions.set(signature(5), { transaction: [p.payload.transaction, 'base64'], meta: { err: null } });
  t.state.statuses.set(signature(5), { confirmationStatus: 'finalized', err: null });
  t.state.rpcCalls.length = 0;
  assert.deepEqual(await settle(t, payment), { status: 'finalized', transaction_id: signature(5) });
  assert.ok(!t.state.rpcCalls.includes('getSignaturesForAddress'));
});

test('tampered evidence or a changed binding fails closed', async () => {
  const t = await setup();
  const payment = await t.verified();
  const saved = JSON.parse(payment.encoded_transaction);
  saved.evidence.payer = 'someone else';
  await assert.rejects(settle(t, { ...payment, encoded_transaction: JSON.stringify(saved) }), /stored authorization changed/);
  await assert.rejects(settle(t, { ...payment, transaction_id: signature(6) }), /binding changed/);
});

test('readiness follows the facilitator fee payer and rides out a short outage', async () => {
  const t = await setup();
  assert.equal((await t.rail.readiness()).feePayer, t.config.feePayer);
  t.state.supportedFails = true;
  t.tick(61_000);
  assert.equal((await t.rail.readiness()).feePayer, t.config.feePayer);
  t.tick(10 * 60_000);
  await assert.rejects(t.rail.readiness(), /unreachable/);
  t.state.supportedFails = false;
  t.state.supported = { kinds: [{ x402Version: 2, scheme: 'exact', network: t.config.network, extra: { feePayer: 'Another1111' } }] };
  await assert.rejects(t.rail.readiness(), /fee payer mismatch/);
});

test('the search pages back to the quote and never calls an unfinished search a failure', async () => {
  const t = await setup();
  const p = await t.payload();
  const payment = await t.verified(p);
  const other = await t.payload(1300n);
  t.land(signature(7), p.payload.transaction);
  for (let n = 0; n < 150; n++) t.land(`U${String(n).padStart(87, '0')}`, other.payload.transaction);
  assert.deepEqual(await settle(t, payment), { status: 'finalized', transaction_id: signature(7) });

  const u = await setup();
  const lost = await u.verified();
  const busy = await u.payload(1300n);
  for (let n = 0; n < 2_100; n++) u.land(`V${String(n).padStart(87, '0')}`, busy.payload.transaction);
  u.state.blockhashValid = false;
  assert.deepEqual(await settle(u, lost), { status: 'pending' });
});
