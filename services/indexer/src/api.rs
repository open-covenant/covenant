use std::sync::Arc;

use axum::{extract::State, routing::get, Json, Router};
use serde::Serialize;
use tower_http::cors::CorsLayer;

use crate::model::{IndexerSnapshot, SolanaEventRecord};
use crate::verified::VerifiedState;
use crate::x402_gate::X402Gate;

pub const FIXTURE_MODE: &str = "fixture";

#[derive(Clone)]
pub struct AppState {
    pub cluster: String,
    pub rpc_url: String,
    pub confirmations: u64,
    pub events: Arc<Vec<SolanaEventRecord>>,
    pub x402: Option<X402Gate>,
    pub verified: Option<VerifiedState>,
}

pub fn router(state: AppState) -> Router {
    let mut router = Router::new()
        .route("/healthz", get(healthz))
        .route("/stats/summary", get(summary))
        .route("/events", get(events));

    if state.x402.is_some() {
        router = router.route("/x402/stats/summary", get(crate::x402_gate::paid_summary));
    }

    if state.verified.is_some() {
        router = router.route("/verified", get(crate::verified::serve_verified));
    }

    router.layer(CorsLayer::permissive()).with_state(state)
}

#[derive(Serialize)]
struct HealthzResponse {
    ok: bool,
    chain: &'static str,
    cluster: String,
    rpc_url: String,
    confirmations: u64,
    latest_slot: u64,
    indexed_events: usize,
    mode: &'static str,
    x402: bool,
}

async fn healthz(State(state): State<AppState>) -> Json<HealthzResponse> {
    let latest_slot = state
        .events
        .iter()
        .map(|event| event.slot)
        .max()
        .unwrap_or(0);

    Json(HealthzResponse {
        ok: true,
        chain: "solana",
        cluster: state.cluster.clone(),
        rpc_url: state.rpc_url.clone(),
        confirmations: state.confirmations,
        latest_slot,
        indexed_events: state.events.len(),
        mode: FIXTURE_MODE,
        x402: state.x402.is_some(),
    })
}

async fn summary(State(state): State<AppState>) -> Json<IndexerSnapshot> {
    Json(summary_snapshot(&state))
}

pub(crate) fn summary_snapshot(state: &AppState) -> IndexerSnapshot {
    let latest_slot = state
        .events
        .iter()
        .map(|event| event.slot)
        .max()
        .unwrap_or(0);
    IndexerSnapshot {
        chain: "solana".to_string(),
        cluster: state.cluster.clone(),
        latest_slot,
        indexed_events: state.events.len(),
        mode: FIXTURE_MODE.to_string(),
    }
}

async fn events(State(state): State<AppState>) -> Json<Vec<SolanaEventRecord>> {
    Json(state.events.as_ref().clone())
}
