// Live TDX verification of a MagicBlock ephemeral rollup, sold per call.
// Pulls a quote from the validator's router-listed endpoint against a fresh
// 64-byte challenge, DCAP-verifies it against the Intel PCCS, and rejects
// stale or replayed quotes. Optionally binds an agent and its provenance root
// into the challenge (first 32 bytes = sha256(domain || agent || root), last
// 32 = nonce), so the result proves that agent's record came from this
// genuine, attested enclave. Endpoints resolve from the router by validator
// identity, never from the caller.
import { createHash, randomBytes } from 'node:crypto';
import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';
import { PublicKey } from '@solana/web3.js';

const require = createRequire(import.meta.url);

const MAGIC_ROUTER = process.env.MAGIC_ROUTER ?? 'https://router.magicblock.app';
const PCCS = process.env.PCCS_URL ?? 'https://pccs.phala.network/tdx/certification/v4';
const BIND_DOMAIN = 'covenant.tee.bind.v1';

export interface EnclaveResult {
  validator: string;
  endpoint: string;
  tee: 'intel-tdx';
  status: string;
  advisory_ids: string[];
  mr_td: string;
  rt_mr0: string | null;
  rt_mr1: string | null;
  rt_mr2: string | null;
  subject: { agent: string; provenance_root: string } | null;
  challenge_b64: string;
  nonce_b64: string;
  verified_at: number;
}

export class ErEnclaveError extends Error {
  constructor(readonly status: number, message: string) {
    super(message);
  }
}

export async function verifyErEnclave(
  validator: string,
  opts: { agent?: string; provenanceRoot?: string } = {},
): Promise<EnclaveResult> {
  const routerRes = await fetch(MAGIC_ROUTER, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'getRoutes' }),
    signal: AbortSignal.timeout(10_000),
  });
  if (!routerRes.ok) throw new ErEnclaveError(502, 'Magic Router unavailable');
  const routes = ((await routerRes.json()) as { result?: Array<{ identity: string; fqdn: string }> }).result ?? [];
  const route = routes.find((r) => r.identity === validator);
  if (!route) throw new ErEnclaveError(404, 'validator is not served by the Magic Router');

  const nonce = randomBytes(32);
  let binding: Buffer;
  let subject: EnclaveResult['subject'] = null;
  if (opts.agent && opts.provenanceRoot) {
    const root = Buffer.from(opts.provenanceRoot, 'hex');
    if (root.length !== 32) throw new ErEnclaveError(400, 'provenance_root must be 32-byte hex');
    binding = createHash('sha256')
      .update(BIND_DOMAIN)
      .update(new PublicKey(opts.agent).toBytes())
      .update(root)
      .digest();
    subject = { agent: opts.agent, provenance_root: opts.provenanceRoot };
  } else {
    binding = randomBytes(32);
  }
  const challenge = Buffer.concat([binding, nonce]);

  const quoteUrl = new URL(`quote?challenge=${encodeURIComponent(challenge.toString('base64'))}`, route.fqdn).toString();
  const qres = await fetch(quoteUrl, { signal: AbortSignal.timeout(15_000) });
  if (!qres.ok) throw new ErEnclaveError(404, 'this ER serves no enclave quote — open, non-TEE validator');
  const q = (await qres.json()) as { quote?: string };
  if (!q.quote) throw new ErEnclaveError(404, 'this ER serves no enclave quote — open, non-TEE validator');
  const rawQuote = Uint8Array.from(Buffer.from(q.quote, 'base64'));

  const qvl = await import('@phala/dcap-qvl-web');
  await qvl.default({ module_or_path: readFileSync(require.resolve('@phala/dcap-qvl-web/dcap-qvl-web_bg.wasm')) });
  const collateral = await qvl.js_get_collateral(PCCS, rawQuote);
  const result = qvl.js_verify(rawQuote, collateral, BigInt(Math.floor(Date.now() / 1000)));
  const report = result.report?.TD10 ?? result.report?.TD15;
  if (!report) throw new ErEnclaveError(502, 'DCAP result carries no TD report');
  const reportData = String(report.report_data).replace(/^0x/, '').toLowerCase();
  if (reportData !== challenge.toString('hex')) throw new ErEnclaveError(502, 'report_data != challenge (stale or replayed quote)');

  const hex = (v: unknown): string | null => (v == null ? null : String(v).replace(/^0x/, ''));
  return {
    validator,
    endpoint: route.fqdn,
    tee: 'intel-tdx',
    status: String(result.status),
    advisory_ids: (result.advisory_ids ?? []) as string[],
    mr_td: hex(report.mr_td) ?? '',
    rt_mr0: hex(report.rt_mr0),
    rt_mr1: hex(report.rt_mr1),
    rt_mr2: hex(report.rt_mr2),
    subject,
    challenge_b64: challenge.toString('base64'),
    nonce_b64: nonce.toString('base64'),
    verified_at: Math.floor(Date.now() / 1000),
  };
}
