"use client";

import { useEffect, useState } from "react";
import {
  countLabel, DAY, fetchHistory, groupPoints, historyPath, price, recordingStart, samplingDescription,
  SOURCE_MARKET, SOURCE_QUOTE, SOURCE_SETTLED, sumSamples, utcDate, utcTime,
  type PriceHistory, type PricePoint,
} from "./_history";

const heading = "text-[13px] uppercase tracking-[0.25em] text-fg sm:text-[14px]";
const paragraph = "text-[13px] leading-relaxed text-fg-soft sm:text-[14px]";
const link = "underline decoration-line-strong underline-offset-4 transition-colors hover:decoration-fg-soft hover:text-fg";

export function PriceChart({ points, currency, startMs, endMs }: { points: PricePoint[]; currency: "USD" | "USDC"; startMs: number; endMs: number }) {
  const left = 49, right = 447, top = 16, bottom = 155;
  const max = Math.max(...points.map(p => p.max_micros_per_gpu_hour), 1) * 1.15;
  const x = (ms: number) => left + (ms - startMs) / Math.max(DAY, endMs - startMs) * (right - left);
  const y = (value: number) => bottom - value / max * (bottom - top);
  return (
    <svg viewBox="0 0 460 194" role="img" className="my-4 block h-auto w-full text-fg-soft" aria-label={`Daily price averages and minimum-to-maximum ranges in ${currency} per GPU-hour. Exact daily values follow in the table.`}>
      {[0, 1, 2, 3].map(i => {
        const value = max * i / 3, py = y(value);
        return <g key={i}><line x1={left} x2={right} y1={py} y2={py} className="stroke-line" strokeDasharray="3 5" /><text x={left - 8} y={py + 3} textAnchor="end" className="fill-muted-2 text-[10px]">{(value / 1_000_000).toLocaleString("en-US", { maximumFractionDigits: 2 })}</text></g>;
      })}
      <text x={left} y={182} className="fill-muted-2 text-[10px]">{utcDate(startMs).slice(5)}</text>
      <text x={right} y={182} textAnchor="end" className="fill-muted-2 text-[10px]">{utcDate(endMs).slice(5)}</text>
      {points.map(p => {
        const px = x(p.day_start_ms);
        const title = `${utcDate(p.day_start_ms)}: ${price(p.mean_micros_per_gpu_hour)} ${currency}/GPU-hour; ${countLabel(p.sample_count, "sample")}; range ${price(p.min_micros_per_gpu_hour)}–${price(p.max_micros_per_gpu_hour)}`;
        return <g key={p.day_start_ms}><line x1={px} x2={px} y1={y(p.min_micros_per_gpu_hour)} y2={y(p.max_micros_per_gpu_hour)} stroke="currentColor" strokeWidth={2} opacity={0.55}><title>{title}</title></line><circle cx={px} cy={y(p.mean_micros_per_gpu_hour)} r={4} fill="currentColor" className="stroke-ground"><title>{title}</title></circle></g>;
      })}
    </svg>
  );
}

function EmptyState({ title, children }: { title: string; children: React.ReactNode }) {
  return <div className="border border-dashed border-line px-5 py-7 sm:px-6"><h3 className="text-[13px] text-fg">{title}</h3><p className="mt-2 max-w-2xl text-[12px] leading-[1.8] text-muted">{children}</p></div>;
}

export function PriceSeries({ points, currency, startMs, endMs }: { points: PricePoint[]; currency: "USD" | "USDC"; startMs: number; endMs: number }) {
  if (!points.length) return <EmptyState title={currency === "USD" ? "No provider quotes in this view" : "No settled leases in this view"}>{currency === "USD" ? "Quotes appear as market samples are collected and new leases are provisioned. Earlier prices are not backfilled. Try another GPU model or a wider date range." : "Leases appear after settlement, once GPU, duration and payment records are available. No leases meet these requirements for the selected model and period."}</EmptyState>;
  return <div className="grid gap-6 lg:grid-cols-2">{groupPoints(points).map(group => {
    const first = group[0];
    return <article key={JSON.stringify([first.gpu_model, first.provider, first.source])} className="min-w-0 border border-line p-5 sm:p-6" data-source={first.source}>
      <h3 className="type-display text-[18px] text-fg">{first.gpu_model}</h3>
      <p className="mt-2 break-words text-[11px] text-muted">{first.provider} · {first.source === SOURCE_MARKET ? "Market sample" : first.source === SOURCE_QUOTE ? "Provisioned offer" : "Settled charge"} · {countLabel(sumSamples(group), "sample")}</p>
      <PriceChart points={group} currency={currency} startMs={startMs} endMs={endMs} />
      <p className="mb-4 text-[10px] leading-relaxed text-muted-2">Dots: daily average · bars: minimum–maximum · {currency} / GPU-hour</p>
      <div className="overflow-x-auto">
        <table className="w-full whitespace-nowrap border-collapse text-[12px] tabular-nums">
          <caption className="sr-only">Daily {currency} prices per GPU-hour for {first.gpu_model} from {first.provider}</caption>
          <thead><tr>{["Day (UTC)", "Average", "Samples", "Evidence"].map((label, i) => <th key={label} className={`border-t border-line py-3 pr-4 text-[10px] font-normal uppercase tracking-[0.12em] text-muted-2 last:pr-0 ${i === 1 || i === 2 ? "text-right" : "text-left"}`}>{label}</th>)}</tr></thead>
          <tbody>{[...group].reverse().map(p => <tr key={p.day_start_ms} title={`Observed ${utcTime(p.first_observed_at_ms)} to ${utcTime(p.last_observed_at_ms)}; range ${price(p.min_micros_per_gpu_hour)}–${price(p.max_micros_per_gpu_hour)} ${currency}/GPU-hour`}>
            <td className="border-t border-line-soft py-3 pr-4 text-muted">{utcDate(p.day_start_ms)}</td>
            <td className="border-t border-line-soft py-3 pr-4 text-right text-fg-soft">{price(p.mean_micros_per_gpu_hour)}</td>
            <td className="border-t border-line-soft py-3 pr-4 text-right text-muted">{p.sample_count.toLocaleString("en-US")}</td>
            <td className="border-t border-line-soft py-3 text-[11px] text-muted">{p.proof_paths.length ? [...new Set(p.proof_paths)].map((path, i) => <a className={`${link} mr-3 last:mr-0`} key={path} href={path} aria-label={`Settlement proof ${i + 1} for ${first.gpu_model} on ${utcDate(p.day_start_ms)}`}>Proof {i + 1}</a>) : currency === "USD" ? "Provider quote" : "Not available"}</td>
          </tr>)}</tbody>
        </table>
      </div>
    </article>;
  })}</div>;
}

export function PriceHistoryView() {
  const [days, setDays] = useState(30);
  const [loadedDays, setLoadedDays] = useState(30);
  const [model, setModel] = useState("");
  const [refresh, setRefresh] = useState(0);
  const [data, setData] = useState<PriceHistory | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [jsonPath, setJsonPath] = useState("/prices/history");

  useEffect(() => {
    let active = true;
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 15_000);
    const path = historyPath(days);
    setJsonPath(path); setLoading(true); setError(null);
    fetchHistory(path, controller.signal).then(result => {
      if (!active) return;
      setData(result);
      setLoadedDays(days);
      setModel(previous => result.points.some(point => point.gpu_model === previous) ? previous : "");
    }).catch((err: unknown) => {
      if (!active) return;
      setData(null);
      setError(err instanceof Error && err.name === "AbortError" ? "The request timed out. Refresh to try again." : `${err instanceof Error ? err.message : "Price history could not be loaded."} Refresh to try again.`);
    }).finally(() => { clearTimeout(timeout); if (active) setLoading(false); });
    return () => { active = false; clearTimeout(timeout); controller.abort(); };
  }, [days, refresh]);

  const points = data?.points.filter(p => !model || p.gpu_model === model) ?? [];
  const quotes = points.filter(p => p.source === SOURCE_QUOTE || p.source === SOURCE_MARKET);
  const settlements = points.filter(p => p.source === SOURCE_SETTLED);
  const models = [...new Set(data?.points.map(p => p.gpu_model) ?? [])].sort();
  const endMs = data ? Math.floor(data.generated_at_ms / DAY) * DAY : 0;
  const startMs = endMs - loadedDays * DAY;
  const sampling = samplingDescription(data?.provider_sampling);
  const stats = [
    { label: "Provider quotes", count: data ? sumSamples(quotes) : null, detail: "Sampled market and lease offers" },
    { label: "Settled leases", count: data ? sumSamples(settlements) : null, detail: "Supported by settlement evidence" },
    { label: "GPU models", count: data ? new Set(points.map(p => p.gpu_model)).size : null, detail: "With data in this view" },
  ];
  function series(points: PricePoint[], currency: "USD" | "USDC") {
    if (error) return <EmptyState title="History unavailable">We could not load a valid response from the price history service. Refresh to try again.</EmptyState>;
    if (!data) return <p className="py-6 text-[12px] text-muted">Loading price history…</p>;
    return <PriceSeries points={points} currency={currency} startMs={startMs} endMs={endMs} />;
  }
  return (
    <div className="mt-16 sm:mt-24">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div className="flex flex-wrap items-center gap-4">
          <div className="inline-flex gap-1 border border-line p-1" role="group" aria-label="History period">{[7, 30, 90].map(period => <button key={period} type="button" aria-pressed={period === days} onClick={() => setDays(period)} className={`px-3 py-2 text-[12px] transition-colors ${period === days ? "bg-raised text-fg" : "text-muted hover:text-fg"}`}>{period} days</button>)}</div>
          <label className="sr-only" htmlFor="gpu-filter">GPU model</label>
          <select id="gpu-filter" value={model} onChange={event => setModel(event.target.value)} className="max-w-full border border-line bg-ground px-3 py-3 text-[12px] text-fg-soft"><option value="">All GPU models</option>{models.map(name => <option key={name} value={name}>{name}</option>)}</select>
        </div>
        <button type="button" disabled={loading} onClick={() => setRefresh(value => value + 1)} className="border border-line px-4 py-3 text-[12px] text-muted transition-colors hover:text-fg disabled:cursor-wait disabled:opacity-50">Refresh ↻</button>
      </div>
      <p className={`mt-4 text-[11px] leading-relaxed ${error ? "text-bad" : "text-muted"}`} role="status" aria-live="polite">{loading ? "Loading price history…" : error ?? (data ? `Data as of ${utcTime(data.generated_at_ms)} · Past ${days} days` : "")}</p>
      {data && <p className={`mt-4 border-l-2 py-1 pl-4 text-[11px] leading-[1.8] text-muted ${sampling.warning ? "border-warn" : sampling.current ? "border-ok" : "border-line-strong"}`} role="status">{sampling.text}</p>}
      <noscript><p className="mt-4 border border-line p-4 text-muted">Enable JavaScript to view the charts, or <a href="/prices/history" className={link}>access the price history as JSON</a>.</p></noscript>
      <dl className="my-8 grid grid-cols-1 divide-y divide-line border-y border-line sm:my-10 sm:grid-cols-3 sm:divide-x sm:divide-y-0">{stats.map(stat => <div key={stat.label} className="py-5 sm:px-6 sm:first:pl-0 sm:last:pr-0"><dt className="text-[10px] uppercase tracking-[0.2em] text-muted">{stat.label}</dt><dd className="mt-3 text-[28px] leading-none tracking-tight text-fg tabular-nums">{stat.count?.toLocaleString("en-US") ?? "—"}</dd><dd className="mt-3 text-[11px] leading-relaxed text-muted-2">{stat.detail}</dd></div>)}</dl>
      <section aria-labelledby="quote-title">
        <div className="mb-6"><div className="flex flex-wrap items-baseline justify-between gap-3"><h2 id="quote-title" className={heading}>Provider quotes</h2><span className="text-[11px] text-muted">USD / GPU-hour</span></div><p className={`mt-4 max-w-3xl ${paragraph}`}>A limited sample of market asking prices, ordered by price, and offers reported when a GPU lease is provisioned. Sources are shown separately. Quotes are not invoices and do not cover the full market.</p></div>
        {series(quotes, "USD")}
      </section>
      <section className="mt-14 border-t border-line pt-8 sm:mt-16" aria-labelledby="settled-title">
        <div className="mb-6"><div className="flex flex-wrap items-baseline justify-between gap-3"><h2 id="settled-title" className={heading}>Settled rental costs</h2><span className="text-[11px] text-muted">USDC / GPU-hour</span></div><p className={`mt-4 max-w-3xl ${paragraph}`}>Actual settled charges per GPU-hour. Each lease must have settlement evidence, a recorded GPU count and a metered duration to appear here.</p></div>
        {series(settlements, "USDC")}
      </section>
      <section className="mt-14 border-t border-line pt-8 sm:mt-16" aria-labelledby="method-title">
        <h2 id="method-title" className={heading}>About the data</h2>
        <div className={`mt-6 max-w-3xl space-y-4 ${paragraph}`}>
          <p><strong className="font-normal text-fg">Reading the charts.</strong> Each dot shows a daily average for one GPU model, provider and source. Bars show the day’s lowest and highest recorded rates. Averages are weighted by GPU time. USD quotes and USDC charges remain separate.</p>
          <p><strong className="font-normal text-fg">Coverage.</strong> {recordingStart(data) ? `Price recording began on ${recordingStart(data)} (UTC). Earlier prices are not backfilled.` : "No historical prices have been backfilled."} Market samples are capped per model, so they do not represent all available capacity. Older leases without the required GPU and price records are excluded. Gaps mean no samples were recorded, not that the price was zero.</p>
          <p><strong className="font-normal text-fg">Settlement evidence.</strong> Links include signed receipts and recorded payout transactions for qualifying leases. Check the cited transactions to confirm finality on Solana. Timestamps reflect when a payout was observed, not its block time. Records from separate deployments are not combined.</p>
          <p className="text-[12px] text-muted">All dates and daily averages use UTC.</p>
        </div>
        {data && <details className="mt-6 max-w-3xl text-[12px] leading-[1.8] text-muted"><summary className="cursor-pointer text-fg-soft">Source notes and deployment identity</summary><div className="mt-4 space-y-3">
          {typeof data.coverage === "string" && <p>{data.coverage}</p>}
          {typeof data.settlement_evidence === "string" && <p>{data.settlement_evidence}</p>}
          <p className="break-all text-[10px] text-muted-2">Coordinator public key: {data.coordinator_pubkey_b58}{typeof data.settlement_mint_b58 === "string" ? ` · Settlement token mint: ${data.settlement_mint_b58}` : " · Settlement identity unavailable"}{typeof data.settlement_identity?.genesis_hash === "string" ? ` · Network genesis hash: ${data.settlement_identity.genesis_hash}` : ""}</p>
        </div></details>}
        <div className="mt-8 flex flex-wrap gap-x-6 gap-y-3 text-[12px] text-muted"><a href={jsonPath} className={link}>Access the data (JSON) ↗</a><a href="https://docs.opencovenant.org/compute" className={link}>Compute documentation ↗</a><a href="https://github.com/open-covenant/covenant" className={link}>Source code ↗</a></div>
      </section>
    </div>
  );
}
