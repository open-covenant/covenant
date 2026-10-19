"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { fetchOperatorSnapshot, liquidityReady, operatorCurrent, quotesCurrent, READINESS_IDS, type OperatorSnapshot, type ReadinessId } from "./_operator";
import { usdc, utcTime } from "./_work";

const GATE_LABELS: Record<ReadinessId, string> = {
  analyst: "Analyst and acceptance checks",
  pool_rehearsal: "Pool transaction rehearsal",
  conversion_route: "Fee conversion route",
  operator_approval: "Operator and cost caps",
  shadow_trial: "Seven-day observation",
};

export function OperatorStatusView({ snapshot, nowMs, failed = false, loading = false, onRefresh, archive = false }: {
  snapshot: OperatorSnapshot | null; nowMs: number; failed?: boolean; loading?: boolean; onRefresh?: () => void; archive?: boolean;
}) {
  const current = snapshot !== null && operatorCurrent(snapshot, nowMs, failed);
  const freshQuotes = snapshot !== null && quotesCurrent(snapshot, nowMs);
  const ready = snapshot !== null && liquidityReady(snapshot, nowMs, failed);
  const heading = archive ? "Preparation evidence" : ready ? "Awaiting liquidity" : current && snapshot?.stage === "observing" ? "Observation in progress" : "Preparing the pilot";
  const q = snapshot?.quote_observation;
  return <section aria-labelledby="operator-status-title" className="border-y border-line py-6 sm:py-7">
    <div className="flex flex-wrap items-start justify-between gap-4">
      <div>
        <p className="text-[10px] uppercase tracking-[0.2em] text-muted">{archive ? "Before launch" : "Unfunded pilot"}</p>
        <h2 id="operator-status-title" className="mt-3 text-xl text-fg">{heading}</h2>
      </div>
      {onRefresh && <button type="button" onClick={onRefresh} disabled={loading} className="text-[11px] text-muted underline decoration-line-strong underline-offset-4 hover:text-fg disabled:opacity-50">{loading ? "Refreshing…" : "Refresh status"}</button>}
    </div>
    <p className="mt-3 max-w-2xl text-[13px] leading-[1.8] text-fg-soft">{archive
      ? "Inspect the analyst evidence, preparation checks and external quote observations. These records do not establish the pool’s current funding or performance."
      : ready
      ? "The published preparation checks have passed. The pool is still unfunded; adding liquidity requires the owner’s approval."
      : "The pool is not funded. We’re checking the analyst, transaction flow and market conditions before adding liquidity."}</p>
    {(!snapshot || !current) && <p role={failed || snapshot ? "alert" : "status"} className="mt-4 border-l-2 border-line-strong pl-3 text-[12px] leading-[1.8] text-muted">{snapshot
      ? archive ? "The preparation report is outdated. Its last published records are shown below." : "Status is outdated. The last report is shown below; current readiness is not confirmed."
      : failed ? archive ? "Preparation evidence is temporarily unavailable." : "Public status is unavailable. Readiness has not been confirmed." : "Loading the latest public checks…"}</p>}
    {snapshot && <>
      <p className="mt-4 text-[10px] leading-[1.8] text-muted">Report: {utcTime(snapshot.generated_at_ms)} · refreshes every minute</p>
      <div className="mt-6 grid gap-6 lg:grid-cols-2">
        <div>
          <h3 className="text-[11px] uppercase tracking-[0.16em] text-fg">Preparation checks</h3>
          <ul className="mt-3 divide-y divide-line">{READINESS_IDS.map(id => {
            const gate = snapshot.readiness.find(g => g.id === id)!;
            return <li key={id} className="flex items-start justify-between gap-4 py-3 text-[12px] leading-[1.6]">
              <span className="text-fg-soft">{GATE_LABELS[id]}</span>
              <span className="shrink-0 text-muted">{gate.status === "passed" ? "Passed" : gate.status === "failed" ? "Needs work" : "Pending"}</span>
            </li>;
          })}</ul>
          {snapshot.analyst.status === "accepted" && snapshot.analyst.evidence_manifest_url && <div className="mt-4 text-[11px] leading-[1.8] text-muted">
            <a href={snapshot.analyst.evidence_manifest_url} className="underline decoration-line-strong underline-offset-4 hover:text-fg">Inspect the accepted analyst evidence ↗</a>
            <p className="mt-2 break-all">Source SHA-256: {snapshot.analyst.source_sha256}</p>
          </div>}
          {snapshot.analyst.status === "failed" && <p className="mt-3 text-[12px] leading-[1.8] text-muted">The latest published analyst attempt did not pass acceptance.</p>}
        </div>
        <div>
          <h3 className="text-[11px] uppercase tracking-[0.16em] text-fg">Market observations</h3>
          <p className="mt-4 text-[17px] text-fg">{!q ? "No observation published" : !freshQuotes ? "Observation outdated" : q.status === "unavailable" ? "Quotes unavailable" : `${q.available_quotes}/6 external quotes available`}</p>
          <p className="mt-2 text-[12px] leading-[1.8] text-muted">$5, $10 and $25 in both directions. Quotes show available routes; they do not establish trading demand, fills, earned fees or profit.</p>
          {q && <p className="mt-3 text-[10px] leading-[1.8] text-muted">Observed: {utcTime(q.observed_at_ms)}</p>}
          {q?.records_url && <a href={q.records_url} className="mt-3 inline-block text-[11px] text-muted underline decoration-line-strong underline-offset-4 hover:text-fg">Inspect this observation ↗</a>}
          <div className="mt-6 border-t border-line pt-4">
            <h3 className="text-[11px] uppercase tracking-[0.16em] text-fg">Operating cost record</h3>
            <dl className="mt-3 space-y-2 text-[12px] leading-[1.7]">
              <div className="flex flex-wrap justify-between gap-x-4"><dt className="text-muted">Reserved commitments</dt><dd className="text-fg-soft">{usdc(snapshot.costs.reserved_usdc_micros)}</dd></div>
              <div className="flex flex-wrap justify-between gap-x-4"><dt className="text-muted">Coordinator-reported charge</dt><dd className="text-fg-soft">{snapshot.costs.coordinator_reported_charge_usdc_micros === null ? "Not reported" : usdc(snapshot.costs.coordinator_reported_charge_usdc_micros)}</dd></div>
            </dl>
            <p className="mt-3 text-[11px] leading-[1.8] text-muted">{snapshot.costs.settlement === "unresolved" ? "Settlement remains unresolved. A reported zero charge does not release a reservation." : "Settlement is reported reconciled."} These are published accounting records, not a claim of pool revenue.</p>
          </div>
        </div>
      </div>
    </>}
  </section>;
}

export function OperatorStatus({ archive = false }: { archive?: boolean } = {}) {
  const [snapshot, setSnapshot] = useState<OperatorSnapshot | null>(null);
  const [failed, setFailed] = useState(false);
  const [loading, setLoading] = useState(true);
  const [nowMs, setNowMs] = useState(() => Date.now());
  const active = useRef<AbortController | null>(null);
  const refresh = useCallback(async () => {
    active.current?.abort();
    const controller = new AbortController();
    active.current = controller;
    const timeout = setTimeout(() => controller.abort(), 15_000);
    setLoading(true);
    try {
      const result = await fetchOperatorSnapshot(controller.signal);
      if (active.current === controller && !controller.signal.aborted) { setSnapshot(result); setFailed(false); }
    } catch {
      if (active.current === controller) setFailed(true);
    } finally {
      clearTimeout(timeout);
      if (active.current === controller) { setLoading(false); setNowMs(Date.now()); }
    }
  }, []);
  useEffect(() => {
    void refresh();
    const polling = setInterval(() => { void refresh(); }, 60_000);
    const clock = setInterval(() => setNowMs(Date.now()), 1_000);
    return () => { clearInterval(polling); clearInterval(clock); active.current?.abort(); active.current = null; };
  }, [refresh]);
  return <OperatorStatusView snapshot={snapshot} nowMs={nowMs} failed={failed} loading={loading} archive={archive} onRefresh={() => { void refresh(); }} />;
}
