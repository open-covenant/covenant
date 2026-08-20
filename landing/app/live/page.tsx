import type { Metadata } from "next";
import { AgentTerminal } from "../AgentTerminal";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

const TITLE = "Build log";
const DESCRIPTION =
  "Covenant is built in the open by the autonomous infrastructure it provides. Every commit runs under a signed grant and leaves a receipt. This is the live build log.";

export const metadata: Metadata = {
  title: TITLE,
  description: DESCRIPTION,
  alternates: { canonical: "/live" },
  openGraph: { type: "website", url: "https://opencovenant.org/live", title: TITLE, description: DESCRIPTION },
  twitter: { card: "summary_large_image", site: "@OpenCovenant", creator: "@OpenCovenant", title: TITLE, description: DESCRIPTION },
};

const eyebrow = "font-mono text-[11px] uppercase tracking-[0.3em] text-neutral-400";
const paragraph = "text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]";

export default function LivePage() {
  return (
    <>
      <SiteHeader />
      <main className="mx-auto w-full max-w-7xl px-5 pb-24 pt-24 sm:px-8 sm:pt-28">
        <p className={eyebrow}>autonomous &middot; live</p>
        <h1 className="mt-4 text-2xl font-extralight tracking-[0.18em] text-neutral-50 sm:text-3xl">
          Covenant builds itself
        </h1>
        <p className={`${paragraph} mt-5 max-w-2xl`}>
          Not a metaphor. The same infrastructure Covenant provides, agents running under signed grants
          with every action leaving a receipt, is what builds Covenant. This is the live build log:
          the work the autonomous loop is doing right now, streaming as it happens.
        </p>

        <div className="mt-8 h-[68vh] min-h-[420px] overflow-hidden rounded-md border border-neutral-800/70 bg-[#050505]">
          <AgentTerminal className="h-full w-full" />
        </div>

        <p className={`${paragraph} mt-4 text-neutral-500`}>
          Every change lands the same way a user&apos;s agent would act through Covenant: scoped by a
          capability, gated before it runs, and recorded in a hash-chained audit. The system holds
          itself to the rules it sells.
        </p>
      </main>
      <SiteFooter className="pb-8" />
    </>
  );
}
