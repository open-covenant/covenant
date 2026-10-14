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

import Link from "next/link";
import { SiteFooter } from "@/app/SiteFooter";
import { SiteHeader } from "@/app/SiteHeader";
import { readNightlyRecords } from "./_records";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export default function MutationQualityPage() {
  const records = readNightlyRecords();
  const latest = records[0];

  return (
    <main id="main-content" className="min-h-[100dvh] bg-ground text-fg-2">
      <SiteHeader />

      <div className="mx-auto max-w-5xl px-6 pb-24 pt-28">
        <div className="mb-10 flex flex-col gap-2">
          <p className="text-[11px] uppercase tracking-[3px] text-muted-2">
            /lineage/mutation-quality
          </p>
          <h1 className="page-title text-fg">
            Code Quality (Not Witnessed)
          </h1>
          <p className="mt-2 max-w-2xl text-[13px] leading-relaxed text-muted">
            The witness chain proves authenticity, on-chain anchoring, and
            adversarial verification. It does <span className="text-warn">not</span> prove
            semantic correctness. This page renders the mutation-catch trend from
            the nightly{" "}
            <Link
              href="https://github.com/open-covenant/covenant/blob/main/docs/provenance/covenant-mutants-nightly.md"
              className="underline-offset-4 hover:text-fg-2 hover:underline"
            >
              cargo-mutants
            </Link>{" "}
            job — the closest test-suite quality signal we have. Honest UX
            discipline: we surface this so the four-light chain can&apos;t be misread
            as &quot;the code is correct.&quot;
          </p>
        </div>

        {!latest && (
          <div className="border border-warn/40 bg-warn/5 p-5">
            <p className="text-[13px] leading-relaxed text-warn/80">
              No nightly mutants records yet. The{" "}
              <code className="text-warn">com.covenant.mutants-nightly</code>{" "}
              launchd job ships with the Witness Loop v0.2 Week 1+ ship targets.
              When it lands, this page will render the trend from{" "}
              <code className="text-warn">landing/public/mutants/&lt;date&gt;.json</code>.
            </p>
          </div>
        )}

        {latest && (
          <div className="grid gap-4 sm:grid-cols-3">
            <div className="border border-line bg-raised/60 p-5">
              <p className="text-[11px] uppercase tracking-[2px] text-muted">
                Latest
              </p>
              <p className="type-display mt-3 text-3xl text-fg">
                {(latest.score * 100).toFixed(1)}%
              </p>
              <p className="mt-1 text-[12px] uppercase tracking-[1.5px] text-muted-2">
                {latest.date} ({latest.scope})
              </p>
            </div>
            <div className="border border-line bg-raised/60 p-5">
              <p className="text-[11px] uppercase tracking-[2px] text-muted">
                Mutations caught
              </p>
              <p className="type-display mt-3 text-3xl text-ok">
                {latest.caught} / {latest.total_mutations}
              </p>
            </div>
            <div className="border border-line bg-raised/60 p-5">
              <p className="text-[11px] uppercase tracking-[2px] text-muted">
                Mutations missed
              </p>
              <p className="type-display mt-3 text-3xl text-warn">
                {latest.missed}
              </p>
            </div>
          </div>
        )}

        {records.length > 1 && (
          <div className="mt-10 border border-line bg-raised/60">
            <table className="w-full text-[12px]">
              <thead className="bg-sunken/40 text-left text-[10px] uppercase tracking-[1.5px] text-muted">
                <tr>
                  <th className="px-4 py-3">Date</th>
                  <th className="px-4 py-3">Scope</th>
                  <th className="px-4 py-3">Score</th>
                  <th className="px-4 py-3">Caught / Total</th>
                </tr>
              </thead>
              <tbody className="text-fg-soft">
                {records.map((r) => (
                  <tr key={r.date} className="border-t border-line-soft">
                    <td className="px-4 py-3 text-fg">{r.date}</td>
                    <td className="px-4 py-3 text-muted-2">{r.scope}</td>
                    <td className="px-4 py-3 text-ok">
                      {(r.score * 100).toFixed(1)}%
                    </td>
                    <td className="px-4 py-3 text-muted">
                      {r.caught} / {r.total_mutations}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        <div className="mt-10 border-t border-line-soft pt-6 text-[11px] uppercase tracking-[1.5px] text-muted-2">
          v0.2 Witness Loop &middot;{" "}
          <Link href="/lineage" className="hover:text-fg-soft">
            Back to lineage
          </Link>{" "}
          &middot;{" "}
          <a
            href="https://github.com/open-covenant/covenant/blob/main/docs/provenance/covenant-mutants-nightly.md"
            target="_blank"
            rel="noopener noreferrer"
            className="hover:text-fg-soft"
          >
            Nightly job spec
          </a>
        </div>
      </div>

      <SiteFooter />
    </main>
  );
}
