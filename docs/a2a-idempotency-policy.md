# A2A Retry Idempotency Policy

Covenant A2A is a durable, explicitly leased queue. Today it does **not** automatically redeliver leased work after restart; operators repair stale leases explicitly. Automatic retry can only be added once tasks can declare duplicate-work safety in a stable, audited way.

This document defines the minimum idempotency metadata and operator expectations required before any automatic retry policy is implemented.

## Terms

- **Attempt**: one lease and execution of a task.
- **Duplicate execution**: a task is executed more than once (for example, because the receiver crashed after performing work but before posting a result).
- **Retry**: requeueing a task for another attempt without changing the task id.

## Policy goals

1. Make duplicate-work risk explicit and machine-checkable.
2. Prevent silent duplicate side effects when automation requeues work.
3. Keep the queue inspectable: retries must be visible via attempt counters and audit rows.

## Required task metadata (planned)

Automatic retry requires that each task carry explicit idempotency metadata. The current `A2ATask` envelope does not include these fields yet; implementing them is tracked as follow-up work.

### Idempotency class

Every task must declare one of the following classes:

- `idempotent`: executing the task multiple times with the same task id is safe. Any side effects must be keyed or conditional such that duplicates do not create new external effects.
- `operator_accepted`: executing the task multiple times may cause duplicate external effects. The system must never auto-retry these tasks; the only way to requeue them is an explicit operator repair action that records the accepted duplicate risk.

The default for tasks without explicit metadata is `operator_accepted`.

### Idempotency key

Automatic retry requeues the **same** task id; the task id is therefore the default idempotency key. If a task performs external work that supports an explicit idempotency key (for example via an API header), the sender should also provide an explicit `idempotency_key` so the receiver can forward it without inventing a new scheme.

## Automatic retry rules (planned)

When automatic retry exists, it must follow these rules:

1. **Never synthesize a new task id.** Retries requeue the same task id and increment the attempt counter on the next lease.
2. **Retry only `idempotent` tasks.** Tasks marked `operator_accepted` are excluded from automatic retry.
3. **Make retry decisions observable.** Each auto-requeue must produce an audit row that records task id, attempt, and reason (stale lease / receiver exit / deadline, etc).
4. **Bound duplicate risk.** Retries must have explicit maximum attempts and backoff; defaults must be documented and surfaced in operator tooling.
5. **Prefer explicit repair over implicit policy.** If the automation cannot classify a task as `idempotent`, it must stop and require an operator decision rather than guessing.

## Receiver obligations for `idempotent` tasks

Receivers may claim a task is `idempotent` only when:

- all persistent writes are conditional on the task id (or explicit idempotency key) so replays do not create new records;
- external calls that support idempotency keys receive the key consistently across retries;
- results are safe to post multiple times (posting the same result twice must not corrupt mailbox state).

If any step cannot be made idempotent, the task must be classified as `operator_accepted`.

## Relationship to manual repair

Manual lease repair already requires an explicit duplicate-risk posture (`idempotent` vs `operator_accepted`). Automatic retry is effectively a daemon-initiated requeue, so it must use the same underlying posture and must never bypass the explicit classification above.

## Follow-up work

- Add idempotency metadata to the A2A task envelope and validate it at dispatch.
- Add an opt-in automatic retry loop that requeues only `idempotent` tasks and records audit evidence for each retry decision.

