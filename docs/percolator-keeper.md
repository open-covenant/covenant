# Percolator keeper

Covenant-governed keeper agents for the Percolator perpetuals protocol
(`aeyakovenko/percolator-prog`). Percolator's v16 design deliberately
pushes liveness onto permissionless participants: per-asset staleness
gating, self-managed oracle freshness, an open `PermissionlessCrank`,
and a recovery sequence anyone may invoke. The open question that
design leaves on the table is **who runs the keepers — reliably,
without being a trusted single operator.** This crate is the
accountability layer that makes a swarm of independent operators safe.

## Shape

| Concern | Where | Notes |
| --- | --- | --- |
| State + actions | `state.rs` | `MarketState` / `AssetState` (per-asset staleness slot), `KeeperAction` mirroring `PushHyperpMark` / `PermissionlessCrank` / the recovery trio (`ForfeitRecoveryLeg` / `RebalanceReduce` / `FinalizeResetSide`). |
| Decision policy | `policy.rs` | Pure `KeeperPolicy::decide(market)` → `Vec<KeeperAction>`. Freshness first (unblocks gated ops), then crank. |
| On-chain seam | `client.rs` | `PercolatorClient` trait + `MockPercolator`. Real impl behind the `solana` feature once `percolator-prog`'s IDL is in-tree. |
| Capability scope | `capability.rs` | `percolator.keeper` action; scope JSON pins market, asset allowlist, verb allowlist, max actions per tick. |
| Agent loop | `keeper.rs` | `read → decide → for each: scope gate → atomic budget debit → execute → settlement receipt`. |

## Governance, per action

1. **Scope gate** — out-of-policy actions are dropped *before* any RPC or spend.
2. **Atomic budget debit** — `BudgetLedger::try_debit(payer, credits, receipt_id)`. `Exhausted` stops the tick; the next tick (after refill) picks up where it left off.
3. **On-chain submission** — via the client.
4. **Settlement receipt** — the receipt's id equals the debit's paired id, tying budget log ↔ settlement log 1:1; the daemon's batching path Merkle-roots the set and anchors on Solana.

So every keeper action carries: who acted (`payer` AgentId), under what authority (scope, signed by the granting operator), what it did (action + tx sig), what it cost (credits), and a single id you can join across the budget, settlement, and (eventually) audit logs.

## Why this is on-thesis for Percolator

Percolator's design **needs** the participants it permissionlessly invites. Today those participants are opaque bots with unbounded authority. With Covenant in front of them:

- An operator can run a keeper without trusting it — its authority is signed and scoped, its spend is capped, its behavior is verifiable.
- The network's *liveness* becomes self-verifying: anyone can audit what every keeper did, against the same on-chain records the protocol settles on.
- Per-asset staleness gating composes naturally with per-asset capability scopes — a keeper freshens *only* the assets it operates, and only those.

## What's live in this crate

- `KeeperAgent::tick` loop, fully exercised by 7 governance tests against `MockPercolator`:
  - stale active asset → push mark → receipt + debit (paired by id);
  - out-of-scope asset rejected before any spend;
  - budget exhaustion stops the tick mid-loop;
  - overdue crank fires under the right verb scope;
  - `max_actions_per_tick` caps a single pass;
  - non-`Active` assets are skipped;
  - scope-parse pins `version: 1` + `market`.

## What's deferred

- **On-chain `RealPercolator`** behind the `solana` feature. Two viable paths:
  - Anchor client + the published IDL from `percolator-prog` (cleanest once the IDL is vendored or fetched at build time).
  - Hand-rolled instruction builders following `src/v16/instructions.ts` in `aeyakovenko/percolator-cli`.

  Either way, the trait stays the same, the governance tests stay the same, and the on-chain wire-up is a thin adapter.

- **Liquidation decisions in `KeeperPolicy`.** The recovery trio is modeled in `KeeperAction`; the policy that decides *when* to recover (health-factor reads, leg sequencing) lives outside v1 because it depends on the per-position reads the real client surfaces.

- **A dedicated `AuditKind` variant** (e.g., `ExternalAgentAction { provider, action, target, payload_hash }`) — for v1, settlement receipts carry the per-action record; a generic audit variant in `covenant-audit` would let the daemon log a richer row.

## Disclaimer

Percolator is unaudited / "educational" per its own README. This crate
bounds what an agent does on Covenant's side; it does not make the
underlying protocol solvent. Start on devnet with small caps.
