import Link from "next/link";
import { buildDocsMetadata } from "../_meta";

export const metadata = buildDocsMetadata("concepts", "Concepts", 'The Covenant data model: intents, agents, capabilities, memory, audit, and settlement.');

export default function ConceptsPage() {
  return (
    <>
      <h1>Concepts</h1>
      <p>
        Covenant is organized around a fixed vocabulary. Intents, agents,
        capabilities, memory records, audit events, and settlement receipts
        are the primary types in the data model; every higher-level behavior
        in the system is defined in terms of these.
      </p>

      <h2>Intent</h2>
      <p>
        An <strong>intent</strong> is a natural-language request issued by a
        user (or another agent) and routed to whichever agent can fulfil it.
        Every intent has a stable UUID, an issuer (an{" "}
        <code>AgentId</code>), an issuing timestamp, a priority, and an
        optional parent. Intents do not carry credentials — the issuer&apos;s
        capabilities are determined by their identity.
      </p>

      <p>
        Intents are submitted over the local socket, the HTTP gateway, or
        the CLI. The daemon routes each intent by keyword overlap against
        the capability cards of registered agents; an intent that matches
        no agent receives a deterministic echo response.
      </p>

      <h2>Agent</h2>
      <p>
        An <strong>agent</strong> is anything Covenant can dispatch an
        intent to. Each agent is a subdirectory of{" "}
        <code>$COVENANT_HOME/agents/</code> containing an{" "}
        <code>agent.toml</code> manifest; the manifest declares:
      </p>

      <ul>
        <li>
          a stable <code>agent.id</code> in the form{" "}
          <code>name@host</code>,
        </li>
        <li>
          a <code>runtime</code> (currently <code>rust-bin</code>,{" "}
          <code>python3</code>, or <code>node</code>),
        </li>
        <li>
          an <code>entry</code> path to the binary or script,
        </li>
        <li>
          the <code>capabilities</code> the agent needs to operate,
        </li>
        <li>
          per-task <code>resources</code> (CPU budget, memory, disk, network
          policy),
        </li>
        <li>
          optional <code>settlement</code> hints (per-hour credit budget,
          priority).
        </li>
      </ul>

      <p>
        See{" "}
        <Link href="/agent-manifest">Agent manifest</Link> for the full
        schema and validation rules.
      </p>

      <h3>Runtime contract</h3>
      <p>
        At dispatch the runtime spawns the agent binary, sends one JSON
        line containing the <code>Intent</code> on stdin, reads one JSON
        line containing the <code>AgentResult</code> on stdout, and kills
        the process if it exceeds <code>resources.cpu_ms_per_task</code>.
        Stderr is streamed to the daemon&apos;s tracing log.
      </p>

      <h2>Capability</h2>
      <p>
        A <strong>capability</strong> is a typed permission slip. It names
        an action (a dotted string from a reserved namespace such as{" "}
        <code>tool.web_search</code>, <code>memory.write</code>,{" "}
        <code>tool.call.&lt;name&gt;</code>) and optionally a scope
        constraint expressed as JSON.
      </p>

      <p>
        A <strong>signed capability</strong> is a capability plus an
        ed25519 signature by the granter over a deterministic byte
        encoding of the fields. Signed capabilities are stored in an
        append-only JSONL log under{" "}
        <code>$COVENANT_HOME/capabilities/granted.jsonl</code>;
        revocations land alongside them in{" "}
        <code>capabilities/revoked.jsonl</code>. The active set at any
        moment is the granted set with revocations subtracted.
      </p>

      <p>
        At dispatch, the daemon enforces capabilities <em>hard</em> — a
        request whose required actions are not all present in the active
        set is rejected, and the rejection itself is audited.
      </p>

      <p>
        The full token shape, the deterministic encoding, and the
        verification path are documented in{" "}
        <Link href="/capabilities">Capability tokens</Link>.
      </p>

      <h2>Memory</h2>
      <p>
        Covenant&apos;s memory is a single store partitioned into three
        tiers:
      </p>

      <ul>
        <li>
          <strong>working</strong> — short-lived per-task scratch.
          Persists until purged via the memory garbage-collection
          primitive.
        </li>
        <li>
          <strong>episodic</strong> — task-grained records that survive
          across runs.
        </li>
        <li>
          <strong>long-term</strong> — durable, intentionally retained
          context; the slowest tier and the one most likely to influence
          future routing.
        </li>
      </ul>

      <p>
        Records are written as <code>MemoryRecord</code> values: a UUID, a
        tier, an owner <code>AgentId</code>, the result text, an embedding
        vector, free-form JSON metadata, a creation timestamp, and an
        optional parent. Persistent backing is SQLite at{" "}
        <code>$COVENANT_HOME/memory.db</code>; an in-memory backend is
        available for tests.
      </p>

      <p>
        Searches are cosine-similarity over the stored embedding vectors,
        scoped to a tier or unioned across all tiers. See{" "}
        <Link href="/memory">Memory tiers</Link> for details.
      </p>

      <h2>Identity</h2>
      <p>
        Every Covenant install owns a single ed25519 keypair persisted at{" "}
        <code>$COVENANT_HOME/identity/local.key</code> (raw 32-byte seed,
        mode <code>0600</code>). The same key:
      </p>

      <ul>
        <li>signs capability grants,</li>
        <li>signs Solana settlement transactions,</li>
        <li>
          fronts the daemon&apos;s issuer field on audit events and memory
          records.
        </li>
      </ul>

      <p>
        There is no secondary key system. Refer to{" "}
        <Link href="/identity">Identity and keys</Link> and{" "}
        <Link href="/security">Security model</Link> for key-management
        practices.
      </p>

      <h2>Audit</h2>
      <p>
        Every state-changing operation emits an <code>AuditEvent</code>:
        intent dispatch, capability check, capability grant, capability
        revocation, ignored intent. Events are appended to{" "}
        <code>$COVENANT_HOME/audit/events.jsonl</code> under a deterministic
        schema. The audit log is the system of record;{" "}
        <code>covenant verify</code> cross-checks it against the other
        state files for drift.
      </p>

      <p>
        See <Link href="/audit">Audit log</Link> for the event
        schema.
      </p>

      <h2>Comms</h2>
      <p>
        The daemon exposes three transports today:
      </p>

      <ul>
        <li>
          <strong>Unix socket</strong> at <code>$COVENANT_HOME/sock</code> —
          the canonical, length-prefixed JSON IPC. The CLI uses this
          transport.
        </li>
        <li>
          <strong>HTTP gateway</strong> at <code>127.0.0.1:8421</code> —
          for browser-facing UIs and third-party tooling. Same surface,
          JSON over HTTP. See{" "}
          <Link href="/http-api">HTTP API</Link>.
        </li>
        <li>
          <strong>MCP and A2A adapters</strong> for protocol-grade tool
          and agent-to-agent communication. See{" "}
          <Link href="/mcp">MCP integration</Link> and{" "}
          <Link href="/a2a">Agent-to-agent</Link>.
        </li>
      </ul>

      <h2>Settlement</h2>
      <p>
        Covenant accounts for resource usage as it happens. Every
        memory write, every tool call, and (eventually) every external
        compute or LLM token spent produces a{" "}
        <strong>settlement receipt</strong> — a UUID, a payer, a resource
        kind, a credits-consumed integer, a timestamp, a memory record id
        when the resource is memory, and an optional on-chain signature.
        Receipts accumulate in{" "}
        <code>$COVENANT_HOME/receipts/working.jsonl</code>. Chain fields
        remain empty unless a future settlement integration records them.
      </p>

      <p>
        See <Link href="/settlement">Settlement</Link> for the current
        local receipt model and the planned on-chain boundary.
      </p>

      <h2>End-to-end intent flow</h2>
      <p>
        A successful intent dispatch exercises every primitive. The daemon
        receives the intent, validates the issuer&apos;s capabilities,
        selects an agent through the router, executes the agent under a
        wall-clock budget, captures the result, persists a memory record,
        emits a settlement receipt, and writes the corresponding audit
        events. Operational state is fully reconstructible from the audit
        log.
      </p>
    </>
  );
}
