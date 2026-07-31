import Link from "next/link";
import { HeroMesh } from "./HeroMesh";
import { SiteFooter } from "./SiteFooter";
import { SiteHeader } from "./SiteHeader";
import { PRIMITIVES } from "./_primitives";

const eyebrow = "font-mono text-[11px] uppercase tracking-[0.3em] text-neutral-400";
const sectionTitle =
  "mt-3 text-xl font-extralight uppercase tracking-[0.18em] text-neutral-50 sm:text-2xl";
const paragraph = "text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]";
const card = "rounded border border-neutral-800 bg-neutral-950/60 p-6";
const cardLink =
  "mt-4 inline-flex items-center gap-1 font-mono text-[11px] uppercase tracking-[0.22em] text-neutral-300 transition-colors hover:text-neutral-50";

const PRODUCTS = [
  {
    name: "Evidence",
    href: "/guard",
    tag: "read-only MCP",
    body: "Inspect public registration records, bounded transfer observations, and Covenant signatures. Evidence for your own policy—not an identity, reputation, or payment-safety verdict.",
  },
  {
    name: "Escrow",
    href: "/robinhood",
    tag: "contract-gated escrow",
    body: "For funds deposited in the Robinhood Chain escrow, the contract enforces total, per-call, provider, and expiry bounds. Optional quality-gated payout follows the configured attestor's signed verdict.",
  },
];

export default function Page() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-[#030303]">
      <link rel="preload" as="image" href="/hero-bg.jpg" type="image/jpeg" fetchPriority="high" />

      <div className="sr-only">
        <h1>Covenant: open agent-native operating layer</h1>
        <p>
          Covenant is an open, local-first coordination layer for agentic
          software. It gives humans and agents eight host-level primitives
          (intent, runtime, memory, identity, permissions, communication, a
          compositor, and settlement) for coordinating on one computer with
          explicit capability and audit boundaries. Settlement is currently
          local-first, with separately deployed onchain components. Try the{" "}
          <a href="https://sandbox.opencovenant.org">interactive sandbox</a>,
          read the{" "}
          <a href="https://docs.opencovenant.org/concepts">
            documentation on the eight primitives
          </a>
          , review the <Link href="/roadmap">development roadmap</Link>, or read
          the{" "}
          <a href="https://doi.org/10.5281/zenodo.20134416">
            technical whitepaper
          </a>
          .
        </p>
      </div>

      <SiteHeader />

      {/* Hero */}
      <section className="px-6 pt-24 sm:pt-28">
        <div className="mx-auto flex max-w-7xl flex-col items-center gap-6 text-center">
          <div className="relative aspect-[1168/774] w-full overflow-hidden rounded-lg sm:aspect-auto sm:h-[46vh]">
            <HeroMesh src="/hero-bg.jpg" />
          </div>
          <div className="flex max-w-2xl flex-col gap-3 sm:gap-4">
            <h2 className="text-balance text-[1.6rem] font-extralight uppercase leading-[1.15] tracking-[1px] text-white sm:text-[2.2rem] sm:leading-[1.1] sm:tracking-[2px]">
              An open operating layer for bounded agents
            </h2>
            <p className="text-balance text-[15px] font-light leading-relaxed text-neutral-200 sm:text-lg">
              Not a chatbot. Not an agent framework. Signed capabilities,
              durable memory, daemon-mediated tools, and append-only audit
              records for agents sharing a host. The current release is
              local-first; production isolation for hostile code and
              wallet-level signing enforcement are not provided by the default
              runtime.
            </p>
          </div>
          <div className="flex flex-wrap items-center justify-center gap-3">
            <a
              href="https://sandbox.opencovenant.org"
              target="_blank"
              rel="noopener noreferrer"
              aria-label="Try the interactive Covenant sandbox"
              className="rounded-full bg-white px-6 py-2.5 text-[11px] uppercase tracking-[0.28em] text-black transition-colors hover:bg-neutral-200 sm:text-[12px]"
            >
              Try the sandbox →
            </a>
            <Link
              href="/live"
              className="rounded-full border border-neutral-700/50 px-6 py-2.5 text-[11px] uppercase tracking-[0.28em] text-neutral-400 transition-colors hover:border-neutral-500 hover:text-neutral-100 sm:text-[12px]"
            >
              Watch development live →
            </Link>
          </div>
        </div>
      </section>

      {/* Eight primitives */}
      <section className="mx-auto mt-24 max-w-7xl border-t border-neutral-900 px-6 pt-14">
        <p className={eyebrow}>eight host-level primitives</p>
        <h3 className={sectionTitle}>Host primitives for agent coordination</h3>
        <div className="mt-8 grid gap-x-8 gap-y-6 sm:grid-cols-2 lg:grid-cols-4">
          {PRIMITIVES.slice(0, 8).map((p) => (
            <a key={p.slug} href={p.docHref} className="group block">
              <h4 className="font-mono text-[12px] uppercase tracking-[0.22em] text-neutral-100 transition-colors group-hover:text-white">
                {p.term}
              </h4>
              <p className="mt-2 text-[12.5px] leading-relaxed text-neutral-500">{p.gloss}</p>
            </a>
          ))}
        </div>
      </section>

      {/* Products */}
      <section className="mx-auto mt-24 max-w-7xl border-t border-neutral-900 px-6 pt-14">
        <p className={eyebrow}>what you plug in</p>
        <h3 className={sectionTitle}>Evidence and policy surfaces</h3>
        <div className="mt-8 grid gap-4 sm:grid-cols-2">
          {PRODUCTS.map((prod) => (
            <div key={prod.href} className={card}>
              <div className="flex items-baseline justify-between gap-3">
                <h4 className="text-[15px] font-extralight uppercase tracking-[0.2em] text-neutral-50">{prod.name}</h4>
                <span className="font-mono text-[10.5px] uppercase tracking-[0.22em] text-neutral-500">{prod.tag}</span>
              </div>
              <p className={`${paragraph} mt-3 text-neutral-400`}>{prod.body}</p>
              <Link href={prod.href} className={cardLink}>
                {prod.name === "Evidence"
                  ? "Explore evidence →"
                  : "See contract evidence →"}
              </Link>
            </div>
          ))}
        </div>
      </section>

      {/* Builds itself */}
      <section className="mx-auto mt-24 max-w-7xl border-t border-neutral-900 px-6 pt-14">
        <p className={eyebrow}>inspectable development</p>
        <h3 className={sectionTitle}>A bounded, public engineering loop</h3>
        <p className={`${paragraph} mt-5 max-w-2xl`}>
          Covenant is developed in public with a bounded agent-assisted loop.
          Task records, validation output, review gates, and provenance
          artifacts make its work inspectable. That evidence does not prove
          every command was mediated or that the loop is fully autonomous.
        </p>
        <Link href="/live" className={cardLink}>
          Watch the live build log →
        </Link>
      </section>

      {/* Ecosystem */}
      <section className="mx-auto mt-24 max-w-7xl border-t border-neutral-900 px-6 pt-14">
        <p className={eyebrow}>ecosystem</p>
        <h3 className={sectionTitle}>Speaks the protocols agents already use</h3>
        <p className={`${paragraph} mt-5 max-w-2xl`}>
          Solana settlement integrations, x402 payment surfaces, and MCP and A2A
          adapters are implemented at different maturity levels. Each
          integration documents which capabilities, signatures, and network
          boundaries are actually active.
        </p>
        <Link href="/partners" className={cardLink}>
          See the partners →
        </Link>
      </section>

      {/* CTA */}
      <section className="mx-auto mt-24 max-w-7xl border-t border-neutral-900 px-6 pt-14">
        <div className="flex flex-col items-start gap-4 sm:flex-row sm:items-center">
          <a
            href="https://sandbox.opencovenant.org"
            target="_blank"
            rel="noopener noreferrer"
            className="rounded-full bg-white px-6 py-2.5 text-[11px] uppercase tracking-[0.28em] text-black transition-colors hover:bg-neutral-200 sm:text-[12px]"
          >
            Try the sandbox →
          </a>
          <a
            href="https://docs.opencovenant.org"
            target="_blank"
            rel="noopener noreferrer"
            className="rounded-full border border-neutral-700/50 px-6 py-2.5 text-[11px] uppercase tracking-[0.28em] text-neutral-400 transition-colors hover:border-neutral-500 hover:text-neutral-100 sm:text-[12px]"
          >
            Read the docs →
          </a>
        </div>
      </section>

      <SiteFooter variant="full" className="mt-24 pb-8" />
    </main>
  );
}
