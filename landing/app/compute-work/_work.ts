export const WORK_PATH = "/compute-work/data";
export const WORK_FRESH_MS = 180_000;
export const WORK_MAX_BYTES = 1024 * 1024;
export const WORK_ID = /^[a-z0-9][a-z0-9-]{0,79}$/;
export type Mode = "unfunded" | "rehearsal" | "live";
export type Work = {
  id: string; title: string; summary: string;
  status: "candidate" | "approved" | "active" | "failed" | "completed" | "ineligible";
  max_budget_usdc_micros: string; acceptance: string[]; source_url: string | null; completed_at_ms: number | null;
  cost_usdc_micros: string; fee_funded_usdc_micros: string; subsidy_usdc_micros: string;
  settlement: "unfunded" | "pending" | "settled"; evidence_status: "unpublished" | "unchecked" | "verified" | "failed";
  before: string | null; after: string | null; patch_url: string | null;
  upstream_status: "not_submitted" | "submitted" | "merged"; upstream_url: string | null;
  checks: { label: string; passed: boolean }[];
  transactions: { kind: "harvest" | "conversion" | "deposit" | "settlement"; signature: string }[];
  attempts: { id: string; status: "active" | "failed" | "completed"; cost_usdc_micros: string; summary: string }[];
};
export type MarketObservation = {
  source: "orca_public_api"; observed_at_ms: number; source_updated_at_ms: number;
  source_updated_slot: number; window_seconds: 86400;
};
export type ValuationObservation = {
  source: "operator_reviewed_marks"; observed_at_ms: number; baseline_at_ms: number;
  baseline_slot: number; baseline_sha256: string; marks_sha256: string;
  price_basis: "unscaled_token"; cvnt_price_usdc: string; nvdax_price_usdc: string;
};
export type Pool = {
  address: string; position_address: string; active: boolean; finalized_slot: number | null; observed_at_ms: number;
  fee_bps: number; range: "full" | "custom"; cvnt_raw: string; nvdax_raw: string; nvdax_multiplier: string;
  cvnt_fees_owed_raw: string; nvdax_fees_owed_raw: string;
  volume_usdc_micros: string | null; lp_value_usdc_micros: string | null; hold_value_usdc_micros: string | null;
  market?: MarketObservation | null; valuation?: ValuationObservation | null;
};
export type WorkSnapshot = {
  schema_version: 1; generated_at_ms: number; mode: Mode; pool: Pool | null;
  fees: { cvnt_retained_raw: string; nvdax_collected_raw: string; nvdax_converted_raw: string };
  budget: { available_usdc_micros: string; reserved_usdc_micros: string; spent_usdc_micros: string; subsidy_usdc_micros: string };
  work: Work[];
};
export type WorkDetail = { schema_version: 1; generated_at_ms: number; mode: Mode; work: Work };
export type EvidenceManifest = {
  schema_version: 1; work_id: string; generated_at_ms: number;
  files: { name: string; sha256: string; bytes: number; download_url: string }[];
  verification: { work: "unchecked" | "verified" | "failed"; funding: "unchecked" | "verified" | "failed" };
  limitations: string[];
};

type ObjectValue = Record<string, unknown>;
function invalid(): never { throw new Error("The work service returned an invalid report."); }
function object(value: unknown): ObjectValue { return value !== null && typeof value === "object" && !Array.isArray(value) ? value as ObjectValue : invalid(); }
function text(value: unknown, max = 2000): string { return typeof value === "string" && value.trim().length > 0 && value.length <= max ? value : invalid(); }
function integer(value: unknown): number { return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : invalid(); }
function time(value: unknown): number { const n = integer(value); return n > 0 && n <= 8_640_000_000_000_000 ? n : invalid(); }
function nullableTime(value: unknown): number | null { return value === null ? null : time(value); }
function nullableText(value: unknown): string | null { return value === null ? null : text(value); }
function choice<T extends string>(value: unknown, values: readonly T[]): T { return typeof value === "string" && values.includes(value as T) ? value as T : invalid(); }
function list(value: unknown, max = 100): unknown[] { return Array.isArray(value) && value.length <= max ? value : invalid(); }
function boolean(value: unknown): boolean { return typeof value === "boolean" ? value : invalid(); }
function amount(value: unknown): string { return typeof value === "string" && /^(0|[1-9][0-9]{0,38})$/.test(value) ? value : invalid(); }
function nullableAmount(value: unknown): string | null { return value === null ? null : amount(value); }
function positiveDecimal(value: unknown): string {
  const s = text(value, 80);
  return /^(0|[1-9][0-9]{0,38})(\.[0-9]{1,38})?$/.test(s) && Number(s) > 0 ? s : invalid();
}
function hash(value: unknown): string { const s = text(value, 64); return /^[a-f0-9]{64}$/.test(s) ? s : invalid(); }
function identifier(value: unknown): string { const id = text(value, 80); return WORK_ID.test(id) ? id : invalid(); }
function address(value: unknown): string { const s = text(value, 44); return /^[1-9A-HJ-NP-Za-km-z]{32,44}$/.test(s) ? s : invalid(); }
function sourceUrl(value: unknown): string | null {
  if (value === null) return null;
  const url = new URL(text(value));
  if (url.protocol !== "https:" || url.hostname !== "github.com" || url.port || url.username || url.password || url.search || !/^\/[\w.-]+\/[\w.-]+\//.test(url.pathname)) invalid();
  return url.href;
}

export function validateWork(value: unknown, generatedAt: number): Work {
  const w = object(value);
  const work: Work = {
    id: identifier(w.id), title: text(w.title, 180), summary: text(w.summary),
    status: choice(w.status, ["candidate", "approved", "active", "failed", "completed", "ineligible"]),
    max_budget_usdc_micros: amount(w.max_budget_usdc_micros), acceptance: list(w.acceptance, 30).map(v => text(v)),
    source_url: sourceUrl(w.source_url), completed_at_ms: nullableTime(w.completed_at_ms),
    cost_usdc_micros: amount(w.cost_usdc_micros), fee_funded_usdc_micros: amount(w.fee_funded_usdc_micros), subsidy_usdc_micros: amount(w.subsidy_usdc_micros),
    settlement: choice(w.settlement, ["unfunded", "pending", "settled"]), evidence_status: choice(w.evidence_status, ["unpublished", "unchecked", "verified", "failed"]),
    before: nullableText(w.before), after: nullableText(w.after), patch_url: sourceUrl(w.patch_url),
    upstream_status: choice(w.upstream_status, ["not_submitted", "submitted", "merged"]), upstream_url: sourceUrl(w.upstream_url),
    checks: list(w.checks, 100).map(value => { const c = object(value); return { label: text(c.label), passed: boolean(c.passed) }; }),
    transactions: list(w.transactions, 100).map(value => {
      const tx = object(value), signature = text(tx.signature, 88);
      if (!/^[1-9A-HJ-NP-Za-km-z]{64,88}$/.test(signature)) invalid();
      return { kind: choice(tx.kind, ["harvest", "conversion", "deposit", "settlement"]), signature };
    }),
    attempts: list(w.attempts, 100).map(value => { const a = object(value); return { id: identifier(a.id), status: choice(a.status, ["active", "failed", "completed"]), cost_usdc_micros: amount(a.cost_usdc_micros), summary: text(a.summary) }; }),
  };
  if ((work.completed_at_ms !== null && work.completed_at_ms > generatedAt) ||
      (work.status === "completed" && work.completed_at_ms === null) ||
      BigInt(work.fee_funded_usdc_micros) + BigInt(work.subsidy_usdc_micros) > BigInt(work.cost_usdc_micros) ||
      (["approved", "active"].includes(work.status) && (work.max_budget_usdc_micros === "0" || !work.acceptance.length)) ||
      (work.upstream_status !== "not_submitted" && !work.upstream_url) ||
      new Set(work.attempts.map(a => a.id)).size !== work.attempts.length ||
      new Set(work.transactions.map(tx => tx.signature)).size !== work.transactions.length) invalid();
  return work;
}

function envelope(value: unknown) {
  const report = object(value);
  if (report.schema_version !== 1) invalid();
  return { report, schema_version: 1 as const, generated_at_ms: time(report.generated_at_ms), mode: choice(report.mode, ["unfunded", "rehearsal", "live"]) };
}

// Reconstruct only explicitly public fields. Never spread upstream objects:
// buyer identities, credentials, job payloads and unpublished records stay private.
export function validateSnapshot(value: unknown): WorkSnapshot {
  const { report, ...base } = envelope(value), fees = object(report.fees), budget = object(report.budget);
  let pool: Pool | null = null;
  if (report.pool !== null) {
    const p = object(report.pool), multiplier = positiveDecimal(p.nvdax_multiplier);
    pool = { address: address(p.address), position_address: address(p.position_address), active: boolean(p.active), finalized_slot: p.finalized_slot === null ? null : integer(p.finalized_slot), observed_at_ms: time(p.observed_at_ms),
      fee_bps: integer(p.fee_bps), range: choice(p.range, ["full", "custom"]), cvnt_raw: amount(p.cvnt_raw), nvdax_raw: amount(p.nvdax_raw), nvdax_multiplier: multiplier,
      cvnt_fees_owed_raw: amount(p.cvnt_fees_owed_raw), nvdax_fees_owed_raw: amount(p.nvdax_fees_owed_raw),
      volume_usdc_micros: nullableAmount(p.volume_usdc_micros), lp_value_usdc_micros: nullableAmount(p.lp_value_usdc_micros), hold_value_usdc_micros: nullableAmount(p.hold_value_usdc_micros) };
    if (pool.observed_at_ms > base.generated_at_ms || pool.fee_bps > 10000) invalid();
    if (p.market != null) {
      const m = object(p.market);
      if (m.window_seconds !== 86400) invalid();
      pool.market = { source: choice(m.source, ["orca_public_api"]), observed_at_ms: time(m.observed_at_ms),
        source_updated_at_ms: time(m.source_updated_at_ms), source_updated_slot: integer(m.source_updated_slot), window_seconds: 86400 };
      const market = pool.market;
      if (market.observed_at_ms > pool.observed_at_ms || market.source_updated_at_ms > market.observed_at_ms ||
          base.generated_at_ms - market.source_updated_at_ms > WORK_FRESH_MS ||
          pool.finalized_slot === null || market.source_updated_slot <= 0 || market.source_updated_slot > pool.finalized_slot) invalid();
    } else pool.market = null;
    if (p.valuation != null) {
      const v = object(p.valuation);
      pool.valuation = { source: choice(v.source, ["operator_reviewed_marks"]), observed_at_ms: time(v.observed_at_ms),
        baseline_at_ms: time(v.baseline_at_ms), baseline_slot: integer(v.baseline_slot), baseline_sha256: hash(v.baseline_sha256),
        marks_sha256: hash(v.marks_sha256), price_basis: choice(v.price_basis, ["unscaled_token"]),
        cvnt_price_usdc: positiveDecimal(v.cvnt_price_usdc), nvdax_price_usdc: positiveDecimal(v.nvdax_price_usdc) };
      const valuation = pool.valuation;
      if (valuation.observed_at_ms > pool.observed_at_ms || base.generated_at_ms - valuation.observed_at_ms > WORK_FRESH_MS ||
          valuation.baseline_at_ms > valuation.observed_at_ms || pool.finalized_slot === null ||
          valuation.baseline_slot <= 0 || valuation.baseline_slot > pool.finalized_slot) invalid();
    } else pool.valuation = null;
    if ((pool.volume_usdc_micros !== null) !== !!pool.market ||
        (pool.lp_value_usdc_micros !== null) !== !!pool.valuation ||
        (pool.hold_value_usdc_micros !== null) !== !!pool.valuation) invalid();
  }
  const work = list(report.work, 200).map(w => validateWork(w, base.generated_at_ms));
  if (new Set(work.map(w => w.id)).size !== work.length || work.filter(w => ["approved", "active"].includes(w.status)).length > 3) invalid();
  return { ...base, pool,
    fees: { cvnt_retained_raw: amount(fees.cvnt_retained_raw), nvdax_collected_raw: amount(fees.nvdax_collected_raw), nvdax_converted_raw: amount(fees.nvdax_converted_raw) },
    budget: { available_usdc_micros: amount(budget.available_usdc_micros), reserved_usdc_micros: amount(budget.reserved_usdc_micros), spent_usdc_micros: amount(budget.spent_usdc_micros), subsidy_usdc_micros: amount(budget.subsidy_usdc_micros) }, work };
}

export function validateDetail(value: unknown, workId?: string): WorkDetail {
  const { report, ...base } = envelope(value), work = validateWork(report.work, base.generated_at_ms);
  if (workId !== undefined && work.id !== workId) invalid();
  return { ...base, work };
}

export function validateManifest(value: unknown, workId: string): EvidenceManifest {
  const m = object(value), verification = object(m.verification);
  if (m.schema_version !== 1 || m.work_id !== workId) invalid();
  const files = list(m.files, 50).map(value => {
    const f = object(value), sha256 = text(f.sha256, 64), bytes = integer(f.bytes), name = text(f.name, 100);
    if (!/^[a-f0-9]{64}$/.test(sha256) || bytes > WORK_MAX_BYTES || !/^[a-zA-Z0-9][a-zA-Z0-9._-]*\.json$/.test(name)) invalid();
    const download_url = `https://compute-api.opencovenant.org/community-work/artifacts/${sha256}.json`;
    if (f.download_url !== download_url) invalid();
    return { name, sha256, bytes, download_url };
  });
  if (new Set(files.map(f => f.name)).size !== files.length) invalid();
  return { schema_version: 1, work_id: identifier(m.work_id), generated_at_ms: time(m.generated_at_ms), files,
    verification: { work: choice(verification.work, ["unchecked", "verified", "failed"]), funding: choice(verification.funding, ["unchecked", "verified", "failed"]) },
    limitations: list(m.limitations, 50).map(v => text(v)) };
}

export function snapshotCurrent(data: { generated_at_ms: number }, nowMs: number, requestFailed = false) {
  return !requestFailed && nowMs >= data.generated_at_ms - 30_000 && nowMs - data.generated_at_ms <= WORK_FRESH_MS;
}
export function fullyFeeFunded(work: Work, mode: Mode): boolean {
  return mode === "live" && work.status === "completed" && work.completed_at_ms !== null && work.settlement === "settled" && work.evidence_status === "verified" &&
    BigInt(work.cost_usdc_micros) > 0n && work.fee_funded_usdc_micros === work.cost_usdc_micros && work.subsidy_usdc_micros === "0" &&
    work.checks.length > 0 && work.checks.every(c => c.passed) && !!work.patch_url &&
    ["harvest", "conversion", "deposit", "settlement"].every(kind => work.transactions.some(tx => tx.kind === kind));
}
export function cardHeadline(work: Work, mode: Mode): string {
  return fullyFeeFunded(work, mode) ? `CVNT/NVDAx fees paid for: ${work.title}` : `${mode === "rehearsal" ? "Rehearsal · " : ""}${work.title}`;
}
export function usdc(raw: string | null): string {
  if (raw === null) return "Not observed";
  const value = BigInt(raw), whole = (value / 1_000_000n).toLocaleString("en-US"), fraction = (value % 1_000_000n).toString().padStart(6, "0").replace(/0+$/, "");
  return `${whole}${fraction ? `.${fraction.padEnd(2, "0")}` : ".00"} USDC`;
}
export function rawAmount(value: string): string { return BigInt(value).toLocaleString("en-US"); }
export function utcTime(ms: number): string { return new Date(ms).toISOString().replace("T", " ").replace(/\.\d{3}Z$/, " UTC"); }

export async function fetchSnapshot(signal: AbortSignal, fetcher: typeof fetch = fetch): Promise<WorkSnapshot> {
  const response = await fetcher(WORK_PATH, { credentials: "omit", cache: "no-store", redirect: "error", headers: { accept: "application/json" }, signal });
  if (!response.ok) throw new Error("Public work data is unavailable.");
  if (response.headers.get("content-type")?.split(";")[0].trim().toLowerCase() !== "application/json") invalid();
  return validateSnapshot(await response.json());
}
