import type { Metadata } from "next";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

const DESCRIPTION =
  "Answers to common questions about Covenant: what it is, how it relates to MCP, whether it needs a blockchain, the eight primitives, and its pre-1.0 status.";

export const metadata: Metadata = {
  title: "FAQ: Covenant",
  description: DESCRIPTION,
  alternates: { canonical: "/faq" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/faq",
    title: "Frequently asked questions about Covenant",
    description: DESCRIPTION,
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "Frequently asked questions about Covenant",
    description: DESCRIPTION,
    images: "/twitter-image.jpg",
  },
};

// One source of truth: drives the visible Q&A and the FAQPage JSON-LD so the
// markup text always matches what a reader sees. Answers are self-contained
// (no pronoun antecedents) so answer engines can quote them verbatim.
const FAQ: { id: string; q: string; a: string }[] = [
  {
    id: "what-is-covenant",
    q: "What is Covenant?",
    a: "Covenant is an open, local-first operating layer that lets people and software agents safely share one computer. It runs as a local daemon and exposes eight host-level primitives: intent, runtime, memory, identity, permissions, comms, a compositor, and settlement.",
  },
  {
    id: "open-source",
    q: "Is Covenant open source?",
    a: "Yes. The core is licensed under the Apache License 2.0 and developed in the open at github.com/open-covenant/covenant. It is pre-1.0 software.",
  },
  {
    id: "vs-mcp",
    q: "How is Covenant different from MCP?",
    a: "MCP is a protocol for connecting a model to tools. Covenant is the host-level layer underneath: it provides identity, signed permissions, shared memory, audit, and settlement across agents, and integrates MCP as one of its comms adapters.",
  },
  {
    id: "blockchain",
    q: "Does Covenant require a blockchain?",
    a: "No. Covenant runs fully locally and records settlement receipts to a local ledger by default. On-chain settlement to Solana is an optional, planned boundary; chain fields stay empty unless a settlement integration is configured.",
  },
  {
    id: "eight-primitives",
    q: "What are the eight primitives?",
    a: "Intent, runtime, memory, identity, permissions, comms, compositor, and settlement. Each one is a host-level service that agents and people share, defined term by term in the glossary.",
  },
  {
    id: "production-ready",
    q: "Is Covenant production-ready?",
    a: "Not yet. Covenant is pre-1.0. The local control plane, including the daemon, CLI, identity, permissions, memory, and audit log, is implemented and live-tested. The Solana settlement program is live on mainnet (credits, staking, slashing, on-chain receipt anchoring), though its daemon-driven per-intent lifecycle is not yet production. Production-grade isolation and networked multi-host operation remain on the roadmap, not the changelog.",
  },
  {
    id: "staking",
    q: "What does staking $CVNT do?",
    a: "Locking $CVNT earns a pro-rata share of protocol revenue, paid in SOL, with longer locks earning more. Distribution amounts reflect actual protocol revenue and are not guaranteed.",
  },
];

const FAQ_JSONLD = {
  "@context": "https://schema.org",
  "@type": "FAQPage",
  "@id": "https://opencovenant.org/faq#faq",
  mainEntity: FAQ.map((f) => ({
    "@type": "Question",
    name: f.q,
    acceptedAnswer: { "@type": "Answer", text: f.a },
  })),
};

const paragraph = "text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]";
const linkClass =
  "text-[12px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-50";

const LINKS = [
  { label: "Glossary", href: "https://docs.opencovenant.org/glossary" },
  { label: "Documentation", href: "https://docs.opencovenant.org" },
  { label: "Roadmap", href: "/roadmap" },
  { label: "Source", href: "https://github.com/open-covenant/covenant" },
];

export default function FaqPage() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-[#030303]">
      <SiteHeader />

      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(FAQ_JSONLD) }}
      />

      <div className="page-container">
        <h1 className="mb-10 text-[11px] uppercase tracking-[0.4em] text-neutral-400 sm:mb-14">
          FAQ
        </h1>

        <h2 className="max-w-3xl text-balance text-[2.2rem] font-extralight uppercase leading-[1.1] tracking-[2px] text-white sm:text-[2.6rem]">
          Frequently asked questions
        </h2>

        <dl className="mt-14 sm:mt-20">
          {FAQ.map((f, i) => (
            <div
              key={f.id}
              id={f.id}
              className={`border-neutral-800/80 py-7 ${i === 0 ? "border-t-0" : "border-t"}`}
            >
              <dt className="max-w-2xl text-[15px] font-light leading-snug text-neutral-100 sm:text-[16px]">
                {f.q}
              </dt>
              <dd className={`mt-3 max-w-2xl ${paragraph}`}>{f.a}</dd>
            </div>
          ))}
        </dl>

        <div className="mt-12 flex flex-wrap gap-x-6 gap-y-3 border-t border-neutral-800/80 pt-8 sm:mt-16">
          {LINKS.map((l) => (
            <a
              key={l.href}
              href={l.href}
              target={l.href.startsWith("http") ? "_blank" : undefined}
              rel={l.href.startsWith("http") ? "noopener noreferrer" : undefined}
              className={linkClass}
            >
              {l.label} →
            </a>
          ))}
        </div>
      </div>

      <SiteFooter className="pb-8" />
    </main>
  );
}
