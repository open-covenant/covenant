import { Fact, FundingLabel, heading, link, paragraph } from "./ComputeWork";
import { fullyFeeFunded, usdc, utcTime, type WorkDetail } from "./_work";

export function WorkArtifact({ data }: { data: WorkDetail }) {
  const work = data.work, feeFunded = fullyFeeFunded(work, data.mode);
  return <>
    <p className="text-[11px] uppercase tracking-[0.2em] text-muted">{data.mode === "rehearsal" ? "Rehearsal · " : ""}{work.status.replaceAll("_", " ")} / {work.id}</p>
    <h1 className="type-display mt-5 max-w-4xl text-balance text-[2rem] leading-[1.15] text-fg sm:text-[3rem]">{work.title}</h1>
    <p className={`mt-6 max-w-3xl ${paragraph}`}>{work.summary}</p>
    <section aria-label="Contribution result" className="mt-10 border border-line p-6 sm:p-9">
      <p className="text-[10px] uppercase tracking-[0.2em] text-muted">Covenant Compute · CVNT/NVDAx public work</p>
      <h2 className="mt-4 max-w-3xl text-[20px] leading-[1.4] text-fg sm:text-[26px]">{data.mode === "rehearsal" ? "Rehearsal result. No live work claimed." : feeFunded ? "Trading fees paid for this contribution." : work.status === "completed" ? "Completion reported. Inspect the evidence." : "A public task with inspectable acceptance criteria."}</h2>
      <div className="mt-4"><FundingLabel work={work} mode={data.mode} /></div>
      <div className="mt-7 grid gap-6 border-t border-line pt-6 sm:grid-cols-2"><div><h3 className={heading}>Before</h3><p className={`mt-3 ${paragraph}`}>{work.before ?? "Baseline reproduction has not been published."}</p></div><div><h3 className={heading}>After</h3><p className={`mt-3 ${paragraph}`}>{work.after ?? "No accepted result has been published."}</p></div></div>
      <dl className="mt-6 grid gap-x-6 sm:grid-cols-3"><Fact label="Coordinator-reported cost">{usdc(work.cost_usdc_micros)}</Fact><Fact label="Fee funding attributed">{usdc(work.fee_funded_usdc_micros)}</Fact><Fact label="Subsidy attributed">{usdc(work.subsidy_usdc_micros)}</Fact></dl>
      <p className="mt-4 text-[11px] leading-[1.8] text-muted">{work.completed_at_ms ? `Completion recorded ${utcTime(work.completed_at_ms)}. ` : "No completion recorded. "}Snapshot generated {utcTime(data.generated_at_ms)}.</p>
    </section>

    <section className="mt-12" aria-labelledby="acceptance-title"><h2 id="acceptance-title" className={heading}>Acceptance & exact patch</h2>
      {work.acceptance.length ? <ol className="mt-5 list-decimal space-y-3 pl-5 text-[13px] leading-[1.8] text-fg-soft">{work.acceptance.map((a, i) => <li key={i}>{a}</li>)}</ol> : <p className={`mt-4 ${paragraph}`}>Acceptance criteria must be fixed before approval and dispatch.</p>}
      {work.checks.length ? <ul className="mt-6 space-y-2 border-t border-line pt-5">{work.checks.map((check, i) => <li key={i} className="flex gap-3 text-[13px] leading-[1.8]"><span className={check.passed ? "text-fg-soft" : "text-warn"}>{check.passed ? "PASS" : "FAIL"}</span><span className="text-fg-soft">{check.label}</span></li>)}</ul> : <p className="mt-5 text-[13px] text-muted">No acceptance results are published.</p>}
      <div className="mt-6 flex flex-wrap gap-x-6 gap-y-3 text-[12px] text-muted">{work.source_url && <a href={work.source_url} className={link} rel="noreferrer">Source problem ↗</a>}{work.patch_url && <a href={work.patch_url} className={link} rel="noreferrer">Pinned patch reference ↗</a>}</div>
      <p className="mt-5 max-w-3xl text-[12px] leading-[1.8] text-muted">Checks apply to the recorded inputs and returned patch. The signed evidence contains the accepted patch bytes and hash; the GitHub reference is not independently checked against them. Later code edits require another check. Mocked settlement in an interoperability test is not a live payment.</p>
    </section>

    <section className="mt-12 border-t border-line pt-8" aria-labelledby="funding-title"><h2 id="funding-title" className={heading}>Funding & settlement</h2>
      <dl className="mt-5 grid gap-x-6 sm:grid-cols-3"><Fact label="Approved spending cap">{usdc(work.max_budget_usdc_micros)}</Fact><Fact label="Settlement status">{work.settlement === "pending" ? "Pending · unresolved amounts stay reserved" : work.settlement === "settled" ? "Settled in the published funding report" : "Unfunded"}</Fact><Fact label="Upstream status (operator-reported)">{work.upstream_status.replaceAll("_", " ")}{work.upstream_url && <><br /><a href={work.upstream_url} className={link} rel="noreferrer">Inspect upstream ↗</a></>}</Fact></dl>
      {work.transactions.length ? <ol className="mt-5 space-y-3">{work.transactions.map(tx => <li key={tx.signature} className="grid gap-1 text-[12px] leading-[1.8] sm:grid-cols-[110px_1fr]"><span className="capitalize text-muted">{tx.kind}</span><a href={`https://solscan.io/tx/${tx.signature}`} className={`${link} break-all text-fg-soft`} rel="noreferrer">{tx.signature} ↗</a></li>)}</ol> : <p className="mt-4 text-[13px] text-muted">No transaction references are published.</p>}
      <p className="mt-5 max-w-3xl text-[12px] leading-[1.8] text-muted">Transaction links identify transfers to reconcile. A link alone does not verify funding attribution. Coordinator charges and provider settlement are separate observations.</p>
    </section>

    <section className="mt-12 border-t border-line pt-8" aria-labelledby="attempts-title"><h2 id="attempts-title" className={heading}>All published attempts</h2>
      {work.attempts.length ? <ol className="mt-5 space-y-4">{work.attempts.map(a => <li key={a.id} className="border border-line p-5"><div className="flex flex-wrap justify-between gap-3 text-[11px] text-muted"><span>{a.id} · {a.status}</span><span>Coordinator-reported cost: {usdc(a.cost_usdc_micros)}</span></div><p className={`mt-3 ${paragraph}`}>{a.summary}</p></li>)}</ol> : <p className="mt-4 text-[13px] text-muted">No attempts have been published.</p>}
    </section>

    <section className="mt-12 border-t border-line pt-8" aria-labelledby="verification-title"><h2 id="verification-title" className={heading}>Verify the work. Reconcile the funding.</h2>
      <p className={`mt-4 max-w-3xl ${paragraph}`}>Published evidence status: <span className="text-fg">{work.evidence_status}</span>. Work verification checks original signed envelopes, source and output hashes, and acceptance of the exact patch. Funding reconciliation checks chain transfers, conversions, deposits and published allocations independently.</p>
      <div className="mt-6 flex flex-wrap gap-x-6 gap-y-3 text-[12px] text-muted">{work.evidence_status !== "unpublished" && <a href={`/compute-work/${work.id}/manifest`} className={link}>Download evidence manifest (JSON) ↓</a>}<a href={`/compute-work/${work.id}/data`} className={link}>Published record (JSON) ↗</a><a href={`/og/compute-work/${work.id}.png`} className={link}>Share card ↗</a></div>
      <p className="mt-5 max-w-3xl text-[12px] leading-[1.8] text-muted">Receipts establish execution evidence, not guaranteed correctness. Bounded log excerpts are not complete logs. Pairing creates no NVIDIA affiliation, GPU entitlement, or stock-redemption right for CVNT holders.</p>
    </section>
  </>;
}
