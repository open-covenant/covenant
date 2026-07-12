// Covenant-signed attestation over an arbitrary claim. Signs with a dedicated
// ed25519 key, separate from any chain identity, so the public product never
// reaches into internal infra. A verifier recomputes sha256(canonical(payload)),
// prepends the domain, and checks the signature against the published pubkey, so
// no trust in this server is required. ed25519 is chain-agnostic: the same key
// and recipe verify identically whether the payment settled on Solana or Base.

import { createPrivateKey, createPublicKey, createHash, randomBytes, sign as edSign, verify as edVerify } from 'node:crypto';
import bs58 from 'bs58';

const DOMAIN = 'covenant.attest.v1\n';

// Published so any consumer can verify an attestation without trusting this
// server: pin the pubkey, recompute the digest, check the signature.
export const ATTEST_DOMAIN = DOMAIN.trimEnd();
export const ATTEST_CANONICALIZATION = 'JSON, recursively key-sorted, no insignificant whitespace, UTF-8';
export const ATTEST_VERIFY_RECIPE =
  `digest = sha256(canonical(payload)) as lowercase hex; message = "${DOMAIN.trimEnd()}\\n" + digest; ` +
  'ed25519-verify base58-decoded signature_b58 over the UTF-8 message against the published pubkey.';

function canonical(value: unknown): string {
  if (value === null || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  const entries = Object.entries(value as Record<string, unknown>)
    .filter(([, v]) => v !== undefined)
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  return `{${entries.map(([k, v]) => `${JSON.stringify(k)}:${canonical(v)}`).join(',')}}`;
}

const b64url = (b: Buffer | Uint8Array): string => Buffer.from(b).toString('base64url');

// DER prefix for a PKCS#8-wrapped raw Ed25519 seed; concat the 32-byte seed to
// import a private key without trusting the keypair's stored public half.
const PKCS8_ED25519_PREFIX = Buffer.from('302e020100300506032b657004220420', 'hex');

export interface Attestation {
  alg: string;
  domain: string;
  canonicalization: string;
  payload: { subject: string; claim: unknown; ts: number };
  digest_sha256_hex: string;
  pubkey_b58: string;
  signature_b58: string;
}

export class Attestor {
  private readonly key;
  readonly pubkeyB58: string;

  constructor(keypair: number[]) {
    const seed = Buffer.from(keypair.slice(0, 32));
    this.key = createPrivateKey({ key: Buffer.concat([PKCS8_ED25519_PREFIX, seed]), format: 'der', type: 'pkcs8' });
    const jwk = createPublicKey(this.key).export({ format: 'jwk' }) as { x: string };
    this.pubkeyB58 = bs58.encode(Buffer.from(jwk.x, 'base64url'));
  }

  static generate(): Attestor {
    return new Attestor([...randomBytes(32)]);
  }

  attest(subject: string, claim: unknown, ts: number): Attestation {
    const payload = { subject, claim, ts };
    const digest = createHash('sha256').update(canonical(payload), 'utf8').digest('hex');
    const sig = edSign(null, Buffer.from(`${DOMAIN}${digest}`, 'utf8'), this.key);
    return {
      alg: 'ed25519',
      domain: DOMAIN.trimEnd(),
      canonicalization: ATTEST_CANONICALIZATION,
      payload,
      digest_sha256_hex: digest,
      pubkey_b58: this.pubkeyB58,
      signature_b58: bs58.encode(sig),
    };
  }
}

export function verifyAttestation(att: Attestation): boolean {
  const digest = createHash('sha256').update(canonical(att.payload), 'utf8').digest('hex');
  if (digest !== att.digest_sha256_hex) return false;
  const pubkey = Buffer.from(bs58.decode(att.pubkey_b58));
  const key = createPublicKey({ format: 'jwk', key: { kty: 'OKP', crv: 'Ed25519', x: b64url(pubkey) } });
  return edVerify(null, Buffer.from(`${att.domain}\n${digest}`, 'utf8'), key, Buffer.from(bs58.decode(att.signature_b58)));
}
