import Link from "next/link";

export const metadata = {
  title: "System architecture",
  description:
    "How the daemon, runtime, storage primitives, adapters, and settlement scaffold fit together.",
};

export default function ArchitecturePage() {
  return (
    <>
      <h1>System architecture</h1>
      <p>
        Covenant is a single long-running daemon — <code>covenantd</code> —
        plus a thin set of clients (the CLI, the web UI, third-party tooling
        over HTTP) and a number of agent processes. The daemon is the only
        component that holds state; everything else is replaceable.
      </p>

      <h2>Component map</h2>

      <pre>
        <code>{`┌──────────────────────────────────────────────────────────────┐
│                          covenantd                           │
│ ┌──────────┐  ┌──────────┐  ┌────────────┐  ┌────────────┐  │
│ │  IPC     │  │   HTTP   │  │   MCP      │  │   A2A      │  │
│ │  socket  │  │  gateway │  │ adapter    │  │ adapter    │  │
│ └────┬─────┘  └────┬─────┘  └─────┬──────┘  └─────┬──────┘  │
│      │             │              │               │          │
│      ▼             ▼              ▼               ▼          │
│ ┌──────────────────────────────────────────────────────────┐ │
│ │                    Server::respond                       │ │
│ │ (intent dispatch, capability checks, audit, ignore set)  │ │
│ └────────┬─────────┬─────────┬─────────┬─────────┬─────────┘ │
│          │         │         │         │         │            │
│          ▼         ▼         ▼         ▼         ▼            │
│      ┌──────┐ ┌──────┐ ┌──────┐ ┌──────┐ ┌──────────┐        │
│      │Router│ │Runtime│ │Memory│ │ Audit│ │Settlement│        │
│      └──┬───┘ └──┬───┘ └──┬───┘ └──┬───┘ └──┬───────┘        │
│         │        │        │        │        │                │
└─────────┼────────┼────────┼────────┼────────┼────────────────┘
          │        │        │        │        │
          ▼        ▼        ▼        ▼        ▼
       cards    spawned  SQLite  JSONL    JSONL     local
       on disk  processes  +    audit/   receipts  settlement
                          embeds events           scaffold`}</code>
      </pre>

      <h2>Process model</h2>
      <p>
        <code>covenantd</code> runs as a single process per machine,
        owned by the operator&apos;s user account. It owns:
      </p>

      <ul>
        <li>
          a Unix socket at <code>$COVENANT_HOME/sock</code> for local
          clients,
        </li>
        <li>
          an HTTP listener on <code>127.0.0.1:8421</code> for browser-
          facing UIs and third-party tooling (loopback only),
        </li>
        <li>
          the SQLite memory database at{" "}
          <code>$COVENANT_HOME/memory.db</code>,
        </li>
        <li>
          append-only JSONL stores at{" "}
          <code>audit/events.jsonl</code>,{" "}
          <code>capabilities/granted.jsonl</code>,{" "}
          <code>capabilities/revoked.jsonl</code>, and{" "}
          <code>receipts/working.jsonl</code>,
        </li>
        <li>
          the local ed25519 identity key.
        </li>
      </ul>

      <p>
        Each registered agent runs as a child process spawned on demand
        when an intent is dispatched. Agents have no direct access to the
        daemon&apos;s state — every interaction goes through the daemon.
        The runtime wall-clocks each agent against the budget declared in
        its manifest and kills processes that overrun.
      </p>

      <h2>Request lifecycle</h2>

      <ol>
        <li>
          A client (CLI, web UI, third-party caller) sends a{" "}
          <code>SubmitIntent</code> request over the Unix socket or HTTP.
        </li>
        <li>
          The daemon checks the intent text against the configured
          ignore set; matches are short-circuited with an
          <code>IntentIgnored</code> audit event and skipped.
        </li>
        <li>
          The router scores the intent against registered agent
          capability cards via keyword overlap and selects the best
          match (or falls back to an echo response).
        </li>
        <li>
          The daemon runs a capability check for the matched agent&apos;s
          required actions. The check writes a{" "}
          <code>CapabilityCheck</code> audit event regardless of outcome.
          A failed check rejects the dispatch with{" "}
          <code>Response::Error</code>.
        </li>
        <li>
          On success, the runtime spawns the agent, sends the intent on
          stdin, reads the result on stdout, and enforces the
          wall-clock budget.
        </li>
        <li>
          The daemon writes a working-tier <code>MemoryRecord</code>{" "}
          (with an embedding vector if an embedder is configured), a{" "}
          <code>SettlementReceipt</code> for the resources consumed,
          and an <code>IntentDispatched</code> audit event.
        </li>
        <li>
          The client receives an <code>IntentResult</code> with the
          intent UUID, the result text, sources, and (when applicable)
          the receipt.
        </li>
      </ol>

      <h2>State on disk</h2>

      <p>
        Everything Covenant remembers about its operations sits under{" "}
        <code>$COVENANT_HOME</code>. Default location is{" "}
        <code>~/.covenant</code>; override with the{" "}
        <code>COVENANT_HOME</code> environment variable.
      </p>

      <table>
        <thead>
          <tr>
            <th>Path</th>
            <th>Format</th>
            <th>Owner</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>
              <code>identity/local.key</code>
            </td>
            <td>raw 32 bytes (ed25519 seed)</td>
            <td>covenant-identity</td>
          </tr>
          <tr>
            <td>
              <code>memory.db</code>
            </td>
            <td>SQLite</td>
            <td>covenant-memory</td>
          </tr>
          <tr>
            <td>
              <code>audit/events.jsonl</code>
            </td>
            <td>JSONL, append-only</td>
            <td>covenant-audit</td>
          </tr>
          <tr>
            <td>
              <code>capabilities/granted.jsonl</code>
            </td>
            <td>JSONL, append-only</td>
            <td>covenant-permissions</td>
          </tr>
          <tr>
            <td>
              <code>capabilities/revoked.jsonl</code>
            </td>
            <td>JSONL, append-only (tombstones)</td>
            <td>covenant-permissions</td>
          </tr>
          <tr>
            <td>
              <code>receipts/working.jsonl</code>
            </td>
            <td>JSONL, append-only</td>
            <td>covenant-settlement</td>
          </tr>
          <tr>
            <td>
              <code>agents/*.toml</code>
            </td>
            <td>TOML, one manifest per file</td>
            <td>covenant-router</td>
          </tr>
          <tr>
            <td>
              <code>secrets.toml</code>
            </td>
            <td>TOML</td>
            <td>covenant-llm, covenant-tools, covenant-mcp</td>
          </tr>
          <tr>
            <td>
              <code>.covenantignore</code>
            </td>
            <td>gitignore-style patterns</td>
            <td>covenant-memory</td>
          </tr>
          <tr>
            <td>
              <code>sock</code>
            </td>
            <td>Unix domain socket</td>
            <td>covenant-ipc</td>
          </tr>
        </tbody>
      </table>

      <h2>Crate layout</h2>

      <p>
        The daemon is composed from a number of small Rust crates, each
        owning one primitive. Each crate exposes a trait + at least two
        implementations (one for production, one in-memory for tests),
        which keeps the daemon&apos;s wiring straightforward and the
        test suite fast.
      </p>

      <table>
        <thead>
          <tr>
            <th>Crate</th>
            <th>Role</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>
              <code>covenant-types</code>
            </td>
            <td>
              Wire-level types shared by every other crate ({" "}
              <code>Intent</code>, <code>AgentId</code>,{" "}
              <code>Capability</code>, <code>MemoryRecord</code>,{" "}
              <code>SettlementReceipt</code>).
            </td>
          </tr>
          <tr>
            <td>
              <code>covenant-manifest</code>
            </td>
            <td>
              Parser and validator for <code>agent.toml</code>.
            </td>
          </tr>
          <tr>
            <td>
              <code>covenant-router</code>
            </td>
            <td>
              Loads agent manifests and matches intents to agents via
              keyword overlap.
            </td>
          </tr>
          <tr>
            <td>
              <code>covenant-runtime</code>
            </td>
            <td>
              Subprocess agent runner with stdin/stdout JSON protocol
              and a wall-clock budget.
            </td>
          </tr>
          <tr>
            <td>
              <code>covenant-memory</code>
            </td>
            <td>
              Three-tier memory store (SQLite + in-memory) with cosine
              similarity search over stored vectors.
            </td>
          </tr>
          <tr>
            <td>
              <code>covenant-identity</code>
            </td>
            <td>
              ed25519 identity, on-disk persistence, signing helpers.
            </td>
          </tr>
          <tr>
            <td>
              <code>covenant-permissions</code>
            </td>
            <td>
              Capability tokens — sign, verify, persist, revoke.
            </td>
          </tr>
          <tr>
            <td>
              <code>covenant-audit</code>
            </td>
            <td>
              Append-only audit log with JSONL and in-memory backends.
            </td>
          </tr>
          <tr>
            <td>
              <code>covenant-settlement</code>
            </td>
            <td>
              Settlement primitive: receipts, credits, off-chain
              accounting that pairs with the on-chain program.
            </td>
          </tr>
          <tr>
            <td>
              <code>covenant-llm</code>
            </td>
            <td>
              Provider trait with mock, Ollama, Anthropic, and
              OpenAI-compatible implementations.
            </td>
          </tr>
          <tr>
            <td>
              <code>covenant-tools</code>
            </td>
            <td>
              Tool provider trait with mock, Brave, and SerpAPI search
              implementations.
            </td>
          </tr>
          <tr>
            <td>
              <code>covenant-mcp</code>
            </td>
            <td>
              Model Context Protocol integration — tool trait, registry,
              native tools, stdio JSON-RPC transport for external
              servers.
            </td>
          </tr>
          <tr>
            <td>
              <code>covenant-a2a</code>
            </td>
            <td>
              Agent-to-agent task and result envelopes; in-process
              mailbox.
            </td>
          </tr>
          <tr>
            <td>
              <code>covenant-ipc</code>
            </td>
            <td>
              Length-prefixed JSON IPC protocol for the local socket.
            </td>
          </tr>
          <tr>
            <td>
              <code>covenantd</code>
            </td>
            <td>
              The daemon binary. Wires the primitives together; exposes
              the IPC and HTTP surfaces.
            </td>
          </tr>
          <tr>
            <td>
              <code>covenant</code>
            </td>
            <td>
              The CLI binary.
            </td>
          </tr>
        </tbody>
      </table>

      <h2>Settlement scaffold</h2>
      <p>
        Covenant records local settlement receipts today. The repository
        also contains an experimental Anchor program for the future Solana
        settlement path described in <Link href="/settlement">Settlement</Link>.
        Credit minting, burn reconciliation, oracle integration, and provider
        payout flows are tracked as protocol hardening work.
      </p>

      <p>
        The design boundary is deliberate: local receipts make resource
        accounting inspectable while the on-chain authority surface remains a
        hardening target.
      </p>

      <h2>Position in the stack</h2>
      <p>
        Covenant operates between the host operating system and user-facing
        agentic applications. It does not host language models and does not
        prescribe agent reasoning strategies. Custom agents, framework-built
        agents, and end-to-end fine-tuned agents integrate against the same
        primitive set, so that identity, permissions, memory, communication,
        and settlement are provided as shared host-level services rather
        than reimplemented per application.
      </p>
    </>
  );
}
