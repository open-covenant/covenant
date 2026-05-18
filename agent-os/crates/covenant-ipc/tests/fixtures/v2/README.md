# Protocol v2 Fixture Staging

ADR 0010 introduced v2 streaming responses via a staged bump: `MAX_PROTOCOL_VERSION` is now `2` (daemon advertises v2 support) while `PROTOCOL_VERSION` remains `1` (the wire form the daemon emits by default). v1 fixture replay stays byte-for-byte; v2 fixtures here pin shapes that only exist in the v2 surface (e.g., `StreamEnvelope` variants).

When adding fixtures here:

- add `*.v2.json` files for envelopes that don't exist in v1 (`StreamBegin`, `StreamChunk`, `StreamEnd`, `StreamError`);
- keep root `*.v1.json` fixtures pinning the current daemon snapshot — when the daemon's emitted shape changes, update the root fixture in the same slice;
- the `covenant-ipc` test suite fails closed if v2 fixture JSON appears while `MAX_PROTOCOL_VERSION` is still `1`, or if a future bump lacks fixtures and migration notes;
- a future re-coupling that bumps `PROTOCOL_VERSION` to `2` (so the daemon emits v2 frames by default) is a follow-up bump documented in its own migration note.
