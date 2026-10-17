export const DAY = 86_400_000;
export const SOURCE_MARKET = "provider_market_quote";
export const SOURCE_QUOTE = "operator_reported_provider_quote";
export const SOURCE_SETTLED = "settled_lease_charge";
export type PricePoint = {
  gpu_model: string; provider: string; source: typeof SOURCE_MARKET | typeof SOURCE_QUOTE | typeof SOURCE_SETTLED;
  currency: "USD" | "USDC"; day_start_ms: number; first_observed_at_ms: number; last_observed_at_ms: number;
  sample_count: number; mean_micros_per_gpu_hour: number; min_micros_per_gpu_hour: number;
  max_micros_per_gpu_hour: number; proof_paths: string[];
};
export type SamplingStatus = {
  identity: { provider: string; gpu_models: string[]; allocated_storage_gb?: number };
  state: "pending" | "available" | "empty_sample" | "provider_unavailable" | "stale" | "persistence_failed";
  stale: boolean; persistence_healthy: boolean; sample_interval_seconds: number; maximum_offers_per_model: number;
  last_sample_offer_count: number; last_attempt_at_ms: number | null; last_success_at_ms: number | null;
};
export type PriceHistory = {
  version: 1; generated_at_ms: number; coordinator_pubkey_b58: string; points: PricePoint[];
  provider_sampling?: SamplingStatus | null; settlement_mint_b58?: unknown; coverage?: unknown;
  settlement_evidence?: unknown; settlement_identity?: { genesis_hash?: unknown; recording_started_at_ms?: unknown } | null;
};
const record = (v: unknown): v is Record<string, unknown> => v !== null && typeof v === "object";
export const safeInteger = (v: unknown): v is number => Number.isSafeInteger(v) && (v as number) >= 0;
const safeTime = (v: unknown): v is number => safeInteger(v) && v <= 8_640_000_000_000_000;
const validText = (v: unknown): v is string => typeof v === "string" && v.length > 0 && v.length <= 500;
export const validProofPath = (v: unknown): v is string => typeof v === "string" && /^\/prices\/proofs\/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(v);
export function validateHistory(data: unknown): PriceHistory {
  if (!record(data) || data.version !== 1 || !safeTime(data.generated_at_ms) || !validText(data.coordinator_pubkey_b58) || !Array.isArray(data.points) || data.points.length > 10_000) throw new Error("The price history service returned an unsupported data format.");
  for (const p of data.points) {
    if (!record(p) || !validText(p.gpu_model) || !validText(p.provider) || !(((p.source === SOURCE_QUOTE || p.source === SOURCE_MARKET) && p.currency === "USD") || (p.source === SOURCE_SETTLED && p.currency === "USDC")) || !["day_start_ms", "first_observed_at_ms", "last_observed_at_ms"].every(k => safeTime(p[k])) || !["sample_count", "mean_micros_per_gpu_hour", "min_micros_per_gpu_hour", "max_micros_per_gpu_hour"].every(k => safeInteger(p[k])) || p.sample_count === 0 || (p.first_observed_at_ms as number) > (p.last_observed_at_ms as number) || (p.min_micros_per_gpu_hour as number) > (p.mean_micros_per_gpu_hour as number) || (p.mean_micros_per_gpu_hour as number) > (p.max_micros_per_gpu_hour as number) || !Array.isArray(p.proof_paths) || !p.proof_paths.every(validProofPath)) throw new Error("The price history service returned an invalid observation.");
  }
  const status = data.provider_sampling;
  if (status != null && (!record(status) || !record(status.identity) || !validText(status.identity.provider) || !Array.isArray(status.identity.gpu_models) || !status.identity.gpu_models.every(validText) || !["pending", "available", "empty_sample", "provider_unavailable", "stale", "persistence_failed"].includes(status.state as string) || typeof status.stale !== "boolean" || typeof status.persistence_healthy !== "boolean" || !["sample_interval_seconds", "maximum_offers_per_model", "last_sample_offer_count"].every(k => safeInteger(status[k])) || !["last_attempt_at_ms", "last_success_at_ms"].every(k => status[k] == null || safeTime(status[k])))) throw new Error("The market sampling status could not be read.");
  return data as PriceHistory;
}
export function price(micros: number): string {
  const n = BigInt(micros);
  return (n / 1_000_000n).toString() + "." + (n % 1_000_000n).toString().padStart(6, "0").replace(/0+$/, "").padEnd(2, "0");
}
export const utcDate = (ms: number) => new Date(ms).toISOString().slice(0, 10);
export const utcTime = (ms: number) => new Date(ms).toISOString().replace("T", " ").replace(/\.\d{3}Z$/, " UTC");
export const countLabel = (count: number, noun: string) => `${count.toLocaleString("en-US")} ${noun}${count === 1 ? "" : "s"}`;
export function sumSamples(points: PricePoint[]): number {
  const n = points.reduce((sum, p) => sum + p.sample_count, 0);
  if (!Number.isSafeInteger(n)) throw new Error("The sample count exceeds the supported display range.");
  return n;
}
export function groupPoints(points: PricePoint[]): PricePoint[][] {
  const groups = new Map<string, PricePoint[]>();
  for (const p of points) {
    const key = JSON.stringify([p.gpu_model, p.provider, p.source]);
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key)!.push(p);
  }
  return [...groups.values()].map(group => group.sort((a, b) => a.day_start_ms - b.day_start_ms));
}
export function samplingDescription(status?: SamplingStatus | null) {
  if (!status) return { warning: false, current: false, text: "Market sampling is not enabled. Quotes recorded when leases are provisioned and settled rental costs may still appear below." };
  const labels = { pending: "Waiting for the first market sample", available: "Market sampling is up to date", empty_sample: "No offers matched the latest market sample", provider_unavailable: "Market sampling is temporarily unavailable", stale: "Market sampling is overdue", persistence_failed: "The latest market sample could not be saved" };
  const warning = status.stale || !status.persistence_healthy || ["provider_unavailable", "stale", "persistence_failed"].includes(status.state);
  let text = labels[status.state] + ". ";
  if (status.last_success_at_ms != null) text += `Last successful sample: ${utcTime(status.last_success_at_ms)} · ${countLabel(status.last_sample_offer_count, "offer")}. `;
  text += `Up to ${countLabel(status.maximum_offers_per_model, "offer")} per model across ${countLabel(status.identity.gpu_models.length, "GPU model")}, sampled every ${countLabel(Math.round(status.sample_interval_seconds / 60), "minute")}.`;
  if (safeInteger(status.identity.allocated_storage_gb)) text += ` Quotes include ${status.identity.allocated_storage_gb} GB of storage and exclude bandwidth and other usage charges. A listed offer does not guarantee availability through Covenant.`;
  if (warning) text += " Previously recorded history remains available. A sampling issue does not mean provider capacity is empty.";
  return { warning, current: status.state === "available" && !warning, text };
}
export function historyPath(days: number, until = Date.now()) {
  return `/prices/history?since_ms=${until - days * DAY}&until_ms=${until}`;
}
export async function fetchHistory(path: string, signal: AbortSignal): Promise<PriceHistory> {
  const response = await fetch(path, { signal, cache: "no-store", credentials: "omit", headers: { Accept: "application/json" } });
  if (!response.ok) throw new Error(response.status === 404 ? "Public price history is not available." : `The price history service is unavailable (HTTP ${response.status}).`);
  const data = validateHistory(await response.json());
  // Reject unrepresentable aggregate counts before displaying any prices.
  sumSamples(data.points);
  return data;
}

export function recordingStart(data: PriceHistory | null): string | null {
  const timestamp = data?.settlement_identity?.recording_started_at_ms;
  return safeTime(timestamp) ? utcDate(timestamp) : null;
}
