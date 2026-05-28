//! `covenant-percolator-keeper` — the operator-runnable binary.
//!
//! Reads a TOML config, builds a `KeeperAgent` wired to in-memory
//! accounting backends, and runs the tick loop on a schedule. The
//! market backend is the `MockPercolator` seeded from the config —
//! the v1 binary lets an operator exercise the full governance path
//! (scope → coordination → budget → execute → record) end-to-end
//! against a synthetic market. The on-chain `RealPercolator` swaps
//! in cleanly when the `solana` feature lands.
//!
//! Usage:
//!     covenant-percolator-keeper --config <path>
//!     covenant-percolator-keeper --config <path> --once

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use covenant_budget::{BudgetLedger, InMemoryLedger};
use covenant_percolator::capability::KeeperScope;
use covenant_percolator::client::MockPercolator;
use covenant_percolator::coordination::KeeperId;
use covenant_percolator::keeper::KeeperAgent;
use covenant_percolator::policy::{KeeperPolicy, RecoveryPolicy};
use covenant_percolator::state::{
    ActionLabel, AssetIndex, AssetLifecycle, AssetState, MarketState,
};
use covenant_settlement::{InMemorySettlement, Settlement};
use covenant_types::AgentId;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Config {
    runtime: RuntimeCfg,
    scope: ScopeCfg,
    policy: PolicyCfg,
    #[serde(default)]
    recovery: Option<RecoveryCfg>,
    budget: BudgetCfg,
    #[serde(default)]
    network: NetworkCfg,
    mock_market: MockMarketCfg,
}

#[derive(Debug, Deserialize)]
struct RuntimeCfg {
    agent_display: String,
    agent_pubkey_hex: String, // 32 bytes, 64 hex chars
    tick_interval_ms: u64,
}

#[derive(Debug, Deserialize)]
struct ScopeCfg {
    market: String,
    #[serde(default)]
    allowed_assets: Option<Vec<AssetIndex>>,
    #[serde(default)]
    allowed_actions: Option<Vec<ActionLabel>>,
    #[serde(default)]
    max_actions_per_tick: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct PolicyCfg {
    max_staleness_slots: u64,
    crank_interval_slots: u64,
}

#[derive(Debug, Deserialize)]
struct RecoveryCfg {
    min_deficit: u128,
}

#[derive(Debug, Deserialize)]
struct BudgetCfg {
    capacity: u64,
    credits_per_action: u64,
}

#[derive(Debug, Default, Deserialize)]
struct NetworkCfg {
    #[serde(default)]
    peers_hex: Vec<String>,
    #[serde(default)]
    coordination_window_slots: u64,
}

#[derive(Debug, Deserialize)]
struct MockMarketCfg {
    program_id: String,
    current_slot: u64,
    last_crank_slot: u64,
    assets: Vec<MockAssetCfg>,
}

#[derive(Debug, Deserialize)]
struct MockAssetCfg {
    index: AssetIndex,
    label: String,
    lifecycle: AssetLifecycle,
    last_mark_slot: u64,
    last_mark_e6: i64,
}

fn decode_32(hex: &str) -> Result<[u8; 32], String> {
    if hex.len() != 64 {
        return Err(format!("expected 64 hex chars, got {}", hex.len()));
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)
            .map_err(|e| format!("invalid hex at {i}: {e}"))?;
    }
    Ok(out)
}

fn build_market(cfg: &MockMarketCfg, market: &str) -> MarketState {
    MarketState {
        market_address: market.to_string(),
        program_id: cfg.program_id.clone(),
        current_slot: cfg.current_slot,
        last_crank_slot: cfg.last_crank_slot,
        assets: cfg
            .assets
            .iter()
            .map(|a| AssetState {
                index: a.index,
                label: a.label.clone(),
                lifecycle: a.lifecycle,
                last_mark_slot: a.last_mark_slot,
                last_mark_e6: a.last_mark_e6,
            })
            .collect(),
    }
}

struct Args {
    config: String,
    once: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut config: Option<String> = None;
    let mut once = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" => config = args.next(),
            "--once" => once = true,
            "-h" | "--help" => {
                eprintln!("Usage: covenant-percolator-keeper --config <path> [--once]");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Args {
        config: config.ok_or("--config <path> is required")?.to_string(),
        once,
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    tracing_subscriber::fmt::init();
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let body = match std::fs::read_to_string(&args.config) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("read {}: {e}", args.config);
            return ExitCode::from(1);
        }
    };
    let cfg: Config = match toml::from_str(&body) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("parse config: {e}");
            return ExitCode::from(1);
        }
    };

    let agent_pk = match decode_32(&cfg.runtime.agent_pubkey_hex) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("agent_pubkey_hex: {e}");
            return ExitCode::from(1);
        }
    };
    let payer = AgentId::new(cfg.runtime.agent_display.as_str(), agent_pk);
    let mut peers: Vec<KeeperId> = Vec::with_capacity(cfg.network.peers_hex.len());
    for h in &cfg.network.peers_hex {
        match decode_32(h) {
            Ok(p) => peers.push(p),
            Err(e) => {
                eprintln!("peer {h}: {e}");
                return ExitCode::from(1);
            }
        }
    }

    let scope = KeeperScope {
        version: 1,
        market: cfg.scope.market.clone(),
        allowed_assets: cfg.scope.allowed_assets.clone(),
        allowed_actions: cfg.scope.allowed_actions.clone(),
        max_actions_per_tick: cfg.scope.max_actions_per_tick,
    };
    let policy = KeeperPolicy {
        max_staleness_slots: cfg.policy.max_staleness_slots,
        crank_interval_slots: cfg.policy.crank_interval_slots,
    };
    let recovery_policy = cfg.recovery.as_ref().map(|r| RecoveryPolicy {
        min_deficit: r.min_deficit,
    });

    let market = build_market(&cfg.mock_market, &cfg.scope.market);
    let client = Arc::new(MockPercolator::new(market));
    let settlement = Arc::new(InMemorySettlement::new());
    let budget = Arc::new(InMemoryLedger::new());
    if let Err(e) = budget.set_capacity(&payer, cfg.budget.capacity).await {
        eprintln!("set_capacity: {e}");
        return ExitCode::from(1);
    }

    let agent = KeeperAgent {
        client: client.clone(),
        settlement: settlement.clone(),
        budget: budget.clone(),
        payer: payer.clone(),
        market_address: cfg.scope.market.clone(),
        scope,
        policy,
        peers,
        coordination_window_slots: cfg.network.coordination_window_slots,
        recovery_policy,
        credits_per_action: cfg.budget.credits_per_action,
    };

    tracing::info!(
        agent = %payer.display,
        market = %cfg.scope.market,
        peers = agent.peers.len(),
        "keeper ready"
    );

    let interval = Duration::from_millis(cfg.runtime.tick_interval_ms.max(50));
    let mut tick = 0u64;
    loop {
        tick += 1;
        match agent.tick().await {
            Ok(report) => {
                let remaining = budget.tokens_remaining(&payer).await.unwrap_or(0);
                let receipts = settlement
                    .recent(1024)
                    .await
                    .map(|r| r.len())
                    .unwrap_or(0);
                tracing::info!(
                    tick,
                    decided = report.decided,
                    executed = report.executed,
                    deferred = report.coordination_deferred,
                    skipped_capability = report.skipped_capability,
                    stopped_budget = report.stopped_budget,
                    errors = report.errors.len(),
                    budget_remaining = remaining,
                    total_receipts = receipts,
                    "tick"
                );
            }
            Err(e) => {
                tracing::error!(tick, error = %e, "tick failed");
            }
        }
        if args.once {
            break;
        }
        tokio::time::sleep(interval).await;
    }
    ExitCode::SUCCESS
}
