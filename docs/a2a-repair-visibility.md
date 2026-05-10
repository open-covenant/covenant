# A2A Repair Visibility

A2A repair is currently operator-owned. Requeue, force-error, retry-stale, and scheduler scans are visible through CLI/IPC/HTTP surfaces and audit rows, but delegated repair should not expand until per-peer visibility and denial coverage exist.

Run the read-only visibility report from the repository root:

```bash
node agent-os/scripts/a2a-repair-visibility.mjs --json
```

Validate the report contract:

```bash
node agent-os/scripts/validate-a2a-repair-visibility.mjs
```

The report uses schema `covenant.a2a-repair-visibility.v1`. It does not requeue tasks, force-error leases, retry stale work, start daemons, or mutate peer state.

## Gates

| Gate | Current state | Evidence |
|---|---|---|
| `operator-repair-contract` | Documented | `docs/a2a-queue-semantics.md` |
| `retry-visibility-contract` | Documented | `docs/a2a-idempotency-policy.md` |
| `cli-repair-surfaces` | Implemented | `agent-os/crates/covenant/src/main.rs` |
| `live-operator-repair-coverage` | Implemented | live A2A repair, retry-stale, and restart tests |
| `per-peer-repair-report` | Planned | Required before delegated repair expansion |
| `delegated-repair-denial-coverage` | Planned | Required before delegated repair expansion |

`ready_for_operator_repair_visibility` can be true while `ready_for_delegated_repair` remains false. That is the expected state until repair reports group stale leases and skipped retries by peer, and tests prove a peer cannot repair another peer's leased task.

## Delegated Repair Requirements

Delegated repair needs:

- a peer-scoped repair report;
- per-peer skipped retry summaries;
- peer-mismatched repair denial tests;
- capability-scope denial fixtures.

Until those exist, repair authority should remain operator-owned.
