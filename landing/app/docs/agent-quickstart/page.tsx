import Link from "next/link";
import { buildDocsMetadata, buildDocsJsonLd } from "../_meta";

const META_ARGS = [
  "agent-quickstart",
  "Agent quickstart",
  "From zero to a running, capability-scoped agent: build a Rust client agent on the local control plane with covenant-sdk.",
] as const;
export const metadata = buildDocsMetadata(...META_ARGS);

export default function AgentQuickstartPage() {
  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(buildDocsJsonLd(...META_ARGS)) }}
      />
      <h1>Agent quickstart</h1>
      <p>
        From zero to a running, capability-scoped agent on your local control
        plane. This builds a <strong>client agent</strong> in Rust with{" "}
        <code>covenant-sdk</code> — a program that connects to a running daemon
        over IPC. For the daemon itself and the bundled Python{" "}
        <code>hello-agent</code>, see{" "}
        <Link href="/getting-started">getting started</Link>.
      </p>

      <h2>1. Build and start the daemon</h2>
      <p>Build the daemon and CLI once:</p>
      <pre>
        <code>{`cd agent-os
cargo build --release --locked`}</code>
      </pre>
      <p>
        Start the daemon. On first run it creates <code>$COVENANT_HOME</code>{" "}
        (default <code>~/.covenant</code>), its IPC socket, and the operator
        bootstrap token at <code>~/.covenant/peers/operator.token</code>:
      </p>
      <pre>
        <code>{`./target/release/covenantd`}</code>
      </pre>
      <p>
        Leave it running. The SDK&apos;s <code>Client::connect_default()</code>{" "}
        finds the socket and that token automatically.
      </p>

      <h2>2. Add the SDK to your project</h2>
      <p>
        <code>covenant-sdk</code> is an in-repo crate. Point your{" "}
        <code>Cargo.toml</code> at it and pull in an async runtime:
      </p>
      <pre>
        <code>{`[dependencies]
covenant-sdk = { path = "../agent-os/crates/covenant-sdk" }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
serde_json = "1"`}</code>
      </pre>

      <h2>3. Write the agent</h2>
      <p>
        A fresh agent holds <strong>no authority</strong>. The daemon denies a
        tool call until the capability exists, so the agent asks for the grant
        the denial names and retries — scoped on demand, never a blanket grant
        up front:
      </p>
      <pre>
        <code>{`use covenant_sdk::{Client, DenialKind, SdkError};

#[tokio::main]
async fn main() -> Result<(), SdkError> {
    let mut client = Client::connect_default().await?;
    println!("connected as {}", client.identity());

    let args = serde_json::json!({ "text": "hi" });
    let result = match client.call_tool("echo", args.clone()).await {
        Ok(result) => result,
        Err(SdkError::Denied {
            capability: Some(action),
            kind: DenialKind::MissingCapability,
            ..
        }) => {
            client.grant_capability(action, None, None).await?;
            client.call_tool("echo", args).await?
        }
        Err(other) => return Err(other),
    };

    println!("tool returned {} block(s)", result.content.len());
    Ok(())
}`}</code>
      </pre>
      <p>
        The same denial-then-grant flow is a compile-checked example in the
        SDK&apos;s module docs, so it cannot silently drift from the API. An
        operator can also grant ahead of time from the CLI with{" "}
        <code>covenant capabilities grant tool.call.echo</code> — a tool call
        needs the <code>tool.call.&lt;name&gt;</code> capability the denial
        names. Either way the grant is{" "}
        <Link href="/capabilities">scoped</Link> — the agent gets{" "}
        <code>tool.call.echo</code> and nothing more.
      </p>

      <h2>4. Run it</h2>
      <p>
        Or skip straight to the bundled example, which discovers and calls a
        tool:
      </p>
      <pre>
        <code>{`cargo run -p covenant-sdk --example tool_agent`}</code>
      </pre>
      <p>
        The <code>memory_agent</code> and <code>a2a_worker</code> examples ship
        alongside it.
      </p>

      <h2>5. Verify what happened</h2>
      <p>Every call left a tamper-evident trail. Inspect and verify it:</p>
      <pre>
        <code>{`covenant audit recent -n 10
covenant audit verify`}</code>
      </pre>
      <p>
        You should see the capability grant and the tool call recorded, and the
        chain <Link href="/audit-integrity">verify</Link> clean.
      </p>

      <h2>Where to go next</h2>
      <ul>
        <li>
          <Link href="/getting-started">Getting started</Link>: the daemon, CLI,
          and end-to-end demo.
        </li>
        <li>
          <Link href="/capabilities">Capabilities</Link>: how scoped authority is
          granted, signed, and enforced.
        </li>
        <li>
          <Link href="/audit-integrity">Audit integrity</Link>: the
          tamper-evident record your agent writes into.
        </li>
      </ul>
    </>
  );
}
