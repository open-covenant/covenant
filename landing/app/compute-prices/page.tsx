import type { Metadata } from "next";
import { pageMetadata } from "../_metadata";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";
import { PriceHistoryView } from "./PriceHistory";

export const metadata: Metadata = pageMetadata("/compute-prices", { title: "GPU price history", section: "Compute" }, {
  title: "GPU price history · Covenant Compute",
  description: "Compare GPU provider quotes and settled rental costs over time, with clear sources, sample counts and prices per GPU-hour.",
});

export default function ComputePricesPage() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-ground">
      <SiteHeader />
      <div className="page-container">
        <p className="mb-10 text-[11px] uppercase tracking-[0.4em] text-muted sm:mb-14">Compute</p>
        <h1 className="type-display max-w-3xl text-balance text-[2.2rem] text-fg sm:text-[2.6rem]">GPU price history</h1>
        <p className="mt-6 max-w-2xl text-pretty text-[15px] leading-[1.7] text-fg-soft sm:text-base">
          Compare sampled provider quotes with settled rental costs. Every series shows its source, sample count and price per GPU-hour.
        </p>
        <PriceHistoryView />
      </div>
      <SiteFooter className="pb-8" />
    </main>
  );
}
