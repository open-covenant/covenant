import type { Metadata } from "next";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

const TITLE = "Covenant Guard: the trust layer your agent plugs into";
const DESCRIPTION =
  "A zero-install MCP for Claude Code and Codex. Before your agent trusts or pays another agent, it can check an on-chain track record, confirm a real identity, and verify a signed claim. No install, no keys.";

export const metadata: Metadata = {
  title: TITLE,
  description: DESCRIPTION,
  alternates: { canonical: "/guard" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/guard",
    title: TITLE,
    description: DESCRIPTION,
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: TITLE,
    description: DESCRIPTION,
  },
};

const eyebrow = "font-mono text-[11px] uppercase tracking-[0.3em] text-neutral-400";
const paragraph = "text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]";
const cmdBlock =
  "block overflow-x-auto whitespace-pre rounded border border-neutral-800 bg-neutral-950 px-4 py-3 font-mono text-[12.5px] leading-relaxed text-neutral-100 sm:text-[13px]";

const CHECKS: { title: string; tool: string; body: string }[] = [
  {
    title: "Reputation",
    tool: "covenant_reputation",
    body: "Any Solana wallet's track record, scored 0 to 1000 from public on-chain USDC settlements: jobs settled, distinct counterparties, volume. Self-payments are excluded, so a wallet can't inflate its own number.",
  },
  {
    title: "Identity",
    tool: "covenant_agent_passport",
    body: "Whether an agent is who it claims: registered in the on-chain Agent Identity registry, in the Covenant collection, and carrying a Covenant attestation, with the author of that attestation named.",
  },
  {
    title: "Verify",
    tool: "covenant_verify",
    body: "Any Covenant-signed receipt or claim, checked with ed25519 over a domain-separated hash of the canonical payload. Change one field and the check fails. No trust in this server required.",
  },
];

export default function GuardPage() {
  return (
    <>
      <SiteHeader />
      <main className="mx-auto w-full max-w-4xl px-5 pb-24 pt-14 sm:px-8">
        <p className={eyebrow}>reputation &middot; identity &middot; proof</p>
        <h1 className="mt-4 text-2xl font-extralight tracking-[0.18em] text-neutral-50 sm:text-3xl">
          Covenant Guard
        </h1>
        <p className={`${paragraph} mt-5 max-w-2xl`}>
          Your agent is starting to work with other agents: paying them, delegating to them, trusting
          what they send back. Covenant Guard is the trust layer it plugs into. Before it acts, it can
          look up an on-chain track record, confirm a real identity, and verify a signed claim, so a
          counterparty with no history or a forged receipt gets caught before a coin moves.
        </p>

        <section className="mt-10">
          <p className={eyebrow}>add it &middot; one line, no install</p>
          <code className={`${cmdBlock} mt-3`}>claude mcp add --transport http covenant https://mcp.opencovenant.org/mcp</code>
          <p className={`${paragraph} mt-2 text-neutral-500`}>
            For Codex, add it to <span className="font-mono text-[12px] text-neutral-400">config.toml</span>:
          </p>
          <code className={`${cmdBlock} mt-2`}>
            {`[mcp_servers.covenant]\nurl = "https://mcp.opencovenant.org/mcp"`}
          </code>
          <p className={`${paragraph} mt-2 text-neutral-500`}>
            Hosted and remote. Nothing to download, no keys to manage. The tools are read-only and take
            no credentials.
          </p>
        </section>

        <section className="mt-12 grid gap-4 sm:grid-cols-3">
          {CHECKS.map((c) => (
            <div key={c.title} className="rounded border border-neutral-800 bg-neutral-950/60 p-5">
              <h2 className="text-[13px] uppercase tracking-[0.22em] text-neutral-100">{c.title}</h2>
              <p className="mt-2 font-mono text-[11.5px] text-neutral-500">{c.tool}</p>
              <p className={`${paragraph} mt-3 text-neutral-400`}>{c.body}</p>
            </div>
          ))}
        </section>

        <section className="mt-12">
          <p className={eyebrow}>what your agent sees</p>
          <code className={`${cmdBlock} mt-3`}>
            {`> covenant_reputation  7Xk9…3fQ2\n  score 12 / 1000 · no track record · 0 settled jobs\n\n> covenant_agent_passport  4mNp…8vLd\n  not registered · no Covenant attestation\n\n> covenant_verify  "verified vendor" receipt\n  FAIL · signature does not match the contents\n\n  verdict · untrusted · payment held`}
          </code>
          <p className={`${paragraph} mt-3 text-neutral-500`}>
            No history, no identity, and a forged receipt. Your agent holds the payment and flags it,
            instead of trusting a stranger.
          </p>
        </section>

        <section className="mt-12">
          <p className={eyebrow}>not our word</p>
          <p className={`${paragraph} mt-3 max-w-2xl`}>
            Every fact comes from the chain: reputation from public Solana settlements, identity from the
            on-chain registry, and each Covenant claim signed ed25519. Anyone can check it, and no one can
            forge it, including us. The tools return the raw on-chain data alongside the summary, so your
            agent acts on the source, not on a badge.
          </p>
        </section>

        <section className="mt-12">
          <p className={eyebrow}>source &middot; registry</p>
          <p className={`${paragraph} mt-3`}>
            Apache-2.0, open source, so the thing you trust is auditable. The server is listed in the
            official{" "}
            <a
              className="underline decoration-neutral-700 underline-offset-4 transition-colors hover:text-neutral-50 hover:decoration-neutral-300"
              href="https://registry.modelcontextprotocol.io/v0.1/servers?search=org.opencovenant/guard"
            >
              MCP Registry
            </a>{" "}
            as <span className="font-mono text-[12px] text-neutral-400">org.opencovenant/guard</span>, published under a
            DNS-verified namespace on this domain.
          </p>
        </section>
      </main>
      <SiteFooter />
    </>
  );
}
