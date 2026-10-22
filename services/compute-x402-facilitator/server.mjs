import { createServer } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import { timingSafeEqual } from 'node:crypto';
import { createKeyPairSignerFromBytes } from '@solana/kit';
import { toFacilitatorSvmSigner } from '@x402/svm';
import { HTTPFacilitatorClient } from '@x402/core/server';
import { ExactSvmScheme } from '@x402/svm/exact/facilitator';
import { createBridge, Rejected, SPONSORSHIP_LIMITS } from './bridge.mjs';
import { createRemoteSvmRail } from './remote-svm.mjs';
import { createEvmRail } from './evm.mjs';
import { readTariffConfig } from './pricing.mjs';

function required(name) { const value = process.env[name]; if (!value) throw new Error(`${name} required`); return value; }
// The value may carry a provider key, so errors name the setting, never the URL.
function httpsUrl(name) {
  const value = required(name); let url;
  try { url = new URL(value); } catch { throw new Error(`${name} must be an https URL`); }
  if (url.protocol !== 'https:' || url.username || url.password) throw new Error(`${name} must be an https URL without credentials`);
  return value;
}
const secret = required('COVENANT_GATEWAY_BRIDGE_SECRET');
const rpcUrl = required('COVENANT_GATEWAY_SOLANA_RPC');
const config = {
  network: required('COVENANT_GATEWAY_NETWORK'), asset: required('COVENANT_GATEWAY_ASSET'),
  payTo: required('COVENANT_GATEWAY_PAY_TO'), feePayer: required('COVENANT_GATEWAY_FEE_PAYER'),
  resourceUrl: required('COVENANT_GATEWAY_RESOURCE_URL'), priceAtomic: required('COVENANT_GATEWAY_PRICE_MICRO_USDC'), secret,
  batchEnabled: process.env.COVENANT_GATEWAY_BATCH_ENABLED === 'true',
  ...await readTariffConfig(process.env.COVENANT_GATEWAY_TARIFFS_FILE),
};
let rpcId = 0;
const jsonRpc = (url, timeoutMs = 15_000) => async (method, params) => {
  const response = await fetch(url, { method: 'POST', redirect: 'error', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ jsonrpc: '2.0', id: ++rpcId, method, params }), signal: AbortSignal.timeout(timeoutMs) });
  if (!response.ok) throw new Error('RPC unavailable');
  const data = await response.json();
  if (data.error || !Object.hasOwn(data, 'result')) throw new Error('RPC refused');
  return data.result;
};
const rpc = jsonRpc(rpcUrl);
// Solana settles through an x402 facilitator when one is named, which then
// pays the fees; otherwise this process co-signs with its own fee payer key.
let bridge;
if (process.env.COVENANT_GATEWAY_SOLANA_FACILITATOR_URL) {
  // The official verifier runs here too, under Covenant's own limits. It only
  // simulates, so its signer knows the fee payer's address and nothing more.
  const verifyOnly = { address: config.feePayer, signMessages: async () => { throw new Error('verification never signs'); } };
  bridge = createRemoteSvmRail({ config, rpc,
    verifier: new ExactSvmScheme(toFacilitatorSvmSigner(verifyOnly, { defaultRpcUrl: rpcUrl }), undefined, SPONSORSHIP_LIMITS),
    facilitator: new HTTPFacilitatorClient({ url: httpsUrl('COVENANT_GATEWAY_SOLANA_FACILITATOR_URL'), timeoutMs: 10_000 }),
    log: message => console.log(message) });
} else {
  const keyFile = required('COVENANT_GATEWAY_FEE_PAYER_KEY_FILE');
  const metadata = await stat(keyFile);
  if ((metadata.mode & 0o077) !== 0) throw new Error('fee payer key file must be private');
  const bytes = JSON.parse(await readFile(keyFile, 'utf8'));
  if (!Array.isArray(bytes) || bytes.length !== 64 || bytes.some(n => !Number.isInteger(n) || n < 0 || n > 255)) throw new Error('invalid key file');
  const keypair = await createKeyPairSignerFromBytes(Uint8Array.from(bytes));
  bytes.fill(0);
  if (keypair.address !== config.feePayer) throw new Error('fee payer key mismatch');
  bridge = createBridge({ signer: toFacilitatorSvmSigner(keypair, { defaultRpcUrl: rpcUrl }), config, rpc });
}
await bridge.assertNetwork();
// Optional USDC on Base once a Base merchant is set: CDP credentials for
// Coinbase's facilitator, or any other by URL. The chain and facilitator are
// first checked on readiness, so a Base outage at boot leaves Solana running.
let evm = null;
if (process.env.COVENANT_GATEWAY_BASE_PAY_TO) {
  const byCdp = Boolean(process.env.CDP_API_KEY_ID || process.env.CDP_API_KEY_SECRET);
  const byUrl = Boolean(process.env.COVENANT_GATEWAY_BASE_FACILITATOR_URL);
  if (byCdp && byUrl) throw new Error('Base takes CDP_API_KEY_ID and CDP_API_KEY_SECRET or COVENANT_GATEWAY_BASE_FACILITATOR_URL, not both');
  if (!byCdp && !byUrl) throw new Error('Base requires CDP_API_KEY_ID and CDP_API_KEY_SECRET, or COVENANT_GATEWAY_BASE_FACILITATOR_URL');
  // Imported only when used: the CDP SDK stays out of a Solana-only verifier.
  const facilitatorConfig = byCdp
    ? (await import('@coinbase/x402')).createFacilitatorConfig(required('CDP_API_KEY_ID'), required('CDP_API_KEY_SECRET'))
    : { url: httpsUrl('COVENANT_GATEWAY_BASE_FACILITATOR_URL') };
  // Verification makes three Base RPC calls and one facilitator call inside
  // the gateway's 30-second request budget.
  evm = createEvmRail({ config: { ...config, network: required('COVENANT_GATEWAY_BASE_NETWORK'),
    asset: required('COVENANT_GATEWAY_BASE_ASSET'), payTo: required('COVENANT_GATEWAY_BASE_PAY_TO') },
    facilitator: new HTTPFacilitatorClient({ ...facilitatorConfig, timeoutMs: 10_000 }),
    rpc: jsonRpc(httpsUrl('COVENANT_GATEWAY_BASE_RPC'), 5_000), log: message => console.log(message) });
}
const expectedAuth = Buffer.from(`Bearer ${secret}`);
const server = createServer(async (req, res) => {
  const reply = (code, body) => { res.writeHead(code, { 'content-type': 'application/json', 'cache-control': 'no-store' }); res.end(JSON.stringify(body)); };
  const auth = Buffer.from(req.headers.authorization ?? '');
  if (auth.length !== expectedAuth.length || !timingSafeEqual(auth, expectedAuth)) return reply(401, { error: 'unauthorized' });
  if (req.method === 'GET' && req.url === '/health') return reply(200, {status:'ready'});
  // Base routes carry an /evm prefix and exist only when Base is configured.
  const rail = req.url?.startsWith('/evm/') ? evm : bridge;
  const route = req.url?.startsWith('/evm/') ? req.url.slice(4) : req.url;
  if (!rail) return reply(404, { error: 'not found' });
  if (req.method === 'GET' && route === '/readiness') {
    try { return reply(200, await rail.readiness()); }
    catch { return reply(503, { error: rail === evm ? 'Base payments unavailable' : 'payment pricing unavailable' }); }
  }
  if (req.method !== 'POST' || !['/verify', '/settle'].includes(route)) return reply(404, { error: 'not found' });
  try {
    let size = 0; const chunks = [];
    // A 32-child manifest appears in both accepted terms and sealed evidence.
    // Bound the entire private request while allowing worst-case JSON escaping.
    for await (const chunk of req) { size += chunk.length; if (size > 131_072) throw new Rejected('request too large'); chunks.push(chunk); }
    const body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    if (typeof body !== 'object' || body === null || Array.isArray(body)) throw new Rejected('invalid request');
    reply(200, await rail[route.slice(1)](body));
  } catch (error) {
    // No transaction payloads, RPC credentials or private keys in logs/errors.
    if (error instanceof Rejected || error instanceof SyntaxError) return reply(400, { error: error instanceof Rejected ? error.message : 'invalid JSON' });
    reply(503, { error: 'payment bridge unavailable' });
  }
});
server.requestTimeout = 30_000;
server.headersTimeout = 10_000;
const port = Number(process.env.COVENANT_GATEWAY_BRIDGE_PORT ?? 4043);
server.listen(port, '127.0.0.1', () => console.log(`Covenant x402 verifier listening on loopback port ${port}; Solana ${process.env.COVENANT_GATEWAY_SOLANA_FACILITATOR_URL ? 'through a facilitator' : 'self-settled'}; Base ${evm ? `on ${evm.network}` : 'off'}`));
