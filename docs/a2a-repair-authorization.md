# A2A Repair Authorization

A2A repair is operator-owned by default. Delegated repair must remain blocked until each repair request is constrained by task identity, lease identity, duplicate-risk posture, and the peer counterparty the repair can affect.

## Current Boundary

Implemented repair commands:

- `a2a.repair.requeue`
- `a2a.repair.force_error`

The daemon requires the authenticated peer to hold the matching action before repair. It then checks that the task is visible to that peer:

- the sender may repair an in-flight task it sent;
- the recipient may repair an in-flight task leased to it;
- unrelated peers cannot repair or inspect the task through repair surfaces.

## Scoped Delegation Contract

Scoped delegated repair uses a versioned scope object:

```json
{
  "version": 1,
  "peer_pubkey_b58": "<counterparty pubkey>",
  "task_id": "<uuid>",
  "lease_id": "<uuid>",
  "duplicate_risk": "idempotent"
}
```

`peer_pubkey_b58` is the counterparty, not the caller:

- if the sender repairs a task, the scope must name the recipient pubkey;
- if the recipient repairs a task, the scope must name the sender pubkey.

`task_id` must match the task being repaired. `lease_id` must match the current in-flight lease when provided. `duplicate_risk` applies to requeue and must match the request value. `force_error` does not use duplicate risk.

## Denial Evidence

The daemon regression test `a2a_repair_rejects_peer_mismatched_delegated_scope` proves the important delegated case: a recipient can see and lease a task, but a repair capability scoped to the wrong counterparty pubkey is rejected before mutation and records a `capability_scope_rejected` audit row.

The opt-in live IPC test `live_covenantd_a2a_repair_rejects_peer_mismatched_delegation` covers the same boundary across a spawned daemon with a pre-seeded delegate peer. It proves the rejected repair leaves the task in flight with the original lease and records `a2a.repair.requeue` scope-rejection audit evidence.

Delegated repair is still not release-ready. Remaining gate:

- explicit review before enabling non-operator repair automation.

That review is an executable release gate, not an informal note. The accepted marker schema is `covenant.a2a-delegated-repair-release-review.v1`; the default marker path is `docs/a2a-delegated-repair-release-review.json`. The marker must preserve task, lease, counterparty, and duplicate-risk scope requirements and must pass:

```bash
node agent-os/scripts/a2a-repair-release-review.mjs --strict
```

Until that strict gate passes with a human-accepted marker, delegated repair automation remains blocked even though peer-mismatched denial coverage exists.
