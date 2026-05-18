# Protocol Migration Notes

This directory records intentional IPC/HTTP protocol version bumps.

Each supported protocol version above v1 must have a `vN.md` note before the constants in `covenant-ipc` move to that version. A migration note should state:

- compatibility window;
- breaking wire-shape changes;
- affected IPC and HTTP surfaces;
- fixture files added for replay;
- expected client behavior during rollout.

[v2.md](./v2.md) is the first concrete migration note. It records the staged bump introduced by ADR 0010: `PROTOCOL_VERSION` stays `1` (default emitted wire form) while `MAX_PROTOCOL_VERSION` is `2` (advertised support for opt-in streaming responses), and lists the `StreamEnvelope` fixture set under `agent-os/crates/covenant-ipc/tests/fixtures/v2/`.
