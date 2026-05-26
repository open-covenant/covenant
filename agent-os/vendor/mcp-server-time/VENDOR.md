# Vendored: mcp-server-time

A pinned copy of the upstream `time` reference MCP server, used only as an
**opt-in, default-off** live-test fixture for Covenant's external MCP stdio
transport. It is not a build or runtime dependency of any crate and is not part
of the Cargo workspace.

## Provenance

| Field | Value |
|---|---|
| Upstream | https://github.com/modelcontextprotocol/servers |
| Subdirectory | `src/time` (vendored under `a1e5a9a9/time/`) |
| Release tag | `2026.1.26` |
| Pinned commit | `a1e5a9a9b186f00462a8a2448ee041728ce052d5` (2026-01-26) |
| License | MIT (`a1e5a9a9/LICENSE`, copied from the upstream repository root) |
| Tools | `get_current_time`, `convert_time` (system clock + IANA timezone DB only) |
| Side effects | none — no network, no filesystem writes, no subprocesses |

`tree_sha256` of `a1e5a9a9/` (sha256 over the sorted list of per-file sha256s):

```
5b449832f5c44e2845180fdbbaecc9903ee50e94733ec31b76ff2beb48f0ac1b
```

Recompute with:

```
find agent-os/vendor/mcp-server-time/a1e5a9a9 -type f -exec shasum -a 256 {} \; \
  | sed 's#agent-os/vendor/mcp-server-time/a1e5a9a9/##' | sort | shasum -a 256
```

## Opt-in live test

The fixture is exercised by `agent-os/crates/covenant-mcp/tests/live_server_time.rs`,
which is `#[ignore]` and additionally gated on an environment variable, so a
default `cargo test` neither builds nor runs it. To run it, install the server
(e.g. from this vendored source) and point the test at the launch command:

```
COVENANT_LIVE_MCP_SERVER_TIME="uv run --project agent-os/vendor/mcp-server-time/a1e5a9a9/time python -m mcp_server_time" \
  cargo test -p covenant-mcp -- --ignored live_server_time
```

The server reads only the system clock and the interpreter's bundled IANA
timezone database; the test performs no network access.

## Removal

Delete `agent-os/vendor/mcp-server-time/` and `live_server_time.rs` in one commit.
