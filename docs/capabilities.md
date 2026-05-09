# Capability Scope Contract

Capability tokens bind an agent subject, an action string, an optional JSON scope, an issuer, and an optional expiry into one signed object. The signature covers `scope`, so scope fields are tamper-evident.

Current enforcement boundary: the daemon validates non-empty scopes for known action namespaces before signing a grant, then enforces action presence, expiry, signature validity, subject matching, revocation, the `tool.call.*` `arguments.allow` predicate, and the `audit.purge` `before_ms` cutoff at dispatch. Other scope predicates remain compatibility metadata until their dispatch semantics stabilize.

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
- `record_id` narrows repair or delete operations to one record.
- `before_ms` narrows retention operations.
- `apply` distinguishes dry-run grants from mutation grants.

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

- `peer_pubkey_b58` is the canonical peer selector. Display names are not stable authority.
- `task_id` and `lease_id` narrow repair or response flows.
- `duplicate_risk` is required for automatic requeue policy and should be either `idempotent` or `operator-accepted`.

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
5. Add dispatch-time checks for the next stable action families.
6. Fail closed for malformed versioned scopes after a migration window.
7. Keep action-only checks as the fallback only for unscoped operator grants.

Until a namespace-specific predicate lands, public docs must describe that namespace's scope as validated signed metadata and compatibility preparation, not as enforced least-privilege behavior.
