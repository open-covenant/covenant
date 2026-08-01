import type { Metadata } from "next";
import { AgentTerminal } from "../AgentTerminal";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

const TITLE = "Build log";
const DESCRIPTION =
  "Covenant is built in the open by an autonomous loop. This page streams its public commit log; provenance coverage is scoped to the implemented build workflow.";

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
          This is the public commit stream produced by Covenant&apos;s
          autonomous build workflow. Implemented privileged steps can be
          capability-gated and recorded, but the stream is not proof that every
          runtime action was mediated or logged.
        </p>

        <div className="mt-8 h-[68vh] min-h-[420px] overflow-hidden rounded-md border border-neutral-800/70 bg-[#050505]">
          <AgentTerminal className="h-full w-full" />
        </div>

        <p className={`${paragraph} mt-4 text-neutral-500`}>
          The repository, validation output, and published provenance show what
          the implemented workflow recorded. They do not establish complete
          host-level mediation.
        </p>
      </main>
      <SiteFooter className="pb-8" />
    </>
  );
}
