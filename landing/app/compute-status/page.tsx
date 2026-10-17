import type { Metadata } from "next";
import { pageMetadata } from "../_metadata";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";
import { ComputeStatusView } from "./ComputeStatus";

export const metadata: Metadata = pageMetadata("/compute-status", { title: "Compute status", section: "Compute" }, {
  title: "Compute status · Covenant",
  description: "Current Compute capacity, journal health, independent Solana lease reconciliation and reviewed incidents. Every observation includes its scope and timestamp.",
});

export default function ComputeStatusPage() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-ground">
      <SiteHeader />
      <div className="page-container">
        <p className="mb-10 text-[11px] uppercase tracking-[0.4em] text-muted sm:mb-14">Compute</p>
        <h1 className="type-display max-w-3xl text-balance text-[2.2rem] text-fg sm:text-[2.6rem]">Compute status</h1>
        <p className="mt-6 max-w-2xl text-pretty text-[15px] leading-[1.7] text-fg-soft sm:text-base">
          Current capacity, settlement checks and reviewed incidents for Covenant Compute.
        </p>
        <ComputeStatusView />
      </div>
      <SiteFooter className="pb-8" />
    </main>
  );
}
