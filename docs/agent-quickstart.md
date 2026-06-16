# Agent quickstart

From zero to a running, capability-scoped agent on your local control plane. This
builds a **client agent** in Rust with [`covenant-sdk`](./agent-sdk.md) — a
program that connects to a running daemon over IPC. For the daemon and the
bundled Python `hello-agent`, see [getting started](./getting-started.md).

## 1. Build and start the daemon

Build the daemon and CLI once (full detail in [getting started](./getting-started.md)):

```bash
cd agent-os
cargo build --release --locked
```

Start the daemon. On first run it creates `$COVENANT_HOME` (default
`~/.covenant`), its IPC socket, and the operator bootstrap token at
`~/.covenant/peers/operator.token`:

```bash
./target/release/covenantd
```

Leave it running. The SDK's `Client::connect_default()` finds the socket and
that token automatically.

## 2. Add the SDK to your project

`covenant-sdk` is an in-repo crate. Point your `Cargo.toml` at it and pull in an
async runtime:

```toml
[dependencies]
covenant-sdk = { path = "../agent-os/crates/covenant-sdk" }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
serde_json = "1"
```

## 3. Write the agent

A fresh agent holds **no authority**. The daemon denies a tool call until the
capability exists, so the agent asks for the grant the denial names and retries
— scoped on demand, never a blanket grant up front:

```rust
use covenant_sdk::{Client, DenialKind, SdkError};

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
            println!("granting {action} and retrying");
            client.grant_capability(action, None, None).await?;
            client.call_tool("echo", args).await?
        }
        Err(other) => return Err(other),
    };

    println!("tool returned {} block(s)", result.content.len());
    Ok(())
}
```

The same denial-then-grant flow is a compile-checked example in the SDK's module
docs, so it cannot silently drift from the API.

An operator can also grant ahead of time from the CLI. A tool call needs the
`tool.call.<name>` capability the denial names:

```bash
covenant capabilities grant tool.call.echo
```

Either way the grant is **scoped** — the agent gets `tool.call.echo` and nothing
more.

## 4. Run it

Or skip straight to the bundled example, which discovers and calls a tool:

```bash
cargo run -p covenant-sdk --example tool_agent
```

See [`agent-os/crates/covenant-sdk/examples`](../agent-os/crates/covenant-sdk/examples)
for the `memory_agent` and `a2a_worker` examples too.

## 5. Verify what happened

Every call left a tamper-evident trail. Inspect and verify it:

```bash
covenant audit recent -n 10
covenant audit verify
```

You should see the capability grant and the tool call recorded, and the chain
verify clean.

## Where to go next

- [`docs/agent-sdk.md`](./agent-sdk.md): the full client surface and error model
- [`examples/`](../examples/): runnable agents, daemon-spawned and SDK-driven
- [`docs/getting-started.md`](./getting-started.md): the daemon, CLI, and demo
