import { COMPUTE_ORIGIN } from "../og/_compute";

export const OPERATOR_PATH = "/compute-work/operator-data";
export const OPERATOR_MAX_BYTES = 64 * 1024;
export const OPERATOR_FRESH_MS = 180_000;
export const READINESS_IDS = ["analyst", "pool_rehearsal", "conversion_route", "operator_approval", "shadow_trial"] as const;
export type ReadinessId = typeof READINESS_IDS[number];
export type OperatorSnapshot = {
  schema_version: 1;
  generated_at_ms: number;
  pool_funded: false;
  stage: "preparing" | "observing" | "awaiting_liquidity";
  quote_observation: null | {
    observed_at_ms: number; completed_at_ms: number;
    status: "available" | "partial" | "unavailable";
    available_quotes: number; observation_sha256: string; records_url: string | null;
  };
  readiness: { id: ReadinessId; status: "pending" | "passed" | "failed"; checked_at_ms: number | null }[];
  analyst: { status: "pending" | "accepted" | "failed"; evidence_manifest_url: string | null; source_sha256: string | null };
  costs: { reserved_usdc_micros: string; coordinator_reported_charge_usdc_micros: string | null; settlement: "unresolved" | "reconciled" };
};

function invalid(): never { throw new Error("Invalid public operator report"); }
function object(value: unknown): Record<string, unknown> { return value !== null && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : invalid(); }
function choice<T extends string>(value: unknown, allowed: readonly T[]): T { return typeof value === "string" && allowed.includes(value as T) ? value as T : invalid(); }
function timestamp(value: unknown): number { return typeof value === "number" && Number.isSafeInteger(value) && value > 0 && value <= 8_640_000_000_000_000 ? value : invalid(); }
function hash(value: unknown): string { return typeof value === "string" && /^[a-f0-9]{64}$/.test(value) ? value : invalid(); }
function amount(value: unknown): string { return typeof value === "string" && /^(0|[1-9][0-9]{0,9})$/.test(value) && BigInt(value) <= 1_000_000_000n ? value : invalid(); }
function publicArtifact(value: unknown, kind: "observations" | "manifests", expectedHash?: string): string | null {
  if (value === null) return null;
  if (typeof value !== "string" || value.length > 220) invalid();
  const url = new URL(value, COMPUTE_ORIGIN);
  const match = new RegExp(`^/pool-operator/${kind}/([a-f0-9]{64})\\.json$`).exec(url.pathname);
  if (url.origin !== COMPUTE_ORIGIN || url.username || url.password || url.search || url.hash || !match ||
      (expectedHash !== undefined && match[1] !== expectedHash) || (value !== url.pathname && value !== url.href)) invalid();
  return url.href;
}

// Rebuild the public allowlist. Never spread an upstream object: buyer identities,
// signed submission requests, credentials and private job APIs stay out of this API.
export function validateOperatorSnapshot(value: unknown): OperatorSnapshot {
  const report = object(value);
  if (report.schema_version !== 1 || report.pool_funded !== false) invalid();
  const generated_at_ms = timestamp(report.generated_at_ms);
  const stage = choice(report.stage, ["preparing", "observing", "awaiting_liquidity"]);
  let quote_observation: OperatorSnapshot["quote_observation"] = null;
  if (report.quote_observation !== null) {
    const q = object(report.quote_observation), observed_at_ms = timestamp(q.observed_at_ms), completed_at_ms = timestamp(q.completed_at_ms);
    if (observed_at_ms > completed_at_ms || completed_at_ms > generated_at_ms || typeof q.available_quotes !== "number" ||
        !Number.isInteger(q.available_quotes) || q.available_quotes < 0 || q.available_quotes > 6) invalid();
    const status = choice(q.status, ["available", "partial", "unavailable"]);
    if ((status === "available" && q.available_quotes !== 6) || (status === "partial" && (q.available_quotes === 0 || q.available_quotes === 6)) ||
        (status === "unavailable" && q.available_quotes !== 0)) invalid();
    const observation_sha256 = hash(q.observation_sha256);
    quote_observation = { observed_at_ms, completed_at_ms, status, available_quotes: q.available_quotes,
      observation_sha256, records_url: publicArtifact(q.records_url, "observations", observation_sha256) };
  }
  if (!Array.isArray(report.readiness) || report.readiness.length !== READINESS_IDS.length) invalid();
  const readiness = report.readiness.map(value => {
    const gate = object(value), id = choice(gate.id, READINESS_IDS), status = choice(gate.status, ["pending", "passed", "failed"]);
    const checked_at_ms = gate.checked_at_ms === null ? null : timestamp(gate.checked_at_ms);
    if ((status !== "pending" && checked_at_ms === null) || (checked_at_ms !== null && checked_at_ms > generated_at_ms)) invalid();
    return { id, status, checked_at_ms };
  });
  if (new Set(readiness.map(g => g.id)).size !== READINESS_IDS.length) invalid();
  const a = object(report.analyst), analyst: OperatorSnapshot["analyst"] = {
    status: choice(a.status, ["pending", "accepted", "failed"]),
    evidence_manifest_url: publicArtifact(a.evidence_manifest_url, "manifests"),
    source_sha256: a.source_sha256 === null ? null : hash(a.source_sha256),
  };
  if (analyst.status === "accepted" ? !analyst.evidence_manifest_url || !analyst.source_sha256 : analyst.evidence_manifest_url !== null || analyst.source_sha256 !== null) invalid();
  if ((analyst.status === "accepted") !== (readiness.find(g => g.id === "analyst")!.status === "passed")) invalid();
  if (stage === "awaiting_liquidity" && (readiness.some(g => g.status !== "passed") || analyst.status !== "accepted" || quote_observation?.status !== "available")) invalid();
  if (stage === "observing" && (analyst.status !== "accepted" || quote_observation === null)) invalid();
  const c = object(report.costs), costs: OperatorSnapshot["costs"] = {
    reserved_usdc_micros: amount(c.reserved_usdc_micros),
    coordinator_reported_charge_usdc_micros: c.coordinator_reported_charge_usdc_micros === null ? null : amount(c.coordinator_reported_charge_usdc_micros),
    settlement: choice(c.settlement, ["unresolved", "reconciled"]),
  };
  if (costs.settlement === "reconciled" && costs.coordinator_reported_charge_usdc_micros === null) invalid();
  return { schema_version: 1, generated_at_ms, pool_funded: false, stage, quote_observation, readiness, analyst, costs };
}

export function operatorCurrent(data: OperatorSnapshot, nowMs: number, failed = false): boolean {
  return !failed && data.generated_at_ms <= nowMs + 30_000 && nowMs - data.generated_at_ms <= OPERATOR_FRESH_MS;
}
export function quotesCurrent(data: OperatorSnapshot, nowMs: number): boolean {
  const q = data.quote_observation;
  return q !== null && q.observed_at_ms <= nowMs + 30_000 && nowMs - q.observed_at_ms <= OPERATOR_FRESH_MS;
}
export function liquidityReady(data: OperatorSnapshot, nowMs: number, failed = false): boolean {
  return operatorCurrent(data, nowMs, failed) && quotesCurrent(data, nowMs) && data.stage === "awaiting_liquidity" &&
    data.readiness.every(g => g.status === "passed") && data.analyst.status === "accepted" && data.quote_observation?.status === "available";
}
export async function fetchOperatorSnapshot(signal: AbortSignal, fetcher: typeof fetch = fetch): Promise<OperatorSnapshot> {
  const response = await fetcher(OPERATOR_PATH, { credentials: "omit", redirect: "error", cache: "no-store", headers: { accept: "application/json" }, signal });
  if (!response.ok || response.headers.get("content-type")?.split(";")[0].trim().toLowerCase() !== "application/json") throw new Error("Public pilot status unavailable");
  return validateOperatorSnapshot(await response.json());
}
