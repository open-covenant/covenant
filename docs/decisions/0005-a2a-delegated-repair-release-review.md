# 0005: A2A Delegated Repair Release Review

## Status

Accepted as a release gate. No delegated repair approval marker is committed.

## Context

A2A repair is currently operator-owned. The daemon supports bounded repair surfaces for requeue and force-error, and it rejects peer-mismatched delegated repair scopes before mutation. That denial path now has daemon and live IPC coverage.

The remaining risk is expansion by configuration or automation drift: a future agent could enable non-operator delegated repair because denial coverage exists, even though no human has accepted the operational blast radius. Delegated repair can change another peer's leased work, so approval needs to be explicit, inspectable, and tied to the scope that will be enabled.

## Decision

Delegated A2A repair automation remains blocked until a human release reviewer provides a valid `covenant.a2a-delegated-repair-release-review.v1` marker.

The default marker path is:

```text
docs/a2a-delegated-repair-release-review.json
```

The marker must include:

- `schema`: `covenant.a2a-delegated-repair-release-review.v1`
- `decision`: `accepted`
- `review_id`
- `approved_by` using a neutral project alias
- `approved_at` as an ISO timestamp
- `scope.automation`: `a2a.delegated-repair`
- `scope.actions`: the repair actions being released
- `scope.task_binding`: `required`
- `scope.lease_binding`: `required`
- `scope.counterparty_binding`: `required`
- `scope.duplicate_risk_policy`: `explicit-scope-only`
- `conditions`: the release constraints the reviewer accepted
- `evidence`: repository-relative paths for the authorization policy, visibility policy, and validators

The marker is checked by:

```bash
node agent-os/scripts/a2a-repair-release-review.mjs --strict
```

Automation that would enable delegated repair must pass the strict gate with an accepted marker. The normal validation suite keeps the repository in the blocked state until that marker exists.

## Consequences

Delegated repair denial coverage can keep hardening without implying release approval. Future work has a concrete gate to satisfy, and reviewers can see exactly what scope they are accepting.

This adds a human-owned approval boundary. That is intentional: cross-peer repair mutation is not just a test-coverage question. It is an operational policy decision.

## Non-Goals

- Do not grant capabilities.
- Do not enable delegated repair automation.
- Do not weaken task, lease, counterparty, or duplicate-risk scope checks.
- Do not treat local test fixtures as release approval.

## Follow-up

When the project is ready to release delegated repair automation, add the marker with a narrow scope, run `node agent-os/scripts/a2a-repair-release-review.mjs --strict`, run the full repository validation gate, and bind the accepted marker to the release evidence bundle.
