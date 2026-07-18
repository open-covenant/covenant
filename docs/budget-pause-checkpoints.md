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
- Projection-attributed hard preempts (preempt reason `budget_projected_overshoot`) persist the staged dispatch checkpoint with reason `projected_overshoot` before the subprocess entry is reaped, so the killed intent stays resumable. Exhaustion-triggered preempts (reason `budget_overshoot`) do not: their bucket is empty, and the resume story belongs to the budget-exhausted admission path.
- `covenant intents resume` claims the checkpoint before redispatching a checkpointed intent. When no `BudgetExhausted` audit row is in the recent window — a projection-preempted intent never writes one, and an exhausted intent's row can age out — the resume falls back to the persisted checkpoint's `resume_state`.
- `resume_state` binds the submitting peer's pubkey (`submitter_pubkey`). Checkpoint-sourced resume enforces it: a caller with a different pubkey receives the same not-found error as a missing checkpoint, so the fallback is neither an existence oracle nor a text leak.
- Operator-requested shutdown saves active budgeted dispatch checkpoints before the daemon exits.
- Resume claims do not mutate the budget ledger or append a debit. The debit that funded already-started work remains the only spend record; a resumed redispatch that starts new work debits as new work.
- `resume_state` must be portable JSON. Machine-local absolute paths are rejected and are not echoed in error messages.

This gives the daemon a durable handoff record for the implemented pause sources. The hard-preempt path still kills the subprocess — in-flight work is lost, not suspended — but a projection-attributed kill now leaves a claimable checkpoint behind, so the intent (not the process) survives the preempt.

## Runtime Integration Path

The daemon uses the checkpoint store when an in-flight task must stop before completion:

1. Save a checkpoint with the intent, agent, requested credits, live token count, refill ETA, reason, and portable resume state.
2. Stop or suspend the active runner.
3. On resume, atomically claim the checkpoint before dispatching the remaining work.
4. Reject duplicate resume attempts after the claim has landed.
5. Leave the budget ledger unchanged unless new work actually consumes new credits.

The checkpoint store is deliberately separate from the token bucket ledger. The ledger records resource consumption; the checkpoint store records resumability.

Current runtime coverage is explicit rather than magical: budget-exhausted dispatches, daemon shutdown drains, and projection-attributed hard preempts are checkpointed. Hard preemption of an already-running subprocess ships via the projection-tick preempt path documented in [runtime-sandbox-security.md](./runtime-sandbox-security.md#budget-driven-preempt); a projection-attributed kill persists the staged checkpoint so the intent can be redispatched from the start, while true mid-run suspension (resuming partial subprocess state) remains a later runtime capability.

## Verification

Run the focused gate from `agent-os/`:

```bash
cargo test -p covenant-budget --locked
cargo test -p covenantd dispatch_budget_exhaustion_saves_checkpoint_and_resume_claims_once --locked
cargo test -p covenantd shutdown_saves_active_budget_checkpoints_once --locked
cargo test -p covenantd projection_tick_preempt_persists_claimable_checkpoint_with_projected_reason --locked
cargo test -p covenantd projection_tick_exhaustion_preempt_keeps_overshoot_reason_and_saves_no_pause --locked
cargo test -p covenantd resume_intent_redispatches_projection_preempted_intent_from_checkpoint_once --locked
cargo test -p covenantd resume_intent_denies_foreign_projection_checkpoint_without_text_leak --locked
```

The current tests cover stable event shape, replay across reopen, single-use resume claims, ledger spend-once invariants, rejection of machine-local resume paths, daemon budget-exhaustion checkpoint saves, resume claim consumption, shutdown checkpoint drains, projection-preempt checkpoint persistence with trigger-attributed reasons, the negative case that exhaustion-triggered preempts keep their overshoot reason and persist no pause, checkpoint-sourced resume redispatch, and submitter-binding denial without text leaks.
