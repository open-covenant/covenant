# IPC and HTTP Gateway

The Covenant daemon exposes two transport surfaces for the same protocol:

- IPC framed responses parsed by `covenant-ipc`.
- The HTTP gateway exposed by `covenantd` for `/version`, `/tools/call`, audit, peer, and A2A endpoints.

Protocol metadata is reported through `ProtocolInfo` over IPC, the HTTP `/version` route, and the `covenant version` CLI. Compatibility rules and the v1/v2 staging boundary live in [docs/protocol-versioning.md](./protocol-versioning.md). Migration notes live under [docs/protocol-migrations/](./protocol-migrations/README.md). The IPC fixture replay harness pins both transports together so they cannot drift independently.

The actual decision to bump the protocol to v2 and publish v2 fixtures remains human-owned. This document defines the contract a v2 fixture pair must satisfy *when* the bump happens, so the eventual landing is deterministic.

## v2 Fixture Contract

The contract is enforced by an internal validator. The validator no-ops while no `*.v2.json` files exist under `agent-os/crates/covenant-ipc/tests/fixtures/v2/`. Once a v2 fixture appears, the validator enforces the rules below and emits a remediation pointer when any rule is violated.

### File Layout

- v2 response fixtures live under `agent-os/crates/covenant-ipc/tests/fixtures/v2/`.
- Each fixture file is named `<envelope>.v2.json` where `<envelope>` matches the response envelope kind. The same envelope must reuse the same base name as its `*.v1.json` sibling so version diffs are obvious.
- v1 fixtures stay at the root of `agent-os/crates/covenant-ipc/tests/fixtures/` until v1 support is intentionally removed.
- The `tests/fixtures/v2/` directory remains a staging boundary; non-fixture files (such as the staging `README.md`) must not match the `*.v2.json` glob.

### Schema-version Field

- Each v2 fixture's wire payload must declare its protocol version as `2`. The file suffix (`*.v2.json`) and the payload version must agree.
- The validator rejects v2 fixtures that do not contain a `"version": 2` field in the JSON payload. Whitespace variation around the colon is tolerated.
- For envelopes with an `info` block (such as `protocol_info`), the version field appears as `info.version`. Other envelopes use the stable version key already defined by the response type.

### Migration-note Pairing

- When the first v2 fixture lands, [docs/protocol-migrations/v2.md](./protocol-migrations/README.md) must already exist with the format from the migration-notes README (compatibility window, breaking changes, affected IPC and HTTP surfaces, fixture files added, expected client behavior).
- Each `*.v2.json` filename must be referenced by name inside `docs/protocol-migrations/v2.md`. The validator rejects fixtures that are not bound to the migration note.
- The migration note must be added before or in the same commit as the first v2 fixture.

### Validator Behavior

- Dormant: when the `tests/fixtures/v2/` directory has no `*.v2.json` files, the validator prints a "dormant (no v2 fixtures present)" line and exits 0.
- Strict: when any `*.v2.json` file appears, the validator fails fast with a remediation message if the file layout, schema-version field, or migration-note pairing rule is violated.
- The validator does not write fixtures, modify migration notes, or change protocol constants.

## Query Parameters

Read-side HTTP routes accept optional query parameters that mirror the corresponding IPC request fields:

- `GET /peers/list?status=live` and `GET /peers/list?status=revoked` narrow the response to live or tombstoned peer entries respectively. Omitting `status` returns the full registry (live plus revoked, including the operator's own row). The query layer uses untyped strings and degrades a typo (or any unrecognised value) to "no filter" rather than rejecting the request, matching the rest of the read-side filter surface. `limit` and `prefix` compose conjunctively with `status`.
- `GET /a2a/queue` accepts `limit`, `min_lease_age_ms`, `deadline_within_ms`, and `state_filter=queued|in_flight`. Each filter is applied before the limit truncation so a noisy filtered-out cluster cannot push matching rows out of the result window. The JSON envelope echoes the active filters back to the caller so machine consumers can distinguish a state-only result from a pre-filter empty result.

## Human Authority

The decision to bump the IPC/HTTP protocol, the wire shapes that change, the migration window, and the public release notes for v2 remain human-owned. Automation may keep this contract documented and validated in dormant mode. It must not introduce v2 fixtures, edit `PROTOCOL_VERSION`, or relax the migration-note pairing without an approved decision.
