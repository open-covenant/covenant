import type { Metadata } from "next";
import Image from "next/image";
import Link from "next/link";
import { MobileMenu } from "../MobileMenu";

export const metadata: Metadata = {
  title: "Roadmap — Covenant",
  description:
    "Development milestones for Covenant around the alpha release target on 13.05.2026: hardening, marketplace and SDKs, native integration, distributed operation, and the 1.0 stability commitment.",
  alternates: { canonical: "/roadmap" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/roadmap",
    title: "Roadmap — Covenant",
    description:
      "Development milestones for Covenant around the alpha release target on 13.05.2026.",
  },
};

const NAV_LINKS = [
  { label: "docs", href: "https://docs.opencovenant.org" },
  { label: "roadmap", href: "/roadmap" },
];

const X_URL = "https://x.com/OpenCovenant";
const GITHUB_URL = "https://github.com/open-covenant/covenant";

function XIcon({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 24 24"
      aria-hidden="true"
      className={className}
      fill="currentColor"
    >
      <path d="M17.53 3H20.5l-6.49 7.41L21.75 21h-6.18l-4.84-6.34L5.16 21H2.18l6.94-7.93L1.75 3h6.34l4.38 5.79L17.53 3Zm-1.08 16.2h1.71L7.66 4.7H5.83l10.62 14.5Z" />
    </svg>
  );
}

function GithubIcon({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 24 24"
      aria-hidden="true"
      className={className}
      fill="currentColor"
    >
      <path
        fillRule="evenodd"
        clipRule="evenodd"
        d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0 1 12 6.844c.85.004 1.705.115 2.504.337 1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.203 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.31.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.02 10.02 0 0 0 22 12.017C22 6.484 17.522 2 12 2Z"
      />
    </svg>
  );
}

type Milestone = {
  code: string;
  title: string;
  status: string;
  live?: boolean;
  intro: string;
  bullets: string[];
};

const MILESTONES: Milestone[] = [
  {
    code: "M0",
    title: "Alpha",
    status: "Target · 13.05.2026",
    intro:
      "Source-built local control plane for engineers and researchers. The alpha boundary is explicit: local daemon, CLI, policy, memory, audit, A2A, MCP, local receipts, and provenance evidence.",
    bullets: [
      "Daemon, CLI, IPC, local HTTP gateway, identity, permissions, memory, audit, A2A, MCP, budget, and local receipt ledger",
      "Trusted-local subprocess runtime plus fail-closed handling for sandbox-required manifests",
      "Opt-in Linux gVisor validation path where host prerequisites are met",
      "Autonomous workflow records, live coverage matrix, identity guards, and commit-scoped provenance envelopes",
      "Explicit non-claims for production sandboxing, on-chain settlement, public release signing, package installers, stable SDKs, marketplace, and multi-host production",
      "Apache 2.0 core",
    ],
  },
  {
    code: "M1",
    title: "Hardening",
    status: "Next",
    intro:
      "Production hardening across the runtime, tooling, and distribution surface.",
    bullets: [
      "Linux gVisor runner promoted into repeatable CI with pinned fixtures",
      "Per-agent LLM routing with fallback chains and budget-aware escalation",
      "Terminal user interface (Ratatui)",
      "Live web search enabled by default via Brave or SerpAPI",
      "Signed installers: Homebrew, deb and rpm packages, macOS notarization",
      "Mid-task graceful save when an agent reaches its resource budget",
      "Versioning for IPC and capability wire formats",
    ],
  },
  {
    code: "M2",
    title: "Marketplace and SDKs",
    status: "Upcoming",
    intro:
      "On-chain marketplace foundations and a published SDK surface.",
    bullets: [
      "On-chain agent registry with slashable identity stakes",
      "Reputation primitive",
      "SDKs published to PyPI (Python), npm (TypeScript), and crates.io (Rust)",
      "Adapters for existing agent frameworks: LangGraph and CrewAI",
    ],
  },
  {
    code: "M3",
    title: "Native Integration",
    status: "Upcoming",
    intro:
      "Deeper integration with the host operating system and end-user surfaces.",
    bullets: [
      "Compositor v1 with native Wayland integration",
      "Orchestrator agent",
      "Memory compaction across the working, episodic, and long-term tiers",
      "First-run onboarding experience",
      "Read-only mobile companion application",
    ],
  },
  {
    code: "M4",
    title: "Distributed",
    status: "Upcoming",
    intro:
      "Multi-host operation, federated identity, and cross-organization workflows.",
    bullets: [
      "Firecracker microVM isolation",
      "Cross-host communication with name@host.tld registry resolution",
      "Multi-device memory synchronization for a single identity",
      "Agent migration across hosts",
      "Trust flows for cross-organization marketplace transactions",
    ],
  },
  {
    code: "M5",
    title: "1.0 Release",
    status: "Upcoming",
    intro: "Long-term API stability and the formal 1.0 commitment.",
    bullets: [
      "Stable v1 wire formats for IPC, capabilities, and agent manifests",
      "Long-term support release line",
      "Bug bounty program",
      "Comprehensive documentation across all primitives and public APIs",
      "Conformance suite for third-party covenant-compatible runtimes",
    ],
  },
];

export default function RoadmapPage() {
  return (
    <main className="relative min-h-screen overflow-x-hidden bg-[#030303]">
      <header
        className="pointer-events-none absolute inset-x-0 top-0 z-20 flex justify-center pt-[max(35px,env(safe-area-inset-top))] sm:pt-[max(59px,env(safe-area-inset-top))]"
      >
        <Link href="/" className="pointer-events-auto" aria-label="Covenant home">
          <Image
            src="/logo.svg"
            alt="covenant"
            width={255}
            height={54}
            priority
            className="h-[42px] w-auto opacity-95 sm:h-[60px]"
          />
        </Link>
      </header>

      <div
        className="absolute left-2 z-20 sm:left-8 sm:top-10"
        style={{ top: "max(0.5rem, env(safe-area-inset-top))" }}
      >
        <div className="sm:hidden">
          <MobileMenu items={NAV_LINKS} />
        </div>
        <nav className="hidden items-center gap-3 sm:flex">
          {NAV_LINKS.map((item) => (
            <Link
              key={item.href}
              href={item.href}
              className="px-3 py-3 text-[12px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-50"
            >
              {item.label}
            </Link>
          ))}
        </nav>
      </div>

      <nav
        className="absolute right-2 z-20 flex items-center gap-1 sm:right-8 sm:top-10 sm:gap-3"
        style={{ top: "max(0.5rem, env(safe-area-inset-top))" }}
      >
        <a
          href={X_URL}
          target="_blank"
          rel="noopener noreferrer"
          aria-label="Covenant on X"
          className="p-3 text-neutral-400 transition-colors hover:text-neutral-50"
        >
          <XIcon className="h-5 w-5" />
        </a>
        <a
          href={GITHUB_URL}
          target="_blank"
          rel="noopener noreferrer"
          aria-label="Covenant on GitHub"
          className="p-3 text-neutral-400 transition-colors hover:text-neutral-50"
        >
          <GithubIcon className="h-5 w-5" />
        </a>
      </nav>

      <div className="mx-auto max-w-2xl px-6 pb-32 pt-[180px] sm:max-w-3xl sm:px-8 sm:pt-[220px]">
        <h1 className="mb-16 text-[11px] uppercase tracking-[0.4em] text-neutral-400 sm:mb-24">
          Roadmap
        </h1>

        <ol className="relative">
          {MILESTONES.map((m, i) => {
            const isLast = i === MILESTONES.length - 1;
            return (
              <li
                key={m.code}
                className={`relative border-l border-neutral-800/80 pl-8 sm:pl-12 ${isLast ? "" : "pb-14 sm:pb-16"}`}
              >
                <span
                  aria-hidden="true"
                  className={
                    m.live
                      ? "absolute -left-[5px] top-[7px] h-[9px] w-[9px] rounded-full bg-neutral-100"
                      : "absolute -left-[5px] top-[7px] h-[9px] w-[9px] rounded-full border border-neutral-700 bg-[#030303]"
                  }
                />
                <div className="mb-3 flex flex-wrap items-baseline gap-x-4 gap-y-1">
                  <span className="font-mono text-[11px] uppercase tracking-[0.3em] text-neutral-500">
                    {m.code}
                  </span>
                  <h2 className="text-[13px] uppercase tracking-[0.25em] text-neutral-100 sm:text-[14px]">
                    {m.title}
                  </h2>
                  <span
                    className={
                      m.live
                        ? "ml-auto text-[10px] uppercase tracking-[0.25em] text-neutral-200"
                        : "ml-auto text-[10px] uppercase tracking-[0.25em] text-neutral-600"
                    }
                  >
                    {m.status}
                  </span>
                </div>
                <p className="mb-4 text-[13px] leading-relaxed text-neutral-400 sm:text-[14px]">
                  {m.intro}
                </p>
                <ul className="space-y-2">
                  {m.bullets.map((b) => (
                    <li
                      key={b}
                      className="flex gap-3 text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]"
                    >
                      <span aria-hidden="true" className="select-none text-neutral-600">
                        ·
                      </span>
                      <span>{b}</span>
                    </li>
                  ))}
                </ul>
              </li>
            );
          })}
        </ol>
      </div>

      <footer className="px-4 pb-8 text-center text-[10px] tracking-widest text-neutral-500 uppercase sm:text-[11px]">
        open agent-native operating layer
      </footer>
    </main>
  );
}
