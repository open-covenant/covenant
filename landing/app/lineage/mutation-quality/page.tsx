// /lineage/mutation-quality — Mutation-catch-rate trend over time.
//
// Spike 13 deliverable: this page reads the nightly cargo-mutants JSON dumps
// at landing/public/mutants/<YYYY-MM-DD>.json and renders the trend. Until
// the first nightly run lands (com.covenant.mutants-nightly launchd job
// shipping with Week 1+), the page renders an honest "trend pending" surface.
//
// This page is linked from the permanent yellow Fifth Status Line on
// /verify/[sha] ("Code Quality (Not Witnessed)"). Honest UX discipline:
// the witness chain does not prove semantic correctness; this trend is
// what visitors look at to gauge it.

import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import Link from "next/link";
import { SiteFooter } from "@/app/SiteFooter";
import { SiteHeader } from "@/app/SiteHeader";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

type NightlyRecord = {
  date: string;
  scope: "in_diff" | "weekly_full";
  total_mutations: number;
  caught: number;
  missed: number;
  score: number;
};

function readNightlyRecords(): NightlyRecord[] {
  const dir = join(process.cwd(), "public", "mutants");
  let names: string[] = [];
  try {
    names = readdirSync(dir);
  } catch {
    return [];
  }
  const out: NightlyRecord[] = [];
  for (const name of names) {
    if (!/^\d{4}-\d{2}-\d{2}\.json$/.test(name)) continue;
    try {
      const r = JSON.parse(readFileSync(join(dir, name), "utf8")) as NightlyRecord;
      out.push(r);
    } catch {
      // skip malformed entries
    }
  }
  return out.sort((a, b) => (a.date < b.date ? 1 : -1));
}

export default function MutationQualityPage() {
  const records = readNightlyRecords();
  const latest = records[0];

  return (
    <main id="main-content" className="min-h-[100dvh] bg-[#030303] text-neutral-200">
      <SiteHeader />

      <div className="mx-auto max-w-5xl px-6 pb-24 pt-28">
        <div className="mb-10 flex flex-col gap-2">
          <p className="text-[11px] uppercase tracking-[3px] text-neutral-500">
            /lineage/mutation-quality
          </p>
          <h1 className="text-balance text-[2.2rem] font-extralight uppercase leading-[1.1] tracking-[2px] text-white">
            Code Quality (Not Witnessed)
          </h1>
          <p className="mt-2 max-w-2xl text-[13px] font-light leading-relaxed text-neutral-400">
            The witness chain proves authenticity, on-chain anchoring, and
            adversarial verification. It does <span className="text-amber-300">not</span> prove
            semantic correctness. This page renders the mutation-catch trend from
            the nightly{" "}
            <Link
              href="https://github.com/open-covenant/covenant/blob/main/docs/provenance/covenant-mutants-nightly.md"
              className="underline-offset-4 hover:text-neutral-200 hover:underline"
            >
              cargo-mutants
            </Link>{" "}
            job — the closest test-suite quality signal we have. Honest UX
            discipline: we surface this so the four-light chain can&apos;t be misread
            as &quot;the code is correct.&quot;
          </p>
        </div>

        {!latest && (
          <div className="border border-amber-800/60 bg-amber-950/20 p-5">
            <p className="text-[13px] font-light leading-relaxed text-amber-200/80">
              No nightly mutants records yet. The{" "}
              <code className="text-amber-100">com.covenant.mutants-nightly</code>{" "}
              launchd job ships with the Witness Loop v0.2 Week 1+ ship targets.
              When it lands, this page will render the trend from{" "}
              <code className="text-amber-100">landing/public/mutants/&lt;date&gt;.json</code>.
            </p>
          </div>
        )}

        {latest && (
          <div className="grid gap-4 sm:grid-cols-3">
            <div className="border border-neutral-800 bg-neutral-950/60 p-5">
              <p className="text-[11px] uppercase tracking-[2px] text-neutral-400">
                Latest
              </p>
              <p className="mt-3 text-3xl font-extralight text-white">
                {(latest.score * 100).toFixed(1)}%
              </p>
              <p className="mt-1 text-[12px] uppercase tracking-[1.5px] text-neutral-500">
                {latest.date} ({latest.scope})
              </p>
            </div>
            <div className="border border-neutral-800 bg-neutral-950/60 p-5">
              <p className="text-[11px] uppercase tracking-[2px] text-neutral-400">
                Mutations caught
              </p>
              <p className="mt-3 text-3xl font-extralight text-emerald-300">
                {latest.caught} / {latest.total_mutations}
              </p>
            </div>
            <div className="border border-neutral-800 bg-neutral-950/60 p-5">
              <p className="text-[11px] uppercase tracking-[2px] text-neutral-400">
                Mutations missed
              </p>
              <p className="mt-3 text-3xl font-extralight text-amber-300">
                {latest.missed}
              </p>
            </div>
          </div>
        )}

        {records.length > 1 && (
          <div className="mt-10 border border-neutral-800 bg-neutral-950/60">
            <table className="w-full text-[12px]">
              <thead className="bg-neutral-900/40 text-left text-[10px] uppercase tracking-[1.5px] text-neutral-400">
                <tr>
                  <th className="px-4 py-3">Date</th>
                  <th className="px-4 py-3">Scope</th>
                  <th className="px-4 py-3">Score</th>
                  <th className="px-4 py-3">Caught / Total</th>
                </tr>
              </thead>
              <tbody className="font-light text-neutral-300">
                {records.map((r) => (
                  <tr key={r.date} className="border-t border-neutral-900">
                    <td className="px-4 py-3 text-neutral-100">{r.date}</td>
                    <td className="px-4 py-3 text-neutral-500">{r.scope}</td>
                    <td className="px-4 py-3 text-emerald-300">
                      {(r.score * 100).toFixed(1)}%
                    </td>
                    <td className="px-4 py-3 text-neutral-400">
                      {r.caught} / {r.total_mutations}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        <div className="mt-10 border-t border-neutral-900 pt-6 text-[11px] uppercase tracking-[1.5px] text-neutral-500">
          v0.2 Witness Loop &middot;{" "}
          <Link href="/lineage" className="hover:text-neutral-300">
            Back to lineage
          </Link>{" "}
          &middot;{" "}
          <a
            href="https://github.com/open-covenant/covenant/blob/main/docs/provenance/covenant-mutants-nightly.md"
            target="_blank"
            rel="noopener noreferrer"
            className="hover:text-neutral-300"
          >
            Nightly job spec
          </a>
        </div>
      </div>

      <SiteFooter />
    </main>
  );
}
