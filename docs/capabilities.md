# Capability Scope Contract

Capability tokens bind an agent subject, an action string, an optional JSON scope, an issuer, and an optional expiry into one signed object. The signature covers `scope`, so scope fields are tamper-evident.

Current enforcement boundary: the daemon validates non-empty scopes for known action namespaces before signing a grant, then enforces action presence, expiry, signature validity, subject matching, revocation, the `tool.call.*` `arguments.allow` predicate, the `audit.purge` `before_ms` cutoff, stable memory predicates for `memory.read`, `memory.read.<tier>`, `memory.write`, `memory.purge`, `memory.repair.*`, and `memory.compact.*`, and stable A2A predicates for send, receive-admission, respond, and repair flows at dispatch. Peer and settlement scope predicates remain compatibility metadata until their dispatch semantics stabilize.

## Scope Envelope

Every non-empty scope for a known action namespace must be a JSON object with a version field:

```json
{
  "version": 1
}
```

`{}` remains valid and means unscoped within the named action. Grant requests for known namespaces reject non-object scopes, missing versions, unsupported versions, and malformed known fields. Unknown future fields are preserved as signed metadata until dispatch-time enforcement defines them.

## Namespaces

### `intent.*` and `agent.*`

Use the base envelope for intent routing and agent lifecycle actions. Predicate fields are not stable yet, so grant-time validation enforces only the versioned object shape and preserves any extra fields as signed metadata.

### `tool.*`

Use for tool listing and tool invocation.

```json
{
  "version": 1,
  "tool": "echo",
  "arguments": {
    "allow": {
      "text": "optional exact or policy-owned value"
    }
  }
}
```

Rules:

- `tool` is optional for broad actions such as `tool.list`; it should match the suffix for `tool.call.<name>`.
- `arguments.allow` is an optional exact JSON argument allowlist. When present on `tool.call.<name>`, the daemon rejects calls whose full argument object does not exactly match it.
- Networked tools should add explicit host or origin fields before enforcement.

### `memory.*`

Use for memory reads, writes, repair, compaction, and purge.

```json
{
  "version": 1,
  "tiers": ["working", "episodic", "longterm"],
  "record_id": null,
  "before_ms": null,
  "apply": false
}
```

Rules:

- `tiers` is optional; absent means every tier allowed by the action.
- `record_id` narrows read, write, and repair operations to one record when the action has a concrete record id.
- `before_ms` narrows purge and compaction cutoffs. On read, write, and repair scopes it means the target record must be older than the cutoff.
- `apply` distinguishes dry-run/read grants from mutation grants. Reads require `apply: false` when the field is present. `memory.write` and `memory.purge` are always mutations and require `apply: true` when the field is present.
- `memory.read` and `memory.read.<tier>` gate recent-memory and semantic-search responses. The daemon still filters returned records by authenticated owner, signed tier scope, optional `record_id`, and optional `before_ms`.
- `memory.write` is required before successful intent dispatch writes a working-tier memory record. The scope is checked before agent execution or fallback dispatch.
- A tier-scoped `memory.purge` grant only permits purging that tier. An un-tiered purge request requires the scope to include all tiers.
- A tier-scoped `memory.compact.*` grant only permits policies that touch the listed tiers. `detach_stale_parents` with an explicit tier scope requires all tiers because parent detaches are not tier-isolated.

### `a2a.*`

Use for agent-to-agent send, receive, respond, repair, and compaction actions.

```json
{
  "version": 1,
  "peer_pubkey_b58": "base58-public-key",
  "task_id": null,
  "lease_id": null,
  "duplicate_risk": "idempotent"
}
```

Rules:

- `peer_pubkey_b58` is the canonical peer selector. On `a2a.send.*` it is the recipient. On `a2a.recv.*` and `a2a.respond.*` it is the sender. On `a2a.repair.*` it is the task counterparty visible to the authenticated peer.
- `task_id` narrows send, receive-admission, response, and repair flows to one task.
- `lease_id` narrows manual repair to one in-flight lease.
- `duplicate_risk` narrows `a2a.repair.requeue` posture and should be either `idempotent` or `operator-accepted`; the daemon also accepts the wire spelling `operator_accepted`.

### `audit.*`

Use for audit reads, verification, and retention.

```json
{
  "version": 1,
  "window": 100,
  "before_ms": null,
  "include_integrity": true
}
```

Rules:

- `window` bounds read and verification work.
- `before_ms` narrows purge authority. When present on `audit.purge`, the requested purge cutoff must be less than or equal to the scoped value.
- `include_integrity` records whether the grant covers hash-chain verification, not only event reads.

### `peers.*` and `identity.*`

Use for peer registry and local identity operations.

```json
{
  "version": 1,
  "peer_pubkey_b58": null,
  "token_prefix": null,
  "self": false,
  "force": false
}
```

Rules:

- `peer_pubkey_b58` is preferred for identity-stable operations.
- `token_prefix` may be used only for operator-facing revoke flows that already perform ambiguity checks.
- `self` and `force` must be explicit for self-revocation recovery paths.

### `chain.*`

Use for local settlement and receipt batching.

```json
{
  "version": 1,
  "limit": 100,
  "mint": null,
  "cluster": null
}
```

Rules:

- `limit` bounds batch size.
- `mint` and `cluster` must be explicit before any production settlement path is enabled.

## Enforcement Path

1. Keep accepting `{}` for existing broad grants.
2. Validate non-empty scopes at grant time for known action namespaces.
3. Interpret the stable `tool.call.*` `arguments.allow` predicate at dispatch.
4. Interpret the stable `audit.purge` `before_ms` cutoff at dispatch.
5. Interpret stable memory read, write, purge, repair, and compaction predicates at dispatch.
6. Interpret stable A2A peer, task, lease, and duplicate-risk predicates at dispatch.
6. Fail closed for malformed versioned scopes after a migration window.
7. Keep action-only checks as the fallback only for unscoped operator grants.

Until a namespace-specific predicate lands, public docs must describe that namespace's scope as validated signed metadata and compatibility preparation, not as enforced least-privilege behavior.
