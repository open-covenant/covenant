import test from 'node:test';
import assert from 'node:assert/strict';
import { createHmac } from 'node:crypto';
import {
  appendTransactionMessageInstructions, assertIsFullySignedTransaction, createTransactionMessage, generateKeyPairSigner,
  getBase64EncodedWireTransaction, getCompiledTransactionMessageDecoder, getSignatureFromTransaction, getTransactionDecoder,
  partiallySignTransactionMessageWithSigners, pipe, setTransactionMessageFeePayer, setTransactionMessageFeePayerSigner,
  setTransactionMessageLifetimeUsingBlockhash, signTransactionMessageWithSigners,
} from '@solana/kit';
import { findAssociatedTokenPda, getTransferCheckedInstruction, TOKEN_PROGRAM_ADDRESS } from '@solana-program/token';
import { getSetComputeUnitLimitInstruction, getSetComputeUnitPriceInstruction } from '@solana-program/compute-budget';
import { toFacilitatorSvmSigner } from '@x402/svm';
import { canonical, createBridge, EXPIRY_MARGIN_MS, jobPriceTable, RAILS, Rejected, shortMemo } from './bridge.mjs';
import { MEMO_PROGRAM, refundMarker, refundMemo } from './refunds.mjs';

const NETWORK = 'solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1';
const RESEARCH = 'https://example.com/v1/jobs/research';
const BLOCKHASH = '11111111111111111111111111111111';
const SECRET = 'test-secret-with-at-least-32-characters';
const ASSOCIATED_TOKEN_PROGRAM = 'ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL';

async function setup() {
  const payer = await generateKeyPairSigner();
  const feePayer = await generateKeyPairSigner();
  const merchant = await generateKeyPairSigner();
  const asset = RAILS[NETWORK].asset;
  const config = { network: NETWORK, asset, payTo: merchant.address, feePayer: feePayer.address,
    resourceUrl: 'https://example.com/v1/chat/completions', secret: SECRET, jobPrices: { [RESEARCH]: '5000000' } };
  const quoteId = '00000000-0000-4000-8000-000000000001';
  const requestSha256 = 'b'.repeat(64);
  const terms = { scheme: 'exact', network: NETWORK, asset, payTo: merchant.address, amount: '5000000', maxTimeoutSeconds: 90,
    extra: { feePayer: feePayer.address, memo: shortMemo(quoteId, requestSha256), quoteId, requestSha256, expiresAtMs: 90_000 } };
  const account = async owner => (await findAssociatedTokenPda({ mint: asset, owner, tokenProgram: TOKEN_PROGRAM_ADDRESS }))[0];
  const merchantAccount = await account(merchant.address);
  /** A payment transaction, signed by its authority, paying `amount` from `source`. */
  async function payload({ amount = 5_000_000n, resourceUrl = RESEARCH, authority = payer, source } = {}) {
    const from = source ?? await account(authority.address);
    const message = pipe(createTransactionMessage({ version: 0 }),
      m => setTransactionMessageFeePayer(feePayer.address, m),
      m => setTransactionMessageLifetimeUsingBlockhash({ blockhash: BLOCKHASH, lastValidBlockHeight: 1000n }, m),
      m => appendTransactionMessageInstructions([
        getSetComputeUnitLimitInstruction({ units: 20_000 }),
        getSetComputeUnitPriceInstruction({ microLamports: 1n }),
        getTransferCheckedInstruction({ source: from, mint: asset, destination: merchantAccount,
          authority, amount, decimals: 6 }),
        { programAddress: 'MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr', accounts: [], data: new TextEncoder().encode(terms.extra.memo) },
      ], m));
    const signed = await partiallySignTransactionMessageWithSigners(message);
    return { x402Version: 2, resource: { url: resourceUrl }, accepted: structuredClone(terms), payload: { transaction: getBase64EncodedWireTransaction(signed) } };
  }
  const signer = toFacilitatorSvmSigner(feePayer);
  signer.simulateTransaction = async () => {};
  // Every answer carries the slot of the node that gave it, as a real RPC does.
  const state = { statuses: {}, landed: {}, accounts: {}, history: {}, sends: [], height: 100, finalizedSlot: 900, statusSlot: 1000,
    valid: { processed: true, finalized: true }, validSlot: { processed: 1000, finalized: 900 }, clock: 1000 };
  const rpc = async (method, params) => {
    switch (method) {
      case 'getGenesisHash': return RAILS[NETWORK].genesis;
      case 'getSignatureStatuses': return { context: { slot: state.statusSlot }, value: [state.statuses[params[0][0]] ?? null] };
      case 'getTransaction': return state.landed[params[0]] ?? null;
      case 'getAccountInfo': return { context: { slot: state.statusSlot }, value: state.accounts[params[0]] ?? null };
      case 'getSignaturesForAddress': return state.history[params[0]] ?? [];
      case 'getMinimumBalanceForRentExemption': return 897_840;
      case 'getEpochInfo': return { absoluteSlot: state.finalizedSlot, blockHeight: state.height };
      case 'getLatestBlockhash': return { context: { slot: 1000 }, value: { blockhash: BLOCKHASH, lastValidBlockHeight: 2000 } };
      case 'isBlockhashValid': return { context: { slot: state.validSlot[params[1].commitment] }, value: state.valid[params[1].commitment] };
      case 'sendTransaction': state.sends.push(params[0]); if (state.refuseSend) throw new Error('refused'); return 'sent';
      default: throw new Error(`unexpected RPC ${method}`);
    }
  };
  const logs = [];
  const open = (changes = {}) => createBridge({ signer, config: { ...config, ...changes }, rpc, now: () => state.clock,
    refundSigners: { feePayer, merchant }, log: message => logs.push(message) });
  const bridge = open();
  const verify = async (overrides = {}) => bridge.verify({ paymentPayload: await payload(overrides), paymentRequirements: terms, resourceUrl: overrides.resourceUrl ?? RESEARCH });
  /** Marks a verified payment as landed and finalized, exactly as stored. */
  const land = payment => {
    const { evidence } = JSON.parse(payment.encoded_transaction);
    state.statuses[payment.transaction_id] = { confirmationStatus: 'finalized', err: null };
    state.landed[payment.transaction_id] = { meta: { err: null }, transaction: [evidence.transaction, 'base64'] };
  };
  return { bridge, open, verify, payload, land, terms, state, payer, feePayer, merchant, asset, account, merchantAccount, logs };
}

async function paid(f) {
  const payment = await f.verify();
  f.land(payment);
  return payment;
}

function decodeRefund(encoded) {
  const { evidence } = JSON.parse(encoded);
  const tx = getTransactionDecoder().decode(Buffer.from(evidence.transaction, 'base64'));
  return { evidence, tx, message: getCompiledTransactionMessageDecoder().decode(tx.messageBytes) };
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
  const repriced = f.open({ jobPrices: { [RESEARCH]: '4000000' } });
  await assert.rejects(repriced.verify({ paymentPayload: await f.payload(), paymentRequirements: f.terms, resourceUrl: RESEARCH }), Rejected);
});

test('job prices name known job kinds on https URLs with positive amounts', () => {
  assert.equal(jobPriceTable({ [RESEARCH]: '5000000' }).get(RESEARCH), '5000000');
  // An empty table sells nothing new; open jobs still settle and refund.
  assert.equal(jobPriceTable({}).size, 0);
  for (const prices of [{ 'https://example.com/v1/jobs/other': '1' }, { 'http://example.com/v1/jobs/research': '1' },
    { [RESEARCH]: '0' }, { [RESEARCH]: 5000000 }, { [`${RESEARCH}?x=1`]: '1' }, null, []]) {
    assert.throws(() => jobPriceTable(prices), /invalid job prices/);
  }
});

test('a jobs rail needs the merchant and fee payer keys it refunds with', async () => {
  const f = await setup();
  const stranger = await generateKeyPairSigner();
  assert.throws(() => createBridge({ signer: toFacilitatorSvmSigner(f.feePayer), config: { network: NETWORK, asset: f.asset, payTo: f.merchant.address,
    feePayer: f.feePayer.address, resourceUrl: 'https://example.com/v1/chat/completions', secret: SECRET, jobPrices: { [RESEARCH]: '1' } },
    rpc: async () => {}, refundSigners: { feePayer: f.feePayer, merchant: stranger } }), /merchant and fee payer keys/);
});

test("a job is paid only from the payer's own USDC account, never by a delegate", async () => {
  const f = await setup();
  const owner = await generateKeyPairSigner();
  // The payer signs as a delegate on someone else's account.
  await assert.rejects(f.verify({ source: await f.account(owner.address) }), /own USDC account/);
});

test('a refund is built only for a payment that landed as stored, back to the account it came from', async () => {
  const f = await setup();
  const payment = await f.verify();
  const refund = () => f.bridge.refund({ payment, paymentRequirements: f.terms });
  await assert.rejects(refund(), /only a finalized payment/);
  f.state.statuses[payment.transaction_id] = { confirmationStatus: 'finalized', err: null };
  await assert.rejects(refund(), /readback/);
  f.land(payment);
  f.state.landed[payment.transaction_id].transaction[0] = (await f.payload()).payload.transaction;
  await assert.rejects(refund(), /readback/);
  f.land(payment);
  const built = await refund();
  assert.equal(built.status, 'built');
  const { evidence, tx, message } = decodeRefund(built.encoded_refund);
  assert.equal(evidence.signature, built.transaction_id);
  assert.equal(evidence.paymentTransactionId, payment.transaction_id);
  assertIsFullySignedTransaction(tx);
  const keys = message.staticAccounts;
  assert.equal(keys[0], f.feePayer.address);
  const programs = message.instructions.map(ix => keys[ix.programAddressIndex]);
  // No account is created for the payer: the refund goes where the money came from.
  assert.equal(programs.includes(ASSOCIATED_TOKEN_PROGRAM), false);
  const transfer = message.instructions.find(ix => keys[ix.programAddressIndex] === TOKEN_PROGRAM_ADDRESS);
  const [from, , to] = transfer.accountIndices.map(i => keys[i]);
  assert.equal(from, f.merchantAccount);
  assert.equal(to, await f.account(f.payer.address));
  assert.equal(Buffer.from(transfer.data).readBigUInt64LE(1), 5_000_000n);
  assert.ok(keys.includes(await refundMarker(f.merchant.address, payment.transaction_id)));
  assert.ok(Buffer.from(tx.messageBytes).includes(Buffer.from(refundMemo(f.terms.extra.quoteId))));
  assert.deepEqual(f.state.sends, [], 'building a refund broadcasts nothing');
});

test('a sealed record that was not a real payment cannot be refunded, even with the seal key', async () => {
  const f = await setup();
  // Someone holding the seal key seals their own landed transfer of one
  // base unit as if it were a payment of the largest amount.
  const attacker = await generateKeyPairSigner();
  const attackerAccount = await f.account(attacker.address);
  const own = pipe(createTransactionMessage({ version: 0 }),
    m => setTransactionMessageFeePayerSigner(attacker, m),
    m => setTransactionMessageLifetimeUsingBlockhash({ blockhash: BLOCKHASH, lastValidBlockHeight: 1000n }, m),
    m => appendTransactionMessageInstructions([getTransferCheckedInstruction({ source: attackerAccount, mint: f.asset,
      destination: f.merchantAccount, authority: attacker, amount: 1n, decimals: 6 })], m));
  const landed = await signTransactionMessageWithSigners(own);
  const wire = getBase64EncodedWireTransaction(landed);
  const signature = getSignatureFromTransaction(landed);
  const terms = { ...f.terms, amount: '18446744073709551615' };
  const evidence = { transaction: wire, authorizationId: 'x', transactionId: signature, payer: attacker.address, terms };
  const seal = createHmac('sha256', SECRET).update(canonical(evidence)).digest('hex');
  const payment = { payer: attacker.address, authorization_id: 'x', transaction_id: signature, encoded_transaction: JSON.stringify({ evidence, seal }) };
  f.state.statuses[signature] = { confirmationStatus: 'finalized', err: null };
  f.state.landed[signature] = { meta: { err: null }, transaction: [wire, 'base64'] };
  await assert.rejects(f.bridge.refund({ payment, paymentRequirements: terms }), /readback/);
});

test('a payment whose marker is already claimed reports that refund instead of building another', async () => {
  const f = await setup();
  const payment = await paid(f);
  const marker = await refundMarker(f.merchant.address, payment.transaction_id);
  // Lamports sent to the marker by anyone do not count as a refund.
  f.state.accounts[marker] = { owner: '11111111111111111111111111111111', lamports: 5000, data: ['', 'base64'] };
  const earlier = await f.bridge.refund({ payment, paymentRequirements: f.terms });
  assert.equal(earlier.status, 'built');
  const { evidence } = JSON.parse(earlier.encoded_refund);
  // The earlier refund landed and claimed the marker. A transaction someone
  // else sent to the marker before it, from their own fee payer, is skipped.
  const stranger = await generateKeyPairSigner();
  const planted = await signTransactionMessageWithSigners(pipe(createTransactionMessage({ version: 0 }),
    m => setTransactionMessageFeePayerSigner(stranger, m),
    m => setTransactionMessageLifetimeUsingBlockhash({ blockhash: BLOCKHASH, lastValidBlockHeight: 1000n }, m),
    m => appendTransactionMessageInstructions([{ programAddress: 'MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr',
      accounts: [{ address: marker, role: 0 }], data: new TextEncoder().encode(refundMemo(f.terms.extra.quoteId)) }], m)));
  const plantedId = getSignatureFromTransaction(planted);
  f.state.accounts[marker] = { owner: MEMO_PROGRAM, lamports: 897_840, data: ['AA==', 'base64'] };
  f.state.history[marker] = [{ signature: evidence.signature, err: null }, { signature: plantedId, err: null }];
  f.state.landed[evidence.signature] = { meta: { err: null }, transaction: [evidence.transaction, 'base64'] };
  f.state.landed[plantedId] = { meta: { err: null }, transaction: [getBase64EncodedWireTransaction(planted), 'base64'] };
  assert.deepEqual(await f.bridge.refund({ payment, paymentRequirements: f.terms }), { status: 'refunded', transaction_id: evidence.signature });
  // Without readable history the refund is still known to have happened.
  f.state.history[marker] = [{ signature: 'unreadable', err: null }];
  assert.deepEqual(await f.bridge.refund({ payment, paymentRequirements: f.terms }), { status: 'refunded', transaction_id: 'unrecorded' });
});

test('a stored refund is broadcast as stored, and only finality or a conclusive expiry ends it', async () => {
  const f = await setup();
  const payment = await paid(f);
  const refund = await f.bridge.refund({ payment, paymentRequirements: f.terms });
  const status = () => f.bridge.refundStatus({ payment, paymentRequirements: f.terms, refund: refund.encoded_refund });
  const stored = JSON.parse(refund.encoded_refund).evidence.transaction;
  assert.deepEqual(await status(), { status: 'pending' });
  assert.deepEqual(f.state.sends, [stored]);
  f.state.refuseSend = true;
  assert.deepEqual(await status(), { status: 'pending' });
  assert.match(f.logs.at(-1), /stays owed/);
  f.state.statuses[refund.transaction_id] = { confirmationStatus: 'confirmed', err: null };
  assert.deepEqual(await status(), { status: 'pending' });
  f.state.statuses[refund.transaction_id] = { confirmationStatus: 'finalized', err: null };
  assert.deepEqual(await status(), { status: 'finalized', transaction_id: refund.transaction_id });
  f.state.statuses[refund.transaction_id] = { confirmationStatus: 'finalized', err: { InstructionError: [3, 'Custom'] } };
  assert.deepEqual(await status(), { status: 'failed' });
  delete f.state.statuses[refund.transaction_id];
  f.state.height = 2001;
  // A status read from a node behind finality proves nothing.
  f.state.statusSlot = f.state.finalizedSlot - 1;
  assert.deepEqual(await status(), { status: 'pending' });
  f.state.statusSlot = f.state.finalizedSlot;
  assert.deepEqual(await status(), { status: 'expired' });
  assert.equal(new Set(f.state.sends).size, 1);
});

test('a refund that failed because another one landed reports the one that landed', async () => {
  const f = await setup();
  const payment = await paid(f);
  const first = await f.bridge.refund({ payment, paymentRequirements: f.terms });
  const second = await f.bridge.refund({ payment, paymentRequirements: f.terms });
  const marker = await refundMarker(f.merchant.address, payment.transaction_id);
  const landed = JSON.parse(first.encoded_refund).evidence;
  f.state.statuses[second.transaction_id] = { confirmationStatus: 'finalized', err: { InstructionError: [3, 'AccountAlreadyInUse'] } };
  f.state.accounts[marker] = { owner: MEMO_PROGRAM, lamports: 897_840, data: ['AA==', 'base64'] };
  f.state.history[marker] = [{ signature: second.transaction_id, err: {} }, { signature: landed.signature, err: null }];
  f.state.landed[landed.signature] = { meta: { err: null }, transaction: [landed.transaction, 'base64'] };
  assert.deepEqual(await f.bridge.refundStatus({ payment, paymentRequirements: f.terms, refund: second.encoded_refund }),
    { status: 'finalized', transaction_id: landed.signature });
});

test('a payment that lands with another signature from its payer is still the same payment', async () => {
  const f = await setup();
  const payment = await f.verify();
  const { evidence } = JSON.parse(payment.encoded_transaction);
  // Same message and fee payer signature; the payer's signature differs.
  const tx = getTransactionDecoder().decode(Buffer.from(evidence.transaction, 'base64'));
  const signatures = { ...tx.signatures, [f.payer.address]: new Uint8Array(64).fill(7) };
  const variant = getBase64EncodedWireTransaction({ ...tx, signatures });
  assert.notEqual(variant, evidence.transaction);
  f.state.statuses[payment.transaction_id] = { confirmationStatus: 'finalized', err: null };
  f.state.landed[payment.transaction_id] = { meta: { err: null }, transaction: [variant, 'base64'] };
  assert.deepEqual(await f.bridge.settle({ payment, paymentRequirements: f.terms }), { status: 'finalized', transaction_id: payment.transaction_id });
  assert.equal((await f.bridge.refund({ payment, paymentRequirements: f.terms })).status, 'built');
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
    feePayer: f.feePayer.address, resourceUrl: 'https://example.com/v1/chat/completions', secret: SECRET, priceAtomic: '1000' }, rpc: async () => {} });
  assert.equal(inference.refund, undefined);
  assert.equal(inference.refundStatus, undefined);
});

test('a job payment ends unpaid only when every read agrees it can no longer land', async () => {
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
  // Reads from nodes behind the finalized one prove nothing.
  f.state.validSlot.processed = f.state.validSlot.finalized - 1;
  assert.deepEqual(await settle(), { status: 'pending' });
  f.state.validSlot.processed = 1000;
  f.state.statusSlot = f.state.validSlot.finalized - 1;
  assert.deepEqual(await settle(), { status: 'pending' });
  f.state.statusSlot = 1000;
  assert.deepEqual(await settle(), { status: 'failed' });
});
