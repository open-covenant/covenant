# Budget Pause Checkpoints

Budget pause checkpoints are the v1 persistence primitive for stopping in-flight work without losing budget context or charging the same work twice on resume.

## Implemented Boundary

The implementation spans `covenant-types`, `covenant-budget`, and the daemon runtime.

- `BudgetPauseCheckpoint` is the shared wire type.
- `JsonlPauseCheckpointStore` persists checkpoint events as JSONL.
- `pause_saved` records one active checkpoint per `(intent_id, agent pubkey)`.
- `resume_claimed` marks the active checkpoint as consumed.
- `claim_resume` is single-use: a second resume attempt for the same active checkpoint fails.
- `covenantd` opens `$COVENANT_HOME/budget/checkpoints.jsonl` at startup.
- Budget-exhausted dispatches save a checkpoint before returning the rejection.
- `covenant intents resume` claims the checkpoint before redispatching a checkpointed intent.
- Operator-requested shutdown saves active budgeted dispatch checkpoints before the daemon exits.
- Resume claims do not mutate the budget ledger or append a debit. The debit that funded already-started work remains the only spend record.
- `resume_state` must be portable JSON. Machine-local absolute paths are rejected and are not echoed in error messages.

This gives the daemon a durable handoff record for the implemented pause sources, distinct from the hard-preempt path, which kills the subprocess rather than suspending it for resume.

## Runtime Integration Path

The daemon uses the checkpoint store when an in-flight task must stop before completion:

1. Save a checkpoint with the intent, agent, requested credits, live token count, refill ETA, reason, and portable resume state.
2. Stop or suspend the active runner.
3. On resume, atomically claim the checkpoint before dispatching the remaining work.
4. Reject duplicate resume attempts after the claim has landed.
5. Leave the budget ledger unchanged unless new work actually consumes new credits.

The checkpoint store is deliberately separate from the token bucket ledger. The ledger records resource consumption; the checkpoint store records resumability.

Current runtime coverage is explicit rather than magical: budget-exhausted dispatches and daemon shutdown drains are checkpointed. Hard preemption of an already-running subprocess ships via the projection-tick preempt path documented in [runtime-sandbox-security.md](./runtime-sandbox-security.md#budget-driven-preempt), but that path kills the subprocess rather than checkpointing it; resumable suspension of an in-flight subprocess remains a later runtime capability.

## Verification

Run the focused gate from `agent-os/`:

```bash
cargo test -p covenant-budget --locked
cargo test -p covenantd dispatch_budget_exhaustion_saves_checkpoint_and_resume_claims_once --locked
cargo test -p covenantd shutdown_saves_active_budget_checkpoints_once --locked
```

The current tests cover stable event shape, replay across reopen, single-use resume claims, ledger spend-once invariants, rejection of machine-local resume paths, daemon budget-exhaustion checkpoint saves, resume claim consumption, and shutdown checkpoint drains.
