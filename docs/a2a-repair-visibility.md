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

For peer-level repair evidence, export the current queue and optional retry-stale output, then run:

```bash
node agent-os/scripts/a2a-peer-repair-report.mjs \
  --status status.json \
  --retry retry.json \
  --now-ms 1700000000000 \
  --json
```

The peer report uses schema `covenant.a2a-peer-repair-report.v1`. It groups queued tasks, in-flight leases, stale lease candidates, retry requeues, and skipped retry reasons by peer pubkey. It does not export display strings, intent text, local paths, or peer tokens, and it does not repair or mutate tasks.

Validate the peer report fixture:

```bash
node agent-os/scripts/validate-a2a-peer-repair-report.mjs
```

Delegated repair authorization is specified separately in [A2A Repair Authorization](./a2a-repair-authorization.md). The delegated authorization policy binds repair authority to the task id, lease id, duplicate-risk posture, and peer counterparty pubkey.

## Gates

| Gate | Current state | Evidence |
|---|---|---|
| `operator-repair-contract` | Documented | `docs/a2a-queue-semantics.md` |
| `retry-visibility-contract` | Documented | `docs/a2a-idempotency-policy.md` |
| `cli-repair-surfaces` | Implemented | `agent-os/crates/covenant/src/main.rs` |
| `live-operator-repair-coverage` | Implemented | live A2A repair, retry-stale, and restart tests |
| `per-peer-repair-report` | Implemented | `agent-os/scripts/a2a-peer-repair-report.mjs` and validator fixture |
| `delegated-repair-denial-coverage` | Partially implemented | unit peer-mismatched scope denial; live coverage still required |

`ready_for_operator_repair_visibility` can be true while `ready_for_delegated_repair` remains false. That is the expected state until live tests prove a peer cannot repair another peer's leased task across the authenticated daemon boundary.

## Delegated Repair Requirements

Remaining delegated repair needs:

- peer-mismatched repair denial tests;
- capability-scope denial fixtures;
- live delegated repair denial coverage.

Until those exist, repair authority should remain operator-owned.
