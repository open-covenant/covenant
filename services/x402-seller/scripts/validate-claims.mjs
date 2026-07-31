import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const files = [
  'src/server.ts',
  'src/passport.ts',
  'src/reputation.ts',
  'openapi.json',
  'package.json',
];
const text = files.map((file) => readFileSync(join(root, file), 'utf8')).join('\n');

const forbidden = [
  'identity passport',
  'settlement-grounded reputation',
  'settled jobs',
  'never charged',
  "proving that agent's on-chain record",
  'independently-verifiable statement',
];

const required = [
  'do not prove identity',
  'not reputation',
  'Resource delivery and settlement are separate',
  'not claim truth',
  'are not fetched',
];

const failures = [
  ...forbidden
    .filter((phrase) => text.toLowerCase().includes(phrase.toLowerCase()))
    .map((phrase) => `forbidden claim: ${phrase}`),
  ...required
    .filter((phrase) => !text.includes(phrase))
    .map((phrase) => `missing boundary: ${phrase}`),
];

JSON.parse(readFileSync(join(root, 'openapi.json'), 'utf8'));

if (failures.length > 0) {
  for (const failure of failures) console.error(failure);
  process.exit(1);
}

console.log('public claim boundaries ok');
