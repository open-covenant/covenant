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

The verb-source-of-truth lives in the CLI emitters: `register_agent_confirmed_json` and `register_agent_timeout_json` at `agent-os/crates/covenant/src/main.rs:679` and `:696`, `stake_confirmed_json` and `stake_timeout_json` at `:879` and `:900`, `buy_credits_confirmed_json` and `buy_credits_timeout_json` at `:1187` and `:1206`. Six unit tests at `main.rs:10061`, `:10082`, `:10473`, `:10492`, `:10847`, `:10864` pin the kind strings, and six sibling `*_pins_top_level_schema` tests at `main.rs:10100`, `:10150`, `:10511`, `:10573`, `:10881`, `:10937` assert the full documented top-level key set so an undocumented field added to any helper fails review.

## CLI Read Envelopes

A separate family of `--json` envelopes covers read-side chain queries and most other CLI surfaces. The section is **structurally mixed**: two discriminator subfamilies coexist.

- **Unversioned `kind` subfamily.** The older shape — every envelope below carries a top-level `kind` string (e.g., `"chain_status"`, `"peer_list"`) with no `.v1` suffix. This subfamily predates the schema-suffix convention and is kept stable by unit-test shape invariants — every entry has a `*_pins_top_level_schema` test plus a `*_renders_stable_shape` test that forces docs/emitter drift to surface in review.
- **Versioned `covenant.<area>.<verb>.v<n>` schema subfamily.** The newer shape — these envelopes carry a top-level `schema` string (e.g., `"covenant.settlement.backfill.v1"`, `"covenant.memory.backfill.v1"`) with a `.v<n>` version slot, and they do **not** carry a `kind` field. A future `.v2` envelope is a separate shape, not a field rename inside the existing `.v1` envelope. Every envelope in this subfamily is now anchored by a `*_pins_top_level_schema` test in the same style as the kind-subfamily envelopes; a refactor that drops a key or renames the schema literal will fail the test rather than silently drift the wire shape.

The two subfamilies are **mutually exclusive** at the top level: a `kind`-subfamily envelope never carries `schema`, and a `schema`-subfamily envelope never carries `kind`. Consumers must inspect which discriminator key is present before routing — a defensive parser that reads only one will misclassify envelopes from the other subfamily. The blocks below note which discriminator each envelope uses in the per-envelope shape table.

In addition to the per-envelope `*_pins_top_level_schema` unit tests, the docs/emitter symmetry across every envelope literal in this section is enforced by `agent-os/scripts/validate-cli-envelope-docs.mjs`. The validator fails if any listed envelope kind or schema literal appears in only one of the two surfaces (this document vs. the CLI emitter at `agent-os/crates/covenant/src/main.rs`); the kinds-array comment in the validator documents the maintenance contract.

`covenant chain status --json` emits:

- `kind`: literal string `"chain_status"`. Pinned at the value level by `main.rs:8142` (asserts `value["kind"].as_str() == Some("chain_status")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `status`: a structured `covenant_ipc::ChainStatus` object with the following fields. The top-level object has exactly two keys (`kind` and `status`); the inner `status` is pinned by the schema test at `main.rs:8143-8146` to be a JSON object, never a string blob.

The inner `ChainStatus` shape, defined at `agent-os/crates/covenant-ipc/src/lib.rs:42`:

- `chain` (string) — chain family identifier, currently `"solana"`.
- `cluster` (string) — named cluster (`devnet`, `testnet`, `mainnet-beta`, `localnet`, or a custom alias).
- `rpc_url` (string | null) — resolved RPC endpoint, null when not configured.
- `ws_url` (string | null) — resolved websocket endpoint, null when not configured.
- `program_id` (string | null) — base58 settlement program ID, null when not configured.
- `covnt_mint` (string | null) — base58 COVNT mint pubkey, null when not configured.
- `ready` (bool) — true when every required config field is present.
- `missing` (array of strings) — names of the absent config fields when `ready` is false; an empty array when `ready` is true.

The envelope source-of-truth lives at `chain_status_json` in `agent-os/crates/covenant/src/main.rs:5475`. Two unit tests at `main.rs:8104` (`chain_status_json_renders_stable_shape`) and `main.rs:8126` (`chain_status_json_pins_top_level_schema`) enforce the top-level key set verbatim; the second test's failure message names this document as the forcing function for docs/emitter drift.

`covenant verify --json` emits a cross-check report comparing the audit log against memory and receipt rows. Envelope shape:

- `kind`: literal string `"verify_report"`. Pinned at the value level by `main.rs:8214` (asserts `value["kind"].as_str() == Some("verify_report")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `window` (u64): the audit-window record count echoed back from the `--window` argument. Pinned as u64 by `main.rs:8215-8218` — never a string.
- `checks` (array of `VerifyCheck`): per-check results, see below. Pinned as an array by `main.rs:8223-8226` — never null or a string.
- `drift` (array of `VerifyDrift`): correlation gaps, see below. Pinned as an array by `main.rs:8227` — never null or a string blob.
- `orphans_total` (u64): total number of unmatched rows the checks discovered. Pinned as u64 by `main.rs:8219-8222` — never a string-of-integer.

Top-level keys are pinned to exactly these five by the test at `agent-os/crates/covenant/src/main.rs:8198` (`verify_report_json_pins_top_level_schema`).

`VerifyCheck` shape, defined at `agent-os/crates/covenant-ipc/src/lib.rs:26`:

- `name` (string) — human-readable check name (e.g., `"memory audit"`).
- `passed` (bool) — whether the check passed.
- `message` (string) — diagnostic message (empty when the check passed cleanly).

`VerifyDrift` shape, defined at `agent-os/crates/covenant-ipc/src/lib.rs:33`:

- `kind` (string) — drift category (e.g., `"memory_without_audit"`).
- `id` (string, omitted when null) — record identifier when the drift entry binds to a specific row. Serialized via `#[serde(default, skip_serializing_if = "Option::is_none")]` at `covenant-ipc/src/lib.rs:35-36`, so absent rather than `null` when unbound.
- `message` (string) — drift description.
- `repair` (string) — operator-facing remediation hint.

The envelope source-of-truth lives at `verify_report_json` in `agent-os/crates/covenant/src/main.rs:5482`. The shape-pinning test at `main.rs:8198-8244` covers both the populated and empty cases (`assert_shape` runs against a one-check, one-drift report and an all-empty report).

`covenant tools list --json` emits the registered MCP-style tool catalog. Envelope shape:

- `kind`: literal string `"tool_list"` (singular `tool_list`, not `tools_list`; consumers routing on `kind` must match the literal exactly). Pinned at the value level by `main.rs:7960` (asserts `value["kind"].as_str() == Some("tool_list")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `tools` (array of `ToolSpec`): the registered tools the daemon advertises via `tools/list`. The array is empty when no tools are registered; the unsuffixed CLI prints `(no tools registered)` for that case at `main.rs:3805`. Pinned as an array by `main.rs:7961-7964` — never null or a string blob.

The inner `ToolSpec` shape, defined at `agent-os/crates/covenant-mcp/src/lib.rs:27`:

- `name` (string) — tool identifier.
- `description` (string) — human-readable tool summary.
- `inputSchema` (object) — JSON Schema for the tool's `arguments` object; an empty object means the tool takes no arguments.

`ToolSpec` carries `#[serde(rename_all = "camelCase")]` (`covenant-mcp/src/lib.rs:26`) so the Rust field `input_schema` serializes on the wire as `inputSchema`. The naming matches the MCP wire format; JSON consumers must deserialize using `inputSchema`, not `input_schema`.

Top-level keys are pinned to exactly these two by the test at `agent-os/crates/covenant/src/main.rs:7944` (`tool_list_json_pins_top_level_schema`), which exercises both a populated single-tool case and an empty list.

The envelope source-of-truth lives at `tool_list_json` in `agent-os/crates/covenant/src/main.rs:5447`. Two unit tests at `main.rs:7920` (`tool_list_json_renders_stable_shape`) and `main.rs:7944` cover both cases. The CLI verb is wired at `main.rs:3119-3145`; without `--json`, the same response prints one line per tool in the form `<name> — <description>` at `main.rs:3808`.

`covenant tools call <name> [--args <json>] --json` emits the tool invocation result. Envelope shape:

- `kind`: literal string `"tool_result"` (singular, not `tools_result`; consumers routing on `kind` must match the literal exactly). Pinned at the value level by `main.rs:8020` (asserts `value["kind"].as_str() == Some("tool_result")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `name` (string): the tool name echoed back from the CLI argument. Pinned as a string by `main.rs:8021` — never an object or array.
- `content` (array of `Content`): the tool's output blocks. Each element is a tagged-enum object whose `type` discriminator selects the variant — `{type: "text", text: <string>}` for textual output or `{type: "json", value: <JSON>}` for structured output. The variants are defined at `agent-os/crates/covenant-mcp/src/lib.rs:39` with `#[serde(tag = "type", rename_all = "camelCase")]`; v0 ships text and json variants only. The array is empty when the tool produced no output blocks; the unsuffixed CLI prints each block sequentially at `main.rs:3186-3192`. Pinned as an array by `main.rs:8022-8025` — never null or a string.
- `is_error` (boolean): `true` when the tool itself raised; pinned as a JSON boolean by the schema test (`main.rs:8026-8029`) — never `0`/`1` or a string. JSON consumers must branch on this boolean, not on the presence/absence of content. `is_error=true` paired with non-empty `content` describes a partial-success outcome with an error indicator.

Top-level keys are pinned to exactly these four by the test at `agent-os/crates/covenant/src/main.rs:8004` (`tool_result_json_pins_top_level_schema`), exercised against both a non-empty content + is_error=true case and an empty content + is_error=false case.

The envelope source-of-truth lives at `tool_result_json` in `agent-os/crates/covenant/src/main.rs:5454`. Two unit tests at `main.rs:7983` (`tool_result_json_renders_stable_shape`) and `main.rs:8004` cover the shape. The CLI verb is wired at `main.rs:3146-3192`; without `--json`, each `Content::Text` block prints its `text` directly and each `Content::Json` block prints its `value` as pretty-printed JSON.

`covenant chain flush-receipts --json` emits a receipt-batch summary when it groups local settlement receipts into a single Solana receipt-root transaction. Envelope shape:

- `kind`: literal string `"receipt_batch_flushed"`. Pinned at the value level by `main.rs:8282` (asserts `value["kind"].as_str() == Some("receipt_batch_flushed")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `limit` (u64): the batch-size cap echoed back from the `--limit` argument. Pinned as u64 by `main.rs:8283-8286` — never a string.
- `receipts_updated` (u64): the number of local receipt rows updated to point at the new batch. Pinned as u64 by `main.rs:8287-8290` — never a string-of-integer.
- `batch` (`ReceiptBatchSummary` object): the batch's wire shape, see below. Pinned as a structured object by `main.rs:8291-8294` — never a string blob.

Top-level keys are pinned to exactly these four by the test at `agent-os/crates/covenant/src/main.rs:8266` (`flush_receipts_json_pins_top_level_schema`).

`ReceiptBatchSummary` shape, defined at `agent-os/crates/covenant-ipc/src/lib.rs:54`:

- `batch_id` (string) — opaque batch identifier.
- `merkle_root` (string, 64 hex characters) — Merkle root over the included receipts.
- `receipt_count` (u32) — number of receipts in the batch (note u32, not u64).
- `tx_sig` (string or null) — base58 Solana transaction signature once the batch confirms; null before submission completes.
- `slot` (u64 or null) — confirmation slot once available; null until then.

The envelope source-of-truth lives at `flush_receipts_json` in `agent-os/crates/covenant/src/main.rs:5497`. Two unit tests at `main.rs:8247` (`flush_receipts_json_renders_stable_shape`) and `main.rs:8266` (`flush_receipts_json_pins_top_level_schema`) cover both the unconfirmed (`tx_sig`/`slot` null) and confirmed (both present) batch states.

`covenant chain receipt-batches --json` emits the list of recent receipt batches recorded on-chain. Envelope shape:

- `kind`: literal string `"receipt_batch_list"`. Pinned at the value level by `main.rs:8080` (asserts `value["kind"].as_str() == Some("receipt_batch_list")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `limit` (u64): the result cap echoed back from the `--limit` argument. Pinned as u64 by `main.rs:8081-8084` — never a string.
- `batches` (array of `ReceiptBatchSummary`): the batches, in the order returned by the daemon. Each item uses the same `ReceiptBatchSummary` shape documented above (including the `tx_sig`/`slot` null convention for batches whose settlement transaction has not yet confirmed). The array may be empty. Pinned as an array by `main.rs:8085-8088` — never null or a string.

Top-level keys are pinned to exactly these three by the test at `agent-os/crates/covenant/src/main.rs:8064` (`receipt_batch_list_json_pins_top_level_schema`).

The envelope source-of-truth lives at `receipt_batch_list_json` in `agent-os/crates/covenant/src/main.rs:5467`. Two unit tests at `main.rs:8046` (`receipt_batch_list_json_renders_stable_shape`) and `main.rs:8064` (`receipt_batch_list_json_pins_top_level_schema`) cover the populated and empty cases.

`covenant receipts recent [-n|--limit <N>] [--since-ms <M>] --json` emits a window of local settlement receipts. Envelope shape:

- `kind`: literal string `"receipt_list"` — verb-name asymmetry: the CLI verb is `recent` but the envelope discriminator is `receipt_list` (singular `receipt_`, not `receipts_`); consumers routing on `kind` must match the literal exactly rather than reusing the verb token or pluralising. Pinned at the value level by `main.rs:6478` (asserts `value["kind"].as_str() == Some("receipt_list")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `limit` (u64): the request limit echoed back from `-n`/`--limit` (default `10`, per `main.rs:2842`). Pinned at the type level by the schema test (`main.rs:6479-6482`) — never a string.
- `since_ms` (u64 or null): the Unix-epoch millisecond threshold echoed from `--since-ms`, or `null` when the flag was omitted. Pinned as u64-or-null at the schema test (`main.rs:6483-6486`) — never a string-of-integer. Filter semantics live with the daemon's `Request::RecentReceipts` handler; this surface only echoes the operator's input.
- `receipts` (array of `SettlementReceipt`): the matched receipts in the order returned by the daemon. The array is empty when no receipts fall in the window; the unsuffixed CLI prints `(no receipts)` for that case at `main.rs:2872`. Pinned as an array by `main.rs:6487-6490` — never null or a string.

The inner `SettlementReceipt` shape, defined at `agent-os/crates/covenant-types/src/lib.rs:392`:

- `id` (string) — receipt UUID, serialized as the canonical hyphenated string form.
- `payer` (object) — `{display: string, pubkey: string (base58)}` per the `AgentId` Serialize impl at `covenant-types/src/lib.rs:177`.
- `resource` (string) — `ResourceKind` slug, exactly one of `"compute"`, `"memory"`, `"tool"`, `"message"`, `"registration"` (lowercase per `#[serde(rename_all = "lowercase")]` at `covenant-types/src/lib.rs:35`). Consumers must route on the lowercase wire form, **not** the Rust enum names (`"Compute"`, `"Memory"`, etc.) — those never appear on the wire.
- `memory_record_id` (string, omitted when null) — record identifier when the receipt settled a memory write. Serialized via `#[serde(default, skip_serializing_if = "Option::is_none")]` at `covenant-types/src/lib.rs:396-397` — so **absent rather than null** when unbound. This is the single asymmetry among the Option fields: every other optional field below carries `#[serde(default)]` without `skip_serializing_if`, so those keys are **always emitted** (as `null` when absent). JSON consumers must check `memory_record_id` with key-existence, not null-vs-value.
- `credits_consumed` (u64) — USD-pegged credits destroyed at this event.
- `settled_at` (u64) — Unix-epoch milliseconds when the receipt was issued locally.
- `chain` (string or null) — chain family identifier (e.g. `"solana"`) once the receipt has been batched and confirmed on-chain; `null` until then. Always present on the wire.
- `cluster` (string or null) — named cluster (e.g. `"devnet"`); `null` until on-chain confirmation. Always present on the wire.
- `batch_id` (string or null) — opaque receipt-batch identifier once the receipt is included in a batch; `null` until then. Always present on the wire.
- `merkle_root` (string or null) — 64-hex Merkle root of the batch the receipt was included in; `null` until then. Always present on the wire.
- `tx_sig` (string or null) — base58 Solana transaction signature once the batch confirms; `null` until then. Always present on the wire.
- `slot` (u64 or null) — confirmation slot once available; `null` until then. Always present on the wire.
- `confirmed_at` (u64 or null) — Unix-epoch milliseconds when the on-chain transaction confirmed; `null` until then. Always present on the wire.
- `onchain_sig` (string or null) — backwards-compatible alias for `tx_sig` (per the struct doc-comment at `covenant-types/src/lib.rs:388-390`) that older clients still consume; new consumers should prefer `tx_sig`. Always present on the wire. Both fields carry the same value once the receipt confirms; the unsuffixed CLI's `(local-only)` fallback at `main.rs:3555-3558` reads `tx_sig` first and falls back to `onchain_sig` for exactly that reason.

Top-level keys are pinned to exactly these four by the test at `agent-os/crates/covenant/src/main.rs:6462` (`receipt_list_json_pins_top_level_schema`), exercised against three cases: populated with `since_ms`, populated without `since_ms`, and empty without `since_ms`.

The envelope source-of-truth lives at `receipt_list_json` in `agent-os/crates/covenant/src/main.rs:5259`. Two unit tests at `main.rs:6421` (`receipt_list_json_renders_stable_shape`) and `main.rs:6462` cover the shape. The CLI verb is wired at `main.rs:2837-2890`; without `--json`, each receipt is printed as `[<settled_at>] <resource>: <credits> credits — <onchain>` at `main.rs:2880-2883`, with `<onchain>` resolving to the `tx_sig`/`onchain_sig` value or the literal `(local-only)` when both are null.

`covenant ping --json` emits a daemon-liveness probe. Envelope shape:

- `kind`: literal string `"daemon_ping"`. Pinned at the value level by `main.rs:6842` (asserts `value["kind"].as_str() == Some("daemon_ping")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `status`: literal string `"ok"` — the daemon only returns this envelope when it has accepted the request and produced a `Response::Pong`; failures surface as a non-zero CLI exit rather than a non-`"ok"` payload, so consumers can branch on transport success alone. Pinned as a string by `main.rs:6843-6846` — never an integer or boolean. The literal value `"ok"` is also pinned at the value level by `main.rs:6847` (asserts `value["status"].as_str() == Some("ok")`), so a future status-rename fails the test rather than silently rewriting the literal.

Top-level keys are pinned to exactly these two by the test at `agent-os/crates/covenant/src/main.rs:6828` (`ping_json_pins_top_level_schema`).

The envelope source-of-truth lives at `ping_json` in `agent-os/crates/covenant/src/main.rs:5289`. The shape-pinning tests at `main.rs:6821` (`ping_json_renders_stable_shape`) and `main.rs:6828` cover the single emitted shape; the CLI verb is wired at `main.rs:1983-2005` (the unsuffixed `covenant ping` prints `pong` instead).

`covenant intent [--json] [--stream] <text>` emits the dispatched intent's outcome with optional settlement evidence. Envelope shape:

- `kind`: literal string `"intent_result"`. Pinned at the value level by `main.rs:6773` (asserts `value["kind"].as_str() == Some("intent_result")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `intent_id` (string): the dispatched intent's UUID, serialized as the canonical hyphenated string form. Pinned as a string by the schema test (`main.rs:6774-6777`) — never a byte array or struct.
- `status` (string): the outcome status (e.g., `"ok"`). The string shape is pinned by `main.rs:6778-6781`; specific value enumeration lives with the daemon's intent dispatcher rather than this docs surface.
- `text` (string): the result text the daemon returned. The unsuffixed CLI prints this value directly at `main.rs:2075` (a single-line `println!("{text}")`), so `covenant intent --json` and `covenant intent` share the result payload but only `--json` wraps it in the envelope. Pinned as a string by `main.rs:6782` — never an object or array.
- `sources` (array of strings): source labels that contributed to the result (e.g., `["research"]`). Pinned as an array of strings by `main.rs:6783-6786` — never a comma-joined string. Empty when no sources are attached.
- `settlement` (object or null): an optional `SettlementReceipt` (defined at `agent-os/crates/covenant-types/src/lib.rs:392`) carrying the on-chain or local settlement evidence when the intent consumed credits. `null` when the intent did not settle (e.g., a phase-0 echo that does not charge). Pinned as object-or-null by `main.rs:6787-6790` — never an integer or array.

Top-level keys are pinned to exactly these six by the test at `agent-os/crates/covenant/src/main.rs:6750` (`intent_result_json_pins_top_level_schema`), exercised against both a populated `Some(SettlementReceipt)` case and an empty unsettled case.

The envelope source-of-truth lives at `intent_result_json` in `agent-os/crates/covenant/src/main.rs:5272`. Two unit tests at `main.rs:6732` (`intent_result_json_renders_stable_shape`) and `main.rs:6750` cover the shape. The CLI verb is wired at `main.rs:2006-2080`; the `--json`/`--stream` flags are recognized only in leading position (`main.rs:2019-2028`) so an interior `--json` token is preserved as part of the intent text. The optional `--stream` flag sets `Request::SubmitIntent.prefer_stream = Some(true)` (`main.rs:2717`), enabling the v2 streaming-response path documented under [docs/protocol-versioning.md](./protocol-versioning.md); the terminal `IntentResult` envelope shape is unchanged when the streaming path is not selected.

`GET /intents/:id/events` (HTTP-only) opens a `text/event-stream` connection that emits one frame per agent runtime event whose `intent_id` matches the path parameter. Frames carry an `AgentEvent` JSON object (`#[serde(tag = "type", rename_all = "snake_case")]`, defined in `covenant-types/src/lib.rs`) — the public taxonomy with four variants `reasoning`, `tool_call`, `tool_result`, and `file_write`. Runner-side `RuntimeTrace` rows are projected into this taxonomy at the SSE boundary (see `impl From<&RuntimeTrace> for AgentEvent` in `covenant-runtime/src/lib.rs`) so the wire form survives runner swaps without breaking browser clients. The `reasoning` slot is reserved: today the Hermes adapter drops reasoning frames at the SSE seam because they are too high-volume to audit, so no runner emits this variant yet — the slot is wire-compatible so a future runtime can stream condensed reasoning summaries without breaking older clients. Approval frames surface as `tool_call` / `tool_result` with `tool = "approval"`; the audit chain keeps the durable `hermes_approval_requested` / `hermes_approval_resolved` rows for the runner-specific record. The endpoint replaces the 3-second poll on `/intents/:id/result` for the live trace view — a browser opens one `EventSource` per intent page and renders each frame the moment the daemon flushes it. The web client caps the rendered step list at the most recent 200 entries (with an explicit operator opt-in to expand to the full set) so a long-running run does not stall the React reconciler on slow machines; the cap matches the audit fetch `limit=200`, so widening one without the other would silently drop history at the seam. The endpoint subscribes to a daemon-side `tokio::sync::broadcast` fan-out populated by `spawn_runtime_event_drainer` in `agent-os/crates/covenantd/src/lib.rs:524`; the channel exists unconditionally, but the drainer that publishes to it only runs when `COVENANT_LIVE_TRACE=1`, so the endpoint streams nothing until the operator opts into live tracing. Audience model mirrors `/intents/:id/result`: any authenticated peer can stream any intent. Slow subscribers that fall behind the broadcast capacity drop the lagged window and keep streaming; the audit chain is the durable record. Reconnects do not replay missed frames — a client that needs the durable history fetches `/audit/recent` instead.

`covenant capabilities recent [-n|--limit <N>] --json` emits a peer-scoped view of recent signed capabilities. Envelope shape:

- `kind`: literal string `"capability_list"` — verb-name asymmetry: the CLI verb is `recent` but the envelope discriminator is `capability_list`. Consumers routing on `kind` must match the latter literal exactly rather than reusing the verb token. Pinned at the value level by `main.rs:6907` (asserts `value["kind"].as_str() == Some("capability_list")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `limit` (u64): the request limit echoed back from `-n`/`--limit` (default `10`, see `main.rs:3253`). Pinned at the type level by the schema test (`main.rs:6908-6911`) — JSON consumers must never receive a string here.
- `capabilities` (array of `SignedCapability`): the filtered live capabilities. Each element has shape `{capability: Capability, signature: <base58>}` where `Capability` is defined at `agent-os/crates/covenant-types/src/lib.rs:224` (fields: `subject`, `action`, `scope`, `granted_by`, `expires_at`) and `SignedCapability` is defined at `agent-os/crates/covenant-permissions/src/lib.rs:58`. The `signature` field is the base58 encoding of the 64-byte ed25519 signature (per the `sig_b58` serde module at `lib.rs:64-84`), never the raw byte array. Pinned as an array by `main.rs:6912-6915` — never null or a string.

The daemon applies a **peer-visibility filter** before returning the list (see `recent_capabilities` at `agent-os/crates/covenantd/src/lib.rs:12478-12494`): only capabilities whose `subject.pubkey` or `granted_by.pubkey` matches the requesting peer's pubkey are included. JSON consumers must not assume this is a global registry dump — operator and delegated callers see a different slice of the same store.

Top-level keys are pinned to exactly these three by the test at `agent-os/crates/covenant/src/main.rs:6891` (`capability_list_json_pins_top_level_schema`), which exercises both a populated single-capability case and an empty list.

The envelope source-of-truth lives at `capability_list_json` in `agent-os/crates/covenant/src/main.rs:5296`. Two unit tests at `main.rs:6851` (`capability_list_json_renders_stable_shape`) and `main.rs:6891` cover both cases. The CLI verb is wired at `main.rs:2580-2636`; without `--json`, the same response prints one line per capability in the form `<subject_display> → <action_label> (<granted_by_display>) [<expiry>]` at `main.rs:3296-3302`, or `(no capabilities granted)` when the filtered list is empty.

`covenant capabilities grant <action> [--scope <json>] [--expires-at <ms>] --json` emits the freshly-signed capability after the daemon accepts the grant. Envelope shape:

- `kind`: literal string `"capability_granted"` — past-tense outcome name, distinct from the verb name `grant`; consumers routing on `kind` must match the literal exactly rather than reusing the verb token. Pinned at the value level by `main.rs:6980` (asserts `value["kind"].as_str() == Some("capability_granted")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `subject_display` (string): the daemon-synthesized human-readable subject (e.g., `operator@local`). The daemon owns this field — consumers must not reconstruct it from the request. Pinned as a string by `main.rs:6981-6984` — never an object or array.
- `action` (string): the action the capability was granted for. **Not always the verbatim CLI argument**: when the CLI receives an a2a peer-prefix shorthand it expands the prefix to the full peer-bound action before signing (see `expand_a2a_action` invoked at `main.rs:2669-2702`); the envelope reports the post-expansion full form, and the unsuffixed CLI prints an `expanding <prefix> → <full>` line to stderr at `main.rs:3364`. Pinned as a string by `main.rs:6985-6988` — never an object or array.
- `signature_b58` (string): the base58 signature over the signed-capability bytes. This is the same value consumers pass back to `covenant capabilities revoke <signature-b58>` to tombstone the capability. Pinned as a string by `main.rs:6989-6992` — never an object or array.
- `scope` (object or null): the structured scope object echoed from the request, or `null` when `--scope` was omitted. Pinned at the type level by the schema test (`main.rs:6993-6996`) — JSON consumers must never receive a string blob here, so a scope value of `"{\"version\":1}"` would be a contract break.
- `expires_at` (u64 or null): the Unix-epoch millisecond expiry echoed from `--expires-at`, or `null` when the flag was omitted. Pinned at the type level by the schema test (`main.rs:6997-7000`) — JSON consumers must never receive a string here, so a value of `"1700000000000"` would be a contract break.

Top-level keys are pinned to exactly these six by the test at `agent-os/crates/covenant/src/main.rs:6957` (`capability_grant_json_pins_top_level_schema`), which also asserts the `scope` object-or-null and `expires_at` u64-or-null typing.

The envelope source-of-truth lives at `capability_grant_json` in `agent-os/crates/covenant/src/main.rs:5304`. Two unit tests at `main.rs:6934` (`capability_grant_json_renders_stable_shape`, covers both a scoped+timed grant and an unscoped+untimed grant) and `main.rs:6957` cover both populated cases. The CLI verb is wired at `main.rs:2638-2730`; without `--json`, the same response prints `granted: <subject> → <action>` followed by the signature on a second line.

`covenant capabilities revoke <signature-b58> --json` emits the outcome of revoking a single signed capability by its signature. Envelope shape:

- `kind`: literal string `"capability_revoked"` — past-tense outcome name, distinct from the verb name `revoke`; consumers routing on `kind` must match the literal exactly rather than reusing the verb token. Pinned at the value level by `main.rs:7050` (asserts `value["kind"].as_str() == Some("capability_revoked")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `signature_b58` (string): the base58 signature echoed back from the request, so consumers can correlate the response to the revoke call without tracking it out of band. Pinned as a string by `main.rs:7051-7054` — never an object or array.
- `removed` (boolean): `true` if a live capability matched and was tombstoned, `false` if no live row matched that signature. Pinned as a JSON boolean by `main.rs:7055-7058` — never `0`/`1` or a string. `false` is a benign no-op outcome, not an error — the daemon still returns `Response::CapabilityRevoked` and the unsuffixed CLI prints `(no live capability with that signature)` for that case at `main.rs:3453`. JSON consumers must not treat `removed=false` as a failure.

Top-level keys are pinned to exactly these three by the test at `agent-os/crates/covenant/src/main.rs:7034` (`capability_revoke_json_pins_top_level_schema`), which also asserts `removed` is a JSON boolean (never `0`/`1` or a string).

The envelope source-of-truth lives at `capability_revoke_json` in `agent-os/crates/covenant/src/main.rs:5321`. Two unit tests at `main.rs:7021` (`capability_revoke_json_renders_stable_shape`) and `main.rs:7034` cover both the `removed=true` and `removed=false` cases. The CLI verb is wired at `main.rs:3843-2786`.

`covenant capabilities purge --json` emits a summary of revoked-capability garbage collection. Envelope shape:

- `kind`: literal string `"capabilities_purged"`. Pinned at the value level by `main.rs:7090` (asserts `value["kind"].as_str() == Some("capabilities_purged")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `before_ms` (u64): the resolved Unix-epoch millisecond cutoff. The CLI accepts either `--before-ms <M>` (echoed verbatim) or `--older-than-ms <D>` (resolved against the system clock as `now - D` per `main.rs:3479-3483`); the envelope always reports the single resolved value, so consumers cannot distinguish which input form the operator typed. Pinned as u64 by `main.rs:7091-7094` — never a string-of-integer.
- `purged` (u64): the count of revoked-capability rows removed. May legitimately be `0` when no rows matched the cutoff — the verb does not error on an empty purge. Pinned as u64 by `main.rs:7095-7098` — never a string-of-integer.

Top-level keys are pinned to exactly these three by the test at `agent-os/crates/covenant/src/main.rs:7074` (`capabilities_purge_json_pins_top_level_schema`).

The envelope source-of-truth lives at `capabilities_purge_json` in `agent-os/crates/covenant/src/main.rs:5329`. Two unit tests at `main.rs:7066` (`capabilities_purge_json_renders_stable_shape`) and `main.rs:7074` (`capabilities_purge_json_pins_top_level_schema`) cover the populated (`purged=3`) and empty (`purged=0`) cases. The CLI verb is wired at `main.rs:3888-2836`; without `--json`, the same response prints `purged <n> revoked capability(ies)`.

`covenant peers list [--limit <N>] [--prefix <P>] [--live-only|--revoked-only] --json` emits the registered peer roster filtered by the supplied flags. Envelope shape:

- `kind`: literal string `"peer_list"`. Pinned at the value level by `main.rs:6036` (asserts `value["kind"].as_str() == Some("peer_list")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `limit` (u64): the request limit echoed back from `--limit` (default `20`, per `main.rs:4390`). Pinned as u64 by `main.rs:6037-6040` — never a string.
- `filter_pubkey_prefix` (string or null): the prefix echoed from `--prefix`, or `null` when the flag was omitted. Pinned at the type level by the schema test (`main.rs:6041-6045`) — never an integer or array.
- `matched_count` (u64): row count of the `peers` array; equals the exhaustive match count when `truncated` is `false`. Pinned as u64 by `main.rs:6046-6049` — never a string.
- `peers` (array of `PeerSummary`): the matched roster slice, see below. Pinned as an array by `main.rs:6050` — never null or a string blob.
- `operator_pubkey_b58` (string): the requesting operator's own pubkey in base58. The unsuffixed CLI line formatter at `peer_list_lines` (`main.rs:4353`) compares each peer's `pubkey_base58()` against this value to append a ` (self)` marker on the operator's own row; JSON consumers must apply the same comparison to render the self-tag, not assume the operator's row is reliably first. Pinned as a string by `main.rs:6051-6054` — never an object or array.
- `truncated` (boolean): `true` when the registry held more matching entries than `limit`, `false` otherwise. Pinned as a JSON boolean by the schema test at `main.rs:6055-6058` — never `0`/`1`. **This is the only signal of incomplete results**; `matched_count == limit` with `truncated == false` means the page is the exhaustive match set, not a hint to paginate.

The inner `PeerSummary` shape, defined at `agent-os/crates/covenant-peer-auth/src/lib.rs:140`:

- `agent_id` (object) — `{display: string, pubkey: string (base58)}` per the `AgentId` Serialize impl at `covenant-types/src/lib.rs:177`.
- `token_prefix` (string) — 6-character redacted token prefix, the same value `peers revoke <token-prefix>` accepts. The full token bytes are never on the wire — same invariant as `Response::PeerList`.
- `registered_at` (u64) — Unix-epoch milliseconds when the peer registered.
- `revoked_at` (u64 or null) — Unix-epoch milliseconds when the peer was tombstoned; `null` for live entries. Composes with the `--live-only`/`--revoked-only` flags (and the equivalent `status_filter` query parameter described above) for filtering — the filter runs before the registry's truncation peek.

Top-level keys are pinned to exactly these seven by the test at `agent-os/crates/covenant/src/main.rs:6012` (`peer_list_json_pins_top_level_schema`), exercised against a populated two-peer (one live, one revoked) case and an empty case.

The envelope source-of-truth lives at `peer_list_json` in `agent-os/crates/covenant/src/main.rs:5165`. Schema and behavioral tests live at `main.rs:6012` (key set + per-key typing), `main.rs:5979` (`peer_list_json_echoes_prefix_and_match_count`), `main.rs:5993` (`peer_list_json_omits_prefix_when_inactive`), and `main.rs:6004` (`peer_list_json_reports_zero_match_count_for_empty_response`). The CLI verb is wired at `main.rs:3719-3772`; without `--json`, the same response is rendered line-by-line by `peer_list_lines` (`main.rs:4353`) with a `(truncated; <n> shown — narrow with --prefix or raise --limit)` hint appended when `truncated` is `true` (`main.rs:4384`). See also the **Query Parameters** section above for the same filter composition rules over the HTTP gateway.

`covenant peers purge --json` emits a summary of revoked-peer garbage collection. Envelope shape:

- `kind`: literal string `"peers_purged"` — the only structural disambiguator from `capabilities_purged`; both envelopes share the same three-key layout, so consumers that route on `kind` must check the full literal rather than treating any `*_purged` envelope as interchangeable. Pinned at the value level by `main.rs:7130` (asserts `value["kind"].as_str() == Some("peers_purged")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `before_ms` (u64): resolved Unix-epoch millisecond cutoff. The CLI accepts `--before-ms` or `--older-than-ms` with the same resolution semantics as `covenant capabilities purge --json` above. Pinned as u64 by `main.rs:7131-7134` — never a string-of-integer.
- `purged` (u64): count of revoked-peer rows removed. Only revoked rows are eligible — the verb does not touch live peers (the unsuffixed CLI prints `purged <n> revoked peer(s)` at `main.rs:3669`). May legitimately be `0` when no rows matched. Pinned as u64 by `main.rs:7135-7138` — never a string-of-integer.

Top-level keys are pinned to exactly these three by the test at `agent-os/crates/covenant/src/main.rs:7114` (`peers_purge_json_pins_top_level_schema`).

The envelope source-of-truth lives at `peers_purge_json` in `agent-os/crates/covenant/src/main.rs:5337`. Two unit tests at `main.rs:7106` (`peers_purge_json_renders_stable_shape`) and `main.rs:7114` cover the populated and empty cases. The CLI verb is wired at `main.rs:3629-3675`.

`covenant peers rotate --json` emits the new operator token after rotation. Envelope shape:

- `kind`: literal string `"peer_token_rotated"`. Pinned at the value level by `main.rs:7169` (asserts `value["kind"].as_str() == Some("peer_token_rotated")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `token_b58` (string): the full base58 operator token. The value is the new authentication credential, not a fingerprint — the envelope is **secret-bearing** and JSON output must be treated as sensitive (no logging, no shell history capture, no transport over unsecured channels). Pinned as a string by `main.rs:7170-7173` — never bytes or a structured object.

Top-level keys are pinned to exactly these two by the test at `agent-os/crates/covenant/src/main.rs:7153` (`peers_rotate_json_pins_top_level_schema`).

Side effects before the envelope returns (per the CLI comment at `main.rs:4366-4372`): the daemon has already persisted the new token to `$COVENANT_HOME/peers/operator.token` (mode `0600`), so the envelope is informational. Existing shells holding the previous token continue to authenticate with the old value until they re-read the file; consumers that cache the token in memory must refresh after rotation.

The envelope source-of-truth lives at `peers_rotate_json` in `agent-os/crates/covenant/src/main.rs:5345`. The shape-pinning tests at `main.rs:7146` (`peers_rotate_json_renders_stable_shape`) and `main.rs:7153` (`peers_rotate_json_pins_top_level_schema`) cover both a typical-token case and an empty-string defensive case (the latter exercises the key-set invariant rather than a legitimate runtime value). The CLI verb is wired at `main.rs:3676-3711`; without `--json`, the same response prints a two-line message terminating in the raw token value.

`covenant peers revoke <token-prefix> [--force] [--limit-matches <N>] --json` emits the outcome of revoking a single peer by its base58 token prefix. Envelope shape:

- `kind`: literal string `"peer_revoke"` — verb-form, not past-tense. Distinct from the sibling envelopes whose outcome names took the past-tense form (`capability_revoked`, `peer_token_rotated`, `peers_purged`); consumers routing on `kind` must match the literal exactly rather than guessing `peer_revoked` or `peers_revoke`. Pinned at the value level by `main.rs:6153` (asserts `value["kind"].as_str() == Some("peer_revoke")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `outcome` (object): a tagged-enum `RevokeOutcome` (defined at `agent-os/crates/covenant-peer-auth/src/lib.rs:182` with `#[serde(tag = "type", rename_all = "snake_case")]`). The top-level object has exactly two keys (`kind` and `outcome`); the inner `outcome` is pinned by the schema test at `main.rs:6154-6157` to be a JSON object, never a string blob.

The five `RevokeOutcome` variants the daemon may return:

- `{type: "revoked", agent_id, token_prefix, registered_at, revoked_at}` — the unique live match was tombstoned. The four extra fields are the inlined `PeerSummary` shape documented in the `peer_list` block above; `revoked_at` carries the moment of revocation and is non-null for this variant.
- `{type: "already_revoked", agent_id, token_prefix, registered_at, revoked_at}` — same inlined `PeerSummary` shape; the unique match was already tombstoned. Idempotent — the operator's intent is satisfied — and `revoked_at` carries the *original* revocation timestamp, not the moment of this call.
- `{type: "not_found"}` — no entry's full base58 token matched the supplied prefix. No extra fields.
- `{type: "ambiguous", matches: [PeerSummary...], truncated: bool}` — more than one entry matched the prefix; the registry is unchanged. `matches.len()` is bounded by `--limit-matches`; `truncated` is `true` when more than that limit matched (see `RevokeOutcome::Ambiguous` at `covenant-peer-auth/src/lib.rs:207-211`). The field carries `#[serde(default)]` so a stale CLI built before `truncated` landed still deserialises a new daemon's response (degrading to the pre-bound assumption that the displayed matches are exhaustive); the daemon-side serializer always writes the field.
- `{type: "self_revoke_forbidden", agent_id, token_prefix, registered_at, revoked_at}` — same inlined `PeerSummary` shape; the unique live match is the operator's own bootstrap row and the request did not pass `--force`. The registry is unchanged and `revoked_at` is `null` (the entry remained live). This is defence-in-depth against the "fat-finger via web UI bypassed by curl" failure mode where a UI-only confirmation guard is trivially circumvented by a direct daemon API call.

**Exit-code coupling**: the `peer_revoke_is_failure` classifier at `agent-os/crates/covenant/src/main.rs:5634-5641` maps `not_found`, `ambiguous`, and `self_revoke_forbidden` to a CLI exit code of `1` — including in the `--json` path (`main.rs:4496-4498`). `revoked` and `already_revoked` map to exit `0`. JSON consumers must branch on `outcome.type` for success/failure semantics; transport success (exit `0`) is **not** synonymous with revocation success. The classifier's mapping is pinned by the test at `main.rs:8599` (`peer_revoke_json_exit_classification_matches_human_cli`).

Top-level keys are pinned to exactly these two by the test at `agent-os/crates/covenant/src/main.rs:6137` (`peer_revoke_json_pins_top_level_schema`), which also asserts `outcome` is a tagged-enum object and exercises both the `Ambiguous` and `NotFound` variants.

The envelope source-of-truth lives at `peer_revoke_json` in `agent-os/crates/covenant/src/main.rs:5572`. Two unit tests at `main.rs:6119` (`peer_revoke_json_renders_stable_ambiguous_shape`) and `main.rs:6137` cover the shape. The CLI verb is wired at `main.rs:3776-3876`; without `--json`, `Revoked` and `AlreadyRevoked` print tab-separated success lines to stdout, while `NotFound`, `Ambiguous`, and `SelfRevokeForbidden` print human-readable diagnostics to stderr before exiting `1`.

`covenant audit recent [-n|--limit <N>] [--since-ms <M>] [--stream] --json` emits a window of audit events. Envelope shape:

- `kind`: literal string `"audit_recent"`. Pinned at the value level by `main.rs:7388` (asserts `value["kind"].as_str() == Some("audit_recent")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `limit` (u64): the request limit echoed back from `-n`/`--limit` (default `50`, per `main.rs:3210`). Pinned as u64 at the schema test (`main.rs:7389-7392`) — never a string.
- `since_ms` (u64 or null): the Unix-epoch millisecond threshold echoed from `--since-ms`, or `null` when the flag was omitted. Pinned as u64-or-null at the schema test (`main.rs:7393-7396`) — never a string-of-integer. Same semantic as the HTTP gateway query parameter described in the **Query Parameters** section above: events whose `timestamp_ms` is strictly less than the threshold are dropped before the limit truncation.
- `events` (array of `AuditEvent`): the matched events. The array is empty when no events fall in the window. Pinned as an array by `main.rs:7397-7400` — never null or a string.

The inner `AuditEvent` shape, defined at `agent-os/crates/covenant-audit/src/lib.rs:43`:

- `id` (string) — event UUID.
- `timestamp_ms` (u64) — Unix-epoch milliseconds when the event was recorded.
- `issuer` (object) — `{display: string, pubkey: string (base58)}` per the `AgentId` Serialize impl at `covenant-types/src/lib.rs:177`.
- `kind` (object) — tagged-enum `AuditKind` (defined at `covenant-audit/src/lib.rs:71` onwards) with a `type` discriminator (e.g., `"capability_granted"`, `"intent_dispatched"`, `"hermes_tool_invoked"`) and variant-specific extra fields. Consumers must route on `kind.type` before reading variant-specific fields.

Top-level keys are pinned to exactly these four by the test at `agent-os/crates/covenant/src/main.rs:7372` (`audit_recent_json_pins_top_level_schema`), exercised against three cases: populated with `since_ms`, empty with `since_ms`, and empty without `since_ms`.

The envelope source-of-truth lives at `audit_recent_json` in `agent-os/crates/covenant/src/main.rs:5367`. Two unit tests at `main.rs:7345` (`audit_recent_json_renders_stable_shape`) and `main.rs:7372` cover the shape. The CLI verb is wired at `main.rs:3209-3279`; without `--json`, the same response is rendered as JSONL (one `AuditEvent` per line at `main.rs:3272`) mirroring the durable `audit/events.jsonl` row shape, with `(no audit events)` printed at `main.rs:3269` when empty. The optional `--stream` flag sets `Request::RecentAudit.prefer_stream = Some(true)` (`main.rs:3239`), enabling the v2 streaming-response path documented under [docs/protocol-versioning.md](./protocol-versioning.md); the terminal-response shape is unchanged when the streaming path is not selected.

`covenant audit purge --json` emits a summary of time-bounded audit-log garbage collection. Envelope shape:

- `kind`: literal string `"audit_purged"`. Pinned at the value level by `main.rs:7329` (asserts `value["kind"].as_str() == Some("audit_purged")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `before_ms` (u64): resolved Unix-epoch millisecond cutoff. The CLI accepts `--before-ms` or `--older-than-ms` with the same resolution semantics as `covenant capabilities purge --json` above. Pinned as u64 by `main.rs:7330-7333` — never a string-of-integer.
- `purged` (u64): count of audit events removed (the unsuffixed CLI message at `main.rs:3339` reads `purged <n> event(s)`, confirming the unit is an audit event, not a row class). May legitimately be `0` when no rows matched. Pinned as u64 by `main.rs:7334-7337` — never a string-of-integer.

Unlike the capability- and peer-purge verbs, this removes hash-chain entries; the cutoff enforcement is bound to the `audit.purge` capability scope at dispatch time so a delegated caller cannot purge beyond its scope's `before_ms` (see `docs/capabilities.md`).

Top-level keys are pinned to exactly these three by the test at `agent-os/crates/covenant/src/main.rs:7313` (`audit_purge_json_pins_top_level_schema`).

The envelope source-of-truth lives at `audit_purge_json` in `agent-os/crates/covenant/src/main.rs:5359`. Two unit tests at `main.rs:7305` (`audit_purge_json_renders_stable_shape`) and `main.rs:7313` cover the populated (`purged=3`) and empty (`purged=0`) cases. The CLI verb is wired at `main.rs:3303-3345`.

`covenant audit verify --json` emits the audit-log hash-chain integrity report. Envelope shape:

- `kind`: literal string `"audit_integrity"` — past-tense outcome name, distinct from the verb name `verify` and from the workspace-level `verify_report` envelope; consumers routing on `kind` must match this literal exactly rather than reusing either of those tokens. Pinned at the value level by `main.rs:7466` (asserts `value["kind"].as_str() == Some("audit_integrity")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `report` (object): a structured `covenant_audit::AuditIntegrityReport`, never a string blob. The top-level object has exactly two keys (`kind` and `report`); the inner `report` is pinned by the schema test at `main.rs:7467-7470` to be a JSON object.

The inner `AuditIntegrityReport` shape, defined at `agent-os/crates/covenant-audit/src/lib.rs:61`:

- `events` (u64) — total audit events the integrity walk visited.
- `anchors` (u64) — count of anchor records (root-hash checkpoints) the walk crossed.
- `valid` (bool) — `true` when the hash chain is intact end-to-end; `false` when one or more failures were recorded.
- `root_hash_hex` (string) — the final root hash as lowercase hex, 64 characters (SHA-256). Pinned at the length level by the stable-shape test at `main.rs:7436-7442`.
- `failures` (array of strings) — human-readable failure descriptions (e.g., `"chain hash mismatch at event 3"`), empty when `valid` is `true`. The empty case is pinned by the stable-shape test at `main.rs:7443-7446` (asserts `as_array().map(Vec::len) == Some(0)`).

Top-level keys are pinned to exactly these two by the test at `agent-os/crates/covenant/src/main.rs:7450` (`audit_verify_json_pins_top_level_schema`), exercised against both a valid and an invalid report.

The envelope source-of-truth lives at `audit_verify_json` in `agent-os/crates/covenant/src/main.rs:5380`. Two unit tests at `main.rs:7422` (`audit_verify_json_renders_stable_shape`) and `main.rs:7450` cover the shape. The CLI verb is wired at `main.rs:3280-3302`; without `--json`, the same response is printed as the bare `AuditIntegrityReport` JSON (no envelope wrapper) at `main.rs:3296`, so JSON consumers must use `--json` to get the kind-discriminated envelope — the unsuffixed output is structurally compatible with `report` but lacks the `kind` field.

`covenant memory purge --json` emits a summary of time-bounded memory-store garbage collection. Envelope shape:

- `kind`: literal string `"memory_purged"`. Pinned at the value level by `main.rs:7521` (asserts `value["kind"].as_str() == Some("memory_purged")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `tier` (string or null): the memory tier slug — exactly one of `"working"`, `"episodic"`, or `"longterm"` (one word, per `memory_tier_slug` at `main.rs:1725-1730`). Null when `--tier` was omitted, meaning the purge applied to all tiers. Note an input-form asymmetry: the CLI parser at `main.rs:1735-1737` accepts `longterm`, `long-term`, and `long_term` for the `--tier` argument, but only the `longterm` slug is ever emitted in the envelope. Pinned as string-or-null by `main.rs:7522-7525` — never a structured object.
- `before_ms` (u64): resolved Unix-epoch millisecond cutoff. Same `--before-ms` / `--older-than-ms` resolution semantics as `covenant capabilities purge --json` above. Pinned as u64 by `main.rs:7526-7529` — never a string-of-integer.
- `purged` (u64): count of memory records removed. The unsuffixed CLI prints `purged <n> record(s)` at `main.rs:2855`, confirming the unit is a memory record. May legitimately be `0` when no rows matched. Pinned as u64 by `main.rs:7530-7533` — never a string-of-integer.

Top-level keys are pinned to exactly these four by the test at `agent-os/crates/covenant/src/main.rs:7505` (`memory_purge_json_pins_top_level_schema`), which also exercises the null-tier case.

The envelope source-of-truth lives at `memory_purge_json` in `agent-os/crates/covenant/src/main.rs:5387`. Two unit tests at `main.rs:7493` (`memory_purge_json_renders_stable_shape`, both a Working-tier populated case and a no-tier null case) and `main.rs:7505` cover the populated and empty (`purged=0`, no-tier) cases. The CLI verb is wired at `main.rs:2126-2182`.

`covenant memory recent [--tier <T>] [-n|--limit <N>] [--stream] --json` and `covenant memory search <query> [--tier <T>] [-n|--limit <N>] [--min-relevance <R>] --json` both emit the same memory-read envelope, distinguished only by the `mode` discriminator. Envelope shape:

- `kind`: literal string `"memory_read"`. Pinned at the value level by `main.rs:7821` (asserts `value["kind"].as_str() == Some("memory_read")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `mode` (string): exactly one of `"recent"` or `"search"` (lowercase, matching the CLI verb name — no other values are emitted). Consumers must route on `mode` to know which null pattern to expect across `query` and `min_relevance`. Pinned as a string by `main.rs:7822` — never an object or array.
- `tier` (string or null): the requested `MemoryTier` as its lowercase wire slug — exactly one of `"working"`, `"episodic"`, or `"longterm"` (one word, per `MemoryTier`'s `#[serde(rename_all = "lowercase")]` at `covenant-types/src/lib.rs:23` and the slug map at `memory_tier_slug` in `main.rs:1725-1730`). The CLI parser accepts `longterm`, `long-term`, and `long_term` as input forms for `--tier`, but only the `longterm` slug is ever emitted. `null` when `--tier` was omitted (meaning the request applied to all tiers). Pinned as string-or-null by the schema test (`main.rs:7827-7830`) — never a structured object.
- `limit` (u64): the request limit echoed back from `-n`/`--limit` (default `10` for both verbs, per `main.rs:2083` and `main.rs:2499`). Pinned as u64 at the schema test (`main.rs:7823-7826`).
- `query` (string or null): for `mode="search"`, the request query (whitespace-joined when the operator passed multiple positional tokens, per `main.rs:2534`). For `mode="recent"`, always `null` (the recent verb does not accept a query). Pinned as string-or-null by the schema test (`main.rs:7831-7834`).
- `min_relevance` (number or null): for `mode="search"`, the float echoed from `--min-relevance` (validated to a finite `f32` in `[0.0, 1.0]` at `main.rs:2522-2526`), or `null` when the flag was omitted. For `mode="recent"`, always `null`. Pinned as f64-or-null by the schema test (`main.rs:7835-7838`) — never a string.
- `records` (array of `MemoryRecord`): the matched records in the order returned by the daemon. The array is empty when no records match; the unsuffixed CLI prints `(no records)` for that case at `main.rs:1631`. Pinned as an array by `main.rs:7839-7842` — never null or a string.

The inner `MemoryRecord` shape, defined at `agent-os/crates/covenant-types/src/lib.rs:236`:

- `id` (string) — record UUID, serialized as the canonical hyphenated string form.
- `tier` (string) — lowercase `MemoryTier` slug (same enumeration as the top-level `tier` above; always present, never null).
- `owner` (object) — `{display: string, pubkey: string (base58)}` per the `AgentId` Serialize impl at `covenant-types/src/lib.rs:177`.
- `text` (string) — the stored memory text.
- `embedding` (array of numbers) — the record's embedding vector as a JSON array of f32 values. The array is always present (empty when no embedding was attached); consumers must not assume the field is omitted.
- `metadata` (JSON value) — an arbitrary `serde_json::Value` (object, array, primitive, or null). The daemon emits whatever metadata the writer attached; consumers must not assume an object shape.
- `created_at` (u64) — Unix-epoch milliseconds when the record was written.
- `parent` (string or null) — parent record UUID for derived memories. Carries `#[serde(default)]` at `covenant-types/src/lib.rs:245-246` **without** `skip_serializing_if`, so the field is **always emitted** (as `null` when the record has no parent), not omitted. JSON consumers must read it with null-vs-value, not key-existence.

Top-level keys are pinned to exactly these seven by the test at `agent-os/crates/covenant/src/main.rs:7797` (`memory_read_json_pins_top_level_schema`), exercised against both a `mode="search"` case (populated `query`, `min_relevance`, non-empty `records`) and a `mode="recent"` case (null `query`, null `min_relevance`, empty `records`).

The envelope source-of-truth lives at `memory_read_json` in `agent-os/crates/covenant/src/main.rs:5415`. Two unit tests at `main.rs:7754` (`memory_read_json_renders_stable_shape`) and `main.rs:7797` cover both modes. The CLI verbs are wired at `main.rs:2081-2125` (`covenant memory recent`) and `main.rs:2493-2559` (`covenant memory search`); without `--json`, each record prints as `[<created_at>] <tier>: <text>` at `main.rs:1635`. The optional `--stream` flag is accepted only by `covenant memory recent` (per `main.rs:2100`) and sets `Request::RecentMemory.prefer_stream = Some(true)` to enable the v2 streaming-response path documented under [docs/protocol-versioning.md](./protocol-versioning.md); the terminal envelope shape is unchanged when the streaming path is not selected. `covenant memory search` has no `--stream` flag.

`covenant a2a status [-n|--limit <N>] [--min-lease-age-ms <N>] [--deadline-within-ms <N>] [--state queued|in_flight] --json` emits the current A2A queue snapshot — queued tasks, in-flight leases, and pending results — narrowed by the supplied filters. Envelope shape:

- `kind`: literal string `"a2a_status"`. Pinned at the value level by `main.rs:8392` (asserts `value["kind"].as_str() == Some("a2a_status")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `limit` (u64): the request limit echoed back from `-n`/`--limit` (default `10`, per `main.rs:4039`). Pinned as u64 by the schema test (`main.rs:8393-8396`).
- `min_lease_age_ms` (u64 or null): the threshold echoed from `--min-lease-age-ms`, or `null` when the flag was omitted. Always emitted (as `null` when inactive) — never omitted from the envelope. Pinned as u64-or-null by the schema test (`main.rs:8397-8400`).
- `deadline_within_ms` (u64 or null): the threshold echoed from `--deadline-within-ms`, or `null` when the flag was omitted. Same always-emitted-as-null contract as `min_lease_age_ms`. Pinned as u64-or-null by the schema test (`main.rs:8401-8404`).
- `state_filter` (string or null): the `A2ATaskQueueState` slug echoed from `--state` — exactly `"queued"` or `"in_flight"` (snake_case, per `A2ATaskQueueState`'s `#[serde(rename_all = "snake_case")]` at `covenant-a2a/src/lib.rs:124-129`), or `null` when the flag was omitted. Pinned as string-or-null by the schema test (`main.rs:8405-8408`) — never an integer or array. Consumers must route on the lowercase wire form, **not** the Rust TitleCase names (`"Queued"`, `"InFlight"`).
- `tasks` (array of `A2ATaskQueueEntry`): the matched queue entries in the order returned by the daemon. The array may be empty. Pinned as an array by `main.rs:8409` — never null or a string blob.
- `results` (array of `A2ATaskResult`): pending results not yet acknowledged. The array may be empty; the unsuffixed CLI prints `(a2a queue empty)` at `main.rs:4104` when both `tasks` and `results` are empty. Pinned as an array by `main.rs:8410-8413` — never null or a string.

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

Top-level keys are pinned to exactly these seven by the test at `agent-os/crates/covenant/src/main.rs:8368` (`a2a_status_json_pins_top_level_schema`), exercised against both a populated-filters case and an all-null-filters case.

The envelope source-of-truth lives at `a2a_status_json` in `agent-os/crates/covenant/src/main.rs:5546`. Three unit tests at `main.rs:8317` (`a2a_status_json_renders_stable_shape`), `main.rs:8359` (`a2a_status_json_omits_deadline_filter_when_inactive`, which pins the always-emitted-as-null contract on the filter fields), and `main.rs:8368` cover the shape. The CLI verb is wired at `main.rs:3361-3446`; without `--json`, the same response is rendered as JSONL with each task printed as `{"type": "task", "entry": <A2ATaskQueueEntry>}` and each result as `{"type": "result", "result": <A2ATaskResult>}` (per `main.rs:3429-3440`) — a different envelope shape than `--json`, so JSON consumers must use `--json` to get the kind-discriminated envelope.

`covenant a2a retry-stale [--enable] [--min-lease-age-ms <N>] [--max-attempts <N>] [--max-requeues <N>] [--scan-limit <N>] --json` emits a per-call report describing what the auto-retry scan considered, requeued, and skipped. Envelope shape:

- `kind`: literal string `"a2a_auto_retry"`. Pinned at the value level by `main.rs:7269` (asserts `value["kind"].as_str() == Some("a2a_auto_retry")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `report` (object): a structured `A2AAutoRetryReport` (defined at `agent-os/crates/covenant-a2a/src/lib.rs:288`), never a string blob. The top-level object has exactly two keys (`kind` and `report`); the inner `report` is pinned by the schema test at `main.rs:7270-7273` to be a JSON object.

**Dry-run by default**: `A2AAutoRetryPolicy.enabled` defaults to `false` (per `Default for A2AAutoRetryPolicy` at `covenant-a2a/src/lib.rs:228-238`), and the CLI's `--enable` flag is the only path that flips it (`main.rs:3549`). On a `--json` call without `--enable`, every queue entry will appear under `skipped[]` with `reason: "disabled"` and the registry will not be mutated — a `requeued=0` result there is **not** a "nothing to retry" signal. Consumers analysing the report must read `report.policy.enabled` before drawing conclusions about whether `considered` minus `requeued.len()` indicates real skip pressure or a dry-run preview.

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

Top-level keys are pinned to exactly these two by the test at `agent-os/crates/covenant/src/main.rs:7253` (`a2a_retry_json_pins_top_level_schema`), exercised against both a populated (requeued + skipped) case and an empty (fresh policy) case.

The envelope source-of-truth lives at `a2a_retry_json` in `agent-os/crates/covenant/src/main.rs:5565`. Two unit tests at `main.rs:7216` (`a2a_retry_json_renders_stable_shape`) and `main.rs:7253` cover the shape. The CLI verb is wired at `main.rs:3536-3592`; without `--json`, the same response prints `considered <N> task(s), requeued <M>, skipped <K>` followed by `automatic retry disabled; pass --enable to mutate` whenever `report.policy.enabled` is `false` (per `main.rs:3578-3586`).

`covenant a2a compact --json` emits a summary of the event-log compaction that drops lines for fully-resolved A2A tasks. Envelope shape:

- `kind`: literal string `"a2a_compacted"` — past-tense outcome name, distinct from the verb name `compact`; consumers routing on `kind` must match the literal exactly rather than reusing the verb token (`"a2a_compact"`) or guessing a noun form (`"a2a_compaction"`). Pinned at the value level by `main.rs:7204` (asserts `value["kind"].as_str() == Some("a2a_compacted")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `dropped` (u64): count of event-log lines removed for resolved tasks. May legitimately be `0` when no resolved tasks remain — the unsuffixed CLI still prints `dropped 0 a2a event(s)` at `main.rs:3609`, and JSON consumers must not treat `dropped=0` as an error. Pinned as u64 by `main.rs:7205-7208` — never a string-of-integer.

Top-level keys are pinned to exactly these two by the test at `agent-os/crates/covenant/src/main.rs:7188` (`a2a_compact_json_pins_top_level_schema`), exercised against both a populated (`dropped=3`) and an empty (`dropped=0`) case.

The envelope source-of-truth lives at `a2a_compact_json` in `agent-os/crates/covenant/src/main.rs:5352`. Two unit tests at `main.rs:7181` (`a2a_compact_json_renders_stable_shape`) and `main.rs:7188` cover the shape. The CLI verb is wired at `main.rs:3593-3615`; without `--json`, the same response prints `dropped <N> a2a event(s)` at `main.rs:3609`.

`covenant memory compact --reason <text> [--apply] [--detach-stale-parents] [--delete-working-before-ms <M> | --delete-working-older-than-ms <D>] [--delete-episodic-before-ms <M> | --delete-episodic-older-than-ms <D>] [--mark-longterm-stale-before-ms <M> | --mark-longterm-stale-older-than-ms <D>] [--marked-at-ms <M>] --json` emits the outcome of a memory-store compaction pass. Envelope shape:

- `kind`: literal string `"memory_compacted"` — past-tense outcome name, distinct from the verb name `compact`; consumers routing on `kind` must match the literal exactly rather than reusing the verb token (`"memory_compact"`) or guessing a noun form (`"memory_compaction"`). Pinned at the value level by `main.rs:7589` (asserts `value["kind"].as_str() == Some("memory_compacted")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `outcome` (object): a structured `MemoryCompactionOutcome` (defined at `agent-os/crates/covenant-types/src/lib.rs:350`), never a string blob. The top-level object has exactly two keys (`kind` and `outcome`); the inner `outcome` is pinned by the schema test at `main.rs:7590-7593` to be a JSON object.

**Dry-run by default, mutates only with `--apply`**: the CLI defaults to `MemoryRepairMode::DryRun` (per `main.rs:2954-2962`) and `--reason <text>` is mandatory regardless of mode (the CLI bails with `"missing --reason"` at `main.rs:2961` when omitted). Without `--apply`, the daemon evaluates the policy and reports what *would* change but does not mutate the store.

The inner `MemoryCompactionOutcome` shape:

- `mode` (string) — `MemoryRepairMode` slug, exactly `"dry_run"` or `"apply"` (snake_case, per `MemoryRepairMode`'s `#[serde(rename_all = "snake_case")]` at `covenant-types/src/lib.rs:249-254`). Consumers must route on the lowercase wire form, **not** the Rust TitleCase names.
- `would_change` (bool) — the policy identified at least one mutation that would land. Reliable in both modes — `true` whenever the policy matched records.
- `changed` (bool) — the store was actually mutated by this call. In `mode: "dry_run"` this is **always `false`** even when `would_change` is `true`; only `mode: "apply"` can set it. JSON consumers branching on `changed` alone will silently treat dry-run planning runs as no-ops; route on the `(mode, would_change, changed)` triple instead.
- `deleted` (array of strings) — UUIDs of records the policy deleted (in `apply` mode) or would delete (in `dry_run` mode). The empty-case is pinned by the stable-shape test at `main.rs:7560-7563` (asserts `value["outcome"]["deleted"].as_array().map(Vec::len) == Some(0)`).
- `stale_marked` (array of strings) — UUIDs of long-term records the policy marked stale (or would mark, in dry-run mode).
- `parents_detached` (array of strings) — UUIDs of records whose parent pointer the policy detached (or would detach, when `--detach-stale-parents` is supplied). The empty-case is pinned by the stable-shape test at `main.rs:7564-7569` (asserts `value["outcome"]["parents_detached"].as_array().map(Vec::len) == Some(0)`).

Top-level keys are pinned to exactly these two by the test at `agent-os/crates/covenant/src/main.rs:7573` (`memory_compaction_json_pins_top_level_schema`), exercised against both a populated `apply` case and an empty `dry_run` case.

The envelope source-of-truth lives at `memory_compaction_json` in `agent-os/crates/covenant/src/main.rs:5396`. Two unit tests at `main.rs:7545` (`memory_compaction_json_renders_stable_shape`) and `main.rs:7573` cover the shape. The CLI verb is wired at `main.rs:2183-2291` (shared with `covenant memory plan-compaction`; the `plan-compaction` arm forces dry-run and emits a different envelope documented below).

`covenant memory plan-compaction --reason <text> [--detach-stale-parents] [--delete-working-before-ms <M> | --delete-working-older-than-ms <D>] [--delete-episodic-before-ms <M> | --delete-episodic-older-than-ms <D>] [--mark-longterm-stale-before-ms <M> | --mark-longterm-stale-older-than-ms <D>] [--marked-at-ms <M>] --json` emits a read-only compaction plan. The verb shares its argument parser with `covenant memory compact` but is forced into dry-run mode. Envelope shape:

- `kind`: literal string `"memory_compaction_plan"` — distinct from `memory_compacted` so consumers can route on the planning vs mutating outcome without inspecting `outcome.mode`. Pinned at the value level by `main.rs:7658` (asserts `value["kind"].as_str() == Some("memory_compaction_plan")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `outcome` (object): the same `MemoryCompactionOutcome` shape documented in the `memory_compacted` block above. For this verb, `outcome.mode` is **always** `"dry_run"` and `outcome.changed` is **always** `false`; a non-`dry_run` value here indicates daemon/CLI drift and JSON consumers should treat it as a protocol violation. Pinned as a structured object by `main.rs:7659-7662` — never a string blob.
- `expected_receipt_changes` (object): a forward-compatibility placeholder pinned by the schema test at `main.rs:7691` (`memory_compaction_plan_json_pins_expected_receipt_changes_schema`). The block has exactly three keys today and is currently a no-claim stub; consumers must validate the inner shape rather than dispatch directly to apply-mode logic. Pinned as a structured object by `main.rs:7663-7666` — never a string blob.

**`--apply` is rejected** at the CLI level (`main.rs:2192-2194`: `bail!("memory plan-compaction is read-only and does not accept --apply")`) even though the underlying `Request::CompactMemory` request accepts both modes. `--reason <text>` remains mandatory, matching the `memory compact` verb.

The inner `expected_receipt_changes` shape:

- `mode` (string): literal `"none"` today. Pinned by the schema test at `main.rs:7710-7714` as the only currently-allowed value; consumers must treat any other value as a sign that receipt-aware compaction has shipped and the docs are stale. Pinned as a string by `main.rs:7706-7709` — never a structured object.
- `records` (array): empty today (length pinned to `0` at `main.rs:7719-7725`). Will gain a real shape once receipt-aware compaction lands. Pinned as an array by `main.rs:7715-7718` — never null or a string. The renders-test sibling at `main.rs:7633-7638` independently pins the same empty-case `expected_receipt_changes.records` assertion (`as_array().map(Vec::len) == Some(0)`).
- `reason` (string): a human-readable explanation of why the block is empty. Currently the literal `"dry-run compaction planning does not mutate memory or settlement receipts"` per `main.rs:4573`; consumers must not branch on the exact text — only on the field's existence and type. Pinned as a string by `main.rs:7726-7729` — never a structured object.

Top-level keys are pinned to exactly these three by the test at `agent-os/crates/covenant/src/main.rs:7642` (`memory_compaction_plan_json_pins_top_level_schema`), exercised against both a populated dry-run case and an empty dry-run case.

The envelope source-of-truth lives at `memory_compaction_plan_json` in `agent-os/crates/covenant/src/main.rs:5403`. Three unit tests at `main.rs:7618` (`memory_compaction_plan_json_renders_stable_shape`), `main.rs:7642` (`memory_compaction_plan_json_pins_top_level_schema`), and `main.rs:7691` (`memory_compaction_plan_json_pins_expected_receipt_changes_schema`) cover both the outer envelope and the placeholder block. The CLI verb is wired at `main.rs:2183-2291` (shared parser with `covenant memory compact`, branched into the plan-only path at `main.rs:2184`); the `plan-compaction` arm sets `as_json` to `true` by default (`main.rs:2188`) so the unsuffixed CLI also emits the JSON envelope — there is no human-readable plan rendering.

`covenant ignore check <text> --json` emits the result of evaluating the configured ignore rules against operator-supplied text. Envelope shape:

- `kind`: literal string `"ignore_report"`. Pinned at the value level by `main.rs:7900` (asserts `value["kind"].as_str() == Some("ignore_report")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `ignored` (boolean): `true` when at least one loaded rule matched the supplied text; `false` otherwise. Pinned as a JSON boolean by the schema test (`main.rs:7901-7904`) — never `0`/`1` or a string-truthy value.
- `matched_pattern` (string or null): the matched rule pattern when `ignored` is `true`; **always `null`** when `ignored` is `false`. Pinned as string-or-null by the schema test (`main.rs:7905-7908`) — never an empty string for the unmatched case. JSON consumers must use `null` (not `""`) as the unmatched discriminator.
- `rules_loaded` (u64): count of ignore rules the daemon evaluated. May legitimately be `0` when no rules are configured, in which case `ignored` is always `false` and `matched_pattern` is always `null`. Pinned as u64 by `main.rs:7909-7912` — never a string-of-integer.

**Exit-code coupling**: when `ignored` is `true`, the CLI exits `1` even in the `--json` path (per `main.rs:4709-4711`); the envelope is written to stdout *before* the exit. JSON consumers running this verb to gate downstream processing must read the envelope rather than relying solely on transport success — a `--json` invocation that exits `1` is the **expected** signal for a matched ignore rule, not an error.

Top-level keys are pinned to exactly these four by the test at `agent-os/crates/covenant/src/main.rs:7884` (`ignore_report_json_pins_top_level_schema`), exercised against both an `ignored=true` case with a non-null `matched_pattern` and an `ignored=false` case with a null `matched_pattern` and zero `rules_loaded`.

The envelope source-of-truth lives at `ignore_report_json` in `agent-os/crates/covenant/src/main.rs:5434`. Two unit tests at `main.rs:7872` (`ignore_report_json_renders_stable_shape`) and `main.rs:7884` cover the shape. The CLI verb is wired at `main.rs:4668-4716`; without `--json`, the matched case prints `ignored — matched rule: <pattern>` at `main.rs:4705` and the unmatched case prints `not ignored (<n> rule(s) loaded)` at `main.rs:4707`. Both paths share the exit-1-when-ignored convention.

`covenant bootstrap --json` emits a summary of the capability-bootstrap pass that grants every action required by manifests under `$COVENANT_HOME/agents/*/agent.toml` (plus the implicit `memory.write`, which the daemon writes on every successful dispatch). Envelope shape:

- `kind`: literal string `"bootstrap_result"`. Pinned at the value level by `main.rs:6689` (asserts `value["kind"].as_str() == Some("bootstrap_result")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.
- `granted` (array of `{action: string, signature_b58: string}` objects): the capabilities **newly granted** during this bootstrap call. Each element echoes the action string and the daemon-signed base58 signature that authorises it. Pinned as an array by `main.rs:6690-6693` — never null or a string. The asymmetric inner shape — `granted` entries are objects, not bare strings — is pinned by `main.rs:6707-6710` (asserts `populated["granted"][0].is_object()`).
- `already_granted` (array of strings): the action names the daemon **already had** before this call. Note the asymmetry with `granted`: this field carries **bare action strings**, not the `{action, signature_b58}` object shape — the existing signatures are not echoed here. JSON consumers must not iterate `already_granted` as if it were objects. Pinned as an array by `main.rs:6694-6697` — never null or a string. The asymmetric inner shape — `already_granted` entries are bare strings, not objects — is pinned by `main.rs:6711-6714` (asserts `populated["already_granted"][0].is_string()`).

Top-level keys are pinned by the test at `agent-os/crates/covenant/src/main.rs:6673` (`bootstrap_result_json_pins_top_level_schema`), exercised against a populated case (two newly-granted entries plus one already-granted entry), an empty-granted case (no new grants, two already-granted entries), and a fully-empty case. The test also asserts the asymmetric inner shape: `granted` entries are `{action, signature_b58}` objects while `already_granted` entries are bare action strings.

Re-running `covenant bootstrap` is idempotent: if every required action is already granted, `granted` is empty and `already_granted` carries the full set. An empty `granted` array is the **expected** signal for "nothing to do" — not a transport failure. The empty-granted case must serialize as a JSON array (`[]`), not as `null` or an absent key; this invariant is pinned by `main.rs:6719-6722` (asserts `no_new_grants["granted"].as_array().unwrap().is_empty()`). The unsuffixed CLI prints `nothing to do — every required capability is already granted (<n> total)` at `main.rs:1953-1956` for that case.

The envelope source-of-truth lives at `bootstrap_result_json` in `agent-os/crates/covenant/src/main.rs:5532`. Two unit tests at `main.rs:6650` (`bootstrap_result_json_renders_stable_shape`) and `main.rs:6673` cover the shape. The CLI verb is wired at `main.rs:1872-1975`; the JSON emission site calls the helper at `main.rs:1950-1951`. Required actions are derived from the union of every `agent.toml`'s `[capabilities].required` list (`main.rs:1888-1908`) plus the unconditional `memory.write` insertion (`main.rs:1890`). The daemon-side dispatch is `Request::GrantCapability` per action (`main.rs:1929-1936`); failures fall through to a `daemon error granting <action>: <message>` bail rather than into the envelope. Without `--json`, the same response prints `granted <n> of <m> capabilities to user@local:` followed by one `  + <friendly-title> (<action>)` line per newly-granted entry and a final `ready. try: covenant intent "say hello"` (per `main.rs:1958-1973`).

`covenant intents resume <intent-id|latest> --json` emits the outcome of resuming a previously-paused intent (typically one that hit a `BudgetExhausted` audit row). The envelope is **two-shape**: success and error share the same `kind` discriminator and use a flat `ok` boolean as the structural discriminator at the top level — **not** a tagged-enum `outcome.type` like `peer_revoke`. Consumers must branch on `ok` to know which key set to expect.

Both branches share these fields:

- `kind`: literal string `"intents_resume"` — verb-name asymmetry: the CLI verb is `resume` but the envelope discriminator is `intents_resume` (the noun-verb compound, not the verb token alone); consumers routing on `kind` must match the literal exactly. The same literal is emitted on both `ok=true` and `ok=false` envelopes. Pinned at the value level by `main.rs:6359` (success branch) and `main.rs:6202` (error branch) — each asserts `value["kind"].as_str() == Some("intents_resume")` — so a future kind-rename fails the tests rather than silently rewriting the discriminator string.
- `ok` (boolean): `true` on success, `false` on every error path. Pinned as a JSON boolean by the schema tests at `main.rs:6360-6363` and `main.rs:6203-6206` — never `0`/`1` or a string-truthy value. JSON consumers branching on `ok` alone get the correct outcome class without inspecting variant-specific fields. The error branch's invariant `ok=false` is also pinned at the value level by `main.rs:6207-6211` (asserts `value["ok"].as_bool() == Some(false)`), so a regression that emitted `ok=true` from the error envelope would fail at test time.
- `mode` (string): exactly `"explicit"` or `"latest"`, derived from the CLI invocation form at `main.rs:4578` (`--latest`/`latest` → `"latest"`, any positional intent-id → `"explicit"`). The envelope echoes the operator's input form, so consumers can distinguish a targeted resume from a "resume the most recent paused intent" call. Pinned as a string by `main.rs:6364` (success branch) and `main.rs:6212` (error branch) — never an object or array.

**Success branch (`ok=true`)** carries these eight top-level keys per the test EXPECTED_KEYS at `main.rs:6335-6344`:


- `intent_id` (string) — the resumed intent's UUID in canonical hyphenated form. Pinned as a string by `main.rs:6365-6368` — never a byte array.
- `status` (string) — the daemon-returned outcome status (typically `"ok"`). The string shape is pinned at `main.rs:6369-6372`; specific value enumeration lives with the daemon's intent dispatcher rather than this docs surface.
- `text` (string) — the result text the daemon returned for the resumed intent. The unsuffixed CLI prints this value directly at `main.rs:3950`. Pinned as a string by `main.rs:6373` — never an object or array.
- `sources` (array of strings) — source labels that contributed to the result. Pinned as an array of strings by `main.rs:6374-6377` — never a comma-joined string. Empty when no sources are attached; the unsuffixed CLI prints a `sources:` block followed by `  - <label>` lines at `main.rs:4630-4633` only when the array is non-empty.
- `settlement` (object or null) — an optional `SettlementReceipt` (defined at `agent-os/crates/covenant-types/src/lib.rs:392` and documented in the `receipt_list` block above) carrying the on-chain or local settlement evidence when the resumed intent consumed credits. `null` when the resume did not settle. Pinned as object-or-null by `main.rs:6378-6381` — never an integer or array.

**Error branch (`ok=false`)** carries these five top-level keys per the test EXPECTED_KEYS at `main.rs:6187`:

- `intent_id` (string or null) — string-uuid when the intent_id was already resolved (e.g., the daemon round-trip started but returned an error, per `main.rs:4640-4645`); **null** when the intent_id could not be resolved before the daemon round-trip (e.g., `missing_intent_id` and `conflicting_flags` paths at `main.rs:4589-4597`). Pinned as string-or-null by `main.rs:6213-6216`. JSON consumers must accept `null` here rather than treating it as a malformed envelope.
- `error` (object): a structured `{code, message}` pair, never a string blob. Pinned as a JSON object by `main.rs:6217-6220`. The inner `error` object has exactly two keys per the test EXPECTED_KEYS at `main.rs:6240`:
  - `code` (string) — typed error slug (snake_case). Pinned as a string by `main.rs:6254-6257` — never a structured object. The recognised slugs from `intents_resume_error_code` at `main.rs:5579-5594` are `"no_budget_exhausted_row"`, `"missing_intent_id"`, `"invalid_intent_id"`, `"conflicting_flags"`, and the catch-all `"error"` for daemon messages that do not match a typed arm. Additionally, the inline emission paths add `"daemon_error"` (when the daemon returns `Response::Error`, `main.rs:4643`) and `"unexpected_response"` (when the daemon returns an unexpected response variant, `main.rs:4658`). Consumers must route on the documented slugs; new slugs are a forcing function to update this docs block and the `intents_resume_error_code_pins_documented_arms` test at `main.rs:6280`.
  - `message` (string) — the human-readable diagnostic the daemon or CLI produced. Pinned as a string by `main.rs:6258-6261` — never a structured object.

**Exit-code coupling**: the `--json` path exits `1` on every error branch (`main.rs:4598`, `main.rs:4647`, `main.rs:4662`); the envelope is written to stdout *before* the exit. JSON consumers gating downstream processing on transport success will silently miss resume failures — branch on `ok` rather than on exit `0`. Success exits `0` via the normal return at `main.rs:4625`. This mirrors the `peer_revoke` and `ignore_report` exit-coupling conventions documented above.

Top-level keys are pinned by three tests at `agent-os/crates/covenant/src/main.rs`: `intents_resume_ok_json_pins_top_level_schema` at `main.rs:6334` (success branch, exercises both `Some(SettlementReceipt)` and `None` cases), `intents_resume_error_json_pins_top_level_schema` at `main.rs:6186` (error branch, exercises both `Some(intent_id)` and `None` cases), and `intents_resume_error_json_pins_error_object_schema` at `main.rs:6239` (inner `error` object's two-key shape). The typed-slug enumeration is pinned by `intents_resume_error_code_pins_documented_arms` at `main.rs:6280`.

The envelope source-of-truth lives at `intents_resume_ok_json` (`main.rs:5596`) and `intents_resume_error_json` (`main.rs:5616`), with the slug classifier at `intents_resume_error_code` (`main.rs:5579`). The CLI verb is wired at `main.rs:4557-4667`; without `--json`, the success branch prints the result `text` at `main.rs:3950` followed by an optional `sources:` block, and the error branches bail with the human-readable diagnostic — there is no envelope rendering, so JSON consumers must use `--json` to get the kind-discriminated envelope.

`covenant settlement backfill-receipts [--dry-run] [--json]` emits a versioned-schema envelope describing the legacy settlement-receipt repair pass. **Schema-suffix convention**, not the unversioned `kind` convention every other envelope in this section uses — consumers route on `schema` rather than `kind`. The asymmetry is consistent with the section preamble (line 86): post-`covenant.<area>.<verb>.v<n>` envelopes land with the suffix; only the older read-side envelopes carry the bare `kind` literal.

Envelope shape:

- `schema`: literal string `"covenant.settlement.backfill.v1"`. The `.v1` suffix is the version slot; a future `.v2` would be a separate envelope, not a field rename inside this one. Consumers must route on the full literal — matching on the prefix `"covenant.settlement.backfill."` will swallow incompatible future versions. Pinned as a string by `main.rs:6554-6557` — never an integer or object. The literal value `"covenant.settlement.backfill.v1"` is also pinned at the value level by `main.rs:6558-6562` (asserts `value["schema"].as_str() == Some("covenant.settlement.backfill.v1")`), so a future `.v2` bump fails the test rather than silently rewriting the schema string.
- `row_count` (u64): count of legacy settlement-receipt rows the backfill operated on (mutation path) or *would* operate on (dry-run path). May legitimately be `0` when no legacy rows match — the verb does not error on an empty backfill. Pinned as u64 by `main.rs:6563-6566` — never a string-of-integer.
- `rollback_path` (string or null): filesystem path to the rollback-evidence file written by a mutation pass; `null` in dry-run mode. The CLI's inline emission at `main.rs:4761` passes `rollback_path.as_deref()` through `Option<&str>`, and the unsuffixed CLI at `main.rs:4769-4771` maps `None` to the literal `(none)` — JSON consumers must use `null` (not `""` or `"(none)"`) as the unset discriminator. When non-null, the path is meaningful only on the daemon's local filesystem; remote consumers must not assume the file is reachable. Pinned as string-or-null by `main.rs:6567-6570` — never the literal `"(none)"`.
- `dry_run` (bool): echoes the `--dry-run` CLI flag. `true` is a safe planning preview that does not mutate the receipt table; `false` is a real mutation pass that may write rollback evidence. Pinned as a JSON boolean by `main.rs:6571-6574` — never `0`/`1` or a string.

**`--scope-pubkey` is reserved, not yet wired**: the CLI accepts a `--scope-pubkey <value>` flag and forwards it through `Request::BackfillSettlementReceipts.scope_pubkey` (`main.rs:4732-4737`, `main.rs:4746`), but the daemon-side filter is not yet implemented (see the help text at `main.rs:2105` and the file-header CLI summary at `main.rs:38`). Operators relying on the flag for scoped backfills will not get the scoping behavior they expect; the envelope reports the unscoped result regardless. This will change when the approved `settlement-receipt-backfill-mutation` slice lands.

Top-level keys are pinned by the test at `agent-os/crates/covenant/src/main.rs:6539` (`settlement_backfill_json_pins_top_level_schema`), exercised against both a dry-run shape (`rollback_path` null), a mutation shape (`rollback_path` set), and an empty-rows dry-run shape; the test also asserts the literal `"covenant.settlement.backfill.v1"` schema string so a future v2 bump must land as a separate envelope, not a field rename inside this one.

The envelope source-of-truth lives at `settlement_backfill_json` in `agent-os/crates/covenant/src/main.rs:5510`. Two unit tests at `main.rs:6517` (`settlement_backfill_json_renders_stable_shape`) and `main.rs:6539` cover the shape. The CLI verb is wired at `main.rs:4723-4777` (the `settlement backfill-receipts` subcommand); without `--json`, the same response prints `row_count: <N>`, `dry_run: <bool>`, and `rollback_path: <path>|(none)` on three separate lines at `main.rs:4766-4771`. The daemon-side `Response::SettlementReceiptsBackfilled` variant carries the three fields directly (`main.rs:4751-4755`); a future schema bump must propagate through the daemon variant, the CLI emitter, and this docs block as one atomic change.

`covenant memory backfill-receipt-correlation [--dry-run] [--json]` emits a versioned-schema envelope describing the legacy memory-record-to-receipt correlation backfill pass. Sibling to `settlement.backfill.v1` above — both use the `covenant.<area>.backfill.v<n>` convention and both share the `--scope-pubkey` reservation. The structural diff is the rollback channel: settlement uses a **filesystem** rollback file (`rollback_path`), memory uses a **SQLite SAVEPOINT** identifier (`savepoint_name`) so a future mutator can `ROLLBACK TO SAVEPOINT` within the same DB transaction.

Envelope shape:

- `schema`: literal string `"covenant.memory.backfill.v1"`. Same versioning semantics as `covenant.settlement.backfill.v1` — route on the full literal, not the prefix. Pinned as a string by `main.rs:6617-6620` — never an integer or object. The literal value `"covenant.memory.backfill.v1"` is also pinned at the value level by `main.rs:6621-6625` (asserts `value["schema"].as_str() == Some("covenant.memory.backfill.v1")`), so a future `.v2` bump fails the test rather than silently rewriting the schema string.
- `row_count` (u64): count of memory records the correlation pass operated on (mutation path) or *would* operate on (dry-run path). May legitimately be `0` when no legacy rows match. Pinned as u64 by `main.rs:6626-6629` — never a string-of-integer.
- `savepoint_name` (string): SQLite SAVEPOINT identifier the daemon emitted for this pass. **Always a non-null string** — the field type at `memory_backfill_json` (`main.rs:5523`) is `&str`, not `Option<&str>`, so even a dry-run call returns a real savepoint name (the daemon allocates one so consumers can correlate planning runs against later mutation runs). JSON consumers must not write null-vs-value branching for this field; treat absence as a protocol violation. This is the only field-shape difference from `settlement.backfill.v1`, whose sibling `rollback_path` is string-or-null. Pinned as a string by `main.rs:6630-6633` — never null (the &str emitter type forbids null at compile time). The non-empty invariant — savepoint_name is also never the empty string `""`, even on dry-run — is pinned by `main.rs:6634-6637` (asserts `!value["savepoint_name"].as_str().unwrap().is_empty()`).
- `dry_run` (bool): echoes the `--dry-run` CLI flag. Same semantics as `settlement.backfill.v1`'s `dry_run` — `true` is a planning preview, `false` is a real mutation pass. Pinned as a JSON boolean by `main.rs:6638-6641` — never `0`/`1` or a string.

**Verb-name asymmetry**: the CLI verb is the long form `memory backfill-receipt-correlation`, **not** `memory backfill` or `memory backfill-receipts` (which would mirror the settlement sibling's shorter name). The match arm is at `main.rs:3041`; the shorter spellings do not parse and return an `unknown flag` bail. JSON consumers driving the CLI from a wrapper must hard-code the long verb token.

**`--scope-pubkey` is reserved, not yet wired**: same caveat as `settlement.backfill.v1`. The CLI accepts the flag and forwards it through `Request::BackfillMemoryRecords.scope_pubkey` (`main.rs:3050-3055`, `main.rs:3064`), but the daemon-side filter is not yet implemented (see the help text at `main.rs:2076` and the file-header CLI summary at `main.rs:12`). This will change when the approved `memory-record-receipt-backfill-mutation` slice lands.

Top-level keys are pinned by the test at `agent-os/crates/covenant/src/main.rs:6602` (`memory_backfill_json_pins_top_level_schema`), exercised against a dry-run shape, a mutation shape, and an empty-rows dry-run shape; the test also asserts the literal `"covenant.memory.backfill.v1"` schema string and the always-non-null, always-non-empty `savepoint_name` contract documented above.

The envelope source-of-truth lives at `memory_backfill_json` in `agent-os/crates/covenant/src/main.rs:5523`. Two unit tests at `main.rs:6587` (`memory_backfill_json_renders_stable_shape`) and `main.rs:6602` cover the shape. The CLI verb is wired at `main.rs:3041-3092` (the `memory backfill-receipt-correlation` arm under the `memory` subcommand); without `--json`, the same response prints `row_count: <N>`, `dry_run: <bool>`, and `savepoint_name: <name>` on three separate lines at `main.rs:3084-3086`. The daemon-side `Response::MemoryRecordsBackfilled` variant carries the three fields directly (`main.rs:3069-3073`); a future schema bump must propagate through the daemon variant, the CLI emitter, and this docs block as one atomic change.

## Human Authority

The decision to bump the IPC/HTTP protocol, the wire shapes that change, the migration window, and the public release notes for v2 remain human-owned. Automation keeps this contract documented and validated; with the v2 `StreamEnvelope` fixtures landed under ADR 0010, the validator now runs in strict mode rather than dormant. It must not introduce v2 fixtures, edit `PROTOCOL_VERSION`, or relax the migration-note pairing without an approved decision.
