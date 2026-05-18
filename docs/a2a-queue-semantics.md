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
- The task will not disappear silently; it remains visible through `covenant a2a status`, IPC `a2_a_queue`, and HTTP `GET /a2a/queue`.
- Covenant does not auto-retry leased tasks. Requeue and lease-expiry policy must be explicit because autonomous agents may perform non-idempotent external work.

Operators can narrow status views to stale leases with `--min-lease-age-ms`:

```bash
covenant a2a status --min-lease-age-ms 300000 --json
```

HTTP exposes the same discovery-only filter through `GET /a2a/queue?min_lease_age_ms=300000`. IPC callers pass `min_lease_age_ms` on `A2AQueue`.

Operators can also narrow by deadline urgency with `--deadline-within-ms`:

```bash
covenant a2a status --deadline-within-ms 60000 --json
```

This keeps only tasks whose `deadline_ms` is set and within at most N ms from the daemon's clock — i.e., already-past-due or about-to-expire tasks. Tasks without a `deadline_ms` are dropped under an active deadline filter so the operator can triage by remaining time without scraping the JSON for `deadline_ms != null`. HTTP and IPC accept the same filter as `deadline_within_ms` on `A2AQueue` and `GET /a2a/queue?deadline_within_ms=60000`. Combining both filters applies them conjunctively.

Operators can also narrow by queue state with `--state queued` or `--state in_flight`:

```bash
covenant a2a status --state in_flight --json
```

The CLI parses `--state` case-sensitively against the wire enum (`queued`, `in_flight`, with `in-flight` accepted as a dash-spelling alias) and rejects any other value before the daemon sees the frame, so a typo cannot silently disable the filter. HTTP and IPC accept the same filter as `state_filter` on `A2AQueue` and `GET /a2a/queue?state_filter=in_flight`. Filters compose conjunctively; the state filter is applied before `--limit` so a noisy `in_flight` cluster cannot push every `queued` row out of the result window.

The `--json` form emits one `a2a_status` object containing `limit`, `min_lease_age_ms`, `deadline_within_ms`, `state_filter`, `tasks`, and `results`. The stale-lease filter applies only to `in_flight` task entries. Queued tasks and pending results remain visible so the operator does not mistake filtered output for a healthy empty queue. The filter never requeues, expires, cancels, or force-errors work.

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

Queue maintenance automation should use the machine-readable compaction form:

```bash
covenant a2a compact --json
```

```json
{
  "kind": "a2a_compacted",
  "dropped": 0
}
```

## Repair Contract

The mailbox crate defines explicit repair primitives for in-flight leases. The daemon exposes them through IPC, HTTP, and CLI surfaces with capability checks, peer-visible task guards, and success audit rows. These are operator-controlled mutation paths; they are not automatic retry policy.

Repair requests require:

- a `task_id`;
- a non-empty `reason`;
- a command of `requeue` or `force_error`;
- an optional `lease_id` guard to prevent repairing a newer lease than the operator inspected;
- for `requeue`, an explicit duplicate-work posture: `idempotent` or `operator_accepted`.

`requeue` moves an in-flight task back to `queued`, preserves the last attempt counter, and increments the counter on the next lease. This makes repeated repair visible without hiding the possibility that the original worker may still complete.

`force_error` clears the in-flight lease and posts an error result for the original sender to drain. It is the explicit way to stop waiting on a stale lease without pretending the task succeeded.

Both repair actions replay from the JSONL mailbox log after daemon restart.

CLI usage:

```bash
covenant capabilities grant a2a.repair.requeue
covenant a2a requeue <task-id> \
  --lease-id <lease-id> \
  --reason "worker heartbeat expired" \
  --duplicate-risk idempotent

covenant capabilities grant a2a.repair.force_error
covenant a2a force-error <task-id> \
  --lease-id <lease-id> \
  --reason "recipient process exited" \
  --message "operator forced stale lease failure"
```

HTTP uses `POST /a2a/repair` with the same `A2ARepairRequest` JSON shape as IPC. Repair calls are rejected unless the authenticated peer can see the in-flight task and holds `a2a.repair.requeue` or `a2a.repair.force_error`, depending on the command. Non-empty A2A scopes are enforced at dispatch: `task_id`, `lease_id`, `peer_pubkey_b58`, and `duplicate_risk` must match the concrete repair request.

Repair visibility readiness and the delegated-repair boundary are tracked internally.

## Idempotency and Retry Policy

Covenant A2A is deliberately conservative about retry. The daemon persists work, leases it explicitly, and refuses to redeliver automatically after restart. Tasks can carry optional task-level idempotency metadata, and the daemon exposes a disabled-by-default retry gate that requeues only stale, idempotent in-flight leases when an operator explicitly enables a bounded scan.

The optional task metadata is:

```json
{
  "idempotency": {
    "duplicate_safety": "idempotent",
    "key": "agent:logical-work-unit"
  }
}
```

- **Duplicate safety** declares whether re-running the same logical task can create harmful external side effects (network writes, payments, ticket creation, etc.). Missing metadata is treated as **unsafe**.
- **Idempotency key** is a stable, caller-chosen key that uniquely identifies the logical work unit across retries. The daemon validates that a present key is non-empty and persists it through queue/status surfaces and restart replay.

For idempotent tasks, the mailbox persists receiver-side result cache entries keyed by sender, recipient, current task kind, and idempotency key. A later task with the same cache key receives a replayed result immediately instead of being leased to the recipient again. Cached entries survive JSONL replay and are not removed by task-history compaction.

Operator policy:

- Treat every stale lease as potentially non-idempotent external work.
- Use `a2a requeue` only when the operator can justify `--duplicate-risk idempotent` (or explicitly accepts the risk).
- Prefer `a2a force-error` when the correct outcome is “stop waiting” rather than “try again”.

The explicit retry gate is available through CLI/IPC:

```bash
covenant capabilities grant a2a.repair.requeue
covenant a2a retry-stale \
  --enable \
  --min-lease-age-ms 300000 \
  --max-attempts 3 \
  --max-requeues 1 \
  --scan-limit 100 \
  --json
```

Without `--enable`, the CLI returns a report and performs no mutation. With `--enable`, the daemon only requeues entries that are in flight, old enough, below the attempt bound, and marked `duplicate_safety = "idempotent"` with a non-empty key. Each requeue records an `auto_requeue` A2A repair audit row. Entries that are unsafe, too young, exhausted, missing metadata, or outside capability scope remain untouched and appear in the report's `skipped` list.

The daemon also ships an opt-in periodic scheduler that runs the same retry gate. It is disabled by default and has no independent mutation path. Enable it only after granting `a2a.repair.requeue` to the operator identity:

```bash
COVENANT_A2A_AUTO_RETRY_SCHEDULER=1
COVENANT_A2A_AUTO_RETRY_INTERVAL_MS=60000
COVENANT_A2A_AUTO_RETRY_MIN_LEASE_AGE_MS=300000
COVENANT_A2A_AUTO_RETRY_MAX_ATTEMPTS=3
COVENANT_A2A_AUTO_RETRY_MAX_REQUEUES=1
COVENANT_A2A_AUTO_RETRY_SCAN_LIMIT=100
```

The cache is intentionally conservative: tasks without metadata, tasks marked `unsafe`, and tasks whose sender, recipient, task kind, or idempotency key differ are delivered normally.

The scheduler remains:

- opt-in (disabled by default);
- limited to tasks marked safe to duplicate with an explicit idempotency key;
- observable (`a2_a_auto_retry_scheduler_scan` audit summaries plus per-requeue `auto_requeue` audit rows);
- bounded (interval, minimum lease age, max attempts, max requeues, and scan limit are operator-configurable).
