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

| Component | State | Where |
| --- | --- | --- |
| `KeeperScope`, scope predicate | shipped | `src/capability.rs` |
| `KeeperPolicy::decide` (mark/crank) | shipped | `src/policy.rs` |
| `RecoveryPolicy::decide` (HealthCertV16 → ForfeitRecoveryLeg) | shipped | `src/policy.rs` |
| `PercolatorClient` trait + `MockPercolator` | shipped | `src/client.rs` |
| `KeeperAgent::tick` (scope → coord → budget → exec → receipt) | shipped | `src/keeper.rs` |
| Coordination (`should_lead`, action_key, FNV-1a) | shipped | `src/coordination.rs` |
| Risk-engine dep (pinned commit `323c9f27`) | shipped | `Cargo.toml` |
| Property + stress tests (5 props × 64 + 3 stress + unit) | shipped | `tests/`, `src/*::tests` |
| Wire-locked instruction builders (tags 5/43/44/45/63) | shipped (`--features solana`) | `src/instruction.rs` |
| `KeeperAction` → `Instruction` bridge (`BuildContext`) | shipped (`--features solana`) | `src/onchain.rs` |
| Operator-runnable binary (`covenant-percolator-keeper`) | shipped | `src/bin/keeper.rs` |
| RPC / signer / bundler integration | **deferred** | operator-supplied (Jito, custom) |

The default build pulls no Solana runtime crates. Under
`--features solana`, the crate exposes byte-for-byte builders for the
v16 keeper surface — discriminator bytes are pinned by golden tests
(`*_wire_bytes_locked`) against the program's `Instruction::encode`
in `percolator-prog/src/v16_program.rs`. The keeper hands callers an
`Instruction` and stops there: signing and submission (RPC, Jito
bundles, custom senders) are the operator's choice.

---

## 8. Open questions

- **Stake-backed slashing surface.** The SAP-anchored agent identity is the
  obvious trust root; the precise slashable invariant (e.g. "submitted an
  action your scope didn't permit") and the on-chain bond mechanism are not
  yet designed.
- **Cross-protocol generalization.** PAK is shaped around Percolator's v16
  surface (push-mark, crank, recovery trio). The same governance pattern
  applies to any permissionless protocol that needs off-chain liveness
  actors; the right level of abstraction (per-protocol crate vs. generic
  framework) is open.
- **Liveness SLA under partition.** What's the formal SLA the network
  delivers when M of N keepers are offline? `proptest` exercises a single
  agent's behavior; an N-keeper liveness sim that injects partitions and
  measures freshness SLA is future work.

---

## 9. Why this engages the design

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

## 10. References

- `aeyakovenko/percolator/spec.md` — risk engine spec (source of truth).
- `aeyakovenko/percolator-prog/README.md` — trust boundaries, account model.
- `aeyakovenko/percolator-prog/kani_audit.md` — 81/81 proof inventory.
- `aeyakovenko/percolator-prog/security.md` — discarded-candidate audit log.
- `aeyakovenko/percolator-stress-test/{bounty_v13.md, max_risk.md}` — adversarial scenarios.
- `aeyakovenko/percolator-cli/src/v16/instructions.ts` — wire-format reference.
- Companion code: `open-covenant/covenant` branch `feat/percolator-keeper`,
  `agent-os/crates/covenant-percolator`.
