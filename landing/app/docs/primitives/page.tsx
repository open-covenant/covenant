import Link from "next/link";

export const metadata = {
  title: "The eight primitives",
  description:
    "Intent, runtime, memory, identity, permissions, comms, compositor, settlement — the OS-level vocabulary Covenant exposes.",
};

export default function PrimitivesPage() {
  return (
    <>
      <h1>The eight primitives</h1>
      <p>
        Covenant exposes a fixed vocabulary of eight primitives. They
        cover everything an agent stack needs to share a computer with a
        human, with another agent, and with itself across time. Every
        higher-level concept in the system is composed from these eight.
      </p>

      <h2>Intent</h2>
      <p>
        A natural-language request, addressed to the daemon. Intents are
        the entry point into the rest of the system. Every interaction
        between a human and an agent — and most interactions between
        agents — is mediated by an intent.
      </p>
      <p>
        Stable UUID, issuer (an <code>AgentId</code>), issuing
        timestamp, priority, optional parent. Routed by the{" "}
        <Link href="/docs/architecture">router</Link> via keyword
        overlap; falls back to a deterministic echo response when no
        agent matches.
      </p>

      <h2>Runtime</h2>
      <p>
        The mechanism for actually executing an agent. Today the runtime
        spawns each agent as a child process and shuttles JSON over
        stdin/stdout under a wall-clock budget. The trait is small and
        designed to back stricter isolation — gVisor, Firecracker —
        without changing the dispatch contract.
      </p>
      <p>
        Agents declare their runtime in{" "}
        <code>agent.toml</code>: <code>rust-bin</code>,{" "}
        <code>python3</code>, or <code>node</code>. Per-task budgets
        cover CPU time, memory, disk, and a network policy
        (<code>off</code>, <code>outbound-https-only</code>,{" "}
        <code>full</code>).
      </p>

      <h2>Memory</h2>
      <p>
        A three-tier semantic store with cosine-similarity search over
        embedded vectors. Tiers are <strong>working</strong> (per-task
        scratch), <strong>episodic</strong> (task-grained, durable),
        and <strong>long-term</strong> (intentionally retained context).
      </p>
      <p>
        SQLite-backed for production, in-memory for tests. Each record
        carries a UUID, a tier, an owner, the result text, an embedding
        vector, free-form JSON metadata, a creation timestamp, and an
        optional parent. Search is cosine over stored vectors, scoped
        by tier or unioned across all tiers.
      </p>
      <p>
        See <Link href="/docs/memory">Memory tiers</Link>.
      </p>

      <h2>Identity</h2>
      <p>
        A single ed25519 keypair per Covenant install. The same key
        signs capability grants, signs Solana settlement transactions,
        and fronts the daemon&apos;s issuer field on audit events and
        memory records. Persisted as a raw 32-byte seed at{" "}
        <code>$COVENANT_HOME/identity/local.key</code>, mode{" "}
        <code>0600</code>.
      </p>
      <p>
        There is no second key system. See{" "}
        <Link href="/docs/identity">Identity and keys</Link>.
      </p>

      <h2>Permissions</h2>
      <p>
        Capability tokens. Each token names an action (a dotted string
        from a reserved namespace such as{" "}
        <code>tool.web_search</code>, <code>memory.write</code>,{" "}
        <code>tool.call.&lt;name&gt;</code>) and optionally a scope
        constraint expressed as JSON. Tokens are signed by the granter
        with ed25519 over a deterministic byte encoding of the fields,
        so they are independently verifiable.
      </p>
      <p>
        Granted tokens are appended to a JSONL log; revocations are
        appended to a separate JSONL log as tombstones; the active set
        is the granted set with revocations subtracted. Capability
        checks are enforced at dispatch and audited regardless of
        outcome.
      </p>
      <p>
        See <Link href="/docs/capabilities">Capability tokens</Link>.
      </p>

      <h2>Comms</h2>
      <p>
        How agents and clients talk to the daemon, to each other, and to
        external tooling. Three transports today:
      </p>
      <ul>
        <li>
          <strong>Local IPC</strong> — length-prefixed JSON over a Unix
          socket. The CLI uses this. Documented in{" "}
          <Link href="/docs/ipc">Local IPC</Link>.
        </li>
        <li>
          <strong>HTTP gateway</strong> — JSON over HTTP on{" "}
          <code>127.0.0.1:8421</code>. Same surface as the IPC, for
          browser-facing UIs. Documented in{" "}
          <Link href="/docs/http-api">HTTP API</Link>.
        </li>
        <li>
          <strong>MCP and A2A adapters</strong> — protocol-grade
          surfaces for tool integration and agent-to-agent traffic.
          Documented in <Link href="/docs/mcp">MCP integration</Link>{" "}
          and <Link href="/docs/a2a">Agent-to-agent</Link>.
        </li>
      </ul>

      <h2>Compositor</h2>
      <p>
        Whatever the operator uses to drive Covenant. The CLI today, a
        web UI alongside, a TUI later, and an optional Wayland
        compositor in the longer term. Compositors are clients of the
        daemon — they speak Local IPC or HTTP and own no state of their
        own beyond the user&apos;s preferences.
      </p>

      <h2>Settlement</h2>
      <p>
        How resource consumption is accounted for. Every memory write,
        every tool call, and (eventually) every external compute or
        token spent produces a <code>SettlementReceipt</code>. Receipts
        accumulate locally in JSONL and are batched and flushed to the
        on-chain program; once flushed, the on-chain signature populates
        and the receipt is reconcilable from chain state alone.
      </p>
      <p>
        See <Link href="/docs/settlement">Settlement</Link>.
      </p>

      <h2>Why these eight</h2>
      <p>
        Every primitive was selected because it is something every
        non-trivial agent stack needs and currently re-implements badly.
        The set is intentionally complete: an agent that uses all eight
        primitives can run alongside a different agent using all eight,
        on the same machine, without either knowing about the other,
        and the operator can audit and pay for both.
      </p>
    </>
  );
}
