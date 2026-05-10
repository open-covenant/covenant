# A2A Repair Visibility

A2A repair is currently operator-owned. Requeue, force-error, retry-stale, and scheduler scans are visible through CLI/IPC/HTTP surfaces and audit rows. Delegated repair denial is covered across daemon and live IPC boundaries, but delegated repair automation remains blocked until explicit human release review.

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

Delegated repair release approval is a separate gate:

```bash
node agent-os/scripts/a2a-repair-release-review.mjs --json
node agent-os/scripts/a2a-repair-release-review.mjs --strict
node agent-os/scripts/validate-a2a-repair-release-review.mjs
```

The release-review report uses schema `covenant.a2a-repair-release-review.v1`. Its approval marker uses schema `covenant.a2a-delegated-repair-release-review.v1` and defaults to `docs/a2a-delegated-repair-release-review.json`. The repository intentionally does not commit that marker today, so strict mode must fail until a human accepts the release scope.

## Gates

| Gate | Current state | Evidence |
|---|---|---|
| `operator-repair-contract` | Documented | `docs/a2a-queue-semantics.md` |
| `retry-visibility-contract` | Documented | `docs/a2a-idempotency-policy.md` |
| `cli-repair-surfaces` | Implemented | `agent-os/crates/covenant/src/main.rs` |
| `live-operator-repair-coverage` | Implemented | live A2A repair, retry-stale, and restart tests |
| `per-peer-repair-report` | Implemented | `agent-os/scripts/a2a-peer-repair-report.mjs` and validator fixture |
| `delegated-repair-denial-coverage` | Implemented | unit and live peer-mismatched scope denial |
| `delegated-repair-release-review` | Human required | ADR 0005, release-review report, strict marker validator |

`ready_for_operator_repair_visibility` can be true while `ready_for_delegated_repair` remains false. That is the expected state: denial evidence exists, but a human release decision is still required before non-operator repair automation is enabled.

## Delegated Repair Requirements

Remaining delegated repair requirement:

- explicit human release-review marker before delegated repair automation is enabled.

Until that review is complete, repair authority should remain operator-owned.
