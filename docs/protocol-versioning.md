# Protocol Versioning

Covenant exposes protocol metadata over IPC `ProtocolInfo`, HTTP `/version`, and `covenant version`.

Current wire contract:

```json
{
  "kind": "protocol_info",
  "info": {
    "protocol": "covenant.ipc",
    "version": 1,
    "min_supported": 1,
    "max_supported": 2
  }
}
```

## Compatibility Rules

- `version` is the daemon's preferred protocol version — the wire form it emits by default.
- `min_supported` and `max_supported` define the inclusive compatibility window accepted by the daemon. `max_supported = 2` advertises that v2 streaming responses are wired and available on an opt-in basis (see below).
- Additive fields that older clients can ignore do not require a version bump when serde defaults keep existing frames parseable.
- Required field removal, field rename, tag rename, semantic reinterpretation, or authentication handshake changes require a protocol version bump.
- `/version` and IPC `ProtocolInfo` stay unauthenticated so stale clients can fail before sending credentials or mutation frames.

## v2 Streaming Responses

ADR 0010 introduced v2 streaming for `RecentMemory`, `RecentAudit`, and `SubmitIntent`. v1 backwards compatibility is non-negotiable: a v1 client that never sets `prefer_stream` continues to receive v1 terminal frames byte-for-byte. A v2-aware client checks `max_supported >= 2` then sets `prefer_stream: true` on a supported request; the daemon may then emit a `StreamBegin` / `StreamChunk` ... / `StreamEnd` sequence (the `StreamEnvelope` enum) instead of the terminal `Response`. Capability or budget failures fall back to a v1 `Response::Error` frame on the same connection.

The daemon advertises the staged bump as `version: 1, max_supported: 2`: the default wire form stays v1 so every existing fixture replays unchanged, and v2 is reachable via the per-verb opt-in. See [docs/protocol-migrations/v2.md](./protocol-migrations/v2.md) for the canonical migration record (compatibility window, affected surfaces, fixture additions, client expectations).

## Migration Harness

Versioned fixtures live under `agent-os/crates/covenant-ipc/tests/fixtures`.

The current harness replays every root `*.v1.json` response fixture through the current `Response` parser. `protocol-info.v1.json` is also compared against the current generated value and reused by HTTP gateway tests, so IPC and HTTP metadata cannot drift independently.

`tests/fixtures/v2/` holds shapes that are v2-only — the four `StreamEnvelope` variants (`StreamBegin`, `StreamChunk`, `StreamEnd`, `StreamError`) are pinned there today. The IPC tests fail closed if:

- a `*.vN.json` fixture sits under `tests/fixtures/vN/` for an `N` that exceeds the codec's `MAX_PROTOCOL_VERSION` (i.e., the codec cannot parse a wire shape it does not yet support);
- a supported protocol version lacks matching `*.vN.json` fixtures;
- a supported protocol version lacks `docs/protocol-migrations/vN.md`.

The canonical record for what landed in the v2 promotion is [docs/protocol-migrations/v2.md](./protocol-migrations/v2.md). Future migrations follow the same pattern: add `*.vN.json` fixtures, write `docs/protocol-migrations/vN.md`, bump the relevant constants, and let the migration-evidence test verify the bundle.
