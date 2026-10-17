import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';
import { verifyEnforcementWitness, verifyRpcEvidence } from './enforcement-lib.mjs';
import { EXPECTED_AUTHORITY_ROOT_PUBLIC_KEY_B64U } from './trust-anchor.mjs';

const args = process.argv.slice(2);
const arg = (name) => {
  const index = args.indexOf(`--${name}`);
  return index >= 0 ? args[index + 1] : undefined;
};
const has = (name) => args.includes(`--${name}`);
const trustRootDirectory = fileURLToPath(
  new URL('../../landing/public/witness/enforcement/trust/', import.meta.url),
);
const SAFE_RUN_ID = /^[a-z0-9][a-z0-9._-]{7,127}$/;
const bundlePath = arg('bundle');
if (!bundlePath) {
  console.error(
    'usage: verify-enforcement.mjs --bundle <bundle.json> [--trust-root <root.json>] [--role-manifest <manifest.json>] [--require-devnet-record] [--rpc <devnet-rpc-url>]',
  );
  process.exit(2);
}

const readJson = (path) => JSON.parse(readFileSync(resolve(path), 'utf8'));
const bundle = readJson(bundlePath);
if (!SAFE_RUN_ID.test(bundle.run_id)) throw new Error('bundle run_id is invalid');
const defaultTrustDirectory = resolve(trustRootDirectory, bundle.run_id);
const trust = {
  authorityRoot: readJson(
    arg('trust-root') || `${defaultTrustDirectory}/authority-root.json`,
  ),
  roleManifest: readJson(
    arg('role-manifest') || `${defaultTrustDirectory}/role-manifest.json`,
  ),
  expectedAuthorityPublicKeyB64u: EXPECTED_AUTHORITY_ROOT_PUBLIC_KEY_B64U,
};

async function rpc(rpcUrl, method, params = []) {
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

const rpcUrl = arg('rpc');
const summary = rpcUrl
  ? await verifyRpcEvidence(bundle, (method, params) => rpc(rpcUrl, method, params), trust)
  : verifyEnforcementWitness(bundle, {
      ...trust,
      requireDevnetRecord: has('require-devnet-record') || has('require-devnet'),
    });
console.log(JSON.stringify(summary, null, 2));
