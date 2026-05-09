import Link from "next/link";

export const metadata = {
  title: "The eight primitives",
  description:
    "Intent, runtime, memory, identity, permissions, comms, audit, settlement — the OS-level vocabulary Covenant exposes.",
};

export default function PrimitivesPage() {
  return (
    <>
      <h1>The eight primitives</h1>
      <p>
        Covenant exposes a fixed set of eight primitives covering the
        operations required for human users, software agents, and
        external tools to share a host system: dispatch of work, isolated
        execution, persistent context, identity, authorization,
        communication, auditability, and economic settlement. Every
        higher-level behavior in the system is composed from these eight.
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
        <Link href="/architecture">router</Link> via keyword
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
        See <Link href="/memory">Memory tiers</Link>.
      </p>

      <h2>Identity</h2>
      <p>
        A single ed25519 keypair per Covenant install. The same key signs
        capability grants, signs Solana settlement transactions, and
        appears as the daemon&apos;s issuer on audit events and memory
        records. Persisted as a raw 32-byte seed at{" "}
        <code>$COVENANT_HOME/identity/local.key</code> with mode{" "}
        <code>0600</code>.
      </p>
      <p>
        There is no secondary key system. See{" "}
        <Link href="/identity">Identity and keys</Link>.
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
        See <Link href="/capabilities">Capability tokens</Link>.
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
          <Link href="/ipc">Local IPC</Link>.
        </li>
        <li>
          <strong>HTTP gateway</strong> — JSON over HTTP on{" "}
          <code>127.0.0.1:8421</code>. Same surface as the IPC, for
          browser-facing UIs. Documented in{" "}
          <Link href="/http-api">HTTP API</Link>.
        </li>
        <li>
          <strong>MCP and A2A adapters</strong> — protocol-grade
          surfaces for tool integration and agent-to-agent traffic.
          Documented in <Link href="/mcp">MCP integration</Link>{" "}
          and <Link href="/a2a">Agent-to-agent</Link>.
        </li>
      </ul>

      <h2>Audit</h2>
      <p>
        The evidence layer for privileged actions. Audit events are
        append-only JSONL rows with structured kinds, issuers, timestamps,
        and local hash-chain integrity reports. The alpha surface supports
        recent reads, bounded purges, integrity verification, and unsigned
        or locally signed audit-root attestations.
      </p>
      <p>
        See <Link href="/audit">Audit log</Link> and{" "}
        <Link href="/audit-integrity">Audit integrity</Link>.
      </p>

      <h2>Settlement</h2>
      <p>
        How resource consumption is accounted for. Today Covenant records
        local <code>SettlementReceipt</code> rows for resources such as
        memory writes. Chain fields remain empty unless a future settlement
        integration records them. The Solana program is scaffolded and not
        part of the alpha runtime claim.
      </p>
      <p>
        See <Link href="/settlement">Settlement</Link>.
      </p>

      <h2>Design rationale</h2>
      <p>
        Each primitive corresponds to a concern that production agent
        deployments must resolve independently. The set is intentionally
        complete: two agents implemented against all eight primitives can
        co-exist on the same host without coordination, and the operator
        can audit and account for both through the same surfaces.
      </p>
    </>
  );
}
