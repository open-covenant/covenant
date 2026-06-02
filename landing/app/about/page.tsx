import type { Metadata } from "next";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

export const metadata: Metadata = {
  title: "About — Covenant",
  description:
    "Covenant is an open, host-level operating layer for agentic software: every agent runs under a signed grant, every action leaves a receipt, and the system is built in the open by an autonomous loop.",
  alternates: { canonical: "/about" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/about",
    title: "About Covenant — the operating layer for agentic software",
    description:
      "Permission, not trust. A receipt for every decision. Built in the open by an autonomous loop that never stops.",
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "About Covenant — the operating layer for agentic software",
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
        Covenant sits above the operating system as a host-level control plane. Eight primitives —
        intent, runtime, memory, identity, permissions, comms, compositor, and settlement — are
        reached through a single daemon over local IPC and a loopback HTTP gateway. It does not
        replace your kernel and it is not a hosted endpoint: it is infrastructure that runs on the
        machine where the work happens, under your control.
      </p>
    ),
  },
  {
    id: "permission",
    label: "02",
    title: "Permission, not trust",
    body: (
      <p className={paragraph}>
        Agents do not get ambient access to your system. Every privileged action is gated by a
        capability — an ed25519-signed token that names a specific action and an optional scope, is
        checked at the moment of dispatch, and carries expiry and revocation. An agent can do
        exactly what you have signed for, and the check runs the same way whether the call succeeds
        or fails.
      </p>
    ),
  },
  {
    id: "receipt",
    label: "03",
    title: "A receipt for every decision",
    body: (
      <p className={paragraph}>
        Evidence is a first-class concern, not an afterthought. Identity issuance, permission
        checks, and settlement all write to an append-only, hash-chained audit log. Every grant,
        dispatch, and receipt is recorded with a structured kind, an issuer, and a timestamp, and
        the chain&apos;s integrity can be verified locally. The result is a receipt for every
        decision the system makes — kept regardless of outcome.
      </p>
    ),
  },
  {
    id: "open",
    label: "04",
    title: "Built in the open, by an autonomous loop",
    body: (
      <p className={paragraph}>
        Covenant is built the way it asks you to run agents. An autonomous engineering loop selects
        scoped work, implements it, reviews its own diff, runs verification, and commits to a public
        repository under a neutral automation identity — continuously, with provenance on every
        privileged change. The terminal on the{" "}
        <a href="https://opencovenant.org" className={linkClass}>
          home page
        </a>{" "}
        is that loop, streaming its real commits as they land.
      </p>
    ),
  },
  {
    id: "local",
    label: "05",
    title: "Local-first, and yours",
    body: (
      <p className={paragraph}>
        A single ed25519 keypair per install is the root of identity: the same key signs capability
        grants, signs settlement transactions, and stamps the audit log. Your agents, your memory,
        and your keys stay on your hardware by default. The core is open source under the{" "}
        <a
          href="https://www.apache.org/licenses/LICENSE-2.0"
          target="_blank"
          rel="noopener noreferrer"
          className={linkClass}
        >
          Apache License 2.0
        </a>
        , and the protocol surface is public, so nothing about how Covenant works is hidden behind a
        service.
      </p>
    ),
  },
  {
    id: "real",
    label: "06",
    title: "What's real, stated plainly",
    body: (
      <p className={paragraph}>
        We separate what ships from what is planned. Today the local control plane is real and
        live-tested across two dozen Rust crates and roughly two thousand tests, including more than
        two hundred that exercise real process, model, and network boundaries. Production-grade
        isolation for hostile code, networked multi-peer operation, and on-chain settlement are on
        the roadmap — not the changelog. Covenant&apos;s honesty boundary is documented in BUILT.md,
        and the documentation marks implemented, experimental, and planned work as distinct. If a
        claim is not yet true, we do not make it.
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

        <p className="max-w-3xl text-pretty text-2xl font-light leading-snug text-neutral-100 sm:text-[2rem]">
          Not a chatbot. Not an agent framework. An open operating layer where every agent runs
          under a <span className="text-white">signed grant</span>, every action leaves a{" "}
          <span className="text-white">receipt</span>, and the system itself is built in the open by
          an <span className="text-white">autonomous loop that never stops</span> — live, in public,
          on the record.
        </p>

        <p className={`mt-8 max-w-2xl text-neutral-400 ${paragraph}`}>
          Covenant is the coordination layer for agentic software. It gives humans and agents eight
          host-level primitives — intent, runtime, memory, identity, permissions, comms, a
          compositor, and settlement — so they can safely share one computer. It runs where your
          work runs, not behind someone else&apos;s API.
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
