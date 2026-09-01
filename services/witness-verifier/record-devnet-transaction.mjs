import { createPrivateKey } from 'node:crypto';
import {
  readFileSync,
  renameSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';
import {
  createDevnetExecutionEnvelope,
  DEVNET_GENESIS_HASH,
  executeAuthorizedW009,
  verifyEnforcementWitness,
} from './enforcement-lib.mjs';
import { EXPECTED_AUTHORITY_ROOT_PUBLIC_KEY_B64U } from './trust-anchor.mjs';

const args = process.argv.slice(2);
const arg = (name) => {
  const index = args.indexOf(`--${name}`);
  return index >= 0 ? args[index + 1] : undefined;
};
const bundlePath = arg('bundle');
const keypairPath = arg('keypair');
const enforcerKeyPath = arg('enforcer-key');
const rpcUrl = arg('rpc') || 'https://api.devnet.solana.com';
if (!bundlePath || !keypairPath || !enforcerKeyPath) {
  console.error(
    'usage: record-devnet-transaction.mjs --bundle <bundle.json> --keypair <solana-keypair.json> --enforcer-key <pem> [--trust-root <root.json>] [--role-manifest <manifest.json>] [--rpc <devnet-rpc-url>]',
  );
  process.exit(2);
}

async function rpc(method, params = []) {
  const response = await fetch(rpcUrl, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
  });
  if (!response.ok) throw new Error(`Solana RPC returned HTTP ${response.status}`);
  const payload = await response.json();
  if (payload.error) throw new Error(`Solana RPC ${method} failed: ${JSON.stringify(payload.error)}`);
  return payload.result;
}

const target = resolve(bundlePath);
const bundle = JSON.parse(readFileSync(target, 'utf8'));
if (!/^[a-z0-9][a-z0-9._-]{7,127}$/.test(bundle.run_id)) {
  throw new Error('bundle run_id is invalid');
}
const trustRootDirectory = fileURLToPath(
  new URL('../../landing/public/witness/enforcement/trust/', import.meta.url),
);
const defaultTrustDirectory = resolve(trustRootDirectory, bundle.run_id);
const trust = {
  authorityRoot: JSON.parse(
    readFileSync(
      resolve(arg('trust-root') || `${defaultTrustDirectory}/authority-root.json`),
      'utf8',
    ),
  ),
  roleManifest: JSON.parse(
    readFileSync(
      resolve(arg('role-manifest') || `${defaultTrustDirectory}/role-manifest.json`),
      'utf8',
    ),
  ),
  expectedAuthorityPublicKeyB64u: EXPECTED_AUTHORITY_ROOT_PUBLIC_KEY_B64U,
};
verifyEnforcementWitness(bundle, trust);
if (bundle.w009.devnet_execution) throw new Error('bundle already has a devnet execution');
if (Date.now() > Date.parse(bundle.w009.proposal.event.scope.expires_at)) {
  throw new Error('W009 approval has expired; generate a fresh run');
}

for (const path of [keypairPath, enforcerKeyPath]) {
  if ((statSync(resolve(path)).mode & 0o077) !== 0) {
    throw new Error(`${path} must not be readable or writable by group or others`);
  }
}
const secret = JSON.parse(readFileSync(resolve(keypairPath), 'utf8'));
if (
  !Array.isArray(secret) ||
  secret.length !== 64 ||
  secret.some((byte) => !Number.isInteger(byte) || byte < 0 || byte > 255)
) {
  throw new Error('keypair must be a 64-byte Solana JSON keypair');
}
const enforcerKey = createPrivateKey(readFileSync(resolve(enforcerKeyPath), 'utf8'));

const genesisHash = await rpc('getGenesisHash');
if (genesisHash !== DEVNET_GENESIS_HASH) throw new Error('refusing to submit outside Solana devnet');
const latest = await rpc('getLatestBlockhash', [{ commitment: 'finalized' }]);
const execution = await executeAuthorizedW009({
  bundle,
  trust,
  secretKey: secret,
  recentBlockhash: latest.value.blockhash,
  submit: async (transaction) => {
    const signature = await rpc('sendTransaction', [
      transaction.wire.toString('base64'),
      { encoding: 'base64', preflightCommitment: 'finalized', skipPreflight: false },
    ]);
    if (signature !== transaction.signature) {
      throw new Error('RPC returned an unexpected transaction signature');
    }
    return signature;
  },
});

let status = null;
for (let attempt = 0; attempt < 90; attempt += 1) {
  const statuses = await rpc('getSignatureStatuses', [
    [execution.transaction.signature],
    { searchTransactionHistory: true },
  ]);
  status = statuses.value[0];
  if (status?.err) throw new Error(`devnet transaction failed: ${JSON.stringify(status.err)}`);
  if (status?.confirmationStatus === 'finalized') break;
  await new Promise((resolvePromise) => setTimeout(resolvePromise, 1_000));
}
if (status?.confirmationStatus !== 'finalized') {
  throw new Error('devnet transaction finalization timed out');
}

let chainRecord = null;
for (let attempt = 0; attempt < 30; attempt += 1) {
  chainRecord = await rpc('getTransaction', [
    execution.transaction.signature,
    { encoding: 'base64', commitment: 'finalized', maxSupportedTransactionVersion: 0 },
  ]);
  if (chainRecord) break;
  await new Promise((resolvePromise) => setTimeout(resolvePromise, 1_000));
}
if (!chainRecord || chainRecord.meta?.err !== null) {
  throw new Error('finalized devnet transaction is unavailable or failed');
}
if (chainRecord.transaction?.[0] !== execution.transaction.wire.toString('base64')) {
  throw new Error('RPC wire transaction differs from submitted bytes');
}
if (!Number.isSafeInteger(chainRecord.blockTime)) {
  throw new Error('finalized devnet transaction has no block time');
}

bundle.w009.devnet_execution = createDevnetExecutionEnvelope({
  bundle,
  trust,
  enforcerKey,
  transaction: execution.transaction,
  slot: chainRecord.slot,
  blockTime: chainRecord.blockTime,
  recordedAt: new Date(Math.floor(Date.now() / 1_000) * 1_000).toISOString(),
  reservationEvidence: execution.reservationEvidence,
});
verifyEnforcementWitness(bundle, { ...trust, requireDevnetRecord: true });

const temporary = `${target}.tmp`;
writeFileSync(temporary, `${JSON.stringify(bundle, null, 2)}\n`, { mode: 0o644 });
renameSync(temporary, target);
console.log(
  JSON.stringify({
    run_id: bundle.run_id,
    transaction_signature: execution.transaction.signature,
    slot: chainRecord.slot,
    block_time: chainRecord.blockTime,
    confirmation_status: 'finalized',
    one_use_journal: 'written',
  }),
);
