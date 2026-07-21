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
    name: "Guard",
    href: "/guard",
    tag: "trust layer",
    body: "Before your agent pays or trusts another agent, it checks an on-chain track record, confirms a real identity, and verifies a signed claim. A zero-install MCP for Claude and Codex.",
  },
  {
    name: "Trading",
    href: "/trading",
    tag: "governed execution",
    body: "Brokerages are opening to agents with no real limits. Covenant is the policy gate in front of the order: caps enforced before it's placed, and every decision anchored onchain where anyone can check it.",
  },
];

export default function Page() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-[#030303]">
      <link rel="preload" as="image" href="/hero-bg.jpg" type="image/jpeg" fetchPriority="high" />

      <div className="sr-only">
        <h1>Covenant: open agent-native operating layer</h1>
        <p>
          Covenant is the open, local-first coordination layer for agentic software. It gives humans
          and agents eight host-level primitives (intent, runtime, memory, identity, permissions,
          communication, a compositor, and on-chain settlement) so they can safely share one computer.
          Try the <a href="https://sandbox.opencovenant.org">interactive sandbox</a>, read the{" "}
          <a href="https://docs.opencovenant.org/concepts">documentation on the eight primitives</a>,
          review the <Link href="/roadmap">development roadmap</Link>, or read the{" "}
          <a href="https://doi.org/10.5281/zenodo.20134416">technical whitepaper</a>.
        </p>
      </div>

      <SiteHeader />

      {/* Hero */}
      <section className="px-6 pt-24 sm:pt-28">
        <div className="mx-auto flex max-w-6xl flex-col items-center gap-6 text-center">
          <div className="relative aspect-[1168/774] w-full overflow-hidden rounded-lg sm:aspect-auto sm:h-[46vh]">
            <HeroMesh src="/hero-bg.jpg" />
          </div>
          <div className="flex max-w-2xl flex-col gap-3 sm:gap-4">
            <h2 className="text-balance text-[1.6rem] font-extralight uppercase leading-[1.15] tracking-[1px] text-white sm:text-[2.2rem] sm:leading-[1.1] sm:tracking-[2px]">
              An operating system that builds itself
            </h2>
            <p className="text-balance text-[15px] font-light leading-relaxed text-neutral-200 sm:text-lg">
              Not a chatbot. Not an agent framework. An open operating layer where every agent runs
              under a <span className="text-white">signed grant</span>, every action leaves a{" "}
              <span className="text-white">receipt</span>, and the system itself is built in the open
              by the <span className="text-white">autonomous infrastructure it provides</span>.
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
              Watch it build itself →
            </Link>
          </div>
        </div>
      </section>

      {/* Eight primitives */}
      <section className="mx-auto mt-24 max-w-6xl border-t border-neutral-900 px-6 pt-14">
        <p className={eyebrow}>eight host-level primitives</p>
        <h3 className={sectionTitle}>One computer, safely shared by humans and agents</h3>
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
      <section className="mx-auto mt-24 max-w-6xl border-t border-neutral-900 px-6 pt-14">
        <p className={eyebrow}>what you plug in</p>
        <h3 className={sectionTitle}>Two ways agents meet the real world safely</h3>
        <div className="mt-8 grid gap-4 sm:grid-cols-2">
          {PRODUCTS.map((prod) => (
            <div key={prod.href} className={card}>
              <div className="flex items-baseline justify-between gap-3">
                <h4 className="text-[15px] font-extralight uppercase tracking-[0.2em] text-neutral-50">{prod.name}</h4>
                <span className="font-mono text-[10.5px] uppercase tracking-[0.22em] text-neutral-500">{prod.tag}</span>
              </div>
              <p className={`${paragraph} mt-3 text-neutral-400`}>{prod.body}</p>
              <Link href={prod.href} className={cardLink}>
                {prod.name === "Guard" ? "Explore Guard →" : "See governed trading →"}
              </Link>
            </div>
          ))}
        </div>
      </section>

      {/* Builds itself */}
      <section className="mx-auto mt-24 max-w-6xl border-t border-neutral-900 px-6 pt-14">
        <p className={eyebrow}>built in the open</p>
        <h3 className={sectionTitle}>The infrastructure builds itself</h3>
        <p className={`${paragraph} mt-5 max-w-2xl`}>
          Covenant is written, in the open, by the autonomous loop it ships. Every commit runs under a
          signed grant, is gated before it executes, and leaves a receipt in a hash-chained audit. The
          system holds itself to the rules it sells you.
        </p>
        <Link href="/live" className={cardLink}>
          Watch the live build log →
        </Link>
      </section>

      {/* Ecosystem */}
      <section className="mx-auto mt-24 max-w-6xl border-t border-neutral-900 px-6 pt-14">
        <p className={eyebrow}>ecosystem</p>
        <h3 className={sectionTitle}>Speaks the protocols agents already use</h3>
        <p className={`${paragraph} mt-5 max-w-2xl`}>
          Settlement on Solana, pay-per-call over x402, tools and agent-to-agent traffic over MCP and
          A2A, and a growing set of partner products that reach agents as capability-scoped, attested
          tools.
        </p>
        <Link href="/partners" className={cardLink}>
          See the partners →
        </Link>
      </section>

      {/* CTA */}
      <section className="mx-auto mt-24 max-w-6xl border-t border-neutral-900 px-6 pt-14">
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
