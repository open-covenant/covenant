//! Full relay + HTTP-over-tunnel proxy path over an in-process duplex pipe, no TLS.
//!
//! The node side runs the real `covenant_inference_node` relay against a throwaway
//! OpenAI-shaped server; the gateway side runs the real registration read and the
//! real HTTP/1-over-stream proxy. This is the fallback coverage for the tunnel path
//! when standing up mTLS is impractical — the same code the mTLS listener drives,
//! minus the TLS wrapper.

use std::net::SocketAddr;

use axum::routing::{get, post};
use axum::{Json, Router};
use covenant_inference_gateway::proxy;
use covenant_inference_gateway::tunnel::register;
use covenant_inference_node::target::InferenceTarget;
use covenant_inference_node::tunnel::{serve_connection, signed_registration};
use ed25519_dalek::SigningKey;
use serde_json::{json, Value};
use tokio::net::TcpStream;

struct EngineTarget {
    addr: SocketAddr,
}

impl InferenceTarget for EngineTarget {
    type Stream = TcpStream;

    async fn connect(&self) -> anyhow::Result<TcpStream> {
        Ok(TcpStream::connect(self.addr).await?)
    }

    async fn health(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

async fn spawn_engine() -> SocketAddr {
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(|| async {
                Json(json!({
                    "choices": [{ "message": { "role": "assistant", "content": "hi there friend" } }]
                }))
            }),
        )
        .route(
            "/v1/models",
            get(|| async {
                Json(json!({
                    "object": "list",
                    "data": [{ "id": "test-model", "object": "model", "model_identity_digest": "deadbeef" }]
                }))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

/// Wires one gateway↔node relay over a duplex pipe: the node registers and serves,
/// the gateway reads the registration and hands back the stream ready to proxy over.
async fn relay(engine: SocketAddr) -> covenant_inference_gateway::pool::PooledStream {
    let key = SigningKey::from_bytes(&[7_u8; 32]);
    let registration = signed_registration("loopback", &key).unwrap();
    let (mut gateway, mut node) = tokio::io::duplex(256 * 1024);

    tokio::spawn(async move {
        let target = EngineTarget { addr: engine };
        let _ = serve_connection(&mut node, &registration, &target).await;
    });

    register(&mut gateway).await.unwrap();
    Box::new(gateway)
}

#[tokio::test]
async fn proxies_a_chat_completion_down_the_relay() {
    let engine = spawn_engine().await;
    let stream = relay(engine).await;

    let mut headers = axum::http::HeaderMap::new();
    headers.insert("content-type", "application/json".parse().unwrap());
    let body = axum::body::Bytes::from_static(br#"{"model":"test-model","messages":[]}"#);

    let response = proxy::relay_request(
        stream,
        true,
        axum::http::Method::POST,
        "/v1/chat/completions",
        &headers,
        body,
    )
    .await;
    assert!(response.status().is_success());

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["choices"][0]["message"]["content"], "hi there friend");
}

#[tokio::test]
async fn probes_the_node_model_catalog_down_the_relay() {
    let engine = spawn_engine().await;
    let stream = relay(engine).await;

    let catalog = proxy::fetch_catalog(stream, true).await.unwrap();
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0].id, "test-model");
    assert_eq!(catalog[0].digest, "deadbeef");
}
