//! Aggregate recent PayAI settlements per seller — pulls real numbers for the
//! demo. `RPC=<url> LIMIT=<n> cargo run -p covenant-payai --example scan`

use std::collections::{HashMap, HashSet};

use covenant_payai::{compute_reputation, SettlementIndexer, PAYAI_SOLANA_FEE_PAYER};

#[tokio::main]
async fn main() {
    let rpc = std::env::var("RPC").unwrap_or_else(|_| "https://solana-rpc.publicnode.com".into());
    let limit: usize = std::env::var("LIMIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);

    let ix = SettlementIndexer::new(&rpc);
    let settlements = ix.recent_settlements(limit).await.expect("fetch");
    eprintln!(
        "scanned ~{limit} signatures, {} settlements",
        settlements.len()
    );

    let mut agg: HashMap<String, (u64, HashSet<String>, u128)> = HashMap::new();
    for s in &settlements {
        let e = agg.entry(s.pay_to.clone()).or_default();
        e.0 += 1;
        e.1.insert(s.payer.clone());
        e.2 += s.amount_micro;
    }

    // The hub = most settled jobs. The strongest reputation story.
    let hub = agg
        .iter()
        .max_by_key(|(_, (jobs, _, _))| *jobs)
        .map(|(w, _)| w.clone());
    if let Some(hub) = hub {
        let rep = compute_reputation(&settlements, &hub, PAYAI_SOLANA_FEE_PAYER);
        println!(
            "HUB wallet={} jobs={} counterparties={} vol_micro={} vol_usdc={:.4} score={} tier={}",
            rep.wallet,
            rep.settled_jobs,
            rep.distinct_counterparties,
            rep.volume_micro_usdc,
            rep.volume_micro_usdc as f64 / 1e6,
            rep.score,
            rep.tier.label()
        );
        if let Some(s) = settlements.iter().find(|s| s.pay_to == hub) {
            println!(
                "HUB_TX sig={} payer={} amount_micro={}",
                s.signature, s.payer, s.amount_micro
            );
        }
    }

    let mut by_vol: Vec<_> = agg.iter().collect();
    by_vol.sort_by(|a, b| b.1 .2.cmp(&a.1 .2));
    println!("TOP BY VOLUME:");
    for (w, (jobs, cps, vol)) in by_vol.iter().take(5) {
        println!(
            "  {w} jobs={jobs} cps={} vol_usdc={:.2}",
            cps.len(),
            *vol as f64 / 1e6
        );
    }
}
