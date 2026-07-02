# covenant-sdk

Ergonomic async client for building agents against a running `covenantd` daemon.

Covenant agents talk to the daemon over a length-prefixed JSON IPC protocol on a Unix
socket (`$COVENANT_HOME/sock`). `covenant-sdk` wraps the connect, authenticate, and typed
request/response round-trips so an author never hand-rolls a wire frame — you write against
typed methods and domain types, and the frame codec, the authentication handshake, and
response demultiplexing stay out of sight.

The client borrows the daemon's own protocol types (`covenant-ipc`'s `Request`/`Response`),
so the typed surface here cannot drift from the wire shapes the daemon accepts.

## Status

`0.1.0`, tracking Covenant IPC protocol v1. This is an in-workspace client: it depends on the
sibling `covenant-*` crates by path, so it is not published to crates.io — consume it from
within the workspace.

## Install

```toml
[dependencies]
covenant-sdk = { path = "crates/covenant-sdk" } # workspace-relative
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

A `covenantd` daemon must be running: the SDK connects to its socket and reads the operator
bootstrap token the daemon mints on first start.

## Quick start

```rust,no_run
use covenant_sdk::{Client, MemoryTier};

#[tokio::main]
async fn main() -> Result<(), covenant_sdk::SdkError> {
    let mut client = Client::connect_default().await?;
    println!("connected as {}", client.identity());

    let tools = client.list_tools().await?;
    let result = client.call_tool("echo", serde_json::json!({ "text": "hi" })).await?;
    let recent = client.recent_memory(Some(MemoryTier::Working), 10).await?;

    println!("{} tools, {} recent records", tools.len(), recent.len());
    let _ = result;
    Ok(())
}
```

`connect_default` resolves the daemon home from `$COVENANT_HOME` (else `$HOME/.covenant`) and
reads the token at `<home>/peers/operator.token`. Each method is a single request/response
round-trip; the connection stays open and bound to the authenticated identity, so reuse one
`Client` across calls.

## Capability-scoped calls

An agent starts with no authority. Call a tool, and if the daemon denies it for a missing
capability, request the grant it named and retry — the secure default, never a blanket grant
up front:

```rust,no_run
use covenant_sdk::{Client, DenialKind, SdkError};

async fn call_with_grant(client: &mut Client) -> Result<(), SdkError> {
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

    let _ = result;
    Ok(())
}
```

## What the client covers

| Area | Methods |
| --- | --- |
| Liveness / protocol | `ping`, `protocol_info` |
| Intents | `submit_intent` |
| Tools | `list_tools`, `call_tool` |
| Memory (read-only) | `recent_memory`, `search_memory` |
| Capabilities | `recent_capabilities`, `grant_capability` |
| Agent-to-agent (worker side) | `try_recv_a2a_task`, `post_a2a_result`, `try_recv_a2a_result` |

## Deliberately not exposed

- **Memory writes.** The daemon writes memory as a side effect of intent execution; there is no
  client-facing write verb to wrap.
- **Sending an a2a task.** The daemon binds `task.sender` to the authenticated peer and requires
  an `a2a.send` capability scoped to the recipient; the handshake returns only a display name, so
  the client cannot populate `sender`. The worker (receiving) half is wrapped.
- **Streaming responses** (protocol v2) and **operator-only verbs** (purge, repair, peer registry,
  settlement backfill) sit outside this author-facing surface.

## Errors

`SdkError` separates transport, protocol, and policy failures. Recognized policy denials surface
as `SdkError::Denied` with the required capability extracted from the daemon's message when it
named one — ready to pass to `grant_capability`. A daemon-level error leaves the connection
usable; a `Wire` error means the transport is broken and the connection should be replaced.
Unexpected responses report only the response's `kind` tag, never a payload field, so a
secret-bearing frame can never spill its value into an error string.

## Examples

Start `covenantd`, then:

```bash
cargo run -p covenant-sdk --example tool_agent    # discover and call a tool
cargo run -p covenant-sdk --example memory_agent  # search and list memory
cargo run -p covenant-sdk --example a2a_worker    # drain and answer delegated tasks
```

## License

Apache-2.0.
