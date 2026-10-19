import { createServer } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import { timingSafeEqual } from 'node:crypto';
import { createKeyPairSignerFromBytes } from '@solana/kit';
import { toFacilitatorSvmSigner } from '@x402/svm';
import { createBridge, Rejected } from './bridge.mjs';
import { readTariffConfig } from './pricing.mjs';

function required(name) { const value = process.env[name]; if (!value) throw new Error(`${name} required`); return value; }
const secret = required('COVENANT_GATEWAY_BRIDGE_SECRET');
const rpcUrl = required('COVENANT_GATEWAY_SOLANA_RPC');
const keyFile = required('COVENANT_GATEWAY_FEE_PAYER_KEY_FILE');
const metadata = await stat(keyFile);
if ((metadata.mode & 0o077) !== 0) throw new Error('fee payer key file must be private');
const bytes = JSON.parse(await readFile(keyFile, 'utf8'));
if (!Array.isArray(bytes) || bytes.length !== 64 || bytes.some(n => !Number.isInteger(n) || n < 0 || n > 255)) throw new Error('invalid key file');
const keypair = await createKeyPairSignerFromBytes(Uint8Array.from(bytes));
bytes.fill(0);
const config = {
  network: required('COVENANT_GATEWAY_NETWORK'), asset: required('COVENANT_GATEWAY_ASSET'),
  payTo: required('COVENANT_GATEWAY_PAY_TO'), feePayer: required('COVENANT_GATEWAY_FEE_PAYER'),
  resourceUrl: required('COVENANT_GATEWAY_RESOURCE_URL'), priceAtomic: required('COVENANT_GATEWAY_PRICE_MICRO_USDC'), secret,
  ...await readTariffConfig(process.env.COVENANT_GATEWAY_TARIFFS_FILE),
};
if (keypair.address !== config.feePayer) throw new Error('fee payer key mismatch');
const signer = toFacilitatorSvmSigner(keypair, { defaultRpcUrl: rpcUrl });
let rpcId = 0;
const rpc = async (method, params) => {
  const response = await fetch(rpcUrl, { method: 'POST', redirect: 'error', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ jsonrpc: '2.0', id: ++rpcId, method, params }), signal: AbortSignal.timeout(15_000) });
  if (!response.ok) throw new Error('RPC unavailable');
  const data = await response.json();
  if (data.error || !Object.hasOwn(data, 'result')) throw new Error('RPC refused');
  return data.result;
};
const bridge = createBridge({ signer, config, rpc });
await bridge.assertNetwork();
const expectedAuth = Buffer.from(`Bearer ${secret}`);
const server = createServer(async (req, res) => {
  const reply = (code, body) => { res.writeHead(code, { 'content-type': 'application/json', 'cache-control': 'no-store' }); res.end(JSON.stringify(body)); };
  const auth = Buffer.from(req.headers.authorization ?? '');
  if (auth.length !== expectedAuth.length || !timingSafeEqual(auth, expectedAuth)) return reply(401, { error: 'unauthorized' });
  if (req.method === 'GET' && req.url === '/health') return reply(200, {status:'ready'});
  if (req.method !== 'POST' || !['/verify', '/settle'].includes(req.url)) return reply(404, { error: 'not found' });
  try {
    let size = 0; const chunks = [];
    for await (const chunk of req) { size += chunk.length; if (size > 32_768) throw new Rejected('request too large'); chunks.push(chunk); }
    const body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    reply(200, await bridge[req.url.slice(1)](body));
  } catch (error) {
    // No transaction payloads, RPC credentials or private keys in logs/errors.
    reply(error instanceof Rejected || error instanceof SyntaxError ? 400 : 503, { error: error instanceof Rejected ? error.message : 'payment bridge unavailable' });
  }
});
server.requestTimeout = 30_000;
server.headersTimeout = 10_000;
server.listen(Number(process.env.COVENANT_GATEWAY_BRIDGE_PORT ?? 4043), '127.0.0.1', () => console.log('Covenant x402 verifier listening on loopback'));
