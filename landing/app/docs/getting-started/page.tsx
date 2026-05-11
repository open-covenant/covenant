import Link from "next/link";
import { buildDocsMetadata } from "../_meta";

export const metadata = buildDocsMetadata("getting-started", "Getting started", 'Install Covenant from source, run the daemon, and submit your first intent.');

export default function GettingStartedPage() {
  return (
    <>
      <h1>Getting started</h1>
      <p>
        This guide installs Covenant from source, brings up the daemon, and
        dispatches a first intent end-to-end. The full loop runs on the
        local host without external accounts or credentials.
      </p>

      <h2>Prerequisites</h2>

      <table>
        <thead>
          <tr>
            <th>Tool</th>
            <th>Version</th>
            <th>Purpose</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>
              <code>rustc</code> / <code>cargo</code>
            </td>
            <td>stable (1.80+)</td>
            <td>Builds the daemon, the CLI, and every workspace crate.</td>
          </tr>
          <tr>
            <td>Node.js</td>
            <td>22+</td>
            <td>Optional — required only to run the landing site locally.</td>
          </tr>
          <tr>
            <td>pnpm</td>
            <td>10+</td>
            <td>Optional — same as Node.js.</td>
          </tr>
          <tr>
            <td>Anchor + solana-cli</td>
            <td>Anchor 0.31+</td>
            <td>
              Optional — required only to build the on-chain settlement
              program.
            </td>
          </tr>
        </tbody>
      </table>

      <h2>Clone and build</h2>

      <pre>
        <code>{`git clone https://github.com/open-covenant/covenant.git
cd covenant
cargo build --workspace --exclude covenant-settlement-program`}</code>
      </pre>

      <p>
        The first build downloads dependencies and may take a few minutes.
        Two binaries land under <code>target/debug/</code>:
      </p>

      <ul>
        <li>
          <code>covenantd</code> — the daemon. Long-running. Listens on a
          Unix socket and an HTTP gateway.
        </li>
        <li>
          <code>covenant</code> — the command-line client. Speaks to the
          daemon over the Unix socket.
        </li>
      </ul>

      <h2>Configure</h2>

      <p>
        Covenant looks for its state under <code>$COVENANT_HOME</code> (default{" "}
        <code>~/.covenant</code>). The directory is created on first
        daemon start; you can pre-create it to drop in a configuration file:
      </p>

      <pre>
        <code>{`mkdir -p ~/.covenant`}</code>
      </pre>

      <p>
        Configuration lives in{" "}
        <code>~/.covenant/secrets.toml</code>. A minimal example pointing at
        a local Ollama instance:
      </p>

      <pre>
        <code>{`[llm]
provider = "ollama"
model    = "qwen2.5:7b"

[embed]
provider = "ollama"
model    = "nomic-embed-text"
`}</code>
      </pre>

      <p>
        In the absence of this file, Covenant defaults to a mock LLM and a
        mock search provider; the research agent will return placeholder
        text under that configuration. Refer to{" "}
        <Link href="/concepts">Concepts</Link> and{" "}
        <Link href="/agent-manifest">Agent manifest</Link> for the full
        configuration surface.
      </p>

      <h2>Run the daemon</h2>

      <pre>
        <code>{`./target/debug/covenantd`}</code>
      </pre>

      <p>The daemon initialises itself on first run:</p>

      <ul>
        <li>
          Generates an ed25519 identity at{" "}
          <code>$COVENANT_HOME/identity/local.key</code> with mode{" "}
          <code>0600</code>.
        </li>
        <li>
          Opens a Unix socket at <code>$COVENANT_HOME/sock</code>.
        </li>
        <li>
          Binds an HTTP gateway on <code>127.0.0.1:8421</code>.
        </li>
        <li>
          Opens the SQLite memory store at{" "}
          <code>$COVENANT_HOME/memory.db</code>.
        </li>
        <li>
          Loads any agent manifests under{" "}
          <code>$COVENANT_HOME/agents/*.toml</code>.
        </li>
      </ul>

      <h2>Submit a first intent</h2>

      <p>From a second terminal session:</p>

      <pre>
        <code>{`./target/debug/covenant ping
# → pong

./target/debug/covenant intent "summarise recent work on agent memory"`}</code>
      </pre>

      <p>
        With no agents registered, the daemon returns a deterministic echo
        response and persists a working-tier memory record alongside a
        settlement receipt. Inspect both:
      </p>

      <pre>
        <code>{`./target/debug/covenant memory recent
./target/debug/covenant receipts recent --json`}</code>
      </pre>

      <h2>Register an agent</h2>

      <p>
        Place an agent manifest under{" "}
        <code>$COVENANT_HOME/agents/</code>. The repository includes a{" "}
        <code>research</code> agent at <code>agents/research</code>. Build
        the agent and create a manifest pointing at the resulting binary:
      </p>

      <pre>
        <code>{`cargo build -p research-agent --release

mkdir -p ~/.covenant/agents
cat > ~/.covenant/agents/research.toml <<'EOF'
[agent]
id      = "research@local"
name    = "research"
version = "0.1.0"
runtime = "rust-bin"
entry   = "target/release/research"

[capabilities]
required = ["tool.web_search"]

[resources]
cpu_ms_per_task = 30000
memory_mb       = 512
disk_mb         = 100
network         = "outbound-https-only"
EOF`}</code>
      </pre>

      <p>
        Restart the daemon, grant the required capability, and submit an
        intent that matches the agent&apos;s registered keywords:
      </p>

      <pre>
        <code>{`./target/debug/covenant capabilities grant tool.web_search
./target/debug/covenant intent "search for recent papers on agent memory"`}</code>
      </pre>

      <p>
        The daemon routes the intent to <code>research@local</code>,
        executes the binary as a subprocess, captures the response,
        persists the memory record and settlement receipt, and emits audit
        events for both the dispatch and the capability check.
      </p>

      <h2>Verify local state</h2>

      <p>Each primitive&apos;s recent state is queryable through the CLI:</p>

      <pre>
        <code>{`covenant memory recent --limit 20
covenant chain status --json
covenant receipts recent --limit 20 --json
covenant chain flush-receipts --limit 20 --json
covenant chain receipt-batches --limit 20 --json
covenant capabilities recent --limit 20 --json
covenant capabilities purge --older-than-ms 2592000000 --json
covenant verify --window 100 --json`}</code>
      </pre>

      <p>
        <code>covenant verify</code> cross-checks the audit log against the
        memory store, capability ledger, and settlement receipts and
        reports any drift.
      </p>

      <h2>Further reading</h2>

      <ul>
        <li>
          <Link href="/concepts">Concepts</Link> — data model: intents,
          agents, capabilities, memory, audit, and settlement.
        </li>
        <li>
          <Link href="/cli">CLI reference</Link> — complete subcommand
          reference.
        </li>
        <li>
          <Link href="/http-api">HTTP API</Link> — gateway routes for
          browser-facing UIs and third-party integrations.
        </li>
        <li>
          <Link href="/agent-manifest">Agent manifest</Link> — full{" "}
          <code>agent.toml</code> schema.
        </li>
        <li>
          <Link href="/security">Security model</Link> — trust
          boundaries and operator responsibilities.
        </li>
      </ul>
    </>
  );
}
