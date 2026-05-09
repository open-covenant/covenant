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
| `memory_stale_parent` | A memory record points at a parent memory id that no longer exists. |
| `capability_without_audit` | A capability grant exists without a matching `CapabilityGranted` audit row. |
| `memory_receipt_mismatch` | Memory records and memory settlement receipts differ for an owner in the sampled window. |
| `memory_without_receipt` | A memory record has no exact or legacy-compatible settlement receipt. |
| `receipt_without_memory_record` | A settlement receipt references a missing memory record. |
| `memory_receipt_duplicate` | More than one settlement receipt references the same memory record. |
| `memory_receipt_owner_mismatch` | A receipt references a memory record owned by a different payer. |

## Operator Posture

The verifier is intentionally non-mutating. Drift is evidence, not an automatic delete instruction.

Safe handling order:

1. Inspect the drift item and the underlying state file.
2. Decide whether the record is valid, stale, missing provenance, or externally mutated.
3. Prefer a future explicit repair command over ad hoc file edits.
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
