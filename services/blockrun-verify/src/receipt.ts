// Receipt verification, mirroring the @covenant-org/blockrun and covenant-blockrun
// SDKs and the covenant-blockrun Rust crate: RFC 8785 (JCS) canonicalization,
// SHA-256 digest, and the requested-vs-served verdict. A receipt built by any of
// those verifies here and vice versa (guarded by the cross-language digest test).

import { createHash } from "node:crypto";

export const VERDICT_DELIVERED = "delivered";
export const VERDICT_SUBSTITUTED = "substituted";
export const VERDICT_UNVERIFIED = "unverified";

export interface PaymentInfo {
  network?: string;
  asset?: string;
  amount?: string;
  amountUsdc?: number;
  payTo?: string;
  tx?: string;
}

export interface CallReceipt {
  provider?: string;
  endpoint?: string;
  modelRequested?: string;
  modelServed?: string;
  verdict?: string;
  inputSha256?: string;
  outputSha256?: string;
  routing?: Record<string, unknown>;
  payment?: PaymentInfo;
}

export interface VerifyResult {
  valid: boolean;
  digest: string;
  digestMatches: boolean | null;
  verdictConsistent: boolean;
  statedVerdict: string;
  expectedVerdict: string;
  // Whether the receipt carries a settlement tx string. This is not an on-chain
  // check: the service never touches the chain, it only confirms the field is
  // present. Named to say exactly that.
  hasSettlementTx: boolean;
}

export function canonicalize(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonicalize).join(",")}]`;
  const entries = Object.entries(value as Record<string, unknown>)
    .filter(([, v]) => v !== undefined)
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  return `{${entries.map(([k, v]) => `${JSON.stringify(k)}:${canonicalize(v)}`).join(",")}}`;
}

export function canonicalSha256Hex(value: unknown): string {
  return createHash("sha256").update(canonicalize(value), "utf8").digest("hex");
}

export function verdictFor(requested: string, served: string): string {
  if (!served) return VERDICT_UNVERIFIED;
  return modelBase(requested) === modelBase(served) ? VERDICT_DELIVERED : VERDICT_SUBSTITUTED;
}

/** Model name without a provider namespace: `openai/gpt-4o-mini` → `gpt-4o-mini`. */
function modelBase(model: string): string {
  const lower = model.toLowerCase();
  return lower.slice(lower.lastIndexOf("/") + 1);
}

export function verifyReceipt(receipt: CallReceipt, expectedDigest?: string): VerifyResult {
  const digest = canonicalSha256Hex(receipt);
  const expectedVerdict = verdictFor(receipt.modelRequested ?? "", receipt.modelServed ?? "");
  const verdictConsistent = expectedVerdict === receipt.verdict;
  const digestMatches = expectedDigest === undefined ? null : expectedDigest === digest;
  return {
    valid: verdictConsistent && (digestMatches ?? true),
    digest,
    digestMatches,
    verdictConsistent,
    statedVerdict: receipt.verdict ?? "",
    expectedVerdict,
    hasSettlementTx: Boolean(receipt.payment?.tx),
  };
}

/** Shape check for an incoming receipt: the fields the verdict/digest depend on. */
export function looksLikeReceipt(value: unknown): value is CallReceipt {
  if (!value || typeof value !== "object") return false;
  const r = value as Record<string, unknown>;
  return (
    typeof r.verdict === "string" &&
    typeof r.modelRequested === "string" &&
    typeof r.modelServed === "string" &&
    typeof r.inputSha256 === "string" &&
    typeof r.outputSha256 === "string"
  );
}
