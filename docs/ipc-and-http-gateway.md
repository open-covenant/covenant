# IPC and HTTP Gateway

The Covenant daemon exposes two transport surfaces for the same protocol:

- IPC framed responses parsed by `covenant-ipc`.
- The HTTP gateway exposed by `covenantd` for `/version`, `/tools/call`, audit, peer, and A2A endpoints.

Protocol metadata is reported through `ProtocolInfo` over IPC, the HTTP `/version` route, and the `covenant version` CLI. Compatibility rules and the v1/v2 staging boundary live in [docs/protocol-versioning.md](./protocol-versioning.md). Migration notes live under [docs/protocol-migrations/](./protocol-migrations/README.md). The IPC fixture replay harness pins both transports together so they cannot drift independently.

The decision to bump the protocol to v2 and publish v2 fixtures is human-owned (ADR 0010, integrated 2026-05-18). This document defines the contract every v2 fixture must satisfy so subsequent additions and any future protocol bump are deterministic.

## v2 Fixture Contract

The contract is enforced by an internal validator. The validator runs in two modes against `agent-os/crates/covenant-ipc/tests/fixtures/v2/`: dormant when the directory has no `*.v2.json` files, strict when any appear. The rules below govern every v2 fixture and the validator emits a remediation pointer on the first rule that fails.

### File Layout

- v2 response fixtures live under `agent-os/crates/covenant-ipc/tests/fixtures/v2/`.
- Each fixture file is named `<envelope>.v2.json` where `<envelope>` is a kebab-case identifier for the envelope variant (e.g., `stream-envelope-begin.v2.json` pins `StreamEnvelope::StreamBegin`). Distinct case shapes of the same envelope may use a suffix (e.g., `stream-envelope-end-with-summary.v2.json` for the SubmitIntent rollup case).
- When an envelope exists in both v1 and v2 (a response variant whose wire shape gained an additive or alternative form), the `*.v2.json` file reuses the base name of its `*.v1.json` sibling so the version diff is obvious. Envelopes that are v2-only (the four `StreamEnvelope` variants today) have no `*.v1.json` sibling and the rule does not apply.
- v1 fixtures stay at the root of `agent-os/crates/covenant-ipc/tests/fixtures/` until v1 support is intentionally removed.
- The `tests/fixtures/v2/` directory remains a staging boundary; non-fixture files (such as the staging `README.md`) must not match the `*.v2.json` glob.

### Schema-version Field

- A v2 fixture whose envelope carries a stable version key (e.g., `protocol_info` exposes `info.version`) must declare that field as `2`. The file suffix (`*.v2.json`) and the payload version must agree where a version slot exists.
- The validator rejects such a fixture if its declared version field is not `2`. Whitespace variation around the colon is tolerated.
- Envelope-shape wire frames that have no natural version slot (e.g., `StreamEnvelope` variants such as `stream_begin`, `stream_chunk`, `stream_end`, `stream_error`) are exempt: their v2 binding is the staging directory location, the `*.v2.json` file suffix, and the migration-note pairing rule below. Adding a synthetic version field to those frames would diverge from the wire bytes they pin.

### Migration-note Pairing

- [docs/protocol-migrations/v2.md](./protocol-migrations/README.md) is the canonical migration record for the v2 promotion. It uses the format from the migration-notes README (compatibility window, breaking changes, affected IPC and HTTP surfaces, fixture files added, expected client behavior).
- Each `*.v2.json` filename must be referenced by name inside `docs/protocol-migrations/v2.md`. The validator rejects fixtures that are not bound to the migration note, both for the existing fixture set and for any new addition.
- A new v2 fixture and its migration-note bullet must land in the same commit.

### Validator Behavior

- Dormant: when the `tests/fixtures/v2/` directory has no `*.v2.json` files, the validator prints a "dormant (no v2 fixtures present)" line and exits 0.
- Strict: when any `*.v2.json` file appears, the validator fails fast with a remediation message if the file layout, schema-version field, or migration-note pairing rule is violated.
- The validator does not write fixtures, modify migration notes, or change protocol constants.

## Query Parameters

Read-side HTTP routes accept optional query parameters that mirror the corresponding IPC request fields:

- `GET /peers/list?status=live` and `GET /peers/list?status=revoked` narrow the response to live or tombstoned peer entries respectively. Omitting `status` returns the full registry (live plus revoked, including the operator's own row). The query layer uses untyped strings and degrades a typo (or any unrecognised value) to "no filter" rather than rejecting the request, matching the rest of the read-side filter surface. `limit` and `prefix` compose conjunctively with `status`. The `covenant peers list --json` CLI wraps the same response in a stable envelope with `kind: "peer_list"`, `filter_pubkey_prefix` (echoes the request value or `null`), `matched_count` (rows in `peers` — equals the exhaustive match count when `truncated` is `false`), `operator_pubkey_b58`, and `truncated` (set when the registry held more matches than `limit`).
- `GET /a2a/queue` accepts `limit`, `min_lease_age_ms`, `deadline_within_ms`, and `state_filter=queued|in_flight`. Each filter is applied before the limit truncation so a noisy filtered-out cluster cannot push matching rows out of the result window. The JSON envelope echoes the active filters back to the caller so machine consumers can distinguish a state-only result from a pre-filter empty result.
- `GET /audit/recent` accepts `limit` and `since_ms=<epoch_ms>`. `since_ms` drops audit events whose `timestamp_ms` is strictly less than the threshold and is applied before `limit` so a recent burst cannot push older-but-still-in-window events out of the truncation slice. The CLI flag `--since-ms <epoch_ms>` and IPC `Request::RecentAudit.since_ms` carry the same semantics; the JSON envelope echoes the active threshold back as `since_ms`.
- `GET /receipts/recent` accepts `limit` and `since_ms=<epoch_ms>`. `since_ms` drops settlement receipts whose `settled_at` is strictly less than the threshold, applied before `limit` so a recent burst cannot push older-but-still-in-window receipts out of the truncation slice. The CLI flag `--since-ms <epoch_ms>` and IPC `Request::RecentReceipts.since_ms` carry the same semantics; the `covenant receipts recent --json` envelope echoes the active threshold back as `since_ms`.

## Mutation Routes

Write-side HTTP routes forward to the same `Server::respond` handler as their IPC request, so the capability check is identical across both transports — neither can under-check the other.

- `POST /settlement/receipts/backfill` maps to IPC `Request::BackfillSettlementReceipts { dry_run, scope_pubkey }`. The JSON body carries the same two fields, both optional (a missing `dry_run` is `false`). `scope_pubkey` is reserved for a future delegated mode and is not yet supported: any request that sets it is rejected before the capability check, since the operation evaluates the authenticated operator's own grants. The handler gates on the `settlement.backfill.*` capability (apply vs dry-run, see [docs/capabilities.md](./capabilities.md)) before repairing legacy receipt rows. The `covenant settlement backfill-receipts --json` CLI wraps the result in a stable envelope `{"schema":"covenant.settlement.backfill.v1", ...}` carrying `row_count`, `rollback_path`, and `dry_run`.
- `POST /memory/records/backfill` maps to IPC `Request::BackfillMemoryRecords { dry_run, scope_pubkey }`. Same two-field optional body shape, same delegated-mode rejection on `scope_pubkey`. The handler gates on `memory.backfill.*` (apply vs dry-run, see [docs/capabilities.md](./capabilities.md)) and requires the operator identity before merging `metadata.receipt_id` onto legacy memory rows. Correlations are recomputed server-side from the operator's own memory and receipt rows via `covenant_memory::memory_receipt_backfill_correlations`; clients cannot supply correlations directly. The `covenant memory backfill-receipt-correlation --json` CLI wraps the result in a stable envelope `{"schema":"covenant.memory.backfill.v1", ...}` carrying `row_count`, `savepoint_name`, and `dry_run`.

## Chain Transaction Envelopes

The operator-keypair-signed chain verbs (`covenant chain register-agent`, `covenant chain stake`, `covenant chain buy-credits`) emit two stable JSON envelopes when invoked with `--json`. Both envelopes share a transport-level core and differ in `kind` plus a per-state field:

- `covenant.chain.tx.v1` — the RPC submitted the transaction and the cluster confirmed it within `--confirm-timeout-ms`. Carries `status: "confirmed"`.
- `covenant.chain.tx.timeout.v1` — the RPC submitted the transaction but the cluster did not confirm before `--confirm-timeout-ms`. Carries `status: "submitted-not-confirmed"` and `timeout_ms`. The signature may still confirm later; the envelope only reports the local timeout boundary.

The `kind`/`status` pairing is invariant: `"confirmed"` only appears under `covenant.chain.tx.v1`, `"submitted-not-confirmed"` only appears under `covenant.chain.tx.timeout.v1`, and `timeout_ms` only appears in the timeout variant. Consumers branching on one signal must branch on the other to avoid silently dropping the timeout fan-out.

Every envelope variant carries:

- `kind`: one of `covenant.chain.tx.v1` or `covenant.chain.tx.timeout.v1`.
- `verb`: one of `register-agent`, `stake`, `buy-credits`.
- `signature`: base58 transaction signature.
- `rpc_url`: the resolved RPC endpoint the transaction was submitted to.
- `cluster`: the named cluster (`devnet`, `testnet`, `mainnet-beta`, or a custom alias).
- `status`: see the kind/status pairing above.

Per-verb fields layer on top, asymmetrically:

- `verb: "register-agent"` — adds `agent_key` (base58 agent pubkey).
- `verb: "stake"` — adds `agent_key` (base58), `amount` (u64), `lock_until` (u64). Both values are echoed verbatim from the CLI arguments and serialize to the on-chain `stake` instruction.
- `verb: "buy-credits"` — adds `owner` (base58 COVNT owner pubkey), `amount_covnt` (u64). The value is echoed verbatim from `--amount-covnt` and serializes to the on-chain `buy_credits` instruction.

The verb-source-of-truth lives in the CLI emitters: `register_agent_confirmed_json` and `register_agent_timeout_json` at `agent-os/crates/covenant/src/main.rs:664` and `:681`, `stake_confirmed_json` and `stake_timeout_json` at `:849` and `:870`, `buy_credits_confirmed_json` and `buy_credits_timeout_json` at `:1131` and `:1150`. Six unit tests at `main.rs:8855`, `:8876`, `:9146`, `:9166`, `:9382`, `:9400` pin the kind strings, so a drift in either the docs or the emitters surfaces in review.

## CLI Read Envelopes

A separate family of `--json` envelopes covers read-side chain queries. These envelopes use unversioned `kind` strings (no `.v1` suffix) and predate the `covenant.<area>.<verb>.v<n>` schema convention; they are kept stable by unit-test shape invariants rather than the suffix.

`covenant chain status --json` emits:

- `kind`: literal string `"chain_status"`.
- `status`: a structured `covenant_ipc::ChainStatus` object with the following fields. The top-level object has exactly two keys (`kind` and `status`); the inner `status` is never a string blob.

The inner `ChainStatus` shape, defined at `agent-os/crates/covenant-ipc/src/lib.rs:42`:

- `chain` (string) — chain family identifier, currently `"solana"`.
- `cluster` (string) — named cluster (`devnet`, `testnet`, `mainnet-beta`, `localnet`, or a custom alias).
- `rpc_url` (string | null) — resolved RPC endpoint, null when not configured.
- `ws_url` (string | null) — resolved websocket endpoint, null when not configured.
- `program_id` (string | null) — base58 settlement program ID, null when not configured.
- `covnt_mint` (string | null) — base58 COVNT mint pubkey, null when not configured.
- `ready` (bool) — true when every required config field is present.
- `missing` (array of strings) — names of the absent config fields when `ready` is false; an empty array when `ready` is true.

The envelope source-of-truth lives at `chain_status_json` in `agent-os/crates/covenant/src/main.rs:4530`. Two unit tests at `main.rs:6892` (`chain_status_json_renders_stable_shape`) and `main.rs:6914` (`chain_status_json_pins_top_level_schema`) enforce the top-level key set verbatim; the second test's failure message names this document as the forcing function for docs/emitter drift.

## Human Authority

The decision to bump the IPC/HTTP protocol, the wire shapes that change, the migration window, and the public release notes for v2 remain human-owned. Automation keeps this contract documented and validated; with the v2 `StreamEnvelope` fixtures landed under ADR 0010, the validator now runs in strict mode rather than dormant. It must not introduce v2 fixtures, edit `PROTOCOL_VERSION`, or relax the migration-note pairing without an approved decision.
