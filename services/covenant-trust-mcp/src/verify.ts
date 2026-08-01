// Verify the cryptographic integrity of a Covenant attestation envelope.
// A valid signature proves that the carried signer authored the payload. It
// does not make that signer a trusted Covenant authority unless the caller
// supplies the expected signer independently.

import { createPublicKey, createHash, verify as edVerify } from 'node:crypto';
import bs58 from 'bs58';

export interface Attestation {
  alg: string;
  domain: string;
  canonicalization: string;
  payload: { subject: string; claim: unknown; ts: number };
  digest_sha256_hex: string;
  pubkey_b58: string;
  signature_b58: string;
}

const DOMAIN = 'covenant.attest.v1';
const CANONICALIZATION = 'JSON, recursively key-sorted, no insignificant whitespace, UTF-8';

export function canonical(value: unknown): string {
  if (
    value === null ||
    typeof value === 'string' ||
    typeof value === 'boolean' ||
    (typeof value === 'number' && Number.isFinite(value))
  ) {
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  if (typeof value !== 'object') throw new Error('value is not JSON');
  const entries = Object.entries(value as Record<string, unknown>)
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  if (entries.some(([, current]) => current === undefined)) {
    throw new Error('undefined is not valid JSON');
  }
  return `{${entries.map(([k, v]) => `${JSON.stringify(k)}:${canonical(v)}`).join(',')}}`;
}

const b64url = (b: Buffer | Uint8Array): string => Buffer.from(b).toString('base64url');

export type VerifyResult =
  | {
      ok: true;
      subject: string;
      signer: string;
      signatureValid: true;
      signerMatches: boolean | null;
    }
  | {ok: false; reason: string};

export function verifyAttestation(att: Attestation, expectedSigner?: string): VerifyResult {
  if (!att || typeof att !== 'object') return {ok: false, reason: 'not an attestation object'};
  if (att.alg !== 'ed25519') return {ok: false, reason: `unsupported alg: ${att.alg}`};
  if (att.domain !== DOMAIN) return {ok: false, reason: `unsupported domain: ${att.domain}`};
  if (att.canonicalization !== CANONICALIZATION) {
    return {ok: false, reason: 'unsupported canonicalization'};
  }
  if (!att.payload || !att.pubkey_b58 || !att.signature_b58 || !att.domain) {
    return {ok: false, reason: 'missing payload, domain, pubkey, or signature'};
  }
  if (
    typeof att.payload !== 'object' ||
    Array.isArray(att.payload) ||
    typeof att.payload.subject !== 'string' ||
    att.payload.subject.length === 0 ||
    !Object.hasOwn(att.payload, 'claim') ||
    !Number.isSafeInteger(att.payload.ts) ||
    att.payload.ts < 0
  ) {
    return {ok: false, reason: 'payload requires a non-empty subject, claim, and non-negative integer ts'};
  }
  if (!/^[0-9a-f]{64}$/.test(att.digest_sha256_hex)) {
    return {ok: false, reason: 'digest is not 32-byte lowercase hex'};
  }
  let digest: string;
  try {
    digest = createHash('sha256').update(canonical(att.payload), 'utf8').digest('hex');
  } catch {
    return {ok: false, reason: 'payload is not canonicalizable'};
  }
  if (digest !== att.digest_sha256_hex) {
    return {ok: false, reason: 'digest does not match payload (tampered)'};
  }
  let valid: boolean;
  try {
    const pubkey = Buffer.from(bs58.decode(att.pubkey_b58));
    if (pubkey.length !== 32) return {ok: false, reason: 'signer public key is not 32 bytes'};
    const signature = Buffer.from(bs58.decode(att.signature_b58));
    if (signature.length !== 64) return {ok: false, reason: 'signature is not 64 bytes'};
    const key = createPublicKey({format: 'jwk', key: {kty: 'OKP', crv: 'Ed25519', x: b64url(pubkey)}});
    valid = edVerify(null, Buffer.from(`${att.domain}\n${digest}`, 'utf8'), key, signature);
  } catch (e) {
    return {ok: false, reason: `signature check failed: ${e instanceof Error ? e.message : 'error'}`};
  }
  if (!valid) return {ok: false, reason: 'signature does not match the signed contents'};
  return {
    ok: true,
    subject: att.payload.subject,
    signer: att.pubkey_b58,
    signatureValid: true,
    signerMatches: expectedSigner !== undefined ? att.pubkey_b58 === expectedSigner : null,
  };
}
