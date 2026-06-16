# Agent SDK (Rust)

`covenant-sdk` is the ergonomic async client for building agents against a
running `covenantd` daemon. Agents talk to the daemon over a length-prefixed
JSON protocol on a Unix socket; the SDK wraps the connect, authenticate, and
request/response round-trips so you never hand-roll a wire frame.

The client borrows the daemon's own protocol types (`covenant-ipc`'s
`Request`/`Response`), so the typed surface here cannot drift from the wire
shapes the daemon accepts.

## Connecting

`Client::connect_default()` resolves the daemon home from the environment
(`$COVENANT_HOME`, default `$HOME/.covenant`), reads the bootstrap token from
`<home>/peers/operator.token`, and authenticates. Start `covenantd` at least
once to mint that token (see [getting started](./getting-started.md)).

```rust
use covenant_sdk::{Client, MemoryTier};

#[tokio::main]
async fn main() -> Result<(), covenant_sdk::SdkError> {
    let mut client = Client::connect_default().await?;
    println!("bound as {}", client.identity());

    // Discover and call tools.
    let tools = client.list_tools().await?;
    let result = client
        .call_tool("echo", serde_json::json!({ "text": "hi" }))
        .await?;

    // Submit an intent and read recent memory.
    let outcome = client.submit_intent("summarize today's audit log").await?;
    let recent = client.recent_memory(Some(MemoryTier::Working), 10).await?;

    let _ = (tools, result, outcome, recent);
    Ok(())
}
```

For an explicit home or token, use `Client::connect_with_token_file(home)` or
`Client::connect_authenticated(home, token_b58)`.

## What the surface covers

| Area | Methods |
|---|---|
| Lifecycle | `connect_default`, `connect_with_token_file`, `connect_authenticated`, `identity`, `ping`, `protocol_info` |
| Intents | `submit_intent` |
| Tools | `list_tools`, `call_tool` |
| Memory (read) | `recent_memory`, `search_memory` |
| A2A (worker) | `try_recv_a2a_task`, `post_a2a_result`, `try_recv_a2a_result` |
| Capabilities | `recent_capabilities`, `grant_capability` |

Every method is a single request/response round-trip on one authenticated
connection, which stays open for reuse across calls.

## Examples

Runnable client agents live in
[`agent-os/crates/covenant-sdk/examples`](../agent-os/crates/covenant-sdk/examples).
Start `covenantd` first so the socket and operator token exist, then:

| Example | Demonstrates | Run |
|---|---|---|
| `tool_agent` | discover the router's tools and call one | `cargo run -p covenant-sdk --example tool_agent` |
| `memory_agent` | semantic search and recent-memory reads | `cargo run -p covenant-sdk --example memory_agent` |
| `a2a_worker` | drain delegated a2a tasks and post results | `cargo run -p covenant-sdk --example a2a_worker` |

Each is a self-contained starting point: copy one, replace the placeholder
work, and you have an agent. The `a2a_worker` example treats incoming
`intent_text` as opaque data and reports task failures back as error results
rather than crashing its loop.

## Errors

Methods return `SdkError`, which distinguishes:

- transport failures (`Connect`, `Token`, `Wire`),
- a rejected token (`Authentication`),
- a **policy denial** (`Denied`) — a missing capability or an out-of-scope
  grant; the connection stays usable,
- any other daemon-level rejection (`Daemon`) — also leaves the connection
  usable,
- a response variant the SDK does not expect for a verb (`Unexpected`).

### Acting on a denial

`SdkError::Denied` carries the daemon's full `message`, a `kind`
(`MissingCapability` or `OutOfScope`), and — when the daemon named it — the
`capability` to request. That value is exactly the argument to
`grant_capability`, so an author can debug against policy without reading daemon
internals and close the loop in code:

```rust
use covenant_sdk::{DenialKind, SdkError};

match client.call_tool("search", args).await {
    Ok(result) => { /* use result */ }
    Err(SdkError::Denied {
        capability: Some(action),
        kind: DenialKind::MissingCapability,
        ..
    }) => {
        client.grant_capability(action, None, None).await?;
        // ...then retry the call
    }
    Err(other) => return Err(other),
}
```

Classification is best-effort over the daemon's denial wording: an unrecognized
message stays `Daemon` with its text intact, so nothing is lost — only
unclassified.

## Not yet exposed

- **Memory writes.** Memory is read-only over IPC today. The daemon writes
  memory as a side effect of intent execution; there is no client-facing
  memory-write verb to wrap.
- **Sending a2a tasks.** The worker side — receive a task, post a result, poll
  for a result — is wrapped. *Sending* a task is not: the daemon binds
  `task.sender` to the authenticated peer and requires an `a2a.send` capability
  scoped to the recipient, and the handshake returns only a display name, so the
  client cannot populate `sender` with its own public key.
- **Streaming responses.** The v2 streaming opt-in (see the
  [protocol v2 migration notes](./protocol-migrations/v2.md)) is not part of this
  surface; the SDK uses terminal v1 frames.
- **Operator-only verbs.** Purge, repair, peer-registry, and settlement-backfill
  verbs are operator surfaces, not author surfaces, and are intentionally out of
  scope for the SDK.
