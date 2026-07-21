import type { Metadata } from "next";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";
import { GITHUB_URL } from "../_brand";

const TITLE = "SDK";
const DESCRIPTION =
  "@covenant-org/sdk — the TypeScript SDK for Covenant's Solana-native protocol surface. Prepare on-chain instructions, expose tools over MCP, and settle over x402.";

export const metadata: Metadata = {
  title: TITLE,
  description: DESCRIPTION,
  alternates: { canonical: "/sdk" },
  openGraph: { type: "website", url: "https://opencovenant.org/sdk", title: TITLE, description: DESCRIPTION },
  twitter: { card: "summary_large_image", site: "@OpenCovenant", creator: "@OpenCovenant", title: TITLE, description: DESCRIPTION },
};

const eyebrow = "font-mono text-[11px] uppercase tracking-[0.3em] text-neutral-400";
const paragraph = "text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]";
const cmdBlock =
  "block overflow-x-auto whitespace-pre rounded border border-neutral-800 bg-neutral-950 px-4 py-3 font-mono text-[12.5px] leading-relaxed text-neutral-100 sm:text-[13px]";
const link =
  "underline decoration-neutral-700 underline-offset-4 transition-colors hover:text-neutral-50 hover:decoration-neutral-300";

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
      <main className="mx-auto w-full max-w-7xl px-5 pb-24 pt-14 sm:px-8">
        <p className={eyebrow}>typescript &middot; solana &middot; x402</p>
        <h1 className="mt-4 text-2xl font-extralight tracking-[0.18em] text-neutral-50 sm:text-3xl">SDK</h1>
        <p className={`${paragraph} mt-5 max-w-2xl`}>
          <span className="font-mono text-[12.5px] text-neutral-200">@covenant-org/sdk</span> is the TypeScript
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
            <div key={d.title} className="rounded border border-neutral-800 bg-neutral-950/60 p-5">
              <h2 className="text-[13px] uppercase tracking-[0.22em] text-neutral-100">{d.title}</h2>
              <p className={`${paragraph} mt-3 text-neutral-400`}>{d.body}</p>
            </div>
          ))}
        </section>

        <section className="mt-12">
          <p className={eyebrow}>source &middot; reference</p>
          <p className={`${paragraph} mt-3 max-w-2xl`}>
            The SDK lives in the monorepo at{" "}
            <a className={link} href={`${GITHUB_URL}/tree/main/packages/sdk`}>
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
