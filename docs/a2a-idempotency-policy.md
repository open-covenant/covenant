# A2A Idempotency Policy

Covenant A2A is a durable, explicitly leased queue. It does **not** automatically redeliver leased work after restart. Operators can repair stale leases explicitly, and an explicit disabled-by-default retry scan can requeue only stale tasks that declare idempotent duplicate safety and carry a non-empty key.

This document defines the current idempotency metadata, operator expectations, and remaining requirements before any periodic background retry scheduler is allowed.

## Terms

- **Attempt**: one lease and execution of a task.
- **Duplicate execution**: a task is executed more than once (for example, because the receiver crashed after performing work but before posting a result).
- **Retry**: requeueing a task for another attempt without changing the task id.

## Policy goals

1. Make duplicate-work risk explicit and machine-checkable.
2. Prevent silent duplicate side effects when automation requeues work.
3. Keep the queue inspectable: retries must be visible via attempt counters and audit rows.

## Task metadata

Tasks may carry explicit idempotency metadata in the `A2ATask` envelope:

```json
{
  "idempotency": {
    "duplicate_safety": "idempotent",
    "key": "agent:logical-work-unit"
  }
}
```

The daemon validates that a present key is non-empty, persists the metadata in the mailbox log, and returns it through queue/status surfaces. Missing metadata is treated as unsafe for automated requeue.

### Idempotency class

The current duplicate-safety classes are:

- `idempotent`: executing the task multiple times with the same task id is safe. Any side effects must be keyed or conditional such that duplicates do not create new external effects.
- `unsafe`: duplicate execution may cause external effects. The system must not automatically requeue these tasks.

Manual repair still uses an operator posture (`idempotent` or `operator_accepted`) because a human may explicitly accept duplicate-work risk for a single repair action. The automated retry gate only accepts task metadata marked `idempotent`.

### Idempotency key

The idempotency key is a stable, caller-chosen key that identifies the logical work unit across retries. If a task performs external work that supports an explicit idempotency key, the sender should provide the same key so the receiver can forward it without inventing a new scheme.

## Explicit retry gate

The daemon exposes an operator-triggered retry scan through CLI and IPC. It is disabled by default and reports what it would do unless the operator passes `--enable`.

```bash
covenant a2a retry-stale \
  --enable \
  --min-lease-age-ms 300000 \
  --max-attempts 3 \
  --max-requeues 1 \
  --scan-limit 100 \
  --json
```

The gate follows these rules:

1. **Never synthesize a new task id.** Requeues use the same task id and increment the attempt counter on the next lease.
2. **Retry only `idempotent` tasks.** Missing metadata and `unsafe` metadata are skipped.
3. **Require a non-empty key.** A task without a stable idempotency key is skipped.
4. **Make decisions observable.** Each auto-requeue records an `auto_requeue` A2A repair audit row; skipped tasks remain visible in the report.
5. **Bound duplicate risk.** Operators must set maximum attempts, maximum requeues, minimum lease age, and scan limits.

## Receiver obligations for `idempotent` tasks

Receivers may claim a task is `idempotent` only when:

- all persistent writes are conditional on the task id (or explicit idempotency key) so replays do not create new records;
- external calls that support idempotency keys receive the key consistently across retries;
- results are safe to post multiple times (posting the same result twice must not corrupt mailbox state).

If any step cannot be made idempotent, the task must be classified as `operator_accepted`.

## Relationship to manual repair

Manual lease repair already requires an explicit duplicate-risk posture (`idempotent` vs `operator_accepted`). The retry gate is effectively a daemon-initiated requeue, so it must use task metadata and must never bypass the explicit classification above.

## Follow-up work

- Add receiver-side idempotency result caching that persists `idempotency_key -> result`.
- Add an opt-in periodic retry scheduler only after receiver-side deduplication exists.
