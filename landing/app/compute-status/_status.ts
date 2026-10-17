export const STATUS_PATH = "/compute-status/data";
export const SNAPSHOT_FRESH_MS = 180_000;
export const STATUS_MAX_BYTES = 1024 * 1024;

export type ObserverState = "disabled" | "not_observed" | "healthy" | "degraded" | "blind" | "stale";
export type Observation = {
  status: ObserverState; observed_at_ms: number | null; l1_slot: number | null;
  scan_complete: boolean; scanned_leases: number; finding_counts: Record<string, number>;
  coverage: "not_observed" | "incomplete" | "no_leases" | "leases_observed"; stale_after_ms: number;
};
export type Incident = {
  id: string; started_at_ms: number; resolved_at_ms: number | null;
  effect: string; cause: string; fix: string; updates: { at_ms: number; message: string }[];
};
export type ComputeStatus = {
  schema_version: 1; generated_at_ms: number; mode: "alert_only";
  journal_writable: boolean; journal_persistence: "durable" | "memory";
  chain_observer: Observation;
  capacity: { registered_operators: number; matchable_operators: number; entries: { kind: string; model: string; operators: number }[] };
  latest_recorded_lease_payout_at_ms: number | null;
  incidents: { schema_version: 1; incidents: Incident[] };
  deployment: { revision: string; coordinator: string; chain: "solana"; genesis_hash: string | null; program: string | null; mint: string | null; lease_coordinator: string | null; funding_renter: string | null; validator: string | null; observer_configured: boolean };
  alerts: { confirmation_observations: number; active_count: number; pending_delivery_count: number; delivery: "structured_logs" | "webhook" | "email"; persistence_healthy: boolean; last_delivery_at_ms: number | null } | null;
};

type ObjectValue = Record<string, unknown>;
const invalid = () => { throw new Error("The status service returned an invalid report."); };
function object(value: unknown): ObjectValue { return value !== null && typeof value === "object" && !Array.isArray(value) ? value as ObjectValue : invalid(); }
function number(value: unknown): number { return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : invalid(); }
function time(value: unknown): number { const n = number(value); return n > 0 && n <= 8_640_000_000_000_000 ? n : invalid(); }
function nullableTime(value: unknown): number | null { return value === null ? null : time(value); }
function text(value: unknown, max = 1000): string { return typeof value === "string" && value.trim().length > 0 && value.length <= max ? value : invalid(); }
function nullableText(value: unknown): string | null { return value === null ? null : text(value); }
function boolean(value: unknown): boolean { return typeof value === "boolean" ? value : invalid(); }
function choice<T extends string>(value: unknown, options: readonly T[]): T { return typeof value === "string" && options.includes(value as T) ? value as T : invalid(); }
function array(value: unknown, max = 1000): unknown[] { return Array.isArray(value) && value.length <= max ? value : invalid(); }

// Reconstruct the public view rather than forwarding unknown fields from an
// upstream response. No lease, buyer, account-finding or RPC details are needed.
export function validateStatus(value: unknown): ComputeStatus {
  const report = object(value), observation = object(report.chain_observer), capacity = object(report.capacity);
  const deployment = object(report.deployment), alerts = report.alerts === null ? null : object(report.alerts), incidents = object(report.incidents);
  if (report.schema_version !== 1 || incidents.schema_version !== 1 || report.mode !== "alert_only") invalid();
  const generated = time(report.generated_at_ms);
  const counts = object(observation.finding_counts);
  if (Object.keys(counts).length > 50) invalid();
  const findingCounts = Object.fromEntries(Object.entries(counts).map(([key, value]) => {
    if (!/^[a-z][a-z0-9_]{0,79}$/.test(key)) invalid();
    return [key, number(value)];
  }));
  const chain: Observation = {
    status: choice(observation.status, ["disabled", "not_observed", "healthy", "degraded", "blind", "stale"]),
    observed_at_ms: nullableTime(observation.observed_at_ms), l1_slot: observation.l1_slot === null ? null : number(observation.l1_slot),
    scan_complete: boolean(observation.scan_complete), scanned_leases: number(observation.scanned_leases), finding_counts: findingCounts,
    coverage: choice(observation.coverage, ["not_observed", "incomplete", "no_leases", "leases_observed"]), stale_after_ms: number(observation.stale_after_ms),
  };
  if (!chain.stale_after_ms || chain.stale_after_ms > 259_200_000 || (chain.observed_at_ms !== null && chain.observed_at_ms > generated) ||
      (chain.status === "healthy" && (!chain.scan_complete || chain.observed_at_ms === null || Object.values(findingCounts).some(n => n > 0))) ||
      (["disabled", "not_observed"].includes(chain.status) && (chain.observed_at_ms !== null || chain.scan_complete)) ||
      (chain.coverage === "no_leases" && (!chain.scan_complete || chain.scanned_leases !== 0)) ||
      (chain.coverage === "leases_observed" && (!chain.scan_complete || chain.scanned_leases === 0))) invalid();
  const registered = number(capacity.registered_operators), matchable = number(capacity.matchable_operators);
  if (matchable > registered) invalid();
  const entries = array(capacity.entries).map(value => {
    const row = object(value), operators = number(row.operators);
    if (!operators || operators > matchable) invalid();
    return { kind: text(row.kind, 80), model: text(row.model, 200), operators };
  });
  const ids = new Set<string>();
  const reviewed = array(incidents.incidents, 100).map(value => {
    const source = object(value);
    const incident: Incident = { id: text(source.id, 100), started_at_ms: time(source.started_at_ms), resolved_at_ms: nullableTime(source.resolved_at_ms),
      effect: text(source.effect, 4000), cause: text(source.cause, 4000), fix: text(source.fix, 4000), updates: [] };
    if (ids.has(incident.id) || incident.started_at_ms > generated || (incident.resolved_at_ms !== null && (incident.resolved_at_ms < incident.started_at_ms || incident.resolved_at_ms > generated))) invalid();
    ids.add(incident.id);
    let previous = incident.started_at_ms;
    incident.updates = array(source.updates, 100).map(value => {
      const update = object(value), at = time(update.at_ms);
      if (at < previous || at > (incident.resolved_at_ms ?? generated)) invalid();
      previous = at;
      return { at_ms: at, message: text(update.message, 4000) };
    });
    if (!incident.updates.length) invalid();
    return incident;
  });
  const lastPayout = nullableTime(report.latest_recorded_lease_payout_at_ms), lastAlert = alerts === null ? null : nullableTime(alerts.last_delivery_at_ms);
  if ((lastPayout !== null && lastPayout > generated) || (lastAlert !== null && lastAlert > generated)) invalid();
  return {
    schema_version: 1, generated_at_ms: generated, mode: "alert_only",
    journal_writable: boolean(report.journal_writable), journal_persistence: choice(report.journal_persistence, ["durable", "memory"]),
    chain_observer: chain, capacity: { registered_operators: registered, matchable_operators: matchable, entries },
    latest_recorded_lease_payout_at_ms: lastPayout, incidents: { schema_version: 1, incidents: reviewed },
    deployment: { revision: text(deployment.revision), coordinator: text(deployment.coordinator), chain: choice(deployment.chain, ["solana"]),
      genesis_hash: nullableText(deployment.genesis_hash), program: nullableText(deployment.program), mint: nullableText(deployment.mint), lease_coordinator: nullableText(deployment.lease_coordinator), funding_renter: nullableText(deployment.funding_renter), validator: nullableText(deployment.validator), observer_configured: boolean(deployment.observer_configured) },
    alerts: alerts === null ? null : { confirmation_observations: number(alerts.confirmation_observations), active_count: number(alerts.active_count), pending_delivery_count: number(alerts.pending_delivery_count),
      delivery: choice(alerts.delivery, ["structured_logs", "webhook", "email"]), persistence_healthy: boolean(alerts.persistence_healthy), last_delivery_at_ms: lastAlert },
  };
}

export function snapshotCurrent(report: ComputeStatus, nowMs: number, requestFailed = false): boolean {
  return !requestFailed && nowMs >= report.generated_at_ms - 30_000 && nowMs - report.generated_at_ms <= SNAPSHOT_FRESH_MS;
}
export function observerState(report: ComputeStatus, nowMs: number): ObserverState {
  const observer = report.chain_observer;
  if (observer.observed_at_ms !== null && nowMs - observer.observed_at_ms > observer.stale_after_ms) return "stale";
  return observer.status;
}
export function utcTime(ms: number): string { return new Date(ms).toISOString().replace("T", " ").replace(/\.\d{3}Z$/, " UTC"); }
export function labelKind(kind: string): string {
  return ({ lease_session: "GPU rentals", inference_call: "Inference", container_exec: "Container jobs", coding_task: "Coding tasks", imd_job: "IMD jobs" } as Record<string, string>)[kind] ?? kind.replaceAll("_", " ");
}

export async function fetchStatus(signal: AbortSignal, fetcher: typeof fetch = fetch): Promise<ComputeStatus> {
  const response = await fetcher(STATUS_PATH, { credentials: "omit", cache: "no-store", redirect: "error", headers: { accept: "application/json" }, signal });
  if (!response.ok) throw new Error(`Status data is unavailable (HTTP ${response.status}).`);
  if (response.headers.get("content-type")?.split(";")[0].trim().toLowerCase() !== "application/json") invalid();
  return validateStatus(await response.json());
}
