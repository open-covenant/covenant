import test from 'node:test';
import assert from 'node:assert/strict';
import {
  appendTransactionMessageInstructions, assertIsFullySignedTransaction, createTransactionMessage, generateKeyPairSigner,
  getBase64EncodedWireTransaction, getCompiledTransactionMessageDecoder, getTransactionDecoder, partiallySignTransactionMessageWithSigners,
  pipe, setTransactionMessageFeePayer, setTransactionMessageLifetimeUsingBlockhash,
} from '@solana/kit';
import { findAssociatedTokenPda, getTransferCheckedInstruction, TOKEN_PROGRAM_ADDRESS } from '@solana-program/token';
import { getSetComputeUnitLimitInstruction, getSetComputeUnitPriceInstruction } from '@solana-program/compute-budget';
import { toFacilitatorSvmSigner } from '@x402/svm';
import { createBridge, EXPIRY_MARGIN_MS, jobPriceTable, RAILS, Rejected, shortMemo } from './bridge.mjs';
import { refundMemo } from './refunds.mjs';

const NETWORK = 'solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1';
const RESEARCH = 'https://example.com/v1/jobs/research';
const BLOCKHASH = '11111111111111111111111111111111';

async function setup() {
  const payer = await generateKeyPairSigner();
  const feePayer = await generateKeyPairSigner();
  const merchant = await generateKeyPairSigner();
  const asset = RAILS[NETWORK].asset;
  const config = { network: NETWORK, asset, payTo: merchant.address, feePayer: feePayer.address,
    resourceUrl: 'https://example.com/v1/chat/completions', secret: 'test-secret-with-at-least-32-characters',
    jobPrices: { [RESEARCH]: '5000000' } };
  const quoteId = '00000000-0000-4000-8000-000000000001';
  const requestSha256 = 'b'.repeat(64);
  const terms = { scheme: 'exact', network: NETWORK, asset, payTo: merchant.address, amount: '5000000', maxTimeoutSeconds: 90,
    extra: { feePayer: feePayer.address, memo: shortMemo(quoteId, requestSha256), quoteId, requestSha256, expiresAtMs: 90_000 } };
  const [source] = await findAssociatedTokenPda({ mint: asset, owner: payer.address, tokenProgram: TOKEN_PROGRAM_ADDRESS });
  const [destination] = await findAssociatedTokenPda({ mint: asset, owner: merchant.address, tokenProgram: TOKEN_PROGRAM_ADDRESS });
  async function payload({ amount = 5_000_000n, resourceUrl = RESEARCH } = {}) {
    const message = pipe(createTransactionMessage({ version: 0 }),
      m => setTransactionMessageFeePayer(feePayer.address, m),
      m => setTransactionMessageLifetimeUsingBlockhash({ blockhash: BLOCKHASH, lastValidBlockHeight: 1000n }, m),
      m => appendTransactionMessageInstructions([
        getSetComputeUnitLimitInstruction({ units: 20_000 }),
        getSetComputeUnitPriceInstruction({ microLamports: 1n }),
        getTransferCheckedInstruction({ source, mint: asset, destination, authority: payer, amount, decimals: 6 }),
        { programAddress: 'MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr', accounts: [], data: new TextEncoder().encode(terms.extra.memo) },
      ], m));
    const signed = await partiallySignTransactionMessageWithSigners(message);
    return { x402Version: 2, resource: { url: resourceUrl }, accepted: structuredClone(terms), payload: { transaction: getBase64EncodedWireTransaction(signed) } };
  }
  const signer = toFacilitatorSvmSigner(feePayer);
  signer.simulateTransaction = async () => {};
  const state = { statuses: {}, sends: [], height: 100, valid: { processed: true, finalized: true }, clock: 1000 };
  const rpc = async (method, params) => {
    if (method === 'getGenesisHash') return RAILS[NETWORK].genesis;
    if (method === 'getSignatureStatuses') return { value: [state.statuses[params[0][0]] ?? null] };
    if (method === 'getTransaction') return state.readback;
    if (method === 'sendTransaction') { state.sends.push(params[0]); if (state.refuseSend) throw new Error(state.refuseSend); return 'sent'; }
    if (method === 'getLatestBlockhash') return { value: { blockhash: BLOCKHASH, lastValidBlockHeight: 2000 } };
    if (method === 'getBlockHeight') return state.height;
    if (method === 'isBlockhashValid') return { value: state.valid[params[1].commitment] };
    throw new Error(`unexpected RPC ${method}`);
  };
  const logs = [];
  const open = (changes = {}) => createBridge({ signer, config: { ...config, ...changes }, rpc, now: () => state.clock,
    refundSigners: { feePayer, merchant }, log: message => logs.push(message) });
  const bridge = open();
  const verify = async (overrides = {}) => bridge.verify({ paymentPayload: await payload(overrides), paymentRequirements: terms, resourceUrl: overrides.resourceUrl ?? RESEARCH });
  return { bridge, open, verify, payload, terms, state, payer, feePayer, merchant, asset, logs };
}

/** A verified payment that has finalized on chain. */
async function paid(f) {
  const payment = await f.verify();
  f.state.statuses[payment.transaction_id] = { confirmationStatus: 'finalized', err: null };
  return payment;
}

test('a jobs rail reports its prices and merchant, and admits only a priced job at its exact amount', async () => {
  const f = await setup();
  const readiness = f.bridge.readiness();
  assert.deepEqual(readiness.jobPrices, { [RESEARCH]: '5000000' });
  assert.equal(readiness.payTo, f.merchant.address);
  assert.equal(Object.hasOwn(readiness, 'resourceUrl'), false);
  const payment = await f.verify();
  assert.equal(payment.payer, f.payer.address);
  await assert.rejects(f.verify({ resourceUrl: 'https://example.com/v1/chat/completions' }), Rejected);
  await assert.rejects(f.verify({ resourceUrl: 'https://example.com/v1/jobs/agent' }), Rejected);
  // A payment for a price the rail no longer sells at is refused.
  const repriced = f.open({ jobPrices: { [RESEARCH]: '4000000' } });
  await assert.rejects(repriced.verify({ paymentPayload: await f.payload(), paymentRequirements: f.terms, resourceUrl: RESEARCH }), Rejected);
});

test('job prices name known job kinds on https URLs with positive amounts', () => {
  assert.equal(jobPriceTable({ [RESEARCH]: '5000000' }).get(RESEARCH), '5000000');
  for (const prices of [{}, { 'https://example.com/v1/jobs/other': '1' }, { 'http://example.com/v1/jobs/research': '1' },
    { [RESEARCH]: '0' }, { [RESEARCH]: 5000000 }, { [`${RESEARCH}?x=1`]: '1' }, null, []]) {
    assert.throws(() => jobPriceTable(prices), /invalid job prices/);
  }
});

test('a jobs rail needs the merchant and fee payer keys it refunds with', async () => {
  const f = await setup();
  const stranger = await generateKeyPairSigner();
  assert.throws(() => createBridge({ signer: toFacilitatorSvmSigner(f.feePayer), config: { network: NETWORK, asset: f.asset, payTo: f.merchant.address,
    feePayer: f.feePayer.address, resourceUrl: 'https://example.com/v1/chat/completions', secret: 'x'.repeat(40), jobPrices: { [RESEARCH]: '1' } },
    rpc: async () => {}, refundSigners: { feePayer: f.feePayer, merchant: stranger } }), /merchant and fee payer keys/);
});

test('a refund is built only for a finalized payment, for its full amount, to its payer', async () => {
  const f = await setup();
  const payment = await f.verify();
  await assert.rejects(f.bridge.refund({ payment, paymentRequirements: f.terms }), /only a finalized payment/);
  f.state.statuses[payment.transaction_id] = { confirmationStatus: 'finalized', err: { InstructionError: [2, 'Custom'] } };
  await assert.rejects(f.bridge.refund({ payment, paymentRequirements: f.terms }), /only a finalized payment/);
  f.state.statuses[payment.transaction_id] = { confirmationStatus: 'finalized', err: null };
  const refund = await f.bridge.refund({ payment, paymentRequirements: f.terms });
  const { evidence } = JSON.parse(refund.encoded_refund);
  assert.equal(evidence.signature, refund.transaction_id);
  assert.equal(evidence.paymentTransactionId, payment.transaction_id);
  assert.equal(evidence.lastValidBlockHeight, 2000);
  const tx = getTransactionDecoder().decode(Buffer.from(evidence.transaction, 'base64'));
  assertIsFullySignedTransaction(tx);
  const message = getCompiledTransactionMessageDecoder().decode(tx.messageBytes);
  const [payerAccount] = await findAssociatedTokenPda({ mint: f.asset, owner: f.payer.address, tokenProgram: TOKEN_PROGRAM_ADDRESS });
  const [merchantAccount] = await findAssociatedTokenPda({ mint: f.asset, owner: f.merchant.address, tokenProgram: TOKEN_PROGRAM_ADDRESS });
  assert.equal(message.staticAccounts[0], f.feePayer.address);
  for (const account of [payerAccount, merchantAccount, f.merchant.address]) assert.ok(message.staticAccounts.includes(account));
  const transfer = message.instructions.find(ix => message.staticAccounts[ix.programAddressIndex] === TOKEN_PROGRAM_ADDRESS);
  assert.equal(Buffer.from(transfer.data).readBigUInt64LE(1), 5_000_000n);
  assert.ok(Buffer.from(tx.messageBytes).includes(Buffer.from(refundMemo(f.terms.extra.quoteId))));
  assert.deepEqual(f.state.sends, [], 'building a refund broadcasts nothing');
});

test('a stored refund is broadcast as stored, and only finality or expiry ends it', async () => {
  const f = await setup();
  const payment = await paid(f);
  const refund = await f.bridge.refund({ payment, paymentRequirements: f.terms });
  const status = () => f.bridge.refundStatus({ payment, paymentRequirements: f.terms, refund: refund.encoded_refund });
  const stored = JSON.parse(refund.encoded_refund).evidence.transaction;
  assert.deepEqual(await status(), { status: 'pending' });
  assert.deepEqual(f.state.sends, [stored]);
  f.state.refuseSend = 'insufficient funds';
  assert.deepEqual(await status(), { status: 'pending' });
  assert.match(f.logs.at(-1), /refund broadcast refused: insufficient funds/);
  f.state.statuses[refund.transaction_id] = { confirmationStatus: 'confirmed', err: null };
  assert.deepEqual(await status(), { status: 'pending' });
  f.state.statuses[refund.transaction_id] = { confirmationStatus: 'finalized', err: null };
  assert.deepEqual(await status(), { status: 'finalized', transaction_id: refund.transaction_id });
  f.state.statuses[refund.transaction_id] = { confirmationStatus: 'finalized', err: { InstructionError: [3, 'Custom'] } };
  assert.deepEqual(await status(), { status: 'failed' });
  delete f.state.statuses[refund.transaction_id];
  f.state.height = 2001;
  assert.deepEqual(await status(), { status: 'expired' });
  assert.equal(new Set(f.state.sends).size, 1);
});

test('a refund record that was changed, or belongs to another payment, is refused', async () => {
  const f = await setup();
  const payment = await paid(f);
  const refund = await f.bridge.refund({ payment, paymentRequirements: f.terms });
  const saved = JSON.parse(refund.encoded_refund);
  saved.evidence.lastValidBlockHeight = 99_999_999;
  await assert.rejects(f.bridge.refundStatus({ payment, paymentRequirements: f.terms, refund: JSON.stringify(saved) }), /stored refund changed/);
  const other = await setup();
  const otherPayment = await paid(other);
  const otherRefund = await other.bridge.refund({ payment: otherPayment, paymentRequirements: other.terms });
  await assert.rejects(f.bridge.refundStatus({ payment, paymentRequirements: f.terms, refund: otherRefund.encoded_refund }), Rejected);
});

test('an inference rail has no refunds', async () => {
  const f = await setup();
  const inference = createBridge({ signer: toFacilitatorSvmSigner(f.feePayer), config: { network: NETWORK, asset: f.asset, payTo: f.merchant.address,
    feePayer: f.feePayer.address, resourceUrl: 'https://example.com/v1/chat/completions', secret: 'x'.repeat(40), priceAtomic: '1000' }, rpc: async () => {} });
  assert.equal(inference.refund, undefined);
  assert.equal(inference.refundStatus, undefined);
});

test('a job payment ends unpaid only once its blockhash has aged out of every bank', async () => {
  const f = await setup();
  const payment = await f.verify();
  const settle = () => f.bridge.settle({ payment, paymentRequirements: f.terms });
  // Too soon after the quote: never judged, only rebroadcast.
  f.state.valid = { processed: false, finalized: false };
  assert.deepEqual(await settle(), { status: 'pending' });
  f.state.clock = f.terms.extra.expiresAtMs + EXPIRY_MARGIN_MS;
  // Newer than the finalized bank, or still valid at the newest one: pending.
  f.state.valid = { processed: true, finalized: false };
  assert.deepEqual(await settle(), { status: 'pending' });
  f.state.valid = { processed: false, finalized: true };
  assert.deepEqual(await settle(), { status: 'pending' });
  f.state.valid = { processed: false, finalized: false };
  assert.deepEqual(await settle(), { status: 'failed' });
});
