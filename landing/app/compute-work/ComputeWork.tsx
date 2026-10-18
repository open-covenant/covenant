"use client";

import { useEffect, useState } from "react";
import { fetchSnapshot, fullyFeeFunded, rawAmount, snapshotCurrent, usdc, utcTime, WORK_PATH, type Mode, type Work, type WorkSnapshot } from "./_work";

export const heading = "text-[12px] uppercase tracking-[0.2em] text-fg";
export const paragraph = "text-[13px] leading-[1.8] text-fg-soft sm:text-[14px]";
export const link = "underline decoration-line-strong underline-offset-4 transition-colors hover:decoration-fg-soft hover:text-fg";
const statusLabels: Record<Work["status"], string> = { candidate: "Candidate · not approved", approved: "Approved", active: "In progress", failed: "Failed checks or execution", completed: "Completed", ineligible: "Ineligible" };

export function Fact({ label, children }: { label: string; children: React.ReactNode }) {
  return <div className="min-w-0 border-t border-line py-4"><dt className="text-[10px] uppercase tracking-[0.14em] text-muted"><span>{label}</span></dt><dd className="mt-2 break-words text-[13px] leading-[1.8] text-fg-soft tabular-nums">{children}</dd></div>;
}

export function FundingLabel({ work, mode }: { work: Work; mode: Mode }) {
  return <p className="text-[12px] leading-[1.8] text-fg-soft">{fullyFeeFunded(work, mode) ? "CVNT/NVDAx trading fees paid for this contribution." : mode === "rehearsal" ? "Rehearsal records. No live funding claim." :
    BigInt(work.subsidy_usdc_micros) > 0n ? `Funding attributed: ${usdc(work.fee_funded_usdc_micros)} fees + ${usdc(work.subsidy_usdc_micros)} subsidy.` :
    work.settlement === "pending" ? "Settlement is pending. Funding attribution is not final." : work.status === "completed" ? "Completed work. Full fee funding has not been verified." : "Work begins only after its approved budget is available."}</p>;
}

export function WorkCard({ work, mode }: { work: Work; mode: Mode }) {
  return <article className="border border-line p-5 sm:p-6">
    <div className="flex flex-wrap items-start justify-between gap-3"><span className={`text-[10px] uppercase tracking-[0.14em] ${work.status === "failed" ? "text-warn" : "text-muted"}`}>{statusLabels[work.status]}</span><span className="text-[11px] text-muted tabular-nums">Cap {usdc(work.max_budget_usdc_micros)}</span></div>
    <h3 className="mt-4 text-[17px] leading-[1.5] text-fg"><a className={link} href={`/compute-work/${work.id}`}>{work.title} ↗</a></h3>
    <p className={`mt-3 ${paragraph}`}>{work.summary}</p>
    <div className="mt-5 border-t border-line pt-4"><FundingLabel work={work} mode={mode} /></div>
    {work.status === "failed" && <p className="mt-2 text-[12px] text-warn">Coordinator-reported cost: {usdc(work.cost_usdc_micros)}. Failed attempts remain public.</p>}
    {work.acceptance.length > 0 && <details className="mt-4 text-[12px] text-muted"><summary className="cursor-pointer">Acceptance criteria ({work.acceptance.length})</summary><ol className="mt-3 list-decimal space-y-2 pl-5 leading-[1.8]">{work.acceptance.map((a, i) => <li key={i}>{a}</li>)}</ol></details>}
  </article>;
}

export function WorkReport({ data, nowMs, requestFailed = false }: { data: WorkSnapshot; nowMs: number; requestFailed?: boolean }) {
  const current = snapshotCurrent(data, nowMs, requestFailed), pool = data.pool;
  const poolCurrent = !!pool && snapshotCurrent({ generated_at_ms: pool.observed_at_ms }, nowMs, requestFailed);
  const queue = data.work.filter(w => ["candidate", "approved", "active"].includes(w.status));
  const outcomes = data.work.filter(w => ["failed", "completed", "ineligible"].includes(w.status));
  return <>
    {!current && <p role="status" className="mt-6 border-l-2 border-warn py-1 pl-4 text-[13px] leading-[1.8] text-fg-soft"><strong className="font-normal text-warn">Current funding state is unknown.</strong> The figures below are the last retrieved report. {requestFailed ? "The latest refresh failed." : "The report is outside the three-minute freshness window, or its clock cannot be verified."}</p>}
    <div className="mt-7 flex flex-wrap items-start justify-between gap-5 border-y border-line py-6">
      <div><p className="text-[10px] uppercase tracking-[0.2em] text-muted">CVNT / NVDAx · Solana pilot</p><p className="mt-3 text-[17px] text-fg">{data.mode === "unfunded" ? "Pilot not funded" : data.mode === "rehearsal" ? "Rehearsal · no live funding claims" : "Live pilot"}{!current ? " · at last report" : ""}</p></div>
      <p className="max-w-lg text-[12px] leading-[1.8] text-muted">{data.mode === "unfunded" ? "No capital allocation is active. Pool compatibility, operating eligibility and explicit capital caps must be confirmed before funding." : data.mode === "rehearsal" ? "These records exercise the funding and work flow. They do not establish real trades, paid jobs or completed settlement." : "Only collected fees from the pilot-owned position are eligible. Principal and unrealized gains are excluded."}</p>
    </div>

    <section className="mt-10" aria-labelledby="budget-title">
      <div className="flex flex-wrap items-baseline justify-between gap-3"><h2 id="budget-title" className={heading}>Work budget</h2><span className="text-[11px] text-muted">USDC · reconciled funding ledger{!current ? " · last report" : ""}</span></div>
      <dl className="mt-5 grid gap-x-6 sm:grid-cols-2 lg:grid-cols-4">
        {([["Available", data.budget.available_usdc_micros], ["Reserved", data.budget.reserved_usdc_micros], ["Spent", data.budget.spent_usdc_micros], ["Subsidy received", data.budget.subsidy_usdc_micros]] as const).map(([label, amount]) => <div key={label} className="border-t border-line py-5"><dt className="text-[10px] uppercase tracking-[0.14em] text-muted">{label}</dt><dd className="mt-3 text-[24px] leading-[1.3] text-fg tabular-nums sm:text-[28px]">{usdc(amount).replace(" USDC", "")}<span className="ml-2 text-[10px] text-muted">USDC</span></dd></div>)}
      </dl><p className="mt-2 text-[12px] leading-[1.8] text-muted">Pending outcomes stay reserved. Subsidy receipts contribute to the budget and are disclosed separately; they are never counted as trading fees.</p>
    </section>

    <section className="mt-12 border-t border-line pt-8 sm:mt-16" aria-labelledby="fees-title">
      <h2 id="fees-title" className={heading}>Collected fees → work</h2>
      <ol className="mt-6 grid gap-5 md:grid-cols-3">{[
        ["01", "Collect", "The position owner harvests earned fees. Collected CVNT stays in the fee treasury."],
        ["02", "Convert & deposit", "Collected NVDAx converts to USDC through an external route. Finalized deposits fund the budget."],
        ["03", "Deliver & inspect", "An approved agent job produces a patch and fresh acceptance checks. Costs and evidence stay public."],
      ].map(([n, title, text]) => <li key={n} className="border border-line p-5"><span className="text-[10px] text-muted">{n}</span><h3 className="mt-3 text-[15px] text-fg">{title}</h3><p className="mt-3 text-[12px] leading-[1.8] text-muted">{text}</p></li>)}</ol>
      <details className="mt-5 text-[12px] text-muted"><summary className="cursor-pointer">Collected token amounts · exact raw units</summary><dl className="mt-4 grid gap-x-6 sm:grid-cols-3"><Fact label="CVNT retained">{rawAmount(data.fees.cvnt_retained_raw)} raw units</Fact><Fact label="NVDAx collected">{rawAmount(data.fees.nvdax_collected_raw)} raw units</Fact><Fact label="NVDAx converted">{rawAmount(data.fees.nvdax_converted_raw)} raw units</Fact></dl><p className="mt-2 leading-[1.8]">Raw token quantities are retained for reconciliation. NVDAx display quantities can change with its issuer multiplier.</p></details>
    </section>

    <section className="mt-12 border-t border-line pt-8 sm:mt-16" aria-labelledby="queue-title">
      <div className="flex flex-wrap items-baseline justify-between gap-3"><h2 id="queue-title" className={heading}>Public work queue</h2><span className="text-[11px] text-muted">{queue.filter(w => w.status !== "candidate").length} / 3 approved slots</span></div>
      <p className={`mt-4 max-w-2xl ${paragraph}`}>Concrete tasks, fixed spending caps, and acceptance criteria set before dispatch. Candidates require operator approval.</p>
      <div className="mt-6 grid gap-5 lg:grid-cols-2">{queue.length ? queue.map(w => <WorkCard key={w.id} work={w} mode={data.mode} />) : <p className="border border-dashed border-line p-5 text-[13px] leading-[1.8] text-muted lg:col-span-2">No public tasks are queued. An empty queue creates no spending authority.</p>}</div>
    </section>

    <section className="mt-12 border-t border-line pt-8 sm:mt-16" aria-labelledby="outcomes-title">
      <h2 id="outcomes-title" className={heading}>Outcomes & attempts</h2>
      <p className={`mt-4 max-w-2xl ${paragraph}`}>Completed contributions and failed attempts remain inspectable. Upstream submission and merge status are recorded separately from execution.</p>
      <div className="mt-6 grid gap-5 lg:grid-cols-2">{outcomes.length ? outcomes.map(w => <WorkCard key={w.id} work={w} mode={data.mode} />) : <p className="border border-dashed border-line p-5 text-[13px] leading-[1.8] text-muted lg:col-span-2">No public outcomes have been recorded. No contribution is claimed as fee-funded.</p>}</div>
    </section>

    <section className="mt-12 border-t border-line pt-8 sm:mt-16" aria-labelledby="pool-title">
      <div className="flex flex-wrap items-baseline justify-between gap-3"><h2 id="pool-title" className={heading}>Pool performance</h2><span className="text-[11px] text-muted">Separate from compute expenditure</span></div>
      {!pool ? <p className="mt-5 border border-dashed border-line p-5 text-[13px] leading-[1.8] text-muted">Pool inventory and trading activity have not been observed. Missing observations are not zero liquidity, zero volume, or zero loss.</p> : <>
        <p className="mt-5 text-[13px] leading-[1.8] text-fg-soft">{pool.active ? "Position active" : "Position inactive"} · {pool.range === "full" ? "Full range" : "Custom range"} · {(pool.fee_bps / 100).toFixed(2)}% observed fee{!poolCurrent ? " · observation stale" : ""}</p>
        {(pool.range !== "full" || pool.fee_bps !== 30) && <p className="mt-3 text-[12px] text-warn">Observed pool settings differ from the pilot default. Operator review is required before further funding.</p>}
        <dl className="mt-5 grid gap-x-6 sm:grid-cols-3"><Fact label="Observed volume">{usdc(pool.volume_usdc_micros)}</Fact><Fact label="LP inventory value">{usdc(pool.lp_value_usdc_micros)}</Fact><Fact label="Holding comparison value">{usdc(pool.hold_value_usdc_micros)}</Fact></dl>
        <p className="mt-2 text-[12px] leading-[1.8] text-muted">LP inventory and holding values use the observer’s recorded valuation. Trading fees and work spending are separate; these values are not a net-return claim.</p>
        <details className="mt-5 text-[12px] text-muted"><summary className="cursor-pointer">Position identity & observation</summary><dl className="mt-4 grid gap-x-6 sm:grid-cols-2"><Fact label="Pool"><a className={`${link} break-all`} href={`https://solscan.io/account/${pool.address}`} rel="noreferrer">{pool.address}</a></Fact><Fact label="Position"><a className={`${link} break-all`} href={`https://solscan.io/account/${pool.position_address}`} rel="noreferrer">{pool.position_address}</a></Fact><Fact label="CVNT inventory">{rawAmount(pool.cvnt_raw)} raw units</Fact><Fact label="NVDAx inventory">{rawAmount(pool.nvdax_raw)} raw units · display multiplier {pool.nvdax_multiplier}</Fact><Fact label="Observed">{utcTime(pool.observed_at_ms)}</Fact><Fact label="Finalized slot">{pool.finalized_slot?.toLocaleString("en-US") ?? "Not observed"}</Fact></dl></details>
      </>}
    </section>
    <section className="mt-12 border-t border-line pt-8 sm:mt-16" aria-labelledby="evidence-title"><h2 id="evidence-title" className={heading}>What the evidence establishes</h2><div className={`mt-4 max-w-3xl space-y-4 ${paragraph}`}><p>Original signatures and output hashes support execution provenance. Acceptance checks test specified behavior; they do not guarantee general correctness. Chain-confirmed transfers, coordinator-reported charges, and work verification are shown separately.</p><p>NVDAx is a stock-linked instrument. Pairing creates no NVIDIA affiliation, GPU entitlement, or stock-redemption right for CVNT holders. Fee income depends on actual trading and may be zero.</p></div></section>
  </>;
}

export function ComputeWorkView() {
  const [data, setData] = useState<WorkSnapshot | null>(null), [loading, setLoading] = useState(true), [error, setError] = useState(false), [refresh, setRefresh] = useState(0), [nowMs, setNowMs] = useState(0);
  useEffect(() => {
    setNowMs(Date.now());
    const clock = setInterval(() => setNowMs(Date.now()), 15_000), poll = setInterval(() => setRefresh(n => n + 1), 60_000);
    return () => { clearInterval(clock); clearInterval(poll); };
  }, []);
  useEffect(() => {
    let active = true;
    const controller = new AbortController(), timeout = setTimeout(() => controller.abort(), 12_000);
    setLoading(true);
    fetchSnapshot(controller.signal).then(report => { if (active) { setData(report); setError(false); setNowMs(Date.now()); } }).catch(() => { if (active) setError(true); }).finally(() => { clearTimeout(timeout); if (active) setLoading(false); });
    return () => { active = false; clearTimeout(timeout); controller.abort(); };
  }, [refresh]);
  return <div className="mt-12 sm:mt-16">
    <div className="flex flex-wrap items-center justify-between gap-4"><p className="text-[11px] leading-[1.8] text-muted" role="status" aria-live="polite">{loading ? "Reading public funding records…" : data ? `Report generated ${utcTime(data.generated_at_ms)}` : "Public funding records unavailable"}</p><button type="button" disabled={loading} onClick={() => setRefresh(n => n + 1)} className="border border-line px-4 py-3 text-[12px] text-muted transition-colors hover:text-fg disabled:cursor-wait disabled:opacity-50">Refresh ↻</button></div>
    {error && <p role="status" className="mt-5 border border-line p-5 text-[13px] leading-[1.8] text-warn">Public funding records could not be refreshed. Current balances and outcomes cannot be confirmed. The page retries every minute.</p>}
    <noscript><p className="mt-5 text-[13px] text-muted">Enable JavaScript for current records, or <a href={WORK_PATH} className={link}>read the public snapshot as JSON</a>.</p></noscript>
    {data ? <WorkReport data={data} nowMs={nowMs} requestFailed={error} /> : <div className="mt-8 border-y border-line py-8"><h2 className={heading}>{loading ? "Loading public records" : "Public records unavailable"}</h2><p className={`mt-4 max-w-2xl ${paragraph}`}>Balances, pool observations and work outcomes appear only after a validated response. This page does not infer funding or completed work from a missing report.</p></div>}
    <nav aria-label="Public work resources" className="mt-10 flex flex-wrap gap-x-6 gap-y-3 text-[12px] text-muted"><a href={WORK_PATH} className={link}>Public snapshot (JSON) ↗</a><a href="/compute-status" className={link}>Compute status</a><a href="/compute" className={link}>Covenant Compute</a><a href="https://docs.opencovenant.org/compute" className={link}>Compute documentation ↗</a></nav>
  </div>;
}
