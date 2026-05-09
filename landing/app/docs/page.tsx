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
  {
    href: "/provenance",
    title: "Provenance",
    body: "Commit-scoped envelopes for autonomy tasks, changed file evidence, and validation records.",
  },
  {
    href: "/live-coverage",
    title: "Live coverage",
    body: "Opt-in real-boundary test inventory across daemon, CLI, A2A, MCP, runtime, and model surfaces.",
  },
  {
    href: "/gvisor-live-runner",
    title: "Linux gVisor runner",
    body: "Repeatable Linux host setup for the opt-in runsc sandbox validation path.",
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

      <h2>Status</h2>
      <p>
        Covenant is pre-1.0 infrastructure. The local daemon, CLI, identity,
        permissions, memory, audit, peer auth, and local receipt ledger are
        implemented in the repository. MCP, A2A, the local web console, the
        autonomous development loop, live coverage matrix, and public
        provenance envelopes are experimental. Runtime sandboxing currently
        has manifest-level requirements, trusted-local fail-closed behavior,
        daemon-selectable Linux gVisor configuration, an initial runtime-level
        gVisor runner, opt-in live Linux sandbox coverage, and a documented
        Linux runner setup. On-chain settlement, installer, SDK ecosystem,
        signed releases, production sandbox CI, and transparency-log
        publication remain planned work.
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
