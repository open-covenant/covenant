//! Explicit read-only acceptance harness. Its local books are EMPTY: real
//! chain leases are expected to appear as missing local records. It starts no
//! server, settlement worker, sidecar, scheduler or signing operation.
use covenant_audit::InMemoryAuditLog;
use covenant_compute_coordinator::{
    observer::{ChainObserver, ObserverConfig, ObserverStatus},
    CoordinatorConfig, CoordinatorState, MockPayout, NoReputation,
};
use covenant_identity::LocalIdentity;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    anyhow::ensure!(std::env::args().nth(1).as_deref() == Some("--empty-journal"),
        "pass --empty-journal to acknowledge orphan findings are expected in this read-only harness");
    let config = ObserverConfig::from_env()
        .map_err(anyhow::Error::msg)?
        .ok_or_else(|| anyhow::anyhow!("observer is disabled"))?;
    let observer = Arc::new(ChainObserver::new(config).map_err(anyhow::Error::msg)?);
    let state = CoordinatorState::new(
        LocalIdentity::generate("observer-acceptance@local"),
        CoordinatorConfig::default(),
        Arc::new(NoReputation),
        Arc::new(MockPayout::new()),
        Arc::new(InMemoryAuditLog::new()),
    );
    let now = now_ms();
    let report = observer.observe(&state, now).await;
    let (_, axum::Json(public_status)) = covenant_compute_coordinator::observer::public_status(
        axum::extract::State(state),
        Some(axum::Extension(observer.clone())),
    )
    .await;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "mode":"read_only_empty_journal", "public_observation":observer.public_report(now_ms()), "public_status":public_status,
            "explanation":"Real chain leases will appear as orphaned because this harness deliberately has no production journal. No transactions are sent."
        }))?
    );
    anyhow::ensure!(
        report.status() != ObserverStatus::Blind,
        "chain observation is incomplete"
    );
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
}
