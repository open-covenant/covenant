#!/usr/bin/env node
// Public discovery acceptance. Built-ins only; no wallet, SDK or environment configuration.
import {createHash, randomUUID} from 'node:crypto';
import {readFile} from 'node:fs/promises';
import {pathToFileURL} from 'node:url';

export const NETWORK = 'solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp';
export const ASSET = 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v';
// Optional USDC on Base, when the policy names a Base merchant (`basePayTo`).
export const BASE_NETWORK = 'eip155:8453';
export const BASE_ASSET = '0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913';
const MAX_BYTES = 262144;
const CLOCK_SKEW_MS = 5000;
// Native freshness (120s) less the 45s delivery margin caps a new quote at 75s.
const QUOTE_LIFETIME_MS = 75000;
const ICON_PATH = '/brand/covenant-icon-02026997.png';
/** Paid job kinds and the request each one's unsigned quote check sends. */
const JOB_KINDS = ['research'];
const JOB_QUOTE_REQUESTS = {research: {objective: 'Discovery quote check; do not submit payment. This request only asks for a price.'}};
const MAX_SAFE = BigInt(Number.MAX_SAFE_INTEGER);
const fail = message => { throw new Error(message); };
const requireThat = (condition, message) => { if (!condition) fail(message); };
const integer = (n, minimum = 0) => Number.isSafeInteger(n) && n >= minimum;
const plain = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const keys = (value, names, label) => requireThat(plain(value)
  && Object.keys(value).sort().join('\0') === [...names].sort().join('\0'), `${label}: unexpected fields`);
export const stable = value => Array.isArray(value) ? `[${value.map(stable).join(',')}]`
  : plain(value) ? `{${Object.keys(value).sort().map(k => `${JSON.stringify(k)}:${stable(value[k])}`).join(',')}}`
  : JSON.stringify(value);
export const sha256 = value => createHash('sha256').update(stable(value)).digest('hex');
// The gateway's v3 payment memo: 32 hex characters, within the official client's 20k CU.
export const shortMemo = (id, hash) => createHash('sha256').update(`covenant.x402.v3:${id}:${hash}`).digest('hex').slice(0, 32);
const equal = (a, b, label) => requireThat(stable(a) === stable(b), `${label}: mismatch`);

// JSON.parse alone silently accepts duplicate keys. Reject ambiguous public documents.
export function strictJson(text) {
  requireThat(typeof text === 'string' && Buffer.byteLength(text) <= MAX_BYTES, 'JSON size');
  let pos = 0;
  const space = () => { while (/[\x20\t\r\n]/.test(text[pos] ?? '\0')) pos++; };
  const string = () => {
    const start = pos++;
    while (pos < text.length) {
      if (text[pos++] === '"') return JSON.parse(text.slice(start, pos));
      if (text[pos - 1] === '\\') pos++;
    }
    return fail('JSON unterminated string');
  };
  const value = depth => {
    requireThat(depth < 64, 'JSON nesting'); space();
    const char = text[pos];
    if (char === '"') return string();
    if (char === '{') {
      pos++; space(); const result = Object.create(null);
      if (text[pos] === '}') { pos++; return result; }
      while (true) {
        requireThat(text[pos] === '"', 'JSON object key'); const key = string();
        requireThat(!Object.hasOwn(result, key), 'JSON duplicate key'); space();
        requireThat(text[pos++] === ':', 'JSON colon'); result[key] = value(depth + 1); space();
        if (text[pos] === '}') { pos++; return result; }
        requireThat(text[pos++] === ',', 'JSON object separator'); space();
      }
    }
    if (char === '[') {
      pos++; space(); const result = [];
      if (text[pos] === ']') { pos++; return result; }
      while (true) {
        result.push(value(depth + 1)); space();
        if (text[pos] === ']') { pos++; return result; }
        requireThat(text[pos++] === ',', 'JSON array separator');
      }
    }
    const token = /^(?:true|false|null|-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?)/.exec(text.slice(pos));
    requireThat(token, 'JSON scalar'); pos += token[0].length;
    const result = JSON.parse(token[0]);
    // Every numeric field in the published discovery/402 contract is integral.
    // Inspect the token too: parsing 1.00000000000000001 first would round to 1.
    requireThat(typeof result !== 'number' || (/^-?(?:0|[1-9]\d*)$/.test(token[0])
      && Number.isSafeInteger(result)), 'JSON unsafe integer');
    return result;
  };
  const result = value(0); space(); requireThat(pos === text.length, 'JSON trailing data'); return result;
}

function address(value) {
  if (typeof value !== 'string' || !/^[1-9A-HJ-NP-Za-km-z]{32,44}$/.test(value)) return false;
  const alphabet = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
  let number = 0n;
  for (const char of value) number = number * 58n + BigInt(alphabet.indexOf(char));
  let length = 0;
  while (number) { number >>= 8n; length++; }
  return length + (value.match(/^1*/)[0].length) === 32;
}
export function baseUrl(value) {
  const url = new URL(value);
  requireThat((url.protocol === 'https:' || (url.protocol === 'http:'
    && ['127.0.0.1', '[::1]', 'localhost'].includes(url.hostname)))
    && !url.username && !url.password && url.pathname === '/' && !url.search && !url.hash,
  'base URL must be an HTTPS origin (loopback HTTP is allowed for fixtures)');
  return url.origin;
}
export function tariffPrice(tariff, cap) {
  requireThat(integer(cap, 1) && cap <= tariff.max_output_tokens, 'tariff output cap');
  const amount = BigInt(tariff.base_micro_usdc)
    + (BigInt(tariff.output_cap_micro_usdc_per_million) * BigInt(cap) + 999999n) / 1000000n;
  requireThat(amount > 0n && amount <= MAX_SAFE, 'tariff amount range');
  return Number(amount);
}
export function validateExpected(expected, now = Date.now()) {
  const optional = ['basePayTo', 'jobs'].filter(key => plain(expected) && Object.hasOwn(expected, key));
  keys(expected, ['payTo', 'feePayer', 'model', 'legacyAmount', 'maxInputBytes', 'maxOutputTokens', 'tariff', ...optional], 'expected policy');
  requireThat(address(expected.payTo) && address(expected.feePayer), 'expected payment addresses');
  if (optional.includes('basePayTo')) requireThat(typeof expected.basePayTo === 'string' && /^0x[0-9a-fA-F]{40}$/.test(expected.basePayTo), 'expected Base merchant');
  if (optional.includes('jobs')) {
    requireThat(Array.isArray(expected.jobs) && expected.jobs.length >= 1 && expected.jobs.length <= JOB_KINDS.length
      && new Set(expected.jobs.map(job => job?.kind)).size === expected.jobs.length, 'expected jobs');
    for (const job of expected.jobs) {
      keys(job, ['kind', 'amount', 'payTo'], 'expected job');
      requireThat(JOB_KINDS.includes(job.kind) && integer(job.amount, 1) && address(job.payTo), 'expected job');
    }
  }
  requireThat(typeof expected.model === 'string' && /^[a-zA-Z0-9._:/-]{1,128}$/.test(expected.model), 'expected model');
  requireThat(integer(expected.legacyAmount, 1) && expected.maxInputBytes === 4096
    && expected.maxOutputTokens === 2048, 'expected release limits');
  const t = expected.tariff;
  keys(t, ['schema_version', 'id', 'revision', 'model', 'base_micro_usdc',
    'output_cap_micro_usdc_per_million', 'max_output_tokens', 'valid_from_ms', 'valid_until_ms'], 'expected tariff');
  requireThat(t.schema_version === 1 && typeof t.id === 'string' && /^[a-zA-Z0-9._:-]{1,128}$/.test(t.id)
    && t.model === expected.model && integer(t.revision, 1)
    && integer(t.base_micro_usdc) && integer(t.output_cap_micro_usdc_per_million)
    && t.max_output_tokens === expected.maxOutputTokens && integer(t.valid_from_ms)
    && integer(t.valid_until_ms, 1) && t.valid_from_ms <= now && t.valid_until_ms > now + 45000,
  'expected tariff must be valid with delivery margin');
  tariffPrice(t, 1); tariffPrice(t, t.max_output_tokens);
  return expected;
}
export function inputSchema(expected, priced) {
  const schema = {type: 'object', additionalProperties: false, required: ['model', 'messages', 'max_tokens'], properties: {
    model: {type: 'string', const: expected.model},
    messages: {type: 'array', minItems: 1, maxItems: 32,
      description: `Normalized JSON message array must not exceed ${expected.maxInputBytes} UTF-8 bytes.`,
      items: {type: 'object', additionalProperties: false, required: ['role', 'content'], properties: {
        role: {enum: ['system', 'user', 'assistant']}, content: {type: 'string', minLength: 1}}}},
    max_tokens: {type: 'integer', minimum: 1, maximum: expected.maxOutputTokens},
    temperature: {type: 'number', minimum: 0, maximum: 2}, stream: {const: false},
  }};
  if (priced) { schema.required.push('pricing'); schema.properties.pricing = {const: 'model_tariff_v1'}; }
  return schema;
}
export function expectedRecords(expected, origin) {
  const legacy = {currency: 'USDC', unit: 'micro_usdc', billing: 'fixed_request',
    price_micro_usdc: expected.legacyAmount, max_output_tokens: expected.maxOutputTokens};
  const tariff = {currency: 'USDC', unit: 'micro_usdc', billing: 'fixed_requested_output_cap',
    request_pricing: 'model_tariff_v1', tariff: expected.tariff,
    max_output_tokens: expected.maxOutputTokens,
    maximum_price_micro_usdc: tariffPrice(expected.tariff, expected.maxOutputTokens)};
  const models = [{id: expected.model, object: 'model', owned_by: 'covenant', streaming: false, tools: false, vision: false,
    max_input_bytes: expected.maxInputBytes, max_output_tokens: expected.maxOutputTokens,
    pricing: tariff, legacy_pricing: legacy}];
  const payment = {scheme: 'exact', network: NETWORK, asset: ASSET, assetDecimals: 6,
    payTo: expected.payTo, feePayer: expected.feePayer};
  const payments = expected.basePayTo ? {payments: [payment, {scheme: 'exact', network: BASE_NETWORK, asset: BASE_ASSET,
    assetDecimals: 6, payTo: expected.basePayTo, name: 'USD Coin', version: '2'}]} : {};
  const endpoints = [{path: '/v1/models', method: 'GET', price: 'free'}, ...[legacy, tariff].map((price, i) => ({
    path: '/v1/chat/completions', url: `${origin}/v1/chat/completions`, method: 'POST',
    description: 'Request a fresh HTTP 402 quote, then submit the identical body with Payment-Signature. Prices are fixed upfront; availability is an observation, not a reservation.',
    payment, ...payments, price, inputSchema: inputSchema(expected, i === 1), max_input_bytes: expected.maxInputBytes,
    paymentRequiredHeader: 'Payment-Required', paymentSignatureHeader: 'Payment-Signature', idempotencyHeader: 'Idempotency-Key',
  }))];
  return {models, endpoints};
}
/** The paid-job manifest entries, checked field by field against the expected prices and merchant. */
export function validateJobEndpoints(endpoints, expected, origin) {
  const jobs = expected.jobs ?? [];
  requireThat(endpoints.length === jobs.length, 'manifest: job endpoints');
  for (const job of jobs) {
    const endpoint = endpoints.find(e => e.path === `/v1/jobs/${job.kind}`);
    keys(endpoint, ['path', 'url', 'method', 'description', 'payment', 'price', 'inputSchema', 'statusPath', 'statusMethod',
      'paymentRequiredHeader', 'paymentSignatureHeader'], 'manifest job endpoint');
    requireThat(typeof endpoint.description === 'string' && endpoint.description.length > 0 && plain(endpoint.inputSchema), 'manifest job endpoint');
    const {description, inputSchema, ...rest} = endpoint;
    equal(rest, {path: `/v1/jobs/${job.kind}`, url: `${origin}/v1/jobs/${job.kind}`, method: 'POST',
      payment: {scheme: 'exact', network: NETWORK, asset: ASSET, assetDecimals: 6, payTo: job.payTo, feePayer: expected.feePayer},
      price: {currency: 'USDC', unit: 'micro_usdc', billing: 'fixed_job', price_micro_usdc: job.amount},
      statusPath: '/v1/jobs/{id}', statusMethod: 'GET',
      paymentRequiredHeader: 'Payment-Required', paymentSignatureHeader: 'Payment-Signature'}, 'manifest job endpoint');
  }
}
export function validateSnapshot({health, manifest, catalog}, {expected, origin, revision, started, ended}) {
  requireThat(/^[a-f0-9]{40}$/.test(revision), 'full source revision required');
  validateExpected(expected, ended);
  equal(health, {status: 'ok', service: 'covenant-compute-x402', protocol: 'x402-v2',
    admissions_enabled: true, revision}, 'health');
  keys(manifest, ['schemaVersion', 'x402Version', 'name', 'description', 'observedAtMs', 'available', 'models', 'endpoints'], 'manifest');
  requireThat(integer(manifest.observedAtMs) && manifest.observedAtMs >= started - CLOCK_SKEW_MS
    && manifest.observedAtMs <= ended + CLOCK_SKEW_MS, 'manifest observation is not fresh');
  const records = expectedRecords(expected, origin);
  requireThat(Array.isArray(manifest.endpoints), 'manifest: endpoints');
  const jobEndpoints = manifest.endpoints.filter(e => typeof e?.path === 'string' && e.path.startsWith('/v1/jobs/'));
  validateJobEndpoints(jobEndpoints, expected, origin);
  equal({...manifest, endpoints: manifest.endpoints.filter(e => !jobEndpoints.includes(e))}, {schemaVersion: 1, x402Version: 2, name: 'Covenant Compute',
    description: expected.basePayTo ? 'Available text inference and its current exact USDC payment terms on Solana and Base.'
      : 'Available text inference and its current exact Solana USDC payment terms.',
    observedAtMs: manifest.observedAtMs, available: true, ...records}, 'manifest');
  equal(catalog, {object: 'list', data: records.models}, 'model catalog');
}

async function readJson(response) {
  requireThat(/^application\/json(?:\s*;|$)/i.test(response.headers.get('content-type') ?? ''), 'response content type');
  const declared = response.headers.get('content-length');
  requireThat(declared === null || (/^\d+$/.test(declared) && Number(declared) <= MAX_BYTES), 'response size');
  const reader = response.body?.getReader(); requireThat(reader, 'response body missing');
  const chunks = []; let count = 0;
  try {
    while (true) {
      const {done, value} = await reader.read(); if (done) break;
      count += value.length; requireThat(count <= MAX_BYTES, 'response size'); chunks.push(value);
    }
    return strictJson(new TextDecoder('utf-8', {fatal: true}).decode(Buffer.concat(chunks)));
  } finally { await reader.cancel().catch(() => {}); }
}
function noStore(response) {
  requireThat((response.headers.get('cache-control') ?? '').split(',').some(v => v.trim().toLowerCase() === 'no-store'), 'response must prohibit caching');
}
export function validateQuote(body, header, {request, id, expected, origin, ended}) {
  requireThat(typeof header === 'string' && header.length <= MAX_BYTES
    && /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(header), 'Payment-Required encoding');
  const decoded = Buffer.from(header, 'base64');
  requireThat(decoded.toString('base64') === header, 'Payment-Required canonical base64');
  equal(strictJson(new TextDecoder('utf-8', {fatal: true}).decode(decoded)), body, 'Payment-Required header/body');
  const amount = request.pricing === 'model_tariff_v1' ? tariffPrice(expected.tariff, request.max_tokens) : expected.legacyAmount;
  const hash = sha256(request);
  const expires = body?.accepts?.[0]?.extra?.expiresAtMs;
  // Server-issued expiry uses the same bounded clock tolerance as the manifest.
  // This does not extend the quote itself or its absolute tariff delivery margin.
  requireThat(integer(expires, 1) && expires > ended
    && expires <= ended + QUOTE_LIFETIME_MS + CLOCK_SKEW_MS, 'quote expiry');
  if (request.pricing) requireThat(expires <= expected.tariff.valid_until_ms - 45000, 'quote tariff delivery margin');
  // A single request also declares Bazaar discovery, checked for shape only.
  const {extensions, ...quote} = body ?? {};
  requireThat(plain(extensions) && Object.keys(extensions).join() === 'bazaar' && plain(extensions.bazaar)
    && plain(extensions.bazaar.info) && plain(extensions.bazaar.schema), 'payment quote: Bazaar declaration');
  equal(quote, {x402Version: 2, resource: {url: `${origin}/v1/chat/completions`,
    description: 'Text inference for AI agents, with a receipt signed by the operator who ran it.', mimeType: 'application/json',
    serviceName: 'Covenant Compute', tags: ['inference', 'llm', 'ai', 'receipts'], iconUrl: `${origin}${ICON_PATH}`},
  accepts: [{scheme: 'exact', network: NETWORK, asset: ASSET, amount: String(amount), payTo: expected.payTo,
    maxTimeoutSeconds: 90, extra: {feePayer: expected.feePayer, memo: shortMemo(id, hash),
      quoteId: id, requestSha256: hash, expiresAtMs: expires}},
  ...(expected.basePayTo ? [{scheme: 'exact', network: BASE_NETWORK, asset: BASE_ASSET, amount: String(amount),
    payTo: expected.basePayTo, maxTimeoutSeconds: 300, extra: {name: 'USD Coin', version: '2',
      quoteId: id, requestSha256: hash, expiresAtMs: expires}}] : [])]}, 'payment quote');
  return {pricing: request.pricing ?? 'legacy', max_tokens: request.max_tokens, quote_id: id,
    request_sha256: hash, amount_micro_usdc: amount, expires_at_ms: expires};
}

/** An unsigned quote for a paid job: its fixed price, the jobs merchant and a v3 memo bound to the request. */
export function validateJobQuote(body, header, {job, request, expected, origin, ended}) {
  requireThat(typeof header === 'string' && header.length <= MAX_BYTES, 'Payment-Required encoding');
  const decoded = Buffer.from(header, 'base64');
  requireThat(decoded.toString('base64') === header, 'Payment-Required canonical base64');
  equal(strictJson(new TextDecoder('utf-8', {fatal: true}).decode(decoded)), body, 'Payment-Required header/body');
  const {extensions, resource, ...quote} = body ?? {};
  requireThat(plain(extensions) && Object.keys(extensions).join() === 'bazaar' && plain(extensions.bazaar), 'job quote: Bazaar declaration');
  keys(resource, ['url', 'description', 'mimeType', 'serviceName', 'tags', 'iconUrl'], 'job quote resource');
  requireThat(resource.url === `${origin}/v1/jobs/${job.kind}` && resource.serviceName === 'Covenant Compute'
    && resource.iconUrl === `${origin}${ICON_PATH}`, 'job quote resource');
  const extra = quote.accepts?.[0]?.extra;
  const hash = sha256(request);
  requireThat(integer(extra?.expiresAtMs, 1) && extra.expiresAtMs > ended
    && extra.expiresAtMs <= ended + 90000 + CLOCK_SKEW_MS, 'job quote expiry');
  requireThat(typeof extra?.quoteId === 'string' && /^[0-9a-f-]{36}$/.test(extra.quoteId), 'job quote identity');
  equal(quote, {x402Version: 2, accepts: [{scheme: 'exact', network: NETWORK, asset: ASSET, amount: String(job.amount),
    payTo: job.payTo, maxTimeoutSeconds: 90, extra: {feePayer: expected.feePayer, memo: shortMemo(extra.quoteId, hash),
      quoteId: extra.quoteId, requestSha256: hash, expiresAtMs: extra.expiresAtMs}}]}, 'job quote');
  return {kind: job.kind, quote_id: extra.quoteId, request_sha256: hash, amount_micro_usdc: job.amount, expires_at_ms: extra.expiresAtMs};
}

export async function acceptDiscovery({base, revision, expected, quotes = false, fetchImpl = fetch, now = Date.now}) {
  const origin = baseUrl(base); validateExpected(expected, now());
  requireThat(/^[a-f0-9]{40}$/.test(revision), 'full source revision required');
  requireThat(typeof quotes === 'boolean', 'quotes flag');
  const calls = {get: 0, unsigned_quote_post: 0};
  const http = async (path, method = 'GET', request, id) => {
    // Construct a closed set of headers. Never accept arbitrary auth/payment headers.
    const headers = {accept: 'application/json', 'cache-control': 'no-cache', pragma: 'no-cache'};
    if (method === 'POST') { headers['content-type'] = 'application/json'; headers['idempotency-key'] = id; }
    if (method === 'GET') calls.get++; else calls.unsigned_quote_post++;
    let response;
    try {
      response = await fetchImpl(`${origin}${path}`, {method, headers, redirect: 'error', cache: 'no-store',
        credentials: 'omit', signal: AbortSignal.timeout(15000), ...(request ? {body: JSON.stringify(request)} : {})});
    } catch { fail(`${method} ${path}: network failure (no automatic retry)`); }
    requireThat(response.status === (method === 'GET' ? 200 : 402), `${method} ${path}: unexpected HTTP status ${response.status}`);
    if (path !== '/health') noStore(response);
    requireThat(!response.headers.has('payment-response'), 'unexpected payment result');
    return {body: await readJson(response), paymentRequired: response.headers.get('payment-required')};
  };
  const snapshot = async () => {
    const started = now();
    const health = (await http('/health')).body;
    const manifest = (await http('/.well-known/x402.json')).body;
    const catalog = (await http('/v1/models')).body;
    validateSnapshot({health, manifest, catalog}, {expected, origin, revision, started, ended: now()});
    return {observed_at_ms: manifest.observedAtMs, manifest_sha256: sha256(manifest), catalog_sha256: sha256(catalog)};
  };
  const before = await snapshot(); const checkedQuotes = [];
  if (quotes) {
    const ids = new Set();
    for (const [priced, cap] of [[false, 1], [true, 1], [true, 256], [true, 2048]]) {
      validateExpected(expected, now());
      const id = randomUUID(); requireThat(!ids.has(id), 'duplicate generated quote ID'); ids.add(id);
      const request = {model: expected.model, messages: [{role: 'user', content: 'Discovery quote check; do not submit payment.'}],
        max_tokens: cap, temperature: 0, ...(priced ? {pricing: 'model_tariff_v1'} : {})};
      const response = await http('/v1/chat/completions', 'POST', request, id);
      checkedQuotes.push(validateQuote(response.body, response.paymentRequired,
        {request, id, expected, origin, ended: now()}));
    }
    for (const job of expected.jobs ?? []) {
      const request = JOB_QUOTE_REQUESTS[job.kind];
      const response = await http(`/v1/jobs/${job.kind}`, 'POST', request, randomUUID());
      checkedQuotes.push(validateJobQuote(response.body, response.paymentRequired, {job, request, expected, origin, ended: now()}));
    }
  }
  const after = quotes ? await snapshot() : null;
  return {schema_version: 1, accepted: true, observed_at: new Date(now()).toISOString(), base_url: origin, revision,
    mode: quotes ? 'unsigned_quotes' : 'get_only', calls, wallet_files_read: 0, signatures_created: 0,
    payment_authorizations_sent: 0, before, after, quotes: checkedQuotes,
    limitations: ['Public availability is an observation, not a reservation.',
      'GET responses do not establish unchanged private journals, provider costs or settlement state.',
      ...(quotes ? ['Unsigned quotes create retained unpaid gateway entries; no payment or inference completion is requested.'] : [])]};
}

export function parseArgs(args) {
  const result = {quotes: false};
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (arg === '--quotes') { requireThat(!result.quotes, 'duplicate --quotes'); result.quotes = true; }
    else if (['--base-url', '--revision', '--expected'].includes(arg)) {
      const key = {'--base-url': 'base', '--revision': 'revision', '--expected': 'expectedPath'}[arg];
      requireThat(!Object.hasOwn(result, key) && args[i + 1] && !args[i + 1].startsWith('--'), `invalid ${arg}`);
      result[key] = args[++i];
    } else fail('unknown argument');
  }
  requireThat(result.base && result.revision && result.expectedPath, 'required: --base-url ORIGIN --revision FULL_SHA --expected PUBLIC_POLICY.json [--quotes]');
  return result;
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const options = parseArgs(process.argv.slice(2));
    const expected = strictJson(await readFile(options.expectedPath, 'utf8'));
    console.log(JSON.stringify(await acceptDiscovery({...options, expected}), null, 2));
  } catch (error) {
    // Do not echo response bodies, file contents, configured URLs or arbitrary exceptions.
    console.error(JSON.stringify({accepted: false, error: error instanceof Error && /^(?:JSON |expected |tariff |base URL |full source|quotes flag|health:|manifest|model catalog|response |Payment-Required|payment quote|quote |unexpected |duplicate |invalid --|unknown argument|required:|GET \/|POST \/)/.test(error.message)
      ? error.message : 'acceptance failed; inspect public policy and endpoint configuration'}));
    process.exitCode = 1;
  }
}
