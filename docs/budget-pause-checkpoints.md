# Budget Pause Checkpoints

Budget pause checkpoints are the v1 persistence primitive for stopping in-flight work without losing budget context or charging the same work twice on resume.

## Implemented Boundary

The current implementation lives in `covenant-types` and `covenant-budget`.

- `BudgetPauseCheckpoint` is the shared wire type.
- `JsonlPauseCheckpointStore` persists checkpoint events as JSONL.
- `pause_saved` records one active checkpoint per `(intent_id, agent pubkey)`.
- `resume_claimed` marks the active checkpoint as consumed.
- `claim_resume` is single-use: a second resume attempt for the same active checkpoint fails.
- Resume claims do not mutate the budget ledger or append a debit. The debit that funded already-started work remains the only spend record.
- `resume_state` must be portable JSON. Machine-local absolute paths are rejected and are not echoed in error messages.

This gives the daemon a durable handoff record before the broader runtime pause/resume loop is wired through execution.

## Runtime Integration Path

The daemon should use the checkpoint store when an in-flight task must stop before completion:

1. Save a checkpoint with the intent, agent, requested credits, live token count, refill ETA, reason, and portable resume state.
2. Stop or suspend the active runner.
3. On resume, atomically claim the checkpoint before dispatching the remaining work.
4. Reject duplicate resume attempts after the claim has landed.
5. Leave the budget ledger unchanged unless new work actually consumes new credits.

The checkpoint store is deliberately separate from the token bucket ledger. The ledger records resource consumption; the checkpoint store records resumability.

## Verification

Run the focused gate from `agent-os/`:

```bash
cargo test -p covenant-budget --locked
```

The current tests cover stable event shape, replay across reopen, single-use resume claims, ledger spend-once invariants, and rejection of machine-local resume paths.
