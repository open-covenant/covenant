import type { Metadata } from "next";
import { pageMetadata } from "../_metadata";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";
import { RentalChecksView } from "./RentalChecks";
import { rentalChecksRelease as release } from "./_release";

export const metadata: Metadata = pageMetadata(
  "/compute-rental-checks",
  { title: "GPU rental checks", section: "Compute" },
  {
    title: "GPU rental checks · Covenant Compute",
    description: "Public results from scheduled GPU rental checks: funding, authenticated SSH, GPU inspection, settlement and instance cleanup. View the latest verdict and recent history.",
    ...(!release.accepted ? { robots: { index: false, follow: false } } : {}),
  },
);

const heading = "text-[13px] uppercase tracking-[0.25em] text-fg sm:text-[14px]";
const paragraph = "text-[13px] leading-[1.8] text-fg-soft sm:text-[14px]";
const link = "underline decoration-line-strong underline-offset-4 transition-colors hover:decoration-fg-soft hover:text-fg";

export default function ComputeRentalChecksPage() {
  return <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-ground">
    <SiteHeader />
    <div className="page-container">
      <p className="mb-10 text-[11px] uppercase tracking-[0.4em] text-muted sm:mb-14">Compute</p>
      <h1 className="type-display max-w-3xl text-balance text-[2.2rem] text-fg sm:text-[2.6rem]">Real GPU rental checks.</h1>
      <p className="mt-6 max-w-2xl text-pretty text-[15px] leading-[1.7] text-fg-soft sm:text-base">{release.accepted ? "Every six hours, Covenant attempts a real GPU rental: fund, connect, inspect, close and verify cleanup. The results show what completed and what still needs attention." : "A scheduled check follows a GPU rental from funding to cleanup. The planned public report will show each result, its timestamp and any work still unresolved."}</p>
      {!release.accepted && <aside className="mt-8 max-w-3xl border-l border-line-strong pl-5" aria-label="Unpublished release preview"><p className="text-[11px] uppercase tracking-[0.2em] text-muted">Release preview · not published</p><p className={`mt-2 ${paragraph}`}>The rental check, schedule and reporting service are being prepared. A completed production run and verified cleanup are required before this page is activated.</p></aside>}

      {release.accepted ? <RentalChecksView /> : <section className="mt-14 border-y border-line py-7 sm:mt-20" aria-labelledby="preview-title"><h2 id="preview-title" className={heading}>Awaiting production acceptance</h2><p className={`mt-4 max-w-2xl ${paragraph}`}>There is no accepted rental-check history to display yet. Test fixtures are not published as production results.</p></section>}

      <section aria-labelledby="scope-title" className="mt-14 border-t border-line pt-8 sm:mt-20">
        <h2 id="scope-title" className={heading}>What each check covers</h2>
        <ol className="mt-7 grid gap-x-10 gap-y-8 sm:grid-cols-2">{[
          ["01", "Fund and rent", "Use the released buyer client to fund and request a real GPU lease within the pilot budget."],
          ["02", "Connect and inspect", "Authenticate the SSH host identity, open a connection and inspect the GPU exposed to the rented instance."],
          ["03", "Close and reconcile", "Close the lease, verify the settlement on Solana and reconcile the buyer’s charge and unused escrow credit."],
          ["04", "Confirm cleanup", "Check that the provider no longer has the rented instance. An unresolved instance keeps the run from passing."],
        ].map(([number,title,body])=><li key={number}><p className="font-mono text-[10px] tracking-[0.2em] text-muted">{number}</p><h3 className="mt-3 text-[15px] text-fg">{title}</h3><p className={`mt-3 ${paragraph}`}>{body}</p></li>)}</ol>
        <p className={`mt-7 max-w-3xl ${paragraph}`}>A passed run confirms that one rental completed these checks at the recorded time. It does not measure all available GPUs, guarantee a future rental or establish hardware attestation.</p>
      </section>

      <section aria-labelledby="states-title" className="mt-14 border-t border-line pt-8 sm:mt-20">
        <h2 id="states-title" className={heading}>Read the result in context</h2>
        <dl className="mt-7 divide-y divide-line border-y border-line">{[
          ["Passed", "The rental completed every required check, including settlement, balance reconciliation and provider cleanup."],
          ["In progress", "The run has started. A final result has not been confirmed."],
          ["Blocked", "A requirement such as capacity, configuration or available budget prevented a complete check. The recorded reason explains the limit."],
          ["Failed", "The run recorded a check that did not succeed. Any remaining settlement or cleanup is tracked separately."],
          ["Recovery complete", "Outstanding settlement or cleanup was resolved after an interruption. This does not turn an incomplete rental into a passed GPU check."],
          ["Report overdue or unavailable", "The reporting heartbeat is stale or cannot be read. The current state is unknown; historical results keep their original verdicts."],
          ["Paused", "New rentals are intentionally stopped, including when the rolling daily allowance or total pilot budget is exhausted."],
        ].map(([term,definition])=><div key={term} className="grid gap-2 py-4 sm:grid-cols-[190px_1fr] sm:gap-7"><dt className="text-[12px] text-fg-soft">{term}</dt><dd className={paragraph}>{definition}</dd></div>)}</dl>
      </section>

      <section aria-labelledby="pilot-title" className="mt-14 border-t border-line pt-8 sm:mt-20">
        <h2 id="pilot-title" className={heading}>Pilot limits</h2>
        <div className={`mt-5 max-w-3xl space-y-4 ${paragraph}`}><p>The schedule is six-hourly, with new rental commitments capped at $2 in a rolling 24-hour window and $20 for the pilot. An insufficient budget pauses new rentals. Outstanding reservations continue to count until they are reconciled.</p><p>Three consecutive missed or unsuccessful windows trigger an operational alert. A missed window is not a completed rental, and an empty capacity response is not proof of a service outage.</p><p>The reporting heartbeat and the rental schedule are separate. A fresh report can contain an older rental result; every result keeps its own timestamp. All displayed times use UTC.</p></div>
      </section>
      <nav aria-label="Rental check resources" className="mt-14 flex flex-wrap gap-x-6 gap-y-3 border-t border-line pt-7 sm:mt-20">{[
        { label:"Compute status", href:"/compute-status" }, { label:"GPU price history", href:"/compute-prices" }, { label:"Covenant Compute", href:"/compute" },
        ...(release.sourceUrl ? [{ label:"Implementation source ↗", href:release.sourceUrl }] : []),
        ...(release.documentationUrl ? [{ label:"Documentation ↗", href:release.documentationUrl }] : []),
        ...(release.releaseUrl ? [{ label:"Released client ↗", href:release.releaseUrl }] : []),
        ...(release.acceptanceUrl ? [{ label:"Release evidence ↗", href:release.acceptanceUrl }] : []),
      ].map(({label,href})=><a key={href} className={`inline-flex min-h-11 items-center text-[12px] text-muted ${link}`} href={href}>{label}</a>)}</nav>
    </div>
    <SiteFooter className="pb-8" />
  </main>;
}
