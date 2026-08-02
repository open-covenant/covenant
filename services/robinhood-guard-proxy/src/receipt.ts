// A signed decision receipt for every governed order, placed or refused. Signed
// ed25519 over sha256 of the canonical payload, base58, with a published verify
// recipe so anyone can check a decision without trusting this proxy. Same shape
// of guarantee as the Covenant x402 attestor; a separate domain and key.
import { createHash, createPrivateKey, createPublicKey, generateKeyPairSync, sign as edSign, verify as edVerify } from "node:crypto";
import bs58 from "bs58";

const DOMAIN = "covenant.robinhood.guard.v1\n";
const PKCS8_ED25519_PREFIX = Buffer.from("302e020100300506032b657004220420", "hex");

export const GUARD_DOMAIN = DOMAIN.trimEnd();
export const GUARD_VERIFY_RECIPE =
  `digest = sha256(canonical(payload)) as lowercase hex; message = "${DOMAIN.trimEnd()}\\n" + digest; ` +
  "ed25519-verify the base58 signature over the UTF-8 message against the published pubkey.";

function canonical(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  const entries = Object.entries(value as Record<string, unknown>)
    .filter(([, v]) => v !== undefined)
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  return `{${entries.map(([k, v]) => `${JSON.stringify(k)}:${canonical(v)}`).join(",")}}`;
}

export type Decision = "executed" | "blocked" | "pending_approval";

export interface DecisionPayload {
  schema: "covenant.robinhood.guard.v1";
  venue: string;
  agent: string;
  decision: Decision;
  reason?: string;
  order: { symbol: string; side: string; type?: string; quantity?: number; notionalUsd?: number };
  ts: number;
}

export interface SignedDecision {
  alg: "ed25519";
  domain: string;
  payload: DecisionPayload;
  digest_sha256_hex: string;
  pubkey_b58: string;
  signature_b58: string;
}

export class GuardAttestor {
  private readonly key;
  readonly pubkeyB58: string;

  constructor(seed: Buffer) {
    this.key = createPrivateKey({ key: Buffer.concat([PKCS8_ED25519_PREFIX, seed]), format: "der", type: "pkcs8" });
    const jwk = createPublicKey(this.key).export({ format: "jwk" }) as { x: string };
    this.pubkeyB58 = bs58.encode(Buffer.from(jwk.x, "base64url"));
  }

  static fromEnv(): GuardAttestor {
    const b64 = process.env.COVENANT_GUARD_ATTESTOR_KEYPAIR?.trim();
    if (b64) return new GuardAttestor(Buffer.from(b64, "base64").subarray(0, 32));
    return GuardAttestor.generate();
  }

  static generate(): GuardAttestor {
    const { privateKey } = generateKeyPairSync("ed25519");
    const der = privateKey.export({ format: "der", type: "pkcs8" }) as Buffer;
    return new GuardAttestor(der.subarray(der.length - 32));
  }

  sign(payload: DecisionPayload): SignedDecision {
    const digest = createHash("sha256").update(canonical(payload), "utf8").digest("hex");
    const sig = edSign(null, Buffer.from(`${DOMAIN}${digest}`, "utf8"), this.key);
    return {
      alg: "ed25519",
      domain: DOMAIN.trimEnd(),
      payload,
      digest_sha256_hex: digest,
      pubkey_b58: this.pubkeyB58,
      signature_b58: bs58.encode(sig),
    };
  }
}

export function verifyDecision(d: SignedDecision): boolean {
  const digest = createHash("sha256").update(canonical(d.payload), "utf8").digest("hex");
  if (digest !== d.digest_sha256_hex) return false;
  try {
    const pubkey = Buffer.from(bs58.decode(d.pubkey_b58));
    const key = createPublicKey({ format: "jwk", key: { kty: "OKP", crv: "Ed25519", x: pubkey.toString("base64url") } });
    return edVerify(null, Buffer.from(`${d.domain}\n${digest}`, "utf8"), key, Buffer.from(bs58.decode(d.signature_b58)));
  } catch {
    return false;
  }
}
