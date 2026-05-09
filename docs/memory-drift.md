# Memory Drift Reports

`covenant verify` is a read-only consistency check across memory, audit, capability, and settlement state. It does not mutate records.

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

## Operator Posture

The alpha verifier is intentionally non-mutating. Drift is evidence, not an automatic delete instruction.

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
| `delete_record` | Remove a memory record confirmed to be unsafe, invalid, or unwanted. | Dry-run reports the deletion without mutating. |
| `backfill_provenance` | Add provenance evidence under `metadata.provenance`. | Rejects null provenance payloads and preserves existing metadata. |

Daemon, CLI, and audit-log exposure are still pending. Until those surfaces are wired, these repair primitives are available inside the memory crate only.

## Current Limits

The receipt check compares counts by owner and resource inside the sampled window. Settlement receipts do not yet carry the memory record id, so the verifier cannot prove exact record-to-receipt pairing. A later schema revision should add a direct correlation id.
