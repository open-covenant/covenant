import type { Metadata } from "next";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

export const metadata: Metadata = {
  title: "Development Roadmap: Covenant",
  description:
    "Covenant roadmap: foundation (M0), production & tools (M1), native integration (M2), distributed operation (M3), and 1.0 stability (M4).",
  alternates: { canonical: "/roadmap" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/roadmap",
    title: "Development Roadmap: From M0 Foundation to M4 Stability",
    description:
      "Development milestones for Covenant across the local control plane, distributed agent networks, and 1.0 stability commitment.",
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "Development Roadmap: From M0 Foundation to M4 Stability",
    description:
      "Development milestones for Covenant across the local control plane, distributed agent networks, and 1.0 stability commitment.",
    images: "/twitter-image.jpg",
  },
};

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
    title: "Foundation",
    status: "Available",
    live: true,
    intro:
      "Local control plane for engineers and researchers building governed autonomous software systems.",
    bullets: [
      "Daemon, CLI, TUI, local HTTP gateway, identity, permissions, durable memory, append-only activity log, agent-to-agent messaging, model-context-protocol bridge, budget ledger, and local resource receipts",
      "Process runtime with budget enforcement (preempts on projected overspend, plus a hard time limit) and optional Linux sandboxing",
      "Signed permission lifecycle with grant, scope check, expiry, and revocation",
      "Verifiable workflow records and commit-scoped provenance for every privileged action",
      "Public sandbox at sandbox.opencovenant.org",
      "Live progress streaming: watch an agent work in real time across the CLI, console, and HTTP gateway",
      "Isolated code execution: agents that write, run, and iterate on real code in a contained environment",
      "Unified model provider: plug in Anthropic, OpenAI, DeepSeek, or local Ollama once, with automatic local fallback",
      "Mid-task save and resume when an agent reaches its resource budget",
      "On-chain agent identity and audit-root attestation on Solana mainnet, verifiable via the Covenant Verified check (Metaplex MPL Core)",
      "Settlement program live on Solana mainnet: $CVNT-to-credits, staking, slashing, and on-chain receipt anchoring transact on-chain; the daemon-driven per-intent settlement lifecycle is not yet production",
      "Apache 2.0 core",
    ],
  },
  {
    code: "M1",
    title: "Production, Tools, and Marketplace Foundation",
    status: "Next",
    intro:
      "The next release. The core tools agents need and the marketplace foundation, landing as a single push.",
    bullets: [
      "Plugin catalog: install vetted tools from a one-click catalog inside the console, starting with filesystem access",
      "Production-grade isolated runtime for untrusted agent code on Linux",
      "Signed installers: Homebrew, Debian, RPM, and notarized macOS packages",
      "Stable wire formats for SDK and integration compatibility",
      "Browser tool: agents that navigate, click, and read pages",
      "Git host integration: read repositories, comment on issues, propose pull requests",
      "Replay and state debugger: pick any moment from the activity log, see full state, re-run from there",
      "SDKs published to PyPI, npm, and crates.io",
      "Editor integration for Visual Studio Code",
      "Adapters for existing agent frameworks: LangGraph and CrewAI",
    ],
  },
  {
    code: "M2",
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
    code: "M3",
    title: "Distributed",
    status: "Upcoming",
    intro:
      "Multi-host operation, federated identity, and cross-organization workflows.",
    bullets: [
      "Multi-host operation with name@host.tld resolution for agent teams across machines",
      "Public agent registry with one-line install and signed manifests",
      "Microvm-grade isolation",
      "Multi-device memory synchronization for a single identity",
      "Agent migration across hosts",
      "Trust flows for cross-organization marketplace transactions",
    ],
  },
  {
    code: "M4",
    title: "1.0 Release",
    status: "Upcoming",
    intro: "Long-term API stability and the formal 1.0 commitment.",
    bullets: [
      "Stable v1 wire formats for IPC, permissions, and agent manifests",
      "Long-term support release line",
      "Bug bounty program",
      "Comprehensive documentation across all primitives and public APIs",
      "Conformance suite for third-party covenant-compatible runtimes",
    ],
  },
];

export default function RoadmapPage() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-[#030303]">
      <SiteHeader />

      <div className="page-container">
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
                  <span className="font-mono text-[11px] uppercase tracking-[0.3em] text-neutral-400">
                    {m.code}
                  </span>
                  <h2 className="text-[13px] uppercase tracking-[0.25em] text-neutral-100 sm:text-[14px]">
                    {m.title}
                  </h2>
                  <span
                    className={
                      m.live
                        ? "ml-auto text-[10px] uppercase tracking-[0.25em] text-neutral-200"
                        : "ml-auto text-[10px] uppercase tracking-[0.25em] text-neutral-400"
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

      <SiteFooter className="pb-8" />
    </main>
  );
}
