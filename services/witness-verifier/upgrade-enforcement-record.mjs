import { createPrivateKey } from 'node:crypto';
import {
  readFileSync,
  renameSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  createDevnetExecutionEnvelope,
  sha256Hex,
  verifyEnforcementWitness,
} from './enforcement-lib.mjs';
import { EXPECTED_AUTHORITY_ROOT_PUBLIC_KEY_B64U } from './trust-anchor.mjs';

const args = process.argv.slice(2);
const arg = (name) => {
  const index = args.indexOf(`--${name}`);
  return index >= 0 ? args[index + 1] : undefined;
};
const bundlePath = arg('bundle');
const enforcerKeyPath = arg('enforcer-key');
if (!bundlePath || !enforcerKeyPath) {
  console.error(
    'usage: upgrade-enforcement-record.mjs --bundle <bundle.json> --enforcer-key <pem> [--trust-root <root.json>] [--role-manifest <manifest.json>]',
  );
  process.exit(2);
}

const target = resolve(bundlePath);
const bundle = JSON.parse(readFileSync(target, 'utf8'));
const existing = bundle.w009?.devnet_execution;
if (!existing) throw new Error('bundle has no W009 execution record to upgrade');
if (existing.event?.durable_reservation) {
  throw new Error('W009 execution record already binds durable reservation evidence');
}

const keyTarget = resolve(enforcerKeyPath);
if ((statSync(keyTarget).mode & 0o077) !== 0) {
  throw new Error('enforcer key must not be readable or writable by group or others');
}
const enforcerKey = createPrivateKey(readFileSync(keyTarget, 'utf8'));
const legacyJournal = `${dirname(keyTarget)}/${bundle.run_id}.consumed.json`;
if ((statSync(legacyJournal).mode & 0o077) !== 0) {
  throw new Error('legacy reservation journal permissions are unsafe');
}
const rawJournal = readFileSync(legacyJournal);
const record = JSON.parse(rawJournal.toString('utf8'));
const expectedBytes = Buffer.from(
  `${JSON.stringify({
    run_id: record.run_id,
    consumption_key: record.consumption_key,
    reserved_at: record.reserved_at,
  })}\n`,
  'utf8',
);
if (!rawJournal.equals(expectedBytes)) {
  throw new Error('legacy reservation journal has unexpected bytes or fields');
}
if (
  record.run_id !== bundle.run_id ||
  record.consumption_key !== bundle.w009.grant_consumption.event.consumption_key
) {
  throw new Error('legacy reservation journal does not match the bundle');
}

const defaultTrustDirectory = fileURLToPath(
  new URL('../../landing/public/witness/enforcement/trust/', import.meta.url),
);
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
bundle.w009.devnet_execution = null;
verifyEnforcementWitness(bundle, trust);
bundle.w009.devnet_execution = createDevnetExecutionEnvelope({
  bundle,
  trust,
  enforcerKey,
  transaction: {
    signature: existing.event.transaction_signature,
    wire: Buffer.from(existing.event.wire_transaction_base64, 'base64'),
  },
  slot: existing.event.slot,
  blockTime: existing.event.block_time,
  recordedAt: new Date(Math.floor(Date.now() / 1_000) * 1_000).toISOString(),
  reservationEvidence: {
    scheme: 'legacy_exclusive_file.v0',
    record,
    record_sha256: `sha256:${sha256Hex(rawJournal)}`,
  },
});
verifyEnforcementWitness(bundle, { ...trust, requireDevnetRecord: true });

const temporary = `${target}.tmp`;
writeFileSync(temporary, `${JSON.stringify(bundle, null, 2)}\n`, { mode: 0o644 });
renameSync(temporary, target);
console.log(
  JSON.stringify({
    run_id: bundle.run_id,
    transaction_signature: existing.event.transaction_signature,
    wire_transaction_preserved: true,
    reservation_evidence: 'legacy_exclusive_file.v0',
  }),
);
