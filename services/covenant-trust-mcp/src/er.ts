// MagicBlock ephemeral-rollup trust facts: which ERs are Covenant-verified
// (Magic Router + SAS registry), a live DCAP check of an ER's TDX enclave, and
// provenance-root verification against the settlement program. All reads and
// pure crypto; enclave endpoints are resolved from the router by validator
// identity, never taken from the caller.
import {Connection, PublicKey} from '@solana/web3.js';
import {createHash, randomBytes} from 'node:crypto';
import {createRequire} from 'node:module';
import fs from 'node:fs';

const require = createRequire(import.meta.url);

const SAS_PROGRAM = new PublicKey('22zoJMtdu4tQc2PzL74ZUT7FrwgB1Udec8DdW4yw4BdG');
const COVENANT_ISSUER = new PublicKey('AdChcSmDKX57rU9qChMJ3MKnqNZbmiQAjuns9VCjzqRb');
const SETTLEMENT_PROGRAM = new PublicKey('cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y');
const MAGIC_ROUTER = process.env.MAGIC_ROUTER ?? 'https://router.magicblock.app';
const PCCS = process.env.PCCS_URL ?? 'https://pccs.phala.network/tdx/certification/v4';
const COLLATERAL_TIMEOUT_MS = 15_000;

// Compile and instantiate the DCAP wasm once. The functions are stateless, so
// a single long-lived instance is correct; a per-request init both wastes a
// compile and races the module-global instance under concurrency. Reset on
// failure so a transient read/compile error does not poison the process.
// (Mirrored in services/x402-seller/src/er-enclave.ts.)
let qvlReady: Promise<typeof import('@phala/dcap-qvl-web')> | undefined;
function loadQvl(): Promise<typeof import('@phala/dcap-qvl-web')> {
  return (qvlReady ??= (async () => {
    const qvl = await import('@phala/dcap-qvl-web');
    await qvl.default({module_or_path: fs.readFileSync(require.resolve('@phala/dcap-qvl-web/dcap-qvl-web_bg.wasm'))});
    return qvl;
  })().catch((e) => { qvlReady = undefined; throw e; }));
}

const rejectAfter = <T>(ms: number, msg: string): Promise<T> =>
  new Promise((_, rej) => setTimeout(() => rej(new Error(msg)), ms));

export interface Route {
  identity: string;
  fqdn: string;
  countryCode?: string;
  baseFee?: number;
  blockTimeMs?: number;
}

export interface ErResolution {
  verified: boolean;
  reason: string;
  status?: string;
  mrTd?: string;
  verifiedAt?: number;
  expiry?: number;
  endpoint?: string;
}

const pda = (seeds: Array<Buffer | Uint8Array>): PublicKey =>
  PublicKey.findProgramAddressSync(seeds, SAS_PROGRAM)[0];

function registryPdas(): {credential: PublicKey; schema: PublicKey} {
  const credential = pda([Buffer.from('credential'), COVENANT_ISSUER.toBuffer(), Buffer.from('covenant')]);
  const schema = pda([Buffer.from('schema'), credential.toBuffer(), Buffer.from('er-verified'), Buffer.from([1])]);
  return {credential, schema};
}

export async function getRoutes(): Promise<Route[]> {
  const res = await fetch(MAGIC_ROUTER, {
    method: 'POST',
    headers: {'content-type': 'application/json'},
    body: JSON.stringify({jsonrpc: '2.0', id: 1, method: 'getRoutes'}),
    signal: AbortSignal.timeout(10_000),
  });
  if (!res.ok) throw new Error(`Magic Router -> ${res.status}`);
  const routes = ((await res.json()) as {result?: Route[]}).result;
  if (!Array.isArray(routes)) throw new Error('Magic Router returned no route list');
  return routes;
}

// Attestation: u8 disc | nonce 32 | credential 32 | schema 32 | u32 len | data |
// signer 32 | expiry i64 LE | token_account 32. er-verified data: verified u8 |
// status str | mr_td vec | verified_at i64 | endpoint str (u32 length prefixes).
function decodeAttestation(buf: Buffer): {verified: boolean; status: string; mrTd: string; verifiedAt: number; endpoint: string; signer: PublicKey; expiry: bigint} {
  let o = 1 + 32 + 32 + 32;
  const dataLen = buf.readUInt32LE(o); o += 4;
  const data = buf.subarray(o, o + dataLen); o += dataLen;
  const signer = new PublicKey(buf.subarray(o, o + 32)); o += 32;
  const expiry = buf.readBigInt64LE(o);

  let d = 0;
  const verified = data[d] === 1; d += 1;
  const statusLen = data.readUInt32LE(d); d += 4;
  const status = data.subarray(d, d + statusLen).toString('utf8'); d += statusLen;
  const mrTdLen = data.readUInt32LE(d); d += 4;
  const mrTd = Buffer.from(data.subarray(d, d + mrTdLen)).toString('hex'); d += mrTdLen;
  const verifiedAt = Number(data.readBigInt64LE(d)); d += 8;
  const endpointLen = data.readUInt32LE(d); d += 4;
  const endpoint = data.subarray(d, d + endpointLen).toString('utf8');
  return {verified, status, mrTd, verifiedAt, endpoint, signer, expiry};
}

function decodeAuthorizedSigners(buf: Buffer): PublicKey[] {
  let o = 1 + 32;
  const nameLen = buf.readUInt32LE(o); o += 4 + nameLen;
  const count = Math.min(buf.readUInt32LE(o), Math.floor((buf.length - (o + 4)) / 32)); o += 4;
  const signers: PublicKey[] = [];
  for (let i = 0; i < count; i++, o += 32) signers.push(new PublicKey(buf.subarray(o, o + 32)));
  return signers;
}

export async function resolveEr(connection: Connection, validator: string): Promise<ErResolution> {
  const {credential, schema} = registryPdas();
  let attestation: PublicKey;
  try {
    attestation = pda([Buffer.from('attestation'), credential.toBuffer(), schema.toBuffer(), new PublicKey(validator).toBuffer()]);
  } catch {
    return {verified: false, reason: 'invalid validator identity'};
  }
  const [attAcct, credAcct] = await connection.getMultipleAccountsInfo([attestation, credential]);
  if (!attAcct || !attAcct.owner.equals(SAS_PROGRAM)) return {verified: false, reason: 'no attestation for this validator'};
  if (!credAcct || !credAcct.owner.equals(SAS_PROGRAM)) return {verified: false, reason: 'issuer credential not found'};
  let att: ReturnType<typeof decodeAttestation>;
  let trustedSigner: boolean;
  try {
    att = decodeAttestation(Buffer.from(attAcct.data));
    trustedSigner = decodeAuthorizedSigners(Buffer.from(credAcct.data)).some((s) => s.equals(att.signer));
  } catch (e) {
    return {verified: false, reason: `malformed attestation account (${e instanceof Error ? e.message : 'decode error'})`};
  }
  const notExpired = att.expiry === 0n || att.expiry > BigInt(Math.floor(Date.now() / 1000));
  const verified = trustedSigner && notExpired && att.verified;
  return {
    verified,
    reason: verified ? 'verified'
      : !trustedSigner ? 'attestation signer is not authorized by the issuer'
      : !notExpired ? 'attestation expired (registry monitor lapsed)'
      : `enclave not verified (${att.status})`,
    status: att.status,
    mrTd: att.mrTd,
    verifiedAt: att.verifiedAt,
    expiry: Number(att.expiry),
    endpoint: att.endpoint,
  };
}

export async function listVerifiedErs(connection: Connection): Promise<Array<Route & {covenant: ErResolution}>> {
  const routes = await getRoutes();
  const out: Array<Route & {covenant: ErResolution}> = [];
  for (const r of routes) out.push({...r, covenant: await resolveEr(connection, r.identity)});
  return out;
}

/** Live TDX check: pull a quote from the ER whose validator identity is given
 *  (endpoint resolved from the Magic Router), DCAP-verify it against the Intel
 *  PCCS, and enforce report_data == our fresh challenge. */
export async function verifyEnclaveLive(validator: string): Promise<{validator: string; endpoint: string; status: string; advisoryIds: string[]; mrTd: string; verifiedAt: number}> {
  const route = (await getRoutes()).find((r) => r.identity === validator);
  if (!route) throw new Error(`validator ${validator} is not served by the Magic Router`);

  const challenge = randomBytes(64);
  const quoteUrl = new URL(`quote?challenge=${encodeURIComponent(challenge.toString('base64'))}`, route.fqdn).toString();
  const qres = await fetch(quoteUrl, {signal: AbortSignal.timeout(15_000)});
  // 404 means no quote endpoint (an open, non-TEE validator); a 5xx is a
  // transient failure of a real one, not proof it lacks an enclave.
  if (qres.status === 404) throw new Error('this ER serves no enclave quote — open, non-TEE validator');
  if (!qres.ok) throw new Error(`quote endpoint returned ${qres.status}`);
  const q = (await qres.json()) as {quote?: string};
  if (!q.quote) throw new Error('this ER serves no enclave quote — open, non-TEE validator');
  const rawQuote = Uint8Array.from(Buffer.from(q.quote, 'base64'));

  const qvl = await loadQvl();
  const collateral = await Promise.race([
    qvl.js_get_collateral(PCCS, rawQuote),
    rejectAfter<Awaited<ReturnType<typeof qvl.js_get_collateral>>>(COLLATERAL_TIMEOUT_MS, 'PCCS collateral fetch timeout'),
  ]);
  const result = qvl.js_verify(rawQuote, collateral, BigInt(Math.floor(Date.now() / 1000)));
  const report = result.report?.TD10 ?? result.report?.TD15;
  if (!report) throw new Error('DCAP result carries no TD report');
  const reportData = String(report.report_data).replace(/^0x/, '').toLowerCase();
  if (reportData !== challenge.toString('hex')) throw new Error('report_data != challenge (stale or replayed quote)');

  return {
    validator,
    endpoint: route.fqdn,
    status: String(result.status),
    advisoryIds: (result.advisory_ids ?? []) as string[],
    mrTd: String(report.mr_td).replace(/^0x/, ''),
    verifiedAt: Math.floor(Date.now() / 1000),
  };
}

const sha256 = (...parts: Buffer[]): Buffer => createHash('sha256').update(Buffer.concat(parts)).digest();

// CreditAccount: 8 disc | owner 32 | balance u64 | bump 1 | provenance_root 32.
const PROVENANCE_OFFSET = 49;

export async function verifyProvenance(connection: Connection, creditsAccount: string, receiptHashes: string[]): Promise<{match: boolean; onChain: string; recomputed: string; actions: number}> {
  const acct = await connection.getAccountInfo(new PublicKey(creditsAccount));
  if (!acct) throw new Error('credit account not found');
  if (!acct.owner.equals(SETTLEMENT_PROGRAM)) throw new Error('account is not owned by the Covenant settlement program');
  if (acct.data.length < PROVENANCE_OFFSET + 32) throw new Error('credit account predates provenance (not migrated)');
  const onChain = Buffer.from(acct.data.subarray(PROVENANCE_OFFSET, PROVENANCE_OFFSET + 32));
  let root = Buffer.alloc(32);
  for (const h of receiptHashes) {
    const buf = Buffer.from(h, 'hex');
    if (buf.length !== 32) throw new Error('receipt hashes must be 32-byte hex');
    root = sha256(root, buf);
  }
  return {match: root.equals(onChain), onChain: onChain.toString('hex'), recomputed: root.toString('hex'), actions: receiptHashes.length};
}
