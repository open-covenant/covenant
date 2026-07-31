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
  // Live — shipped on the main line.
  {
    slug: "synapse",
    name: "Synapse Agent Protocol",
    blurb:
      "Covenant publishes signed audit-root statements to Synapse on Solana; the signature attributes the bytes, not a real-world identity.",
    status: "live",
  },
  {
    slug: "metaplex",
    name: "Metaplex",
    blurb:
      "Agent audit-roots and provenance written into Metaplex Core asset data on Solana.",
    status: "live",
    href: "https://www.metaplex.com",
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
      "Covenant can send signed local event statements to FairScale as inputs to FairScale's scoring.",
    status: "live",
    href: "https://fairscale.xyz",
  },
  {
    slug: "hermes",
    name: "Hermes",
    by: "Nous Research",
    blurb:
      "Covenant runs Hermes agents through a capability-gated coding gateway; host-process controls are not OS isolation.",
    status: "live",
    href: "https://github.com/NousResearch/hermes-agent",
  },
  {
    slug: "hatcherlabs",
    name: "HatcherLabs",
    blurb:
      "Covenant joins HatcherLabs' agent mesh as a local-first connector with capability-scoped host controls.",
    status: "live",
  },
  {
    slug: "zauth",
    name: "Zauth",
    blurb:
      "Covenant queries Zauth's agent-attestation directory and runs one-shot RepoScan verification, billed over x402.",
    status: "live",
  },
  {
    slug: "sns",
    name: "Solana Name Service",
    blurb:
      "Covenant agents resolve and verify .sol names through read-only Solana Name Service tools.",
    status: "live",
  },
  {
    slug: "acedata",
    name: "Ace Data Cloud",
    blurb:
      "A capability-gated gateway to Ace Data Cloud models emits local audit metadata on the audited call path.",
    status: "live",
    href: "https://acedata.cloud",
  },
  {
    slug: "xona",
    name: "Xona Agent",
    blurb:
      "Xona Agent's creative generation endpoints reach agents as capability-scoped, x402-billed tools.",
    status: "live",
  },
  {
    slug: "wurk",
    name: "Wurk",
    blurb:
      "Wurk's microjob endpoints run as capability-scoped, x402-billed tools with publisher-attributed call records, not proof of completed work.",
    status: "live",
  },
  {
    slug: "orbserv",
    name: "Orbserv",
    blurb:
      "Covenant exposes an advisory pre-spend decision endpoint for Orbserv; the external wallet remains the signing boundary.",
    status: "live",
  },
  {
    slug: "magicblock",
    name: "MagicBlock",
    blurb:
      "Explicit receipt hashes can be metered on a MagicBlock ephemeral rollup and observed after state commits to Solana.",
    status: "live",
  },

  // In development — active build on a feature line, not yet on the main line.
  {
    slug: "clawville",
    name: "ClawVille",
    blurb:
      "The ClawVille prototype combines scoped grants, hash-chained event records, and publisher-signed verdict statements.",
    status: "building",
    href: "https://clawville.world",
  },
  {
    slug: "zkmedusa",
    name: "zkMedusa",
    blurb:
      "The zkMedusa prototype exchanges signed profile statements derived from local heuristics; they are not reputation truth.",
    status: "building",
  },
  {
    slug: "syra",
    name: "Syra",
    blurb:
      "Syra's market-intelligence endpoints (signal, news, sentiment, smart-money) reach agents as capability-scoped, x402-billed tools.",
    status: "building",
  },
  {
    slug: "earnfi",
    name: "EarnFi",
    blurb:
      "The EarnFi prototype exposes human-task endpoints as capability-scoped, x402-billed tools without proving delivery quality.",
    status: "building",
  },
  {
    slug: "percolator",
    name: "Percolator",
    blurb:
      "The Percolator prototype explores policy-scoped keeper actions and a future stake-backed accountability model.",
    status: "building",
  },
  {
    slug: "gitlawb",
    name: "Gitlawb",
    blurb:
      "The Gitlawb prototype associates publisher-signed statements with commits sent to decentralized git nodes.",
    status: "building",
    href: "https://github.com/Gitlawb",
  },
  {
    slug: "bento",
    name: "Bento",
    blurb:
      "The Bento prototype treats an external verdict and onchain standing as labeled policy inputs; signer enforcement is not implemented.",
    status: "building",
    href: "https://bentoguard.xyz",
  },
  {
    slug: "robinhood",
    name: "Robinhood",
    blurb:
      "The Robinhood prototype is a dry-run policy demonstration; no brokerage account, live order path, or venue-enforced gate is attached.",
    status: "building",
    href: "https://robinhood.com/us/en/agentic-trading/",
  },
];

// Partner avatars (X profile images) committed at public/partners/<slug>.jpg.
// Listed here so a mark only renders once its file actually exists; add a slug
// when you drop a file in (or set `logo` on the entry directly to override).
const LOGO_SLUGS = new Set<string>([
  "solana",
  "synapse",
  "metaplex",
  "hyre",
  "fairscale",
  "hermes",
  "sns",
  "magicblock",
  "gitlawb",
  "xona",
  "wurk",
  "acedata",
  "hatcherlabs",
  "zauth",
  "clawville",
  "orbserv",
  "zkmedusa",
  "syra",
  "earnfi",
  "percolator",
  "bento",
]);

export function logoFor(slug: string): string | undefined {
  return LOGO_SLUGS.has(slug) ? `/partners/${slug}.jpg` : undefined;
}
