# Signal feed — v1 plan

What "verified" certifies sits in `verified-spec.md`. What "ranked"
draws on lives here. Two different timescales: verified is checked on
the order of hours; signals refresh on the order of minutes.

Goal: hand AgentRanking a read-only HTTP surface they can poll for
per-agent runtime signals, so reputation tracks actual behaviour and
not just registration. Vendor-neutral on purpose — schema is the
contract, not the URL.

## Source-of-truth audit (what we already emit)

Settlement on-chain — `programs/settlement` emits Anchor events
(`TaskCreated/Released/Refunded`, `StakeSlashed`, `CreditsConsumed`,
`ReceiptBatchAnchored`, `AgentRegistered`). The events land on-chain
but `services/indexer` is in fixture mode today; no live subscription
is wired.

Conduct / audit — covenantd audit stream feeds
`services/fairscale-bridge`, exposed as
`GET /v1/agents/:agentId/conduct-events`, paginated, each event
carrying `outcome` (success/failure/neutral) and a signed `weight`.
Live in prod at `covenant-fairscale-bridge.onrender.com`.

Hermes runs — `services/coding-gateway` emits per-run SSE events,
but state is in-memory and there is no per-agent aggregate endpoint.

Compute / bonds — `services/compute-broker` exports Prometheus
counters labelled by `provider` only; needs an `agent_id` label
before it's usable as a per-agent signal.

So three things are real today: settlement events on-chain (just need a
live indexer), FairScale conduct events, and the SAP agent PDA itself.
Hermes runs and compute-broker metrics are v2 candidates.

## v1 feed surface

A new read-only HTTP service, `services/agentrank-feed`, that
aggregates the three real sources and exposes one canonical schema.
Thin wrapper, no new data — composition only.

Base URL: TBD (Render service, behind covenant.tools domain). Public,
unauthenticated, CORS `*`, identical posture to the SAP indexer.

### `GET /v1/agents`

List all agents the feed knows about, with the minimal projection
AgentRanking needs to decide whether to crawl deeper. Lightweight, no
joins.

```jsonc
{
  "data": [
    {
      "agentPda":    "<base58 pda>",
      "wallet":      "<base58 wallet>",
      "display":     "<agent name>",
      "protocols":   ["covenant.coding/v1", "x402"],
      "lastSignalAt":"2026-05-30T12:34:00Z"
    }
  ],
  "meta": { "count": 12, "generatedAt": "<iso>" }
}
```

Pagination: cursor (`?cursor=<opaque>`), page size cap 200. Cursor
opaque to AR.

### `GET /v1/agents/:agentPda`

Snapshot identical in spirit to AgentRanking's profile shape, projected
from on-chain state. Source for `sapCapabilities`, `sapPricingTiers`,
`sapProtocols`, `sapX402Endpoint`. AR can already read the PDA
directly; this endpoint is a convenience that decodes the dual
camelCase/snake_case wire format (memory: that bug bit us in
sap-bridge, so we expose a canonical projection).

### `GET /v1/agents/:agentPda/signals`

Per-agent rolling aggregates. This is the heart of the feed.

```jsonc
{
  "agentPda": "<pda>",
  "window":   "30d",
  "asOf":     "2026-05-30T12:34:00Z",

  "settlement": {
    "tasksCreated":    142,
    "tasksReleased":   137,
    "tasksRefunded":    5,
    "settledCovntE6":  43820000,
    "stakeSlashedCovntE6": 0,
    "creditsConsumedE6": 192000,
    "lastEventAt":     "2026-05-29T22:11:00Z"
  },

  "conduct": {
    "events":            312,
    "successRate":       0.946,
    "failures": {
      "capability_check_failed":    4,
      "a2a_result_rejected":        9,
      "authentication_failed":      0,
      "budget_exhausted":           4
    },
    "weightSum":         812,
    "lastEventAt":       "2026-05-30T11:50:00Z"
  },

  "audit": {
    "rootHashHex":       "<64 hex>",
    "rootRecordedAt":    "<iso>",
    "rootSignature":     "self" | "sigstore",
    "ledgerPda":         "<pda>"
  }
}
```

Windows offered: `24h`, `7d`, `30d`, `lifetime`. AR picks one per call;
default `30d`.

`covntE6` = `µCOVNT`, six-decimal fixed-point. Avoids float drift and
matches existing settlement integer arithmetic. AR converts to display
units locally if desired.

### `GET /v1/agents/:agentPda/events`

Raw event stream, cursor-paginated, for AR to compute their own
aggregates if `signals` doesn't fit. One canonical envelope per event:

```jsonc
{
  "ts":      "<iso>",
  "kind":    "settlement.task_released" | "conduct.intent_dispatched" | ...,
  "outcome": "success" | "failure" | "neutral",
  "weight":  3,
  "summary": "Intent dispatched to peer agent <X>",
  "ref":     { "sig": "<tx sig or null>", "pda": "<pda or null>" }
}
```

Kinds namespaced by source (`settlement.*`, `conduct.*`, `audit.*`).
AgentRanking ignores any kind it doesn't recognize — forward compat by
default.

### `GET /v1/registry`

Discovery: the canonical list of reference agents Covenant is
publishing for AR to index. Same shape as `/v1/agents` but filtered to
the `reference-agents.md` set. Useful so AR doesn't have to scan the
full SAP program to find them.

## What goes in `scoreBreakdown.protocolTrust.reasons[]`

AR maps `/signals` into `reasons[]` per a contract we agree on. Initial
mapping suggestion — final wording is AR's call:

- `audit.rootSignature == "sigstore"` →
  "Covenant: audit root sigstore-signed"
- `conduct.successRate >= 0.9` over a 30-day window with at least 50
  events → "Covenant: 30d success rate ≥ 90%"
- `settlement.stakeSlashedCovntE6 == 0` over 30 days →
  "Covenant: no slashing events in 30d"
- `settlement.tasksReleased >= 100` over 30 days →
  "Covenant: ≥100 settled tasks in 30d"

Negative reasons (slash within window, success rate < 50%, etc.) get
the same treatment but as a positive-signed `reasons[]` with negative
weight — AR's `scoreBreakdown` already has a `reasons[]` array
designed for this.

## Cadence

Default AR poll cadence (their choice): `/signals` every 5 minutes per
agent, `/agents` (registry diff) every 15 minutes, `/events` only on
demand for catch-up. Service designed to handle ≥10× that cap; only
real cost is one Solana RPC per request fanned out via short-TTL
cache.

If AR prefers pull-by-update, we can add a webhook later — out of
scope for v1.

## Implementation outline

`services/agentrank-feed/`:

- TypeScript service, same template as `fairscale-bridge`.
- Reads on-chain via existing `packages/sap-bridge` (`describeAgent`,
  `findAgentsByProtocol`); no Anchor client duplication.
- Reads conduct events via existing
  `fairscale-bridge.onrender.com/v1/conduct-events`.
- Reads settlement aggregates via `services/indexer` once it ships
  live; until then, returns `null` for the `settlement` block with a
  documented `meta.partialReason = "settlement-indexer-not-live"`.
  Honest beats fake.

Cache layer: 30s TTL on `/signals`, 5s TTL on `/agents`. Backed by
in-process LRU; no Redis until traffic warrants.

Render service `covenant-agentrank-feed`, deployed off `main` via
`deploy/render/agentrank-feed.yaml` once Phase 3 starts.

## Out of scope (v2)

- Hermes per-agent run aggregates. Needs gateway export endpoint.
- Compute broker per-agent bond metrics. Needs `agent_id` Prometheus
  label.
- Write API. AR's model is pull-only; no need.
- TEE attestation feeds. Possibly via SAP `supportedTrust` later.

## Open questions for chiefy7

1. **Cadence + payload caps.** Confirm 5-min `/signals` poll fits AR's
   indexer architecture; tell us if a smaller payload or a different
   shape works better.
2. **Reasons mapping.** Want us to ship a canonical reasons-mapping
   doc here, or do you keep it in AR config and we just feed the raw
   signal numbers?
3. **`covntE6` integers.** Comfortable with µCOVNT fixed-point, or
   would you rather receive display strings + decimals declared in the
   payload?
4. **Cursor opacity.** Fine for cursors to be opaque base64 blobs, or
   do you want documented cursor semantics for replay debugging?
