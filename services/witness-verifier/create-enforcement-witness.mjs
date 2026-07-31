import { createPrivateKey } from 'node:crypto';
import { mkdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import {
  actorFor,
  base58Encode,
  createEnforcementWitness,
  createTrustDocuments,
  DEVNET_GENESIS_HASH,
  privateKeyFromSolanaKeypair,
  sha256Hex,
  verifyEnforcementWitness,
} from './enforcement-lib.mjs';

const args = process.argv.slice(2);
const arg = (name) => {
  const index = args.indexOf(`--${name}`);
  return index >= 0 ? args[index + 1] : undefined;
};

const required = [
  'output',
  'trust-root',
  'role-manifest',
  'agent-keypair',
  'authority-key',
  'approver-key',
  'enforcer-key',
  'verifier-key',
];
for (const name of required) {
  if (!arg(name)) {
    console.error(
      'usage: create-enforcement-witness.mjs --output <bundle.json> --trust-root <root.json> --role-manifest <manifest.json> --agent-keypair <solana-keypair.json> --authority-key <pem> --approver-key <pem> --enforcer-key <pem> --verifier-key <pem> [--run-id <id>] [--created-at <whole-second-iso>] [--expires-at <iso>]',
    );
    process.exit(2);
  }
}

function loadSolanaSecret(path) {
  if ((statSync(resolve(path)).mode & 0o077) !== 0) {
    throw new Error('agent keypair must not be readable or writable by group or others');
  }
  const secret = JSON.parse(readFileSync(resolve(path), 'utf8'));
  if (
    !Array.isArray(secret) ||
    secret.length !== 64 ||
    secret.some((byte) => !Number.isInteger(byte) || byte < 0 || byte > 255)
  ) {
    throw new Error('agent keypair must be a 64-byte Solana JSON keypair');
  }
  return Buffer.from(secret);
}

const loadPem = (name) => {
  const path = resolve(arg(`${name}-key`));
  if ((statSync(path).mode & 0o077) !== 0) {
    throw new Error(`${name} key must not be readable or writable by group or others`);
  }
  return createPrivateKey(readFileSync(path, 'utf8'));
};
const agentSecret = loadSolanaSecret(arg('agent-keypair'));
const agentKey = privateKeyFromSolanaKeypair(agentSecret);
const authorityKey = loadPem('authority');
const approverKey = loadPem('approver');
const enforcerKey = loadPem('enforcer');
const verifierKey = loadPem('verifier');
const createdAt =
  arg('created-at') || new Date(Math.floor(Date.now() / 1_000) * 1_000).toISOString();
const expiresAt =
  arg('expires-at') || new Date(Date.parse(createdAt) + 7 * 24 * 60 * 60 * 1_000).toISOString();
const runId =
  arg('run-id') ||
  `w009-w011-${createdAt.slice(0, 10).replaceAll('-', '')}-${sha256Hex(Buffer.from(createdAt)).slice(0, 8)}`;
const feePayer = base58Encode(agentSecret.subarray(32));
const roles = {
  agent: actorFor('agent', agentKey),
  approver: actorFor('approver', approverKey),
  enforcer: actorFor('enforcer', enforcerKey),
  verifier: actorFor('verifier', verifierKey),
};
const { authorityRoot, roleManifest } = createTrustDocuments({
  runId,
  createdAt,
  expiresAt,
  authorityKey,
  roles,
});
const expectedAuthorityPublicKeyB64u = authorityRoot.payload.authority.public_key_b64u;

async function rpc(method, params = []) {
  const rpcUrl = arg('rpc') || 'https://api.devnet.solana.com';
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
const genesisHash = await rpc('getGenesisHash');
if (genesisHash !== DEVNET_GENESIS_HASH) {
  throw new Error('refusing to source a W011 proposal blockhash outside Solana devnet');
}
const latestBlockhash = await rpc('getLatestBlockhash', [{ commitment: 'finalized' }]);

const sourceData =
  'covenant-commit-v1:1682e27f0a4a51c73a9bfe552a6144944dbc75d9:99bb9f889f6104f4305a5c69c3489acbfd6c9c0bd74935b475474fbb09b44e7b:1780856859000';
const bundle = createEnforcementWitness({
  runId,
  createdAt,
  expiresAt,
  feePayer,
  agentKey,
  approverKey,
  enforcerKey,
  verifierKey,
  authorityRoot,
  roleManifest,
  expectedAuthorityPublicKeyB64u,
  source: {
    transaction_signature:
      '2yKCJcWnqJ44KS3nhvovaaB651g7NQfzPxXEGSnrMkTjqfs1n6U9rTNwkqv4BN8nbdEstkbps2kQs2PDhAP5VEGs',
    slot: 467835173,
    block_time: 1780856859,
    confirmation_status: 'finalized',
    instruction_index: 1,
    program_id: 'MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr',
    data_utf8: sourceData,
    data_hash: sha256Hex(Buffer.from(sourceData, 'utf8')),
    wire_transaction_base64:
      'AmKUbcV+zr12BDBTbfsaPBy9NwfkcHy+L1eul9LtQn/nQWgD81ExOm6wcdEEtoZ8u5yX2pzzHi8ziwf9ObdJYwyxqJSTRkdny2Pzue9RWJ9vbr6C0mgySRsafgCJ1Kjyp+TLkaOj2kmMFq5XiLW49Qgw3zP9uENAU4A21btn9mcEAgACBHY/yYUV3gt83oAS6ZEsmwfxlPS+A6+ZuIed841Joqaqjv9SJ1N6SPy5GKRhEr1Y7VdUw60haRTImGioNLo1WLoAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAVKU1qZKSEGTSTocWDaOHx8NbXdvJK7geQfqEBBBUSNtXo6M0icptN3yaDejI10mpl7ToFIj3WyR0IxQSch2voCAgIBAQwCAAAAoIYBAAAAAAADAIoBY292ZW5hbnQtY29tbWl0LXYxOjE2ODJlMjdmMGE0YTUxYzczYTliZmU1NTJhNjE0NDk0NGRiYzc1ZDk6OTliYjlmODg5ZjYxMDRmNDMwNWE1YzY5YzM0ODlhY2JmZDZjOWMwYmQ3NDkzNWI0NzU0NzRmYmIwOWI0NGU3YjoxNzgwODU2ODU5MDAw',
    proposal_recent_blockhash: latestBlockhash.value.blockhash,
  },
});
const trust = { authorityRoot, roleManifest, expectedAuthorityPublicKeyB64u };
const summary = verifyEnforcementWitness(bundle, trust);

for (const [path, value] of [
  [arg('output'), bundle],
  [arg('trust-root'), authorityRoot],
  [arg('role-manifest'), roleManifest],
]) {
  const target = resolve(path);
  mkdirSync(dirname(target), { recursive: true });
  writeFileSync(target, `${JSON.stringify(value, null, 2)}\n`, { mode: 0o644 });
}

console.log(
  JSON.stringify({
    run_id: runId,
    output: arg('output'),
    authority_public_key_b64u: expectedAuthorityPublicKeyB64u,
    ...summary,
  }),
);
