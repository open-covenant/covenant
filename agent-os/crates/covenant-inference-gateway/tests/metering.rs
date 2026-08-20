//! The metering surface over the direct transport: an unpaid request is challenged
//! with a well-formed `402`, a paid request is served and receipted, and the receipt
//! the caller gets back is the one the gateway stores — hashes only.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::{to_bytes, Body, Bytes};
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD};
use base64::Engine as _;
use covenant_inference_gateway::registry::NodeRegistry;
use covenant_inference_gateway::server::{router, Gateway, Metering, Transport};
use covenant_inference_gateway::x402::{DevPrefundedVerifier, PaymentVerifier};
use covenant_inference_protocol::{
    node_id, receipt_hash, InferenceReceiptPayload, ModelIdentity, NodeEnrollment, NodeTelemetry,
    SamplingParams, UnsignedNodeEnrollment, UnsignedTelemetry,
};
use ed25519_dalek::SigningKey;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const DEV_TOKEN: &str = "dev-prefunded-token";
const NETWORK: &str = "solana:devnet";
const ASSET: &str = "UsdcDevnetMint";
const PAY_TO: &str = "PayToWallet";

fn model() -> ModelIdentity {
    ModelIdentity {
        weights_hash: "sha256:feed".into(),
        quantization: "q4_k_m".into(),
        runtime: "llama.cpp".into(),
        runtime_version: "test".into(),
        sampling_params: SamplingParams {
            temperature: 0.0,
            top_p: 1.0,
            seed: 0,
            max_tokens: 512,
        },
    }
}

async fn spawn_stub() -> SocketAddr {
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(|| async {
                Json(json!({
                    "id": "chatcmpl-test",
                    "choices": [{ "index": 0, "message": { "role": "assistant", "content": "hi there" } }]
                }))
            }),
        )
        .route(
            "/v1/models",
            get(|| async {
                Json(json!({ "object": "list", "data": [] }))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

async fn online_gateway_router(verifier: Arc<dyn PaymentVerifier>) -> (Router, String) {
    let upstream = spawn_stub().await;
    let registry = Arc::new(NodeRegistry::default());
    let gateway = Gateway::new(registry, Transport::Direct(Arc::from(upstream.to_string())))
        .with_metering(Metering {
            require_payment: true,
            price_micro: 1_000,
            pay_to: PAY_TO.into(),
            rail_network: NETWORK.into(),
            rail_asset: ASSET.into(),
        })
        .with_verifier(verifier);
    let app = router(gateway);

    let key = SigningKey::from_bytes(&[3_u8; 32]);
    let id = node_id(&key.verifying_key());
    let device_public_key = URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes());

    let enrollment = NodeEnrollment::sign(
        UnsignedNodeEnrollment {
            node_id: id.clone(),
            device_public_key: device_public_key.clone(),
            operator_wallet: "operator".into(),
            payout_wallet: "payout".into(),
            served_models: vec![model()],
            max_tokens_per_second: 200,
            rate_per_second: 1_000,
            issued_at: chrono::Utc::now(),
        },
        &key,
    )
    .unwrap();
    let status = post_json(
        &app,
        "/v1/nodes/enroll",
        serde_json::to_vec(&enrollment).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let telemetry = NodeTelemetry::sign(
        UnsignedTelemetry {
            node_id: id.clone(),
            sequence: 1,
            observed_at: chrono::Utc::now(),
            active_sessions: 0,
            tokens_per_second: 10,
            tunnel_connected: true,
            served_model_digest: Some(model().digest()),
            posture: None,
        },
        &key,
    )
    .unwrap();
    let status = post_json(
        &app,
        &format!("/v1/nodes/{id}/heartbeat"),
        serde_json::to_vec(&telemetry).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    (app, model().digest())
}

async fn post_json(app: &Router, uri: &str, body: Vec<u8>) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

fn payment_header(reference: &str) -> String {
    BASE64.encode(
        serde_json::to_vec(&json!({
            "x402Version": 1,
            "scheme": "exact",
            "network": NETWORK,
            "payload": {
                "reference": reference,
                "credential": DEV_TOKEN,
                "amount": "1000",
                "payTo": PAY_TO,
                "asset": ASSET
            }
        }))
        .unwrap(),
    )
}

fn chat_body(digest: &str) -> Bytes {
    Bytes::from(
        serde_json::to_vec(&json!({
            "model": digest,
            "messages": [{ "role": "user", "content": "say hi" }]
        }))
        .unwrap(),
    )
}

#[tokio::test]
async fn unpaid_request_is_challenged_with_a_well_formed_402() {
    let verifier = Arc::new(DevPrefundedVerifier::new(DEV_TOKEN));
    let (app, digest) = online_gateway_router(verifier).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(chat_body(&digest)))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["x402Version"], 1);
    assert_eq!(body["accepts"][0]["scheme"], "exact");
    assert_eq!(body["accepts"][0]["network"], NETWORK);
    assert_eq!(body["accepts"][0]["maxAmountRequired"], "1000");
    assert_eq!(body["accepts"][0]["resource"], "/v1/chat/completions");
    assert_eq!(body["accepts"][0]["payTo"], PAY_TO);
    assert_eq!(body["accepts"][0]["asset"], ASSET);
}

#[tokio::test]
async fn paid_request_is_served_and_receipted() {
    let verifier = Arc::new(DevPrefundedVerifier::new(DEV_TOKEN));
    let (app, digest) = online_gateway_router(verifier).await;
    let node = node_id(&SigningKey::from_bytes(&[3_u8; 32]).verifying_key());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-payment", payment_header("ref-1"))
                .body(Body::from(chat_body(&digest)))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let header_hash = response
        .headers()
        .get("x-covenant-receipt")
        .expect("receipt header present")
        .to_str()
        .unwrap()
        .to_owned();
    let served = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert!(
        serde_json::from_slice::<Value>(&served).unwrap()["choices"][0]["message"]["content"]
            .as_str()
            .unwrap()
            .contains("hi")
    );

    let listed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/receipts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let page: Value =
        serde_json::from_slice(&to_bytes(listed.into_body(), usize::MAX).await.unwrap()).unwrap();
    let record = &page["data"][0];
    assert_eq!(record["receipt_hash"], header_hash);
    assert_eq!(record["node_id"], node);
    assert_eq!(record["model_identity_digest"], digest);
    assert_eq!(record["amount_micro"], 1_000);
    assert_eq!(record["payment_reference"], "ref-1");

    // The stored receipt hash is exactly what an independently-built payload hashes to.
    let rebuilt = InferenceReceiptPayload {
        model_identity_digest: digest.clone(),
        prompt_hash: record["prompt_hash"].as_str().unwrap().to_owned(),
        output_hash: record["output_hash"].as_str().unwrap().to_owned(),
        node_id: node,
        timestamp: record["timestamp"].as_i64().unwrap(),
    };
    assert_eq!(header_hash, receipt_hash(&rebuilt).unwrap());

    // Output commits to the exact response bytes; prompt commits to the messages only.
    assert_eq!(record["output_hash"], hex::encode(Sha256::digest(&served)));
    let messages = json!([{ "role": "user", "content": "say hi" }]);
    assert_eq!(
        record["prompt_hash"],
        hex::encode(Sha256::digest(serde_json::to_vec(&messages).unwrap()))
    );

    // No plaintext prompt or output survives anywhere in the receipts surface.
    let page_text = serde_json::to_string(&page).unwrap();
    assert!(!page_text.contains("say hi"));
    assert!(!page_text.contains("hi there"));

    // Fetch by hash resolves the same record; a replayed payment is refused.
    let by_hash = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/receipts/{header_hash}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(by_hash.status(), StatusCode::OK);

    let replay = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-payment", payment_header("ref-1"))
                .body(Body::from(chat_body(&digest)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::PAYMENT_REQUIRED);
}
