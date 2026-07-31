// Covenant-signed statement over caller-supplied data. A verifier recomputes
// sha256(canonical(payload)), prepends the domain, and checks the signature
// against the published pubkey. A passing signature authenticates Covenant as
// publisher and detects payload changes; it does not establish that the claim
// is true. Ed25519 is chain-agnostic, so the recipe is independent of where the
// payment settled.

import {
  createPrivateKey,
  createPublicKey,
  createHash,
  randomBytes,
  sign as edSign,
  timingSafeEqual,
  verify as edVerify,
} from "node:crypto";
import bs58 from "bs58";

const DOMAIN = 'covenant.attest.v1\n';

// Published so a consumer can authenticate the publisher and signed bytes:
// pin the pubkey, recompute the digest, and check the signature.
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

  constructor(keypair: readonly number[]) {
    if (
      (keypair.length !== 32 && keypair.length !== 64) ||
      keypair.some((byte) => !Number.isInteger(byte) || byte < 0 || byte > 255)
    ) {
      throw new Error(
        "attestation keypair must be a 32-byte seed or 64-byte seed+pubkey array",
      );
    }
    const seed = Buffer.from(keypair.slice(0, 32));
    this.key = createPrivateKey({
      key: Buffer.concat([PKCS8_ED25519_PREFIX, seed]),
      format: "der",
      type: "pkcs8",
    });
    const jwk = createPublicKey(this.key).export({ format: "jwk" }) as {
      x: string;
    };
    const publicKey = Buffer.from(jwk.x, "base64url");
    if (
      keypair.length === 64 &&
      !timingSafeEqual(Buffer.from(keypair.slice(32)), publicKey)
    ) {
      throw new Error(
        "attestation keypair public half does not match its seed",
      );
    }
    this.pubkeyB58 = bs58.encode(publicKey);
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

export function verifyAttestation(
  att: Attestation,
  expectedPubkeyB58: string,
): boolean {
  try {
    if (
      att.alg !== "ed25519" ||
      att.domain !== ATTEST_DOMAIN ||
      att.canonicalization !== ATTEST_CANONICALIZATION ||
      att.pubkey_b58 !== expectedPubkeyB58
    ) {
      return false;
    }
    const digest = createHash("sha256")
      .update(canonical(att.payload), "utf8")
      .digest("hex");
    if (digest !== att.digest_sha256_hex) return false;
    const pubkey = Buffer.from(bs58.decode(expectedPubkeyB58));
    const signature = Buffer.from(bs58.decode(att.signature_b58));
    if (pubkey.length !== 32 || signature.length !== 64) return false;
    const key = createPublicKey({
      format: "jwk",
      key: { kty: "OKP", crv: "Ed25519", x: b64url(pubkey) },
    });
    return edVerify(
      null,
      Buffer.from(`${ATTEST_DOMAIN}\n${digest}`, "utf8"),
      key,
      signature,
    );
  } catch {
    return false;
  }
}
