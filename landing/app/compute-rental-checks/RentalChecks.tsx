"use client";

import { useEffect, useState } from "react";
import { amount, fetchRentalChecks, heartbeatCurrent, reasonLabel, recentRuns, RENTAL_CHECKS_PATH, utcTime, verdictLabels, type RentalCheck, type RentalChecks } from "./_status";

const heading = "text-[13px] uppercase tracking-[0.25em] text-fg sm:text-[14px]";
const paragraph = "text-[13px] leading-[1.8] text-fg-soft sm:text-[14px]";
const link = "underline decoration-line-strong underline-offset-4 transition-colors hover:decoration-fg-soft hover:text-fg";
const colors = { success:"text-ok", failed:"text-bad", blocked:"text-warn", pending:"text-muted", recovered:"text-fg-soft" };
function Fact({label,children}:{label:string;children:React.ReactNode}) {
  return <div className="min-w-0 border-t border-line py-4"><dt className="text-[10px] uppercase tracking-[0.14em] text-muted-2">{label}</dt><dd className="mt-2 break-words text-[12px] leading-[1.8] text-fg-soft tabular-nums">{children}</dd></div>;
}
function RunDetails({run}:{run:RentalCheck}) {
  return <>
    <dl className="mt-5 grid gap-x-8 sm:grid-cols-2">
      <Fact label="Started">{utcTime(run.started_at_unix)}</Fact><Fact label="Finished">{run.finished_at_unix === null ? "Not confirmed" : utcTime(run.finished_at_unix)}</Fact>
      <Fact label="GPU reported by probe">{run.gpu_model ?? "Not recorded"}</Fact><Fact label="Recorded stage">{reasonLabel(run.stage)}</Fact>
      <Fact label="Charge">{amount(run.charged_micro_usdc)}</Fact><Fact label="Unused escrow credit">{amount(run.refunded_micro_usdc)}</Fact>
    </dl>
    <ul className="mt-4 grid gap-x-8 sm:grid-cols-2" aria-label="Run verification steps">{([
      ["SSH host identity",run.ssh_identity_verified], ["GPU inspection",run.gpu_probe_verified], ["Solana settlement",run.settlement_verified_onchain], ["Buyer balance reconciliation",run.balance_reconciled], ["Provider instance cleanup",run.provider_cleanup_verified],
    ] as const).map(([label,verified])=><li key={label} className="flex items-baseline justify-between gap-5 border-t border-line py-3 text-[12px]"><span className="text-muted">{label}</span><span className={`shrink-0 ${verified ? "text-fg-soft" : "text-muted-2"}`}>{verified ? "Verified" : "Not verified"}</span></li>)}</ul>
    {run.reason && <p className="mt-5 border-l border-line-strong pl-4 text-[13px] leading-[1.8] text-fg-soft">Recorded reason: {reasonLabel(run.reason)}.</p>}
    {run.recovery_only && <p className="mt-4 text-[12px] leading-[1.8] text-muted">This run addresses an earlier interruption. Successful recovery does not establish a new completed GPU rental check.</p>}
    <dl className="mt-5 grid gap-x-8 sm:grid-cols-2"><Fact label="Run ID"><span className="break-all">{run.run_id}</span></Fact><Fact label="Job / lease ID"><span className="break-all">{run.job_id ?? "No job recorded"}</span></Fact></dl>
  </>;
}

export function RentalChecksReport({data,nowMs,requestFailed=false}:{data:RentalChecks;nowMs:number;requestFailed?:boolean}) {
  const current = heartbeatCurrent(data,nowMs,requestFailed);
  const latest = data.latest;
  const history = recentRuns(data).filter(run=>run.run_id !== latest?.run_id);
  const paused = !data.enabled || data.pause_reason !== null;
  const hasState = data.generated_at_unix !== null;
  const feedDescription = requestFailed ? "The latest refresh failed. The current rental-check service state is unknown." : data.feed_status === "invalid" ? "The service could not validate its stored report. No current rental result can be confirmed from this report." : data.feed_status === "not_observed" ? "The reporting service has not published its first observation." : !current ? "The reporting heartbeat is overdue or its clock cannot be verified. Historical rental results keep their original verdicts." : data.feed_status === "disabled" ? "The reporting service is current. New rentals are paused. The budget and any recorded pause reason are shown below." : "The reporting service is current. The rental result below has its own timestamp.";
  return <>
    <section className="mt-7 border-y border-line py-6" aria-labelledby="heartbeat-title"><h2 id="heartbeat-title" className="text-[10px] uppercase tracking-[0.16em] text-muted">Reporting heartbeat</h2><p className={`mt-3 text-[15px] ${current ? "text-fg" : "text-warn"}`}>{current ? "Current report" : data.feed_status === "not_observed" ? "Awaiting first report" : data.feed_status === "invalid" ? "Report unavailable" : "Current state unknown"}</p><p className={`mt-3 max-w-3xl ${paragraph}`}>{feedDescription}</p><p className="mt-3 text-[11px] leading-[1.8] text-muted">{data.generated_at_unix === null ? "No reporting heartbeat is recorded." : `Heartbeat ${utcTime(data.generated_at_unix)}.`} The freshness limit is {data.stale_after_seconds} seconds; the rental cadence is six hours.</p></section>

    <div className="mt-8 grid gap-8 lg:grid-cols-[1.35fr_1fr]">
      <section aria-labelledby="latest-title"><h2 id="latest-title" className={heading}>Latest rental result</h2>{latest ? <><p className={`mt-5 text-[22px] ${current ? colors[latest.verdict] : "text-muted"}`}>{verdictLabels[latest.verdict]}{!current ? " · last report" : ""}</p><p className="mt-2 text-[12px] leading-[1.8] text-muted">{latest.verdict === "success" ? "Every required check completed for this rental." : latest.verdict === "recovered" ? "Outstanding settlement and cleanup were resolved; this is not a new passed rental." : latest.verdict === "pending" ? "The run has not reached a final result." : latest.verdict === "blocked" ? "A requirement prevented a complete check. Review the recorded reason." : "The run did not complete every required check. Review the remaining verification steps."}</p><RunDetails run={latest}/></> : <p className={`mt-5 border-l border-line-strong pl-5 ${paragraph}`}>No rental result has been recorded. A fresh reporting service alone does not establish that a GPU rental can complete.</p>}</section>
      <aside className="border-t border-line pt-6 lg:border-l lg:border-t-0 lg:pl-8 lg:pt-0" aria-labelledby="schedule-title"><h2 id="schedule-title" className={heading}>Schedule and pilot budget</h2><p className={`mt-5 text-[15px] ${paused ? "text-warn" : "text-fg-soft"}`}>{!hasState ? "Schedule not confirmed" : paused ? `New rentals paused${current ? "" : " · last report"}` : current ? "Six-hour schedule enabled" : "Schedule at last report"}</p>{data.pause_reason && <p className={`mt-3 ${paragraph}`}>{reasonLabel(data.pause_reason)}.</p>}<dl className="mt-5"><Fact label="Next scheduled window">{!hasState ? "Not confirmed" : paused ? "Not scheduled while new rentals are paused" : data.next_due_at_unix === null ? "Not scheduled" : utcTime(data.next_due_at_unix)}</Fact><Fact label="Last passed rental">{data.monitor?.last_success_at_unix ? utcTime(data.monitor.last_success_at_unix) : "Not recorded"}</Fact>{data.monitor && <><Fact label="Consecutive unsuccessful / missed windows">{data.monitor.consecutive_unsuccessful_windows}</Fact><Fact label="Alert threshold">{data.monitor.alert_due ? "Reached — delivery status is not reported here" : "Not reached"}</Fact></>}</dl>{data.budget ? <dl className="mt-4"><Fact label="Rolling 24-hour allowance">{amount(data.budget.daily_limit_micro_usd,"USD")}</Fact><Fact label="Daily commitments counted">{amount(data.budget.daily_reserved_micro_usd,"USD")}</Fact><Fact label="Total pilot allowance">{amount(data.budget.pilot_limit_micro_usd,"USD")}</Fact><Fact label="Pilot commitments counted">{amount(data.budget.pilot_reserved_micro_usd,"USD")}</Fact></dl> : <p className="mt-5 text-[12px] leading-[1.8] text-muted">Budget figures are unavailable; no zero balance is inferred.</p>}<p className="mt-4 text-[11px] leading-[1.8] text-muted">Budget commitments include unresolved reservations. They are not a report of final rental charges.</p></aside>
    </div>

    <section className="mt-12 border-t border-line pt-8 sm:mt-16" aria-labelledby="history-title"><h2 id="history-title" className={heading}>Earlier checks</h2><p className={`mt-4 max-w-3xl ${paragraph}`}>Up to 40 recent runs, including the latest result above. Each row is a recorded outcome. A missed schedule window does not create a successful rental.</p>{history.length ? <ol className="mt-6 divide-y divide-line border-y border-line">{history.map(run=><li key={run.run_id}><details className="group py-5"><summary className="flex cursor-pointer list-none flex-wrap items-baseline justify-between gap-x-5 gap-y-2 text-[13px]"><span className="flex flex-wrap items-baseline gap-x-4 gap-y-2"><span className="text-fg-soft">{verdictLabels[run.verdict]}</span><time dateTime={new Date(run.started_at_unix*1000).toISOString()} className="text-[11px] text-muted">{utcTime(run.started_at_unix)}</time></span><span className="text-[11px] text-muted">Details <span aria-hidden="true" className="inline-block transition-transform group-open:rotate-45">+</span></span></summary><RunDetails run={run}/></details></li>)}</ol> : <p className="mt-6 border border-dashed border-line p-5 text-[13px] text-muted">No earlier runs are included in this report.</p>}</section>
  </>;
}

export function RentalChecksView() {
  const [data,setData] = useState<RentalChecks|null>(null);
  const [loading,setLoading] = useState(true);
  const [error,setError] = useState(false);
  const [refresh,setRefresh] = useState(0);
  const [nowMs,setNowMs] = useState(0);
  useEffect(()=>{setNowMs(Date.now());const clock=setInterval(()=>setNowMs(Date.now()),15000);const poll=setInterval(()=>setRefresh(value=>value+1),60000);return()=>{clearInterval(clock);clearInterval(poll);};},[]);
  useEffect(()=>{let active=true;const controller=new AbortController();const timeout=setTimeout(()=>controller.abort(),12000);setLoading(true);
    fetchRentalChecks(controller.signal).then(report=>{if(active){setData(report);setError(false);setNowMs(Date.now());}}).catch(()=>{if(active)setError(true);}).finally(()=>{clearTimeout(timeout);if(active)setLoading(false);});
    return()=>{active=false;clearTimeout(timeout);controller.abort();};},[refresh]);
  return <div className="mt-14 sm:mt-20"><div className="flex flex-wrap items-center justify-between gap-4"><p role="status" aria-live="polite" className="text-[11px] leading-[1.8] text-muted">{loading ? "Retrieving rental-check report…" : error ? "Report refresh unavailable" : data ? `Report served ${utcTime(data.server_time_unix)}` : "Awaiting report"}</p><button type="button" onClick={()=>setRefresh(value=>value+1)} disabled={loading} className="border border-line px-4 py-3 text-[12px] text-muted transition-colors hover:text-fg disabled:cursor-wait disabled:opacity-50">Refresh ↻</button></div>
    {error && <p role="status" className="mt-5 border-l-2 border-warn pl-4 text-[13px] leading-[1.8] text-fg-soft">The report could not be refreshed. This does not establish a rental failure. The page will retry automatically.</p>}
    {data ? <RentalChecksReport data={data} nowMs={nowMs} requestFailed={error}/> : <section className="mt-7 border-y border-line py-7" aria-labelledby="empty-title"><h2 id="empty-title" className={heading}>{loading ? "Loading rental checks" : "Current report unavailable"}</h2><p className={`mt-4 max-w-2xl ${paragraph}`}>No current rental result can be confirmed until the reporting service responds.</p></section>}
    <noscript><p className={`mt-5 ${paragraph}`}>Enable JavaScript to view current reports, or read the JSON report below.</p></noscript>
    <a href={RENTAL_CHECKS_PATH} className={`mt-7 inline-flex min-h-11 items-center text-[12px] text-muted ${link}`}>Rental-check report (JSON) ↗</a>
  </div>;
}
