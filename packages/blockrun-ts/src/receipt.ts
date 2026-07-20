/**
 * The Covenant receipt for a BlockRun paid call. Field names and hashing match
 * the `covenant-blockrun` Rust crate exactly, so a receipt built here verifies
 * in the daemon and vice versa.
 */
import { canonicalize } from "./jcs.js";

export const PROVIDER = "blockrun";

export const VERDICT_DELIVERED = "delivered";
export const VERDICT_SUBSTITUTED = "substituted";
export const VERDICT_UNVERIFIED = "unverified";
export type Verdict =
  | typeof VERDICT_DELIVERED
  | typeof VERDICT_SUBSTITUTED
  | typeof VERDICT_UNVERIFIED;

export interface PaymentInfo {
  network: string;
  asset: string;
  amount: string;
  amountUsdc: number;
  payTo: string;
  /** On-chain settlement signature, from the `x-payment-receipt` header. */
  tx?: string;
}

export interface RoutingClaim {
  profile?: string;
  model?: string;
  savingsPct?: number;
}

export interface CallReceipt {
  provider: string;
  endpoint: string;
  modelRequested: string;
  modelServed: string;
  verdict: Verdict;
  inputSha256: string;
  outputSha256: string;
  routing?: RoutingClaim;
  payment: PaymentInfo;
}

/** SHA-256 of the JCS-canonical form, lowercase hex. */
export async function canonicalSha256Hex(value: unknown): Promise<string> {
  const bytes = new TextEncoder().encode(canonicalize(value));
  const digest = await sha256(bytes);
  return hex(digest);
}

export function verdictFor(requested: string, served: string): Verdict {
  if (!served) return VERDICT_UNVERIFIED;
  return modelBase(requested) === modelBase(served)
    ? VERDICT_DELIVERED
    : VERDICT_SUBSTITUTED;
}

/** Model name without a provider namespace: `openai/gpt-4o-mini` → `gpt-4o-mini`. */
function modelBase(model: string): string {
  const parts = model.toLowerCase().split("/");
  return parts[parts.length - 1];
}

/** Build a receipt from one exchange. */
export async function buildReceipt(args: {
  endpoint: string;
  request: unknown;
  response: unknown;
  payment: PaymentInfo;
  routing?: RoutingClaim;
}): Promise<CallReceipt> {
  const { endpoint, request, response, payment } = args;
  const routing = args.routing ?? {};
  const modelRequested = readModel(request) ?? "";
  const modelServed = readModel(response) ?? routing.model ?? "";
  const receipt: CallReceipt = {
    provider: PROVIDER,
    endpoint,
    modelRequested,
    modelServed,
    verdict: verdictFor(modelRequested, modelServed),
    inputSha256: await canonicalSha256Hex(request),
    outputSha256: await canonicalSha256Hex(response),
    payment,
  };
  if (routing.profile || routing.model || routing.savingsPct !== undefined) {
    receipt.routing = routing;
  }
  return receipt;
}

/** Stable digest over the receipt; the identity a signer or anchor binds to. */
export async function receiptDigest(receipt: CallReceipt): Promise<string> {
  return canonicalSha256Hex(receipt);
}

export interface VerifyResult {
  valid: boolean;
  digest: string;
  digestMatches?: boolean;
  verdictConsistent: boolean;
  statedVerdict: string;
  expectedVerdict: Verdict;
}

/**
 * Re-check a receipt offline: recompute its digest and confirm the stated
 * verdict matches the requested/served pair. Optionally check the digest
 * against an expected value.
 */
export async function verifyReceipt(
  receipt: CallReceipt,
  expectedDigest?: string,
): Promise<VerifyResult> {
  const digest = await receiptDigest(receipt);
  const expectedVerdict = verdictFor(receipt.modelRequested, receipt.modelServed);
  const verdictConsistent = expectedVerdict === receipt.verdict;
  const digestMatches =
    expectedDigest === undefined ? undefined : expectedDigest === digest;
  return {
    valid: verdictConsistent && (digestMatches ?? true),
    digest,
    digestMatches,
    verdictConsistent,
    statedVerdict: receipt.verdict,
    expectedVerdict,
  };
}

function readModel(value: unknown): string | undefined {
  if (value && typeof value === "object" && "model" in value) {
    const m = (value as Record<string, unknown>).model;
    return typeof m === "string" ? m : undefined;
  }
  return undefined;
}

async function sha256(bytes: Uint8Array): Promise<Uint8Array> {
  const subtle = globalThis.crypto?.subtle;
  if (subtle) {
    // Copy into a plain ArrayBuffer-backed view so the type is BufferSource
    // regardless of how the input Uint8Array was allocated.
    const buf = new Uint8Array(bytes);
    return new Uint8Array(await subtle.digest("SHA-256", buf.buffer));
  }
  // Node without global WebCrypto (older runtimes).
  const { createHash } = await import("node:crypto");
  return new Uint8Array(createHash("sha256").update(bytes).digest());
}

function hex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}
