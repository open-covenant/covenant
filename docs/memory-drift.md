# Memory Drift Reports

`covenant verify` is a read-only consistency check across memory, audit, capability, and settlement state. It does not mutate records. Add `--json` to emit one stable `verify_report` object for supervisors.

The verifier returns two layers:

- `checks`: aggregate pass/fail rows suitable for humans and dashboards.
- `drift`: machine-readable repair candidates with a kind, optional id, evidence message, and safe next repair action.

## Drift Kinds

| Kind | Meaning |
| --- | --- |
| `memory_without_audit` | A memory record has no matching `IntentDispatched` audit row. |
| `audit_without_memory` | An `IntentDispatched` audit row has no matching memory record in the sampled window. |
| `intent_dispatched_duplicate` | Two or more `IntentDispatched` audit rows share the same `intent_id`. |
| `memory_stale_parent` | A memory record points at a parent memory id that no longer exists. |
| `memory_self_parent` | A memory record's parent reference points at the record's own id. |
| `memory_parent_cycle` | A memory record's parent chain loops back on itself two or more hops up (e.g. A → B → A). |
| `capability_without_audit` | A capability grant exists without a matching `CapabilityGranted` audit row. |
| `memory_receipt_mismatch` | Memory records and memory settlement receipts differ for an owner in the sampled window. |
| `memory_without_receipt` | A memory record has no exact or legacy-compatible settlement receipt. |
| `receipt_without_memory_record` | A settlement receipt references a missing memory record. |
| `memory_receipt_duplicate` | More than one settlement receipt references the same memory record. |
| `memory_receipt_owner_mismatch` | A receipt references a memory record owned by a different payer. |
| `memory_receipt_resource_mismatch` | A settlement receipt carries `memory_record_id` but reports a non-Memory `resource`. |
| `memory_receipt_settled_before_created` | A settlement receipt's `settled_at` precedes the correlated memory record's `created_at`. |
| `memory_empty_text` | A memory record's `text` is empty; the record cannot anchor retrieval and usually indicates a tool emitter that dropped its result body. |
| `memory_nan_embedding` | A memory record's `embedding` contains NaN values; cosine similarity poisons every ranking the record competes in. |
| `memory_record_id_nil` | A memory record has `id == Uuid::nil()` (the all-zero UUID). Every production memory write allocates the id via `Uuid::new_v4()` (either flowing through from `Intent.id` at IPC ingest or freshly allocated at write time), which never produces the nil UUID. A nil id is therefore evidence of a serde regression (`Uuid::default()` is nil), an import tool that constructed records without `Uuid::new_v4()`, or an operator SQLite edit that broke the `memory_record_id` back-reference settlement receipts and audit `IntentDispatched` rows correlate on. |
| `memory_record_created_at_zero` | A memory record has `created_at == 0`. Every production memory write stamps `created_at` from `epoch_ms()` (either flowing through `Intent.issued_at` — itself `epoch_ms()` at IPC ingest — or a fresh `epoch_ms()` at write time), which returns 0 only when the system clock predates 1970-01-01 (impossible). A zero `created_at` is therefore evidence of a serde regression, an import tool that bypassed `epoch_ms()`, or an operator SQLite edit that anonymized when the record was written. |
| `memory_record_owner_pubkey_zeroed` | A memory record has `owner.pubkey == [0u8; 32]` (the all-zero ed25519 verifying key). Every production memory write sources `owner.pubkey` from either an authenticated peer's `AgentId` (`issuer.clone()` on the intent path) or from `LocalIdentity::pubkey_bytes` (operator-initiated writes). ed25519 verifying keys are never the all-zero sequence, and a peer whose token resolved to a zeroed pubkey would fail signature verification on every capability check. A zeroed owner pubkey is therefore evidence of a serde regression (`Default` for `[u8; 32]` is all-zero), an import or replay tool that constructed records with a placeholder `AgentId`, or an operator SQLite edit that broke the owner-based scope, capability, and retrieval routing the daemon relies on. |
| `receipt_confirmed_without_chain` | A settlement receipt carries `confirmed_at` but `chain` is unset; only `annotate_receipt` writes `confirmed_at`, and it always sets `chain` from the same confirmation. |
| `receipt_chain_partial` | A settlement receipt has a strict subset (1-3 of 4) of the `chain`/`cluster`/`batch_id`/`merkle_root` bundle set; `annotate_receipt` writes the bundle as a unit. |
| `receipt_tx_sig_onchain_sig_diverged` | A settlement receipt has both `tx_sig` and `onchain_sig` populated but the values disagree; `annotate_receipt` writes both fields from the same `confirmation.tx_sig.clone()`, so a divergence is out-of-band. `Some`+`None` in either direction is tolerated for legacy/forward compatibility. |
| `receipt_settled_at_zero` | A settlement receipt has `settled_at == 0`. Every production receipt write stamps `settled_at` via `epoch_ms()`, which returns 0 only when the system clock predates 1970-01-01 (impossible). A zero `settled_at` is therefore evidence of a serde regression, a writer that bypassed `epoch_ms()`, or an operator JSONL edit that anonymized when the receipt was issued. The settlement-receipt JSONL has no chain-hash anchor covering this invariant. |
| `receipt_id_nil` | A settlement receipt has `id == Uuid::nil()` (the all-zero UUID). Every production receipt write allocates the id via `Uuid::new_v4()`, which never produces the nil UUID. A nil id is therefore evidence of a serde regression (`Uuid::default()` is nil), an import or replay tool that constructed receipts without `Uuid::new_v4()`, or an operator JSONL edit that broke the memory-record `metadata.receipt_id` back-reference and chain-batch correlation, both of which key on the receipt id. |
| `receipt_payer_pubkey_zeroed` | A settlement receipt has `payer.pubkey == [0u8; 32]` (the all-zero ed25519 verifying key). Every production receipt write sources `payer.pubkey` from either an authenticated peer's `AgentId` (`issuer.clone()` on the intent settlement path) or from `LocalIdentity::pubkey_bytes` (daemon-initiated settlement). ed25519 verifying keys are never the all-zero sequence, and a peer whose token resolved to a zeroed pubkey would fail signature verification on every capability check. A zeroed payer pubkey is therefore evidence of a serde regression (`Default` for `[u8; 32]` is all-zero), an import or replay tool that constructed receipts with a placeholder `AgentId`, or an operator JSONL edit that broke the payer-based credit accounting, per-peer settlement queries, and on-chain bridge correlation the daemon relies on. |
| `audit_event_timestamp_zero` | An audit event has `timestamp_ms == 0`. Every production audit write goes through `epoch_ms()`, which returns 0 only when the system clock predates 1970-01-01 (impossible). A zero timestamp is therefore evidence of a serde regression, a writer that bypassed `epoch_ms()`, or an operator JSONL edit that anonymized when the event was recorded. The `AuditIntegrityReport` chain hash covers byte-tampering of the persisted file, not this semantic invariant. |
| `audit_event_id_nil` | An audit event has `id == Uuid::nil()` (the all-zero UUID). Every production audit write allocates the id via `Uuid::new_v4()`, which never produces the nil UUID. A nil id is therefore evidence of a serde regression (`Uuid::default()` is nil), an import or replay tool that constructed events without `Uuid::new_v4()`, or an operator JSONL edit that broke cross-event correlation by id. The chain hash protects byte integrity of the row, not the semantic invariant that an event id is unique and addressable. |
| `audit_event_issuer_pubkey_zeroed` | An audit event has `issuer.pubkey == [0u8; 32]` (the all-zero ed25519 verifying key). Every production audit write sources `issuer.pubkey` either from `LocalIdentity::pubkey_bytes` (daemon-issued events) or from an authenticated peer's `AgentId` (peer-issued events). ed25519 verifying keys are never the all-zero sequence, and a peer whose token resolves to a zeroed pubkey would fail signature verification on every capability check. A zeroed issuer pubkey is therefore evidence of a serde regression (`Default` for `[u8; 32]` is all-zero), an import or replay tool that constructed events with a placeholder `AgentId`, or an operator JSONL edit that anonymized issuer attribution and collapsed incident-response correlation to one identity. |
| `capability_action_empty` | A capability grant has an empty `action` string. Every production capability grant routes through `grant_capability`, which builds the `Capability` struct from a non-empty caller-supplied action (e.g. `memory.read`, `tool.call.web`, `a2a.send`). `validate_scope` short-circuits to Ok for empty actions because `ScopeNamespace::from_action` returns `None` on no-prefix match, so an empty-action capability still signs and persists in `granted.jsonl`. The cap is a no-op at use time — capability checks compare against required action strings that are always non-empty — but the row is structural evidence of a serde regression (`String::default()` is empty), a CLI bug that forwarded an empty `--action`, or a JSONL edit that anonymized the action while preserving the signature. |
| `capability_subject_pubkey_zeroed` | A capability grant has `subject.pubkey == [0u8; 32]` (the all-zero ed25519 verifying key). Every production capability grant sources `subject.pubkey` from either an authenticated peer's `AgentId` (`peer.clone()` at grant dispatch) or from `LocalIdentity::pubkey_bytes` (operator self-grants). ed25519 verifying keys are never the all-zero sequence, and a peer whose token resolved to a zeroed pubkey would fail signature verification on every transport check before reaching `grant_capability`. A zeroed subject pubkey is therefore evidence of a serde regression (`Default` for `[u8; 32]` is all-zero), an import or replay tool that constructed grants with a placeholder `AgentId`, or a JSONL edit that anonymized the subject so per-peer capability scope queries collapse zeroed subjects into one bucket. |
| `capability_grantor_pubkey_zeroed` | A capability grant has `granted_by.pubkey == [0u8; 32]` (the all-zero ed25519 verifying key). Every production capability grant sources `granted_by` from `self.identity.agent_id()` at grant dispatch — the daemon's local trust root, whose `AgentId.pubkey` is the 32-byte ed25519 verifying key derived from the daemon's signing key. ed25519 verifying keys are never the all-zero sequence, and `verify_with_clock` rejects any cap whose `granted_by` does not match the daemon trust root as `UntrustedGrantor`. A zeroed grantor pubkey is therefore evidence of a serde regression (`Default` for `[u8; 32]` is all-zero), an import tool that constructed grants without `self.identity.agent_id()`, or a JSONL edit that detached the row from the trust root, collapsing capability provenance across operator rotations and breaking incident-response correlation by grantor. |
| `capability_expires_at_zero` | A capability grant has `expires_at == Some(0)`. The cap is permanently expired before any check window — `verify_with_clock` rejects any cap whose `expires_at` is in the past — and no production grant path records this value (callers pass `None` for perpetual caps or `Some(future_epoch_ms)` for time-bounded caps). A `Some(0)` expiry is therefore evidence of a serde regression (e.g. `expires_at` rewritten as `u64` with `#[serde(default)]`), a CLI bug that defaulted a missing `--expires-at` to `Some(0)`, or a JSONL edit that disabled the cap without going through `revoke_capability` (which would record a `CapabilityRevoked` audit row). |
| `capability_signature_zeroed` | A capability grant has `signature == [0u8; 64]` (the all-zero ed25519 signature). Every production capability grant routes through `sign_capability`, which signs the canonical capability bytes with the daemon's ed25519 signing key. ed25519 signatures are never the all-zero 64-byte sequence — even an empty message signed by a real key produces non-zero bytes — and `verify_with_clock` would reject an all-zero signature at every capability check. An all-zero signature is therefore evidence of a serde regression (`Default` for `[u8; 64]` is all-zero), an import tool that skipped `sign_capability`, or a JSONL edit that wiped the signature to detach the row from the trust root. Co-fires `capability_without_audit` because the zeroed signature has no matching `CapabilityGranted.signature_b58` audit row. |
| `audit_capability_granted_signature_b58_empty` | An audit event has `kind == AuditKind::CapabilityGranted { signature_b58: "", .. }`. Every production `CapabilityGranted` audit write builds the variant with `signature_b58` sourced from `bs58::encode(signed.signature).into_string()`, where `signed.signature` is the 64-byte ed25519 signature produced by `sign_capability`. base58-encoding 64 bytes always produces a non-empty string. An empty `signature_b58` is therefore evidence of a serde regression (`String::default()` is empty), an audit import tool that constructed the row before computing the signature, or a JSONL edit that wiped the trust-root binding from the audit row. The empty value detaches the row from the `signature_b58` join key used by the `capability ↔ audit` correlation, masking otherwise-orphaned capability rows behind a row whose top-level fields (`id`, `timestamp_ms`, `issuer`) all look valid. |
| `audit_capability_revoke_rejected_signature_b58_empty` | An audit event has `kind == AuditKind::CapabilityRevokeRejected { signature_b58: "", .. }`. Every production `CapabilityRevokeRejected` audit row is written from `revoke_capability`, which decodes the request's `signature_b58` via `bs58::decode` and only reaches the audit write after the decoded byte slice has exactly 64 entries. The recorded `signature_b58` is therefore the original non-empty base58 string of a 64-byte ed25519 signature. An empty `signature_b58` is evidence of a serde regression, an import tool that constructed the row without the original decoded signature, or a JSONL edit that wiped the join key — detaching the rejection row from the matching `CapabilityGranted` row and breaking every cap-grant ↔ revoke-rejection correlation. |
| `receipt_memory_record_id_nil` | A settlement receipt has `memory_record_id == Some(Uuid::nil())`. Production receipt writes either pass `None` for resource paths with no associated memory record, or `Some(memory_id)` where `memory_id` is allocated via `Uuid::new_v4()` at the matching memory write. `Uuid::new_v4()` never produces the nil UUID. A `Some(Uuid::nil())` is therefore evidence of a serde regression (`Uuid::default()` is nil), an import or replay tool that constructed receipts with a placeholder Uuid, or a JSONL edit that detached the row from the receipt ↔ memory record correlation while preserving payer, settled_at, and credits_consumed so the row still accounts for the credit burn. Co-fires nothing else — `receipt_id_nil` checks `receipt.id`, not `receipt.memory_record_id`. |
| `audit_intent_dispatched_result_hash_hex_empty` | An audit event has `kind == AuditKind::IntentDispatched { result_hash_hex: "", .. }`. Every production `IntentDispatched` audit write sources `result_hash_hex` from `covenant_audit::hash_hex(text_out.as_bytes())`, which produces a 64-character SHA-256 hex string and never the empty string. An empty `result_hash_hex` is therefore evidence of a serde regression (`String::default()` is empty), an audit import tool that reconstructed the row without the original hash, or a JSONL edit that wiped the join key used to correlate the dispatch row with the `SettlementReceipt` that pins the same `text_out`. The empty value detaches the row from the `result_hash_hex` join key used by the `audit ↔ receipt` correlation. |
| `audit_hermes_tool_invoked_preview_hash_hex_empty` | An audit event has `kind == AuditKind::HermesToolInvoked { preview_hash_hex: "", .. }`. Every production `HermesToolInvoked` audit write sources `preview_hash_hex` from `covenant_audit::hash_hex(preview.as_bytes())` in `runtime_trace_to_audit_kind`, enforcing the documented redaction floor that Hermes tool-input previews are SHA-256-hashed before the audit chain persists them. SHA-256 hex is 64 lowercase characters and never empty. An empty `preview_hash_hex` is therefore evidence of a serde regression (`String::default()` is empty), an audit import tool that reconstructed the row without the original hash, or a JSONL edit that wiped the redaction marker. The empty value erases the redaction binding and detaches the row from any audit-chain consumer that uses `preview_hash_hex` to verify the redaction floor. |

## Operator Posture

The verifier is intentionally non-mutating. Drift is evidence, not an automatic delete instruction.

Safe handling order:

1. Inspect the drift item and the underlying state file.
2. Decide whether the record is valid, stale, missing provenance, or externally mutated.
3. Prefer the explicit repair commands (Repair Contract below) over ad hoc file edits.
4. Preserve useful long-term memory unless there is clear evidence it is stale or unsafe.

## Repair Contract

The memory crate defines explicit repair requests with two modes:

- `dry_run`: compute the exact before/after shape without mutating the store.
- `apply`: perform the mutation after the same checks pass.

Every repair request requires a non-empty reason. Supported crate-level commands are:

| Command | Use | Safety guard |
| --- | --- | --- |
| `detach_parent` | Clear a stale `parent` reference after inspection. | Optional `expected_parent` prevents detaching if the record changed since the drift report. |
| `delete_record` | Remove a memory record confirmed to be unsafe, invalid, or unwanted. | Dry-run reports the deletion without mutating; apply requires an explicit reason and capability. |
| `backfill_provenance` | Add provenance evidence under `metadata.provenance`. | Rejects null provenance payloads and preserves existing metadata. |

The daemon exposes the same repair request shape over IPC and HTTP `POST /memory/repair`. The CLI defaults to dry-run and requires `--apply` before mutation:

```bash
covenant capabilities grant memory.repair.dry_run
covenant memory repair detach-parent <memory-id> \
  --expected-parent <parent-id> \
  --reason "verified stale parent"

covenant capabilities grant memory.repair.apply
covenant memory repair backfill-provenance <memory-id> \
  --provenance '{"source":"audit-reconciliation"}' \
  --reason "verified missing provenance" \
  --apply
```

Dry-run calls require `memory.repair.dry_run`; apply calls require `memory.repair.apply`. Successful dry-runs and mutations record `memory_repair_applied` audit rows containing the memory id, action, mode, changed flag, and operator reason. Full before/after records stay in the repair response rather than being duplicated into the audit log.

## Compaction Contract

Compaction is separate from targeted repair. It is an operator-only maintenance request that computes one deterministic plan over the current memory snapshot. The same request shape is available over IPC, HTTP `POST /memory/compact`, and the CLI.

Supported policy fields:

| Field | Effect |
| --- | --- |
| `delete_working_before_ms` | Delete working-tier records older than the cutoff. |
| `delete_episodic_before_ms` | Delete episodic records older than the cutoff. |
| `mark_longterm_stale_before_ms` | Keep long-term records, but mark matching records under `metadata.stale_context`. |
| `detach_stale_parents` | Clear parent ids that do not resolve or are deleted by the same compaction plan. |
| `marked_at_ms` | Optional deterministic timestamp for stale-context markers; the daemon fills it when omitted. |

The CLI defaults to dry-run and requires `--apply` before mutation:

```bash
covenant capabilities grant memory.compact.dry_run
covenant memory compact \
  --delete-working-older-than-ms 86400000 \
  --detach-stale-parents \
  --reason "daily working-memory compaction"

covenant capabilities grant memory.compact.apply
covenant memory compact \
  --delete-working-older-than-ms 86400000 \
  --delete-episodic-older-than-ms 2592000000 \
  --mark-longterm-stale-older-than-ms 7776000000 \
  --detach-stale-parents \
  --reason "monthly memory hygiene" \
  --apply
```

The CLI prints a bare `MemoryCompactionOutcome` JSON object by default. Use `--json` for a stable envelope:

```bash
covenant memory compact --reason "monthly memory hygiene" --json ...
```

```json
{ "kind": "memory_compacted", "outcome": { "mode": "dry_run", "changed": false } }
```

Dry-run calls require `memory.compact.dry_run`; apply calls require `memory.compact.apply`. Successful dry-runs and mutations record `memory_compaction_applied` audit rows containing the mode, changed flag, operator reason, deleted ids, stale-marked ids, and detached-parent ids. Long-term memory is not deleted by compaction; it is marked stale so future retrieval policy can decide how to treat it.

## Receipt Correlation

Memory settlement receipts now carry an optional `memory_record_id` field that points at the originating `MemoryRecord.id` when daemon memory writes produce the receipt. `covenant verify` joins on that id first, then falls back to owner/resource counts only for older receipt rows that predate the field. Exact drift surfaces as `memory_without_receipt`, `receipt_without_memory_record`, `memory_receipt_duplicate`, or `memory_receipt_owner_mismatch`; aggregate count drift still surfaces as `memory_receipt_mismatch` when exact pairing is impossible.

The settlement crate now exposes a rollback-backed receipt backfill primitive for this migration boundary. `backfill_receipts_with_correlations` rewrites a receipt JSONL only after writing and fsyncing a sibling rollback checkpoint, then applies explicit `receipt_id -> memory_record_id` correlations and canonicalizes serde-decodable legacy rows. The convenience `backfill_receipts(path, dry_run)` wrapper performs only safe canonical row repair because the settlement crate cannot infer memory ids by itself.

The operator-facing path is now wired end-to-end on the memory side. `covenant memory backfill-receipt-correlation --json` (HTTP equivalent: `POST /memory/records/backfill`) applies by default; pass `--dry-run` to report the row_count an apply would change without writing. Dry-run requires the `memory.backfill.dry_run` capability; apply requires `memory.backfill.apply` — see [docs/capabilities.md](./capabilities.md) for the scope contract. The daemon recomputes correlations server-side from the operator's own memory and receipt rows; clients cannot supply correlations directly, which keeps a peer holding `memory.backfill.apply` from rewriting arbitrary `metadata.receipt_id` values by inventing pairings. A successful apply wraps the per-row updates in a SQLite SAVEPOINT named `backfill_receipt_correlation` so a per-row failure rolls the entire batch back to zero rows changed, then emits an operator-issued `MemoryRecordBackfillApplied` audit row carrying `row_count`, `savepoint_name`, and `dry_run`. The read-only verifier and `covenant memory repair` primitives remain the right tools for drift items outside the legacy-receipt-correlation surface (e.g., stale parent references, invalid records, missing provenance).
