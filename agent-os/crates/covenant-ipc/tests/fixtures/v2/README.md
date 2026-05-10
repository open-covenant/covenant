# Protocol v2 Fixture Staging

This directory is intentionally empty of JSON fixtures while `PROTOCOL_VERSION` remains `1`.

When the IPC/HTTP protocol intentionally moves to v2:

- add `*.v2.json` response fixtures here for every changed stable envelope;
- keep root `*.v1.json` fixtures until v1 support is deliberately removed;
- add `docs/protocol-migrations/v2.md` with the compatibility window, breaking changes, and client expectations;
- update `PROTOCOL_VERSION`, `MIN_PROTOCOL_VERSION`, and `MAX_PROTOCOL_VERSION` together.

The `covenant-ipc` test suite fails closed if v2 fixture JSON appears before the version bump, or if a future bump lacks fixtures and migration notes.
