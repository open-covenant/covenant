// Verify a Covenant-signed attestation with no trust in any server: recompute
// sha256(canonical(payload)), prepend the domain, check the ed25519 signature
// against the pubkey carried in the attestation. Ported from the x402-seller
// Attestor so the verifier and the signer never drift.

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

function canonical(value: unknown): string {
  if (value === null || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  const entries = Object.entries(value as Record<string, unknown>)
    .filter(([, v]) => v !== undefined)
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  return `{${entries.map(([k, v]) => `${JSON.stringify(k)}:${canonical(v)}`).join(',')}}`;
}

const b64url = (b: Buffer | Uint8Array): string => Buffer.from(b).toString('base64url');

export type VerifyResult =
  | {ok: true; subject: string; signer: string}
  | {ok: false; reason: string};

export function verifyAttestation(att: Attestation): VerifyResult {
  if (!att || typeof att !== 'object') return {ok: false, reason: 'not an attestation object'};
  if (att.alg !== 'ed25519') return {ok: false, reason: `unsupported alg: ${att.alg}`};
  if (!att.payload || !att.pubkey_b58 || !att.signature_b58 || !att.domain) {
    return {ok: false, reason: 'missing payload, domain, pubkey, or signature'};
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
    const key = createPublicKey({format: 'jwk', key: {kty: 'OKP', crv: 'Ed25519', x: b64url(pubkey)}});
    valid = edVerify(null, Buffer.from(`${att.domain}\n${digest}`, 'utf8'), key, Buffer.from(bs58.decode(att.signature_b58)));
  } catch (e) {
    return {ok: false, reason: `signature check failed: ${e instanceof Error ? e.message : 'error'}`};
  }
  if (!valid) return {ok: false, reason: 'signature does not match the signed contents'};
  return {ok: true, subject: att.payload.subject, signer: att.pubkey_b58};
}
