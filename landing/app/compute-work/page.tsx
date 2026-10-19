import type { Metadata } from "next";
import { pageMetadata } from "../_metadata";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";
import { ComputeWorkView } from "./ComputeWork";
import { WaitingPilot } from "./WaitingPilot";
import { WORK_PILOT_PHASE } from "./_phase";

export const metadata: Metadata = pageMetadata("/compute-work", { title: "Trading fees → public work", section: "Compute" }, {
  title: "Community-funded work · Covenant Compute",
  description: "Why CVNT/NVDAx: connect interest in AI hardware with paid agent work on Covenant Compute. Collected pool fees can fund public jobs with code, costs and receipts.",
});

export default function ComputeWorkPage() {
  return <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-ground">
    <SiteHeader />
    <div className="page-container">
      <p className="mb-10 text-[11px] uppercase tracking-[0.4em] text-muted sm:mb-14">Covenant Compute / Public work</p>
      <h1 className="type-display max-w-4xl text-balance text-[2.2rem] leading-[1.12] text-fg sm:text-[3.6rem]">Trading fees.<br /><span className="text-muted">Useful software.</span></h1>
      <p className="mt-6 max-w-2xl text-pretty text-[15px] leading-[1.8] text-fg-soft sm:text-base">{WORK_PILOT_PHASE === "awaiting_funding" ? "We’re preparing a CVNT/NVDAx pool on Orca to fund public bug fixes, tests and small tools through Covenant Compute." : "Our CVNT/NVDAx pool on Orca uses collected trading fees to fund public bug fixes, tests and small tools through Covenant Compute."}</p>
      <section className="mt-8 max-w-2xl" aria-labelledby="pairing-rationale-title">
        <h2 id="pairing-rationale-title" className="text-[12px] uppercase tracking-[0.2em] text-fg">Why NVDAx?</h2>
        <p className="mt-4 text-[13px] leading-[1.8] text-fg-soft sm:text-[14px]">NVIDIA builds AI hardware. Covenant Compute runs paid agent jobs. Pairing CVNT with NVDAx connects the hardware and execution sides of the AI story.</p>
        <p className="mt-4 text-[13px] leading-[1.8] text-muted sm:text-[14px]">We want people following AI infrastructure to discover Covenant Compute through the pair. Fees earned by our position can then fund public examples of what Compute can deliver, with code, costs and receipts anyone can inspect.</p>
      </section>
      {WORK_PILOT_PHASE === "awaiting_funding" ? <WaitingPilot /> : <ComputeWorkView />}
    </div>
    <SiteFooter className="pb-8" />
  </main>;
}
