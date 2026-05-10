# Settlement Receipt Migration

Local settlement receipts are append-only JSONL records. Current daemon-created memory receipts include `memory_record_id`; older rows may only show payer, resource, credits, and settlement time. Migration work must begin with a dry-run inventory because rewriting receipts changes the evidence used by verification, batching, and future on-chain anchoring.

## Dry-Run Planner

```bash
node agent-os/scripts/settlement-receipt-migration.mjs --json
```

The planner reads `$COVENANT_HOME/receipts/working.jsonl` by default. Use `--receipts <path>` for an exported ledger fixture and `--limit <n>` to scan the most recent rows. The JSON envelope uses schema `covenant.settlement.receipt_migration.plan.v1` and reports:

- parsed versus malformed JSONL rows;
- memory receipts that already carry `memory_record_id`;
- legacy memory receipts that need a memory-record match;
- non-memory receipts excluded from memory backfill;
- batched versus unbatched receipt counts;
- the evidence required before any future mutation command can exist.

The planner does not export receipt file paths, payer display strings, malformed row contents, private keys, or peer tokens.

## Mutation Boundary

`--apply` is rejected. The planner does not rewrite the JSONL file, create audit rows, or synthesize memory correlations.

Any future mutation command must be separate from this planner and must provide:

- explicit authorization through the daemon;
- a rollback snapshot for the receipt JSONL;
- before and after receipt hashes;
- the memory record id being attached to each legacy row;
- payer pubkey evidence that links the receipt payer to the memory owner;
- an audit event id for every applied migration batch.

Malformed receipt rows are blockers for mutation design. They must be quarantined or repaired with explicit operator review before a backfill command is allowed to modify the ledger.

## Relation To Memory Backfill Planning

`covenant memory plan-receipt-backfill --json` works through daemon read surfaces and proposes candidate memory-to-receipt correlations for recent rows.

`settlement-receipt-migration.mjs` works directly over a receipt JSONL export and is meant for ledger-level migration review. It can distinguish malformed rows from well-formed legacy rows before a mutation design is considered.
