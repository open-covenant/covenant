import { createServer } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import { timingSafeEqual } from 'node:crypto';
import { createKeyPairSignerFromBytes } from '@solana/kit';
import { toFacilitatorSvmSigner } from '@x402/svm';
import { HTTPFacilitatorClient } from '@x402/core/server';
import { createBridge, JOB_KINDS, Rejected } from './bridge.mjs';
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
async function readKeypair(name) {
  const file = required(name);
  const metadata = await stat(file);
  if ((metadata.mode & 0o077) !== 0) throw new Error(`${name} must be private`);
  // A parse error would quote the file, so it is replaced with a plain one.
  let bytes;
  try { bytes = JSON.parse(await readFile(file, 'utf8')); } catch { throw new Error(`${name} is not a keypair`); }
  if (!Array.isArray(bytes) || bytes.length !== 64 || bytes.some(n => !Number.isInteger(n) || n < 0 || n > 255)) throw new Error(`${name} is not a keypair`);
  const keypair = await createKeyPairSignerFromBytes(Uint8Array.from(bytes));
  bytes.fill(0);
  return keypair;
}
const keypair = await readKeypair('COVENANT_GATEWAY_FEE_PAYER_KEY_FILE');
const config = {
  network: required('COVENANT_GATEWAY_NETWORK'), asset: required('COVENANT_GATEWAY_ASSET'),
  payTo: required('COVENANT_GATEWAY_PAY_TO'), feePayer: required('COVENANT_GATEWAY_FEE_PAYER'),
  resourceUrl: required('COVENANT_GATEWAY_RESOURCE_URL'), priceAtomic: required('COVENANT_GATEWAY_PRICE_MICRO_USDC'), secret,
  batchEnabled: process.env.COVENANT_GATEWAY_BATCH_ENABLED === 'true',
  ...await readTariffConfig(process.env.COVENANT_GATEWAY_TARIFFS_FILE),
};
if (keypair.address !== config.feePayer) throw new Error('fee payer key mismatch');
const signer = toFacilitatorSvmSigner(keypair, { defaultRpcUrl: rpcUrl });
let rpcId = 0;
const jsonRpc = (url, timeoutMs = 15_000) => async (method, params) => {
  const response = await fetch(url, { method: 'POST', redirect: 'error', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ jsonrpc: '2.0', id: ++rpcId, method, params }), signal: AbortSignal.timeout(timeoutMs) });
  if (!response.ok) throw new Error('RPC unavailable');
  const data = await response.json();
  if (data.error || !Object.hasOwn(data, 'result')) throw new Error('RPC refused');
  return data.result;
};
const rpc = jsonRpc(rpcUrl);
// Optionally an x402 facilitator holding the same fee payer key broadcasts
// Solana payments; this process still signs and confirms them.
const solanaFacilitator = process.env.COVENANT_GATEWAY_SOLANA_FACILITATOR_URL
  ? new HTTPFacilitatorClient({ url: httpsUrl('COVENANT_GATEWAY_SOLANA_FACILITATOR_URL'), timeoutMs: 5_000 })
  : undefined;
// A facilitator that does not list this fee payer declines every payment, and
// each one is then sent directly; say so once rather than per payment.
solanaFacilitator?.getSupported().then(supported => {
  const listed = supported?.kinds?.some(kind => kind.network === config.network && kind.extra?.feePayer === config.feePayer)
    || supported?.signers?.['solana:*']?.includes(config.feePayer);
  if (!listed) console.log('Solana facilitator does not list this fee payer; payments will be sent directly');
}).catch(() => console.log('Solana facilitator unreachable at startup; payments are sent directly until it answers'));
const bridge = createBridge({ signer, config, rpc, facilitator: solanaFacilitator, log: message => console.log(message) });
await bridge.assertNetwork();
// Paid jobs settle to their own merchant wallet, which refunds a failed job.
// Prices are fixed per job kind; an unset price leaves that kind unsold.
let jobsRail = null;
if (process.env.COVENANT_GATEWAY_JOBS_PAY_TO) {
  // A shared merchant would let a served inference payment be refunded as a job.
  if (process.env.COVENANT_GATEWAY_JOBS_PAY_TO === config.payTo) throw new Error('paid jobs need their own merchant wallet');
  const jobPrices = Object.fromEntries(JOB_KINDS
    .filter(kind => process.env[`COVENANT_GATEWAY_JOBS_${kind.toUpperCase()}_PRICE_MICRO_USDC`])
    .map(kind => [new URL(`/v1/jobs/${kind}`, config.resourceUrl).href, process.env[`COVENANT_GATEWAY_JOBS_${kind.toUpperCase()}_PRICE_MICRO_USDC`]]));
  const merchant = await readKeypair('COVENANT_GATEWAY_JOBS_MERCHANT_KEY_FILE');
  jobsRail = createBridge({ signer, config: { ...config, payTo: required('COVENANT_GATEWAY_JOBS_PAY_TO'), jobPrices }, rpc,
    facilitator: solanaFacilitator, refundSigners: { feePayer: keypair, merchant }, log: message => console.log(`jobs: ${message}`) });
}
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
  // Base routes carry an /evm prefix and paid-job routes a /jobs prefix; each
  // exists only when configured.
  const prefix = ['/evm', '/jobs'].find(p => req.url?.startsWith(`${p}/`));
  const rail = { '/evm': evm, '/jobs': jobsRail }[prefix] ?? (prefix ? null : bridge);
  const route = prefix ? req.url.slice(prefix.length) : req.url;
  if (!rail) return reply(404, { error: 'not found' });
  if (req.method === 'GET' && route === '/readiness') {
    try { return reply(200, await rail.readiness()); }
    catch { return reply(503, { error: rail === evm ? 'Base payments unavailable' : 'payment pricing unavailable' }); }
  }
  const method = { '/verify': 'verify', '/settle': 'settle', '/refund': 'refund', '/refund-status': 'refundStatus' }[route];
  if (req.method !== 'POST' || !method || !rail[method]) return reply(404, { error: 'not found' });
  try {
    let size = 0; const chunks = [];
    // A 32-child manifest appears in both accepted terms and sealed evidence.
    // Bound the entire private request while allowing worst-case JSON escaping.
    for await (const chunk of req) { size += chunk.length; if (size > 131_072) throw new Rejected('request too large'); chunks.push(chunk); }
    const body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    if (typeof body !== 'object' || body === null || Array.isArray(body)) throw new Rejected('invalid request');
    reply(200, await rail[method](body));
  } catch (error) {
    // No transaction payloads, RPC credentials or private keys in logs/errors.
    if (error instanceof Rejected || error instanceof SyntaxError) return reply(400, { error: error instanceof Rejected ? error.message : 'invalid JSON' });
    reply(503, { error: 'payment bridge unavailable' });
  }
});
server.requestTimeout = 30_000;
server.headersTimeout = 10_000;
const port = Number(process.env.COVENANT_GATEWAY_BRIDGE_PORT ?? 4043);
server.listen(port, '127.0.0.1', () => console.log(`Covenant x402 verifier listening on loopback port ${port}; Solana broadcast ${solanaFacilitator ? 'through a facilitator' : 'direct'}; Base ${evm ? `on ${evm.network}` : 'off'}; paid jobs ${jobsRail ? 'on' : 'off'}`));
