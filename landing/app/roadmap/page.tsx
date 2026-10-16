import type { Metadata } from "next";
import { pageMetadata } from "@/app/_metadata";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

export const metadata: Metadata = pageMetadata("/roadmap", { title: "Roadmap" }, {
  title: "Development Roadmap: Covenant",
  description:
    "Covenant's roadmap: what is live today (including Covenant Compute on Solana mainnet), what we are working on now, and the milestones on the way to the 1.0 release.",
  alternates: { canonical: "/roadmap" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/roadmap",
    title: "Covenant Roadmap: What Is Live and What Comes Next",
    description:
      "Covenant Compute is live on Solana mainnet. See what we are working on now and the milestones on the way to the 1.0 release.",
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "Covenant Roadmap: What Is Live and What Comes Next",
    description:
      "Covenant Compute is live on Solana mainnet. See what we are working on now and the milestones on the way to the 1.0 release.",
  },
});

type Milestone = {
  code?: string;
  title: string;
  status: string;
  state: "live" | "progress" | "upcoming";
  intro: string;
  bullets: string[];
  next?: string[];
};

const MILESTONES: Milestone[] = [
  {
    code: "M0",
    title: "Foundation",
    status: "Available",
    state: "live",
    intro:
      "A local control plane: software on your own machine that lets engineers and researchers build and run autonomous agents within permissions and budgets they set.",
    bullets: [
      "A local daemon you control from the command line, an interactive terminal app, a web console, or an HTTP gateway on your machine",
      "Built into the daemon: agent identity, permissions, durable memory, an append-only activity log, agent-to-agent messaging, a Model Context Protocol (MCP) bridge, a budget ledger, and local receipts for the resources agents use",
      "A process runtime that stops a run as soon as it is projected to overspend its budget or reaches a hard time limit, with optional Linux sandboxing",
      "Signed permission lifecycle with grant, scope check, expiry, and revocation",
      "Verifiable workflow records, and provenance tied to a specific commit for every privileged action",
      "Public sandbox at sandbox.opencovenant.org",
      "Live progress streaming: watch an agent work in real time from the command line, the web console, or the HTTP gateway",
      "Isolated code execution: agents that write, run, and iterate on real code in a contained environment",
      "Unified model provider: plug in Anthropic, OpenAI, DeepSeek, or local Ollama once, with automatic local fallback",
      "Mid-task save and resume when an agent reaches its resource budget",
      "Agent identity on Solana mainnet, built on Metaplex MPL Core: the root hash of each agent's activity log is attested on chain, and anyone can confirm it with the Covenant Verified check",
      "Settlement program live on Solana mainnet: $CVNT-to-credits, staking, slashing, and on-chain receipt anchoring transact on-chain; the daemon-driven per-intent settlement lifecycle is not yet production",
      "Multi-chain trust on Base mainnet: the agent is registered under ERC-8004, with a deployed bond-receipt verifier, an EAS reputation schema, and ENS name resolution, all pointing back to the canonical Solana identity and verifiable with one ecrecover; on-chain reputation writes and funded bonds are not yet exercised, and $CVNT never leaves Solana",
      "Robinhood Chain mainnet (chain 4663): a USDG x402 payment settled on mainnet in real USDG, and a bounded-spend escrow enforcing a per-call ceiling, a provider allowlist, and pay-only-for-passing-output, with its overspend-revert, payout, and refund lifecycle proven on Robinhood Chain testnet; the bond and reputation verifiers are deployed but not yet exercised, and identity stays Solana-canonical",
      "TypeScript SDK on npm (@covenant-org/sdk): signed settlement, staking, and receipt-anchor instructions built from the on-chain program IDLs",
      "Apache 2.0 core",
    ],
  },
  {
    title: "Covenant Compute",
    status: "Live",
    state: "live",
    intro:
      "A network where people and their agents rent GPUs by the second and pay for finished work in USDC. It runs on Solana mainnet, with metering on MagicBlock.",
    bullets: [
      "GPU rental by the second: each session's meter runs on a MagicBlock ephemeral rollup, where updating it costs nothing, and the session settles on Solana once, when it ends",
      "Coding agents for hire: one operator's agent makes the change in your repository, a different operator runs your acceptance tests on it, and you pay only for work that passes",
      "Tests you keep hidden from the agent can be added to the check, so the work has to hold up beyond the tests the agent could read",
      "Bug fixes in two steps: one agent writes tests that reproduce the bug, a second agent from a different operator fixes it, and nothing is charged unless the fix passes",
      "Every operator who checks the work signs a verdict, and each verdict is counted on the MagicBlock rollup and recorded on Solana before anyone is paid",
      "Research reports and other finished work from IMD, a swarm of about 1,000 AI agents: you pay in USDC, and you get a full refund if nothing arrives",
      "Every payout can be checked on Solana, and finished work arrives with a receipt signed by the operator who did it",
      "Operators stake 1,000,000 CVNT for each node before it can take work, and lose part of that stake if the network proves a fault",
      "Agents can order this work through MCP tools or the command line, with a spending limit on every call",
    ],
    next: [
      "Inference and embeddings from network operators, through OpenAI- and Anthropic-compatible endpoints",
      "Research reports drawn from live posts on X and the web, at a fixed price per report",
      "Paying for a single job through x402, an open standard for payments over HTTP, without an account or a deposit",
    ],
  },
  {
    title: "Company",
    status: "In progress",
    state: "progress",
    intro: "Incorporating Covenant and verifying its official account on X.",
    bullets: [
      "Incorporating Covenant as a company",
      "Verified Organization status on X, so the gold checkmark shows that @OpenCovenant is the official account",
    ],
  },
  {
    code: "M1",
    title: "Production, Tools, and Marketplace Foundation",
    status: "Next",
    state: "upcoming",
    intro:
      "The next release brings the core tools agents need and the groundwork for a marketplace of agent tools, starting with the plugin catalog.",
    bullets: [
      "Plugin catalog: install vetted tools in one click from inside the web console, starting with filesystem access",
      "Production-grade isolated runtime for untrusted agent code on Linux",
      "Signed installers: Homebrew, Debian, RPM, and notarized macOS packages",
      "Stable wire formats for SDK and integration compatibility",
      "Browser tool: agents that navigate, click, and read pages",
      "Git host integration: read repositories, comment on issues, propose pull requests",
      "Replay and state debugger: pick any moment from the activity log, see full state, re-run from there",
      "SDKs published to PyPI and crates.io",
      "Editor integration for Visual Studio Code",
      "Adapters for existing agent frameworks: LangGraph and CrewAI",
    ],
  },
  {
    code: "M2",
    title: "Native Integration",
    status: "Upcoming",
    state: "upcoming",
    intro:
      "Deeper integration with the host operating system, on the desktop and on mobile.",
    bullets: [
      "A first version of Covenant's compositor, the part that draws windows on screen, with native Wayland integration on Linux",
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
    state: "upcoming",
    intro:
      "Multi-host operation, federated identity, and cross-organization workflows.",
    bullets: [
      "Multi-host operation with name@host.tld resolution for agent teams across machines",
      "Public agent registry with one-line install and signed manifests",
      "Microvm-grade isolation",
      "Multi-device memory synchronization for a single identity",
      "Agent migration across hosts",
      "Trust flows for cross-organization marketplace transactions",
      "Cross-chain enforcement: slash a Solana bond from an EVM-proven event, with an objective fault definition and a challenge window",
    ],
  },
  {
    code: "M4",
    title: "1.0 Release",
    status: "Upcoming",
    state: "upcoming",
    intro: "A formal commitment to long-term API stability.",
    bullets: [
      "Stable v1 wire formats for IPC, permissions, and agent manifests",
      "Long-term support release line",
      "Bug bounty program",
      "Comprehensive documentation across all primitives and public APIs",
      "Conformance suite for third-party covenant-compatible runtimes",
    ],
  },
];

// live: filled mint · in progress: mint ring · upcoming: grey ring
const DOT: Record<Milestone["state"], string> = {
  live: "bg-mint",
  progress: "border border-mint bg-ground",
  upcoming: "border border-line-strong bg-ground",
};

function Points({ items }: { items: string[] }) {
  return (
    <ul className="space-y-2">
      {items.map((b) => (
        <li key={b} className="flex gap-3 text-[13px] leading-relaxed text-fg-soft sm:text-[14px]">
          <span aria-hidden="true" className="select-none text-faint">
            ·
          </span>
          <span>{b}</span>
        </li>
      ))}
    </ul>
  );
}

export default function RoadmapPage() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-ground">
      <SiteHeader />

      <div className="page-container">
        <h1 className="mb-16 text-[11px] uppercase tracking-[0.4em] text-muted sm:mb-24">
          Roadmap
        </h1>

        <ol className="relative">
          {MILESTONES.map((m, i) => {
            const isLast = i === MILESTONES.length - 1;
            return (
              <li
                key={m.title}
                className={`relative border-l border-line/80 pl-8 sm:pl-12 ${isLast ? "" : "pb-14 sm:pb-16"}`}
              >
                <span aria-hidden="true" className={`absolute -left-[5px] top-[7px] h-[9px] w-[9px] rounded-full ${DOT[m.state]}`} />
                <div className="mb-3 flex flex-wrap items-baseline gap-x-4 gap-y-1">
                  {m.code ? (
                    <span className="font-mono text-[11px] uppercase tracking-[0.3em] text-muted">
                      {m.code}
                    </span>
                  ) : null}
                  <h2 className="text-[13px] uppercase tracking-[0.25em] text-fg sm:text-[14px]">
                    {m.title}
                  </h2>
                  <span
                    className={`ml-auto text-[10px] uppercase tracking-[0.25em] ${m.state === "upcoming" ? "text-muted" : "text-fg-2"}`}
                  >
                    {m.status}
                  </span>
                </div>
                <p className="mb-4 text-[13px] leading-relaxed text-muted sm:text-[14px]">
                  {m.intro}
                </p>
                <Points items={m.bullets} />
                {m.next ? (
                  <>
                    <h3 className="mb-3 mt-6 font-mono text-[11px] uppercase tracking-[0.3em] text-muted">
                      Next
                    </h3>
                    <Points items={m.next} />
                  </>
                ) : null}
              </li>
            );
          })}
        </ol>
      </div>

      <SiteFooter className="pb-8" />
    </main>
  );
}
