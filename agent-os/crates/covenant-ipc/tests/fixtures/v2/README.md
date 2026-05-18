# Protocol v2 Fixture Staging

ADR 0010 introduced v2 streaming responses via a staged bump: `MAX_PROTOCOL_VERSION` is `2` (daemon advertises v2 support) while `PROTOCOL_VERSION` remains `1` (the wire form the daemon emits by default). v1 fixture replay stays byte-for-byte; v2 fixtures here pin shapes that only exist in the v2 surface. All four `StreamEnvelope` variants are pinned: `stream-envelope-begin.v2.json`, `stream-envelope-chunk.v2.json`, `stream-envelope-end.v2.json` (no-summary terminal), `stream-envelope-end-with-summary.v2.json` (with-summary terminal for `SubmitIntent`), and `stream-envelope-error.v2.json`.

When adding fixtures here:

- add `*.v2.json` files for envelopes that don't exist in v1 (`StreamBegin`, `StreamChunk`, `StreamEnd`, `StreamError`) or for additional case shapes (e.g., the with-summary `StreamEnd` for SubmitIntent rollups);
- keep root `*.v1.json` fixtures pinning the current daemon snapshot — when the daemon's emitted shape changes, update the root fixture in the same slice;
- the `covenant-ipc` test suite fails closed if a supported protocol version lacks matching `*.vN.json` fixtures or `docs/protocol-migrations/vN.md`;
- a future re-coupling that bumps `PROTOCOL_VERSION` to `2` (so the daemon emits v2 frames by default) is a follow-up bump documented in its own migration note.
