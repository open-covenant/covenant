import Link from "next/link";
import { ModuleCurtain } from "./ModuleCurtain";
import { FieldSeam } from "./FieldSeam";
import { ModuleField } from "./ModuleField";
import { SiteFooter } from "./SiteFooter";
import { SiteHeader } from "./SiteHeader";
import { SiteMotion } from "./SiteMotion";
import { FOOTER_PROTOCOLS, RELEASE_STATUS } from "./_brand";
import { PRIMITIVES } from "./_primitives";

const shell = "mx-auto w-full max-w-7xl px-5 sm:px-8";
const eyebrow = "font-mono text-[10px] font-medium uppercase tracking-[0.28em] text-muted";
const sectionTitle = "type-display mt-5 max-w-3xl text-[clamp(1.9rem,3.8vw,3.1rem)]";
const paragraph = "max-w-[36rem] text-[13px] leading-[1.7] text-muted";
const arrowLink =
  "inline-flex items-center gap-2 font-mono text-[10.5px] font-medium uppercase tracking-[0.2em] transition-colors";

// Sections share one ground and meet across a seam of the hero's field.
const sectionPad = "py-[clamp(56px,7vw,104px)]";

const PRODUCTS = [
  {
    name: "Guard",
    href: "/guard",
    tag: "trust layer",
    body: "Before your agent pays or trusts another agent, it checks an on-chain track record, confirms a real identity, and verifies a signed claim. A zero-install MCP for Claude and Codex.",
    cta: "Explore Guard",
  },
  {
    name: "Trading",
    href: "/trading",
    tag: "governed execution",
    body: "Brokerages are opening to agents with no real limits. Covenant is the policy gate in front of the order: caps enforced before it's placed, and every decision anchored onchain where anyone can check it.",
    cta: "See governed trading",
  },
];

export default function Page() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-clip bg-ground">
      <div className="sr-only">
        <h1>Covenant: open agent-native operating layer</h1>
        <p>
          Covenant is the open, local-first operating layer for agentic software. It gives humans
          and agents eight host-level primitives (intent, runtime, memory, identity, permissions,
          communication, a compositor, and on-chain settlement) so they can safely share one computer.
          Try the <a href="https://sandbox.opencovenant.org">interactive sandbox</a>, read the{" "}
          <a href="https://docs.opencovenant.org/concepts">documentation on the eight primitives</a>,
          review the <Link href="/roadmap">development roadmap</Link>, or read the{" "}
          <a href="https://doi.org/10.5281/zenodo.20134416">technical whitepaper</a>.
        </p>
      </div>

      <ModuleCurtain />
      <SiteMotion />
      <SiteHeader />

      {/* Hero: the field drifts as ribbons and resolves into the mark as the
          stage scrolls; the headline is split across it. */}
      <section className="hero-stage" aria-labelledby="hero-title">
        <div className="hero-canvas-wrap">
          <div className="hero-canvas-track">
            <div className="hero-sticky">
              <ModuleField className="hero-scene" />
              <div className="hero-veil" />
            </div>
          </div>
        </div>

        <div className="hero-overlay">
          <h2 id="hero-title" className="sr-only">
            An operating system that builds itself
          </h2>
          <div className="hero-title-stack">
            <div className="flex flex-col items-start gap-6">
              <p className="hero-meta">Covenant · open agent-native operating layer</p>
              <p className="hero-display" aria-hidden="true">
                <span className="block">An operating</span>
                <span className="block">system</span>
              </p>
            </div>

            <div className="w-full">
              <div className="hero-meta-row mb-8 flex flex-col items-start gap-3 md:flex-row md:items-end md:justify-between md:gap-8">
                <p className="hero-meta hero-meta-dim">Open source · Apache-2.0</p>
                <div className="flex flex-wrap items-center gap-x-8 gap-y-3">
                  <p className="hero-meta hero-meta-dim">{FOOTER_PROTOCOLS.join(" · ")}</p>
                  <p className="hero-meta">{RELEASE_STATUS}</p>
                </div>
              </div>

              <div className="flex flex-col items-start gap-10 md:flex-row md:items-end md:gap-8">
                <div className="hero-copy-col w-full md:w-1/3">
                  <p className="hero-lede">
                    Not a chatbot. Not an agent framework. An open operating layer where every agent
                    runs under a <strong>signed grant</strong>, every action leaves a{" "}
                    <strong>receipt</strong>, and the system itself is built in the open by the{" "}
                    <strong>autonomous infrastructure it provides</strong>.
                  </p>
                  <div className="mt-7 flex flex-wrap gap-3">
                    <a
                      href="https://sandbox.opencovenant.org"
                      target="_blank"
                      rel="noopener noreferrer"
                      aria-label="Try the interactive Covenant sandbox"
                      className="btn btn-primary"
                    >
                      Try the sandbox <span aria-hidden="true">→</span>
                    </a>
                    <Link href="/live" className="btn btn-secondary">
                      Watch it build itself
                    </Link>
                  </div>
                </div>
                <div className="flex w-full flex-1 md:justify-end">
                  <p className="hero-display md:text-right" aria-hidden="true">
                    <span className="block">that builds</span>
                    <span className="block">itself</span>
                  </p>
                </div>
              </div>
            </div>
          </div>
        </div>
      </section>

      {/* Eight primitives */}
      <section className="pb-[clamp(56px,7vw,104px)] pt-[clamp(72px,9vw,128px)]" aria-labelledby="primitives-title">
        <div className={shell}>
          <div className="grid gap-8 md:grid-cols-[minmax(0,1.3fr)_minmax(280px,0.7fr)] md:items-end md:gap-16">
            <div>
              <p className={eyebrow}>Eight host-level primitives</p>
              <h2 id="primitives-title" className={sectionTitle} data-reveal-words>
                One computer, safely shared by humans and agents
              </h2>
            </div>
            <p className={paragraph}>
              Covenant gives humans and agents eight host-level primitives: intent, runtime, memory,
              identity, permissions, communication, a compositor, and on-chain settlement, so they can
              safely share one computer.
            </p>
          </div>

          <ol className="mt-14 grid border-t border-line sm:grid-cols-2 lg:grid-cols-4">
            {PRIMITIVES.slice(0, 8).map((p, i) => (
              <li key={p.slug} className="reveal border-b border-line sm:[&:nth-child(odd)]:border-r lg:border-r lg:[&:nth-child(4n)]:border-r-0">
                <a href={p.docHref} className="group flex h-full flex-col gap-4 p-6 transition-colors hover:bg-raised">
                  <span className="font-mono text-[10px] tabular-nums tracking-[0.2em] text-faint">
                    {String(i + 1).padStart(2, "0")}
                  </span>
                  <span className="font-mono text-[12px] font-semibold uppercase tracking-[0.2em] text-fg">
                    {p.term}
                  </span>
                  <span className="text-[12.5px] leading-relaxed text-muted">{p.gloss}</span>
                  <span
                    aria-hidden="true"
                    className="mt-auto font-mono text-[11px] text-faint transition-colors group-hover:text-blue"
                  >
                    →
                  </span>
                </a>
              </li>
            ))}
          </ol>
        </div>
      </section>

      <FieldSeam seed={0.4} />

      {/* Products */}
      <section className={sectionPad} aria-labelledby="products-title">
        <div className={shell}>
          <p className={eyebrow}>What you plug in</p>
          <h2 id="products-title" className={sectionTitle} data-reveal-words>
            Two ways agents meet the real world safely
          </h2>
          <div className="mt-12 grid gap-4 md:grid-cols-2">
            {PRODUCTS.map((prod) => (
              <article
                key={prod.href}
                className="reveal flex flex-col rounded-[4px] border border-line bg-raised p-7"
              >
                <div className="flex items-baseline justify-between gap-3">
                  <h3 className="type-display text-[1.6rem] text-fg">{prod.name}</h3>
                  <span className="font-mono text-[10px] uppercase tracking-[0.22em] text-muted-2">{prod.tag}</span>
                </div>
                <p className="mt-4 text-[13px] leading-[1.7] text-muted">{prod.body}</p>
                <Link href={prod.href} className={`${arrowLink} mt-8 text-fg hover:text-mint`}>
                  {prod.cta} <span aria-hidden="true">→</span>
                </Link>
              </article>
            ))}
          </div>
        </div>
      </section>

      <FieldSeam seed={1.9} />

      {/* Builds itself */}
      <section className={sectionPad} aria-labelledby="builds-title">
        <div className={shell}>
          <div className="grid gap-8 md:grid-cols-[minmax(0,1.3fr)_minmax(280px,0.7fr)] md:items-end md:gap-16">
            <div>
              <p className={eyebrow}>Built in the open</p>
              <h2 id="builds-title" className={sectionTitle} data-reveal-words>
                The infrastructure <span className="text-ramp">builds itself</span>
              </h2>
            </div>
            <div className="reveal">
              <p className={paragraph}>
                Covenant&apos;s own code is written in the open by agents running on it. Every commit is made
                under a signed grant, checked before it goes through, and leaves a receipt in a hash-chained
                audit log. Covenant runs under the same rules it gives your agents.
              </p>
              <Link href="/live" className={`${arrowLink} mt-6 text-fg hover:text-blue`}>
                Watch the live build log <span aria-hidden="true">→</span>
              </Link>
            </div>
          </div>
        </div>
      </section>

      <FieldSeam seed={2.6} />

      {/* Ecosystem */}
      <section className={sectionPad} aria-labelledby="ecosystem-title">
        <div className={shell}>
          <div className="grid gap-8 md:grid-cols-[minmax(0,1.3fr)_minmax(280px,0.7fr)] md:items-end md:gap-16">
            <div>
              <p className={eyebrow}>Ecosystem</p>
              <h2 id="ecosystem-title" className={sectionTitle} data-reveal-words>
                Speaks the protocols agents already use
              </h2>
            </div>
            <p className={paragraph}>
              Settlement on Solana, pay-per-call over x402, tools and agent-to-agent traffic over MCP and
              A2A, and a growing set of partner products that reach agents as capability-scoped, attested
              tools.
            </p>
          </div>
          <div className="mt-12 flex flex-col gap-8 border-t border-line pt-8 sm:flex-row sm:items-center sm:justify-between">
            <ul className="flex flex-wrap gap-2" aria-label="Protocols">
              {FOOTER_PROTOCOLS.map((name) => (
                <li
                  key={name}
                  className="rounded-full border border-line px-4 py-2 font-mono text-[10.5px] uppercase tracking-[0.2em] text-fg"
                >
                  {name}
                </li>
              ))}
            </ul>
            <Link href="/partners" className="btn btn-primary self-start sm:self-auto">
              See the partners <span aria-hidden="true">→</span>
            </Link>
          </div>
        </div>
      </section>

      <FieldSeam seed={3.3} />

      {/* CTA */}
      <section className={sectionPad} aria-labelledby="cta-title">
        <div className={`${shell} flex flex-col items-start gap-8 md:flex-row md:items-end md:justify-between`}>
          <h2 id="cta-title" className="type-display text-[clamp(2.4rem,6vw,5rem)] leading-[0.84]" data-reveal-words>
            Try the <span className="text-ramp">sandbox</span>
          </h2>
          <div className="flex shrink-0 flex-wrap gap-3">
            <a
              href="https://sandbox.opencovenant.org"
              target="_blank"
              rel="noopener noreferrer"
              className="btn btn-primary"
            >
              Try the sandbox <span aria-hidden="true">→</span>
            </a>
            <a
              href="https://docs.opencovenant.org"
              target="_blank"
              rel="noopener noreferrer"
              className="btn btn-secondary"
            >
              Read the docs <span aria-hidden="true">→</span>
            </a>
          </div>
        </div>
      </section>

      <SiteFooter className="pb-4" />
    </main>
  );
}
