# Signed Capabilities Live Coverage Matrix

This matrix enumerates the scoped capability paths from [docs/capabilities.md](./capabilities.md) and pairs each with the live boundary test that exercises a non-operator delegated subject. "Gap" entries identify scoped paths that currently have only mock or operator-only coverage.

The matrix lets the autonomy loop target delegated coverage gaps deterministically. It does not add tests, change capability enforcement, or claim that delegation is implemented for every namespace.

The validator at `agent-os/scripts/validate-signed-capabilities-live-coverage.mjs` parses the cited live-test paths from this document, refuses to pass when any path is missing under `agent-os/crates/*/tests/`, and cross-binds each `delegated-covered` or `delegated-denial-only` row to the `delegated: true` scope evidence in [`agent-os/autonomy/live-coverage.json`](../agent-os/autonomy/live-coverage.json). Run it from the repository root:

```bash
node agent-os/scripts/validate-signed-capabilities-live-coverage.mjs
```

## Coverage Marks

- `delegated-covered` — at least one live test exercises a non-operator subject under the named scope and asserts both denial and allowance behavior.
- `delegated-denial-only` — at least one live test exercises a non-operator subject denial; allowance is gated by a human release review.
- `gap` — no live test exercises a non-operator subject under this scope; coverage is mock-only or operator-only.

## Matrix

| Action | Stable predicate fields | Delegated live test | Coverage |
|---|---|---|---|
| `peers.revoke` | `token_prefix`, `force` | `agent-os/crates/covenantd/tests/live_peers_revoke.rs` | delegated-covered |
| `peers.list` | `peer_pubkey_b58` | gap | gap |
| `peers.purge` | `before_ms` | gap | gap |
| `a2a.send.<task_kind>` | `peer_pubkey_b58`, `task_id` | gap | gap |
| `a2a.recv.<task_kind>` | `peer_pubkey_b58`, `task_id` | gap | gap |
| `a2a.respond.<task_kind>` | `peer_pubkey_b58`, `task_id` | gap | gap |
| `a2a.repair.requeue` | `peer_pubkey_b58`, `task_id`, `lease_id`, `duplicate_risk` | `agent-os/crates/covenantd/tests/live_a2a.rs` | delegated-denial-only |
| `tool.call.<name>` | `tool`, `arguments.allow` | gap | gap |
| `audit.purge` | `before_ms` | gap | gap |
| `memory.read`, `memory.read.<tier>` | `tiers`, `record_id`, `before_ms` | gap | gap |
| `memory.write` | `tiers`, `record_id`, `apply` | gap | gap |
| `memory.purge` | `tiers`, `before_ms`, `apply` | gap | gap |
| `memory.repair.*` | `tiers`, `record_id` | gap | gap |
| `memory.compact.*` | tier policy | gap | gap |
| `chain.receipts` | `limit`, `payer_pubkey_b58`, `resource` | gap | gap |
| `chain.batches` | `limit`, `cluster`, `batch_id` | gap | gap |
| `chain.flush` | `mint`, `limit` | gap | gap |

## Notes

- `peers.revoke` delegated coverage covers missing-grant denial, mismatched `token_prefix` denial, and matching-prefix allowance, in `live_peers_revoke.rs`.
- `a2a.repair.requeue` delegated coverage is denial-only by design; delegated repair automation is gated on the human release review documented in [`docs/decisions/0005-a2a-delegated-repair-release-review.md`](./decisions/0005-a2a-delegated-repair-release-review.md).
- Operator-grant coverage for namespaces marked `gap` may still exist in the live test tree. Operator scope evidence is tracked separately in [`agent-os/autonomy/live-coverage.json`](../agent-os/autonomy/live-coverage.json) and is not what this matrix counts.
- Closing a `gap` requires a live test that exercises a non-operator subject with a scoped grant and asserts the denial path, ideally paired with an allowance assertion when the namespace permits delegated mutation.

## Human Authority

Adding delegated allowance coverage for a mutating namespace requires a human release review. Automation may add denial-only delegated coverage and document gaps; it must not enable delegated mutation paths without an approved decision.
