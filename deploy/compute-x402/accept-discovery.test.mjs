import test from 'node:test';
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {acceptDiscovery, ASSET, BASE_ASSET, BASE_NETWORK, NETWORK, baseUrl, expectedRecords, inputSchema, parseArgs,
  sha256, shortMemo, stable, strictJson, tariffPrice, validateExpected, validateJobQuote, validateQuote, validateSnapshot} from './accept-discovery.mjs';

const now = 1800000000000;
const revision = 'a'.repeat(40);
const origin = 'https://inference.example';
function policy() {
  return {payTo: '11111111111111111111111111111111', feePayer: ASSET, model: 'claude-haiku-4.5',
    legacyAmount: 50000, maxInputBytes: 4096, maxOutputTokens: 2048,
    tariff: {schema_version: 1, id: 'covenant-haiku-4.5-retail', revision: 1, model: 'claude-haiku-4.5',
      base_micro_usdc: 12000, output_cap_micro_usdc_per_million: 9000000,
      max_output_tokens: 2048, valid_from_ms: now - 1000, valid_until_ms: now + 86400000}};
}
function snapshot(url = origin, serverNow = now) {
  const expected = policy();
  return {health: {status: 'ok', service: 'covenant-compute-x402', protocol: 'x402-v2', admissions_enabled: true, revision},
    manifest: {schemaVersion: 1, x402Version: 2, name: 'Covenant Compute',
      description: 'Available text inference and its current exact Solana USDC payment terms.',
      observedAtMs: serverNow, available: true, ...expectedRecords(expected, url)},
    catalog: {object: 'list', data: expectedRecords(expected, url).models}};
}
function quote(request, id, url = origin, serverNow = now) {
  const expected = policy();
  const hash = sha256(request);
  const amount = request.pricing ? tariffPrice(expected.tariff, request.max_tokens) : 50000;
  return {x402Version: 2, resource: {url: `${url}/v1/chat/completions`,
    description: 'Text inference for AI agents, with a receipt signed by the operator who ran it.', mimeType: 'application/json',
    serviceName: 'Covenant Compute', tags: ['inference', 'llm', 'ai', 'receipts'], iconUrl: `${url}/brand/covenant-icon-02026997.png`},
  accepts: [{scheme: 'exact', network: NETWORK, asset: ASSET, amount: String(amount), payTo: expected.payTo,
    maxTimeoutSeconds: 90, extra: {feePayer: expected.feePayer, memo: shortMemo(id, hash),
      quoteId: id, requestSha256: hash, expiresAtMs: serverNow + 75000}}],
  extensions: {bazaar: {info: {input: {type: 'http'}}, schema: {type: 'object'}}}};
}
const JOBS_MERCHANT = 'SysvarRent111111111111111111111111111111111';
function jobEndpoint(url = origin) {
  return {path: '/v1/jobs/research', url: `${url}/v1/jobs/research`, method: 'POST', description: 'A sourced research report.',
    payment: {scheme: 'exact', network: NETWORK, asset: ASSET, assetDecimals: 6, payTo: JOBS_MERCHANT, feePayer: ASSET},
    price: {currency: 'USDC', unit: 'micro_usdc', billing: 'fixed_job', price_micro_usdc: 5000000},
    inputSchema: {type: 'object'}, statusPath: '/v1/jobs/{id}', statusMethod: 'GET',
    paymentRequiredHeader: 'Payment-Required', paymentSignatureHeader: 'Payment-Signature'};
}
function jobQuote(request, url = origin, serverNow = now) {
  const hash = sha256(request), id = '5b0e8a52-2f7e-4c55-9a2d-6c0b0f3e9a11';
  return {x402Version: 2, resource: {url: `${url}/v1/jobs/research`, description: 'A sourced research report.', mimeType: 'application/json',
    serviceName: 'Covenant Compute', tags: ['research'], iconUrl: `${url}/brand/covenant-icon-02026997.png`},
  accepts: [{scheme: 'exact', network: NETWORK, asset: ASSET, amount: '5000000', payTo: JOBS_MERCHANT, maxTimeoutSeconds: 90,
    extra: {feePayer: ASSET, memo: shortMemo(id, hash), quoteId: id, requestSha256: hash, expiresAtMs: serverNow + 90000}}],
  extensions: {bazaar: {info: {}, schema: {}}}};
}
const encoded = body => Buffer.from(JSON.stringify(body)).toString('base64');
const context = {expected: policy(), origin, revision, started: now, ended: now};

test('strict parser rejects duplicate decoded keys, bad numbers, nesting and trailing data', () => {
  assert.equal(strictJson('{"x":1}').x, 1);
  for (const text of ['{"x":1,"x":2}', '{"x":1,"\\u0078":2}', '[1,]', 'true false', '1e999',
    '{"x":"unfinished}', '['.repeat(65) + ']'.repeat(65), '{"x":01}', '{"x":"a\nb"}',
    '9007199254740993', '1.00000000000000001', '1e0']) {
    assert.throws(() => strictJson(text));
  }
  assert.equal(strictJson('{"__proto__":{"x":1}}').__proto__.x, 1);
  assert.equal(Object.getPrototypeOf(strictJson('{}')), null);
  assert.throws(() => strictJson(' '.repeat(262145)));
});
test('fixed requested cap formula uses exact ceil and rejects unsafe amounts', () => {
  const t = policy().tariff;
  assert.deepEqual([1, 256, 2048].map(cap => tariffPrice(t, cap)), [12009, 14304, 30432]);
  assert.equal(tariffPrice({...t, base_micro_usdc: 10000, output_cap_micro_usdc_per_million: 7500000}, 1), 10008);
  for (const cap of [0, 2049, 1.5, Number.MAX_SAFE_INTEGER]) assert.throws(() => tariffPrice(t, cap));
  assert.throws(() => tariffPrice({...t, base_micro_usdc: Number.MAX_SAFE_INTEGER}, 1));
});
test('independent policy pins valid identities, tariff window and qualified full limits', () => {
  validateExpected(policy(), now);
  const changes = [p => p.privateKey = 'no', p => p.payTo = 'not-an-address', p => p.feePayer = '1'.repeat(33),
    p => p.maxOutputTokens = 256, p => p.maxInputBytes = 10000, p => p.tariff.revision = 1.5,
    p => p.tariff.valid_until_ms = now + 45000, p => p.tariff.valid_from_ms = now + 1,
    p => p.tariff.model = 'other', p => p.tariff.base_micro_usdc = Number.MAX_SAFE_INTEGER + 1,
    p => p.tariff.native_price_micro_usdc = 10000];
  for (const change of changes) { const p = policy(); change(p); assert.throws(() => validateExpected(p, now)); }
});
test('request schema permits only text and separates absent pricing from exact opt in', () => {
  const legacy = inputSchema(policy(), false), tariff = inputSchema(policy(), true);
  assert.equal(legacy.additionalProperties, false);
  assert.equal(Object.hasOwn(legacy.properties, 'pricing'), false);
  assert.deepEqual(tariff.required, ['model', 'messages', 'max_tokens', 'pricing']);
  assert.deepEqual(tariff.properties.pricing, {const: 'model_tariff_v1'});
  assert.equal(tariff.properties.max_tokens.maximum, 2048);
  assert.deepEqual(tariff.properties.stream, {const: false});
  assert.deepEqual(tariff.properties.messages.items.properties.role.enum, ['system', 'user', 'assistant']);
  for (const feature of ['tools', 'images', 'audio']) assert.equal(Object.hasOwn(tariff.properties, feature), false);
});
test('exact manifest/catalog rejects stale, false, unsupported, private and mismatched offers', () => {
  validateSnapshot(snapshot(), context);
  const changes = [s => s.health.revision = 'b'.repeat(40), s => s.health.admissions_enabled = false,
    s => s.manifest.observedAtMs = now - 5001, s => s.manifest.available = false,
    s => s.manifest.bridge_secret = 'no', s => s.manifest.x402Version = 1,
    s => s.manifest.endpoints[1].payment.network = 'solana:devnet',
    s => s.manifest.endpoints[1].payment.assetDecimals = 9,
    s => s.manifest.endpoints[1].payment.feePayer = '11111111111111111111111111111111',
    s => s.manifest.endpoints[1].inputSchema.properties.tools = {},
    s => s.manifest.endpoints[1].inputSchema.properties.pricing = {const: 'model_tariff_v1'},
    s => s.manifest.endpoints[2].inputSchema.required.pop(),
    s => s.manifest.endpoints[2].price.max_output_tokens = 256,
    s => s.manifest.endpoints[2].price.tariff.revision = 2,
    s => s.manifest.models[0].streaming = true, s => s.manifest.models[0].vision = true,
    s => s.manifest.models[0].native_identity = 'private', s => s.catalog.data[0].pricing.maximum_price_micro_usdc = 50000,
    s => s.catalog.data.push(s.catalog.data[0]), s => s.manifest.endpoints.push({path: '/private', method: 'GET'})];
  for (const change of changes) { const s = snapshot(); change(s); assert.throws(() => validateSnapshot(s, context)); }
});
test('402 binds exact request, ID, rail and fixed cap amount; does not invent native proof', () => {
  const id = '11111111-1111-4111-8111-111111111111';
  const request = {model: policy().model, messages: [{role: 'user', content: 'fixture'}], max_tokens: 256, pricing: 'model_tariff_v1'};
  const ctx = {request, id, expected: policy(), origin, ended: now};
  const body = quote(request, id);
  assert.equal(validateQuote(body, encoded(body), ctx).amount_micro_usdc, 14304);
  assert.equal(sha256({b: 1, a: {z: 2, y: 3}}), sha256({a: {y: 3, z: 2}, b: 1}));
  const changes = [b => b.accepts[0].amount = '50000', b => b.accepts[0].amount = 14304,
    b => b.accepts[0].extra.quoteId = 'other', b => b.accepts[0].extra.requestSha256 = 'b'.repeat(64),
    b => b.accepts[0].extra.memo += 'wrong', b => b.accepts[0].extra.expiresAtMs = now,
    b => b.accepts[0].extra.expiresAtMs = now + 80001, b => b.accepts[0].native_tariff = {},
    b => b.resource.url = 'https://other.example/v1/chat/completions', b => b.accepts.push(b.accepts[0])];
  for (const change of changes) { const b = structuredClone(body); change(b); assert.throws(() => validateQuote(b, encoded(b), ctx)); }
  assert.throws(() => validateQuote(body, encoded({...body, x402Version: 1}), ctx));
  assert.throws(() => validateQuote(body, encoded(body) + '\n', ctx));
});
test('quote clock tolerance preserves expiry and absolute tariff delivery limits', () => {
  const id = '11111111-1111-4111-8111-111111111111';
  for (const priced of [false, true]) {
    const request = {model: policy().model, messages: [{role: 'user', content: 'fixture'}], max_tokens: 256,
      ...(priced ? {pricing: 'model_tariff_v1'} : {})};
    const ctx = {request, id, expected: policy(), origin, ended: now};
    for (const offset of [75054, 80000]) {
      const body = quote(request, id); body.accepts[0].extra.expiresAtMs = now + offset;
      assert.equal(validateQuote(body, encoded(body), ctx).expires_at_ms, now + offset);
    }
    for (const offset of [-1, 0, 80001]) {
      const body = quote(request, id); body.accepts[0].extra.expiresAtMs = now + offset;
      assert.throws(() => validateQuote(body, encoded(body), ctx), /quote expiry/);
    }
    if (priced) {
      const body = quote(request, id, origin, now + 5000);
      const expected = policy(); expected.tariff.valid_until_ms = now + 125000;
      assert.equal(validateQuote(body, encoded(body), {...ctx, expected}).expires_at_ms, now + 80000);
      expected.tariff.valid_until_ms--;
      assert.throws(() => validateQuote(body, encoded(body), {...ctx, expected}), /quote tariff delivery margin/);
    }
  }
});
test('CLI and URL surface cannot inject authorization, config, alternate paths or paid requests', () => {
  assert.deepEqual(parseArgs(['--base-url', origin, '--revision', revision, '--expected', 'public.json']),
    {base: origin, revision, expectedPath: 'public.json', quotes: false});
  assert.equal(parseArgs(['--base-url', origin, '--revision', revision, '--expected', 'public.json', '--quotes']).quotes, true);
  for (const args of [['--wallet', 'key.json'], ['--quotes', '--quotes'], ['--base-url', origin, '--base-url', origin]]) assert.throws(() => parseArgs(args));
  for (const url of ['http://example.com', `${origin}/private`, 'https://secret@example.com', `${origin}?token=x`, 'file:///tmp/file']) assert.throws(() => baseUrl(url));
  assert.equal(baseUrl('http://127.0.0.1:8888/'), 'http://127.0.0.1:8888');
});

async function fixture(t, mutate, serverClockOffset = 0) {
  const requests = [];
  let url;
  const server = createServer(async (req, res) => {
    let raw = ''; for await (const bytes of req) raw += bytes;
    const entry = {method: req.method, path: req.url, headers: req.headers, body: raw ? strictJson(raw) : undefined};
    requests.push(entry);
    if (mutate?.(entry, res)) return;
    const s = snapshot(url, now + serverClockOffset);
    let body = {'/health': s.health, '/.well-known/x402.json': s.manifest, '/v1/models': s.catalog}[req.url];
    res.setHeader('content-type', 'application/json'); res.setHeader('cache-control', 'no-store');
    if (req.method === 'POST') {
      body = quote(entry.body, req.headers['idempotency-key'], url, now + serverClockOffset);
      res.statusCode = 402; res.setHeader('payment-required', encoded(body));
    }
    res.end(JSON.stringify(body));
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  url = `http://127.0.0.1:${server.address().port}`;
  t.after(() => new Promise(resolve => { server.closeAllConnections(); server.close(resolve); }));
  return {url, requests};
}
test('default mode performs exactly three uncached GETs, with no wallet or payment calls', async t => {
  const f = await fixture(t);
  const report = await acceptDiscovery({base: f.url, revision, expected: policy(), now: () => now});
  assert.equal(report.accepted, true); assert.equal(report.mode, 'get_only');
  assert.deepEqual(report.calls, {get: 3, unsigned_quote_post: 0}); assert.deepEqual(report.quotes, []);
  assert.equal(report.after, null); assert.equal(report.wallet_files_read, 0);
  for (const r of f.requests) { assert.equal(r.method, 'GET'); assert.equal(r.headers['cache-control'], 'no-cache'); assert.equal(r.headers.pragma, 'no-cache'); }
});
test('explicit quote mode makes four fresh unpaid requests and validates post-observation', async t => {
  const f = await fixture(t);
  const report = await acceptDiscovery({base: f.url, revision, expected: policy(), now: () => now, quotes: true});
  assert.deepEqual(report.calls, {get: 6, unsigned_quote_post: 4});
  assert.deepEqual(report.quotes.map(q => q.amount_micro_usdc), [50000, 12009, 14304, 30432]);
  const posts = f.requests.filter(r => r.method === 'POST');
  assert.equal(new Set(posts.map(r => r.headers['idempotency-key'])).size, 4);
  assert.equal(Object.hasOwn(posts[0].body, 'pricing'), false);
  assert.deepEqual(posts.slice(1).map(r => [r.body.pricing, r.body.max_tokens]), [['model_tariff_v1', 1], ['model_tariff_v1', 256], ['model_tariff_v1', 2048]]);
  for (const r of f.requests) for (const header of ['authorization', 'cookie', 'payment-signature', 'x-payment']) assert.equal(r.headers[header], undefined);
  assert.equal(report.payment_authorizations_sent, 0); assert.equal(report.signatures_created, 0);
  assert.match(report.limitations.join(' '), /do not establish unchanged private journals/);
});
test('real HTTP acceptance tolerates a server clock five seconds ahead for both quote modes', async t => {
  const f = await fixture(t, undefined, 5000);
  const report = await acceptDiscovery({base: f.url, revision, expected: policy(), now: () => now, quotes: true});
  assert.equal(report.accepted, true);
  assert.equal(report.before.observed_at_ms, now + 5000);
  assert.equal(report.after.observed_at_ms, now + 5000);
  assert.deepEqual(report.calls, {get: 6, unsigned_quote_post: 4});
  assert.deepEqual(report.quotes.map(q => q.amount_micro_usdc), [50000, 12009, 14304, 30432]);
  assert.deepEqual(report.quotes.map(q => q.expires_at_ms), Array(4).fill(now + 80000));
  assert.equal(report.payment_authorizations_sent, 0);
  for (const r of f.requests) assert.equal(r.headers['payment-signature'], undefined);
});
test('redirects are not followed and do not lead to quote creation', async t => {
  const f = await fixture(t, (req, res) => { if (req.path === '/health') { res.writeHead(302, {location: '/target'}); res.end(); return true; } });
  await assert.rejects(acceptDiscovery({base: f.url, revision, expected: policy(), now: () => now, quotes: true}), /network failure/);
  assert.deepEqual(f.requests.map(r => r.path), ['/health']);
});
test('oversized chunked JSON, duplicate keys and missing no-store fail closed', async t => {
  for (const kind of ['oversized', 'duplicate', 'cache']) {
    const f = await fixture(t, (req, res) => {
      if (req.path !== '/.well-known/x402.json') return false;
      res.writeHead(200, {'content-type': 'application/json', ...(kind === 'cache' ? {} : {'cache-control': 'no-store'})});
      res.end(kind === 'oversized' ? ' '.repeat(262145) : '{"schemaVersion":1,"schemaVersion":2}'); return true;
    });
    await assert.rejects(acceptDiscovery({base: f.url, revision, expected: policy(), now: () => now, quotes: true}));
    assert.equal(f.requests.some(r => r.method === 'POST'), false);
  }
});
test('an unknown quote outcome stops immediately without retry or subsequent quote', async t => {
  const f = await fixture(t, (req, res) => { if (req.method === 'POST') { res.destroy(); return true; } });
  await assert.rejects(acceptDiscovery({base: f.url, revision, expected: policy(), now: () => now, quotes: true}), /no automatic retry/);
  assert.equal(f.requests.filter(r => r.method === 'POST').length, 1);
});
test('fetch policy is closed and rejects payment results even on otherwise valid responses', async () => {
  let calls = 0;
  const fetchImpl = async (url, init) => {
    calls++; assert.equal(init.redirect, 'error'); assert.equal(init.cache, 'no-store'); assert.equal(init.credentials, 'omit');
    assert.equal(init.method, 'GET'); assert.equal(init.body, undefined);
    return new Response(stable(snapshot().health), {headers: {'content-type': 'application/json', 'payment-response': 'unexpected'}});
  };
  await assert.rejects(acceptDiscovery({base: origin, revision, expected: policy(), now: () => now, fetchImpl}), /unexpected payment result/);
  assert.equal(calls, 1);
});

test('short memo matches the published vector', () => {
  assert.equal(shortMemo('00000000-0000-4000-8000-000000000001', 'a'.repeat(64)), '8bfb7fde3cb99c6b0f16b7a18d83a0e1');
});

test('a Base merchant in the policy requires Base in the manifest and in every quote', () => {
  const base = {...policy(), basePayTo: '0x5fA1d0C0bfFE257a20027C523093F941834f5D66'};
  validateExpected(base, now);
  assert.throws(() => validateExpected({...base, basePayTo: 'not-an-address'}, now));
  const records = expectedRecords(base, origin);
  assert.equal(records.endpoints[1].payments[1].network, BASE_NETWORK);
  const manifest = {schemaVersion: 1, x402Version: 2, name: 'Covenant Compute',
    description: 'Available text inference and its current exact USDC payment terms on Solana and Base.',
    observedAtMs: now, available: true, ...records};
  const health = snapshot().health;
  validateSnapshot({health, manifest, catalog: {object: 'list', data: records.models}}, {...context, expected: base});
  assert.throws(() => validateSnapshot(snapshot(), {...context, expected: base}), /manifest/);
  assert.throws(() => validateSnapshot({health, manifest, catalog: {object: 'list', data: records.models}}, context), /manifest/,
    'Base advertised without an approved merchant');
  const id = '11111111-1111-4111-8111-111111111111';
  const request = {model: base.model, messages: [{role: 'user', content: 'fixture'}], max_tokens: 256};
  const body = quote(request, id);
  body.accepts.push({scheme: 'exact', network: BASE_NETWORK, asset: BASE_ASSET, amount: '50000', payTo: base.basePayTo,
    maxTimeoutSeconds: 300, extra: {name: 'USD Coin', version: '2', quoteId: id, requestSha256: sha256(request),
      expiresAtMs: body.accepts[0].extra.expiresAtMs}});
  const ctx = {request, id, expected: base, origin, ended: now};
  assert.equal(validateQuote(body, encoded(body), ctx).amount_micro_usdc, 50000);
  const solanaOnly = quote(request, id);
  assert.throws(() => validateQuote(solanaOnly, encoded(solanaOnly), ctx), /payment quote/);
  assert.throws(() => validateQuote(body, encoded(body), {...ctx, expected: policy()}), /payment quote/,
    'Base quoted without an approved merchant');
  const reordered = structuredClone(body); reordered.accepts.reverse();
  for (const changed of [reordered, ...[o => { o.payTo = '0x0000000000000000000000000000000000000001'; }, o => { o.amount = '50001'; },
    o => { o.extra.name = 'USDC'; }].map(change => { const copy = structuredClone(body); change(copy.accepts[1]); return copy; })]) {
    assert.throws(() => validateQuote(changed, encoded(changed), ctx), /payment quote/);
  }
});

test('paid jobs in the manifest must match the expected price and merchant exactly', () => {
  const expected = {...policy(), jobs: [{kind: 'research', amount: 5000000, payTo: JOBS_MERCHANT}]};
  const withJobs = snapshot();
  withJobs.manifest.endpoints.push(jobEndpoint());
  validateSnapshot(withJobs, {...context, expected});
  assert.throws(() => validateSnapshot(withJobs, context), /job endpoints/);
  assert.throws(() => validateSnapshot(snapshot(), {...context, expected}), /job endpoints/);
  for (const change of [e => { e.price.price_micro_usdc = 4000000; }, e => { e.payment.payTo = ASSET; }, e => { e.url = 'https://other.example/v1/jobs/research'; },
    e => { e.extra = true; }, e => { delete e.statusPath; }, e => { e.description = ''; }]) {
    const changed = snapshot(); const endpoint = jobEndpoint(); change(endpoint); changed.manifest.endpoints.push(endpoint);
    assert.throws(() => validateSnapshot(changed, {...context, expected}));
  }
  for (const jobs of [[], [{kind: 'other', amount: 1, payTo: JOBS_MERCHANT}], [{kind: 'research', amount: 0, payTo: JOBS_MERCHANT}],
    [{kind: 'research', amount: 1, payTo: JOBS_MERCHANT}, {kind: 'research', amount: 1, payTo: JOBS_MERCHANT}]]) {
    assert.throws(() => validateExpected({...policy(), jobs}, now), /expected job/);
  }
});
test('a job quote binds its fixed price, the jobs merchant and the request', () => {
  const job = {kind: 'research', amount: 5000000, payTo: JOBS_MERCHANT};
  const request = {objective: 'Discovery quote check; do not submit payment. This request only asks for a price.'};
  const body = jobQuote(request);
  const ctx = {job, request, expected: policy(), origin, ended: now};
  assert.equal(validateJobQuote(body, encoded(body), ctx).amount_micro_usdc, 5000000);
  for (const change of [b => { b.accepts[0].amount = '4000000'; }, b => { b.accepts[0].payTo = ASSET; }, b => { b.accepts[0].extra.memo = 'x'.repeat(32); },
    b => { b.accepts[0].extra.requestSha256 = 'c'.repeat(64); }, b => { b.resource.url = `${origin}/v1/chat/completions`; },
    b => { delete b.extensions; }, b => { b.accepts.push(b.accepts[0]); }, b => { b.accepts[0].extra.expiresAtMs = now + 200000; }]) {
    const b = structuredClone(body); change(b); assert.throws(() => validateJobQuote(b, encoded(b), ctx));
  }
});
