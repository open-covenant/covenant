//! HTTP gateway integration tests. Spawn the axum router on a random
//! ephemeral port, hit each endpoint with `reqwest`, assert the JSON
//! response shape end-to-end. Complements the unix-socket
//! `tests/end_to_end.rs`.

use covenant_audit::InMemoryAuditLog;
use covenant_identity::LocalIdentity;
use covenant_llm::MockEmbedder;
use covenant_manifest::Manifest;
use covenant_memory::InMemoryStore;
use covenant_permissions::InMemoryCapabilityStore;
use covenant_router::{AgentCard, Router};
use covenant_runtime::MockRunner;
use covenant_settlement::InMemorySettlement;
use covenantd::http::{router, HttpState};
use covenantd::Server;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;

fn stub_card() -> AgentCard {
    let toml = r#"
[agent]
id = "research"
name = "Research Agent"
version = "0.0.1"
runtime = "rust-bin"
entry = "./fake"

[capabilities]
required = ["tool.web_search"]
"#;
    let m = Manifest::parse(toml).unwrap();
    AgentCard::from_manifest_and_dir(m, PathBuf::from("/tmp/nope"))
}

async fn spawn_test_server() -> (String, tokio::task::JoinHandle<()>) {
    let server = Server::new(
        Arc::new(Router::from_cards(vec![stub_card()])),
        Arc::new(MockRunner::new("mocked summary")),
        Arc::new(InMemoryStore::new()),
        Arc::new(InMemorySettlement::new()),
        Arc::new(InMemoryAuditLog::new()),
        Arc::new(InMemoryCapabilityStore::new()),
        Arc::new(MockEmbedder::new(64)),
        Arc::new(LocalIdentity::generate("user@local")),
        Arc::new(covenant_memory::IgnoreSet::default()),
        Arc::new(covenant_mcp::ToolRegistry::from_tools(vec![Arc::new(
            covenant_mcp::native::EchoTool,
        )])),
        Arc::new(covenant_a2a::InMemoryMailbox::new()),
        Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
    );
    let app = router(HttpState { server });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let h = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), h)
}

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let (base, _h) = spawn_test_server().await;
    let r = reqwest::get(format!("{base}/health")).await.unwrap();
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn intent_rejects_when_capabilities_missing() {
    let (base, _h) = spawn_test_server().await;
    let client = reqwest::Client::new();
    let r: serde_json::Value = client
        .post(format!("{base}/intent"))
        .json(&json!({ "text": "find recent papers on agent memory" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["kind"], "error");
    assert!(r["message"]
        .as_str()
        .unwrap()
        .contains("missing capabilities"));
}

#[tokio::test]
async fn intent_round_trip_after_grant() {
    let (base, _h) = spawn_test_server().await;
    let client = reqwest::Client::new();

    // Grant the cap the matched agent requires.
    let g: serde_json::Value = client
        .post(format!("{base}/capabilities/grant"))
        .json(&json!({ "action": "tool.web_search" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(g["kind"], "capability_granted");
    let sig = g["signature_b58"].as_str().unwrap().to_string();

    // Submit intent — should pass now.
    let r: serde_json::Value = client
        .post(format!("{base}/intent"))
        .json(&json!({ "text": "find recent papers on agent memory" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["kind"], "intent_result");
    assert_eq!(r["text"], "mocked summary");

    // Memory tail should have one record.
    let m: serde_json::Value = client
        .get(format!("{base}/memory/recent?limit=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(m["records"].as_array().unwrap().len(), 1);

    // Receipts should have one record.
    let recv: serde_json::Value = client
        .get(format!("{base}/receipts/recent?limit=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(recv["receipts"].as_array().unwrap().len(), 1);

    // Capabilities recent should show the granted one.
    let caps: serde_json::Value = client
        .get(format!("{base}/capabilities/recent?limit=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(caps["capabilities"].as_array().unwrap().len(), 1);

    // Semantic search: with the deterministic-but-pseudo-random `MockEmbedder`,
    // cosine between two arbitrary strings is ~0. Querying the exact stored
    // text gives identical vectors → cosine 1.0 → guaranteed match.
    let s: serde_json::Value = client
        .get(format!("{base}/memory/search?q=mocked%20summary&limit=5"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(s["records"].as_array().unwrap().len(), 1);

    // Revoke the cap; subsequent dispatch is rejected.
    let rev: serde_json::Value = client
        .post(format!("{base}/capabilities/revoke"))
        .json(&json!({ "signature_b58": sig }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rev["kind"], "capability_revoked");
    assert_eq!(rev["removed"], true);

    let r2: serde_json::Value = client
        .post(format!("{base}/intent"))
        .json(&json!({ "text": "find recent papers on agent memory" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r2["kind"], "error");
}

#[tokio::test]
async fn tools_list_and_call_round_trip() {
    let (base, _h) = spawn_test_server().await;
    let client = reqwest::Client::new();

    let list: serde_json::Value = client
        .get(format!("{base}/tools"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["kind"], "tool_list");
    let names: Vec<&str> = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["echo"]);
    // Camel-case wire-format is preserved on `inputSchema`.
    assert!(list["tools"][0]["inputSchema"].is_object());

    // Without the cap, /tools/call is rejected.
    let denied: serde_json::Value = client
        .post(format!("{base}/tools/call"))
        .json(&json!({ "name": "echo", "arguments": { "text": "hi from http" } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(denied["kind"], "error");
    assert!(denied["message"]
        .as_str()
        .unwrap()
        .contains("tool.call.echo"));

    // Grant the cap, retry — succeeds.
    let _: serde_json::Value = client
        .post(format!("{base}/capabilities/grant"))
        .json(&json!({ "action": "tool.call.echo" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let call: serde_json::Value = client
        .post(format!("{base}/tools/call"))
        .json(&json!({ "name": "echo", "arguments": { "text": "hi from http" } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(call["kind"], "tool_result");
    assert_eq!(call["is_error"], false);
    assert_eq!(call["content"][0]["type"], "text");
    assert_eq!(call["content"][0]["text"], "hi from http");
}
