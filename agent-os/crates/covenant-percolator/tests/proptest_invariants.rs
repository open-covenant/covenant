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
const PROGRAM: &str = "2SSnp35m7FQ7cRLNKGdW5UzjYFF6RBUNq7d3m5mqNByp";

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
            if let KeeperAction::PushHyperpMark { asset_index, .. } = action {
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
        KeeperAction::PushHyperpMark { asset_index: 7, .. }
    ));
    // Other 15 push_marks + the crank get refused at the scope gate.
    assert!(outcome.report.skipped_capability >= 15);
}
