import type { Metadata } from "next";
import { pageMetadata } from "../_metadata";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";
import { ComputeWorkView } from "./ComputeWork";
import { WaitingPilot } from "./WaitingPilot";
import { WORK_PILOT_PHASE } from "./_phase";

export const metadata: Metadata = pageMetadata("/compute-work", { title: "Trading fees → public work", section: "Compute" }, {
  title: "Community-funded work · Covenant Compute",
  description: "A planned CVNT/NVDAx pool on Orca could turn trading fees into useful software work through Covenant Compute. Existing tokens, approved tasks and public receipts.",
});

export default function ComputeWorkPage() {
  return <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-ground">
    <SiteHeader />
    <div className="page-container">
      <p className="mb-10 text-[11px] uppercase tracking-[0.4em] text-muted sm:mb-14">Covenant Compute / Public work</p>
      <h1 className="type-display max-w-4xl text-balance text-[2.2rem] leading-[1.12] text-fg sm:text-[3.6rem]">Trading fees.<br /><span className="text-muted">Useful software.</span></h1>
      <p className="mt-6 max-w-2xl text-pretty text-[15px] leading-[1.8] text-fg-soft sm:text-base">We’re testing whether CVNT/NVDAx trading fees can pay for useful software work.</p>
      <p className="mt-4 max-w-2xl text-[13px] leading-[1.8] text-muted sm:text-[14px]">NVDAx is a token linked to NVIDIA stock. {WORK_PILOT_PHASE === "awaiting_funding" ? "We’re preparing an Orca pool on Solana using the existing CVNT and NVDAx tokens." : "Our Orca pool on Solana pairs the existing CVNT and NVDAx tokens. This page tracks the budget, the work and the receipts."}</p>
      {WORK_PILOT_PHASE === "awaiting_funding" ? <WaitingPilot /> : <ComputeWorkView />}
    </div>
    <SiteFooter className="pb-8" />
  </main>;
}
