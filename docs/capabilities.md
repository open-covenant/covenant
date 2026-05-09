# Capability Scope Contract

Capability tokens bind an agent subject, an action string, an optional JSON scope, an issuer, and an optional expiry into one signed object. The signature already covers `scope`, so scope fields are tamper-evident today.

Current enforcement boundary: the daemon enforces action presence, expiry, signature validity, subject matching, and revocation. It does not yet interpret scope predicates at dispatch time. Scope schemas below are the compatibility contract for grants created now and the target for later enforcement.

## Scope Envelope

Every non-empty scope should be a JSON object with a version field:

```json
{
  "version": 1
}
```

`{}` remains valid and means unscoped within the named action. Consumers must reject non-object scopes when strict enforcement lands.

## Namespaces

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
- `arguments.allow` is an optional object of literal argument constraints.
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
- `before_ms` narrows purge authority.
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
2. Add schema validation at grant time for known action namespaces.
3. Add dispatch-time checks that interpret scope only for actions with stable predicates.
4. Fail closed for malformed versioned scopes after a migration window.
5. Keep action-only checks as the fallback only for unscoped operator grants.

Until step 3 lands, public docs must describe scopes as signed metadata and compatibility preparation, not as enforced least-privilege predicates.
