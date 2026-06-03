import type { Metadata } from "next";
import { CopyAddress } from "../CopyAddress";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

export const metadata: Metadata = {
  title: "About Covenant",
  description:
    "Covenant is an open, host-level operating layer for agentic software: every agent runs under a signed grant, every action leaves a receipt, and the system is built in the open by an autonomous loop.",
  alternates: { canonical: "/about" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/about",
    title: "About Covenant: the operating layer for agentic software",
    description:
      "Permission, not trust. A receipt for every decision. Built in the open by an autonomous loop that never stops.",
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "About Covenant: the operating layer for agentic software",
    description:
      "Permission, not trust. A receipt for every decision. Built in the open by an autonomous loop that never stops.",
    images: "/twitter-image.jpg",
  },
};

const eyebrow = "font-mono text-[11px] uppercase tracking-[0.3em] text-neutral-500";
const headingTitle = "text-[13px] uppercase tracking-[0.25em] text-neutral-100 sm:text-[14px]";
const paragraph = "text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]";
const linkClass =
  "underline decoration-neutral-700 underline-offset-4 transition-colors hover:decoration-neutral-300 hover:text-neutral-50";

type Section = { id: string; label: string; title: string; body: React.ReactNode };

const SECTIONS: Section[] = [
  {
    id: "layer",
    label: "01",
    title: "The operating layer, not a wrapper",
    body: (
      <p className={paragraph}>
        Think of an operating system: it sits between your apps and the hardware and decides what
        each one is allowed to do. Covenant is that layer for AI agents. It exposes eight
        capabilities through one local service (a daemon) over your machine&apos;s own internal
        channels: intent, runtime, memory, identity, permissions, comms, compositor, and settlement.
        It doesn&apos;t replace your operating system, and it isn&apos;t a website you call out to.
        It&apos;s infrastructure that runs on the machine where the work happens, under your control.
      </p>
    ),
  },
  {
    id: "permission",
    label: "02",
    title: "Permission, not trust",
    body: (
      <p className={paragraph}>
        Most software trusts an agent the moment it starts running. It can quietly reach anything the
        program itself can. Covenant flips that. An agent gets no standing access. Every privileged
        action needs a capability: a cryptographically signed permission slip (ed25519) that names
        one specific action, can be narrowed to a scope, expires, and can be revoked. It is checked
        the instant the action is attempted, so an agent can do exactly what you signed for and
        nothing else, and the check runs the same way whether the action succeeds or fails.
      </p>
    ),
  },
  {
    id: "receipt",
    label: "03",
    title: "A receipt for every decision",
    body: (
      <p className={paragraph}>
        Every consequential thing the system does is written down as it happens: who was issued an
        identity, which permissions were checked, what was settled. It all goes into a tamper-evident
        log: append-only and hash-chained, so any later edit breaks the chain and shows. Each entry
        records what kind of event it was, who issued it, and when, and you can verify the whole
        chain yourself, on your own machine. The result is a receipt for every decision the system
        makes, kept no matter the outcome.
      </p>
    ),
  },
  {
    id: "open",
    label: "04",
    title: "Built in the open, by an autonomous loop",
    body: (
      <p className={paragraph}>
        Covenant is built the same way it asks you to run agents. An autonomous engineering loop
        picks a scoped task, writes the code, reviews its own changes, runs the tests, and commits to
        a public repository, around the clock, under a neutral automation identity, with provenance
        on every privileged change. The terminal on the{" "}
        <a href="https://opencovenant.org" className={linkClass}>
          home page
        </a>{" "}
        is not a demo: it is that loop, streaming its real commits as they land.
      </p>
    ),
  },
  {
    id: "local",
    label: "05",
    title: "Local-first, and yours",
    body: (
      <p className={paragraph}>
        One key, on your hardware, is the root of everything. A single ed25519 keypair per install
        signs your permission grants, signs on-chain settlement, and stamps the audit log. One
        identity ties it all together. Your agents, your memory, and your keys stay on your machine
        by default. The core is open source under the{" "}
        <a
          href="https://www.apache.org/licenses/LICENSE-2.0"
          target="_blank"
          rel="noopener noreferrer"
          className={linkClass}
        >
          Apache License 2.0
        </a>
        , and the protocol is public, so how Covenant works is never hidden behind a service you
        can&apos;t inspect.
      </p>
    ),
  },
  {
    id: "real",
    label: "06",
    title: "What's real, stated plainly",
    body: (
      <p className={paragraph}>
        We are careful to separate what ships from what is planned. Today the local control plane is
        real and live-tested across two dozen Rust crates and roughly two thousand tests, including
        more than two hundred that exercise real process, model, and network boundaries.
        Production-grade isolation for untrusted code, networked multi-peer operation, and on-chain
        settlement are on the roadmap, not the changelog. The line between done, experimental, and
        planned is documented in BUILT.md and throughout the docs. If a claim is not true yet, we do
        not make it.
      </p>
    ),
  },
];

const CTA = [
  { label: "Try the sandbox", href: "https://sandbox.opencovenant.org" },
  { label: "Documentation", href: "https://docs.opencovenant.org" },
  { label: "Source", href: "https://github.com/open-covenant/covenant" },
  { label: "Whitepaper", href: "https://doi.org/10.5281/zenodo.20134416" },
];

export default function AboutPage() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-[#030303]">
      <SiteHeader />

      <div className="page-container">
        <h1 className="mb-10 text-[11px] uppercase tracking-[0.4em] text-neutral-400 sm:mb-14">
          About
        </h1>

        <h2 className="max-w-3xl text-balance text-[2.2rem] font-extralight uppercase leading-[1.1] tracking-[2px] text-white sm:text-[2.6rem]">
          An operating system that builds itself
        </h2>

        <p className="mt-6 max-w-2xl text-pretty text-lg font-light leading-relaxed text-neutral-300 sm:text-xl">
          AI agents are starting to act on your computer: running code, moving money, touching your
          files. Covenant is the layer that lets them do that safely. It gives people and agents a
          small set of host-level controls (intent, runtime, memory, identity, permissions, comms, a
          compositor, and settlement) so they can share one machine without having to trust each
          other. It runs where your work runs, not behind someone else&apos;s API.
        </p>

        <ol className="relative mt-16 sm:mt-24">
          {SECTIONS.map((s, i) => {
            const isLast = i === SECTIONS.length - 1;
            return (
              <li
                key={s.id}
                id={s.id}
                className={`relative border-l border-neutral-800/80 pl-8 sm:pl-12 ${isLast ? "" : "pb-14 sm:pb-16"}`}
              >
                <span
                  aria-hidden="true"
                  className="absolute -left-[5px] top-[7px] h-[9px] w-[9px] rounded-full border border-neutral-700 bg-[#030303]"
                />
                <div className="mb-4 flex flex-wrap items-baseline gap-x-4 gap-y-1">
                  <span className={eyebrow}>{s.label}</span>
                  <h2 className={headingTitle}>{s.title}</h2>
                </div>
                {s.body}
              </li>
            );
          })}
        </ol>

        <div className="mt-16 border-t border-neutral-800/80 pt-8 sm:mt-24">
          <div className="mb-4 flex flex-wrap items-baseline gap-x-4 gap-y-1">
            <span className={eyebrow}>$CVNT</span>
            <h2 className={headingTitle}>The token</h2>
          </div>
          <p className={`max-w-2xl ${paragraph}`}>
            $CVNT is Covenant&apos;s token on Solana, with a fixed supply and a renounced mint
            authority. It launched in the open on pump.fun.
          </p>
          <p className={`mt-4 max-w-2xl ${paragraph}`}>
            It runs on a simple loop. As Covenant gets used, the network earns revenue, and that
            revenue is shared automatically: part goes to people who stake $CVNT, part is used to
            buy the token on the open market and lock it away, and the rest funds the treasury. The
            more the network is used, the more value returns to holders and the more supply comes
            off the market. Stake to earn a share of that revenue, paid in SOL, with longer locks
            earning more. You can stake on the{" "}
            <a href="/stake" className={linkClass}>
              stake page
            </a>
            .
          </p>
          <div className="mt-5">
            <span className={`${eyebrow} mb-2 block`}>Contract address</span>
            <CopyAddress
              address="2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump"
              label="Copy $CVNT contract address"
            />
          </div>
        </div>

        <div className="mt-16 flex flex-wrap gap-x-6 gap-y-3 border-t border-neutral-800/80 pt-8 sm:mt-24">
          {CTA.map((c) => (
            <a
              key={c.href}
              href={c.href}
              target="_blank"
              rel="noopener noreferrer"
              className="text-[12px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-50"
            >
              {c.label} →
            </a>
          ))}
        </div>
      </div>

      <SiteFooter className="pb-8" />
    </main>
  );
}
