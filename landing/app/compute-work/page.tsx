import type { Metadata } from "next";
import { pageMetadata } from "../_metadata";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";
import { ComputeWorkView } from "./ComputeWork";
import { WaitingPilot } from "./WaitingPilot";
import { WORK_PILOT_PHASE } from "./_phase";

export const metadata: Metadata = pageMetadata("/compute-work", { title: "CVNT × NVDAx · Operated with Compute", section: "Compute" }, {
  title: "CVNT/NVDAx pool pilot · Covenant Compute",
  description: "A CVNT/NVDAx pool pilot on Orca. Covenant Compute compares routes, checks liquidity and recommends owner-approved actions. Inspect preparation checks, observations and costs.",
});

export default function ComputeWorkPage() {
  return <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-ground">
    <SiteHeader />
    <div className="page-container">
      <p className="mb-10 text-[11px] uppercase tracking-[0.4em] text-muted sm:mb-14">Covenant Compute / Pool pilot</p>
      <h1 className="type-display max-w-4xl text-balance text-[2.2rem] leading-[1.12] text-fg sm:text-[3.6rem]">CVNT × NVDAx.<br /><span className="text-muted">Operated with Compute.</span></h1>
      <p className="mt-6 max-w-2xl text-pretty text-[15px] leading-[1.8] text-fg-soft sm:text-base">{WORK_PILOT_PHASE === "awaiting_funding"
        ? "We’re preparing a small CVNT/NVDAx pool on Orca, with Covenant Compute checking the market and recommending how to manage its liquidity. The owner approves every transaction."
        : "Our CVNT/NVDAx pool on Orca uses Covenant Compute to check the market and recommend liquidity decisions. The owner approves every transaction."}</p>
      <section className="mt-8 max-w-2xl" aria-labelledby="pairing-rationale-title">
        <h2 id="pairing-rationale-title" className="text-[12px] uppercase tracking-[0.2em] text-fg">Why this pair?</h2>
        <p className="mt-4 text-[13px] leading-[1.8] text-fg-soft sm:text-[14px]">NVIDIA builds AI hardware. Covenant runs agent jobs. The pair connects those two sides of compute and gives our agents a focused market to help operate.</p>
        <p className="mt-4 text-[13px] leading-[1.8] text-muted sm:text-[14px]">Traders still need a good price. We’re comparing $5, $10 and $25 swaps with existing routes before adding liquidity. The goal is a useful direct route whose fees can help cover the cost of running it.</p>
      </section>
      {WORK_PILOT_PHASE === "awaiting_funding" ? <WaitingPilot /> : <ComputeWorkView />}
    </div>
    <SiteFooter className="pb-8" />
  </main>;
}
