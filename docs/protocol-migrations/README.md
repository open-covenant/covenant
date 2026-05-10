# Protocol Migration Notes

This directory records intentional IPC/HTTP protocol version bumps.

Each supported protocol version above v1 must have a `vN.md` note before the constants in `covenant-ipc` move to that version. A migration note should state:

- compatibility window;
- breaking wire-shape changes;
- affected IPC and HTTP surfaces;
- fixture files added for replay;
- expected client behavior during rollout.

No v2 migration note exists yet because the implementation still advertises protocol v1 only.
