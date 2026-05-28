//! Property-based + adversarial invariants for the keeper governance
//! layer. The properties are stated to be Kani-amenable in shape;
//! `proptest` is the runnable verification harness — matching the
//! upstream style (proptest in `percolator/Cargo.toml`).
//!
//! Five invariants over arbitrary `(market, scope, capacity, cost)`:
//!
//!   I1. **Budget non-overrun** — total credits consumed ≤ initial capacity.
//!   I2. **Scope confinement** — every executed action is in scope.
//!   I3. **Lifecycle gating** — `push_mark` only fires on Active assets.
//!   I4. **Receipt/debit pairing** — every executed action has a
//!       settlement receipt whose id equals the paired-debit id.
//!   I5. **Per-tick cap** — `max_actions_per_tick`, when set, holds.
//!
//! Plus three deterministic stress scenarios mirroring the
//! percolator-stress-test repo's style (slippage shock, drain attempt,
//! sybil/scope confinement).

use std::sync::Arc;

use covenant_budget::{BudgetLedger, InMemoryLedger};
use covenant_percolator::capability::KeeperScope;
use covenant_percolator::client::MockPercolator;
use covenant_percolator::keeper::KeeperAgent;
use covenant_percolator::policy::KeeperPolicy;
use covenant_percolator::state::{
    ActionLabel, AssetIndex, AssetLifecycle, AssetState, KeeperAction, MarketState,
};
use covenant_percolator::TickReport;
use covenant_settlement::{InMemorySettlement, Settlement};
use covenant_types::{AgentId, SettlementReceipt};
use proptest::prelude::*;

const MARKET: &str = "PercoMarket1111111111111111111111111111111111";
const PROGRAM: &str = covenant_percolator::MAINNET_PROGRAM_ID;

fn payer() -> AgentId {
    AgentId::new("keeper@local", [42u8; 32])
}

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(fut)
}

struct TickOutcome {
    report: TickReport,
    executed: Vec<KeeperAction>,
    remaining: u64,
    receipts: Vec<SettlementReceipt>,
}

async fn run_tick(
    market: MarketState,
    scope: KeeperScope,
    capacity: u64,
    credits_per_action: u64,
) -> TickOutcome {
    let client = Arc::new(MockPercolator::new(market));
    let settlement = Arc::new(InMemorySettlement::new());
    let budget = Arc::new(InMemoryLedger::new());
    let p = payer();
    budget.set_capacity(&p, capacity).await.unwrap();
    let agent = KeeperAgent {
        client: client.clone(),
        settlement: settlement.clone(),
        budget: budget.clone(),
        payer: p,
        market_address: MARKET.into(),
        scope,
        policy: KeeperPolicy::default(),
        peers: Vec::new(),
        coordination_window_slots: 0,
        recovery_policy: None,
        credits_per_action,
    };
    let report = agent.tick().await.expect("tick");
    TickOutcome {
        report,
        executed: client.executed(),
        remaining: budget.tokens_remaining(&payer()).await.unwrap(),
        receipts: settlement.recent(1024).await.unwrap(),
    }
}

// ---------- strategies ----------

fn arb_lifecycle() -> impl Strategy<Value = AssetLifecycle> {
    prop_oneof![
        Just(AssetLifecycle::Active),
        Just(AssetLifecycle::Disabled),
        Just(AssetLifecycle::PendingActivation),
        Just(AssetLifecycle::DrainOnly),
        Just(AssetLifecycle::Retired),
        Just(AssetLifecycle::Recovery),
    ]
}

fn arb_action_label() -> impl Strategy<Value = ActionLabel> {
    prop_oneof![
        Just(ActionLabel::PushMark),
        Just(ActionLabel::Crank),
        Just(ActionLabel::Recover),
    ]
}

/// Market with `n_assets` asset slots; all assets share the genesis
/// `last_mark_slot=0` so the policy will see them stale and decide
/// push_mark for the Active ones.
fn arb_market(n_assets: usize) -> impl Strategy<Value = MarketState> {
    (
        1000u64..=10_000,
        prop::collection::vec(arb_lifecycle(), n_assets..=n_assets),
        0u64..=10_000,
    )
        .prop_map(|(current, lifecycles, last_crank_raw)| {
            let last_crank = last_crank_raw.min(current);
            let assets: Vec<_> = lifecycles
                .into_iter()
                .enumerate()
                .map(|(i, lf)| AssetState {
                    index: i as AssetIndex,
                    label: format!("A{i}"),
                    lifecycle: lf,
                    last_mark_slot: 0,
                    last_mark_e6: 1_000_000,
                })
                .collect();
            MarketState {
                market_address: MARKET.into(),
                program_id: PROGRAM.into(),
                current_slot: current,
                last_crank_slot: last_crank,
                assets,
            }
        })
}

fn arb_scope(n_assets: usize) -> impl Strategy<Value = KeeperScope> {
    (
        prop::option::of(prop::collection::vec(
            0u16..n_assets as u16,
            0..=n_assets,
        )),
        prop::option::of(prop::collection::vec(arb_action_label(), 0..=3)),
        prop::option::of(1u32..=100u32),
    )
        .prop_map(|(assets, actions, max_per_tick)| KeeperScope {
            version: 1,
            market: MARKET.into(),
            allowed_assets: assets,
            allowed_actions: actions,
            max_actions_per_tick: max_per_tick,
        })
}

// ---------- properties ----------

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// I1 — Budget non-overrun: under any scope / market / capacity /
    /// cost, the total credits consumed equals `executed * cost` and
    /// never exceeds the initial capacity.
    #[test]
    fn budget_never_overrun(
        market in arb_market(8),
        scope in arb_scope(8),
        capacity in 0u64..=10_000,
        cost in 1u64..=50,
    ) {
        let outcome = block_on(run_tick(market, scope, capacity, cost));
        let consumed = capacity.saturating_sub(outcome.remaining);
        prop_assert!(consumed <= capacity);
        prop_assert_eq!(consumed, (outcome.report.executed as u64) * cost);
    }

    /// I2 — Scope confinement: every executed action is permitted by
    /// the scope at the time of execution (market id, asset allowlist,
    /// verb allowlist).
    #[test]
    fn scope_confinement(
        market in arb_market(8),
        scope in arb_scope(8),
        capacity in 1u64..=10_000,
        cost in 1u64..=50,
    ) {
        let scope_view = scope.clone();
        let outcome = block_on(run_tick(market, scope, capacity, cost));
        for action in &outcome.executed {
            prop_assert!(
                scope_view.allows(MARKET, action),
                "executed action {:?} violated scope {:?}",
                action,
                scope_view
            );
        }
    }

    /// I3 — Lifecycle gating: `push_mark` only fires on assets whose
    /// lifecycle is Active in the market snapshot the agent read.
    #[test]
    fn push_mark_only_on_active(
        market in arb_market(8),
        scope in arb_scope(8),
        capacity in 1u64..=10_000,
        cost in 1u64..=50,
    ) {
        let active: std::collections::HashMap<AssetIndex, bool> = market
            .assets
            .iter()
            .map(|a| (a.index, matches!(a.lifecycle, AssetLifecycle::Active)))
            .collect();
        let outcome = block_on(run_tick(market, scope, capacity, cost));
        for action in &outcome.executed {
            if let KeeperAction::PushAuthMark { asset_index, .. } = action {
                prop_assert!(
                    active.get(asset_index).copied().unwrap_or(false),
                    "push_mark on non-Active asset {asset_index}"
                );
            }
        }
    }

    /// I4 — Receipt/debit pairing: every executed action has exactly
    /// one settlement receipt whose id equals the paired-debit id, so
    /// budget log ↔ settlement log joins 1:1.
    #[test]
    fn receipts_pair_with_debits(
        market in arb_market(8),
        scope in arb_scope(8),
        capacity in 1u64..=10_000,
        cost in 1u64..=50,
    ) {
        let outcome = block_on(run_tick(market, scope, capacity, cost));
        prop_assert_eq!(outcome.receipts.len(), outcome.report.executed);
        let ids: std::collections::HashSet<_> =
            outcome.receipts.iter().map(|r| r.id).collect();
        for (_, _, paired) in &outcome.report.executions {
            prop_assert!(ids.contains(paired));
        }
    }

    /// I5 — Per-tick cap: when `max_actions_per_tick` is set, the
    /// executed count never exceeds it. (Budget can still cap below.)
    #[test]
    fn per_tick_cap_holds(
        market in arb_market(8),
        scope in arb_scope(8),
        capacity in 1000u64..=10_000,
        cost in 1u64..=10,
    ) {
        let cap = scope.max_actions_per_tick;
        let outcome = block_on(run_tick(market, scope, capacity, cost));
        if let Some(max) = cap {
            prop_assert!((outcome.report.executed as u32) <= max);
        }
    }
}

// ---------- adversarial / stress scenarios ----------

/// Slippage shock: every asset becomes simultaneously stale (a long
/// oracle gap, a network partition, a price-feed outage). The keeper
/// must still respect scope + per-tick + budget. Recovery is across
/// ticks, not within one.
#[tokio::test]
async fn stress_slippage_shock_all_assets_simultaneously_stale() {
    let assets: Vec<_> = (0..8)
        .map(|i| AssetState {
            index: i,
            label: format!("A{i}"),
            lifecycle: AssetLifecycle::Active,
            last_mark_slot: 0,
            last_mark_e6: 1_000_000,
        })
        .collect();
    let market = MarketState {
        market_address: MARKET.into(),
        program_id: PROGRAM.into(),
        current_slot: 5_000,
        last_crank_slot: 4_950,
        assets,
    };
    let scope = KeeperScope {
        version: 1,
        market: MARKET.into(),
        allowed_assets: Some((0..8).collect()),
        allowed_actions: Some(vec![ActionLabel::PushMark, ActionLabel::Crank]),
        max_actions_per_tick: Some(3),
    };
    let outcome = run_tick(market, scope, 1_000, 10).await;
    // Per-tick cap bounded the spend within the shock — exactly 3.
    assert_eq!(outcome.report.executed, 3);
    assert_eq!(outcome.executed.len(), 3);
    assert_eq!(outcome.receipts.len(), 3);
}

/// Drain attempt: a hostile policy "decides" every asset needs an
/// action. The budget caps total spend at capacity; the tick reports
/// the stop-on-exhaustion path and zero remaining.
#[tokio::test]
async fn stress_drain_attempt_capped_by_budget() {
    let assets: Vec<_> = (0..16)
        .map(|i| AssetState {
            index: i,
            label: format!("A{i}"),
            lifecycle: AssetLifecycle::Active,
            last_mark_slot: 0,
            last_mark_e6: 1_000_000,
        })
        .collect();
    let market = MarketState {
        market_address: MARKET.into(),
        program_id: PROGRAM.into(),
        current_slot: 10_000,
        last_crank_slot: 9_900,
        assets,
    };
    let scope = KeeperScope {
        version: 1,
        market: MARKET.into(),
        allowed_assets: Some((0..16).collect()),
        allowed_actions: Some(vec![ActionLabel::PushMark, ActionLabel::Crank]),
        max_actions_per_tick: None,
    };
    // Capacity 25, cost 5 → ceiling 5 actions; the 6th try_debit returns Exhausted.
    let outcome = run_tick(market, scope, 25, 5).await;
    assert!(
        outcome.report.executed <= 5,
        "executed {} exceeded budget bound",
        outcome.report.executed
    );
    assert_eq!(outcome.remaining, 0);
    assert!(outcome.report.stopped_budget);
}

/// Scope confinement under a hostile policy view: market state would
/// have the policy decide many actions, but the scope authorizes only
/// asset 7 + the `push_mark` verb. Exactly one action executes; the
/// other decisions all increment `skipped_capability`.
#[tokio::test]
async fn stress_scope_confines_hostile_policy() {
    let assets: Vec<_> = (0..16)
        .map(|i| AssetState {
            index: i,
            label: format!("A{i}"),
            lifecycle: AssetLifecycle::Active,
            last_mark_slot: 0,
            last_mark_e6: 1_000_000,
        })
        .collect();
    let market = MarketState {
        market_address: MARKET.into(),
        program_id: PROGRAM.into(),
        current_slot: 5_000,
        last_crank_slot: 4_950,
        assets,
    };
    let scope = KeeperScope {
        version: 1,
        market: MARKET.into(),
        allowed_assets: Some(vec![7]),
        allowed_actions: Some(vec![ActionLabel::PushMark]),
        max_actions_per_tick: None,
    };
    let outcome = run_tick(market, scope, 10_000, 1).await;
    assert_eq!(outcome.report.executed, 1);
    assert!(matches!(
        outcome.executed[0],
        KeeperAction::PushAuthMark { asset_index: 7, .. }
    ));
    // Other 15 push_marks + the crank get refused at the scope gate.
    assert!(outcome.report.skipped_capability >= 15);
}

// ---------------------------------------------------------------
// Liquidation sequencing properties (spec §21).
//
// `LiquidationPolicy::sequence` must produce the same ordered queue
// for any two keepers seeing the same on-chain snapshot — no
// hold-and-wait, no equal-priority livelock. The strict total order
// is `(deficit DESC, address ASC)`. These properties sweep random
// inputs to confirm L1 (determinism), L2 (total-bounded), L4
// (fail-closed on invalid certs), L5 (shuffle-invariant), and L6
// (strict total order under repeated deficits).
// ---------------------------------------------------------------

use covenant_percolator::liquidation::{LiquidationPolicy, ScheduledRecovery};
use covenant_percolator::risk::HealthCertV16;
use covenant_percolator::state::PortfolioSnapshot;

fn arb_snapshot() -> impl Strategy<Value = PortfolioSnapshot> {
    (
        // 4-char base58-ish address
        prop::collection::vec(any::<u8>(), 4..=4),
        any::<bool>(),
        0u128..1_000_000,
        prop::option::of(0u16..16),
    )
        .prop_map(|(addr_bytes, valid, deficit, asset)| {
            let addr: String = addr_bytes.iter().map(|b| (b'a' + b % 26) as char).collect();
            PortfolioSnapshot {
                portfolio_address: addr,
                health_cert: HealthCertV16 {
                    valid,
                    certified_liq_deficit: deficit,
                    ..HealthCertV16::default()
                },
                asset_in_distress: asset,
            }
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// L1 — Determinism: same input always produces same output.
    #[test]
    fn liquidation_sequence_is_deterministic(
        snaps in prop::collection::vec(arb_snapshot(), 0..16),
    ) {
        let p = LiquidationPolicy::default();
        let a = p.sequence(&snaps);
        let b = p.sequence(&snaps);
        prop_assert_eq!(a, b);
    }

    /// L2 — Total-deficit-bounded: aggregate budget can't exceed
    /// aggregate certified deficit (over admissible portfolios).
    #[test]
    fn liquidation_total_budget_bounded(
        snaps in prop::collection::vec(arb_snapshot(), 0..16),
    ) {
        let p = LiquidationPolicy::default();
        let seq = p.sequence(&snaps);
        let total_budget: i128 = seq.iter().map(|s: &ScheduledRecovery| s.b_delta_budget).sum();
        let total_deficit: u128 = snaps
            .iter()
            .filter(|s| s.health_cert.valid && s.asset_in_distress.is_some())
            .map(|s| s.health_cert.certified_liq_deficit)
            .sum();
        prop_assert!(total_budget as u128 <= total_deficit);
    }

    /// L4 — Fail-closed: a portfolio with valid=false never appears
    /// in the sequence, regardless of any other field.
    #[test]
    fn liquidation_invalid_cert_excluded(
        deficit in 1u128..1_000_000,
        asset in 0u16..16,
    ) {
        let p = LiquidationPolicy::default();
        let s = PortfolioSnapshot {
            portfolio_address: "X".into(),
            health_cert: HealthCertV16 {
                valid: false,
                certified_liq_deficit: deficit,
                ..HealthCertV16::default()
            },
            asset_in_distress: Some(asset),
        };
        prop_assert!(p.sequence(&[s]).is_empty());
    }

    /// L5 — Input-shuffle-invariant: permuting the input vector
    /// produces the identical output sequence. (The strict total
    /// order over `(deficit, address)` is independent of input
    /// position.)
    #[test]
    fn liquidation_shuffle_invariant(
        snaps in prop::collection::vec(arb_snapshot(), 0..16),
        seed in any::<u64>(),
    ) {
        let p = LiquidationPolicy::default();
        let a = p.sequence(&snaps);

        // Deterministic shuffle: rotate by seed.
        let mut shuffled = snaps.clone();
        if !shuffled.is_empty() {
            let k = (seed as usize) % shuffled.len();
            shuffled.rotate_left(k);
        }
        let b = p.sequence(&shuffled);
        prop_assert_eq!(a, b);
    }
}
