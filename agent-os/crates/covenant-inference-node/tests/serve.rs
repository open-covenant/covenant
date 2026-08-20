use axum::body::{Body, Bytes};
use axum::http::header::CONTENT_TYPE;
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use covenant_inference_node::serve;
use covenant_inference_protocol::{ModelIdentity, SamplingParams};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;

const CANNED_CHAT: &str = r#"{"id":"chatcmpl-mock","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"pong from mock"},"finish_reason":"stop"}]}"#;
const CANNED_EMBEDDING: &str =
    r#"{"object":"list","data":[{"object":"embedding","index":0,"embedding":[0.1,0.2,0.3]}]}"#;
const SSE_BODY: &str =
    "data: {\"choices\":[{\"delta\":{\"content\":\"pong\"}}]}\n\ndata: [DONE]\n\n";

fn sample_identity() -> ModelIdentity {
    ModelIdentity {
        weights_hash: "ab".repeat(32),
        quantization: "q4_k_m".to_owned(),
        runtime: "llama.cpp".to_owned(),
        runtime_version: "b9410 (031ddb2e0)".to_owned(),
        sampling_params: SamplingParams {
            temperature: 0.0,
            top_p: 1.0,
            seed: 0,
            max_tokens: 512,
        },
    }
}

async fn spawn(router: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{addr}")
}

async fn mock_chat(body: Bytes) -> Response {
    let streaming = std::str::from_utf8(&body)
        .map(|text| text.contains("\"stream\":true") || text.contains("\"stream\": true"))
        .unwrap_or(false);
    let (content_type, payload) = if streaming {
        ("text/event-stream", SSE_BODY)
    } else {
        ("application/json", CANNED_CHAT)
    };
    Response::builder()
        .header(CONTENT_TYPE, content_type)
        .body(Body::from(payload))
        .unwrap()
}

async fn mock_engine() -> String {
    let router = Router::new()
        .route("/v1/chat/completions", post(mock_chat))
        .route(
            "/v1/embeddings",
            post(|| async {
                Response::builder()
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(CANNED_EMBEDDING))
                    .unwrap()
            }),
        )
        .route("/health", get(|| async { "ok" }));
    spawn(router).await
}

async fn serve_over(engine_url: String) -> String {
    let router = serve::router(engine_url, "qwen3:8b".to_owned(), sample_identity());
    spawn(router).await
}

#[tokio::test]
async fn weights_hash_matches_an_independent_sha256() {
    let mut suffix = [0_u8; 8];
    getrandom::getrandom(&mut suffix).unwrap();
    let path = std::env::temp_dir().join(format!("covenant-serve-{}.bin", hex::encode(suffix)));
    let bytes = b"the quick brown fox jumps over the lazy dog";
    std::fs::write(&path, bytes).unwrap();

    let hashed = serve::hash_weights(&path).await.unwrap();
    let expected = hex::encode(Sha256::digest(bytes));
    assert_eq!(hashed, expected);

    // The hash is what the identity commits to, so it must be stable.
    let mut identity = sample_identity();
    identity.weights_hash = hashed;
    assert_eq!(identity.digest(), identity.digest());

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn chat_completion_round_trips_through_serve() {
    let engine = mock_engine().await;
    let base = serve_over(engine).await;

    let response = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "qwen3:8b",
            "messages": [{"role": "user", "content": "ping"}],
            "temperature": 0,
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        body["choices"][0]["message"]["content"], "pong from mock",
        "the mock engine's completion must survive the proxy hop"
    );
}

#[tokio::test]
async fn embeddings_round_trip_through_serve() {
    let engine = mock_engine().await;
    let base = serve_over(engine).await;

    let response = reqwest::Client::new()
        .post(format!("{base}/v1/embeddings"))
        .json(&serde_json::json!({"model": "qwen3:8b", "input": "ping"}))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["data"][0]["embedding"][1], 0.2);
}

#[tokio::test]
async fn streaming_completion_passes_through_as_sse() {
    let engine = mock_engine().await;
    let base = serve_over(engine).await;

    let response = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "qwen3:8b",
            "messages": [{"role": "user", "content": "ping"}],
            "stream": true,
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream"),
        "the SSE content-type must be preserved end to end"
    );
    let body = response.text().await.unwrap();
    assert!(body.contains("delta"));
    assert!(body.trim_end().ends_with("[DONE]"));
}

#[tokio::test]
async fn models_endpoint_carries_the_served_identity() {
    let base = serve_over(mock_engine().await).await;

    let body: serde_json::Value = reqwest::get(format!("{base}/v1/models"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let entry = &body["data"][0];
    assert_eq!(entry["id"], "qwen3:8b");
    assert_eq!(entry["model_identity_digest"], sample_identity().digest());
    assert_eq!(entry["model_identity"]["runtime"], "llama.cpp");
    assert_eq!(entry["model_identity"]["weights_hash"], "ab".repeat(32));
}

#[tokio::test]
async fn health_reports_ready_when_the_engine_answers() {
    let base = serve_over(mock_engine().await).await;

    let body: serde_json::Value = reqwest::get(format!("{base}/health"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["status"], "ok");
    assert_eq!(body["engine_ready"], true);
    assert_eq!(body["model_identity_digest"], sample_identity().digest());
}

#[tokio::test]
async fn health_reports_not_ready_when_the_engine_is_down() {
    // Port 1 refuses immediately, so the probe fails without waiting out its timeout.
    let base = serve_over("http://127.0.0.1:1".to_owned()).await;

    let body: serde_json::Value = reqwest::get(format!("{base}/health"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["status"], "ok");
    assert_eq!(body["engine_ready"], false);
}
