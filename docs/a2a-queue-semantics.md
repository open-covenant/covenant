# A2A Queue Semantics

Covenant A2A is an at-least-durable, explicitly leased task queue for local agent coordination. It is not an automatic distributed retry system yet.

## Task States

| State | Meaning |
| --- | --- |
| `queued` | The task has been accepted by the daemon and has not been delivered to its recipient. |
| `in_flight` | The task has been leased to a recipient and no result has been posted yet. |
| `result_pending` | A result has been posted for the task and is waiting for the original sender to drain it. |
| `resolved` | The task was leased, a result was posted, and the sender drained the result. It is eligible for compaction. |

The public queue-status surface returns `queued` and `in_flight` task entries plus pending results. `resolved` tasks are intentionally absent from the operator view unless they remain in audit logs.

## Delivery Contract

The daemon appends a durable `task_leased` event before returning a task to a receiver. After restart, a leased task is restored as `in_flight` and is not redelivered automatically.

This chooses inspectability over accidental duplicate work:

- A crash before the receiver observes the response may leave a task in `in_flight`.
- The task will not disappear silently; it remains visible through `covenant a2a status`, IPC `a2a_queue`, and HTTP `GET /a2a/queue`.
- Alpha does not auto-retry leased tasks. Requeue and lease-expiry policy must be explicit because autonomous agents may perform non-idempotent external work.

## Result Contract

Posting a result for a known task clears the task's in-flight lease and queues the result for the original sender. Result reads remain sender-scoped through the mailbox sender map, compared by pubkey rather than display string.

Results for unknown task ids are rejected by the daemon before capability checks. This prevents callers from probing respond capabilities with arbitrary task ids.

## Compaction Contract

Compaction may drop events for a task only when all of the following are true:

- The task was sent.
- The task was delivered through a legacy `task_recv` event or a current `task_leased` event.
- At least one result was posted.
- Every posted result has been drained.

Queued tasks, leased tasks without results, and pending results are never compacted. This keeps recovery state available across daemon restarts.

## Future Work

The next queue hardening step is an explicit operator repair surface:

- `a2a.requeue.<task>` or an operator-only requeue command.
- Lease expiry thresholds for stale in-flight work.
- Attempt counters that survive requeue cycles.
- Idempotency markers for tasks that can safely auto-retry.
- Audit rows for manual requeue and forced resolution.
