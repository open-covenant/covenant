import type { Metadata } from "next";
import { pageMetadata } from "../_metadata";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";
import { ComputeWorkView } from "./ComputeWork";

export const metadata: Metadata = pageMetadata("/compute-work", { title: "Trading fees → public work", section: "Compute" }, {
  title: "Community-funded work · Covenant Compute",
  description: "Follow the CVNT/NVDAx pilot from collected trading fees to public contributions. Inspect funding allocations, costs, code and execution evidence.",
});

export default function ComputeWorkPage() {
  return <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-ground">
    <SiteHeader />
    <div className="page-container">
      <p className="mb-10 text-[11px] uppercase tracking-[0.4em] text-muted sm:mb-14">Covenant Compute / Public work</p>
      <h1 className="type-display max-w-4xl text-balance text-[2.2rem] leading-[1.12] text-fg sm:text-[3.6rem]">Trading fees.<br /><span className="text-muted">Inspectable work.</span></h1>
      <p className="mt-6 max-w-2xl text-pretty text-[15px] leading-[1.8] text-fg-soft sm:text-base">Follow one CVNT/NVDAx pilot from collected fees to useful public contributions. Every job has a budget, acceptance checks, and an outcome you can inspect.</p>
      <ComputeWorkView />
    </div>
    <SiteFooter className="pb-8" />
  </main>;
}
