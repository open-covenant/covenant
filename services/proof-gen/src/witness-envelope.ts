import { createCipheriv, createDecipheriv, randomBytes } from 'node:crypto';

// Envelope encryption for the per-job witness key.
//
// Threat closed: an attacker with Redis-only access cannot decrypt the
// witness ciphertext. The witness AES key is wrapped under a long-lived
// 32-byte secret held in PROOFGEN_WITNESS_WRAP_KEY env (base64). Redis
// stores only the wrapped key; the unwrap secret lives outside Redis.
// Both Redis and the env must be compromised to recover plaintext.
//
// Wrap format (base64): nonce[12] || ciphertext[32] || gcm_tag[16] = 60 bytes.

const NONCE_LEN = 12;
const TAG_LEN = 16;
const WITNESS_KEY_LEN = 32;
const WRAP_KEY_LEN = 32;

let cachedWrapKey: Buffer | null = null;

function readWrapKey(): Buffer {
  const raw = process.env.PROOFGEN_WITNESS_WRAP_KEY;
  if (!raw) {
    throw new Error(
      'PROOFGEN_WITNESS_WRAP_KEY env var required (32-byte base64); witness envelope refuses to operate without it',
    );
  }
  const buf = Buffer.from(raw, 'base64');
  if (buf.length !== WRAP_KEY_LEN) {
    throw new Error(
      `PROOFGEN_WITNESS_WRAP_KEY must decode to ${WRAP_KEY_LEN} bytes (got ${buf.length})`,
    );
  }
  return buf;
}

function wrapKey(): Buffer {
  if (cachedWrapKey) return cachedWrapKey;
  cachedWrapKey = readWrapKey();
  return cachedWrapKey;
}

// Exposed for tests + boot-time validation. Throws if the env is unset
// or malformed; callers in main() should invoke this at startup so a
// misconfigured deploy crashes loudly instead of failing on the first
// /prove request.
export function ensureWitnessEnvelopeReady(): void {
  wrapKey();
}

export function generateWitnessKey(): Buffer {
  return randomBytes(WITNESS_KEY_LEN);
}

export function wrapWitnessKey(witnessKey: Buffer): string {
  if (witnessKey.length !== WITNESS_KEY_LEN) {
    throw new Error(`witness key must be ${WITNESS_KEY_LEN} bytes`);
  }
  const nonce = randomBytes(NONCE_LEN);
  const cipher = createCipheriv('aes-256-gcm', wrapKey(), nonce);
  const ct = Buffer.concat([cipher.update(witnessKey), cipher.final()]);
  const tag = cipher.getAuthTag();
  return Buffer.concat([nonce, ct, tag]).toString('base64');
}

export function unwrapWitnessKey(wrapped: string): Buffer {
  const buf = Buffer.from(wrapped, 'base64');
  if (buf.length !== NONCE_LEN + WITNESS_KEY_LEN + TAG_LEN) {
    throw new Error('wrapped witness key has unexpected length');
  }
  const nonce = buf.subarray(0, NONCE_LEN);
  const ct = buf.subarray(NONCE_LEN, NONCE_LEN + WITNESS_KEY_LEN);
  const tag = buf.subarray(NONCE_LEN + WITNESS_KEY_LEN);
  const decipher = createDecipheriv('aes-256-gcm', wrapKey(), nonce);
  decipher.setAuthTag(tag);
  return Buffer.concat([decipher.update(ct), decipher.final()]);
}

// Test-only — drop the cached key so a follow-up call re-reads the env.
// Not exported from the package index; tests reach for the module path
// directly.
export function __resetWitnessEnvelopeCacheForTests(): void {
  cachedWrapKey = null;
}
