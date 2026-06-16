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
| Capabilities | `recent_capabilities`, `grant_capability` |

Every method is a single request/response round-trip on one authenticated
connection, which stays open for reuse across calls.

## Errors

Methods return `SdkError`, which distinguishes:

- transport failures (`Connect`, `Token`, `Wire`),
- a rejected token (`Authentication`),
- a daemon-level rejection such as a denied capability (`Daemon`) — the
  connection stays usable for the next request,
- a response variant the SDK does not expect for a verb (`Unexpected`).

## Not yet exposed

- **Memory writes.** Memory is read-only over IPC today. The daemon writes
  memory as a side effect of intent execution; there is no client-facing
  memory-write verb to wrap.
- **Streaming responses.** The v2 streaming opt-in (see the
  [protocol v2 migration notes](./protocol-migrations/v2.md)) is not part of this
  surface; the SDK uses terminal v1 frames.
- **Operator-only verbs.** Purge, repair, peer-registry, and settlement-backfill
  verbs are operator surfaces, not author surfaces, and are intentionally out of
  scope for the SDK.
