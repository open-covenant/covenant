import Link from "next/link";

export const metadata = {
  title: "Overview",
  description:
    "Covenant is an open, agent-native operating layer. The documentation covers concepts, architecture, reference, protocols, and operations.",
};

const TILES = [
  {
    href: "/getting-started",
    title: "Getting started",
    body: "Install the daemon, register an agent, and dispatch a first intent.",
  },
  {
    href: "/concepts",
    title: "Concepts",
    body: "Intents, agents, capabilities, memory, audit, and settlement.",
  },
  {
    href: "/architecture",
    title: "Architecture",
    body: "Architecture of the daemon, runtime, and on-chain settlement program.",
  },
  {
    href: "/cli",
    title: "Command-line interface",
    body: "Reference for every covenant subcommand, including arguments and exit codes.",
  },
  {
    href: "/http-api",
    title: "HTTP API",
    body: "Routes, request schemas, and response shapes for the local HTTP gateway.",
  },
  {
    href: "/agent-manifest",
    title: "Agent manifest",
    body: "Schema, runtime contract, and validation rules for the agent.toml manifest.",
  },
  {
    href: "/capabilities",
    title: "Capability tokens",
    body: "ed25519-signed permission tokens: structure, canonical encoding, verification, and revocation.",
  },
  {
    href: "/mcp",
    title: "MCP integration",
    body: "The Tool trait, native tools, and integration with external MCP servers over JSON-RPC.",
  },
  {
    href: "/security",
    title: "Security model",
    body: "Trust boundaries, threat model, defaults, and operator responsibilities.",
  },
];

export default function DocsIndexPage() {
  return (
    <>
      <h1>Documentation</h1>
      <p>
        Covenant is an open, agent-native operating layer. It runs locally on
        the host and exposes eight operating-layer primitives — intent,
        runtime, memory, identity, permissions, comms, compositor, and
        settlement — through which human users, software agents, and tools
        coordinate work, share state, and settle usage.
      </p>

      <p>
        The documentation is organized into concepts, architecture, reference,
        protocols, and operations. The{" "}
        <Link href="/getting-started">Getting started</Link> guide covers
        installation and an end-to-end intent dispatch;{" "}
        <Link href="/concepts">Concepts</Link> establishes the model referenced
        throughout the remainder of the documentation.
      </p>

      <h2>Browse by area</h2>

      <div className="not-prose mt-6 grid gap-3 sm:grid-cols-2">
        {TILES.map((t) => (
          <Link
            key={t.href}
            href={t.href}
            className="group block rounded-md border border-neutral-800/80 bg-[#0a0a0a] p-5 transition-colors hover:border-neutral-700 hover:bg-[#0f0f0f]"
          >
            <div className="text-[15px] font-medium text-neutral-50 group-hover:text-white">
              {t.title}
            </div>
            <div className="mt-2 text-[13px] leading-relaxed text-neutral-400">
              {t.body}
            </div>
          </Link>
        ))}
      </div>

      <h2>Release</h2>
      <p>
        Covenant 0.1 (alpha) is released on 13 May 2026. Settlement runs on
        Solana mainnet from the alpha release. An external security audit of
        the settlement program is scheduled for the M2 milestone; the
        post-alpha milestone schedule is published on the{" "}
        <a href="https://opencovenant.org/roadmap">public roadmap</a>. Protocol
        wire formats — IPC, capabilities, and agent manifest — are subject to
        revision ahead of the 1.0 release.
      </p>

      <h2>Position in the stack</h2>
      <p>
        Covenant operates between the host operating system and user-facing
        agentic applications. It provides identity, permissions, memory,
        communication, and settlement as shared, host-level services, allowing
        language models, agent frameworks, and end-user applications to
        integrate against a common substrate rather than reimplement these
        primitives independently.
      </p>
    </>
  );
}
