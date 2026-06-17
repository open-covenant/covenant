# Capability Scope Contract

Capability tokens bind an agent subject, an action string, an optional JSON scope, an issuer, and an optional expiry into one signed object. The signature covers `scope`, so scope fields are tamper-evident.

Current enforcement boundary: the daemon validates non-empty scopes for known action namespaces before signing a grant, then enforces action presence, expiry, signature validity, subject matching, revocation, the `tool.call.*` `arguments.allow` predicate, the `audit.purge` and `capabilities.purge` `before_ms` cutoffs, stable memory predicates for `memory.read`, `memory.read.<tier>`, `memory.write`, `memory.purge`, `memory.repair.*`, `memory.compact.*`, and `memory.backfill.*`, stable A2A predicates for send, receive-admission, respond, and repair flows, peer predicates for delegated list/revoke flows plus purge retention, chain predicates for receipt reads, receipt batch reads, and receipt flushing, the `settlement.backfill.*` predicate for receipt backfill, the `x402.outbound.pay` destination-class predicate for outbound paid-call egress, and the `secret.access` named-secret predicate for daemon-mediated secret reads at dispatch.

## Action Grammar

A capability `action` is a dotted identifier — a namespace segment, a method, and an optional sub-method (`namespace.method[.sub]`, e.g. `memory.read`, `tool.call.echo`, `x402.outbound.pay`). The separator is `.`. There is no wildcard form, so each granted action is matched literally; the one hierarchical exception is that an umbrella `memory.read` grant also satisfies a tier-scoped `memory.read.<tier>` request.

The leading segment selects the scope validator. Twelve namespaces are recognized at grant time:

`intent`, `tool`, `memory`, `agent`, `a2a`, `audit`, `peers`, `identity`, `chain`, `settlement`, `x402`, `secret`.

`capabilities.*` is enforced only at dispatch (the `before_ms` retention cutoff) and is not bound at grant time, so its non-empty scope is preserved as signed metadata. An action outside every recognized namespace passes grant-time scope validation unchanged.

This grammar — the dotted action form, the namespace inventory, and one representative grant per namespace — is frozen as a versioned conformance contract under [`agent-os/crates/covenant-permissions/tests/golden/capabilities/`](../agent-os/crates/covenant-permissions/tests/golden/capabilities/). For every namespace, `tests/golden_capabilities.rs` re-serializes the in-code grant and asserts it is byte-for-byte equal to the committed `<namespace>.json`, round-trips it back to the same grant, and asserts the frozen scope still passes the live `validate_scope` — so tightening the validator against a grant already in the field fails the build rather than breaking it silently. A compile-time `match` over the namespace enum (`scope_namespace_inventory_is_frozen`) breaks the build when a namespace is added, forcing a matching vector. Regenerate deliberately, only after reviewing the diff:

```sh
cd agent-os
COVENANT_BLESS_CAPABILITY_GOLDEN=1 cargo test -p covenant-permissions \
  --test golden_capabilities grammar_vectors_match_committed_corpus
```

## Scope Envelope

Every non-empty scope for a known action namespace must be a JSON object with a version field:

```json
{
  "version": 1
}
```

`{}` remains valid and means unscoped within the named action. Grant requests for known namespaces reject non-object scopes, missing versions, unsupported versions, and malformed known fields. Unknown future fields are preserved as signed metadata until dispatch-time enforcement defines them.

### `max_uses` (usage budget)

`max_uses` is a cross-namespace field accepted on any recognized scope alongside `version`. It bounds how many times a grant may authorize its action — a usage budget that complements the time budget of `expires_at`:

```json
{
  "version": 1,
  "max_uses": 5
}
```

It must be a positive integer; grant requests reject `0`, negative, fractional, and non-numeric values. Because the signature covers `scope`, a budget is tamper-evident — a holder cannot raise it without a fresh daemon-signed grant. An absent `max_uses` means unlimited, which is the behavior of every grant issued before budgets existed.

The budget is enforced at capability-check time. Each authorized use consumes one unit, the count is durable across daemon restart, and the check-and-consume is atomic per signature so two concurrent checks cannot both spend the final unit. Once the count reaches `max_uses` the action is refused and the daemon records a [`CapabilityBudgetExhausted`](./audit-integrity.md) audit event naming the spent grant's signature — distinct from a never-granted action, which records a failed capability check instead. `max_uses` is a lifetime budget: a spent grant stays spent, so the use count is never purged. Revoking and re-granting issues a fresh signature with a fresh budget.

Opt-in live coverage pins this boundary through the real daemon: a `max_uses = 1` grant runs once, the second call is refused with a `CapabilityBudgetExhausted` row, and after the daemon is restarted against the same state the refusal still holds — the spent budget is not refilled.

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

Live HTTP coverage pins this boundary for `tool.call.echo`: a scoped grant rejects a mismatched argument object before dispatch and permits only the exact allowed object.

### `memory.*`

Use for memory reads, writes, repair, compaction, purge, and receipt backfill.

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
- `memory.backfill.apply` and `memory.backfill.dry_run` are distinct grants for the memory-record receipt-correlation backfill; the backfill mode is part of the action and a scope may pin `apply` to bind a grant to a single mode. `before_ms` bounds the backfill to records at or before a millisecond cutoff (inclusive); `null` or an absent value is unbounded. Grants for `memory.backfill.*` reject `tiers` and `record_id` at validation time because the dispatch predicate does not bind by tier or record.
- The `memory backfill-receipt-correlation` command (IPC `BackfillMemoryRecords`, HTTP `POST /memory/records/backfill`) enforces this scope at dispatch: an apply requires `memory.backfill.apply`, a dry run requires `memory.backfill.dry_run`, and the operator identity is required. The backfill correlates every legacy row with no recency filter, so the dispatch probes the scope with an unbounded cutoff — a recency-bounded grant (`before_ms` set) does not authorize a full repair. Correlations are recomputed server-side from the operator's own memory and receipt rows; clients cannot supply correlations directly.

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
- Each verb is a distinct action and is matched exactly, so a grant for one verb authorizes no other: `a2a.repair.requeue` and `a2a.repair.force_error` are separate deny-by-default grants, and holding `requeue` never authorizes the destructive `force_error` arm.

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

### `capabilities.*`

Use for revoked-capability retention. `capabilities.purge` gates `covenant capabilities purge` (IPC `Request::PurgeCapabilities`, HTTP `POST /capabilities/purge`), the garbage collection that removes revoked-capability rows by a millisecond cutoff.

```json
{
  "version": 1,
  "before_ms": null
}
```

Rules:

- `before_ms` narrows purge authority: when present on `capabilities.purge`, the requested cutoff must be less than or equal to the scoped value; `null` or an absent field is unbounded. The dispatch predicate is the same `before_ms` cutoff enforced for `audit.purge` and the `peers.purge` retention sweep.
- The operator identity remains the root authority for capability-registry control. A scoped `capabilities.purge` grant is delegated retention authority for a non-operator peer.
- Grant-time validation does not yet bind the `capabilities.*` namespace, so a non-empty scope is preserved as signed metadata at grant time and only `before_ms` is interpreted at dispatch. Treat the cutoff as an enforced dispatch bound, not a grant-time-validated envelope.

### `peers.*` and `identity.*`

Use for peer registry and local identity operations.

```json
{
  "version": 1,
  "peer_pubkey_b58": null,
  "token_prefix": null,
  "self": null,
  "force": null,
  "before_ms": null
}
```

Rules:

- The operator identity remains the root authority for local peer-registry control. Scoped peer grants are delegated authority for non-operator peers.
- `peer_pubkey_b58` narrows delegated `peers.list`; when present, it must decode to a 32-byte base58 public key and the request must use the exact target pubkey as `pubkey_prefix`.
- `token_prefix` narrows delegated `peers.revoke`; when present, it must be a non-empty base58 prefix. The requested token prefix must start with the scoped prefix, and the daemon's normal ambiguity checks still run before mutation.
- `force` narrows delegated revoke requests when present. `force: false` permits only non-force revocations; `force: true` permits only force revocations.
- `before_ms` narrows `peers.purge`; the requested cutoff must be less than or equal to the scoped value.
- `self` is reserved for self-targeting peer and identity operations; when present, it must match the daemon's concrete self-target predicate.

Live coverage pins this boundary through a non-operator `peers.revoke` case: missing grants are denied, mismatched `token_prefix` scopes are denied before mutation, and a matching token-prefix scope revokes only the scoped target.

### `chain.*`

Use for local settlement and receipt batching.

```json
{
  "version": 1,
  "limit": 100,
  "mint": null,
  "cluster": null,
  "payer_pubkey_b58": null,
  "resource": null,
  "batch_id": null
}
```

Rules:

- `chain.receipts` gates local receipt reads; `chain.batches` gates local receipt batch summaries; `chain.flush` gates local receipt batching.
- `limit` bounds read and batch sizes. A request above the scoped limit is rejected before receipts are read or batched.
- `payer_pubkey_b58` narrows receipt rows to a 32-byte base58 payer key. The daemon still applies the authenticated-payer filter first.
- `resource` narrows receipt rows to `compute`, `memory`, `tool`, `message`, or `registration`.
- `cluster` and `batch_id` narrow already-batched receipt rows. Unbatched local receipts do not satisfy a concrete `cluster` or `batch_id` selector.
- `mint` is checked against the configured settlement mint for `chain.flush`; a concrete mint selector does not match if the daemon has no configured mint.

### `settlement.*`

Use for local settlement-receipt maintenance.

```json
{
  "version": 1,
  "apply": true,
  "before_ms": null
}
```

Rules:

- `settlement.backfill.apply` and `settlement.backfill.dry_run` are distinct grants; the backfill mode is part of the action. A scope may pin `apply` to bind a grant to a single mode.
- `before_ms` bounds the backfill to receipts at or before a millisecond cutoff (inclusive); `null` or an absent value is unbounded.
- The `settlement backfill-receipts` command (IPC `BackfillSettlementReceipts`, HTTP `POST /settlement/receipts/backfill`) now enforces this scope at dispatch: an apply requires `settlement.backfill.apply`, a dry run requires `settlement.backfill.dry_run`, and the operator identity is required. The backfill repairs every legacy row with no recency filter, so the dispatch probes the scope with an unbounded cutoff — a recency-bounded grant (`before_ms` set) does not authorize a full repair.

### `x402.*`

Use to bound an agent's outbound paid-call egress to a destination class.

```json
{
  "version": 1,
  "provider": "xona"
}
```

Rules:

- `x402.outbound.pay` authorizes the daemon to make an outbound x402 paid call on the caller's behalf. The call's `provider` and `endpoint` are caller-supplied, so an unscoped grant authorizes payment to *any* destination.
- `provider` binds the grant to one destination class: a scoped grant only authorizes calls whose provider matches. An empty scope, or one that omits `provider` (or sets it to `null`), keeps the unbounded blanket behavior. The field must be a non-empty string or `null`. The bound is the logical `provider` label, not the `endpoint` URL — a granted provider can still be paired with any endpoint, so treat `provider` as the destination-class boundary, not a per-URL allowlist.
- Capabilities are additive, so several destinations are expressed as several grants — the same shape as per-tool `tool.call.<name>` grants.
- The `PayX402` dispatch enforces this scope at dispatch, after the action check and before the dispatch-config check: a call to a provider outside the granted class is refused with a `CapabilityScopeRejected` audit event, so a denied egress is recorded for incident review even on a daemon with no funding-key sidecar wired. Hyre's paid calls egress through `tool.call.hyre.*` and are already bound by that tool scope.

### `secret.*`

Use to bound an agent's daemon-mediated secret access to a named secret.

```json
{
  "version": 1,
  "name": "openai-api-key"
}
```

Rules:

- `secret.access` authorizes the holder to fetch a named secret from the daemon's secret broker (`GetSecret`) at call time instead of receiving it in the agent's process environment, where a compromised agent could read it. The requested `name` is caller-supplied, so an unscoped grant authorizes reading *any* secret the broker holds.
- `name` binds the grant to one secret: a scoped grant only authorizes a read whose name matches. An empty scope, or one that omits `name` (or sets it to `null`), keeps the unbounded blanket behavior. The field must be a non-empty string or `null`.
- Capabilities are additive, so several secrets are expressed as several grants — the same shape as per-tool `tool.call.<name>` grants.
- The `GetSecret` dispatch enforces this scope after the action check and before the not-configured check: a read of a name outside the grant is refused with a `SecretAccessDenied` audit event, and a released secret records a `SecretAccessGranted` event naming the secret and the `signature_b58` of the grant that authorized the read; neither names the value, so secret use is accountable for incident review even on a daemon with no secret source wired. The broker serves nothing until an operator wires a secret source; sourcing real credentials from an external store is operator configuration, not part of the enforcement mechanism.

## Enforcement Path

1. Keep accepting `{}` for existing broad grants.
2. Validate non-empty scopes at grant time for known action namespaces.
3. Interpret the stable `tool.call.*` `arguments.allow` predicate at dispatch.
4. Interpret the stable `audit.purge` and `capabilities.purge` `before_ms` cutoffs at dispatch.
5. Interpret stable memory read, write, purge, repair, and compaction predicates at dispatch.
6. Interpret stable A2A peer, task, lease, and duplicate-risk predicates at dispatch.
7. Interpret stable peer-registry list/revoke/purge predicates, chain receipt-read, batch-read, and flush predicates, the settlement receipt-backfill predicate, the `x402.outbound.pay` destination-class predicate, and the `secret.access` named-secret predicate at dispatch.
8. Fail closed for malformed versioned scopes after a migration window.
9. Keep action-only checks as the fallback only for unscoped operator grants.

Until a namespace-specific predicate lands, public docs must describe that namespace's scope as validated signed metadata and compatibility preparation, not as enforced least-privilege behavior.

## Machine-readable inspection

`covenant capabilities recent --json` emits one stable object for supervisors:

```json
{
  "kind": "capability_list",
  "limit": 10,
  "capabilities": []
}
```

Each row is the wire `SignedCapability`, including `capability`, `scope`, optional `expires_at`, and the base58-encoded `signature`. Human output from `covenant capabilities recent` remains unchanged.

Retention maintenance has the same machine-readable convention:

```json
{
  "kind": "capabilities_purged",
  "before_ms": 1700000000000,
  "purged": 0
}
```

`before_ms` is the effective cutoff, including values derived from `--older-than-ms`.

### Operator capability-usage query

`covenant capabilities recent` reports the signed grant ledger but not how much of a `max_uses` budget a grant has spent — a holder learns its remaining budget only by being refused. The daemon exposes an operator-only read query over capability state for that visibility. It is an IPC `Request` of kind `capability_usage` (no CLI verb yet) and returns:

```json
{
  "kind": "capability_usage",
  "grants": [
    {
      "signature_b58": "3xS9Yk1f8wL2bN7pQz4mRtUvJh6cKaDe5gXyWnVoBqAr",
      "action": "tool.call.echo",
      "scope": { "version": 1, "tool": "echo", "max_uses": 5 },
      "subject_display": "agent@host",
      "subject_pubkey_b58": "5Gw3z9KpXqL8mNvR2tY7hJ4cF6bA1sDeZxWnVoBqUtM",
      "expires_at": 1700000000000,
      "revoked": false,
      "effective": "live",
      "budget": { "max_uses": 5, "used": 2, "remaining": 3 }
    }
  ]
}
```

One entry per grant in the ledger, including revoked-but-not-yet-purged grants (flagged `revoked: true`). `subject_display` and `subject_pubkey_b58` name the agent the authority is delegated to — the holder — sourced from the signed capability's subject, with the base58 pubkey as the stable identity and the display as the human label; this is the grantee, never the grantor that issued the grant. So the query answers which agent holds each grant, not only what it permits. `budget` is present only for grants that declared a `max_uses` budget; an unbudgeted grant omits the field. `used` is the durable count the enforcement path has recorded — it is read from the same `uses.jsonl` ledger `consume_uses` maintains, without recording a use, so it survives daemon restart and never advertises a refilled budget. `remaining` is `max_uses - used`.

`scope` is the grant's signed scope verbatim — the constraints that bound what the authority permits within its `action`: the tool a `tool.call` grant is pinned to, the recipient an `a2a.send` grant may message, a path or host constraint, the `max_uses` budget. It is sourced from `signed.capability.scope`, the same object the signature covers and the daemon enforces, so the reported scope cannot diverge from the one in force. Without it, two grants for the same action with different scopes are indistinguishable — an operator sees the action verb but not the boundary that narrows it. Unlike `budget`, `scope` is always present and emitted exactly as signed, including for an unscoped grant (whatever the capability carries, for example `null` or `{}`), so the wire shape stays stable.

`effective` is the daemon's own verdict on whether the grant would authorize an action right now — one of `live`, `expired`, `revoked`, or `exhausted` — computed with the daemon clock and the same predicates the enforcement path applies. It is reported so an operator reads the daemon's decision directly rather than re-deriving it from `expires_at`, `revoked`, and `budget`, where a different clock or precedence could disagree with enforcement. Its precedence matches enforcement order: a revoked grant is dropped from the live set before expiry is checked, and a grant's budget is consumed only after the expiry-aware signature check passes, so `revoked` dominates `expired`, which dominates `exhausted`. A grant past its `expires_at` reports `expired` (using `now > expires_at`, so the grant is still `live` at the exact expiry millisecond, matching the check path).

The query joins grants to their use counts and revocations by `signature_b58`, the base58 ed25519 grant signature, not by `action`. Several grants for the same action therefore stay distinct, each reporting its own subject, scope, and budget — the scope stays bound to its grant rather than transposing onto the shared action verb.

The query requires the operator identity. A peer that merely holds a grant is refused, so delegated-authority state — which capabilities exist, which agent holds each, what scope bounds each, and how much budget remains — never leaks to a non-operator. The boundary is observability only: it reports on-disk capability state and changes nothing.

An opt-in live test (`live_capability_usage_introspection.rs`) exercises the query through the real daemon: it grants a `max_uses` budget, spends part of it through real tool calls, asserts the reported `used`/`remaining`, and restarts the daemon against the same state to confirm the count is read from the durable ledger rather than a counter that resets on restart.

A second opt-in live test (`live_capability_usage_effective_status.rs`) pins `effective` to enforcement through the real daemon: it drives three grants into `exhausted` (its single unit spent through a real tool call, after which the next call is refused), `live` (unbudgeted), and `revoked` (revoked through `RevokeCapability`, still flagged in the snapshot), asserts the query reports each status joined by signature, then restarts the daemon against the same state and re-asserts all three — so the status is shown to be derived from the durable granted/uses/revoked ledgers plus the daemon clock, not an in-memory value a restart would reset.

A third opt-in live test (`live_capability_usage_subject_attribution.rs`) pins the subject through the real grant path: it grants `tool.call.echo` as the authenticated operator and asserts the entry attributes the grant to that identity by `subject_display` and `subject_pubkey_b58` (joined on the signature, the pubkey checked against the daemon's persisted identity key), then restarts the daemon against the same state and re-asserts the same subject — so the attribution is shown to be read from the durable grant ledger and the reloaded identity, not an in-memory value a restart would reset. Because `GrantCapability` assigns the subject from the authenticated peer, the live path attributes the grant to a single identity; subject-vs-grantor transposition between distinct holders stays covered by the in-process unit tests.

A fourth opt-in live test (`live_capability_usage_scope_visibility.rs`) pins the `scope` through the real grant path: it grants two `tool.call.echo` capabilities with distinct scopes as the operator — differing on an `arguments.allow` constraint, since the `tool` field is bound to the action suffix — and asserts each entry reports its own signed scope verbatim, joined on the signature, then restarts the daemon against the same state and re-asserts both scopes — so the scope is shown to be read from the durable grant ledger, not an in-memory value a restart would reset. Because the operator chooses each grant's scope, the live path drives distinct scopes for one action and confirms they cannot transpose onto the shared action verb, the half the in-process unit tests prove against constructed state.
