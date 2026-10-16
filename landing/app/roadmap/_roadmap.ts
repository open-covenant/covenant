// The roadmap's content: every track, every item and its state. Items carry a
// state, never a date. An item without a state of its own takes its track's.

export type State = "live" | "progress" | "next" | "planned";

export const STATES: { id: State; label: string; definition: string }[] = [
  { id: "live", label: "Live", definition: "Available today." },
  { id: "progress", label: "In progress", definition: "Being built or set up now." },
  { id: "next", label: "Next", definition: "The next things we plan to ship." },
  { id: "planned", label: "Planned", definition: "Further out, after the items marked Next." },
];

export const STATE_LABEL: Record<State, string> = Object.fromEntries(STATES.map((s) => [s.id, s.label])) as Record<
  State,
  string
>;

export type Item = { name: string; body?: string; state?: State; href?: string };

export type Track = {
  id: string;
  code?: string;
  kicker: string;
  title: string;
  state: State;
  intro: string;
  items: Item[];
};

export const TRACKS: Track[] = [
  {
    id: "foundation",
    code: "M0",
    kicker: "Operating layer",
    title: "Foundation",
    state: "live",
    intro:
      "Covenant's operating layer is software on your own machine that lets engineers and researchers build and run autonomous agents within permissions and budgets they set.",
    items: [
      {
        name: "Local daemon",
        body: "Covenant runs in the background on your machine, and you control it from the command line, an interactive terminal app, a web console, or a local HTTP gateway.",
      },
      {
        name: "Built-in features",
        body: "Agent identity, permissions, durable memory, an append-only activity log, agent-to-agent messaging, a Model Context Protocol (MCP) bridge, a budget ledger, and local receipts for the resources agents use.",
      },
      {
        name: "Budget limits",
        body: "A process runtime that stops a run as soon as it is projected to overspend its budget or reaches a hard time limit, with optional Linux sandboxing.",
      },
      {
        name: "Signed permissions",
        body: "A signed lifecycle for every permission: grant, scope check, expiry, and revocation.",
      },
      {
        name: "Provenance",
        body: "Verifiable workflow records, and provenance tied to a specific commit for every privileged action.",
      },
      {
        name: "Public sandbox",
        body: "Try Covenant at sandbox.opencovenant.org without installing anything.",
        href: "https://sandbox.opencovenant.org",
      },
      {
        name: "Live progress",
        body: "Watch an agent work in real time from the command line, the web console, or the HTTP gateway.",
      },
      {
        name: "Isolated code execution",
        body: "Agents write, run, and iterate on real code in a contained environment.",
      },
      {
        name: "Model providers",
        body: "Plug in Anthropic, OpenAI, DeepSeek, or local Ollama once, with automatic local fallback.",
      },
      {
        name: "Save and resume",
        body: "An agent that reaches its resource budget saves its task midway and can resume from there.",
      },
      {
        name: "Onchain identity",
        body: "Each agent has an identity on Solana mainnet, built on Metaplex MPL Core, with the root hash of its activity log attested there; anyone can confirm it with the Covenant Verified check.",
        href: "/agents",
      },
      {
        name: "Settlement program",
        body: "Covenant's settlement program runs on Solana mainnet and handles staking, slashing, anchoring receipts onchain, and buying credits with CVNT to pay for the resources agents use.",
      },
      {
        name: "Identity on Base",
        body: "Covenant's own agent is registered on Base under ERC-8004, with a deployed bond-receipt verifier, a reputation schema on the Ethereum Attestation Service (EAS), and ENS name resolution, all pointing back to its canonical Solana identity and verifiable with a single signature check. CVNT never leaves Solana.",
      },
      {
        name: "TypeScript SDK",
        body: "On npm as @covenant-org/sdk: signed settlement, staking, and receipt-anchor instructions, built from the onchain program IDLs.",
        href: "https://www.npmjs.com/package/@covenant-org/sdk",
      },
      { name: "Open source", body: "Covenant's source code is licensed under Apache 2.0." },
      {
        name: "Settlement from the daemon",
        body: "The daemon will pay for the resources agents use through the settlement program on Solana.",
        state: "planned",
      },
      {
        name: "Reputation and bonds",
        body: "Recording reputation onchain and funding bonds on Base and Robinhood Chain, where the verifiers are already deployed.",
        state: "planned",
      },
      {
        name: "Payments on Robinhood Chain",
        body: "Agents will pay in USDG over x402, an open standard for payments over HTTP, on Robinhood Chain mainnet (chain 4663), where a first payment has already settled.",
        state: "planned",
        href: "/robinhood",
      },
      {
        name: "Spend escrow on Robinhood Chain",
        body: "Agents will spend from an onchain grant with a per-call ceiling and a provider allowlist, and providers will be paid only for output that passes its checks. Blocking an overspend, paying out, and refunding have already been proven onchain.",
        state: "planned",
      },
    ],
  },
  {
    id: "compute",
    kicker: "Compute network",
    title: "Covenant Compute",
    state: "live",
    intro:
      "A network where people and their agents rent GPUs by the second and pay for finished work in USDC. It runs on Solana mainnet, with metering on MagicBlock.",
    items: [
      {
        name: "GPU rental by the second",
        body: "Each session's meter runs on a MagicBlock ephemeral rollup, where updating it costs nothing, and the session settles on Solana once, when it ends.",
        href: "/compute",
      },
      {
        name: "Coding agents for hire",
        body: "One operator's agent makes the change in your repository, a different operator runs your acceptance tests on it, and you pay only for work that passes.",
      },
      {
        name: "Hidden tests",
        body: "Tests you keep hidden from the agent can be added to the check, so the work has to hold up beyond the tests the agent could read.",
      },
      {
        name: "Two-step bug fixes",
        body: "One agent writes tests that reproduce the bug, a second agent from a different operator fixes it, and nothing is charged unless the fix passes.",
      },
      {
        name: "Verdicts onchain",
        body: "Every operator who checks the work signs a verdict, and each verdict is counted on the MagicBlock rollup and recorded on Solana before anyone is paid.",
      },
      {
        name: "Work from IMD",
        body: "Research reports and other finished work from IMD, a swarm of about 1,000 AI agents: you pay in USDC, and you get a full refund if nothing arrives.",
      },
      {
        name: "Receipts",
        body: "Every payout can be checked on Solana, and finished work arrives with a receipt signed by the operator who did it.",
      },
      {
        name: "Staked operators",
        body: "Operators stake 1,000,000 CVNT for each node before it can take work, and lose part of that stake if the network proves a fault.",
      },
      {
        name: "Agent access",
        body: "Agents can order this work through MCP tools or the command line, with a spending limit on every call.",
      },
      {
        name: "Inference and embeddings",
        body: "From network operators, through OpenAI- and Anthropic-compatible endpoints.",
        state: "next",
      },
      {
        name: "Research from X and the web",
        body: "Research reports drawn from live posts on X and the web, at a fixed price per report.",
        state: "next",
      },
      {
        name: "Pay per job",
        body: "Pay for a single job through x402, an open standard for payments over HTTP, without an account or a deposit.",
        state: "next",
      },
    ],
  },
  {
    id: "company",
    kicker: "Organization",
    title: "Company",
    state: "progress",
    intro: "Incorporating Covenant and verifying its official account on X.",
    items: [
      { name: "Incorporation", body: "Incorporating Covenant as a company." },
      {
        name: "Verified on X",
        body: "Verified Organization status on X, so the gold checkmark lets you tell the official @OpenCovenant account from impersonators.",
      },
    ],
  },
  {
    id: "m1",
    code: "M1",
    kicker: "Operating layer",
    title: "Production Readiness, Tools, and Marketplace Groundwork",
    state: "next",
    intro:
      "The next release brings the core tools agents need and the groundwork for a marketplace of agent tools, starting with the plugin catalog.",
    items: [
      {
        name: "Plugin catalog",
        body: "Install vetted tools in one click from inside the web console, starting with filesystem access.",
      },
      { name: "Hardened runtime", body: "Production-grade isolation for untrusted agent code on Linux." },
      { name: "Signed installers", body: "Homebrew, Debian, RPM, and notarized macOS packages." },
      { name: "Stable wire formats", body: "For SDK and integration compatibility." },
      { name: "Browser tool", body: "Agents that navigate, click, and read pages." },
      { name: "Git host integration", body: "Read repositories, comment on issues, and propose pull requests." },
      {
        name: "Replay debugger",
        body: "Pick any moment from the activity log, see the full state, and re-run from there.",
      },
      { name: "Python and Rust SDKs", body: "Published to PyPI and crates.io." },
      { name: "VS Code", body: "Editor integration for Visual Studio Code." },
      { name: "Framework adapters", body: "For LangGraph and CrewAI." },
    ],
  },
  {
    id: "m2",
    code: "M2",
    kicker: "Operating layer",
    title: "Native Integration",
    state: "planned",
    intro: "Deeper integration with the host operating system, on the desktop and on mobile.",
    items: [
      {
        name: "Desktop compositor",
        body: "The first version of Covenant's compositor, which shows agents' state, decisions, and results to the person running them, with native Wayland integration on Linux.",
      },
      { name: "Orchestrator agent", body: "An agent that splits a task across other agents and brings their results together." },
      { name: "Memory compaction", body: "Across the working, episodic, and long-term memory tiers." },
      { name: "First-run onboarding", body: "Guided setup the first time you run Covenant." },
      { name: "Mobile companion", body: "A read-only companion app." },
    ],
  },
  {
    id: "m3",
    code: "M3",
    kicker: "Operating layer",
    title: "Distributed Operation",
    state: "planned",
    intro: "Multi-host operation, federated identity, and cross-organization workflows.",
    items: [
      { name: "Multi-host teams", body: "Agent teams that work across machines, with agents addressed as name@host.tld." },
      { name: "Agent registry", body: "A public registry with one-line install and signed manifests." },
      { name: "MicroVM isolation", body: "Lightweight virtual machines that keep agent code isolated from the host." },
      { name: "Memory sync", body: "One identity's memory, synchronized across devices." },
      { name: "Agent migration", body: "Move an agent from one host to another." },
      {
        name: "Cross-organization trust",
        body: "Ways for organizations to establish trust with each other when they do business in the marketplace.",
      },
      {
        name: "Cross-chain enforcement",
        body: "Slash a Solana bond from an event proven on an EVM chain, with an objective fault definition and a challenge window.",
      },
    ],
  },
  {
    id: "m4",
    code: "M4",
    kicker: "Operating layer",
    title: "1.0 Release",
    state: "planned",
    intro: "A formal commitment to long-term API stability.",
    items: [
      {
        name: "Stable v1 formats",
        body: "The v1 wire formats for IPC, permissions, and agent manifests, covered by the formal commitment to long-term API stability.",
      },
      { name: "Long-term support", body: "A long-term support release line." },
      { name: "Bug bounty", body: "Rewards for security researchers who find and report vulnerabilities in Covenant." },
      { name: "Complete documentation", body: "Across all primitives and public APIs." },
      { name: "Conformance suite", body: "For third-party runtimes compatible with Covenant." },
    ],
  },
];

const ORDER: State[] = ["live", "progress", "next", "planned"];

export function stateOf(track: Track, item: Item): State {
  return item.state ?? track.state;
}

export function itemId(track: Track, item: Item): string {
  return `${track.id}-${item.name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "")}`;
}

/** A track's items in state order (live first), each with its number on the track. */
export function orderedItems(track: Track) {
  return ORDER.flatMap((s) =>
    track.items.flatMap((item, i) => (stateOf(track, item) === s ? [{ item, n: i + 1, state: s }] : [])),
  );
}

/** A track's items grouped by state, in state order; empty groups left out. */
export function groupsOf(track: Track) {
  const items = orderedItems(track);
  return ORDER.map((state) => ({ state, items: items.filter((x) => x.state === state) })).filter(
    (g) => g.items.length > 0,
  );
}

export function countByState(tracks: Track[]): Record<State, number> {
  const counts: Record<State, number> = { live: 0, progress: 0, next: 0, planned: 0 };
  for (const t of tracks) for (const item of t.items) counts[stateOf(t, item)] += 1;
  return counts;
}

/** "16 live · 3 planned" */
export function tally(track: Track): string {
  return groupsOf(track)
    .map((g) => `${g.items.length} ${STATE_LABEL[g.state].toLowerCase()}`)
    .join(" · ");
}
