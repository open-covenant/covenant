// Ecosystem inventory for /partners. Kept as a single source of truth so the
// partners page (and any future home-page logo strip) read the same list and
// never drift. Facts here are condensed strictly from the integration crates,
// bridge services, and docs in the repo — no invented relationships.
//
// LOGOS: drop a monochrome/light-on-dark mark at `public/partners/<slug>.svg`
// (SVG preferred; PNG with transparency also fine) and set `logo` below to
// `/partners/<slug>.svg`. Until then each card renders a clean monospace
// wordmark, so the page is complete without any assets.

export type IntegrationStatus = "live" | "building";

export type Integration = {
  slug: string;
  /** The brand mark / title shown on the card. */
  name: string;
  /** Optional attribution line, e.g. the org behind an open-source project. */
  by?: string;
  /** One factual sentence: what it is + how Covenant integrates. */
  blurb: string;
  status: IntegrationStatus;
  /** Partner's own site — only set when verified from the repo or universally known. */
  href?: string;
  /** Set once a logo file exists at this path. */
  logo?: string;
};

// Open protocols and chains Covenant speaks. Integrating with these needs no
// bespoke partnership — any compliant client interoperates.
export const PROTOCOLS: Integration[] = [
  {
    slug: "solana",
    name: "Solana",
    blurb:
      "Settlement, staking, and on-chain audit-root anchoring all run on Solana.",
    status: "live",
    href: "https://solana.com",
  },
  {
    slug: "x402",
    name: "x402",
    blurb:
      "Agents make capability-scoped, pay-per-call HTTP requests over the x402 payment protocol.",
    status: "live",
  },
  {
    slug: "mcp",
    name: "Model Context Protocol",
    blurb:
      "Covenant exposes its tools and agents to any MCP client, and bridges external MCP servers in.",
    status: "live",
    href: "https://modelcontextprotocol.io",
  },
  {
    slug: "a2a",
    name: "Agent-to-Agent",
    blurb:
      "Signed task and result envelopes coordinate work across agents over the A2A protocol.",
    status: "live",
  },
];

// Named products that integrate with Covenant. `live` = shipped and running;
// `building` = integration in active development, not yet on the main line.
export const PARTNERS: Integration[] = [
  {
    slug: "synapse",
    name: "Synapse Agent Protocol",
    blurb:
      "Covenant publishes signed audit-root attestations to Synapse on Solana for cross-party agent identity.",
    status: "live",
  },
  {
    slug: "hyre",
    name: "Hyre",
    blurb:
      "Hyre's DeFi and market-data endpoints reach agents as capability-scoped, x402-billed tools.",
    status: "live",
    href: "https://hyreagent.fun",
  },
  {
    slug: "fairscale",
    name: "FairScale",
    blurb:
      "Covenant streams audit-attested conduct events to FairScale's agent-reputation scoring.",
    status: "live",
    href: "https://fairscale.xyz",
  },
  {
    slug: "hermes",
    name: "Hermes",
    by: "Nous Research",
    blurb:
      "Covenant runs Hermes agents behind a sandboxed, capability-gated coding gateway.",
    status: "live",
    href: "https://github.com/NousResearch/hermes-agent",
  },
  {
    slug: "acedata",
    name: "Ace Data Cloud",
    blurb:
      "One capability-gated gateway to Ace Data Cloud's image, video, music, and search models, with provenance on every call.",
    status: "building",
    href: "https://acedata.cloud",
  },
  {
    slug: "metaplex",
    name: "Metaplex",
    blurb:
      "Agent audit-roots and provenance written into Metaplex Core asset data on Solana.",
    status: "building",
    href: "https://www.metaplex.com",
  },
  {
    slug: "magicblock",
    name: "MagicBlock",
    blurb:
      "Agent resource metering on a MagicBlock ephemeral rollup, committed periodically back to Solana.",
    status: "building",
  },
  {
    slug: "gitlawb",
    name: "Gitlawb",
    blurb:
      "Agents push cryptographically attested commits to Gitlawb's decentralized git nodes.",
    status: "building",
    href: "https://github.com/Gitlawb",
  },
  {
    slug: "xona",
    name: "Xona Agent",
    blurb:
      "Xona Agent's creative generation endpoints reach agents as capability-scoped, x402-billed tools.",
    status: "building",
  },
  {
    slug: "orbserv",
    name: "Orbserv",
    blurb:
      "Covenant acts as the spend-authorization policy layer for Orbserv's agent wallets.",
    status: "building",
  },
];
