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
  "task_kind": "release.notes",
  "idempotency": {
    "duplicate_safety": "idempotent",
    "key": "agent:logical-work-unit"
  }
}
```

The daemon validates that a present `task_kind` and idempotency key are non-empty, persists the metadata in the mailbox log, and returns it through queue/status surfaces. Missing idempotency metadata is treated as unsafe for automated requeue.

### Task kind

`task_kind` is an optional stable type for the logical operation being delegated. When present, it is part of the receiver-side idempotency cache key. When absent, Covenant falls back to `intent_text` so legacy tasks keep their existing behavior.

Use `task_kind` for durable categories such as `release.notes`, `code.review`, or `memory.compaction`; keep free-form instructions in `intent_text`.

### Idempotency class

The current duplicate-safety classes are:

- `idempotent`: executing the task multiple times with the same task id is safe. Any side effects must be keyed or conditional such that duplicates do not create new external effects.
- `unsafe`: duplicate execution may cause external effects. The system must not automatically requeue these tasks.

Manual repair still uses an operator posture (`idempotent` or `operator_accepted`) because a human may explicitly accept duplicate-work risk for a single repair action. The automated retry gate only accepts task metadata marked `idempotent`.

### Idempotency key

The idempotency key is a stable, caller-chosen key that identifies the logical work unit across retries. If a task performs external work that supports an explicit idempotency key, the sender should provide the same key so the receiver can forward it without inventing a new scheme.

## Receiver-side result cache

When an idempotent task posts a result, the mailbox stores a cached result payload keyed by:

- sender public key;
- recipient public key;
- task kind, using explicit `task_kind` when present and `intent_text` as the legacy fallback;
- idempotency key.

A later task with the same cache key is not leased to the recipient. The mailbox immediately queues a replayed result for the new task id, preserving the original status, content, and error message. JSONL-backed mailboxes persist cache entries in the event log; task compaction removes resolved task history but keeps cache entries so future duplicates can still short-circuit after restart.

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

## Periodic scheduler

The daemon can run the same retry gate on a timer, but only through an explicit environment opt-in:

```bash
COVENANT_A2A_AUTO_RETRY_SCHEDULER=1
COVENANT_A2A_AUTO_RETRY_INTERVAL_MS=60000
COVENANT_A2A_AUTO_RETRY_MIN_LEASE_AGE_MS=300000
COVENANT_A2A_AUTO_RETRY_MAX_ATTEMPTS=3
COVENANT_A2A_AUTO_RETRY_MAX_REQUEUES=1
COVENANT_A2A_AUTO_RETRY_SCAN_LIMIT=100
```

The scheduler does not bypass the repair capability gate. If the operator identity does not hold `a2a.repair.requeue`, the scan is rejected and recorded as an `a2_a_auto_retry_scheduler_scan` audit row with an error. Successful scans record the same audit summary plus per-task `auto_requeue` repair rows for actual mutations.

## Receiver obligations for `idempotent` tasks

Receivers may claim a task is `idempotent` only when:

- all persistent writes are conditional on the task id (or explicit idempotency key) so replays do not create new records;
- external calls that support idempotency keys receive the key consistently across retries;
- results are safe to post multiple times (posting the same result twice must not corrupt mailbox state).

If any step cannot be made idempotent, the task must be classified as `operator_accepted`.

## Relationship to manual repair

Manual lease repair already requires an explicit duplicate-risk posture (`idempotent` vs `operator_accepted`). The retry gate is effectively a daemon-initiated requeue, so it must use task metadata and must never bypass the explicit classification above.
