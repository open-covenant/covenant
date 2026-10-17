"use client";

import { useEffect, useState } from "react";
import { fetchStatus, labelKind, observerState, snapshotCurrent, STATUS_PATH, utcTime, type ComputeStatus, type ObserverState } from "./_status";

const heading = "text-[13px] uppercase tracking-[0.25em] text-fg sm:text-[14px]";
const paragraph = "text-[13px] leading-[1.8] text-fg-soft sm:text-[14px]";
const link = "underline decoration-line-strong underline-offset-4 transition-colors hover:decoration-fg-soft hover:text-fg";
type Tone = "good" | "warning" | "bad" | "neutral";
const colors: Record<Tone, string> = { good: "text-ok", warning: "text-warn", bad: "text-bad", neutral: "text-muted" };
const observations: Record<ObserverState, { label: string; text: string; tone: Tone }> = {
  disabled: { label: "Not enabled", text: "Independent chain reconciliation is not enabled for this deployment. Lease and settlement consistency has not been verified by this observer.", tone: "neutral" },
  not_observed: { label: "Awaiting first scan", text: "The observer is enabled. It has not completed its first observation yet.", tone: "neutral" },
  healthy: { label: "No discrepancies detected", text: "The latest scan found no discrepancies within its configured scope. This is a point-in-time comparison, not an uptime or settlement guarantee.", tone: "good" },
  degraded: { label: "Discrepancies detected", text: "The observer found differences between chain accounts and coordinator records. These findings require review; they do not by themselves establish a loss of funds.", tone: "warning" },
  blind: { label: "Chain visibility incomplete", text: "The observer could not read all required chain evidence. This is a loss of visibility, not proof that leases or funds are missing.", tone: "warning" },
  stale: { label: "Observation overdue", text: "The last observation is older than the configured freshness limit. Current chain consistency is unknown.", tone: "warning" },
};

function StatusLabel({ children, tone, current = true }: { children: React.ReactNode; tone: Tone; current?: boolean }) {
  return <p className={`mt-4 flex items-start gap-2.5 text-[16px] leading-snug ${colors[current ? tone : "neutral"]}`}><span className="mt-[7px] h-1.5 w-1.5 shrink-0 rounded-full bg-current" aria-hidden="true" /><span>{children}</span></p>;
}
function Fact({ label, children }: { label: string; children: React.ReactNode }) {
  return <div className="min-w-0 border-t border-line py-4"><dt className="text-[10px] uppercase tracking-[0.14em] text-muted-2">{label}</dt><dd className="mt-2 break-words text-[12px] leading-[1.8] text-fg-soft tabular-nums">{children}</dd></div>;
}

export function StatusReport({ data, nowMs, requestFailed = false }: { data: ComputeStatus; nowMs: number; requestFailed?: boolean }) {
  const current = snapshotCurrent(data, nowMs, requestFailed);
  const observer = data.chain_observer, state = observerState(data, nowMs), description = observations[state];
  const activeIncidents = data.incidents.incidents.filter(incident => incident.resolved_at_ms === null);
  const findings = Object.entries(observer.finding_counts).filter(([, count]) => count > 0).sort(([a], [b]) => a.localeCompare(b));
  const journal = !data.journal_writable ? { label: "Writes unavailable", tone: "bad" as const, text: "The coordinator reports that its journal cannot accept writes. New work may be restricted until persistence is restored." }
    : data.journal_persistence === "memory" ? { label: "Memory only", tone: "warning" as const, text: "The journal is writable but has no durable backing. This deployment does not report persistent records." }
    : { label: "Writable", tone: "good" as const, text: "The coordinator reports that its durable journal can accept writes." };

  return <>
    {!current && <div role="status" className="mt-6 border-l-2 border-warn py-1 pl-4 text-[13px] leading-[1.8] text-fg-soft"><strong className="font-normal text-warn">Current status is unknown.</strong> The figures below are from the last retrieved report, generated {utcTime(data.generated_at_ms)}. {requestFailed ? "The latest refresh failed." : "This report is outside the freshness window, or its clock cannot be verified."}</div>}
    <div className="mt-8 grid border-y border-line md:grid-cols-2 md:divide-x md:divide-line">
      <section className="border-b border-line py-7 md:border-b-0 md:pr-8" aria-labelledby="journal-title">
        <h2 id="journal-title" className={heading}>Coordinator journal</h2>
        <StatusLabel tone={journal.tone} current={current}>{journal.label}{!current ? " at last report" : ""}</StatusLabel>
        <p className={`mt-3 max-w-xl ${paragraph}`}>{journal.text}</p>
      </section>
      <section className="py-7 md:pl-8" aria-labelledby="capacity-title">
        <h2 id="capacity-title" className={heading}>Available capacity</h2>
        <StatusLabel tone={data.capacity.matchable_operators ? "good" : "neutral"} current={current}>{data.capacity.matchable_operators.toLocaleString("en-US")} eligible {data.capacity.matchable_operators === 1 ? "operator" : "operators"}{!current ? " at last report" : ""}</StatusLabel>
        <p className={`mt-3 max-w-xl ${paragraph}`}>{data.capacity.matchable_operators ? "Operators currently meeting the coordinator’s liveness, reputation and bond requirements. A specific job still needs a matching offer." : "No operators currently meet the coordinator’s matching requirements. Empty capacity alone does not establish a service outage."}</p>
        <p className="mt-3 text-[11px] text-muted">{data.capacity.registered_operators.toLocaleString("en-US")} registered in this deployment</p>
      </section>
    </div>

    <section className="mt-12 sm:mt-16" aria-labelledby="reconciliation-title">
      <div className="flex flex-wrap items-baseline justify-between gap-3"><h2 id="reconciliation-title" className={heading}>Independent chain reconciliation</h2><span className="border border-line px-2.5 py-1 text-[10px] uppercase tracking-[0.12em] text-muted">Alert-only</span></div>
      <div className="mt-5 grid gap-8 lg:grid-cols-[1.2fr_1fr]">
        <div>
          <StatusLabel tone={description.tone} current={current}>{description.label}{!current ? " at last report" : ""}</StatusLabel>
          <p className={`mt-4 max-w-2xl ${paragraph}`}>{description.text}</p>
          {observer.coverage === "no_leases" && <p className="mt-4 border-l-2 border-line-strong pl-4 text-[12px] leading-[1.8] text-muted">No lease accounts were found in the configured scope. This scan provides no evidence of a completed rental or settlement.</p>}
          <p className="mt-4 text-[12px] leading-[1.8] text-muted">The observer compares Solana lease accounts with coordinator records and checks delegated meters where required. It can identify missing records, delayed settlement and conflicting lease states. It cannot move funds, repair records or settle a lease.</p>
        </div>
        <dl className="grid content-start grid-cols-2 gap-x-6">
          <Fact label="Last observation">{observer.observed_at_ms === null ? "Not yet observed" : utcTime(observer.observed_at_ms)}</Fact>
          <Fact label="Scan coverage">{observer.coverage === "not_observed" ? "Not yet observed" : observer.coverage === "incomplete" ? "Incomplete" : observer.coverage === "no_leases" ? "Complete · no lease accounts" : `Complete · ${observer.scanned_leases.toLocaleString("en-US")} lease ${observer.scanned_leases === 1 ? "account" : "accounts"}`}</Fact>
          <Fact label="Solana slot">{observer.l1_slot?.toLocaleString("en-US") ?? "Not observed"}</Fact>
          <Fact label="Freshness limit">{(observer.stale_after_ms / 1000).toLocaleString("en-US")} seconds</Fact>
        </dl>
      </div>
      {findings.length > 0 && <div className="mt-7 overflow-x-auto"><table className="w-full border-collapse text-left text-[12px]"><caption className="mb-3 text-left text-[11px] leading-relaxed text-muted">Findings from the last scan. Counts can overlap; individual lease and account details are withheld.</caption><thead><tr><th className="border-y border-line py-3 font-normal text-muted">Finding</th><th className="border-y border-line py-3 text-right font-normal text-muted">Count</th></tr></thead><tbody>{findings.map(([kind, count]) => <tr key={kind}><td className="border-b border-line-soft py-3 capitalize text-fg-soft">{kind.replaceAll("_", " ")}</td><td className="border-b border-line-soft py-3 text-right text-fg-soft tabular-nums">{count.toLocaleString("en-US")}</td></tr>)}</tbody></table></div>}
      <div className="mt-7 border-t border-line pt-5 text-[12px] leading-[1.8] text-muted">
        {data.alerts ? <p>Alert confirmation: {data.alerts.confirmation_observations} consecutive observations. {data.alerts.active_count} active alert {data.alerts.active_count === 1 ? "category" : "categories"}. Delivery: {data.alerts.delivery === "webhook" ? "configured webhook" : data.alerts.delivery === "email" ? "configured email destination" : "structured operational logs"}.</p> : <p>Observer alert delivery is not enabled.</p>}
        {data.alerts && (!data.alerts.persistence_healthy || data.alerts.pending_delivery_count > 0) && <p className="mt-2 text-warn">{!data.alerts.persistence_healthy ? "Alert persistence needs attention. " : ""}{data.alerts.pending_delivery_count > 0 ? `${data.alerts.pending_delivery_count} alert deliveries are pending.` : ""}</p>}
        <p className="mt-2">{data.latest_recorded_lease_payout_at_ms === null ? "No lease payout time is recorded in this deployment." : `Latest recorded lease payout: ${utcTime(data.latest_recorded_lease_payout_at_ms)}.`} A lack of recent payouts can reflect a lack of demand; it does not establish a settlement failure.</p>
      </div>
    </section>

    {data.capacity.entries.length > 0 && <section className="mt-12 border-t border-line pt-8 sm:mt-16" aria-labelledby="workloads-title"><h2 id="workloads-title" className={heading}>Capacity by workload</h2><p className={`mt-4 max-w-2xl ${paragraph}`}>Eligible operators by workload and model. One operator may appear in several rows, so the rows do not add up to the network total.</p><div className="mt-6 overflow-x-auto"><table className="w-full border-collapse text-left text-[12px]"><caption className="sr-only">Eligible operators by workload and model in the latest report</caption><thead><tr>{["Workload", "Model", "Operators"].map((label, i) => <th key={label} className={`border-y border-line py-3 pr-4 font-normal text-muted last:pr-0 ${i === 2 ? "text-right" : ""}`}>{label}</th>)}</tr></thead><tbody>{data.capacity.entries.map((row, i) => <tr key={`${row.kind}:${row.model}:${i}`}><td className="border-b border-line-soft py-3 pr-4 text-fg-soft">{labelKind(row.kind)}</td><td className="max-w-[240px] break-words border-b border-line-soft py-3 pr-4 text-muted">{row.model === "any" ? "Any supported model" : row.model}</td><td className="border-b border-line-soft py-3 text-right text-fg-soft tabular-nums">{row.operators.toLocaleString("en-US")}</td></tr>)}</tbody></table></div></section>}

    <section className="mt-12 border-t border-line pt-8 sm:mt-16" aria-labelledby="incidents-title">
      <div className="flex flex-wrap items-baseline justify-between gap-3"><h2 id="incidents-title" className={heading}>Reviewed incidents</h2>{activeIncidents.length > 0 && <span className="text-[12px] text-warn">{activeIncidents.length} unresolved</span>}</div>
      <p className={`mt-4 max-w-2xl ${paragraph}`}>Published accounts of confirmed service incidents, including their effect, cause and remediation. Automated findings appear above and require review before becoming an incident report.</p>
      {!data.incidents.incidents.length ? <p className="mt-6 border border-dashed border-line p-5 text-[13px] text-muted">No reviewed incidents have been published for this deployment.</p> : <div className="mt-6 space-y-8">{[...data.incidents.incidents].sort((a, b) => b.started_at_ms - a.started_at_ms).map(incident => <article key={incident.id} className="border border-line p-5 sm:p-6"><div className="flex flex-wrap items-start justify-between gap-3"><h3 className="text-[15px] leading-relaxed text-fg">{incident.effect}</h3><span className={`text-[11px] ${incident.resolved_at_ms === null ? "text-warn" : "text-muted"}`}>{incident.resolved_at_ms === null ? "Unresolved" : "Resolved"}</span></div><p className="mt-3 text-[11px] leading-relaxed text-muted">Started {utcTime(incident.started_at_ms)}{incident.resolved_at_ms !== null ? ` · Resolved ${utcTime(incident.resolved_at_ms)}` : ""}</p><dl className="mt-5 grid gap-5 md:grid-cols-2"><div><dt className="text-[11px] uppercase tracking-[0.14em] text-muted">Cause</dt><dd className={`mt-2 ${paragraph}`}>{incident.cause}</dd></div><div><dt className="text-[11px] uppercase tracking-[0.14em] text-muted">Remediation</dt><dd className={`mt-2 ${paragraph}`}>{incident.fix}</dd></div></dl><ol className="mt-6 space-y-4 border-t border-line pt-5">{incident.updates.map((update, i) => <li key={i} className="text-[12px] leading-[1.8] text-fg-soft"><time dateTime={new Date(update.at_ms).toISOString()} className="block text-[10px] text-muted">{utcTime(update.at_ms)}</time>{update.message}</li>)}</ol></article>)}</div>}
    </section>

    <section className="mt-12 border-t border-line pt-8 sm:mt-16" aria-labelledby="scope-title"><h2 id="scope-title" className={heading}>Scope and source</h2><div className={`mt-4 max-w-3xl space-y-4 ${paragraph}`}><p>This page reports one Compute deployment. The coordinator supplies journal and capacity observations; the reconciliation observer reads chain evidence separately. A responding API does not prove a rental can complete, and an unavailable API leaves the current service state unknown.</p><p>Reports refresh every minute. This page treats reports older than three minutes as outdated and checks the observer’s own freshness limit separately. All timestamps use UTC.</p></div><details className="mt-6 text-[12px] text-muted"><summary className="cursor-pointer text-fg-soft">Deployment identity</summary><dl className="mt-4 grid gap-x-8 sm:grid-cols-2">{[["Revision", data.deployment.revision], ["Coordinator identity", data.deployment.coordinator], ["Network genesis hash", data.deployment.genesis_hash ?? "Not reported"], ["Solana program", data.deployment.program ?? "Not reported"], ["Settlement mint", data.deployment.mint ?? "Not reported"], ["Lease coordinator account", data.deployment.lease_coordinator ?? "Not configured"], ["Service funding account", data.deployment.funding_renter ?? "Not configured"], ["Ephemeral rollup validator", data.deployment.validator ?? "Not configured"]].map(([name, value]) => <Fact key={name} label={name}><span className="break-all">{value}</span></Fact>)}</dl></details></section>
  </>;
}

export function ComputeStatusView() {
  const [data, setData] = useState<ComputeStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [refresh, setRefresh] = useState(0);
  const [nowMs, setNowMs] = useState(0);
  useEffect(() => {
    setNowMs(Date.now());
    const clock = setInterval(() => setNowMs(Date.now()), 15_000);
    const poll = setInterval(() => setRefresh(value => value + 1), 60_000);
    return () => { clearInterval(clock); clearInterval(poll); };
  }, []);
  useEffect(() => {
    let active = true;
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 12_000);
    setLoading(true);
    fetchStatus(controller.signal).then(report => { if (active) { setData(report); setError(null); setNowMs(Date.now()); } })
      .catch(() => { if (active) setError("Status data could not be refreshed. This does not establish whether Compute is available."); })
      .finally(() => { clearTimeout(timeout); if (active) setLoading(false); });
    return () => { active = false; clearTimeout(timeout); controller.abort(); };
  }, [refresh]);
  return <div className="mt-14 sm:mt-20">
    <div className="flex flex-wrap items-center justify-between gap-4"><p className="text-[11px] leading-relaxed text-muted" role="status" aria-live="polite">{loading ? "Checking current observations…" : data ? `Report generated ${utcTime(data.generated_at_ms)}` : "Current observations unavailable"}</p><button type="button" disabled={loading} onClick={() => setRefresh(value => value + 1)} className="border border-line px-4 py-3 text-[12px] text-muted transition-colors hover:text-fg disabled:cursor-wait disabled:opacity-50">Refresh ↻</button></div>
    {error && <p role="status" className="mt-5 border border-line p-5 text-[13px] leading-[1.8] text-warn">{error} The page will retry automatically, or you can refresh now.</p>}
    <noscript><p className="mt-5 border border-line p-5 text-[13px] text-muted">Enable JavaScript for current observations, or <a href={STATUS_PATH} className={link}>read the status report as JSON</a>.</p></noscript>
    {data ? <StatusReport data={data} nowMs={nowMs} requestFailed={error !== null} /> : <div className="mt-8 border-y border-line py-8"><h2 className={heading}>{loading ? "Loading status report" : "Status report unavailable"}</h2><p className={`mt-4 max-w-2xl ${paragraph}`}>{loading ? "Journal, capacity and chain observations will appear when the status service responds." : "No current service state can be confirmed from this page. Try again shortly or check the source endpoint."}</p></div>}
    <nav aria-label="Compute resources" className="mt-10 flex flex-wrap gap-x-6 gap-y-3 text-[12px] text-muted"><a href={STATUS_PATH} className={link}>Status data (JSON) ↗</a><a href="/compute-prices" className={link}>GPU price history</a><a href="/compute" className={link}>Covenant Compute</a><a href="https://docs.opencovenant.org/compute" className={link}>Compute documentation ↗</a></nav>
  </div>;
}
