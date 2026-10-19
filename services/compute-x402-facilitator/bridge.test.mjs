import test from 'node:test';
import assert from 'node:assert/strict';
import { generateKeyPairSigner, pipe, createTransactionMessage, setTransactionMessageFeePayer, setTransactionMessageLifetimeUsingBlockhash, appendTransactionMessageInstructions, partiallySignTransactionMessageWithSigners, getBase64EncodedWireTransaction } from '@solana/kit';
import { findAssociatedTokenPda, TOKEN_PROGRAM_ADDRESS, getTransferCheckedInstruction } from '@solana-program/token';
import { getSetComputeUnitLimitInstruction, getSetComputeUnitPriceInstruction } from '@solana-program/compute-budget';
import { toFacilitatorSvmSigner } from '@x402/svm';
import { createBridge, RAILS, Rejected } from './bridge.mjs';
import { createPricingPolicy, readTariffConfig } from './pricing.mjs';
import { mkdtemp, writeFile, symlink, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

async function setup(options = {}) {
  const payer = await generateKeyPairSigner();
  const facilitator = await generateKeyPairSigner();
  const merchant = await generateKeyPairSigner();
  const network = 'solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1';
  const config = { network, asset: RAILS[network].asset, payTo: merchant.address, feePayer: facilitator.address, resourceUrl: 'https://example.com/v1/chat/completions', priceAtomic: '1200', secret: 'test-secret-with-at-least-32-characters', ...options.config };
  const quoteId = '00000000-0000-4000-8000-000000000001';
  const requestSha256 = 'a'.repeat(64);
  const terms = { scheme: 'exact', network, asset: config.asset, payTo: config.payTo, amount: options.amount ?? '1200', maxTimeoutSeconds: 90, extra: { feePayer: config.feePayer, memo: `covenant.x402.v2:${quoteId}:${requestSha256}`, quoteId, requestSha256, expiresAtMs: 90_000 } };
  const source = (await findAssociatedTokenPda({ mint: config.asset, owner: payer.address, tokenProgram: TOKEN_PROGRAM_ADDRESS }))[0];
  const destination = (await findAssociatedTokenPda({ mint: config.asset, owner: merchant.address, tokenProgram: TOKEN_PROGRAM_ADDRESS }))[0];
  async function payload(overrides = {}) {
    const tx = pipe(createTransactionMessage({ version: 0 }),
      m => setTransactionMessageFeePayer(facilitator.address, m),
      m => setTransactionMessageLifetimeUsingBlockhash({ blockhash: '11111111111111111111111111111111', lastValidBlockHeight: 1000n }, m),
      m => appendTransactionMessageInstructions([
        getSetComputeUnitLimitInstruction({ units: overrides.units ?? 60_000 }),
        getSetComputeUnitPriceInstruction({ microLamports: overrides.priority ?? 1n }),
        getTransferCheckedInstruction({ source, mint: config.asset, destination, authority: payer, amount: overrides.amount ?? BigInt(terms.amount), decimals: 6 }),
        { programAddress: 'MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr', accounts: [], data: new TextEncoder().encode(overrides.memo ?? terms.extra.memo) },
      ], m));
    const signed = await partiallySignTransactionMessageWithSigners(tx);
    return { x402Version: 2, resource: { url: config.resourceUrl }, accepted: structuredClone(terms), payload: { transaction: getBase64EncodedWireTransaction(signed) } };
  }
  const signer = toFacilitatorSvmSigner(facilitator);
  let simulations = 0; signer.simulateTransaction = async () => { simulations++; };
  let signatures = 0; const sign = signer.signTransaction.bind(signer);
  signer.signTransaction = async (...args) => { signatures++; return sign(...args); };
  const state = { sends: [], status: null, readback: null, loseResponse: false };
  const rpc = async (method, params) => {
    if (method === 'getGenesisHash') return RAILS[network].genesis;
    if (method === 'getSignatureStatuses') return { value: [state.status] };
    if (method === 'getTransaction') return state.readback;
    if (method === 'sendTransaction') { state.sends.push(params[0]); if (state.loseResponse) throw new Error('lost RPC response'); return state.signature; }
    throw new Error(`unexpected RPC ${method}`);
  };
  const reopen = (changes = {}, clock = 1000) => createBridge({ signer, config: { ...config, ...changes }, rpc, now: () => clock });
  const bridge = reopen();
  const verify = async (overrides = {}) => bridge.verify({ paymentPayload: await payload(overrides), paymentRequirements: terms, resourceUrl: config.resourceUrl });
  return { bridge, verify, terms, state, payload, config, reopen, simulations: () => simulations, signatures: () => signatures };
}

function tariffs() {
  const native = { schema_version: 1, id: 'covenant-haiku-4.5-native', revision: 1, model: 'claude-haiku-4.5', base_micro_usdc: 10000,
    output_cap_micro_usdc_per_million: 7500000, max_output_tokens: 2048, valid_from_ms: 1000, valid_until_ms: 200000 };
  return [{ native, retail: { ...native, id: 'covenant-haiku-4.5-retail', base_micro_usdc: 12000, output_cap_micro_usdc_per_million: 9000000 } }];
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

test('independently configured tariffs admit both requested-cap prices and the legacy flat price', async () => {
  for (const amount of ['14304', '30432', '50000']) {
    const f = await setup({ amount, config: { priceAtomic: '50000', tariffRows: tariffs() } });
    const payment = await f.verify();
    assert.ok(payment.transaction_id);
    assert.equal(f.signatures(), 1);
    assert.equal(f.simulations(), 1);
    assert.equal(f.state.sends.length, 0, 'verification cannot broadcast');
    for (const override of [{ amount: BigInt(amount) + 1n }, { memo: 'another request' }, { units: 60_001 }, { priority: 1001n }]) {
      await assert.rejects(f.verify(override), Rejected);
    }
    assert.equal(f.signatures(), 1, 'changed transaction cannot reach sponsor signing');
    const changed = await f.payload(); changed.accepted.amount = amount === '14304' ? '30432' : '14304';
    await assert.rejects(f.bridge.verify({ paymentPayload: changed, paymentRequirements: f.terms, resourceUrl: f.config.resourceUrl }), Rejected);
    assert.equal(f.signatures(), 1, 'another configured price cannot replace exact accepted terms');
  }
});

test('nonmember amounts are refused before official simulation or sponsor signing', async () => {
  for (const amount of ['1', '12000', '12010', '14305', '30441', '50001', '014304']) {
    const f = await setup({ amount, config: { priceAtomic: '50000', tariffRows: tariffs() } });
    await assert.rejects(f.verify(), Rejected);
    assert.equal(f.simulations(), 0); assert.equal(f.signatures(), 0); assert.equal(f.state.sends.length, 0);
  }
  const legacy = await setup({ amount: '14304', config: { priceAtomic: '50000' } });
  await assert.rejects(legacy.verify(), Rejected);
  assert.equal(legacy.signatures(), 0, 'omitting tariffs preserves flat-only admission');
});

test('tariff price membership handles rounding, plateaus, zero rates and exclusive expiry', () => {
  const rows = tariffs();
  rows[0].native.base_micro_usdc = 1; rows[0].native.output_cap_micro_usdc_per_million = 0;
  rows[0].retail.base_micro_usdc = 100;
  for (const rate of [0, 1, 500000, 1000001, 1500000, 9000000]) {
    rows[0].retail.output_cap_micro_usdc_per_million = rate;
    const policy = createPricingPolicy({ priceAtomic: '50000', tariffRows: rows });
    const prices = new Set(Array.from({ length: 2048 }, (_, i) => String(100n + (BigInt(i + 1) * BigInt(rate) + 999999n) / 1000000n)));
    for (let amount = 99; amount <= 100 + Math.ceil(2048 * rate / 1000000) + 1; amount++) {
      assert.equal(policy.allows(String(amount), 1000), prices.has(String(amount)), `rate ${rate}, amount ${amount}`);
    }
    const amount = [...prices][0];
    assert.equal(policy.allows(amount, 999), false);
    assert.equal(policy.allows(amount, 199999), true);
    assert.equal(policy.allows(amount, 200000), false);
    assert.equal(policy.allows('50000', 200000), true, 'tariff expiry does not remove legacy service');
  }
});

test('malformed tariff or legacy policy closes all new authorizations', async () => {
  const cases = [
    { tariffRows: [] }, { tariffRows: null }, { tariffConfigError: true }, { priceAtomic: '0' }, { priceAtomic: 50000 },
    { priceAtomic: '9007199254740992' }, { tariffRows: [tariffs()[0], tariffs()[0]] },
  ];
  for (const change of [
    row => { row.extra = true; }, row => { row.retail.unknown = 1; }, row => { row.native.model = 'other'; },
    row => { row.retail.max_output_tokens++; }, row => { row.native.valid_from_ms++; }, row => { row.retail.valid_until_ms = 1000; },
    row => { row.retail.revision = 0; }, row => { row.retail.base_micro_usdc = Number.MAX_SAFE_INTEGER; },
    row => { row.retail.output_cap_micro_usdc_per_million = 0.5; }, row => { row.retail.base_micro_usdc = 0; row.retail.output_cap_micro_usdc_per_million = 0; },
    row => { row.retail.base_micro_usdc = 1; }, row => { row.native.max_output_tokens = row.retail.max_output_tokens = 32769; },
  ]) { const row = tariffs()[0]; change(row); cases.push({ tariffRows: [row] }); }
  for (const change of cases) {
    const f = await setup({ amount: '50000', config: { priceAtomic: '50000', tariffRows: tariffs(), ...change } });
    await assert.rejects(f.verify(), Rejected);
    assert.equal(f.simulations(), 0); assert.equal(f.signatures(), 0); assert.equal(f.state.sends.length, 0);
  }
});

test('sealed tariff settlement survives expiry, rotation and malformed or missing configuration', async () => {
  const f = await setup({ amount: '14304', config: { priceAtomic: '50000', tariffRows: tariffs() } });
  const payment = await f.verify();
  const transaction = JSON.parse(payment.encoded_transaction).evidence.transaction;
  f.state.status = { confirmationStatus: 'finalized', err: null };
  f.state.readback = { meta: { err: null }, transaction: [transaction, 'base64'] };
  const rotated = tariffs(); rotated[0].retail.base_micro_usdc++;
  const missing = await readTariffConfig('/does-not-exist/covenant-tariffs.json');
  for (const [change, clock] of [[{}, 200000], [{ tariffRows: rotated }, 1000], [{ tariffRows: undefined }, 1000], [{ tariffRows: [] }, 1000], [missing, 1000], [{ priceAtomic: 'bad' }, 1000]]) {
    const bridge = f.reopen(change, clock);
    await assert.rejects(bridge.verify({ paymentPayload: await f.payload(), paymentRequirements: f.terms, resourceUrl: f.config.resourceUrl }), Rejected);
    assert.deepEqual(await bridge.settle({ payment: JSON.parse(JSON.stringify(payment)), paymentRequirements: f.terms }), { status: 'finalized', transaction_id: payment.transaction_id });
    await assert.rejects(bridge.settle({ payment, paymentRequirements: { ...f.terms, amount: '30432' } }), Rejected);
  }
  assert.equal(f.signatures(), 1); assert.equal(f.state.sends.length, 0);
});

test('legacy sealed settlement still recovers after the flat price changes', async () => {
  const f = await setup(); const payment = await f.verify();
  f.state.status = { confirmationStatus: 'finalized', err: null };
  f.state.readback = { meta: { err: null }, transaction: [JSON.parse(payment.encoded_transaction).evidence.transaction, 'base64'] };
  assert.equal((await f.reopen({ priceAtomic: '50000' }).settle({ payment, paymentRequirements: f.terms })).status, 'finalized');
});

test('server tariff file loader uses the configured regular file and closes fresh admission on bad input', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'covenant-bridge-pricing-'));
  try {
    const file = join(directory, 'tariffs.json'); await writeFile(file, JSON.stringify(tariffs()));
    const config = await readTariffConfig(file);
    assert.equal(createPricingPolicy({ priceAtomic: '50000', ...config }).allows('14304', 1000), true);
    assert.deepEqual(await readTariffConfig(undefined), {});
    await symlink(file, join(directory, 'link.json'));
    for (const invalid of ['', 'relative.json', directory, join(directory, 'missing'), join(directory, 'link.json')]) {
      assert.deepEqual(await readTariffConfig(invalid), { tariffConfigError: true });
    }
    await writeFile(file, '{broken'); assert.deepEqual(await readTariffConfig(file), { tariffConfigError: true });
    await writeFile(file, ' '.repeat(262145)); assert.deepEqual(await readTariffConfig(file), { tariffConfigError: true });
  } finally { await rm(directory, { recursive: true, force: true }); }
});
