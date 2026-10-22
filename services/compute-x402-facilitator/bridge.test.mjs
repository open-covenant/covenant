import test from 'node:test';
import assert from 'node:assert/strict';
import { getTransactionDecoder, getCompiledTransactionMessageDecoder, getSignatureFromTransaction, generateKeyPairSigner, getBase58Decoder, pipe, createTransactionMessage, setTransactionMessageFeePayer, setTransactionMessageLifetimeUsingBlockhash, appendTransactionMessageInstructions, partiallySignTransactionMessageWithSigners, getBase64EncodedWireTransaction } from '@solana/kit';
import { findAssociatedTokenPda, TOKEN_PROGRAM_ADDRESS, getTransferCheckedInstruction } from '@solana-program/token';
import { getSetComputeUnitLimitInstruction, getSetComputeUnitPriceInstruction } from '@solana-program/compute-budget';
import { toFacilitatorSvmSigner } from '@x402/svm';
import { x402Client } from '@x402/core/client';
import { ExactSvmScheme as OfficialClientScheme } from '@x402/svm/exact/client';
import { createBridge, FACILITATOR_GRACE_MS, RAILS, Rejected, shortMemo } from './bridge.mjs';
import { batchPricingTotal, createPricingPolicy, readTariffConfig } from './pricing.mjs';
import { mkdtemp, writeFile, symlink, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createServer } from 'node:http';
import { createHash, generateKeyPairSync } from 'node:crypto';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

async function setup(options = {}) {
  const payer = await generateKeyPairSigner();
  const facilitator = await generateKeyPairSigner();
  const merchant = await generateKeyPairSigner();
  const network = 'solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1';
  const config = { network, asset: RAILS[network].asset, payTo: merchant.address, feePayer: facilitator.address, resourceUrl: 'https://example.com/v1/chat/completions', priceAtomic: '1200', secret: 'test-secret-with-at-least-32-characters', ...options.config };
  const quoteId = '00000000-0000-4000-8000-000000000001';
  const requestSha256 = 'a'.repeat(64);
  const terms = { scheme: 'exact', network, asset: config.asset, payTo: config.payTo, amount: options.amount ?? '1200', maxTimeoutSeconds: 90, extra: { feePayer: config.feePayer, memo: options.legacyMemo ? `covenant.x402.v2:${quoteId}:${requestSha256}` : shortMemo(quoteId, requestSha256), quoteId, requestSha256, expiresAtMs: 90_000 } };
  if (options.batchPricing !== undefined) terms.extra.batchPricing = options.batchPricing;
  const resourceUrl = options.batchPricing === undefined ? config.resourceUrl : new URL('/v1/inference/batches', config.resourceUrl).href;
  const source = (await findAssociatedTokenPda({ mint: config.asset, owner: payer.address, tokenProgram: TOKEN_PROGRAM_ADDRESS }))[0];
  const destination = (await findAssociatedTokenPda({ mint: config.asset, owner: merchant.address, tokenProgram: TOKEN_PROGRAM_ADDRESS }))[0];
  async function payload(overrides = {}) {
    const tx = pipe(createTransactionMessage({ version: 0 }),
      m => setTransactionMessageFeePayer(facilitator.address, m),
      m => setTransactionMessageLifetimeUsingBlockhash({ blockhash: '11111111111111111111111111111111', lastValidBlockHeight: 1000n }, m),
      m => appendTransactionMessageInstructions([
        getSetComputeUnitLimitInstruction({ units: overrides.units ?? 20_000 }),
        getSetComputeUnitPriceInstruction({ microLamports: overrides.priority ?? 1n }),
        getTransferCheckedInstruction({ source, mint: config.asset, destination, authority: payer, amount: overrides.amount ?? BigInt(terms.amount), decimals: 6 }),
        { programAddress: 'MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr', accounts: [], data: new TextEncoder().encode(overrides.memo ?? terms.extra.memo) },
      ], m));
    const signed = await partiallySignTransactionMessageWithSigners(tx);
    return { x402Version: 2, resource: { url: overrides.resourceUrl ?? resourceUrl }, accepted: structuredClone(terms), payload: { transaction: getBase64EncodedWireTransaction(signed) } };
  }
  const signer = toFacilitatorSvmSigner(facilitator);
  let simulations = 0; signer.simulateTransaction = async () => { simulations++; };
  let signatures = 0; const sign = signer.signTransaction.bind(signer);
  signer.signTransaction = async (...args) => { signatures++; return sign(...args); };
  const state = { sends: [], rpcCalls: [], status: null, readback: null, loseResponse: false };
  const rpc = async (method, params) => {
    state.rpcCalls.push(method);
    if (method === 'getGenesisHash') return RAILS[network].genesis;
    if (method === 'getSignatureStatuses') return { value: [state.status] };
    if (method === 'getTransaction') return state.readback;
    if (method === 'sendTransaction') { state.sends.push(params[0]); if (state.loseResponse) throw new Error('lost RPC response'); return state.signature; }
    throw new Error(`unexpected RPC ${method}`);
  };
  const reopen = (changes = {}, clock = 1000) => createBridge({ signer, config: { ...config, ...changes }, rpc, now: () => clock, facilitator: options.facilitator });
  const bridge = reopen();
  const verify = async (overrides = {}) => bridge.verify({ paymentPayload: await payload(overrides), paymentRequirements: terms, resourceUrl });
  return { bridge, verify, terms, state, payload, config, reopen, resourceUrl, payer, feePayerKey: facilitator, simulations: () => simulations, signatures: () => signatures };
}

function tariffs() {
  const native = { schema_version: 1, id: 'covenant-haiku-4.5-native', revision: 1, model: 'claude-haiku-4.5', base_micro_usdc: 10000,
    output_cap_micro_usdc_per_million: 7500000, max_output_tokens: 2048, valid_from_ms: 1000, valid_until_ms: 200000 };
  return [{ native, retail: { ...native, id: 'covenant-haiku-4.5-retail', base_micro_usdc: 12000, output_cap_micro_usdc_per_million: 9000000 } }];
}

function batchManifest() {
  return { version: 1, batch_id: '00000000-0000-4000-8000-000000000002', children: [256, 2048].map((cap, i) => ({
    job_id: `00000000-0000-4000-8000-00000000000${i + 3}`, model: 'claude-haiku-4.5', max_output_tokens: cap,
    pricing: 'model_tariff_v1', amount_atomic: 12000 + cap * 9,
  })) };
}

test('batch policy authorizes exact ordered child prices, with opt-in and bounded identities', () => {
  const policy = createPricingPolicy({ priceAtomic: '50000', tariffRows: tariffs(), batchEnabled: true });
  const manifest = batchManifest();
  assert.equal(batchPricingTotal(manifest), 44736n);
  assert.equal(policy.allowsBatch(manifest, '44736', 1000), true);
  assert.equal(policy.allowsBatch(manifest, '44737', 1000), false);
  assert.equal(policy.allowsBatch(manifest, '44736', 200000), false);
  assert.equal(createPricingPolicy({ priceAtomic: '50000', tariffRows: tariffs() }).allowsBatch(manifest, '44736', 1000), false);
  for (const mutate of [
    m => { m.version = 2; }, m => { m.batch_id = 'bad'; }, m => { m.batch_id = '00000000-0000-0000-0000-000000000000'; },
    m => { m.children = []; }, m => { m.children = Array(33).fill(m.children[0]); },
    m => { m.children[1].job_id = m.children[0].job_id; }, m => { m.children[0].job_id = 'bad'; },
    m => { m.children[0].max_output_tokens = 0; }, m => { m.children[0].max_output_tokens = 32769; },
    m => { m.children[0].max_output_tokens = 257; }, m => { m.children[0].model = 'other'; },
    m => { m.children[0].model = 'any'; }, m => { m.children[0].amount_atomic++; m.children[1].amount_atomic--; },
    m => { m.children[0].amount_atomic = Number.MAX_SAFE_INTEGER; }, m => { m.children[0].pricing = 'unknown'; },
    m => { m.children[0].extra = true; }, m => { m.extra = true; },
  ]) {
    const changed = structuredClone(manifest); mutate(changed);
    assert.equal(policy.allowsBatch(changed, '44736', 1000), false);
  }
  const legacy = structuredClone(manifest); delete legacy.children[0].pricing; legacy.children[0].amount_atomic = 50000;
  assert.equal(policy.allowsBatch(legacy, '80432', 1000), true);
  legacy.children[0].pricing = null;
  assert.equal(policy.allowsBatch(legacy, '80432', 1000), true);
  const three = batchManifest();
  three.children.push({ ...three.children[0], job_id: '00000000-0000-4000-8000-000000000099' });
  assert.equal(batchPricingTotal(three), 59040n, 'recovery format retains its protocol bound');
  assert.equal(policy.allowsBatch(three, '59040', 1000), false, 'first release admits at most two children');
});

test('one official SVM authorization covers the batch sum and survives disabled or rotated admission', async () => {
  const f = await setup({ config: { priceAtomic: '50000', tariffRows: tariffs(), batchEnabled: true }, batchPricing: batchManifest(), amount: '44736' });
  assert.equal(f.bridge.readiness().batchEnabled, true);
  await assert.rejects(f.verify({ resourceUrl: f.config.resourceUrl }), Rejected);
  assert.equal(f.signatures(), 0); assert.deepEqual(f.state.rpcCalls, []);
  const payment = await f.verify();
  assert.equal(f.signatures(), 1); assert.equal(f.simulations(), 1); assert.deepEqual(f.state.sends, []);
  f.state.signature = payment.transaction_id;
  for (const changes of [{ batchEnabled: false }, { priceAtomic: '60000', tariffRows: undefined }, { tariffConfigError: true }]) {
    const recovered = f.reopen(changes, 300000);
    await assert.rejects(recovered.verify({ paymentPayload: await f.payload(), paymentRequirements: f.terms, resourceUrl: f.resourceUrl }), Rejected);
    assert.equal((await recovered.settle({ payment, paymentRequirements: f.terms })).status, 'pending');
  }
  assert.equal(f.state.sends.length, 3); assert.equal(new Set(f.state.sends).size, 1);
  f.state.status = { confirmationStatus: 'finalized', err: null };
  f.state.readback = { meta: { err: null }, transaction: [f.state.sends[0], 'base64'] };
  assert.deepEqual(await f.reopen({ batchEnabled: false }, 300000).settle({ payment, paymentRequirements: f.terms }), { status: 'finalized', transaction_id: payment.transaction_id });
  const changed = structuredClone(f.terms); changed.extra.batchPricing.children.reverse();
  await assert.rejects(f.bridge.settle({ payment, paymentRequirements: changed }), Rejected);
  assert.equal(f.signatures(), 1, 'recovery never replaces the signed transaction');
});

test('disabled or malformed batches stop before RPC, simulation, signing and broadcast', async () => {
  for (const options of [
    { config: { batchEnabled: false }, batchPricing: batchManifest(), amount: '44736' },
    { batchPricing: null, amount: '44736' },
    { batchPricing: batchManifest(), amount: '44737' },
    { batchPricing: { ...batchManifest(), children: [] }, amount: '50000' },
  ]) {
    const f = await setup({ ...options, config: { priceAtomic: '50000', tariffRows: tariffs(), batchEnabled: true, ...options.config } });
    await assert.rejects(f.verify(), Rejected);
    assert.equal(f.signatures(), 0); assert.equal(f.simulations(), 0); assert.deepEqual(f.state.rpcCalls, []); assert.deepEqual(f.state.sends, []);
  }
  const single = await setup({ config: { batchEnabled: true } });
  await single.verify();
  const payload = await single.payload({ resourceUrl: new URL('/v1/inference/batches', single.config.resourceUrl).href });
  await assert.rejects(single.bridge.verify({ paymentPayload: payload, paymentRequirements: single.terms, resourceUrl: payload.resource.url }), Rejected);
});

test('readiness binds the loaded rail and complete pricing policy without payment activity', async () => {
  const f = await setup({ config: { priceAtomic: '50000', tariffRows: tariffs() } });
  const expected = { schemaVersion: 1, observedAtMs: 1000, pricingValid: true, scheme: 'exact',
    network: f.config.network, asset: f.config.asset, assetDecimals: 6, payTo: f.config.payTo,
    feePayer: f.config.feePayer, resourceUrl: f.config.resourceUrl, legacyPriceAtomic: '50000', retailTariffs: [tariffs()[0].retail], batchEnabled: false };
  assert.deepEqual(f.bridge.readiness(), expected);
  const changed = f.bridge.readiness(); changed.retailTariffs[0].base_micro_usdc++;
  f.config.tariffRows[0].retail.revision++;
  assert.deepEqual(f.bridge.readiness(), expected, 'mutable inputs or returned JSON cannot alter loaded policy');
  assert.equal(f.signatures(), 0); assert.equal(f.simulations(), 0);
  assert.deepEqual(f.state.rpcCalls, []); assert.deepEqual(f.state.sends, []);
});

test('readiness reports exact rotated policy and legacy-only policy, and refuses invalid configuration', async () => {
  const f = await setup({ config: { priceAtomic: '50000', tariffRows: tariffs() } });
  const rotated = tariffs(); rotated[0].retail.revision++;
  assert.deepEqual(f.reopen({ tariffRows: rotated }).readiness().retailTariffs, [rotated[0].retail]);
  assert.deepEqual(f.reopen({ tariffRows: undefined }).readiness().retailTariffs, []);
  assert.equal(f.reopen({ priceAtomic: '60000' }).readiness().legacyPriceAtomic, '60000');
  for (const config of [{ tariffRows: [] }, { tariffConfigError: true }, { priceAtomic: 'bad' }]) {
    assert.throws(() => f.reopen(config).readiness(), /payment pricing unavailable/);
  }
  assert.deepEqual(f.state.rpcCalls, []); assert.equal(f.signatures(), 0); assert.equal(f.simulations(), 0);
});

test('authenticated server readiness fails closed while malformed-policy recovery health stays ready', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'covenant-readiness-http-'));
  const network = 'solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1';
  const { privateKey, publicKey } = generateKeyPairSync('ed25519');
  const publicBytes = publicKey.export({ type: 'spki', format: 'der' }).subarray(-32);
  const bytes = Buffer.concat([privateKey.export({ type: 'pkcs8', format: 'der' }).subarray(-32), publicBytes]);
  const address = getBase58Decoder().decode(publicBytes), keyFile = join(directory, 'disposable-key.json');
  await writeFile(keyFile, JSON.stringify([...bytes]), { mode: 0o600 }); bytes.fill(0);
  const calls = [];
  const rpc = createServer(async (req, res) => {
    const chunks = []; for await (const chunk of req) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks)); calls.push(body.method);
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ jsonrpc: '2.0', id: body.id, result: RAILS[network].genesis }));
  });
  await new Promise(resolve => rpc.listen(0, '127.0.0.1', resolve));
  let child;
  try {
    for (const mode of ['valid', 'broken tariffs', 'Base unreachable']) {
      const valid = mode !== 'broken tariffs';
      const portProbe = createServer();
      await new Promise(resolve => portProbe.listen(0, '127.0.0.1', resolve));
      const port = portProbe.address().port;
      await new Promise(resolve => portProbe.close(resolve));
      const tariffsFile = join(directory, 'tariffs.json');
      await writeFile(tariffsFile, valid ? JSON.stringify(tariffs()) : '{broken');
      child = spawn(process.execPath, [fileURLToPath(new URL('./server.mjs', import.meta.url))], {
        env: { PATH: process.env.PATH, HOME: directory,
          COVENANT_GATEWAY_BRIDGE_SECRET: 'test-secret-with-at-least-32-characters',
          COVENANT_GATEWAY_SOLANA_RPC: `http://127.0.0.1:${rpc.address().port}`,
          COVENANT_GATEWAY_FEE_PAYER_KEY_FILE: keyFile, COVENANT_GATEWAY_NETWORK: network,
          COVENANT_GATEWAY_ASSET: RAILS[network].asset, COVENANT_GATEWAY_PAY_TO: address,
          COVENANT_GATEWAY_FEE_PAYER: address, COVENANT_GATEWAY_RESOURCE_URL: 'https://example.com/v1/chat/completions',
          COVENANT_GATEWAY_PRICE_MICRO_USDC: '50000', COVENANT_GATEWAY_TARIFFS_FILE: tariffsFile,
          COVENANT_GATEWAY_BRIDGE_PORT: String(port),
          // Nothing listens on port 9, so Base cannot start; Solana must still run.
          ...(mode === 'Base unreachable' ? { COVENANT_GATEWAY_BASE_PAY_TO: '0x5fA1d0C0bfFE257a20027C523093F941834f5D66',
            COVENANT_GATEWAY_BASE_NETWORK: 'eip155:84532', COVENANT_GATEWAY_BASE_ASSET: '0x036CbD53842c5426634e7929541eC2318f3dCF7e',
            COVENANT_GATEWAY_BASE_RPC: 'https://127.0.0.1:9', COVENANT_GATEWAY_BASE_FACILITATOR_URL: 'https://127.0.0.1:9/facilitator' } : {}) },
        stdio: ['ignore', 'pipe', 'pipe'],
      });
      child.stderr.resume();
      await new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error('test server startup timeout')), 5000);
        child.once('exit', () => { clearTimeout(timer); reject(new Error('test server exited before ready')); });
        child.once('error', error => { clearTimeout(timer); reject(error); });
        child.stdout.on('data', chunk => { if (String(chunk).includes('listening on loopback')) { clearTimeout(timer); resolve(); } });
      });
      const base = `http://127.0.0.1:${port}`, headers = { authorization: 'Bearer test-secret-with-at-least-32-characters' };
      assert.equal((await fetch(`${base}/readiness`)).status, 401);
      assert.equal((await fetch(`${base}/health`, { headers })).status, 200);
      const response = await fetch(`${base}/readiness`, { headers });
      assert.equal(response.headers.get('cache-control'), 'no-store');
      assert.equal(response.status, valid ? 200 : 503);
      const body = await response.json();
      if (valid) { assert.equal(body.legacyPriceAtomic, '50000'); assert.deepEqual(body.retailTariffs, [tariffs()[0].retail]); }
      else assert.deepEqual(body, { error: 'payment pricing unavailable' });
      for (const [size, expected] of [[70000, 'payment rail or amount mismatch'], [131073, 'request too large']]) {
        const response = await fetch(`${base}/verify`, { method: 'POST', headers, body: JSON.stringify({ padding: 'x'.repeat(size) }) });
        assert.equal(response.status, 400);
        assert.equal((await response.json()).error, expected);
      }
      const evm = await fetch(`${base}/evm/readiness`, { headers });
      if (mode === 'Base unreachable') assert.deepEqual([evm.status, await evm.json()], [503, { error: 'Base payments unavailable' }]);
      else assert.equal(evm.status, 404);
      const exited = new Promise(resolve => child.once('exit', resolve)); child.kill('SIGTERM'); await exited; child = undefined;
    }
    assert.deepEqual(calls, ['getGenesisHash', 'getGenesisHash', 'getGenesisHash'], 'only startup identity checks; readiness never uses Solana RPC');
  } finally {
    if (child && child.exitCode === null && child.signalCode === null) {
      const exited = new Promise(resolve => child.once('exit', resolve)); child.kill('SIGKILL'); await exited;
    }
    await new Promise(resolve => rpc.close(resolve)); await rm(directory, { recursive: true, force: true });
  }
});

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

test('short memo matches the published vector', () => {
  assert.equal(shortMemo('00000000-0000-4000-8000-000000000001', 'a'.repeat(64)), '8bfb7fde3cb99c6b0f16b7a18d83a0e1');
});

for (const batch of [false, true]) test(`verifier accepts a v3 ${batch ? 'batch' : 'single'} payment built by the stock x402 client at 20,000 CU`, async () => {
  const f = await setup(batch ? { config: { priceAtomic: '50000', tariffRows: tariffs(), batchEnabled: true }, batchPricing: batchManifest(), amount: '44736' } : {});
  assert.equal(f.terms.extra.memo.length, 32);
  // The stock client reads the mint and a blockhash; nothing else leaves the test.
  const mint = Buffer.alloc(82); mint[44] = 6; mint[45] = 1;
  const server = createServer((req, res) => {
    let body = ''; req.on('data', c => { body += c; }); req.on('end', () => {
      const { id, method } = JSON.parse(body);
      const result = method === 'getAccountInfo'
        ? { context: { slot: 1 }, value: { owner: TOKEN_PROGRAM_ADDRESS, data: [mint.toString('base64'), 'base64'], executable: false, lamports: 1, rentEpoch: 0, space: 82 } }
        : method === 'getLatestBlockhash' ? { context: { slot: 1 }, value: { blockhash: '11111111111111111111111111111111', lastValidBlockHeight: 1000 } } : null;
      res.writeHead(200, { 'content-type': 'application/json' }); res.end(JSON.stringify({ jsonrpc: '2.0', id, result }));
    });
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  try {
    const client = new x402Client().register(f.terms.network, new OfficialClientScheme(f.payer, { rpcUrl: `http://127.0.0.1:${server.address().port}` }));
    const paymentPayload = await client.createPaymentPayload({ x402Version: 2, resource: { url: f.resourceUrl }, accepts: [f.terms] });
    const message = getCompiledTransactionMessageDecoder().decode(getTransactionDecoder().decode(Buffer.from(paymentPayload.payload.transaction, 'base64')).messageBytes);
    const program = instruction => message.staticAccounts[instruction.programAddressIndex];
    const limit = message.instructions.find(i => program(i) === 'ComputeBudget111111111111111111111111111111' && i.data[0] === 2);
    assert.equal(Buffer.from(limit.data).readUInt32LE(1), 20_000);
    const memo = message.instructions.find(i => program(i) === 'MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr');
    assert.equal(Buffer.from(memo.data).toString(), f.terms.extra.memo);
    const payment = await f.bridge.verify({ paymentPayload, paymentRequirements: f.terms, resourceUrl: f.resourceUrl });
    assert.ok(payment.transaction_id);
    assert.equal(f.signatures(), 1);
  } finally { server.close(); }
});

test('verifier refuses a memo that does not match the quote and request', async () => {
  const f = await setup();
  const { quoteId, requestSha256 } = f.terms.extra;
  for (const memo of [shortMemo(quoteId, 'b'.repeat(64)), `covenant.x402.v2:00000000-0000-4000-8000-000000000009:${requestSha256}`,
    f.terms.extra.memo.toUpperCase(), createHash('sha256').update(`covenant.x402.v3:${quoteId}:${requestSha256}`).digest('hex')]) {
    const terms = structuredClone(f.terms); terms.extra.memo = memo;
    const paymentPayload = await f.payload({ memo }); paymentPayload.accepted = terms;
    await assert.rejects(f.bridge.verify({ paymentPayload, paymentRequirements: terms, resourceUrl: f.config.resourceUrl }), /invalid quote binding/);
  }
  assert.equal(f.simulations(), 0);
  assert.equal(f.signatures(), 0);
});

test('a quote issued before v3 still verifies with its v2 memo and 60,000 units', async () => {
  const f = await setup({ legacyMemo: true });
  const payment = await f.verify({ units: 60_000 });
  assert.ok(payment.transaction_id);
  await assert.rejects(f.verify({ memo: shortMemo(f.terms.extra.quoteId, f.terms.extra.requestSha256) }), /memo/);
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
  changed.extra.memo = shortMemo(changed.extra.quoteId, changed.extra.requestSha256);
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

test('a facilitator holding the same key broadcasts exactly the journaled bytes', async () => {
  const calls = [];
  let reply = async () => ({ success: true });
  const replies = [];
  const facilitator = { settle: async (payload, terms) => { calls.push({ payload, terms }); const r = reply(payload, terms); replies.push(r); return r; } };
  const f = await setup({ facilitator });
  // The facilitator signs the payer-signed payload with the same fee payer key.
  const own = toFacilitatorSvmSigner(f.feePayerKey);
  reply = async (payload, terms) => {
    const signed = await own.signTransaction(payload.payload.transaction, terms.extra.feePayer, terms.network);
    return { success: true, transaction: getSignatureFromTransaction(getTransactionDecoder().decode(Buffer.from(signed, 'base64'))) };
  };
  const payment = await f.verify();
  f.state.signature = payment.transaction_id;
  assert.equal((await f.bridge.settle({ payment, paymentRequirements: f.terms })).status, 'pending');
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0].terms, f.terms);
  // Ed25519 is deterministic: the facilitator's transaction is the journaled one.
  assert.equal((await replies[0]).transaction, payment.transaction_id);
  assert.equal(f.state.sends.length, 0);
  // While the facilitator holds it, retries neither resubmit nor send.
  assert.equal((await f.bridge.settle({ payment, paymentRequirements: f.terms })).status, 'pending');
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(calls.length, 1);
  assert.equal(f.state.sends.length, 0);
  const saved = JSON.parse(payment.encoded_transaction).evidence;
  f.state.status = { confirmationStatus: 'finalized', err: null };
  f.state.readback = { meta: { err: null }, transaction: [saved.transaction, 'base64'] };
  assert.deepEqual(await f.bridge.settle({ payment, paymentRequirements: f.terms }), { status: 'finalized', transaction_id: payment.transaction_id });
});

test('a facilitator that declines, is unreachable or settles another signature leaves the broadcast to the verifier', async () => {
  for (const reply of [async () => ({ success: false, errorReason: 'facilitator_daily_limit_reached' }), async () => { throw new Error('unreachable'); }, async () => ({ success: true, transaction: '1'.repeat(64) })]) {
    const f = await setup({ facilitator: { settle: reply } });
    const payment = await f.verify();
    f.state.signature = payment.transaction_id;
    assert.equal((await f.bridge.settle({ payment, paymentRequirements: f.terms })).status, 'pending');
    await new Promise(resolve => setTimeout(resolve, 10));
    assert.equal(f.state.sends.length, 0);
    assert.equal((await f.bridge.settle({ payment, paymentRequirements: f.terms })).status, 'pending');
    assert.equal(f.state.sends.length, 1);
    assert.equal(f.state.sends[0], JSON.parse(payment.encoded_transaction).evidence.transaction);
  }
});

test('payments stored before the facilitator was configured are sent directly', async () => {
  const plain = await setup();
  const payment = await plain.verify();
  assert.equal(JSON.parse(payment.encoded_transaction).evidence.payload, undefined);
  let called = 0;
  const withFacilitator = createBridge({ signer: toFacilitatorSvmSigner(plain.feePayerKey), config: plain.config,
    rpc: async (method, params) => method === 'getGenesisHash' ? RAILS[plain.config.network].genesis : method === 'getSignatureStatuses' ? { value: [null] } : (plain.state.sends.push(params[0]), payment.transaction_id),
    now: () => 1000, facilitator: { settle: async () => { called++; return { success: true }; } } });
  assert.equal((await withFacilitator.settle({ payment, paymentRequirements: plain.terms })).status, 'pending');
  assert.equal(called, 0);
  assert.equal(plain.state.sends.length, 1);
});

test('a facilitator that hangs gets a few seconds before the bytes are sent from here', async () => {
  const f = await setup({ facilitator: { settle: () => new Promise(() => {}) } });
  const payment = await f.verify();
  f.state.signature = payment.transaction_id;
  let clock = 1000;
  const bridge = createBridge({ signer: toFacilitatorSvmSigner(f.feePayerKey), config: f.config,
    rpc: async (method, params) => method === 'getGenesisHash' ? RAILS[f.config.network].genesis
      : method === 'getSignatureStatuses' ? { value: [null] } : (f.state.sends.push(params[0]), payment.transaction_id),
    now: () => clock, facilitator: { settle: () => new Promise(() => {}) } });
  assert.equal((await bridge.settle({ payment, paymentRequirements: f.terms })).status, 'pending');
  clock += FACILITATOR_GRACE_MS - 1;
  assert.equal((await bridge.settle({ payment, paymentRequirements: f.terms })).status, 'pending');
  assert.equal(f.state.sends.length, 0);
  clock += 1;
  assert.equal((await bridge.settle({ payment, paymentRequirements: f.terms })).status, 'pending');
  assert.deepEqual(f.state.sends, [JSON.parse(payment.encoded_transaction).evidence.transaction]);
});

