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
    "max_supported": 1
  }
}
```

## Compatibility Rules

- `version` is the daemon's preferred protocol version.
- `min_supported` and `max_supported` define the inclusive compatibility window accepted by the daemon.
- Additive fields that older clients can ignore do not require a version bump when serde defaults keep existing frames parseable.
- Required field removal, field rename, tag rename, semantic reinterpretation, or authentication handshake changes require a protocol version bump.
- `/version` and IPC `ProtocolInfo` stay unauthenticated so stale clients can fail before sending credentials or mutation frames.

## Migration Harness

Versioned fixtures live under `agent-os/crates/covenant-ipc/tests/fixtures`.

The current harness replays every root `*.v1.json` response fixture through the current `Response` parser. `protocol-info.v1.json` is also compared against the current generated value and reused by HTTP gateway tests, so IPC and HTTP metadata cannot drift independently.

Future major fixtures must live in a version directory such as `tests/fixtures/v2/`. The v2 directory is present as a staging boundary but intentionally contains no JSON while the crate still advertises protocol v1. The IPC tests fail closed if:

- `*.v2.json` fixtures appear before `PROTOCOL_VERSION` moves to v2;
- a future supported protocol version lacks matching `*.vN.json` fixtures;
- a future supported protocol version lacks `docs/protocol-migrations/vN.md`.

Before the first breaking change lands:

1. Add `tests/fixtures/v2/*.v2.json` fixtures for every stable response envelope affected by the change.
2. Keep v1 fixtures committed until the code intentionally drops v1 support.
3. Add parser tests for every supported compatibility window.
4. Add `docs/protocol-migrations/v2.md`.
5. Update the internal status matrix, this document, and release notes with the migration boundary.
