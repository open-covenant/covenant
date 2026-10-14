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

const eyebrow = "font-mono text-[11px] uppercase tracking-[0.3em] text-muted";
const paragraph = "text-[13px] leading-relaxed text-fg-soft sm:text-[14px]";

export default function LivePage() {
  return (
    <>
      <SiteHeader />
      <main className="mx-auto w-full max-w-7xl px-5 pb-24 pt-24 sm:px-8 sm:pt-28">
        <p className={eyebrow}>autonomous &middot; live</p>
        <h1 className="page-title mt-4 text-fg">
          Covenant builds itself
        </h1>
        <p className={`${paragraph} mt-5 max-w-2xl`}>
          Not a metaphor. The same infrastructure Covenant provides, agents running under signed grants
          with every action leaving a receipt, is what builds Covenant. This is the live build log:
          the work the autonomous loop is doing right now, streaming as it happens.
        </p>

        <div className="mt-8 h-[68vh] min-h-[420px] overflow-hidden rounded-[6px] border border-line bg-sunken">
          <AgentTerminal className="h-full w-full" />
        </div>

        <p className={`${paragraph} mt-4 text-muted-2`}>
          Every change goes through Covenant: it is limited to the permissions its agent was granted,
          checked before it takes effect and recorded in a hash-chained audit log. Covenant runs under the
          same rules it gives your agents.
        </p>
      </main>
      <SiteFooter className="pb-8" />
    </>
  );
}
