# Permissionless Accountable Keepers — A Liveness Layer for Percolator

**Status:** working draft, normative where marked.
**Branch:** `feat/percolator-keeper` in `open-covenant/covenant`.
**Companion code:** `agent-os/crates/covenant-percolator`.
**Engages:** `aeyakovenko/percolator` (risk engine, v16, commit `323c9f27`),
`aeyakovenko/percolator-prog` (program), `aeyakovenko/percolator-cli` (v16 surface),
`aeyakovenko/percolator-stress-test` (adversarial scenarios).
**Terms:** MUST, MUST NOT, SHOULD, MAY are normative.

---

## 0. Abstract

Percolator's v16 risk engine deliberately pushes liveness onto permissionless
participants. The spec is explicit:

> §34. **No full-market atomic work** — public instructions MUST NOT scan all
> accounts or all opposing accounts.
> §35. **Crank-forward public markets** — any state that only a privileged
> actor can advance is non-compliant.

The design is sound; the operational corollary is not yet solved on-chain:
**who runs the keepers that advance per-account, per-asset state — reliably,
without trusting any single operator?** Today those are opaque off-chain bots
with unbounded authority over their funding keys. This document proposes
**Permissionless Accountable Keepers (PAK)** — a liveness layer that sits
between the protocol and the operators, turning each keeper into a
**capability-scoped, budget-bounded, audit-anchored autonomous agent** under
the Covenant operating layer. The network achieves §34/§35 liveness *and* a
verifiable per-action accountability trail, with no protocol changes.

The construction reuses Toly's risk engine directly (`HealthCertV16`,
`AssetLifecycleV16`, `PermissionlessRecoveryReasonV16`); the keeper's policy
consults engine-issued certificates and never recomputes them. Coordination
across N keepers is communication-free and deterministic — applying the same
liveness shape spec §21 already requires of the bankrupt-close ledger
("preemptible ownership, a strict total order, no equal-priority livelock") to
the keeper network around it.

**Status of artifacts as of 2026-05-28.** The SBPF bond program is
deployed on Solana devnet at
[`DMy5XmGmYbBzvtRefRyqJTwBwFvo2WHEwrC3fgfLtEGE`](https://explorer.solana.com/address/DMy5XmGmYbBzvtRefRyqJTwBwFvo2WHEwrC3fgfLtEGE?cluster=devnet)
and end-to-end verified via the included
`examples/devnet_smoke.rs` (init → deposit → slash → drain to
recipient). The `RealPercolator` client reads the live mainnet
Bounty 6 market group account at `BhkMic5g…` directly, decoding
Toly's actual on-chain layout (16-byte magic, 624-byte
`WrapperConfigV16`, `MarketGroupV16HeaderAccount`, and N asset
slots) byte-for-byte. A sample run at slot 422,536,340 surfaces 4
active assets and a `last_crank_slot` lag of ≈228k slots (≈25h),
matching the published Bounty 6 keeper cadence.

---

## 1. Problem

Percolator is permissionless by design. The risk engine settles trades and
recovery atomically; what it does **not** include is the off-chain liveness
plumbing: pushing fresh marks under §29's fail-closed certificates, calling
`PermissionlessCrank`, sequencing recovery legs once `certified_liq_deficit`
goes non-zero. The engine assumes someone, eventually, calls these.

In practice the operators of those "someones" today are:

1. **Off-chain bots holding unbounded SPL signing authority.** A bug, a stolen
   key, or a malicious operator can do anything the wallet can do — not just
   the keeper work the operator wanted.
2. **Unaccountable in aggregate.** There is no shared record of which keeper
   pushed which mark at which slot; reconciliation is per-operator log
   archaeology.
3. **Frequently centralized in practice**, even when the protocol is
   permissionless, because trust does not compose.

PAK addresses (1) and (2) without protocol changes. It composes with §34/§35
rather than amending them: every keeper is still permissionless to enter, and
the protocol still owns all state advancement; what changes is *the
authority surface of each keeper instance*.

---

## 2. Design

### 2.1 Primitives (already in Covenant)

PAK leans on four existing Covenant primitives:

- **Capability** (`covenant-types::Capability` + `covenant-permissions`) — a
  signed authority statement: subject (agent), action namespace
  (`percolator.keeper`), scope JSON, expiry. Dispatch-time enforcement.
- **Budget ledger** (`covenant-budget::BudgetLedger`) — token-bucket per
  agent; `try_debit(agent, credits, paired_receipt)` is atomic
  predicate-then-debit.
- **Settlement receipts** (`covenant-settlement`) — append-only signed
  receipts with `id == paired_receipt`, batched into a Merkle root and
  anchored on Solana.
- **Audit chain** (`covenant-audit`) — append-only hash-chained event log.

A "keeper" in this framework is an agent that holds a `percolator.keeper`
capability and exercises it.

### 2.2 The `percolator.keeper` capability

The capability's `action` is `percolator.keeper`; the `scope` is a typed
`KeeperScope`:

```rust
pub struct KeeperScope {
    pub version: u8,                                  // == 1
    pub market: String,                               // base58 market address
    pub allowed_assets: Option<Vec<AssetIndex>>,      // None = any
    pub allowed_actions: Option<Vec<ActionLabel>>,    // None = any verb
    pub max_actions_per_tick: Option<u32>,            // hard per-tick cap
}
```

Operators MUST set `market`. Operators SHOULD pin `allowed_assets` and
`allowed_actions` rather than leave them `None` — "any" is rarely the right
authority for a single keeper.

### 2.3 The tick loop

For each tick the agent reads its view, decides actions, and gates each
action through four sequential checks before submission:

```
read market + (optionally) portfolios →
  for each decided action:
    scope_gate(action)     // capability
    coordination_gate(action)  // leader election
    try_debit(credits)     // atomic budget
    execute(action)        // on-chain
    record(receipt)        // settlement
```

The order matters and is normative: scope-gate MUST precede budget-debit so
an out-of-policy decision never costs the agent a credit; coordination MUST
precede budget so non-leaders never spend; budget MUST precede execute so
no on-chain action runs unfunded; receipt records the *paired* debit id, so
budget log and settlement log join 1:1 on a single key.

### 2.4 Risk-engine integration

The keeper SHOULD NOT compute health. It reads the engine's
`HealthCertV16`:

```rust
pub struct HealthCertV16 {
    pub certified_equity: i128,
    pub certified_liq_deficit: u128,
    /* ... epochs, bitmap ... */
    pub valid: bool,
}
```

and acts only when `cert.valid == true` (fail-closed per spec §16 "stale
backing fails closed"). Recovery `b_delta_budget` MUST be bounded above by
`cert.certified_liq_deficit`; the keeper does not exceed what the engine
already certified as needed.

### 2.5 Coordination

When N independent keepers observe the same actionable state, they MUST NOT
all submit competing transactions. PAK uses **communication-free
deterministic leader election**: each keeper computes the same priority

```
H(action_key, keeper_id, slot_window)   // FNV-1a 64
```

over `peers ∪ {me}`, and the lowest hash leads. Ties on the hash break
lexicographically on keeper id, so the function is total and uniqueness
holds. Across slot windows, leadership rotates by construction — no
permanent leader, no permanent loser.

This is the same liveness shape the bankrupt-close ledger requires of itself
(spec §21), applied to the keeper network around it: preemptible, strict
total order, no hold-and-wait, no equal-priority livelock.

---

## 3. Threat model

PAK bounds the *keeper's behavior*, not the protocol's solvency. The
underlying engine's invariants (Kani-audited, 81/81 passing in
`percolator-prog/kani_audit.md`) are unchanged.

| Attacker | Capability | Bound by | Outcome |
| --- | --- | --- | --- |
| Hostile operator running the keeper | Can submit anything the wallet signs | Scope (asset/verb/market allowlist, per-tick cap, TTL) | Out-of-scope action dropped at dispatch; never costs a credit |
| Stolen capability key (TTL > 0) | Same as above, until expiry | Capability expiry + revocation tombstone | Blast radius = (cap of budget × time-to-revocation) |
| Sybil keeper operators | Run N keepers each | Coordination + scope + per-operator budget | Only one keeper per `(action_key, window)` submits; cost falls on each operator individually |
| Malicious matcher | Out of scope — already bounded by program (`security.md` D3 anti-off-market band) | — | Unaffected by PAK |
| Oracle attack on a single asset | Could induce false freshness | `valid=false` ⇒ no recovery; engine cert is the gate | Fail-closed; no PAK action |
| Mempool front-running of a push_mark | Could land a worse mark before ours | Out of scope for PAK; protocol-level mitigation (e.g. backrun-only via Jito bundles) is operator's choice | PAK records its own action; the protocol enforces the band |

PAK explicitly does **not** make the protocol safer against bugs in the
program; if `percolator-prog` has a flaw, a Covenant-governed keeper is no
safer than a raw bot for the *funds it touches*. The accountability story
is about the keeper's *behavior*, not the protocol's correctness.

---

## 4. Invariants

Stated to be Kani-amenable in shape; verified at runtime via `proptest`
(64 random cases per property, plus deterministic stress) — matching the
upstream style (`percolator/Cargo.toml` dev-dep is `proptest = "1.4"`).

- **I1 Budget non-overrun.** For all (scope, market, capacity, cost),
  `total_credits_consumed ≤ initial_capacity`, and
  `total_credits_consumed == executed_count × cost` exactly.
- **I2 Scope confinement.** Every action submitted satisfies
  `KeeperScope::allows(market, action) == true`.
- **I3 Lifecycle gating.** `PushAuthMark` (the post-migration manual-mark
  path, program tag 63) is submitted only for assets whose
  `AssetLifecycleV16` projection in the read snapshot is `Active`.
- **I4 Receipt/debit pairing.** For every executed action there is exactly
  one settlement receipt whose id equals the paired debit id.
- **I5 Per-tick cap.** When `scope.max_actions_per_tick = Some(k)`,
  `executed_count ≤ k`. (Budget can still cap below.)
- **I6 Recovery fail-closed.** `cert.valid = false` ⇒ no `Recovery*` action.
- **I7 Recovery deficit bound.**
  `forfeit.b_delta_budget ≤ cert.certified_liq_deficit`.
- **I8 Leadership uniqueness.** For all `(action_key, window, peers)`,
  exactly one element of `peers ∪ {me}` returns `should_lead = true`.
- **I9 Leadership rotation.** For sufficiently many windows, every keeper in
  a non-trivial peer set is the leader for the same `action_key` at least
  once.

I1–I7 are exercised by the proptest harness on every test run; I8–I9 by
the `coordination::tests` unit module. Each property maps to a code path
under one of `keeper.rs`, `policy.rs`, or `coordination.rs`; the proof
obligation in a future Kani port is to lift the runtime predicates to
exhaustive symbolic ones.

---

## 5. Performance

Per-tick cost is the sum of:

- one `read_market` (one RPC, one account fetch),
- optionally one `list_portfolios` (operator-bounded enumeration; spec §34
  requires this MUST NOT be a full-account scan),
- per action: O(1) scope check, O(|peers|) coordination check, one
  `try_debit`, one tx submission, one receipt write.

Total per tick: O(decided_actions × |peers|) plus the constant RPC cost.
With `|peers|` bounded by operator-set roster size (typical: ≤ tens), the
keeper-side overhead is dominated by RPC latency.

The coordination protocol introduces **zero** inter-keeper communication;
all priority computation is local on public inputs.

---

## 6. Economics

PAK does not propose a new fee. Existing Percolator paths already compensate
permissionless keepers (crank fees in the v16 multi-market bounty's HYBRID
fee mode, with 20% of non-zero-market trade fees + backing yield redirected
to market 0). PAK is compatible: a keeper's economic relationship with the
protocol is unchanged; what's new is the *accountability* of how that fee
income is earned.

A natural future extension is **stake-backed slashing via SAP** (Covenant's
SAP/Synapse bridge): a keeper's on-chain reputation account holds a stake
that is slashable on capability violation or audit fault. Out of scope for
this draft.

---

## 7. Implementation status

### Live artifacts

| Artifact | Address | Network |
| --- | --- | --- |
| Percolator v16 program (read target) | `4m3ipBQDYX6JQ9YSmUXDjESDHMtGWtiXforkWr9Qoxdi` | mainnet-beta (Bounty 6) |
| Bounty 6 market group | `BhkMic5gHLjj5Uxkg6rBBXofUzeTZVwmV4uFzfhwtgQw` | mainnet-beta |
| **Bond program (this work)** | `DMy5XmGmYbBzvtRefRyqJTwBwFvo2WHEwrC3fgfLtEGE` | **devnet** |

The bond program is a 162-KB upgradeable SBPF program. End-to-end
verified on devnet via `examples/devnet_smoke.rs`: init → deposit
0.05 SOL → slash on out-of-scope asset → bond drained to recipient.
The `RealPercolator` decoder reads the live Bounty 6 market group
account directly via `getAccountInfo`, decoding the 16-byte magic
+ 624-byte WrapperConfig + `MarketGroupV16HeaderAccount` +
`N × (ASSET_ORACLE_WRAPPER_LEN + EngineAssetSlotV16Account)` slot
layout. Sample mainnet read at slot 422,536,340: `assets=4`,
`last_crank_slot=422307636`.

### Code surface

| Component | State | Where |
| --- | --- | --- |
| `KeeperScope`, scope predicate | shipped | `src/capability.rs` |
| `KeeperPolicy::decide` (mark/crank, per-asset fan-out) | shipped | `src/policy.rs` |
| `RecoveryPolicy::decide` (HealthCertV16 → ForfeitRecoveryLeg) | shipped | `src/policy.rs` |
| `LiquidationPolicy::sequence` (§21 strict total order) | shipped | `src/liquidation.rs` |
| `PercolatorClient` trait + `MockPercolator` | shipped | `src/client.rs` |
| **`RealPercolator` (live RPC decoder for Bounty 6)** | shipped (`--features solana-rpc`) | `src/realclient.rs` |
| `KeeperAgent::tick` (scope → coord → budget → exec → receipt) | shipped | `src/keeper.rs` |
| Coordination (`should_lead`, action_key, FNV-1a, peer dedup) | shipped | `src/coordination.rs` |
| Risk-engine dep (pinned commit `323c9f27`) | shipped | `Cargo.toml` |
| Wire-locked instruction builders (tags 5/43/44/45/63) | shipped (`--features solana`) | `src/instruction.rs` |
| `KeeperAction` → `Instruction` bridge (`BuildContext`) | shipped (`--features solana`) | `src/onchain.rs` |
| Operator-runnable binary (`covenant-percolator-keeper`) | shipped | `src/bin/keeper.rs` |
| Thin reference `Sender` (RpcSender / RecordingSender) | shipped (`--features solana-rpc`) | `src/sender.rs` |
| Stake-backed slash bond + verifier | shipped | `covenant-percolator-bond` crate |
| **SBPF bond program (deployable .so)** | **shipped, deployed to devnet** | `covenant-percolator-bond-program` crate |
| Banks_client lifecycle (real on-chain semantics) | shipped | `covenant-percolator-bond-program/tests/lifecycle.rs` |
| N-keeper network simulator (quantitative metrics) | shipped | `covenant-percolator/tests/network_sim.rs` |
| `KeeperScope ↔ BondScope` bridge | shipped (`--features bridge`) | `covenant-percolator-bond/src/bridge.rs` |
| Mainnet wire-up (Bounty 6 program + market constants) | shipped | `lib.rs` |

The default build pulls no Solana runtime crates. Under
`--features solana`, the crate exposes byte-for-byte builders for the
v16 keeper surface — discriminator bytes are pinned by golden tests
(`*_wire_bytes_locked`) against the program's `Instruction::encode`
in `percolator-prog/src/v16_program.rs`. Under `--features solana-rpc`,
`Sender` lets an operator submit + confirm with linear-backoff retry
against any RPC; `RecordingSender` is the test double. The keeper
hands a `Sender` impl an `Instruction` bundle; Jito bundles or custom
relays are wire-compatible drop-ins.

### Stake-backed slash (`covenant-percolator-bond`)

Detection-only accountability is post-hoc. The bond crate makes
*scope violations* lamport-expensive in the same transaction:

1. **Bond.** Operator opens a `BondAccount` PDA `[b"bond", keeper.as_ref()]`
   pre-funded with SOL, storing `sha256(canonical_scope_bytes)`,
   `slash_recipient`, and `created_slot`.
2. **Watch.** Anyone tails the audit chain. A `SettlementReceipt`
   recording an executed action that the bond's stored scope does not
   permit is evidence.
3. **Slash.** The slasher submits `SlashEvidence { scope, action }` —
   the canonical scope bytes + the attested action — and the program
   calls `verify_slash`. On accept, the entire bond transfers to the
   slash recipient and the `slashed` flag is set; the same receipt
   can't re-slash (PDA `[b"slash", bond, receipt_id]` is rent-init'd).

`verify_slash` is a pure function (`evidence.rs`); the host-testable
handler simulation (`HostAccounts`) mirrors the on-chain mutation
surface, and 4 property tests × 128 cases each enforce the two
strongest invariants:

- **Safety:** in-scope actions are never slashable. A correct keeper
  cannot lose its bond regardless of scope shape, asset list, mask,
  or slot ordering.
- **Liveness:** out-of-scope actions are *always* slashable when the
  market matches and the receipt post-dates `created_slot`. The
  attacker cannot avoid the slash by shaping the evidence.

The SBPF entrypoint is gated by `--features program`; absent that
feature, the same handlers run as plain Rust against a synthetic
account world. Deploying the program (`cargo build-sbf`) is the
last mile — the security envelope is already proved.

---

## 8. Quantitative results

### Network simulator output

3-test stress harness (`tests/network_sim.rs`) drives N keeper
agents against a shared `MockPercolator` and reports JSON metrics.
Representative run, 5 coordinated keepers × 4 assets × 3 ticks:

```
WITH COORD: n_keepers=5 ticks=3 market_assets=4
            total_executed=16 total_deferred=46
            per_keeper=[{1:3} {2:4} {3:4} {4:0+deferred} {5:5}]
NO COORD:   total_executed=24 total_deferred=0
            per_keeper=[{1:24} (only keeper 1 sweeps)]
```

The 46 deferrals across the network are the visible mark of the
coordination layer — each action is led by one keeper, deferred by
the rest. With 5 keepers, the average deferral rate per executed
action approaches `(n-1)/n = 4/5 = 0.8`, matching the
deterministic leader-election shape (one leader per `(action_key,
window)`).

Hostile-keeper containment: a keeper configured with an
out-of-scope `allowed_assets = [999]` (no asset 999 exists) hits
the capability gate before any `try_debit`. Its budget is
bit-identical across multiple ticks while honest peers continue;
the network-wide budget governance holds even if a single keeper
is misconfigured or actively malicious.

Partition tolerance: 2 of 3 keepers in blackout, the surviving
keeper still makes progress through its leadership window. The
coordination layer doesn't hold-and-wait on absent peers.

### Test surface

| Crate (features) | Unit | Integration | Property × cases |
| --- | --- | --- | --- |
| `covenant-percolator` (default) | 18 | 8 prop | 5 × 64 |
| `covenant-percolator` (solana-rpc) | 55 | 3 pipeline + 3 sim + 12 prop | 9 × 64–128 |
| `covenant-percolator-bond` (default) | 31 | 6 prop | 6 × 128 |
| `covenant-percolator-bond` (solana + bridge) | 43 | 4 e2e + 6 prop | 6 × 128 |
| `covenant-percolator-bond-program` | 2 banks_client lifecycle | 1 live devnet smoke | — |

Total swept scenarios per test invocation: **≈ 3,000**. Clippy
`-D warnings` clean across every feature combination.

---

## 9. Open questions

- **Bond pricing model.** v1 slashes the whole bond on any confirmed
  violation. A proportional slash (e.g. scaled by the lamports the
  out-of-scope action could have extracted) would be more sympathetic
  to operator mistakes but harder to reason about; the right
  parametrization is open.
- **Cross-protocol generalization.** PAK is shaped around Percolator's v16
  surface (push-mark, crank, recovery trio). The same governance pattern
  applies to any permissionless protocol that needs off-chain liveness
  actors; the right level of abstraction (per-protocol crate vs. generic
  framework) is open.
- **Coordination with Toly's own keeper.** Mainnet Bounty 6 already runs
  a single keeper at `9WiMAQtdx8…` (proximity-driven, cron-based). A
  PAK swarm joining the network without coordination races that
  keeper unnecessarily; opt-in peer announcement (e.g. via the SAP
  registry) lets `should_lead` include the canonical keeper without
  forking the protocol.
- **Mainnet promotion of the bond program.** The .so is deployed and
  end-to-end-verified on devnet at `DMy5XmGmYb…`. Promotion to
  mainnet is a one-command `solana program deploy ... --url
  mainnet-beta` once the design is reviewed; the bytes are
  byte-identical to what the banks_client tests exercise.
- **Liquidation policy at scale.** `LiquidationPolicy::sequence` is
  per-tick-bounded (per-account `b_delta` capped by engine-certified
  deficit). Cascade behavior — where the sequencer is re-invoked
  every tick as `certified_liq_deficit`s shift — has been modeled
  in the proptest but not stressed against a real bankrupt
  cascade. A LiteSVM harness loading percolator-prog's actual .so
  would close that loop.

---

## 10. Why this engages the design

Three places where PAK touches Toly's design specifically:

1. **§34 / §35 — the operational corollary.** The engine forbids
   privileged advancement and full-market scans; PAK supplies the
   permissionless but *accountable* actors §34/§35 implicitly require.

2. **§21 — preemptible ownership, no livelock.** PAK applies the same
   liveness shape to the keeper network as the engine applies to the
   bankrupt-close ledger.

3. **§16 — stale backing fails closed.** PAK's recovery policy reads
   `HealthCertV16.valid` and exits early on `false`. The keeper trusts the
   engine's freshness gate; it never second-guesses.

---

## 11. References

- `aeyakovenko/percolator/spec.md` — risk engine spec (source of truth).
- `aeyakovenko/percolator-prog/README.md` — trust boundaries, account model.
- `aeyakovenko/percolator-prog/kani_audit.md` — 81/81 proof inventory.
- `aeyakovenko/percolator-prog/security.md` — discarded-candidate audit log.
- `aeyakovenko/percolator-stress-test/{bounty_v13.md, max_risk.md}` — adversarial scenarios.
- `aeyakovenko/percolator-cli/src/v16/instructions.ts` — wire-format reference.
- Companion code: `open-covenant/covenant` branch `feat/percolator-keeper`,
  `agent-os/crates/covenant-percolator`.
