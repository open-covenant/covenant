import { generateKeyPairSync } from 'node:crypto';
import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { actorFor } from './enforcement-lib.mjs';

const args = process.argv.slice(2);
const index = args.indexOf('--output-dir');
const outputDir = index >= 0 ? args[index + 1] : undefined;
if (!outputDir) {
  console.error('usage: generate-enforcement-keys.mjs --output-dir <private-directory>');
  process.exit(2);
}

const directory = resolve(outputDir);
mkdirSync(directory, { recursive: true, mode: 0o700 });
const publicActors = {};
for (const role of ['authority', 'approver', 'enforcer', 'verifier']) {
  const { privateKey } = generateKeyPairSync('ed25519');
  const target = `${directory}/${role}.pem`;
  writeFileSync(target, privateKey.export({ type: 'pkcs8', format: 'pem' }), {
    flag: 'wx',
    mode: 0o600,
  });
  publicActors[role] = actorFor(role === 'authority' ? 'authority_root' : role, privateKey);
}
console.log(
  JSON.stringify({
    output_dir: 'configured',
    authority_public_key_b64u: publicActors.authority.public_key_b64u,
    key_ids: Object.fromEntries(
      Object.entries(publicActors).map(([role, actor]) => [role, actor.key_id]),
    ),
  }),
);
