import Link from "next/link";

export const metadata = {
  title: "Overview",
  description:
    "Covenant is an open, agent-native operating layer. The docs cover concepts, architecture, reference, protocols, and operations.",
};

const TILES = [
  {
    href: "/getting-started",
    title: "Getting started",
    body: "Install the daemon, run your first agent, and submit an intent.",
  },
  {
    href: "/concepts",
    title: "Concepts",
    body: "Intents, agents, capabilities, memory, audit, settlement.",
  },
  {
    href: "/architecture",
    title: "Architecture",
    body: "How the daemon, the runtime, and the on-chain settlement program fit together.",
  },
  {
    href: "/cli",
    title: "Command-line interface",
    body: "Every covenant subcommand, with arguments and exit codes.",
  },
  {
    href: "/http-api",
    title: "HTTP API",
    body: "Routes, request bodies, and responses on the local HTTP gateway.",
  },
  {
    href: "/agent-manifest",
    title: "Agent manifest",
    body: "agent.toml schema, runtime contract, and validation rules.",
  },
  {
    href: "/capabilities",
    title: "Capability tokens",
    body: "ed25519-signed permissions: shape, canonical encoding, verification, revocation.",
  },
  {
    href: "/mcp",
    title: "MCP integration",
    body: "Tool trait, native tools, and external MCP servers over JSON-RPC.",
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
        Covenant is an open, agent-native operating layer. It runs on your own
        machine, talks to local and remote AI agents, and provides the
        OS-level primitives — intent, runtime, memory, identity, permissions,
        comms, compositor, settlement — that humans and agents need to safely
        share a computer, delegate work, and pay for usage.
      </p>

      <p>
        These docs cover concepts, architecture, reference, protocols, and
        operations. New here? Start with{" "}
        <Link href="/getting-started">Getting started</Link>, then read{" "}
        <Link href="/concepts">Concepts</Link> for the mental model.
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

      <h2>Status</h2>
      <p>
        Covenant is pre-alpha. The protocol surfaces and the local daemon are
        under active development; the on-chain settlement program is evolving
        in lock-step. We do not recommend production use yet. Feedback,
        sandbox experimentation, and contributions are welcome.
      </p>

      <h2>Where this fits in the broader stack</h2>
      <p>
        Covenant sits above the operating system and below user-facing
        agentic applications. It is not an LLM, not an agent framework, and
        not a chat product — it is the coordination layer those things plug
        into so that permissions, memory, identity, and settlement are not
        each application&apos;s problem to solve from scratch.
      </p>
    </>
  );
}
