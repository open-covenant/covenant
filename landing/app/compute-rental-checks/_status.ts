export const RENTAL_CHECKS_PATH = "/compute-rental-checks/data";
export const RENTAL_CHECKS_MAX_BYTES = 256 * 1024;
export const HEARTBEAT_FRESH_SECONDS = 180;

export type RentalVerdict = "pending" | "success" | "failed" | "blocked" | "recovered";
export type FeedStatus = "fresh" | "stale" | "disabled" | "not_observed" | "invalid";
export type RentalCheck = {
  run_id: string; job_id: string | null; started_at_unix: number; finished_at_unix: number | null;
  verdict: RentalVerdict; stage: string; reason: string | null; gpu_model: string | null;
  charged_micro_usdc: number | null; refunded_micro_usdc: number | null;
  ssh_identity_verified: boolean; gpu_probe_verified: boolean; provider_cleanup_verified: boolean;
  balance_reconciled: boolean; settlement_verified_onchain: boolean; recovery_only: boolean;
};
export type RentalChecks = {
  schema: 1; generated_at_unix: number | null; server_time_unix: number;
  stale_after_seconds: number; observed_state_age_seconds: number | null; is_stale: boolean; feed_status: FeedStatus;
  enabled: boolean; cadence_seconds: number; next_due_at_unix: number | null; pause_reason: string | null;
  budget: null | { daily_limit_micro_usd: number; pilot_limit_micro_usd: number; daily_reserved_micro_usd: number; pilot_reserved_micro_usd: number };
  monitor: null | { consecutive_unsuccessful_windows: number; alert_due: boolean; last_success_at_unix: number | null };
  latest: RentalCheck | null; history: RentalCheck[];
};

type ObjectValue = Record<string, unknown>;
const invalid = (): never => { throw new Error("The rental-check report is invalid."); };
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const CODE = /^[a-z][a-z0-9_]{0,79}$/;
function object(value: unknown): ObjectValue { return value !== null && typeof value === "object" && !Array.isArray(value) ? value as ObjectValue : invalid(); }
function number(value: unknown): number { return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : invalid(); }
function time(value: unknown): number { const n = number(value); return n > 0 && n <= 8_640_000_000_000 ? n : invalid(); }
function nullableNumber(value: unknown): number | null { return value === null ? null : number(value); }
function nullableTime(value: unknown): number | null { return value === null ? null : time(value); }
function text(value: unknown, max = 100): string { return typeof value === "string" && value.trim().length > 0 && value.length <= max && !/[\x00-\x1f\x7f]/.test(value) ? value : invalid(); }
function code(value: unknown): string { const v = text(value,80); return CODE.test(v) ? v : invalid(); }
function uuid(value: unknown): string { const v = text(value,36); return UUID.test(v) ? v : invalid(); }
function bool(value: unknown): boolean { return typeof value === "boolean" ? value : invalid(); }
function choice<T extends string>(value: unknown, options: readonly T[]): T { return typeof value === "string" && options.includes(value as T) ? value as T : invalid(); }

const STAGES = ["configuration","preflight","funding","budget","launch","ready","ssh_identity","gpu_probe","close","settlement","provider_cleanup","complete","recovery","monitor"] as const;

function validateRun(value: unknown, observedAt: number): RentalCheck {
  const v = object(value);
  const run: RentalCheck = {
    run_id: uuid(v.run_id), job_id: v.job_id === null ? null : uuid(v.job_id),
    started_at_unix: time(v.started_at_unix), finished_at_unix: nullableTime(v.finished_at_unix),
    verdict: choice(v.verdict,["pending","success","failed","blocked","recovered"]),
    stage: choice(v.stage,STAGES), reason: v.reason === null ? null : code(v.reason), gpu_model: v.gpu_model === null ? null : text(v.gpu_model),
    charged_micro_usdc: nullableNumber(v.charged_micro_usdc), refunded_micro_usdc: nullableNumber(v.refunded_micro_usdc),
    ssh_identity_verified: bool(v.ssh_identity_verified), gpu_probe_verified: bool(v.gpu_probe_verified),
    provider_cleanup_verified: bool(v.provider_cleanup_verified), balance_reconciled: bool(v.balance_reconciled),
    settlement_verified_onchain: bool(v.settlement_verified_onchain), recovery_only: bool(v.recovery_only),
  };
  if (run.started_at_unix > observedAt + 30 || (run.finished_at_unix !== null && (run.finished_at_unix < run.started_at_unix || run.finished_at_unix > observedAt + 30))) invalid();
  if (run.verdict === "pending" && run.finished_at_unix !== null) invalid();
  if (run.verdict === "success" && (!run.finished_at_unix || !run.job_id || !run.ssh_identity_verified || !run.gpu_probe_verified || !run.provider_cleanup_verified || !run.balance_reconciled || !run.settlement_verified_onchain || run.recovery_only)) invalid();
  if (run.verdict === "recovered" && (!run.finished_at_unix || !run.recovery_only || !run.provider_cleanup_verified || !run.balance_reconciled || !run.settlement_verified_onchain)) invalid();
  return run;
}

// Return only the public schema. Provider URLs, SSH endpoints, credentials and
// arbitrary extra fields never reach the browser through this proxy.
export function validateRentalChecks(value: unknown): RentalChecks {
  const v = object(value);
  if (v.schema !== 1 || v.cadence_seconds !== 21600) invalid();
  const rawHistory = Array.isArray(v.history) && v.history.length <= 40 ? v.history as unknown[] : invalid();
  const generated = nullableTime(v.generated_at_unix), serverTime = time(v.server_time_unix);
  const staleAfter = number(v.stale_after_seconds), age = nullableNumber(v.observed_state_age_seconds), isStale = bool(v.is_stale);
  const feed = choice(v.feed_status,["fresh","stale","disabled","not_observed","invalid"]);
  if (staleAfter !== HEARTBEAT_FRESH_SECONDS || (generated !== null && generated > serverTime + 30)) invalid();
  if (feed === "fresh" && (generated === null || age === null || age > staleAfter || isStale)) invalid();
  if (feed === "stale" && !isStale) invalid();
  if (generated === null && (v.latest !== null || rawHistory.length || v.budget !== null || v.monitor !== null || age !== null)) invalid();
  const budget = v.budget === null ? null : object(v.budget), monitor = v.monitor === null ? null : object(v.monitor);
  const history = rawHistory.map(item => validateRun(item,generated ?? serverTime));
  if (new Set(history.map(run=>run.run_id)).size !== history.length) invalid();
  const latest = v.latest === null ? null : validateRun(v.latest,generated ?? serverTime);
  const repeated = latest && history.find(run=>run.run_id===latest.run_id);
  if (repeated && JSON.stringify(repeated) !== JSON.stringify(latest)) invalid();
  return {
    schema: 1, generated_at_unix: generated, server_time_unix: serverTime,
    stale_after_seconds: staleAfter, observed_state_age_seconds: age, is_stale: isStale, feed_status: feed,
    enabled: bool(v.enabled), cadence_seconds: 21600, next_due_at_unix: nullableTime(v.next_due_at_unix), pause_reason: v.pause_reason === null ? null : code(v.pause_reason),
    budget: budget === null ? null : { daily_limit_micro_usd: number(budget.daily_limit_micro_usd), pilot_limit_micro_usd: number(budget.pilot_limit_micro_usd), daily_reserved_micro_usd: number(budget.daily_reserved_micro_usd), pilot_reserved_micro_usd: number(budget.pilot_reserved_micro_usd) },
    monitor: monitor === null ? null : { consecutive_unsuccessful_windows: number(monitor.consecutive_unsuccessful_windows), alert_due: bool(monitor.alert_due), last_success_at_unix: nullableTime(monitor.last_success_at_unix) },
    latest, history,
  };
}

export function heartbeatCurrent(report: RentalChecks, nowMs: number, requestFailed = false): boolean {
  const serverMs = report.server_time_unix * 1000;
  return !requestFailed && ["fresh","disabled"].includes(report.feed_status) && !report.is_stale && report.generated_at_unix !== null &&
    nowMs >= serverMs - 30_000 && nowMs - report.generated_at_unix * 1000 <= report.stale_after_seconds * 1000;
}
export function utcTime(seconds: number): string { return new Date(seconds * 1000).toISOString().replace("T"," ").replace(/\.\d{3}Z$/," UTC"); }
export function amount(value: number | null, asset: "USD" | "USDC" = "USDC"): string { return value === null ? "Not confirmed" : `${(value / 1_000_000).toLocaleString("en-US",{maximumFractionDigits:6})} ${asset}`; }
export const verdictLabels: Record<RentalVerdict,string> = { pending:"In progress", success:"Passed", failed:"Failed", blocked:"Blocked", recovered:"Recovery complete" };
export function reasonLabel(reason: string): string {
  const labels: Record<string,string> = { daily_budget_exhausted:"Rolling daily budget exhausted", pilot_budget_exhausted:"Pilot budget exhausted", capacity_unavailable:"No matching GPU capacity", no_affordable_gpu_capacity:"No GPU capacity within the price limit", provider_budget_breach:"Provider cost exceeded its reservation", unresolved_rental:"An earlier rental still needs recovery", launch_outcome_unknown_do_not_retry:"Launch outcome unknown; reconciliation required", gpu_probe_failed:"GPU inspection failed", host_key_binding_unverified:"SSH host identity was not verified", prior_rental_reconciled:"Earlier rental reconciled", preflight_only:"Preflight completed; no rental started", insufficient_funds:"Insufficient available funds", cleanup_pending:"Provider cleanup pending", settlement_pending:"Settlement pending" };
  return labels[reason] ?? reason.replaceAll("_"," ");
}
export function recentRuns(report: RentalChecks): RentalCheck[] {
  return [...(report.latest ? [report.latest] : []),...report.history.filter(run=>run.run_id !== report.latest?.run_id)].sort((a,b)=>b.started_at_unix-a.started_at_unix).slice(0,40);
}
export async function fetchRentalChecks(signal: AbortSignal, fetcher: typeof fetch = fetch): Promise<RentalChecks> {
  const response = await fetcher(RENTAL_CHECKS_PATH,{credentials:"omit",cache:"no-store",redirect:"error",headers:{accept:"application/json"},signal});
  if (!response.ok || response.headers.get("content-type")?.split(";")[0].trim().toLowerCase() !== "application/json") throw new Error("Rental-check data is unavailable.");
  const reader = response.body?.getReader() ?? invalid();
  const decoder = new TextDecoder("utf-8",{fatal:true});
  let bytes = 0,body = "";
  try {
    while (true) {
      const chunk = await reader.read();
      if (chunk.done) break;
      bytes += chunk.value.byteLength;
      if (bytes > RENTAL_CHECKS_MAX_BYTES) invalid();
      body += decoder.decode(chunk.value,{stream:true});
    }
    body += decoder.decode();
  } finally { await reader.cancel(); reader.releaseLock(); }
  return validateRentalChecks(JSON.parse(body));
}
