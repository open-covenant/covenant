# Memory Maintenance

Covenant memory maintenance is operator-controlled. The daemon can already compute dry-run and apply compaction outcomes; the CLI now exposes a dedicated planning command for scheduled maintenance loops.

## Read-Only Plan

```bash
covenant memory plan-compaction \
  --reason "scheduled maintenance dry run" \
  --delete-working-older-than-ms 86400000 \
  --delete-episodic-older-than-ms 2592000000 \
  --mark-longterm-stale-older-than-ms 7776000000 \
  --detach-stale-parents \
  --json
```

`plan-compaction` is read-only. It always sends a dry-run `CompactMemory` request, rejects `--apply`, and emits a `memory_compaction_plan` JSON envelope by default. The envelope includes the candidate memory mutations computed by the daemon and an `expected_receipt_changes` section.

Current receipt behavior is intentionally conservative:

```json
{
  "expected_receipt_changes": {
    "mode": "none",
    "records": [],
    "reason": "dry-run compaction planning does not mutate memory or settlement receipts"
  }
}
```

This means scheduled jobs can publish and review candidate compaction work without changing memory state or backfilling settlement receipts.

## Receipt Backfill Plan

Legacy receipt rows may predate exact `memory_record_id` correlation. Operators can now produce a read-only candidate plan:

```bash
covenant memory plan-receipt-backfill --limit 100 --json
```

The plan reads recent memory records and recent receipts through existing daemon read surfaces. It proposes candidate correlations only when a legacy memory receipt has no `memory_record_id` and the same payer has an uncorrelated memory record in the requested window. It also lists unmatched legacy receipts and unmatched memory records so a reviewer can see why no candidate was proposed.

`plan-receipt-backfill` rejects `--apply`. It does not mutate memory, rewrite settlement receipts, or emit audit rows. Any future mutation path must be a separate command with explicit before/after receipt evidence and authorization.

## Apply Boundary

Use `covenant memory compact --apply` only after a dry-run plan has been reviewed and the operator has granted the matching `memory.compaction.apply` capability. Apply mode mutates memory and records daemon audit evidence. Receipt backfill mutation for legacy uncorrelated rows is still future work; the current receipt-backfill command is a dry-run planning surface only.

## Scheduler Contract

A safe scheduler should:

- run `plan-compaction --json` first;
- store the JSON plan with the validation or sprint evidence for that run;
- apply only when the operator policy says the plan is acceptable;
- never synthesize or backfill receipts during the read-only planning step;
- run `plan-receipt-backfill --json` before designing any receipt mutation;
- escalate if candidate deletions affect records whose settlement receipts cannot be reconciled.
