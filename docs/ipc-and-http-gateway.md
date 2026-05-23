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

`covenant verify --json` emits a cross-check report comparing the audit log against memory and receipt rows. Envelope shape:

- `kind`: literal string `"verify_report"`.
- `window` (u64): the audit-window record count echoed back from the `--window` argument.
- `checks` (array of `VerifyCheck`): per-check results, see below.
- `drift` (array of `VerifyDrift`): correlation gaps, see below.
- `orphans_total` (u64): total number of unmatched rows the checks discovered.

Top-level keys are pinned to exactly these five by the test at `agent-os/crates/covenant/src/main.rs:6987` (`verify_report_json_pins_top_level_schema`).

`VerifyCheck` shape, defined at `agent-os/crates/covenant-ipc/src/lib.rs:26`:

- `name` (string) — human-readable check name (e.g., `"memory audit"`).
- `passed` (bool) — whether the check passed.
- `message` (string) — diagnostic message (empty when the check passed cleanly).

`VerifyDrift` shape, defined at `agent-os/crates/covenant-ipc/src/lib.rs:33`:

- `kind` (string) — drift category (e.g., `"memory_without_audit"`).
- `id` (string, omitted when null) — record identifier when the drift entry binds to a specific row. Serialized via `#[serde(skip_serializing_if = "Option::is_none")]`, so absent rather than `null` when unbound.
- `message` (string) — drift description.
- `repair` (string) — operator-facing remediation hint.

The envelope source-of-truth lives at `verify_report_json` in `agent-os/crates/covenant/src/main.rs:4537`. The shape-pinning test at `main.rs:6987-7033` covers both the populated and empty cases (`assert_shape` runs against a one-check, one-drift report and an all-empty report).

`covenant tools list --json` emits the registered MCP-style tool catalog. Envelope shape:

- `kind`: literal string `"tool_list"` (singular `tool_list`, not `tools_list`; consumers routing on `kind` must match the literal exactly).
- `tools` (array of `ToolSpec`): the registered tools the daemon advertises via `tools/list`. The array is empty when no tools are registered; the unsuffixed CLI prints `(no tools registered)` for that case at `main.rs:3123`.

The inner `ToolSpec` shape, defined at `agent-os/crates/covenant-mcp/src/lib.rs:27`:

- `name` (string) — tool identifier.
- `description` (string) — human-readable tool summary.
- `inputSchema` (object) — JSON Schema for the tool's `arguments` object; an empty object means the tool takes no arguments.

`ToolSpec` carries `#[serde(rename_all = "camelCase")]` (`covenant-mcp/src/lib.rs:26`) so the Rust field `input_schema` serializes on the wire as `inputSchema`. The naming matches the MCP wire format; JSON consumers must deserialize using `inputSchema`, not `input_schema`.

Top-level keys are pinned to exactly these two by the test at `agent-os/crates/covenant/src/main.rs:6733` (`tool_list_json_pins_top_level_schema`), which exercises both a populated single-tool case and an empty list.

The envelope source-of-truth lives at `tool_list_json` in `agent-os/crates/covenant/src/main.rs:4502`. Two unit tests at `main.rs:6709` (`tool_list_json_renders_stable_shape`) and `main.rs:6733` cover both cases. The CLI verb is wired at `main.rs:3107-3133`; without `--json`, the same response prints one line per tool in the form `<name> — <description>` at `main.rs:3126`.

`covenant tools call <name> [--args <json>] --json` emits the tool invocation result. Envelope shape:

- `kind`: literal string `"tool_result"` (singular, not `tools_result`; consumers routing on `kind` must match the literal exactly).
- `name` (string): the tool name echoed back from the CLI argument.
- `content` (array of `Content`): the tool's output blocks. Each element is a tagged-enum object whose `type` discriminator selects the variant — `{type: "text", text: <string>}` for textual output or `{type: "json", value: <JSON>}` for structured output. The variants are defined at `agent-os/crates/covenant-mcp/src/lib.rs:38` with `#[serde(tag = "type", rename_all = "camelCase")]`; v0 ships text and json variants only. The array is empty when the tool produced no output blocks; the unsuffixed CLI prints each block sequentially at `main.rs:3174-3180`.
- `is_error` (boolean): `true` when the tool itself raised; pinned as a JSON boolean by the schema test (`main.rs:6815-6818`) — never `0`/`1` or a string. JSON consumers must branch on this boolean, not on the presence/absence of content. `is_error=true` paired with non-empty `content` describes a partial-success outcome with an error indicator.

Top-level keys are pinned to exactly these four by the test at `agent-os/crates/covenant/src/main.rs:6793` (`tool_result_json_pins_top_level_schema`), exercised against both a non-empty content + is_error=true case and an empty content + is_error=false case.

The envelope source-of-truth lives at `tool_result_json` in `agent-os/crates/covenant/src/main.rs:4509`. Two unit tests at `main.rs:6772` (`tool_result_json_renders_stable_shape`) and `main.rs:6793` cover the shape. The CLI verb is wired at `main.rs:3134-3180`; without `--json`, each `Content::Text` block prints its `text` directly and each `Content::Json` block prints its `value` as pretty-printed JSON.

`covenant chain flush-receipts --json` emits a receipt-batch summary when it groups local settlement receipts into a single Solana receipt-root transaction. Envelope shape:

- `kind`: literal string `"receipt_batch_flushed"`.
- `limit` (u64): the batch-size cap echoed back from the `--limit` argument.
- `receipts_updated` (u64): the number of local receipt rows updated to point at the new batch.
- `batch` (`ReceiptBatchSummary` object): the batch's wire shape, see below.

Top-level keys are pinned to exactly these four by the test at `agent-os/crates/covenant/src/main.rs:7055` (`flush_receipts_json_pins_top_level_schema`).

`ReceiptBatchSummary` shape, defined at `agent-os/crates/covenant-ipc/src/lib.rs:53`:

- `batch_id` (string) — opaque batch identifier.
- `merkle_root` (string, 64 hex characters) — Merkle root over the included receipts.
- `receipt_count` (u32) — number of receipts in the batch (note u32, not u64).
- `tx_sig` (string or null) — base58 Solana transaction signature once the batch confirms; null before submission completes.
- `slot` (u64 or null) — confirmation slot once available; null until then.

The envelope source-of-truth lives at `flush_receipts_json` in `agent-os/crates/covenant/src/main.rs:4552`. Two unit tests at `main.rs:7036` (`flush_receipts_json_renders_stable_shape`) and `main.rs:7054` (`flush_receipts_json_pins_top_level_schema`) cover both the unconfirmed (`tx_sig`/`slot` null) and confirmed (both present) batch states.

`covenant chain receipt-batches --json` emits the list of recent receipt batches recorded on-chain. Envelope shape:

- `kind`: literal string `"receipt_batch_list"`.
- `limit` (u64): the result cap echoed back from the `--limit` argument.
- `batches` (array of `ReceiptBatchSummary`): the batches, in the order returned by the daemon. Each item uses the same `ReceiptBatchSummary` shape documented above (including the `tx_sig`/`slot` null convention for batches whose settlement transaction has not yet confirmed). The array may be empty.

Top-level keys are pinned to exactly these three by the test at `agent-os/crates/covenant/src/main.rs:6852` (`receipt_batch_list_json_pins_top_level_schema`).

The envelope source-of-truth lives at `receipt_batch_list_json` in `agent-os/crates/covenant/src/main.rs:4522`. Two unit tests at `main.rs:6834` (`receipt_batch_list_json_renders_stable_shape`) and `main.rs:6852` (`receipt_batch_list_json_pins_top_level_schema`) cover the populated and empty cases.

`covenant receipts recent [-n|--limit <N>] [--since-ms <M>] --json` emits a window of local settlement receipts. Envelope shape:

- `kind`: literal string `"receipt_list"` — verb-name asymmetry: the CLI verb is `recent` but the envelope discriminator is `receipt_list` (singular `receipt_`, not `receipts_`); consumers routing on `kind` must match the literal exactly rather than reusing the verb token or pluralising.
- `limit` (u64): the request limit echoed back from `-n`/`--limit` (default `10`, per `main.rs:2837`). Pinned at the type level by the schema test (`main.rs:5484-5486`) — never a string.
- `since_ms` (u64 or null): the Unix-epoch millisecond threshold echoed from `--since-ms`, or `null` when the flag was omitted. Pinned as u64-or-null at the schema test (`main.rs:5487-5490`) — never a string-of-integer. Filter semantics live with the daemon's `Request::RecentReceipts` handler; this surface only echoes the operator's input.
- `receipts` (array of `SettlementReceipt`): the matched receipts in the order returned by the daemon. The array is empty when no receipts fall in the window; the unsuffixed CLI prints `(no receipts)` for that case at `main.rs:2867`.

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
- `onchain_sig` (string or null) — backwards-compatible alias for `tx_sig` (per the struct doc-comment at `covenant-types/src/lib.rs:335-337`) that older clients still consume; new consumers should prefer `tx_sig`. Always present on the wire. Both fields carry the same value once the receipt confirms; the unsuffixed CLI's `(local-only)` fallback at `main.rs:2871-2874` reads `tx_sig` first and falls back to `onchain_sig` for exactly that reason.

Top-level keys are pinned to exactly these four by the test at `agent-os/crates/covenant/src/main.rs:5466` (`receipt_list_json_pins_top_level_schema`), exercised against three cases: populated with `since_ms`, populated without `since_ms`, and empty without `since_ms`.

The envelope source-of-truth lives at `receipt_list_json` in `agent-os/crates/covenant/src/main.rs:4314`. Two unit tests at `main.rs:5425` (`receipt_list_json_renders_stable_shape`) and `main.rs:5466` cover the shape. The CLI verb is wired at `main.rs:2832-2885`; without `--json`, each receipt is printed as `[<settled_at>] <resource>: <credits> credits — <onchain>` at `main.rs:2875-2879`, with `<onchain>` resolving to the `tx_sig`/`onchain_sig` value or the literal `(local-only)` when both are null.

`covenant ping --json` emits a daemon-liveness probe. Envelope shape:

- `kind`: literal string `"daemon_ping"`.
- `status`: literal string `"ok"` — the daemon only returns this envelope when it has accepted the request and produced a `Response::Pong`; failures surface as a non-zero CLI exit rather than a non-`"ok"` payload, so consumers can branch on transport success alone.

Top-level keys are pinned to exactly these two by the test at `agent-os/crates/covenant/src/main.rs:5617` (`ping_json_pins_top_level_schema`).

The envelope source-of-truth lives at `ping_json` in `agent-os/crates/covenant/src/main.rs:4344`. The shape-pinning tests at `main.rs:5610` (`ping_json_renders_stable_shape`) and `main.rs:5617` cover the single emitted shape; the CLI verb is wired at `main.rs:1977-1999` (the unsuffixed `covenant ping` prints `pong` instead).

`covenant intent [--json] [--stream] <text>` emits the dispatched intent's outcome with optional settlement evidence. Envelope shape:

- `kind`: literal string `"intent_result"`.
- `intent_id` (string): the dispatched intent's UUID, serialized as the canonical hyphenated string form. Pinned as a string by the schema test (`main.rs:5563-5566`) — never a byte array or struct.
- `status` (string): the outcome status (e.g., `"ok"`). The string shape is pinned by `main.rs:5567-5570`; specific value enumeration lives with the daemon's intent dispatcher rather than this docs surface.
- `text` (string): the result text the daemon returned. The unsuffixed CLI prints this value directly at `main.rs:2069` (a single-line `println!("{text}")`), so `covenant intent --json` and `covenant intent` share the result payload but only `--json` wraps it in the envelope.
- `sources` (array of strings): source labels that contributed to the result (e.g., `["research"]`). Empty when no sources are attached.
- `settlement` (object or null): an optional `SettlementReceipt` (defined at `agent-os/crates/covenant-types/src/lib.rs:339`) carrying the on-chain or local settlement evidence when the intent consumed credits. `null` when the intent did not settle (e.g., a phase-0 echo that does not charge). Pinned as object-or-null by `main.rs:5576-5579` — never an integer or array.

Top-level keys are pinned to exactly these six by the test at `agent-os/crates/covenant/src/main.rs:5539` (`intent_result_json_pins_top_level_schema`), exercised against both a populated `Some(SettlementReceipt)` case and an empty unsettled case.

The envelope source-of-truth lives at `intent_result_json` in `agent-os/crates/covenant/src/main.rs:4327`. Two unit tests at `main.rs:5521` (`intent_result_json_renders_stable_shape`) and `main.rs:5539` cover the shape. The CLI verb is wired at `main.rs:2000-2074`; the `--json`/`--stream` flags are recognized only in leading position (`main.rs:2013-2022`) so an interior `--json` token is preserved as part of the intent text. The optional `--stream` flag sets `Request::SubmitIntent.prefer_stream = Some(true)` (`main.rs:2033`), enabling the v2 streaming-response path documented under [docs/protocol-versioning.md](./protocol-versioning.md); the terminal `IntentResult` envelope shape is unchanged when the streaming path is not selected.

`covenant capabilities recent [-n|--limit <N>] --json` emits a peer-scoped view of recent signed capabilities. Envelope shape:

- `kind`: literal string `"capability_list"` — verb-name asymmetry: the CLI verb is `recent` but the envelope discriminator is `capability_list`. Consumers routing on `kind` must match the latter literal exactly rather than reusing the verb token.
- `limit` (u64): the request limit echoed back from `-n`/`--limit` (default `10`, see `main.rs:2569`). Pinned at the type level by the schema test (`main.rs:5698`) — JSON consumers must never receive a string here.
- `capabilities` (array of `SignedCapability`): the filtered live capabilities. Each element has shape `{capability: Capability, signature: <base58>}` where `Capability` is defined at `agent-os/crates/covenant-types/src/lib.rs:171` (fields: `subject`, `action`, `scope`, `granted_by`, `expires_at`) and `SignedCapability` is defined at `agent-os/crates/covenant-permissions/src/lib.rs:58`. The `signature` field is the base58 encoding of the 64-byte ed25519 signature (per the `sig_b58` serde module at `lib.rs:64-83`), never the raw byte array.

The daemon applies a **peer-visibility filter** before returning the list (see `recent_capabilities` at `agent-os/crates/covenantd/src/lib.rs:5834-5849`): only capabilities whose `subject.pubkey` or `granted_by.pubkey` matches the requesting peer's pubkey are included. JSON consumers must not assume this is a global registry dump — operator and delegated callers see a different slice of the same store.

Top-level keys are pinned to exactly these three by the test at `agent-os/crates/covenant/src/main.rs:5680` (`capability_list_json_pins_top_level_schema`), which exercises both a populated single-capability case and an empty list.

The envelope source-of-truth lives at `capability_list_json` in `agent-os/crates/covenant/src/main.rs:4351`. Two unit tests at `main.rs:5640` (`capability_list_json_renders_stable_shape`) and `main.rs:5680` cover both cases. The CLI verb is wired at `main.rs:2568-2624`; without `--json`, the same response prints one line per capability in the form `<subject_display> → <action_label> (<granted_by_display>) [<expiry>]` at `main.rs:2612-2618`, or `(no capabilities granted)` when the filtered list is empty.

`covenant capabilities grant <action> [--scope <json>] [--expires-at <ms>] --json` emits the freshly-signed capability after the daemon accepts the grant. Envelope shape:

- `kind`: literal string `"capability_granted"` — past-tense outcome name, distinct from the verb name `grant`; consumers routing on `kind` must match the literal exactly rather than reusing the verb token.
- `subject_display` (string): the daemon-synthesized human-readable subject (e.g., `operator@local`). The daemon owns this field — consumers must not reconstruct it from the request.
- `action` (string): the action the capability was granted for. **Not always the verbatim CLI argument**: when the CLI receives an a2a peer-prefix shorthand it expands the prefix to the full peer-bound action before signing (see `expand_a2a_action` invoked at `main.rs:2657-2690`); the envelope reports the post-expansion full form, and the unsuffixed CLI prints an `expanding <prefix> → <full>` line to stderr at `main.rs:2680`.
- `signature_b58` (string): the base58 signature over the signed-capability bytes. This is the same value consumers pass back to `covenant capabilities revoke <signature-b58>` to tombstone the capability.
- `scope` (object or null): the structured scope object echoed from the request, or `null` when `--scope` was omitted. Pinned at the type level by the schema test (`main.rs:5783`) — JSON consumers must never receive a string blob here, so a scope value of `"{\"version\":1}"` would be a contract break.
- `expires_at` (u64 or null): the Unix-epoch millisecond expiry echoed from `--expires-at`, or `null` when the flag was omitted. Pinned at the type level by the schema test (`main.rs:5787`) — JSON consumers must never receive a string here, so a value of `"1700000000000"` would be a contract break.

Top-level keys are pinned to exactly these six by the test at `agent-os/crates/covenant/src/main.rs:5746` (`capability_grant_json_pins_top_level_schema`), which also asserts the `scope` object-or-null and `expires_at` u64-or-null typing.

The envelope source-of-truth lives at `capability_grant_json` in `agent-os/crates/covenant/src/main.rs:4359`. Two unit tests at `main.rs:5723` (`capability_grant_json_renders_stable_shape`, covers both a scoped+timed grant and an unscoped+untimed grant) and `main.rs:5746` cover both populated cases. The CLI verb is wired at `main.rs:2626-2718`; without `--json`, the same response prints `granted: <subject> → <action>` followed by the signature on a second line.

`covenant capabilities revoke <signature-b58> --json` emits the outcome of revoking a single signed capability by its signature. Envelope shape:

- `kind`: literal string `"capability_revoked"` — past-tense outcome name, distinct from the verb name `revoke`; consumers routing on `kind` must match the literal exactly rather than reusing the verb token.
- `signature_b58` (string): the base58 signature echoed back from the request, so consumers can correlate the response to the revoke call without tracking it out of band.
- `removed` (boolean): `true` if a live capability matched and was tombstoned, `false` if no live row matched that signature. `false` is a benign no-op outcome, not an error — the daemon still returns `Response::CapabilityRevoked` and the unsuffixed CLI prints `(no live capability with that signature)` for that case at `main.rs:2769`. JSON consumers must not treat `removed=false` as a failure.

Top-level keys are pinned to exactly these three by the test at `agent-os/crates/covenant/src/main.rs:5823` (`capability_revoke_json_pins_top_level_schema`), which also asserts `removed` is a JSON boolean (never `0`/`1` or a string).

The envelope source-of-truth lives at `capability_revoke_json` in `agent-os/crates/covenant/src/main.rs:4376`. Two unit tests at `main.rs:5810` (`capability_revoke_json_renders_stable_shape`) and `main.rs:5823` cover both the `removed=true` and `removed=false` cases. The CLI verb is wired at `main.rs:2731-2774`.

`covenant capabilities purge --json` emits a summary of revoked-capability garbage collection. Envelope shape:

- `kind`: literal string `"capabilities_purged"`.
- `before_ms` (u64): the resolved Unix-epoch millisecond cutoff. The CLI accepts either `--before-ms <M>` (echoed verbatim) or `--older-than-ms <D>` (resolved against the system clock as `now - D` per `main.rs:2795-2799`); the envelope always reports the single resolved value, so consumers cannot distinguish which input form the operator typed.
- `purged` (u64): the count of revoked-capability rows removed. May legitimately be `0` when no rows matched the cutoff — the verb does not error on an empty purge.

Top-level keys are pinned to exactly these three by the test at `agent-os/crates/covenant/src/main.rs:5863` (`capabilities_purge_json_pins_top_level_schema`).

The envelope source-of-truth lives at `capabilities_purge_json` in `agent-os/crates/covenant/src/main.rs:4384`. Two unit tests at `main.rs:5855` (`capabilities_purge_json_renders_stable_shape`) and `main.rs:5863` (`capabilities_purge_json_pins_top_level_schema`) cover the populated (`purged=3`) and empty (`purged=0`) cases. The CLI verb is wired at `main.rs:2776-2824`; without `--json`, the same response prints `purged <n> revoked capability(ies)`.

`covenant peers list [--limit <N>] [--prefix <P>] [--live-only|--revoked-only] --json` emits the registered peer roster filtered by the supplied flags. Envelope shape:

- `kind`: literal string `"peer_list"`.
- `limit` (u64): the request limit echoed back from `--limit` (default `20`, per `main.rs:3708`).
- `filter_pubkey_prefix` (string or null): the prefix echoed from `--prefix`, or `null` when the flag was omitted. Pinned at the type level by the schema test (`main.rs:5046-5049`) — never an integer or array.
- `matched_count` (u64): row count of the `peers` array; equals the exhaustive match count when `truncated` is `false`. Pinned as u64 by `main.rs:5051-5053` — never a string.
- `peers` (array of `PeerSummary`): the matched roster slice, see below.
- `operator_pubkey_b58` (string): the requesting operator's own pubkey in base58. The unsuffixed CLI line formatter at `peer_list_lines` (`main.rs:4238`) compares each peer's `pubkey_base58()` against this value to append a ` (self)` marker on the operator's own row; JSON consumers must apply the same comparison to render the self-tag, not assume the operator's row is reliably first.
- `truncated` (boolean): `true` when the registry held more matching entries than `limit`, `false` otherwise. Pinned as a JSON boolean by the schema test at `main.rs:5059-5062` — never `0`/`1`. **This is the only signal of incomplete results**; `matched_count == limit` with `truncated == false` means the page is the exhaustive match set, not a hint to paginate.

The inner `PeerSummary` shape, defined at `agent-os/crates/covenant-peer-auth/src/lib.rs:140`:

- `agent_id` (object) — `{display: string, pubkey: string (base58)}` per the `AgentId` Serialize impl at `covenant-types/src/lib.rs:124`.
- `token_prefix` (string) — 6-character redacted token prefix, the same value `peers revoke <token-prefix>` accepts. The full token bytes are never on the wire — same invariant as `Response::PeerList`.
- `registered_at` (u64) — Unix-epoch milliseconds when the peer registered.
- `revoked_at` (u64 or null) — Unix-epoch milliseconds when the peer was tombstoned; `null` for live entries. Composes with the `--live-only`/`--revoked-only` flags (and the equivalent `status_filter` query parameter described above) for filtering — the filter runs before the registry's truncation peek.

Top-level keys are pinned to exactly these seven by the test at `agent-os/crates/covenant/src/main.rs:5016` (`peer_list_json_pins_top_level_schema`), exercised against a populated two-peer (one live, one revoked) case and an empty case.

The envelope source-of-truth lives at `peer_list_json` in `agent-os/crates/covenant/src/main.rs:4220`. Schema and behavioral tests live at `main.rs:5016` (key set + per-key typing), `main.rs:4983` (`peer_list_json_echoes_prefix_and_match_count`), `main.rs:4997` (`peer_list_json_omits_prefix_when_inactive`), and `main.rs:5008` (`peer_list_json_reports_zero_match_count_for_empty_response`). The CLI verb is wired at `main.rs:3707-3760`; without `--json`, the same response is rendered line-by-line by `peer_list_lines` (`main.rs:4238`) with a `(truncated; <n> shown — narrow with --prefix or raise --limit)` hint appended when `truncated` is `true` (`main.rs:4269`). See also the **Query Parameters** section above for the same filter composition rules over the HTTP gateway.

`covenant peers purge --json` emits a summary of revoked-peer garbage collection. Envelope shape:

- `kind`: literal string `"peers_purged"` — the only structural disambiguator from `capabilities_purged`; both envelopes share the same three-key layout, so consumers that route on `kind` must check the full literal rather than treating any `*_purged` envelope as interchangeable.
- `before_ms` (u64): resolved Unix-epoch millisecond cutoff. The CLI accepts `--before-ms` or `--older-than-ms` with the same resolution semantics as `covenant capabilities purge --json` above.
- `purged` (u64): count of revoked-peer rows removed. Only revoked rows are eligible — the verb does not touch live peers (the unsuffixed CLI prints `purged <n> revoked peer(s)` at `main.rs:3664`). May legitimately be `0` when no rows matched.

Top-level keys are pinned to exactly these three by the test at `agent-os/crates/covenant/src/main.rs:5903` (`peers_purge_json_pins_top_level_schema`).

The envelope source-of-truth lives at `peers_purge_json` in `agent-os/crates/covenant/src/main.rs:4392`. Two unit tests at `main.rs:5895` (`peers_purge_json_renders_stable_shape`) and `main.rs:5903` cover the populated and empty cases. The CLI verb is wired at `main.rs:3624-3670`.

`covenant peers rotate --json` emits the new operator token after rotation. Envelope shape:

- `kind`: literal string `"peer_token_rotated"`.
- `token_b58` (string): the full base58 operator token. The value is the new authentication credential, not a fingerprint — the envelope is **secret-bearing** and JSON output must be treated as sensitive (no logging, no shell history capture, no transport over unsecured channels).

Top-level keys are pinned to exactly these two by the test at `agent-os/crates/covenant/src/main.rs:5942` (`peers_rotate_json_pins_top_level_schema`).

Side effects before the envelope returns (per the CLI comment at `main.rs:3684-3690`): the daemon has already persisted the new token to `$COVENANT_HOME/peers/operator.token` (mode `0600`), so the envelope is informational. Existing shells holding the previous token continue to authenticate with the old value until they re-read the file; consumers that cache the token in memory must refresh after rotation.

The envelope source-of-truth lives at `peers_rotate_json` in `agent-os/crates/covenant/src/main.rs:4400`. The shape-pinning tests at `main.rs:5935` (`peers_rotate_json_renders_stable_shape`) and `main.rs:5942` (`peers_rotate_json_pins_top_level_schema`) cover both a typical-token case and an empty-string defensive case (the latter exercises the key-set invariant rather than a legitimate runtime value). The CLI verb is wired at `main.rs:3671-3706`; without `--json`, the same response prints a two-line message terminating in the raw token value.

`covenant peers revoke <token-prefix> [--force] [--limit-matches <N>] --json` emits the outcome of revoking a single peer by its base58 token prefix. Envelope shape:

- `kind`: literal string `"peer_revoke"` — verb-form, not past-tense. Distinct from the sibling envelopes whose outcome names took the past-tense form (`capability_revoked`, `peer_token_rotated`, `peers_purged`); consumers routing on `kind` must match the literal exactly rather than guessing `peer_revoked` or `peers_revoke`.
- `outcome` (object): a tagged-enum `RevokeOutcome` (defined at `agent-os/crates/covenant-peer-auth/src/lib.rs:182` with `#[serde(tag = "type", rename_all = "snake_case")]`). The top-level object has exactly two keys (`kind` and `outcome`); the inner `outcome` is pinned by the schema test at `main.rs:5158-5161` to be a JSON object, never a string blob.

The five `RevokeOutcome` variants the daemon may return:

- `{type: "revoked", agent_id, token_prefix, registered_at, revoked_at}` — the unique live match was tombstoned. The four extra fields are the inlined `PeerSummary` shape documented in the `peer_list` block above; `revoked_at` carries the moment of revocation and is non-null for this variant.
- `{type: "already_revoked", agent_id, token_prefix, registered_at, revoked_at}` — same inlined `PeerSummary` shape; the unique match was already tombstoned. Idempotent — the operator's intent is satisfied — and `revoked_at` carries the *original* revocation timestamp, not the moment of this call.
- `{type: "not_found"}` — no entry's full base58 token matched the supplied prefix. No extra fields.
- `{type: "ambiguous", matches: [PeerSummary...], truncated: bool}` — more than one entry matched the prefix; the registry is unchanged. `matches.len()` is bounded by `--limit-matches`; `truncated` is `true` when more than that limit matched (see `RevokeOutcome::Ambiguous` at `covenant-peer-auth/src/lib.rs:207-211`). The field carries `#[serde(default)]` so a stale CLI built before `truncated` landed still deserialises a new daemon's response (degrading to the pre-bound assumption that the displayed matches are exhaustive); the daemon-side serializer always writes the field.
- `{type: "self_revoke_forbidden", agent_id, token_prefix, registered_at, revoked_at}` — same inlined `PeerSummary` shape; the unique live match is the operator's own bootstrap row and the request did not pass `--force`. The registry is unchanged and `revoked_at` is `null` (the entry remained live). This is defence-in-depth against the "fat-finger via web UI bypassed by curl" failure mode where a UI-only confirmation guard is trivially circumvented by a direct daemon API call.

**Exit-code coupling**: the `peer_revoke_is_failure` classifier at `agent-os/crates/covenant/src/main.rs:4675-4682` maps `not_found`, `ambiguous`, and `self_revoke_forbidden` to a CLI exit code of `1` — including in the `--json` path (`main.rs:3814-3816`). `revoked` and `already_revoked` map to exit `0`. JSON consumers must branch on `outcome.type` for success/failure semantics; transport success (exit `0`) is **not** synonymous with revocation success. The classifier's mapping is pinned by the test at `main.rs:7388` (`peer_revoke_json_exit_classification_matches_human_cli`).

Top-level keys are pinned to exactly these two by the test at `agent-os/crates/covenant/src/main.rs:5141` (`peer_revoke_json_pins_top_level_schema`), which also asserts `outcome` is a tagged-enum object and exercises both the `Ambiguous` and `NotFound` variants.

The envelope source-of-truth lives at `peer_revoke_json` in `agent-os/crates/covenant/src/main.rs:4613`. Two unit tests at `main.rs:5122` (`peer_revoke_json_renders_stable_ambiguous_shape`) and `main.rs:5141` cover the shape. The CLI verb is wired at `main.rs:3771-3871`; without `--json`, `Revoked` and `AlreadyRevoked` print tab-separated success lines to stdout, while `NotFound`, `Ambiguous`, and `SelfRevokeForbidden` print human-readable diagnostics to stderr before exiting `1`.

`covenant audit recent [-n|--limit <N>] [--since-ms <M>] [--stream] --json` emits a window of audit events. Envelope shape:

- `kind`: literal string `"audit_recent"`.
- `limit` (u64): the request limit echoed back from `-n`/`--limit` (default `50`, per `main.rs:3205`). Pinned as u64 at the schema test (`main.rs:6178-6181`) — never a string.
- `since_ms` (u64 or null): the Unix-epoch millisecond threshold echoed from `--since-ms`, or `null` when the flag was omitted. Pinned as u64-or-null at the schema test (`main.rs:6182-6185`) — never a string-of-integer. Same semantic as the HTTP gateway query parameter described in the **Query Parameters** section above: events whose `timestamp_ms` is strictly less than the threshold are dropped before the limit truncation.
- `events` (array of `AuditEvent`): the matched events. The array is empty when no events fall in the window.

The inner `AuditEvent` shape, defined at `agent-os/crates/covenant-audit/src/lib.rs:43`:

- `id` (string) — event UUID.
- `timestamp_ms` (u64) — Unix-epoch milliseconds when the event was recorded.
- `issuer` (object) — `{display: string, pubkey: string (base58)}` per the `AgentId` Serialize impl at `covenant-types/src/lib.rs:124`.
- `kind` (object) — tagged-enum `AuditKind` (defined at `covenant-audit/src/lib.rs:71` onwards) with a `type` discriminator (e.g., `"capability_granted"`, `"intent_dispatched"`, `"hermes_tool_invoked"`) and variant-specific extra fields. Consumers must route on `kind.type` before reading variant-specific fields.

Top-level keys are pinned to exactly these four by the test at `agent-os/crates/covenant/src/main.rs:6161` (`audit_recent_json_pins_top_level_schema`), exercised against three cases: populated with `since_ms`, empty with `since_ms`, and empty without `since_ms`.

The envelope source-of-truth lives at `audit_recent_json` in `agent-os/crates/covenant/src/main.rs:4422`. Two unit tests at `main.rs:6134` (`audit_recent_json_renders_stable_shape`) and `main.rs:6161` cover the shape. The CLI verb is wired at `main.rs:3204-3273`; without `--json`, the same response is rendered as JSONL (one `AuditEvent` per line at `main.rs:3267`) mirroring the durable `audit/events.jsonl` row shape, with `(no audit events)` printed at `main.rs:3264` when empty. The optional `--stream` flag sets `Request::RecentAudit.prefer_stream = Some(true)` (`main.rs:3234`), enabling the v2 streaming-response path documented under [docs/protocol-versioning.md](./protocol-versioning.md); the terminal-response shape is unchanged when the streaming path is not selected.

`covenant audit purge --json` emits a summary of time-bounded audit-log garbage collection. Envelope shape:

- `kind`: literal string `"audit_purged"`.
- `before_ms` (u64): resolved Unix-epoch millisecond cutoff. The CLI accepts `--before-ms` or `--older-than-ms` with the same resolution semantics as `covenant capabilities purge --json` above.
- `purged` (u64): count of audit events removed (the unsuffixed CLI message at `main.rs:3334` reads `purged <n> event(s)`, confirming the unit is an audit event, not a row class). May legitimately be `0` when no rows matched.

Unlike the capability- and peer-purge verbs, this removes hash-chain entries; the cutoff enforcement is bound to the `audit.purge` capability scope at dispatch time so a delegated caller cannot purge beyond its scope's `before_ms` (see `docs/capabilities.md`).

Top-level keys are pinned to exactly these three by the test at `agent-os/crates/covenant/src/main.rs:6102` (`audit_purge_json_pins_top_level_schema`).

The envelope source-of-truth lives at `audit_purge_json` in `agent-os/crates/covenant/src/main.rs:4414`. Two unit tests at `main.rs:6094` (`audit_purge_json_renders_stable_shape`) and `main.rs:6102` cover the populated (`purged=3`) and empty (`purged=0`) cases. The CLI verb is wired at `main.rs:3298-3340`.

`covenant audit verify --json` emits the audit-log hash-chain integrity report. Envelope shape:

- `kind`: literal string `"audit_integrity"` — past-tense outcome name, distinct from the verb name `verify` and from the workspace-level `verify_report` envelope; consumers routing on `kind` must match this literal exactly rather than reusing either of those tokens.
- `report` (object): a structured `covenant_audit::AuditIntegrityReport`, never a string blob. The top-level object has exactly two keys (`kind` and `report`); the inner `report` is pinned by the schema test at `main.rs:6256` to be a JSON object.

The inner `AuditIntegrityReport` shape, defined at `agent-os/crates/covenant-audit/src/lib.rs:61`:

- `events` (u64) — total audit events the integrity walk visited.
- `anchors` (u64) — count of anchor records (root-hash checkpoints) the walk crossed.
- `valid` (bool) — `true` when the hash chain is intact end-to-end; `false` when one or more failures were recorded.
- `root_hash_hex` (string) — the final root hash as lowercase hex, 64 characters (SHA-256). Pinned at the length level by the stable-shape test at `main.rs:6229`.
- `failures` (array of strings) — human-readable failure descriptions (e.g., `"chain hash mismatch at event 3"`), empty when `valid` is `true`.

Top-level keys are pinned to exactly these two by the test at `agent-os/crates/covenant/src/main.rs:6239` (`audit_verify_json_pins_top_level_schema`), exercised against both a valid and an invalid report.

The envelope source-of-truth lives at `audit_verify_json` in `agent-os/crates/covenant/src/main.rs:4435`. Two unit tests at `main.rs:6211` (`audit_verify_json_renders_stable_shape`) and `main.rs:6239` cover the shape. The CLI verb is wired at `main.rs:3275-3296`; without `--json`, the same response is printed as the bare `AuditIntegrityReport` JSON (no envelope wrapper) at `main.rs:3291`, so JSON consumers must use `--json` to get the kind-discriminated envelope — the unsuffixed output is structurally compatible with `report` but lacks the `kind` field.

`covenant memory purge --json` emits a summary of time-bounded memory-store garbage collection. Envelope shape:

- `kind`: literal string `"memory_purged"`.
- `tier` (string or null): the memory tier slug — exactly one of `"working"`, `"episodic"`, or `"longterm"` (one word, per `memory_tier_slug` at `main.rs:1719-1724`). Null when `--tier` was omitted, meaning the purge applied to all tiers. Note an input-form asymmetry: the CLI parser at `main.rs:1729-1731` accepts `longterm`, `long-term`, and `long_term` for the `--tier` argument, but only the `longterm` slug is ever emitted in the envelope.
- `before_ms` (u64): resolved Unix-epoch millisecond cutoff. Same `--before-ms` / `--older-than-ms` resolution semantics as `covenant capabilities purge --json` above.
- `purged` (u64): count of memory records removed. The unsuffixed CLI prints `purged <n> record(s)` at `main.rs:2171`, confirming the unit is a memory record. May legitimately be `0` when no rows matched.

Top-level keys are pinned to exactly these four by the test at `agent-os/crates/covenant/src/main.rs:6294` (`memory_purge_json_pins_top_level_schema`), which also exercises the null-tier case.

The envelope source-of-truth lives at `memory_purge_json` in `agent-os/crates/covenant/src/main.rs:4442`. Two unit tests at `main.rs:6282` (`memory_purge_json_renders_stable_shape`, both a Working-tier populated case and a no-tier null case) and `main.rs:6294` cover the populated and empty (`purged=0`, no-tier) cases. The CLI verb is wired at `main.rs:2127-2177`.

`covenant memory recent [--tier <T>] [-n|--limit <N>] [--stream] --json` and `covenant memory search <query> [--tier <T>] [-n|--limit <N>] [--min-relevance <R>] --json` both emit the same memory-read envelope, distinguished only by the `mode` discriminator. Envelope shape:

- `kind`: literal string `"memory_read"`.
- `mode` (string): exactly one of `"recent"` or `"search"` (lowercase, matching the CLI verb name — no other values are emitted). Consumers must route on `mode` to know which null pattern to expect across `query` and `min_relevance`.
- `tier` (string or null): the requested `MemoryTier` as its lowercase wire slug — exactly one of `"working"`, `"episodic"`, or `"longterm"` (one word, per `MemoryTier`'s `#[serde(rename_all = "lowercase")]` at `covenant-types/src/lib.rs:23` and the slug map at `memory_tier_slug` in `main.rs:1719-1724`). The CLI parser accepts `longterm`, `long-term`, and `long_term` as input forms for `--tier`, but only the `longterm` slug is ever emitted. `null` when `--tier` was omitted (meaning the request applied to all tiers). Pinned as string-or-null by the schema test (`main.rs:6616-6619`) — never a structured object.
- `limit` (u64): the request limit echoed back from `-n`/`--limit` (default `10` for both verbs, per `main.rs:2084` and `main.rs:2494`). Pinned as u64 at the schema test (`main.rs:6612-6615`).
- `query` (string or null): for `mode="search"`, the request query (whitespace-joined when the operator passed multiple positional tokens, per `main.rs:2529`). For `mode="recent"`, always `null` (the recent verb does not accept a query). Pinned as string-or-null by the schema test (`main.rs:6620-6623`).
- `min_relevance` (number or null): for `mode="search"`, the float echoed from `--min-relevance` (validated to a finite `f32` in `[0.0, 1.0]` at `main.rs:2517-2521`), or `null` when the flag was omitted. For `mode="recent"`, always `null`. Pinned as f64-or-null by the schema test (`main.rs:6624-6627`) — never a string.
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

Top-level keys are pinned to exactly these seven by the test at `agent-os/crates/covenant/src/main.rs:6586` (`memory_read_json_pins_top_level_schema`), exercised against both a `mode="search"` case (populated `query`, `min_relevance`, non-empty `records`) and a `mode="recent"` case (null `query`, null `min_relevance`, empty `records`).

The envelope source-of-truth lives at `memory_read_json` in `agent-os/crates/covenant/src/main.rs:4470`. Two unit tests at `main.rs:6543` (`memory_read_json_renders_stable_shape`) and `main.rs:6586` cover both modes. The CLI verbs are wired at `main.rs:2082-2126` (`covenant memory recent`) and `main.rs:2488-2554` (`covenant memory search`); without `--json`, each record prints as `[<created_at>] <tier>: <text>` at `main.rs:1629`. The optional `--stream` flag is accepted only by `covenant memory recent` (per `main.rs:2101`) and sets `Request::RecentMemory.prefer_stream = Some(true)` to enable the v2 streaming-response path documented under [docs/protocol-versioning.md](./protocol-versioning.md); the terminal envelope shape is unchanged when the streaming path is not selected. `covenant memory search` has no `--stream` flag.

## Human Authority

The decision to bump the IPC/HTTP protocol, the wire shapes that change, the migration window, and the public release notes for v2 remain human-owned. Automation keeps this contract documented and validated; with the v2 `StreamEnvelope` fixtures landed under ADR 0010, the validator now runs in strict mode rather than dormant. It must not introduce v2 fixtures, edit `PROTOCOL_VERSION`, or relax the migration-note pairing without an approved decision.
