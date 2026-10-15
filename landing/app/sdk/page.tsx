import type { Metadata } from "next";
import { pageMetadata } from "@/app/_metadata";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";
import { GITHUB_REPO_URL } from "../_brand";

const TITLE = "SDK";
const DESCRIPTION =
  "@covenant-org/sdk — the TypeScript SDK for Covenant's Solana-native protocol surface. Prepare on-chain instructions, expose tools over MCP, and settle over x402.";

export const metadata: Metadata = pageMetadata("/sdk", { title: "Covenant SDK" }, {
  title: TITLE,
  description: DESCRIPTION,
  alternates: { canonical: "/sdk" },
  openGraph: { type: "website", url: "https://opencovenant.org/sdk", title: TITLE, description: DESCRIPTION },
  twitter: { card: "summary_large_image", site: "@OpenCovenant", creator: "@OpenCovenant", title: TITLE, description: DESCRIPTION },
});

const eyebrow = "font-mono text-[11px] uppercase tracking-[0.3em] text-muted";
const paragraph = "text-[13px] leading-relaxed text-fg-soft sm:text-[14px]";
const cmdBlock =
  "block overflow-x-auto whitespace-pre rounded border border-line bg-raised px-4 py-3 font-mono text-[12.5px] leading-relaxed text-fg sm:text-[13px]";
const link =
  "underline decoration-line-strong underline-offset-4 transition-colors hover:text-fg hover:decoration-fg-soft";

const DOES: { title: string; body: string }[] = [
  {
    title: "Prepare instructions",
    body: "Build the Solana instructions for the protocol surface: register an agent, create and settle a task, stake, buy credits, and anchor a receipt batch. You sign and send with your own wallet; the SDK never holds a key.",
  },
  {
    title: "Expose tools over MCP",
    body: "Turn those prepared calls into MCP tools any agent client can reach, so a Claude or Codex agent can transact on the protocol without bespoke glue.",
  },
  {
    title: "Settle over x402",
    body: "Make and take capability-scoped, pay-per-call requests over the x402 payment protocol, with the settlement receipt landing on-chain.",
  },
];

export default function SdkPage() {
  return (
    <>
      <SiteHeader />
      <main className="mx-auto w-full max-w-7xl px-5 pb-24 pt-[88px] sm:px-8 sm:pt-[120px]">
        <p className={eyebrow}>typescript &middot; solana &middot; x402</p>
        <h1 className="page-title mt-4 text-fg">SDK</h1>
        <p className={`${paragraph} mt-5 max-w-2xl`}>
          <span className="font-mono text-[12.5px] text-fg-2">@covenant-org/sdk</span> is the TypeScript
          SDK for Covenant&apos;s Solana-native protocol surface. It prepares the on-chain instructions,
          exposes them as agent tools, and settles over x402, so you build on the protocol without wiring
          the low-level calls by hand.
        </p>

        <section className="mt-10">
          <p className={eyebrow}>install</p>
          <code className={`${cmdBlock} mt-3`}>npm i @covenant-org/sdk</code>
        </section>

        <section className="mt-12 grid gap-4 sm:grid-cols-3">
          {DOES.map((d) => (
            <div key={d.title} className="rounded border border-line bg-raised/60 p-5">
              <h2 className="text-[13px] uppercase tracking-[0.22em] text-fg">{d.title}</h2>
              <p className={`${paragraph} mt-3 text-muted`}>{d.body}</p>
            </div>
          ))}
        </section>

        <section className="mt-12">
          <p className={eyebrow}>source &middot; reference</p>
          <p className={`${paragraph} mt-3 max-w-2xl`}>
            The SDK lives in the monorepo at{" "}
            <a className={link} href={`${GITHUB_REPO_URL}/tree/main/packages/sdk`}>
              packages/sdk
            </a>
            , Apache-2.0. The wire format it speaks is documented in the{" "}
            <a className={link} href="/docs/http-api">
              HTTP API reference
            </a>
            , with the full docs at{" "}
            <a className={link} href="https://docs.opencovenant.org">
              docs.opencovenant.org
            </a>
            .
          </p>
        </section>
      </main>
      <SiteFooter className="pb-8" />
    </>
  );
}
