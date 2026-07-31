import type { Metadata } from "next";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

const TITLE = "Covenant Evidence: inspect public agent records";
const DESCRIPTION =
  "A read-only MCP that returns public Solana registration, transfer-observation, and signature data. It does not establish identity, delivery, quality, or whether a payment is safe.";

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
    title: "Settlement activity",
    tool: "covenant_reputation",
    body: "Returns bounded public USDC transfer observations and coverage metadata. The legacy score is a heuristic, not proof of jobs, delivery, quality, or reputation.",
  },
  {
    title: "Registration",
    tool: "covenant_agent_passport",
    body: "Checks whether the supplied asset has a MIP-014 registration, belongs to the Covenant collection, and carries a record attributed to Covenant by the configured data source. Registration and record authorship do not prove who operates the agent or whether a claim is true.",
  },
  {
    title: "Signature",
    tool: "covenant_verify",
    body: "Checks whether a payload matches a signature under the published Covenant key. A passing signature authenticates Covenant as publisher; it does not validate the claim.",
  },
];

export default function GuardPage() {
  return (
    <>
      <SiteHeader />
      <main className="mx-auto w-full max-w-7xl px-5 pb-24 pt-[88px] sm:px-8 sm:pt-[120px]">
        <p className={eyebrow}>
          registration &middot; activity &middot; signatures
        </p>
        <h1 className="mt-4 text-2xl font-extralight tracking-[0.18em] text-neutral-50 sm:text-3xl">
          Covenant Evidence
        </h1>
        <p className={`${paragraph} mt-5 max-w-2xl`}>
          Covenant Evidence is a read-only evidence reader. It returns public
          registration records, observed transfer activity, and signature
          results for the caller&apos;s own policy to evaluate. It does not
          approve or block payments.
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
            {`> covenant_reputation  7Xk9…3fQ2\n  observed transfers 0 · coverage bounded · legacy score 12 / 1000\n\n> covenant_agent_passport  4mNp…8vLd\n  no MIP-014 registration observed · no matching Covenant record\n\n> covenant_verify  signed statement\n  FAIL · signature does not match the contents`}
          </code>
          <p className={`${paragraph} mt-3 text-neutral-500`}>
            These are evidence observations, not a trust verdict. The calling
            agent or wallet decides what to do with them.
          </p>
        </section>

        <section className="mt-12">
          <p className={eyebrow}>limits</p>
          <p className={`${paragraph} mt-3 max-w-2xl`}>
            Chain and indexer data can be stale, incomplete, or misleading.
            Registration is not real-world identity; a settlement is not proof
            of delivery; and a signature proves authorship, not truth. The tools
            return the underlying observations so callers can apply their own
            policy.
          </p>
        </section>

        <section className="mt-12">
          <p className={eyebrow}>source &middot; registry</p>
          <p className={`${paragraph} mt-3`}>
            Apache-2.0 and open source, so the implementation is auditable. The
            server is listed in the official{" "}
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
