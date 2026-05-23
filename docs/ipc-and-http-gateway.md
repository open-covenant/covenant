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

The verb-source-of-truth lives in the CLI emitters: `register_agent_confirmed_json` and `register_agent_timeout_json` at `agent-os/crates/covenant/src/main.rs:664` and `:681`, `stake_confirmed_json` and `stake_timeout_json` at `:849` and `:870`, `buy_credits_confirmed_json` and `buy_credits_timeout_json` at `:1131` and `:1150`. Six unit tests at `main.rs:9066`, `:9087`, `:9465`, `:9484`, `:9832`, `:9849` pin the kind strings, and six sibling `*_pins_top_level_schema` tests at `main.rs:9105`, `:9155`, `:9503`, `:9565`, `:9866`, `:9922` assert the full documented top-level key set so an undocumented field added to any helper fails review.

## CLI Read Envelopes

A separate family of `--json` envelopes covers read-side chain queries and most other CLI surfaces. The section is **structurally mixed**: two discriminator subfamilies coexist.

- **Unversioned `kind` subfamily.** The older shape — every envelope below carries a top-level `kind` string (e.g., `"chain_status"`, `"peer_list"`) with no `.v1` suffix. This subfamily predates the schema-suffix convention and is kept stable by unit-test shape invariants — every entry has a `*_pins_top_level_schema` test plus a `*_renders_stable_shape` test that forces docs/emitter drift to surface in review.
- **Versioned `covenant.<area>.<verb>.v<n>` schema subfamily.** The newer shape — these envelopes carry a top-level `schema` string (e.g., `"covenant.settlement.backfill.v1"`, `"covenant.memory.backfill.v1"`) with a `.v<n>` version slot, and they do **not** carry a `kind` field. A future `.v2` envelope is a separate shape, not a field rename inside the existing `.v1` envelope. Every envelope in this subfamily is now anchored by a `*_pins_top_level_schema` test in the same style as the kind-subfamily envelopes; a refactor that drops a key or renames the schema literal will fail the test rather than silently drift the wire shape.

The two subfamilies are **mutually exclusive** at the top level: a `kind`-subfamily envelope never carries `schema`, and a `schema`-subfamily envelope never carries `kind`. Consumers must inspect which discriminator key is present before routing — a defensive parser that reads only one will misclassify envelopes from the other subfamily. The blocks below note which discriminator each envelope uses in the per-envelope shape table.

In addition to the per-envelope `*_pins_top_level_schema` unit tests, the docs/emitter symmetry across every envelope literal in this section is enforced by `agent-os/scripts/validate-cli-envelope-docs.mjs`. The validator fails if any listed envelope kind or schema literal appears in only one of the two surfaces (this document vs. the CLI emitter at `agent-os/crates/covenant/src/main.rs`); the kinds-array comment in the validator documents the maintenance contract.

`covenant chain status --json` emits:

- `kind`: literal string `"chain_status"`.
- `status`: a structured `covenant_ipc::ChainStatus` object with the following fields. The top-level object has exactly two keys (`kind` and `status`); the inner `status` is pinned by the schema test at `main.rs:7154-7157` to be a JSON object, never a string blob.

The inner `ChainStatus` shape, defined at `agent-os/crates/covenant-ipc/src/lib.rs:42`:

- `chain` (string) — chain family identifier, currently `"solana"`.
- `cluster` (string) — named cluster (`devnet`, `testnet`, `mainnet-beta`, `localnet`, or a custom alias).
- `rpc_url` (string | null) — resolved RPC endpoint, null when not configured.
- `ws_url` (string | null) — resolved websocket endpoint, null when not configured.
- `program_id` (string | null) — base58 settlement program ID, null when not configured.
- `covnt_mint` (string | null) — base58 COVNT mint pubkey, null when not configured.
- `ready` (bool) — true when every required config field is present.
- `missing` (array of strings) — names of the absent config fields when `ready` is false; an empty array when `ready` is true.

The envelope source-of-truth lives at `chain_status_json` in `agent-os/crates/covenant/src/main.rs:4523`. Two unit tests at `main.rs:7115` (`chain_status_json_renders_stable_shape`) and `main.rs:7137` (`chain_status_json_pins_top_level_schema`) enforce the top-level key set verbatim; the second test's failure message names this document as the forcing function for docs/emitter drift.

`covenant verify --json` emits a cross-check report comparing the audit log against memory and receipt rows. Envelope shape:

- `kind`: literal string `"verify_report"`.
- `window` (u64): the audit-window record count echoed back from the `--window` argument. Pinned as u64 by `main.rs:7226-7229` — never a string.
- `checks` (array of `VerifyCheck`): per-check results, see below. Pinned as an array by `main.rs:7234-7237` — never null or a string.
- `drift` (array of `VerifyDrift`): correlation gaps, see below.
- `orphans_total` (u64): total number of unmatched rows the checks discovered. Pinned as u64 by `main.rs:7230-7233` — never a string-of-integer.

Top-level keys are pinned to exactly these five by the test at `agent-os/crates/covenant/src/main.rs:7209` (`verify_report_json_pins_top_level_schema`).

`VerifyCheck` shape, defined at `agent-os/crates/covenant-ipc/src/lib.rs:26`:

- `name` (string) — human-readable check name (e.g., `"memory audit"`).
- `passed` (bool) — whether the check passed.
- `message` (string) — diagnostic message (empty when the check passed cleanly).

`VerifyDrift` shape, defined at `agent-os/crates/covenant-ipc/src/lib.rs:33`:

- `kind` (string) — drift category (e.g., `"memory_without_audit"`).
- `id` (string, omitted when null) — record identifier when the drift entry binds to a specific row. Serialized via `#[serde(default, skip_serializing_if = "Option::is_none")]` at `covenant-ipc/src/lib.rs:35-36`, so absent rather than `null` when unbound.
- `message` (string) — drift description.
- `repair` (string) — operator-facing remediation hint.

The envelope source-of-truth lives at `verify_report_json` in `agent-os/crates/covenant/src/main.rs:4530`. The shape-pinning test at `main.rs:7209-7255` covers both the populated and empty cases (`assert_shape` runs against a one-check, one-drift report and an all-empty report).

`covenant tools list --json` emits the registered MCP-style tool catalog. Envelope shape:

- `kind`: literal string `"tool_list"` (singular `tool_list`, not `tools_list`; consumers routing on `kind` must match the literal exactly).
- `tools` (array of `ToolSpec`): the registered tools the daemon advertises via `tools/list`. The array is empty when no tools are registered; the unsuffixed CLI prints `(no tools registered)` for that case at `main.rs:3123`. Pinned as an array by `main.rs:6972-6975` — never null or a string blob.

The inner `ToolSpec` shape, defined at `agent-os/crates/covenant-mcp/src/lib.rs:27`:

- `name` (string) — tool identifier.
- `description` (string) — human-readable tool summary.
- `inputSchema` (object) — JSON Schema for the tool's `arguments` object; an empty object means the tool takes no arguments.

`ToolSpec` carries `#[serde(rename_all = "camelCase")]` (`covenant-mcp/src/lib.rs:26`) so the Rust field `input_schema` serializes on the wire as `inputSchema`. The naming matches the MCP wire format; JSON consumers must deserialize using `inputSchema`, not `input_schema`.

Top-level keys are pinned to exactly these two by the test at `agent-os/crates/covenant/src/main.rs:6955` (`tool_list_json_pins_top_level_schema`), which exercises both a populated single-tool case and an empty list.

The envelope source-of-truth lives at `tool_list_json` in `agent-os/crates/covenant/src/main.rs:4495`. Two unit tests at `main.rs:6931` (`tool_list_json_renders_stable_shape`) and `main.rs:6955` cover both cases. The CLI verb is wired at `main.rs:3107-3133`; without `--json`, the same response prints one line per tool in the form `<name> — <description>` at `main.rs:3126`.

`covenant tools call <name> [--args <json>] --json` emits the tool invocation result. Envelope shape:

- `kind`: literal string `"tool_result"` (singular, not `tools_result`; consumers routing on `kind` must match the literal exactly).
- `name` (string): the tool name echoed back from the CLI argument.
- `content` (array of `Content`): the tool's output blocks. Each element is a tagged-enum object whose `type` discriminator selects the variant — `{type: "text", text: <string>}` for textual output or `{type: "json", value: <JSON>}` for structured output. The variants are defined at `agent-os/crates/covenant-mcp/src/lib.rs:39` with `#[serde(tag = "type", rename_all = "camelCase")]`; v0 ships text and json variants only. The array is empty when the tool produced no output blocks; the unsuffixed CLI prints each block sequentially at `main.rs:3174-3180`.
- `is_error` (boolean): `true` when the tool itself raised; pinned as a JSON boolean by the schema test (`main.rs:7037-7040`) — never `0`/`1` or a string. JSON consumers must branch on this boolean, not on the presence/absence of content. `is_error=true` paired with non-empty `content` describes a partial-success outcome with an error indicator.

Top-level keys are pinned to exactly these four by the test at `agent-os/crates/covenant/src/main.rs:7015` (`tool_result_json_pins_top_level_schema`), exercised against both a non-empty content + is_error=true case and an empty content + is_error=false case.

The envelope source-of-truth lives at `tool_result_json` in `agent-os/crates/covenant/src/main.rs:4502`. Two unit tests at `main.rs:6994` (`tool_result_json_renders_stable_shape`) and `main.rs:7015` cover the shape. The CLI verb is wired at `main.rs:3134-3180`; without `--json`, each `Content::Text` block prints its `text` directly and each `Content::Json` block prints its `value` as pretty-printed JSON.

`covenant chain flush-receipts --json` emits a receipt-batch summary when it groups local settlement receipts into a single Solana receipt-root transaction. Envelope shape:

- `kind`: literal string `"receipt_batch_flushed"`.
- `limit` (u64): the batch-size cap echoed back from the `--limit` argument. Pinned as u64 by `main.rs:7294-7297` — never a string.
- `receipts_updated` (u64): the number of local receipt rows updated to point at the new batch. Pinned as u64 by `main.rs:7298-7301` — never a string-of-integer.
- `batch` (`ReceiptBatchSummary` object): the batch's wire shape, see below. Pinned as a structured object by `main.rs:7302-7305` — never a string blob.

Top-level keys are pinned to exactly these four by the test at `agent-os/crates/covenant/src/main.rs:7277` (`flush_receipts_json_pins_top_level_schema`).

`ReceiptBatchSummary` shape, defined at `agent-os/crates/covenant-ipc/src/lib.rs:54`:

- `batch_id` (string) — opaque batch identifier.
- `merkle_root` (string, 64 hex characters) — Merkle root over the included receipts.
- `receipt_count` (u32) — number of receipts in the batch (note u32, not u64).
- `tx_sig` (string or null) — base58 Solana transaction signature once the batch confirms; null before submission completes.
- `slot` (u64 or null) — confirmation slot once available; null until then.

The envelope source-of-truth lives at `flush_receipts_json` in `agent-os/crates/covenant/src/main.rs:4545`. Two unit tests at `main.rs:7258` (`flush_receipts_json_renders_stable_shape`) and `main.rs:7277` (`flush_receipts_json_pins_top_level_schema`) cover both the unconfirmed (`tx_sig`/`slot` null) and confirmed (both present) batch states.

`covenant chain receipt-batches --json` emits the list of recent receipt batches recorded on-chain. Envelope shape:

- `kind`: literal string `"receipt_batch_list"`.
- `limit` (u64): the result cap echoed back from the `--limit` argument. Pinned as u64 by `main.rs:7092-7095` — never a string.
- `batches` (array of `ReceiptBatchSummary`): the batches, in the order returned by the daemon. Each item uses the same `ReceiptBatchSummary` shape documented above (including the `tx_sig`/`slot` null convention for batches whose settlement transaction has not yet confirmed). The array may be empty. Pinned as an array by `main.rs:7096-7099` — never null or a string.

Top-level keys are pinned to exactly these three by the test at `agent-os/crates/covenant/src/main.rs:7075` (`receipt_batch_list_json_pins_top_level_schema`).

The envelope source-of-truth lives at `receipt_batch_list_json` in `agent-os/crates/covenant/src/main.rs:4515`. Two unit tests at `main.rs:7057` (`receipt_batch_list_json_renders_stable_shape`) and `main.rs:7075` (`receipt_batch_list_json_pins_top_level_schema`) cover the populated and empty cases.

`covenant receipts recent [-n|--limit <N>] [--since-ms <M>] --json` emits a window of local settlement receipts. Envelope shape:

- `kind`: literal string `"receipt_list"` — verb-name asymmetry: the CLI verb is `recent` but the envelope discriminator is `receipt_list` (singular `receipt_`, not `receipts_`); consumers routing on `kind` must match the literal exactly rather than reusing the verb token or pluralising.
- `limit` (u64): the request limit echoed back from `-n`/`--limit` (default `10`, per `main.rs:2830`). Pinned at the type level by the schema test (`main.rs:5490-5493`) — never a string.
- `since_ms` (u64 or null): the Unix-epoch millisecond threshold echoed from `--since-ms`, or `null` when the flag was omitted. Pinned as u64-or-null at the schema test (`main.rs:5494-5497`) — never a string-of-integer. Filter semantics live with the daemon's `Request::RecentReceipts` handler; this surface only echoes the operator's input.
- `receipts` (array of `SettlementReceipt`): the matched receipts in the order returned by the daemon. The array is empty when no receipts fall in the window; the unsuffixed CLI prints `(no receipts)` for that case at `main.rs:2860`.

The inner `SettlementReceipt` shape, defined at `agent-os/crates/covenant-types/src/lib.rs:339`:

- `id` (string) — receipt UUID, serialized as the canonical hyphenated string form.
- `payer` (object) — `{display: string, pubkey: string (base58)}` per the `AgentId` Serialize impl at `covenant-types/src/lib.rs:124`.
- `resource` (string) — `ResourceKind` slug, exactly one of `"compute"`, `"memory"`, `"tool"`, `"message"`, `"registration"` (lowercase per `#[serde(rename_all = "lowercase")]` at `covenant-types/src/lib.rs:35`). Consumers must route on the lowercase wire form, **not** the Rust enum names (`"Compute"`, `"Memory"`, etc.) — those never appear on the wire.
- `memory_record_id` (string, omitted when null) — record identifier when the receipt settled a memory write. Serialized via `#[serde(default, skip_serializing_if = "Option::is_none")]` at `covenant-types/src/lib.rs:343-344` — so **absent rather than null** when unbound. This is the single asymmetry among the Option fields: every other optional field below carries `#[serde(default)]` without `skip_serializing_if`, so those keys are **always emitted** (as `null` when absent). JSON consumers must check `memory_record_id` with key-existence, not null-vs-value.
- `credits_consumed` (u64) — USD-pegged credits destroyed at this event.
- `settled_at` (u64) — Unix-epoch milliseconds when the receipt was issued locally.
- `chain` (string or null) — chain family identifier (e.g. `"solana"`) once the receipt has been batched and confirmed on-chain; `null` until then. Always present on the wire.
- `cluster` (string or null) — named cluster (e.g. `"devnet"`); `null` until on-chain confirmation. Always present on the wire.
- `batch_id` (string or null) — opaque receipt-batch identifier once the receipt is included in a batch; `null` until then. Always present on the wire.
- `merkle_root` (string or null) — 64-hex Merkle root of the batch the receipt was included in; `null` until then. Always present on the wire.
- `tx_sig` (string or null) — base58 Solana transaction signature once the batch confirms; `null` until then. Always present on the wire.
- `slot` (u64 or null) — confirmation slot once available; `null` until then. Always present on the wire.
- `confirmed_at` (u64 or null) — Unix-epoch milliseconds when the on-chain transaction confirmed; `null` until then. Always present on the wire.
- `onchain_sig` (string or null) — backwards-compatible alias for `tx_sig` (per the struct doc-comment at `covenant-types/src/lib.rs:335-337`) that older clients still consume; new consumers should prefer `tx_sig`. Always present on the wire. Both fields carry the same value once the receipt confirms; the unsuffixed CLI's `(local-only)` fallback at `main.rs:2864-2867` reads `tx_sig` first and falls back to `onchain_sig` for exactly that reason.

Top-level keys are pinned to exactly these four by the test at `agent-os/crates/covenant/src/main.rs:5473` (`receipt_list_json_pins_top_level_schema`), exercised against three cases: populated with `since_ms`, populated without `since_ms`, and empty without `since_ms`.

The envelope source-of-truth lives at `receipt_list_json` in `agent-os/crates/covenant/src/main.rs:4307`. Two unit tests at `main.rs:5432` (`receipt_list_json_renders_stable_shape`) and `main.rs:5473` cover the shape. The CLI verb is wired at `main.rs:2825-2878`; without `--json`, each receipt is printed as `[<settled_at>] <resource>: <credits> credits — <onchain>` at `main.rs:2868-2871`, with `<onchain>` resolving to the `tx_sig`/`onchain_sig` value or the literal `(local-only)` when both are null.

`covenant ping --json` emits a daemon-liveness probe. Envelope shape:

- `kind`: literal string `"daemon_ping"`.
- `status`: literal string `"ok"` — the daemon only returns this envelope when it has accepted the request and produced a `Response::Pong`; failures surface as a non-zero CLI exit rather than a non-`"ok"` payload, so consumers can branch on transport success alone.

Top-level keys are pinned to exactly these two by the test at `agent-os/crates/covenant/src/main.rs:5839` (`ping_json_pins_top_level_schema`).

The envelope source-of-truth lives at `ping_json` in `agent-os/crates/covenant/src/main.rs:4337`. The shape-pinning tests at `main.rs:5832` (`ping_json_renders_stable_shape`) and `main.rs:5839` cover the single emitted shape; the CLI verb is wired at `main.rs:1977-1999` (the unsuffixed `covenant ping` prints `pong` instead).

`covenant intent [--json] [--stream] <text>` emits the dispatched intent's outcome with optional settlement evidence. Envelope shape:

- `kind`: literal string `"intent_result"`.
- `intent_id` (string): the dispatched intent's UUID, serialized as the canonical hyphenated string form. Pinned as a string by the schema test (`main.rs:5785-5788`) — never a byte array or struct.
- `status` (string): the outcome status (e.g., `"ok"`). The string shape is pinned by `main.rs:5789-5792`; specific value enumeration lives with the daemon's intent dispatcher rather than this docs surface.
- `text` (string): the result text the daemon returned. The unsuffixed CLI prints this value directly at `main.rs:2069` (a single-line `println!("{text}")`), so `covenant intent --json` and `covenant intent` share the result payload but only `--json` wraps it in the envelope.
- `sources` (array of strings): source labels that contributed to the result (e.g., `["research"]`). Pinned as an array of strings by `main.rs:5794-5797` — never a comma-joined string. Empty when no sources are attached.
- `settlement` (object or null): an optional `SettlementReceipt` (defined at `agent-os/crates/covenant-types/src/lib.rs:339`) carrying the on-chain or local settlement evidence when the intent consumed credits. `null` when the intent did not settle (e.g., a phase-0 echo that does not charge). Pinned as object-or-null by `main.rs:5798-5801` — never an integer or array.

Top-level keys are pinned to exactly these six by the test at `agent-os/crates/covenant/src/main.rs:5761` (`intent_result_json_pins_top_level_schema`), exercised against both a populated `Some(SettlementReceipt)` case and an empty unsettled case.

The envelope source-of-truth lives at `intent_result_json` in `agent-os/crates/covenant/src/main.rs:4320`. Two unit tests at `main.rs:5743` (`intent_result_json_renders_stable_shape`) and `main.rs:5761` cover the shape. The CLI verb is wired at `main.rs:2000-2074`; the `--json`/`--stream` flags are recognized only in leading position (`main.rs:2013-2022`) so an interior `--json` token is preserved as part of the intent text. The optional `--stream` flag sets `Request::SubmitIntent.prefer_stream = Some(true)` (`main.rs:2033`), enabling the v2 streaming-response path documented under [docs/protocol-versioning.md](./protocol-versioning.md); the terminal `IntentResult` envelope shape is unchanged when the streaming path is not selected.

`covenant capabilities recent [-n|--limit <N>] --json` emits a peer-scoped view of recent signed capabilities. Envelope shape:

- `kind`: literal string `"capability_list"` — verb-name asymmetry: the CLI verb is `recent` but the envelope discriminator is `capability_list`. Consumers routing on `kind` must match the latter literal exactly rather than reusing the verb token.
- `limit` (u64): the request limit echoed back from `-n`/`--limit` (default `10`, see `main.rs:2569`). Pinned at the type level by the schema test (`main.rs:5919-5922`) — JSON consumers must never receive a string here.
- `capabilities` (array of `SignedCapability`): the filtered live capabilities. Each element has shape `{capability: Capability, signature: <base58>}` where `Capability` is defined at `agent-os/crates/covenant-types/src/lib.rs:171` (fields: `subject`, `action`, `scope`, `granted_by`, `expires_at`) and `SignedCapability` is defined at `agent-os/crates/covenant-permissions/src/lib.rs:58`. The `signature` field is the base58 encoding of the 64-byte ed25519 signature (per the `sig_b58` serde module at `lib.rs:64-84`), never the raw byte array.

The daemon applies a **peer-visibility filter** before returning the list (see `recent_capabilities` at `agent-os/crates/covenantd/src/lib.rs:5834-5850`): only capabilities whose `subject.pubkey` or `granted_by.pubkey` matches the requesting peer's pubkey are included. JSON consumers must not assume this is a global registry dump — operator and delegated callers see a different slice of the same store.

Top-level keys are pinned to exactly these three by the test at `agent-os/crates/covenant/src/main.rs:5902` (`capability_list_json_pins_top_level_schema`), which exercises both a populated single-capability case and an empty list.

The envelope source-of-truth lives at `capability_list_json` in `agent-os/crates/covenant/src/main.rs:4344`. Two unit tests at `main.rs:5862` (`capability_list_json_renders_stable_shape`) and `main.rs:5902` cover both cases. The CLI verb is wired at `main.rs:2568-2624`; without `--json`, the same response prints one line per capability in the form `<subject_display> → <action_label> (<granted_by_display>) [<expiry>]` at `main.rs:2612-2618`, or `(no capabilities granted)` when the filtered list is empty.

`covenant capabilities grant <action> [--scope <json>] [--expires-at <ms>] --json` emits the freshly-signed capability after the daemon accepts the grant. Envelope shape:

- `kind`: literal string `"capability_granted"` — past-tense outcome name, distinct from the verb name `grant`; consumers routing on `kind` must match the literal exactly rather than reusing the verb token.
- `subject_display` (string): the daemon-synthesized human-readable subject (e.g., `operator@local`). The daemon owns this field — consumers must not reconstruct it from the request. Pinned as a string by `main.rs:5992-5995` — never an object or array.
- `action` (string): the action the capability was granted for. **Not always the verbatim CLI argument**: when the CLI receives an a2a peer-prefix shorthand it expands the prefix to the full peer-bound action before signing (see `expand_a2a_action` invoked at `main.rs:2657-2690`); the envelope reports the post-expansion full form, and the unsuffixed CLI prints an `expanding <prefix> → <full>` line to stderr at `main.rs:2680`. Pinned as a string by `main.rs:5996-5999` — never an object or array.
- `signature_b58` (string): the base58 signature over the signed-capability bytes. This is the same value consumers pass back to `covenant capabilities revoke <signature-b58>` to tombstone the capability. Pinned as a string by `main.rs:6000-6003` — never an object or array.
- `scope` (object or null): the structured scope object echoed from the request, or `null` when `--scope` was omitted. Pinned at the type level by the schema test (`main.rs:6004-6007`) — JSON consumers must never receive a string blob here, so a scope value of `"{\"version\":1}"` would be a contract break.
- `expires_at` (u64 or null): the Unix-epoch millisecond expiry echoed from `--expires-at`, or `null` when the flag was omitted. Pinned at the type level by the schema test (`main.rs:6008-6011`) — JSON consumers must never receive a string here, so a value of `"1700000000000"` would be a contract break.

Top-level keys are pinned to exactly these six by the test at `agent-os/crates/covenant/src/main.rs:5968` (`capability_grant_json_pins_top_level_schema`), which also asserts the `scope` object-or-null and `expires_at` u64-or-null typing.

The envelope source-of-truth lives at `capability_grant_json` in `agent-os/crates/covenant/src/main.rs:4352`. Two unit tests at `main.rs:5945` (`capability_grant_json_renders_stable_shape`, covers both a scoped+timed grant and an unscoped+untimed grant) and `main.rs:5968` cover both populated cases. The CLI verb is wired at `main.rs:2626-2718`; without `--json`, the same response prints `granted: <subject> → <action>` followed by the signature on a second line.

`covenant capabilities revoke <signature-b58> --json` emits the outcome of revoking a single signed capability by its signature. Envelope shape:

- `kind`: literal string `"capability_revoked"` — past-tense outcome name, distinct from the verb name `revoke`; consumers routing on `kind` must match the literal exactly rather than reusing the verb token.
- `signature_b58` (string): the base58 signature echoed back from the request, so consumers can correlate the response to the revoke call without tracking it out of band. Pinned as a string by `main.rs:6062-6065` — never an object or array.
- `removed` (boolean): `true` if a live capability matched and was tombstoned, `false` if no live row matched that signature. Pinned as a JSON boolean by `main.rs:6066-6069` — never `0`/`1` or a string. `false` is a benign no-op outcome, not an error — the daemon still returns `Response::CapabilityRevoked` and the unsuffixed CLI prints `(no live capability with that signature)` for that case at `main.rs:2769`. JSON consumers must not treat `removed=false` as a failure.

Top-level keys are pinned to exactly these three by the test at `agent-os/crates/covenant/src/main.rs:6045` (`capability_revoke_json_pins_top_level_schema`), which also asserts `removed` is a JSON boolean (never `0`/`1` or a string).

The envelope source-of-truth lives at `capability_revoke_json` in `agent-os/crates/covenant/src/main.rs:4369`. Two unit tests at `main.rs:6032` (`capability_revoke_json_renders_stable_shape`) and `main.rs:6045` cover both the `removed=true` and `removed=false` cases. The CLI verb is wired at `main.rs:2731-2774`.

`covenant capabilities purge --json` emits a summary of revoked-capability garbage collection. Envelope shape:

- `kind`: literal string `"capabilities_purged"`.
- `before_ms` (u64): the resolved Unix-epoch millisecond cutoff. The CLI accepts either `--before-ms <M>` (echoed verbatim) or `--older-than-ms <D>` (resolved against the system clock as `now - D` per `main.rs:2795-2799`); the envelope always reports the single resolved value, so consumers cannot distinguish which input form the operator typed. Pinned as u64 by `main.rs:6102-6105` — never a string-of-integer.
- `purged` (u64): the count of revoked-capability rows removed. May legitimately be `0` when no rows matched the cutoff — the verb does not error on an empty purge. Pinned as u64 by `main.rs:6106-6109` — never a string-of-integer.

Top-level keys are pinned to exactly these three by the test at `agent-os/crates/covenant/src/main.rs:6085` (`capabilities_purge_json_pins_top_level_schema`).

The envelope source-of-truth lives at `capabilities_purge_json` in `agent-os/crates/covenant/src/main.rs:4377`. Two unit tests at `main.rs:6077` (`capabilities_purge_json_renders_stable_shape`) and `main.rs:6085` (`capabilities_purge_json_pins_top_level_schema`) cover the populated (`purged=3`) and empty (`purged=0`) cases. The CLI verb is wired at `main.rs:2776-2824`; without `--json`, the same response prints `purged <n> revoked capability(ies)`.

`covenant peers list [--limit <N>] [--prefix <P>] [--live-only|--revoked-only] --json` emits the registered peer roster filtered by the supplied flags. Envelope shape:

- `kind`: literal string `"peer_list"`.
- `limit` (u64): the request limit echoed back from `--limit` (default `20`, per `main.rs:3708`).
- `filter_pubkey_prefix` (string or null): the prefix echoed from `--prefix`, or `null` when the flag was omitted. Pinned at the type level by the schema test (`main.rs:5052-5056`) — never an integer or array.
- `matched_count` (u64): row count of the `peers` array; equals the exhaustive match count when `truncated` is `false`. Pinned as u64 by `main.rs:5057-5060` — never a string.
- `peers` (array of `PeerSummary`): the matched roster slice, see below.
- `operator_pubkey_b58` (string): the requesting operator's own pubkey in base58. The unsuffixed CLI line formatter at `peer_list_lines` (`main.rs:4238`) compares each peer's `pubkey_base58()` against this value to append a ` (self)` marker on the operator's own row; JSON consumers must apply the same comparison to render the self-tag, not assume the operator's row is reliably first.
- `truncated` (boolean): `true` when the registry held more matching entries than `limit`, `false` otherwise. Pinned as a JSON boolean by the schema test at `main.rs:5066-5069` — never `0`/`1`. **This is the only signal of incomplete results**; `matched_count == limit` with `truncated == false` means the page is the exhaustive match set, not a hint to paginate.

The inner `PeerSummary` shape, defined at `agent-os/crates/covenant-peer-auth/src/lib.rs:140`:

- `agent_id` (object) — `{display: string, pubkey: string (base58)}` per the `AgentId` Serialize impl at `covenant-types/src/lib.rs:124`.
- `token_prefix` (string) — 6-character redacted token prefix, the same value `peers revoke <token-prefix>` accepts. The full token bytes are never on the wire — same invariant as `Response::PeerList`.
- `registered_at` (u64) — Unix-epoch milliseconds when the peer registered.
- `revoked_at` (u64 or null) — Unix-epoch milliseconds when the peer was tombstoned; `null` for live entries. Composes with the `--live-only`/`--revoked-only` flags (and the equivalent `status_filter` query parameter described above) for filtering — the filter runs before the registry's truncation peek.

Top-level keys are pinned to exactly these seven by the test at `agent-os/crates/covenant/src/main.rs:5023` (`peer_list_json_pins_top_level_schema`), exercised against a populated two-peer (one live, one revoked) case and an empty case.

The envelope source-of-truth lives at `peer_list_json` in `agent-os/crates/covenant/src/main.rs:4213`. Schema and behavioral tests live at `main.rs:5023` (key set + per-key typing), `main.rs:4990` (`peer_list_json_echoes_prefix_and_match_count`), `main.rs:5004` (`peer_list_json_omits_prefix_when_inactive`), and `main.rs:5015` (`peer_list_json_reports_zero_match_count_for_empty_response`). The CLI verb is wired at `main.rs:3707-3760`; without `--json`, the same response is rendered line-by-line by `peer_list_lines` (`main.rs:4238`) with a `(truncated; <n> shown — narrow with --prefix or raise --limit)` hint appended when `truncated` is `true` (`main.rs:4269`). See also the **Query Parameters** section above for the same filter composition rules over the HTTP gateway.

`covenant peers purge --json` emits a summary of revoked-peer garbage collection. Envelope shape:

- `kind`: literal string `"peers_purged"` — the only structural disambiguator from `capabilities_purged`; both envelopes share the same three-key layout, so consumers that route on `kind` must check the full literal rather than treating any `*_purged` envelope as interchangeable.
- `before_ms` (u64): resolved Unix-epoch millisecond cutoff. The CLI accepts `--before-ms` or `--older-than-ms` with the same resolution semantics as `covenant capabilities purge --json` above. Pinned as u64 by `main.rs:6142-6145` — never a string-of-integer.
- `purged` (u64): count of revoked-peer rows removed. Only revoked rows are eligible — the verb does not touch live peers (the unsuffixed CLI prints `purged <n> revoked peer(s)` at `main.rs:3657`). May legitimately be `0` when no rows matched. Pinned as u64 by `main.rs:6146-6149` — never a string-of-integer.

Top-level keys are pinned to exactly these three by the test at `agent-os/crates/covenant/src/main.rs:6125` (`peers_purge_json_pins_top_level_schema`).

The envelope source-of-truth lives at `peers_purge_json` in `agent-os/crates/covenant/src/main.rs:4385`. Two unit tests at `main.rs:6117` (`peers_purge_json_renders_stable_shape`) and `main.rs:6125` cover the populated and empty cases. The CLI verb is wired at `main.rs:3617-3663`.

`covenant peers rotate --json` emits the new operator token after rotation. Envelope shape:

- `kind`: literal string `"peer_token_rotated"`.
- `token_b58` (string): the full base58 operator token. The value is the new authentication credential, not a fingerprint — the envelope is **secret-bearing** and JSON output must be treated as sensitive (no logging, no shell history capture, no transport over unsecured channels).

Top-level keys are pinned to exactly these two by the test at `agent-os/crates/covenant/src/main.rs:6164` (`peers_rotate_json_pins_top_level_schema`).

Side effects before the envelope returns (per the CLI comment at `main.rs:3677-3683`): the daemon has already persisted the new token to `$COVENANT_HOME/peers/operator.token` (mode `0600`), so the envelope is informational. Existing shells holding the previous token continue to authenticate with the old value until they re-read the file; consumers that cache the token in memory must refresh after rotation.

The envelope source-of-truth lives at `peers_rotate_json` in `agent-os/crates/covenant/src/main.rs:4393`. The shape-pinning tests at `main.rs:6157` (`peers_rotate_json_renders_stable_shape`) and `main.rs:6164` (`peers_rotate_json_pins_top_level_schema`) cover both a typical-token case and an empty-string defensive case (the latter exercises the key-set invariant rather than a legitimate runtime value). The CLI verb is wired at `main.rs:3664-3699`; without `--json`, the same response prints a two-line message terminating in the raw token value.

`covenant peers revoke <token-prefix> [--force] [--limit-matches <N>] --json` emits the outcome of revoking a single peer by its base58 token prefix. Envelope shape:

- `kind`: literal string `"peer_revoke"` — verb-form, not past-tense. Distinct from the sibling envelopes whose outcome names took the past-tense form (`capability_revoked`, `peer_token_rotated`, `peers_purged`); consumers routing on `kind` must match the literal exactly rather than guessing `peer_revoked` or `peers_revoke`.
- `outcome` (object): a tagged-enum `RevokeOutcome` (defined at `agent-os/crates/covenant-peer-auth/src/lib.rs:182` with `#[serde(tag = "type", rename_all = "snake_case")]`). The top-level object has exactly two keys (`kind` and `outcome`); the inner `outcome` is pinned by the schema test at `main.rs:5165-5168` to be a JSON object, never a string blob.

The five `RevokeOutcome` variants the daemon may return:

- `{type: "revoked", agent_id, token_prefix, registered_at, revoked_at}` — the unique live match was tombstoned. The four extra fields are the inlined `PeerSummary` shape documented in the `peer_list` block above; `revoked_at` carries the moment of revocation and is non-null for this variant.
- `{type: "already_revoked", agent_id, token_prefix, registered_at, revoked_at}` — same inlined `PeerSummary` shape; the unique match was already tombstoned. Idempotent — the operator's intent is satisfied — and `revoked_at` carries the *original* revocation timestamp, not the moment of this call.
- `{type: "not_found"}` — no entry's full base58 token matched the supplied prefix. No extra fields.
- `{type: "ambiguous", matches: [PeerSummary...], truncated: bool}` — more than one entry matched the prefix; the registry is unchanged. `matches.len()` is bounded by `--limit-matches`; `truncated` is `true` when more than that limit matched (see `RevokeOutcome::Ambiguous` at `covenant-peer-auth/src/lib.rs:207-211`). The field carries `#[serde(default)]` so a stale CLI built before `truncated` landed still deserialises a new daemon's response (degrading to the pre-bound assumption that the displayed matches are exhaustive); the daemon-side serializer always writes the field.
- `{type: "self_revoke_forbidden", agent_id, token_prefix, registered_at, revoked_at}` — same inlined `PeerSummary` shape; the unique live match is the operator's own bootstrap row and the request did not pass `--force`. The registry is unchanged and `revoked_at` is `null` (the entry remained live). This is defence-in-depth against the "fat-finger via web UI bypassed by curl" failure mode where a UI-only confirmation guard is trivially circumvented by a direct daemon API call.

**Exit-code coupling**: the `peer_revoke_is_failure` classifier at `agent-os/crates/covenant/src/main.rs:4682-4689` maps `not_found`, `ambiguous`, and `self_revoke_forbidden` to a CLI exit code of `1` — including in the `--json` path (`main.rs:3807-3809`). `revoked` and `already_revoked` map to exit `0`. JSON consumers must branch on `outcome.type` for success/failure semantics; transport success (exit `0`) is **not** synonymous with revocation success. The classifier's mapping is pinned by the test at `main.rs:7610` (`peer_revoke_json_exit_classification_matches_human_cli`).

Top-level keys are pinned to exactly these two by the test at `agent-os/crates/covenant/src/main.rs:5148` (`peer_revoke_json_pins_top_level_schema`), which also asserts `outcome` is a tagged-enum object and exercises both the `Ambiguous` and `NotFound` variants.

The envelope source-of-truth lives at `peer_revoke_json` in `agent-os/crates/covenant/src/main.rs:4620`. Two unit tests at `main.rs:5130` (`peer_revoke_json_renders_stable_ambiguous_shape`) and `main.rs:5148` cover the shape. The CLI verb is wired at `main.rs:3764-3864`; without `--json`, `Revoked` and `AlreadyRevoked` print tab-separated success lines to stdout, while `NotFound`, `Ambiguous`, and `SelfRevokeForbidden` print human-readable diagnostics to stderr before exiting `1`.

`covenant audit recent [-n|--limit <N>] [--since-ms <M>] [--stream] --json` emits a window of audit events. Envelope shape:

- `kind`: literal string `"audit_recent"`.
- `limit` (u64): the request limit echoed back from `-n`/`--limit` (default `50`, per `main.rs:3198`). Pinned as u64 at the schema test (`main.rs:6400-6403`) — never a string.
- `since_ms` (u64 or null): the Unix-epoch millisecond threshold echoed from `--since-ms`, or `null` when the flag was omitted. Pinned as u64-or-null at the schema test (`main.rs:6404-6407`) — never a string-of-integer. Same semantic as the HTTP gateway query parameter described in the **Query Parameters** section above: events whose `timestamp_ms` is strictly less than the threshold are dropped before the limit truncation.
- `events` (array of `AuditEvent`): the matched events. The array is empty when no events fall in the window. Pinned as an array by `main.rs:6408-6411` — never null or a string.

The inner `AuditEvent` shape, defined at `agent-os/crates/covenant-audit/src/lib.rs:43`:

- `id` (string) — event UUID.
- `timestamp_ms` (u64) — Unix-epoch milliseconds when the event was recorded.
- `issuer` (object) — `{display: string, pubkey: string (base58)}` per the `AgentId` Serialize impl at `covenant-types/src/lib.rs:124`.
- `kind` (object) — tagged-enum `AuditKind` (defined at `covenant-audit/src/lib.rs:71` onwards) with a `type` discriminator (e.g., `"capability_granted"`, `"intent_dispatched"`, `"hermes_tool_invoked"`) and variant-specific extra fields. Consumers must route on `kind.type` before reading variant-specific fields.

Top-level keys are pinned to exactly these four by the test at `agent-os/crates/covenant/src/main.rs:6383` (`audit_recent_json_pins_top_level_schema`), exercised against three cases: populated with `since_ms`, empty with `since_ms`, and empty without `since_ms`.

The envelope source-of-truth lives at `audit_recent_json` in `agent-os/crates/covenant/src/main.rs:4415`. Two unit tests at `main.rs:6356` (`audit_recent_json_renders_stable_shape`) and `main.rs:6383` cover the shape. The CLI verb is wired at `main.rs:3197-3267`; without `--json`, the same response is rendered as JSONL (one `AuditEvent` per line at `main.rs:3260`) mirroring the durable `audit/events.jsonl` row shape, with `(no audit events)` printed at `main.rs:3257` when empty. The optional `--stream` flag sets `Request::RecentAudit.prefer_stream = Some(true)` (`main.rs:3227`), enabling the v2 streaming-response path documented under [docs/protocol-versioning.md](./protocol-versioning.md); the terminal-response shape is unchanged when the streaming path is not selected.

`covenant audit purge --json` emits a summary of time-bounded audit-log garbage collection. Envelope shape:

- `kind`: literal string `"audit_purged"`.
- `before_ms` (u64): resolved Unix-epoch millisecond cutoff. The CLI accepts `--before-ms` or `--older-than-ms` with the same resolution semantics as `covenant capabilities purge --json` above. Pinned as u64 by `main.rs:6341-6344` — never a string-of-integer.
- `purged` (u64): count of audit events removed (the unsuffixed CLI message at `main.rs:3327` reads `purged <n> event(s)`, confirming the unit is an audit event, not a row class). May legitimately be `0` when no rows matched. Pinned as u64 by `main.rs:6345-6348` — never a string-of-integer.

Unlike the capability- and peer-purge verbs, this removes hash-chain entries; the cutoff enforcement is bound to the `audit.purge` capability scope at dispatch time so a delegated caller cannot purge beyond its scope's `before_ms` (see `docs/capabilities.md`).

Top-level keys are pinned to exactly these three by the test at `agent-os/crates/covenant/src/main.rs:6324` (`audit_purge_json_pins_top_level_schema`).

The envelope source-of-truth lives at `audit_purge_json` in `agent-os/crates/covenant/src/main.rs:4407`. Two unit tests at `main.rs:6316` (`audit_purge_json_renders_stable_shape`) and `main.rs:6324` cover the populated (`purged=3`) and empty (`purged=0`) cases. The CLI verb is wired at `main.rs:3291-3333`.

`covenant audit verify --json` emits the audit-log hash-chain integrity report. Envelope shape:

- `kind`: literal string `"audit_integrity"` — past-tense outcome name, distinct from the verb name `verify` and from the workspace-level `verify_report` envelope; consumers routing on `kind` must match this literal exactly rather than reusing either of those tokens.
- `report` (object): a structured `covenant_audit::AuditIntegrityReport`, never a string blob. The top-level object has exactly two keys (`kind` and `report`); the inner `report` is pinned by the schema test at `main.rs:6478-6481` to be a JSON object.

The inner `AuditIntegrityReport` shape, defined at `agent-os/crates/covenant-audit/src/lib.rs:61`:

- `events` (u64) — total audit events the integrity walk visited.
- `anchors` (u64) — count of anchor records (root-hash checkpoints) the walk crossed.
- `valid` (bool) — `true` when the hash chain is intact end-to-end; `false` when one or more failures were recorded.
- `root_hash_hex` (string) — the final root hash as lowercase hex, 64 characters (SHA-256). Pinned at the length level by the stable-shape test at `main.rs:6447-6453`.
- `failures` (array of strings) — human-readable failure descriptions (e.g., `"chain hash mismatch at event 3"`), empty when `valid` is `true`.

Top-level keys are pinned to exactly these two by the test at `agent-os/crates/covenant/src/main.rs:6461` (`audit_verify_json_pins_top_level_schema`), exercised against both a valid and an invalid report.

The envelope source-of-truth lives at `audit_verify_json` in `agent-os/crates/covenant/src/main.rs:4428`. Two unit tests at `main.rs:6433` (`audit_verify_json_renders_stable_shape`) and `main.rs:6461` cover the shape. The CLI verb is wired at `main.rs:3268-3290`; without `--json`, the same response is printed as the bare `AuditIntegrityReport` JSON (no envelope wrapper) at `main.rs:3284`, so JSON consumers must use `--json` to get the kind-discriminated envelope — the unsuffixed output is structurally compatible with `report` but lacks the `kind` field.

`covenant memory purge --json` emits a summary of time-bounded memory-store garbage collection. Envelope shape:

- `kind`: literal string `"memory_purged"`.
- `tier` (string or null): the memory tier slug — exactly one of `"working"`, `"episodic"`, or `"longterm"` (one word, per `memory_tier_slug` at `main.rs:1719-1724`). Null when `--tier` was omitted, meaning the purge applied to all tiers. Note an input-form asymmetry: the CLI parser at `main.rs:1729-1731` accepts `longterm`, `long-term`, and `long_term` for the `--tier` argument, but only the `longterm` slug is ever emitted in the envelope. Pinned as string-or-null by `main.rs:6533-6536` — never a structured object.
- `before_ms` (u64): resolved Unix-epoch millisecond cutoff. Same `--before-ms` / `--older-than-ms` resolution semantics as `covenant capabilities purge --json` above. Pinned as u64 by `main.rs:6537-6540` — never a string-of-integer.
- `purged` (u64): count of memory records removed. The unsuffixed CLI prints `purged <n> record(s)` at `main.rs:2164`, confirming the unit is a memory record. May legitimately be `0` when no rows matched. Pinned as u64 by `main.rs:6541-6544` — never a string-of-integer.

Top-level keys are pinned to exactly these four by the test at `agent-os/crates/covenant/src/main.rs:6516` (`memory_purge_json_pins_top_level_schema`), which also exercises the null-tier case.

The envelope source-of-truth lives at `memory_purge_json` in `agent-os/crates/covenant/src/main.rs:4435`. Two unit tests at `main.rs:6504` (`memory_purge_json_renders_stable_shape`, both a Working-tier populated case and a no-tier null case) and `main.rs:6516` cover the populated and empty (`purged=0`, no-tier) cases. The CLI verb is wired at `main.rs:2120-2170`.

`covenant memory recent [--tier <T>] [-n|--limit <N>] [--stream] --json` and `covenant memory search <query> [--tier <T>] [-n|--limit <N>] [--min-relevance <R>] --json` both emit the same memory-read envelope, distinguished only by the `mode` discriminator. Envelope shape:

- `kind`: literal string `"memory_read"`.
- `mode` (string): exactly one of `"recent"` or `"search"` (lowercase, matching the CLI verb name — no other values are emitted). Consumers must route on `mode` to know which null pattern to expect across `query` and `min_relevance`.
- `tier` (string or null): the requested `MemoryTier` as its lowercase wire slug — exactly one of `"working"`, `"episodic"`, or `"longterm"` (one word, per `MemoryTier`'s `#[serde(rename_all = "lowercase")]` at `covenant-types/src/lib.rs:23` and the slug map at `memory_tier_slug` in `main.rs:1719-1724`). The CLI parser accepts `longterm`, `long-term`, and `long_term` as input forms for `--tier`, but only the `longterm` slug is ever emitted. `null` when `--tier` was omitted (meaning the request applied to all tiers). Pinned as string-or-null by the schema test (`main.rs:6838-6841`) — never a structured object.
- `limit` (u64): the request limit echoed back from `-n`/`--limit` (default `10` for both verbs, per `main.rs:2077` and `main.rs:2487`). Pinned as u64 at the schema test (`main.rs:6834-6837`).
- `query` (string or null): for `mode="search"`, the request query (whitespace-joined when the operator passed multiple positional tokens, per `main.rs:2522`). For `mode="recent"`, always `null` (the recent verb does not accept a query). Pinned as string-or-null by the schema test (`main.rs:6842-6845`).
- `min_relevance` (number or null): for `mode="search"`, the float echoed from `--min-relevance` (validated to a finite `f32` in `[0.0, 1.0]` at `main.rs:2510-2514`), or `null` when the flag was omitted. For `mode="recent"`, always `null`. Pinned as f64-or-null by the schema test (`main.rs:6846-6849`) — never a string.
- `records` (array of `MemoryRecord`): the matched records in the order returned by the daemon. The array is empty when no records match; the unsuffixed CLI prints `(no records)` for that case at `main.rs:1625`.

The inner `MemoryRecord` shape, defined at `agent-os/crates/covenant-types/src/lib.rs:183`:

- `id` (string) — record UUID, serialized as the canonical hyphenated string form.
- `tier` (string) — lowercase `MemoryTier` slug (same enumeration as the top-level `tier` above; always present, never null).
- `owner` (object) — `{display: string, pubkey: string (base58)}` per the `AgentId` Serialize impl at `covenant-types/src/lib.rs:124`.
- `text` (string) — the stored memory text.
- `embedding` (array of numbers) — the record's embedding vector as a JSON array of f32 values. The array is always present (empty when no embedding was attached); consumers must not assume the field is omitted.
- `metadata` (JSON value) — an arbitrary `serde_json::Value` (object, array, primitive, or null). The daemon emits whatever metadata the writer attached; consumers must not assume an object shape.
- `created_at` (u64) — Unix-epoch milliseconds when the record was written.
- `parent` (string or null) — parent record UUID for derived memories. Carries `#[serde(default)]` at `covenant-types/src/lib.rs:192-193` **without** `skip_serializing_if`, so the field is **always emitted** (as `null` when the record has no parent), not omitted. JSON consumers must read it with null-vs-value, not key-existence.

Top-level keys are pinned to exactly these seven by the test at `agent-os/crates/covenant/src/main.rs:6808` (`memory_read_json_pins_top_level_schema`), exercised against both a `mode="search"` case (populated `query`, `min_relevance`, non-empty `records`) and a `mode="recent"` case (null `query`, null `min_relevance`, empty `records`).

The envelope source-of-truth lives at `memory_read_json` in `agent-os/crates/covenant/src/main.rs:4463`. Two unit tests at `main.rs:6765` (`memory_read_json_renders_stable_shape`) and `main.rs:6808` cover both modes. The CLI verbs are wired at `main.rs:2075-2119` (`covenant memory recent`) and `main.rs:2481-2547` (`covenant memory search`); without `--json`, each record prints as `[<created_at>] <tier>: <text>` at `main.rs:1629`. The optional `--stream` flag is accepted only by `covenant memory recent` (per `main.rs:2094`) and sets `Request::RecentMemory.prefer_stream = Some(true)` to enable the v2 streaming-response path documented under [docs/protocol-versioning.md](./protocol-versioning.md); the terminal envelope shape is unchanged when the streaming path is not selected. `covenant memory search` has no `--stream` flag.

`covenant a2a status [-n|--limit <N>] [--min-lease-age-ms <N>] [--deadline-within-ms <N>] [--state queued|in_flight] --json` emits the current A2A queue snapshot — queued tasks, in-flight leases, and pending results — narrowed by the supplied filters. Envelope shape:

- `kind`: literal string `"a2a_status"`.
- `limit` (u64): the request limit echoed back from `-n`/`--limit` (default `10`, per `main.rs:3357`). Pinned as u64 by the schema test (`main.rs:7404-7407`).
- `min_lease_age_ms` (u64 or null): the threshold echoed from `--min-lease-age-ms`, or `null` when the flag was omitted. Always emitted (as `null` when inactive) — never omitted from the envelope. Pinned as u64-or-null by the schema test (`main.rs:7408-7411`).
- `deadline_within_ms` (u64 or null): the threshold echoed from `--deadline-within-ms`, or `null` when the flag was omitted. Same always-emitted-as-null contract as `min_lease_age_ms`. Pinned as u64-or-null by the schema test (`main.rs:7412-7415`).
- `state_filter` (string or null): the `A2ATaskQueueState` slug echoed from `--state` — exactly `"queued"` or `"in_flight"` (snake_case, per `A2ATaskQueueState`'s `#[serde(rename_all = "snake_case")]` at `covenant-a2a/src/lib.rs:124-129`), or `null` when the flag was omitted. Pinned as string-or-null by the schema test (`main.rs:7416-7419`) — never an integer or array. Consumers must route on the lowercase wire form, **not** the Rust TitleCase names (`"Queued"`, `"InFlight"`).
- `tasks` (array of `A2ATaskQueueEntry`): the matched queue entries in the order returned by the daemon. The array may be empty.
- `results` (array of `A2ATaskResult`): pending results not yet acknowledged. The array may be empty; the unsuffixed CLI prints `(a2a queue empty)` at `main.rs:3422` when both `tasks` and `results` are empty.

The inner `A2ATaskQueueEntry` shape, defined at `agent-os/crates/covenant-a2a/src/lib.rs:132`:

- `state` (string) — `A2ATaskQueueState` slug, exactly `"queued"` or `"in_flight"` (same enumeration as the top-level `state_filter`). The canonical signal for queue-state branching — **not** lease-field presence.
- `task` (object) — nested `A2ATask` (see below).
- `lease_id` (string, omitted when null) — UUID of the active lease. Carries `#[serde(default, skip_serializing_if = "Option::is_none")]` at `covenant-a2a/src/lib.rs:135-136`, so the key is **absent** when the entry is not leased.
- `leased_to` (object, omitted when null) — `AgentId` of the leaseholder. Same skip-when-absent contract.
- `leased_at_ms` (u64, omitted when null) — Unix-epoch milliseconds when the lease was taken. Same skip-when-absent contract.
- `attempt` (u32) — delivery attempt counter (always emitted; `0` for a fresh queue entry per `#[serde(default)]` at `covenant-a2a/src/lib.rs:141-142`).

The nested `A2ATask` shape, defined at `agent-os/crates/covenant-a2a/src/lib.rs:109`:

- `id` (string) — task UUID.
- `sender` (object) — `AgentId` `{display, pubkey}` per the form documented in the `peer_list` block.
- `recipient` (object) — `AgentId` of the routed-to peer.
- `intent_text` (string) — the task body.
- `task_kind` (string, omitted when null) — optional task-kind label; `skip_serializing_if = "Option::is_none"`.
- `parent` (string, omitted when null) — optional parent task UUID; same skip contract.
- `deadline_ms` (u64, omitted when null) — optional deadline (Unix-epoch ms); same skip contract.
- `idempotency` (object, omitted when null) — optional `A2AIdempotency` `{duplicate_safety: "unsafe"|"idempotent", key: string}` (defined at `covenant-a2a/src/lib.rs:55-59`); same skip contract.

The inner `A2ATaskResult` shape, defined at `agent-os/crates/covenant-a2a/src/lib.rs:387`:

- `task_id` (string) — the task UUID this result binds to.
- `status` (string) — `A2ATaskStatus` slug, exactly one of `"ok"`, `"error"`, `"partial"` (snake_case per `covenant-a2a/src/lib.rs:40-46`). Consumers must route on the lowercase wire form, **not** the Rust TitleCase names.
- `content` (array of `Content`) — the same tagged-enum `Content` shape (`{type: "text", text: <string>}` or `{type: "json", value: <JSON>}`) already documented in the `tool_result` block above; empty for `error` results per `A2ATaskResult::error` at `covenant-a2a/src/lib.rs:406-413`.
- `error_message` (string, omitted when null) — diagnostic message for `error` results; `skip_serializing_if = "Option::is_none"` per `covenant-a2a/src/lib.rs:392-393`. Absent on `ok` and `partial` results.

Top-level keys are pinned to exactly these seven by the test at `agent-os/crates/covenant/src/main.rs:7379` (`a2a_status_json_pins_top_level_schema`), exercised against both a populated-filters case and an all-null-filters case.

The envelope source-of-truth lives at `a2a_status_json` in `agent-os/crates/covenant/src/main.rs:4594`. Three unit tests at `main.rs:7328` (`a2a_status_json_renders_stable_shape`), `main.rs:7370` (`a2a_status_json_omits_deadline_filter_when_inactive`, which pins the always-emitted-as-null contract on the filter fields), and `main.rs:7379` cover the shape. The CLI verb is wired at `main.rs:3349-3434`; without `--json`, the same response is rendered as JSONL with each task printed as `{"type": "task", "entry": <A2ATaskQueueEntry>}` and each result as `{"type": "result", "result": <A2ATaskResult>}` (per `main.rs:3417-3428`) — a different envelope shape than `--json`, so JSON consumers must use `--json` to get the kind-discriminated envelope.

`covenant a2a retry-stale [--enable] [--min-lease-age-ms <N>] [--max-attempts <N>] [--max-requeues <N>] [--scan-limit <N>] --json` emits a per-call report describing what the auto-retry scan considered, requeued, and skipped. Envelope shape:

- `kind`: literal string `"a2a_auto_retry"`.
- `report` (object): a structured `A2AAutoRetryReport` (defined at `agent-os/crates/covenant-a2a/src/lib.rs:288`), never a string blob. The top-level object has exactly two keys (`kind` and `report`); the inner `report` is pinned by the schema test at `main.rs:6281-6284` to be a JSON object.

**Dry-run by default**: `A2AAutoRetryPolicy.enabled` defaults to `false` (per `Default for A2AAutoRetryPolicy` at `covenant-a2a/src/lib.rs:228-238`), and the CLI's `--enable` flag is the only path that flips it (`main.rs:3537`). On a `--json` call without `--enable`, every queue entry will appear under `skipped[]` with `reason: "disabled"` and the registry will not be mutated — a `requeued=0` result there is **not** a "nothing to retry" signal. Consumers analysing the report must read `report.policy.enabled` before drawing conclusions about whether `considered` minus `requeued.len()` indicates real skip pressure or a dry-run preview.

The inner `A2AAutoRetryReport` shape:

- `policy` (object) — the `A2AAutoRetryPolicy` echoed from the request (see below).
- `considered` (u64) — number of in-flight queue entries the scan evaluated. Bounded by `policy.scan_limit`.
- `requeued` (array of `A2AAutoRetryRequeued`) — entries the scan successfully requeued under the policy. Empty when the policy is disabled or when no candidate met the requeue criteria. Carries `#[serde(default)]` on the deserialization side (`covenant-a2a/src/lib.rs:291-292`); the serializer always writes the array.
- `skipped` (array of `A2AAutoRetrySkipped`) — entries the scan considered but did not requeue, each with a typed skip reason. Same `#[serde(default)]` contract.

The inner `A2AAutoRetryPolicy` shape, defined at `covenant-a2a/src/lib.rs:215`:

- `enabled` (bool) — see the dry-run note above.
- `min_lease_age_ms` (u64) — minimum lease age before an in-flight entry is eligible for auto-requeue.
- `max_attempts` (u32) — per-entry attempt ceiling.
- `max_requeues` (u64) — per-call requeue ceiling (`usize` on the Rust side; serialized as a JSON integer).
- `scan_limit` (u64) — per-call scan size cap (`usize` on the Rust side).

The inner `A2AAutoRetryRequeued` shape, defined at `covenant-a2a/src/lib.rs:280`:

- `task_id` (string) — task UUID.
- `lease_id` (string) — the lease UUID that was preempted by the requeue.
- `attempt` (u32) — the attempt counter before the requeue (the requeued entry will resurface with `attempt+1`).
- `idempotency_key` (string) — the idempotency key that bound this task's delivery — present because `unsafe_duplicate_safety` is one of the documented skip reasons, so only safely-bound tasks reach `requeued[]`.

The inner `A2AAutoRetrySkipped` shape, defined at `covenant-a2a/src/lib.rs:271`:

- `task_id` (string) — task UUID.
- `reason` (string) — `A2AAutoRetrySkipReason` slug (see enumeration below).
- `attempt` (u32) — the entry's current attempt counter.
- `lease_age_ms` (u64, omitted when null) — observed lease age in milliseconds. Carries `#[serde(default, skip_serializing_if = "Option::is_none")]` at `covenant-a2a/src/lib.rs:275-276`, so the key is **absent** when the skip happened before any lease age was meaningful (e.g. `reason: "disabled"` or `reason: "not_in_flight"`). JSON consumers must read it with key-existence, not null-vs-value.

`A2AAutoRetrySkipReason` enumerates exactly these nine snake_case slugs (per `covenant-a2a/src/lib.rs:240-252`):

- `"disabled"` — `policy.enabled = false`; emitted for every considered entry on a dry-run call.
- `"not_in_flight"` — entry is queued rather than leased.
- `"missing_lease"` — entry state is `in_flight` but the lease record is absent.
- `"lease_too_young"` — lease age is below `policy.min_lease_age_ms`.
- `"missing_idempotency"` — task carries no `A2AIdempotency` binding.
- `"unsafe_duplicate_safety"` — task's idempotency binding declares `duplicate_safety: "unsafe"`.
- `"max_attempts_reached"` — entry has hit `policy.max_attempts`.
- `"limit_reached"` — this call has already requeued `policy.max_requeues` entries.
- `"capability_scope_mismatch"` — the caller's signed capability scope does not authorise requeue on this task.

Consumers must route on the lowercase wire form, **not** the Rust TitleCase names (`"Disabled"`, `"NotInFlight"`, etc.) — those never appear on the wire.

Top-level keys are pinned to exactly these two by the test at `agent-os/crates/covenant/src/main.rs:6264` (`a2a_retry_json_pins_top_level_schema`), exercised against both a populated (requeued + skipped) case and an empty (fresh policy) case.

The envelope source-of-truth lives at `a2a_retry_json` in `agent-os/crates/covenant/src/main.rs:4613`. Two unit tests at `main.rs:6227` (`a2a_retry_json_renders_stable_shape`) and `main.rs:6264` cover the shape. The CLI verb is wired at `main.rs:3524-3580`; without `--json`, the same response prints `considered <N> task(s), requeued <M>, skipped <K>` followed by `automatic retry disabled; pass --enable to mutate` whenever `report.policy.enabled` is `false` (per `main.rs:3566-3574`).

`covenant a2a compact --json` emits a summary of the event-log compaction that drops lines for fully-resolved A2A tasks. Envelope shape:

- `kind`: literal string `"a2a_compacted"` — past-tense outcome name, distinct from the verb name `compact`; consumers routing on `kind` must match the literal exactly rather than reusing the verb token (`"a2a_compact"`) or guessing a noun form (`"a2a_compaction"`).
- `dropped` (u64): count of event-log lines removed for resolved tasks. May legitimately be `0` when no resolved tasks remain — the unsuffixed CLI still prints `dropped 0 a2a event(s)` at `main.rs:3597`, and JSON consumers must not treat `dropped=0` as an error.

Top-level keys are pinned to exactly these two by the test at `agent-os/crates/covenant/src/main.rs:6199` (`a2a_compact_json_pins_top_level_schema`), exercised against both a populated (`dropped=3`) and an empty (`dropped=0`) case.

The envelope source-of-truth lives at `a2a_compact_json` in `agent-os/crates/covenant/src/main.rs:4400`. Two unit tests at `main.rs:6192` (`a2a_compact_json_renders_stable_shape`) and `main.rs:6199` cover the shape. The CLI verb is wired at `main.rs:3581-3603`; without `--json`, the same response prints `dropped <N> a2a event(s)` at `main.rs:3597`.

`covenant memory compact --reason <text> [--apply] [--detach-stale-parents] [--delete-working-before-ms <M> | --delete-working-older-than-ms <D>] [--delete-episodic-before-ms <M> | --delete-episodic-older-than-ms <D>] [--mark-longterm-stale-before-ms <M> | --mark-longterm-stale-older-than-ms <D>] [--marked-at-ms <M>] --json` emits the outcome of a memory-store compaction pass. Envelope shape:

- `kind`: literal string `"memory_compacted"` — past-tense outcome name, distinct from the verb name `compact`; consumers routing on `kind` must match the literal exactly rather than reusing the verb token (`"memory_compact"`) or guessing a noun form (`"memory_compaction"`).
- `outcome` (object): a structured `MemoryCompactionOutcome` (defined at `agent-os/crates/covenant-types/src/lib.rs:297`), never a string blob. The top-level object has exactly two keys (`kind` and `outcome`); the inner `outcome` is pinned by the schema test at `main.rs:6601-6604` to be a JSON object.

**Dry-run by default, mutates only with `--apply`**: the CLI defaults to `MemoryRepairMode::DryRun` (per `main.rs:2263-2271`) and `--reason <text>` is mandatory regardless of mode (the CLI bails with `"missing --reason"` at `main.rs:2270` when omitted). Without `--apply`, the daemon evaluates the policy and reports what *would* change but does not mutate the store.

The inner `MemoryCompactionOutcome` shape:

- `mode` (string) — `MemoryRepairMode` slug, exactly `"dry_run"` or `"apply"` (snake_case, per `MemoryRepairMode`'s `#[serde(rename_all = "snake_case")]` at `covenant-types/src/lib.rs:196-201`). Consumers must route on the lowercase wire form, **not** the Rust TitleCase names.
- `would_change` (bool) — the policy identified at least one mutation that would land. Reliable in both modes — `true` whenever the policy matched records.
- `changed` (bool) — the store was actually mutated by this call. In `mode: "dry_run"` this is **always `false`** even when `would_change` is `true`; only `mode: "apply"` can set it. JSON consumers branching on `changed` alone will silently treat dry-run planning runs as no-ops; route on the `(mode, would_change, changed)` triple instead.
- `deleted` (array of strings) — UUIDs of records the policy deleted (in `apply` mode) or would delete (in `dry_run` mode).
- `stale_marked` (array of strings) — UUIDs of long-term records the policy marked stale (or would mark, in dry-run mode).
- `parents_detached` (array of strings) — UUIDs of records whose parent pointer the policy detached (or would detach, when `--detach-stale-parents` is supplied).

Top-level keys are pinned to exactly these two by the test at `agent-os/crates/covenant/src/main.rs:6584` (`memory_compaction_json_pins_top_level_schema`), exercised against both a populated `apply` case and an empty `dry_run` case.

The envelope source-of-truth lives at `memory_compaction_json` in `agent-os/crates/covenant/src/main.rs:4444`. Two unit tests at `main.rs:6556` (`memory_compaction_json_renders_stable_shape`) and `main.rs:6584` cover the shape. The CLI verb is wired at `main.rs:2171-2279` (shared with `covenant memory plan-compaction`; the `plan-compaction` arm forces dry-run and emits a different envelope documented below).

`covenant memory plan-compaction --reason <text> [--detach-stale-parents] [--delete-working-before-ms <M> | --delete-working-older-than-ms <D>] [--delete-episodic-before-ms <M> | --delete-episodic-older-than-ms <D>] [--mark-longterm-stale-before-ms <M> | --mark-longterm-stale-older-than-ms <D>] [--marked-at-ms <M>] --json` emits a read-only compaction plan. The verb shares its argument parser with `covenant memory compact` but is forced into dry-run mode. Envelope shape:

- `kind`: literal string `"memory_compaction_plan"` — distinct from `memory_compacted` so consumers can route on the planning vs mutating outcome without inspecting `outcome.mode`.
- `outcome` (object): the same `MemoryCompactionOutcome` shape documented in the `memory_compacted` block above. For this verb, `outcome.mode` is **always** `"dry_run"` and `outcome.changed` is **always** `false`; a non-`dry_run` value here indicates daemon/CLI drift and JSON consumers should treat it as a protocol violation.
- `expected_receipt_changes` (object): a forward-compatibility placeholder pinned by the schema test at `main.rs:6702` (`memory_compaction_plan_json_pins_expected_receipt_changes_schema`). The block has exactly three keys today and is currently a no-claim stub; consumers must validate the inner shape rather than dispatch directly to apply-mode logic.

**`--apply` is rejected** at the CLI level (`main.rs:2180-2182`: `bail!("memory plan-compaction is read-only and does not accept --apply")`) even though the underlying `Request::CompactMemory` request accepts both modes. `--reason <text>` remains mandatory, matching the `memory compact` verb.

The inner `expected_receipt_changes` shape:

- `mode` (string): literal `"none"` today. Pinned by the schema test at `main.rs:6721-6725` as the only currently-allowed value; consumers must treat any other value as a sign that receipt-aware compaction has shipped and the docs are stale.
- `records` (array): empty today (length pinned to `0` at `main.rs:6730-6736`). Will gain a real shape once receipt-aware compaction lands.
- `reason` (string): a human-readable explanation of why the block is empty. Currently the literal `"dry-run compaction planning does not mutate memory or settlement receipts"` per `main.rs:4458`; consumers must not branch on the exact text — only on the field's existence and type.

Top-level keys are pinned to exactly these three by the test at `agent-os/crates/covenant/src/main.rs:6653` (`memory_compaction_plan_json_pins_top_level_schema`), exercised against both a populated dry-run case and an empty dry-run case.

The envelope source-of-truth lives at `memory_compaction_plan_json` in `agent-os/crates/covenant/src/main.rs:4451`. Three unit tests at `main.rs:6629` (`memory_compaction_plan_json_renders_stable_shape`), `main.rs:6653` (`memory_compaction_plan_json_pins_top_level_schema`), and `main.rs:6702` (`memory_compaction_plan_json_pins_expected_receipt_changes_schema`) cover both the outer envelope and the placeholder block. The CLI verb is wired at `main.rs:2171-2279` (shared parser with `covenant memory compact`, branched into the plan-only path at `main.rs:2172`); the `plan-compaction` arm sets `as_json` to `true` by default (`main.rs:2176`) so the unsuffixed CLI also emits the JSON envelope — there is no human-readable plan rendering.

`covenant ignore check <text> --json` emits the result of evaluating the configured ignore rules against operator-supplied text. Envelope shape:

- `kind`: literal string `"ignore_report"`.
- `ignored` (boolean): `true` when at least one loaded rule matched the supplied text; `false` otherwise. Pinned as a JSON boolean by the schema test (`main.rs:6912-6915`) — never `0`/`1` or a string-truthy value.
- `matched_pattern` (string or null): the matched rule pattern when `ignored` is `true`; **always `null`** when `ignored` is `false`. Pinned as string-or-null by the schema test (`main.rs:6916-6919`) — never an empty string for the unmatched case. JSON consumers must use `null` (not `""`) as the unmatched discriminator.
- `rules_loaded` (u64): count of ignore rules the daemon evaluated. May legitimately be `0` when no rules are configured, in which case `ignored` is always `false` and `matched_pattern` is always `null`.

**Exit-code coupling**: when `ignored` is `true`, the CLI exits `1` even in the `--json` path (per `main.rs:4020-4022`); the envelope is written to stdout *before* the exit. JSON consumers running this verb to gate downstream processing must read the envelope rather than relying solely on transport success — a `--json` invocation that exits `1` is the **expected** signal for a matched ignore rule, not an error.

Top-level keys are pinned to exactly these four by the test at `agent-os/crates/covenant/src/main.rs:6895` (`ignore_report_json_pins_top_level_schema`), exercised against both an `ignored=true` case with a non-null `matched_pattern` and an `ignored=false` case with a null `matched_pattern` and zero `rules_loaded`.

The envelope source-of-truth lives at `ignore_report_json` in `agent-os/crates/covenant/src/main.rs:4482`. Two unit tests at `main.rs:6883` (`ignore_report_json_renders_stable_shape`) and `main.rs:6895` cover the shape. The CLI verb is wired at `main.rs:3979-4027`; without `--json`, the matched case prints `ignored — matched rule: <pattern>` at `main.rs:4016` and the unmatched case prints `not ignored (<n> rule(s) loaded)` at `main.rs:4018`. Both paths share the exit-1-when-ignored convention.

`covenant bootstrap --json` emits a summary of the capability-bootstrap pass that grants every action required by manifests under `$COVENANT_HOME/agents/*/agent.toml` (plus the implicit `memory.write`, which the daemon writes on every successful dispatch). Envelope shape:

- `kind`: literal string `"bootstrap_result"`.
- `granted` (array of `{action: string, signature_b58: string}` objects): the capabilities **newly granted** during this bootstrap call. Each element echoes the action string and the daemon-signed base58 signature that authorises it. Pinned as an array by `main.rs:5701-5704` — never null or a string.
- `already_granted` (array of strings): the action names the daemon **already had** before this call. Note the asymmetry with `granted`: this field carries **bare action strings**, not the `{action, signature_b58}` object shape — the existing signatures are not echoed here. JSON consumers must not iterate `already_granted` as if it were objects.

Top-level keys are pinned by the test at `agent-os/crates/covenant/src/main.rs:5684` (`bootstrap_result_json_pins_top_level_schema`), exercised against a populated case (two newly-granted entries plus one already-granted entry), an empty-granted case (no new grants, two already-granted entries), and a fully-empty case. The test also asserts the asymmetric inner shape: `granted` entries are `{action, signature_b58}` objects while `already_granted` entries are bare action strings.

Re-running `covenant bootstrap` is idempotent: if every required action is already granted, `granted` is empty and `already_granted` carries the full set. An empty `granted` array is the **expected** signal for "nothing to do" — not a transport failure. The unsuffixed CLI prints `nothing to do — every required capability is already granted (<n> total)` at `main.rs:1947-1950` for that case.

The envelope source-of-truth lives at `bootstrap_result_json` in `agent-os/crates/covenant/src/main.rs:4580`. Two unit tests at `main.rs:5661` (`bootstrap_result_json_renders_stable_shape`) and `main.rs:5684` cover the shape. The CLI verb is wired at `main.rs:1866-1969`; the JSON emission site calls the helper at `main.rs:1944-1945`. Required actions are derived from the union of every `agent.toml`'s `[capabilities].required` list (`main.rs:1882-1902`) plus the unconditional `memory.write` insertion (`main.rs:1884`). The daemon-side dispatch is `Request::GrantCapability` per action (`main.rs:1923-1930`); failures fall through to a `daemon error granting <action>: <message>` bail rather than into the envelope. Without `--json`, the same response prints `granted <n> of <m> capabilities to user@local:` followed by one `  + <friendly-title> (<action>)` line per newly-granted entry and a final `ready. try: covenant intent "say hello"` (per `main.rs:1952-1967`).

`covenant intents resume <intent-id|latest> --json` emits the outcome of resuming a previously-paused intent (typically one that hit a `BudgetExhausted` audit row). The envelope is **two-shape**: success and error share the same `kind` discriminator and use a flat `ok` boolean as the structural discriminator at the top level — **not** a tagged-enum `outcome.type` like `peer_revoke`. Consumers must branch on `ok` to know which key set to expect.

Both branches share these fields:

- `kind`: literal string `"intents_resume"` — verb-name asymmetry: the CLI verb is `resume` but the envelope discriminator is `intents_resume` (the noun-verb compound, not the verb token alone); consumers routing on `kind` must match the literal exactly. The same literal is emitted on both `ok=true` and `ok=false` envelopes.
- `ok` (boolean): `true` on success, `false` on every error path. Pinned as a JSON boolean by the schema tests at `main.rs:5371-5374` and `main.rs:5214-5217` — never `0`/`1` or a string-truthy value. JSON consumers branching on `ok` alone get the correct outcome class without inspecting variant-specific fields.
- `mode` (string): exactly `"explicit"` or `"latest"`, derived from the CLI invocation form at `main.rs:3889` (`--latest`/`latest` → `"latest"`, any positional intent-id → `"explicit"`). The envelope echoes the operator's input form, so consumers can distinguish a targeted resume from a "resume the most recent paused intent" call.

**Success branch (`ok=true`)** carries these eight top-level keys per the test EXPECTED_KEYS at `main.rs:5346-5355`:

- `intent_id` (string) — the resumed intent's UUID in canonical hyphenated form. Pinned as a string by `main.rs:5376-5379` — never a byte array.
- `status` (string) — the daemon-returned outcome status (typically `"ok"`). The string shape is pinned at `main.rs:5380-5383`; specific value enumeration lives with the daemon's intent dispatcher rather than this docs surface.
- `text` (string) — the result text the daemon returned for the resumed intent. The unsuffixed CLI prints this value directly at `main.rs:3938`.
- `sources` (array of strings) — source labels that contributed to the result. Pinned as an array of strings by `main.rs:5385-5388` — never a comma-joined string. Empty when no sources are attached; the unsuffixed CLI prints a `sources:` block followed by `  - <label>` lines at `main.rs:3941-3944` only when the array is non-empty.
- `settlement` (object or null) — an optional `SettlementReceipt` (defined at `agent-os/crates/covenant-types/src/lib.rs:339` and documented in the `receipt_list` block above) carrying the on-chain or local settlement evidence when the resumed intent consumed credits. `null` when the resume did not settle. Pinned as object-or-null by `main.rs:5389-5392` — never an integer or array.

**Error branch (`ok=false`)** carries these five top-level keys per the test EXPECTED_KEYS at `main.rs:5198`:

- `intent_id` (string or null) — string-uuid when the intent_id was already resolved (e.g., the daemon round-trip started but returned an error, per `main.rs:3951-3956`); **null** when the intent_id could not be resolved before the daemon round-trip (e.g., `missing_intent_id` and `conflicting_flags` paths at `main.rs:3900-3908`). Pinned as string-or-null by `main.rs:5224-5227`. JSON consumers must accept `null` here rather than treating it as a malformed envelope.
- `error` (object): a structured `{code, message}` pair, never a string blob. Pinned as a JSON object by `main.rs:5228-5231`. The inner `error` object has exactly two keys per the test EXPECTED_KEYS at `main.rs:5251`:
  - `code` (string) — typed error slug (snake_case). Pinned as a string by `main.rs:5265-5268` — never a structured object. The recognised slugs from `intents_resume_error_code` at `main.rs:4627-4642` are `"no_budget_exhausted_row"`, `"missing_intent_id"`, `"invalid_intent_id"`, `"conflicting_flags"`, and the catch-all `"error"` for daemon messages that do not match a typed arm. Additionally, the inline emission paths add `"daemon_error"` (when the daemon returns `Response::Error`, `main.rs:3954`) and `"unexpected_response"` (when the daemon returns an unexpected response variant, `main.rs:3969`). Consumers must route on the documented slugs; new slugs are a forcing function to update this docs block and the `intents_resume_error_code_pins_documented_arms` test at `main.rs:5291`.
  - `message` (string) — the human-readable diagnostic the daemon or CLI produced. Pinned as a string by `main.rs:5269-5272` — never a structured object.

**Exit-code coupling**: the `--json` path exits `1` on every error branch (`main.rs:3909`, `main.rs:3958`, `main.rs:3973`); the envelope is written to stdout *before* the exit. JSON consumers gating downstream processing on transport success will silently miss resume failures — branch on `ok` rather than on exit `0`. Success exits `0` via the normal return at `main.rs:3936`. This mirrors the `peer_revoke` and `ignore_report` exit-coupling conventions documented above.

Top-level keys are pinned by three tests at `agent-os/crates/covenant/src/main.rs`: `intents_resume_ok_json_pins_top_level_schema` at `main.rs:5345` (success branch, exercises both `Some(SettlementReceipt)` and `None` cases), `intents_resume_error_json_pins_top_level_schema` at `main.rs:5197` (error branch, exercises both `Some(intent_id)` and `None` cases), and `intents_resume_error_json_pins_error_object_schema` at `main.rs:5250` (inner `error` object's two-key shape). The typed-slug enumeration is pinned by `intents_resume_error_code_pins_documented_arms` at `main.rs:5291`.

The envelope source-of-truth lives at `intents_resume_ok_json` (`main.rs:4644`) and `intents_resume_error_json` (`main.rs:4664`), with the slug classifier at `intents_resume_error_code` (`main.rs:4627`). The CLI verb is wired at `main.rs:3868-3977`; without `--json`, the success branch prints the result `text` at `main.rs:3938` followed by an optional `sources:` block, and the error branches bail with the human-readable diagnostic — there is no envelope rendering, so JSON consumers must use `--json` to get the kind-discriminated envelope.

`covenant settlement backfill-receipts [--dry-run] [--json]` emits a versioned-schema envelope describing the legacy settlement-receipt repair pass. **Schema-suffix convention**, not the unversioned `kind` convention every other envelope in this section uses — consumers route on `schema` rather than `kind`. The asymmetry is consistent with the section preamble (line 86): post-`covenant.<area>.<verb>.v<n>` envelopes land with the suffix; only the older read-side envelopes carry the bare `kind` literal.

Envelope shape:

- `schema`: literal string `"covenant.settlement.backfill.v1"`. The `.v1` suffix is the version slot; a future `.v2` would be a separate envelope, not a field rename inside this one. Consumers must route on the full literal — matching on the prefix `"covenant.settlement.backfill."` will swallow incompatible future versions.
- `row_count` (u64): count of legacy settlement-receipt rows the backfill operated on (mutation path) or *would* operate on (dry-run path). May legitimately be `0` when no legacy rows match — the verb does not error on an empty backfill.
- `rollback_path` (string or null): filesystem path to the rollback-evidence file written by a mutation pass; `null` in dry-run mode. The CLI's inline emission at `main.rs:4072` passes `rollback_path.as_deref()` through `Option<&str>`, and the unsuffixed CLI at `main.rs:4080-4082` maps `None` to the literal `(none)` — JSON consumers must use `null` (not `""` or `"(none)"`) as the unset discriminator. When non-null, the path is meaningful only on the daemon's local filesystem; remote consumers must not assume the file is reachable.
- `dry_run` (bool): echoes the `--dry-run` CLI flag. `true` is a safe planning preview that does not mutate the receipt table; `false` is a real mutation pass that may write rollback evidence. Pinned at the type level only by the inline `serde_json::json!` macro — never `0`/`1` or a string.

**`--scope-pubkey` is reserved, not yet wired**: the CLI accepts a `--scope-pubkey <value>` flag and forwards it through `Request::BackfillSettlementReceipts.scope_pubkey` (`main.rs:4043-4048`, `main.rs:4057`), but the daemon-side filter is not yet implemented (see the help text at `main.rs:1480` and the file-header CLI summary at `main.rs:27`). Operators relying on the flag for scoped backfills will not get the scoping behavior they expect; the envelope reports the unscoped result regardless. This will change when the approved `settlement-receipt-backfill-mutation` slice lands.

Top-level keys are pinned by the test at `agent-os/crates/covenant/src/main.rs:5550` (`settlement_backfill_json_pins_top_level_schema`), exercised against both a dry-run shape (`rollback_path` null), a mutation shape (`rollback_path` set), and an empty-rows dry-run shape; the test also asserts the literal `"covenant.settlement.backfill.v1"` schema string so a future v2 bump must land as a separate envelope, not a field rename inside this one.

The envelope source-of-truth lives at `settlement_backfill_json` in `agent-os/crates/covenant/src/main.rs:4558`. Two unit tests at `main.rs:5528` (`settlement_backfill_json_renders_stable_shape`) and `main.rs:5550` cover the shape. The CLI verb is wired at `main.rs:4034-4088` (the `settlement backfill-receipts` subcommand); without `--json`, the same response prints `row_count: <N>`, `dry_run: <bool>`, and `rollback_path: <path>|(none)` on three separate lines at `main.rs:4077-4082`. The daemon-side `Response::SettlementReceiptsBackfilled` variant carries the three fields directly (`main.rs:4062-4066`); a future schema bump must propagate through the daemon variant, the CLI emitter, and this docs block as one atomic change.

`covenant memory backfill-receipt-correlation [--dry-run] [--json]` emits a versioned-schema envelope describing the legacy memory-record-to-receipt correlation backfill pass. Sibling to `settlement.backfill.v1` above — both use the `covenant.<area>.backfill.v<n>` convention and both share the `--scope-pubkey` reservation. The structural diff is the rollback channel: settlement uses a **filesystem** rollback file (`rollback_path`), memory uses a **SQLite SAVEPOINT** identifier (`savepoint_name`) so a future mutator can `ROLLBACK TO SAVEPOINT` within the same DB transaction.

Envelope shape:

- `schema`: literal string `"covenant.memory.backfill.v1"`. Same versioning semantics as `covenant.settlement.backfill.v1` — route on the full literal, not the prefix.
- `row_count` (u64): count of memory records the correlation pass operated on (mutation path) or *would* operate on (dry-run path). May legitimately be `0` when no legacy rows match.
- `savepoint_name` (string): SQLite SAVEPOINT identifier the daemon emitted for this pass. **Always a non-null string** — the field type at `memory_backfill_json` (`main.rs:4571`) is `&str`, not `Option<&str>`, so even a dry-run call returns a real savepoint name (the daemon allocates one so consumers can correlate planning runs against later mutation runs). JSON consumers must not write null-vs-value branching for this field; treat absence as a protocol violation. This is the only field-shape difference from `settlement.backfill.v1`, whose sibling `rollback_path` is string-or-null.
- `dry_run` (bool): echoes the `--dry-run` CLI flag. Same semantics as `settlement.backfill.v1`'s `dry_run` — `true` is a planning preview, `false` is a real mutation pass.

**Verb-name asymmetry**: the CLI verb is the long form `memory backfill-receipt-correlation`, **not** `memory backfill` or `memory backfill-receipts` (which would mirror the settlement sibling's shorter name). The match arm is at `main.rs:2350`; the shorter spellings do not parse and return an `unknown flag` bail. JSON consumers driving the CLI from a wrapper must hard-code the long verb token.

**`--scope-pubkey` is reserved, not yet wired**: same caveat as `settlement.backfill.v1`. The CLI accepts the flag and forwards it through `Request::BackfillMemoryRecords.scope_pubkey` (`main.rs:2359-2364`, `main.rs:2373`), but the daemon-side filter is not yet implemented (see the help text at `main.rs:1454` and the file-header CLI summary at `main.rs:12`). This will change when the approved `memory-record-receipt-backfill-mutation` slice lands.

Top-level keys are pinned by the test at `agent-os/crates/covenant/src/main.rs:5613` (`memory_backfill_json_pins_top_level_schema`), exercised against a dry-run shape, a mutation shape, and an empty-rows dry-run shape; the test also asserts the literal `"covenant.memory.backfill.v1"` schema string and the always-non-null, always-non-empty `savepoint_name` contract documented above.

The envelope source-of-truth lives at `memory_backfill_json` in `agent-os/crates/covenant/src/main.rs:4571`. Two unit tests at `main.rs:5598` (`memory_backfill_json_renders_stable_shape`) and `main.rs:5613` cover the shape. The CLI verb is wired at `main.rs:2350-2401` (the `memory backfill-receipt-correlation` arm under the `memory` subcommand); without `--json`, the same response prints `row_count: <N>`, `dry_run: <bool>`, and `savepoint_name: <name>` on three separate lines at `main.rs:2393-2395`. The daemon-side `Response::MemoryRecordsBackfilled` variant carries the three fields directly (`main.rs:2378-2382`); a future schema bump must propagate through the daemon variant, the CLI emitter, and this docs block as one atomic change.

## Human Authority

The decision to bump the IPC/HTTP protocol, the wire shapes that change, the migration window, and the public release notes for v2 remain human-owned. Automation keeps this contract documented and validated; with the v2 `StreamEnvelope` fixtures landed under ADR 0010, the validator now runs in strict mode rather than dormant. It must not introduce v2 fixtures, edit `PROTOCOL_VERSION`, or relax the migration-note pairing without an approved decision.
