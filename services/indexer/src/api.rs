use std::sync::Arc;

use axum::{extract::State, routing::get, Json, Router};
use serde::Serialize;
use tower_http::cors::CorsLayer;

use crate::model::{IndexerSnapshot, SolanaEventRecord};
use crate::x402_gate::X402Gate;

pub const FIXTURE_MODE: &str = "fixture";

#[derive(Clone)]
pub struct AppState {
    pub cluster: String,
    pub rpc_url: String,
    pub confirmations: u64,
    pub events: Arc<Vec<SolanaEventRecord>>,
    pub x402: Option<X402Gate>,
}

pub fn router(state: AppState) -> Router {
    let mut router = Router::new()
        .route("/healthz", get(healthz))
        .route("/stats/summary", get(summary))
        .route("/events", get(events));

    if state.x402.is_some() {
        router = router.route(
            "/x402/stats/summary",
            get(crate::x402_gate::paid_summary),
        );
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

#[cfg(test)]
mod tests {
    use super::{router, AppState};
    use crate::model::seed_events;
    use crate::x402_gate::{X402Config, X402Gate, PAYAI_FEE_PAYER, SOLANA_NETWORK, USDC_MINT};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn valid_addr() -> String {
        "1".repeat(44)
    }

    fn state_with(events: Vec<crate::model::SolanaEventRecord>, x402: Option<X402Gate>) -> AppState {
        AppState {
            cluster: "devnet".into(),
            rpc_url: "https://rpc.example".into(),
            confirmations: 32,
            events: Arc::new(events),
            x402,
        }
    }

    async fn json_body(resp: axum::response::Response) -> serde_json::Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn healthz_reports_latest_slot_count_mode_and_x402_flag() {
        // seed_events yields slots 12_345_678 and 12_345_700; latest is the max.
        let events = seed_events("devnet", &valid_addr());
        let app = router(state_with(events, None));
        let resp = app
            .oneshot(Request::builder().method("GET").uri("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = json_body(resp).await;
        assert_eq!(v["ok"], true);
        assert_eq!(v["chain"], "solana");
        assert_eq!(v["cluster"], "devnet");
        assert_eq!(v["confirmations"], 32);
        assert_eq!(v["latest_slot"], 12_345_700);
        assert_eq!(v["indexed_events"], 2);
        assert_eq!(v["mode"], "fixture");
        assert_eq!(v["x402"], false);
    }

    #[tokio::test]
    async fn healthz_latest_slot_falls_back_to_zero_with_no_events() {
        let app = router(state_with(vec![], None));
        let resp = app
            .oneshot(Request::builder().method("GET").uri("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let v = json_body(resp).await;
        assert_eq!(v["latest_slot"], 0);
        assert_eq!(v["indexed_events"], 0);
    }

    #[tokio::test]
    async fn healthz_x402_flag_is_true_when_gate_is_configured() {
        let gate = X402Gate::new(X402Config {
            pay_to: "9covntsTreasuryAddressForTestingPurposesOnly".into(),
            fee_payer: PAYAI_FEE_PAYER.into(),
            facilitator: "http://facilitator.example".into(),
            network: SOLANA_NETWORK.into(),
            asset: USDC_MINT.into(),
            price_atomic: "50000".into(),
            resource_base_url: "http://indexer.example".into(),
            description: "test".into(),
        });
        let app = router(state_with(vec![], Some(gate)));
        let resp = app
            .oneshot(Request::builder().method("GET").uri("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let v = json_body(resp).await;
        assert_eq!(v["x402"], true);
    }

    #[tokio::test]
    async fn summary_returns_snapshot_shape() {
        let events = seed_events("devnet", &valid_addr());
        let app = router(state_with(events, None));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/stats/summary")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = json_body(resp).await;
        assert_eq!(v["chain"], "solana");
        assert_eq!(v["cluster"], "devnet");
        assert_eq!(v["latest_slot"], 12_345_700);
        assert_eq!(v["indexed_events"], 2);
        assert_eq!(v["mode"], "fixture");
    }

    #[tokio::test]
    async fn events_endpoint_returns_stored_records() {
        let events = seed_events("devnet", &valid_addr());
        let app = router(state_with(events, None));
        let resp = app
            .oneshot(Request::builder().method("GET").uri("/events").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = json_body(resp).await;
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["event_name"], "AgentRegistered");
        assert_eq!(arr[0]["chain"], "solana");
    }
}
