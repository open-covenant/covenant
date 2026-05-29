//! HTTP gateway integration tests. Spawn the axum router on a random
//! ephemeral port, hit each endpoint with `reqwest`, assert the JSON
//! response shape end-to-end. Complements the unix-socket
//! `tests/end_to_end.rs`.

use covenant_audit::{AuditLog, InMemoryAuditLog};
use covenant_identity::LocalIdentity;
use covenant_llm::MockEmbedder;
use covenant_manifest::Manifest;
use covenant_memory::{InMemoryStore, MemoryStore};
use covenant_permissions::InMemoryCapabilityStore;
use covenant_router::{AgentCard, Router};
use covenant_runtime::MockRunner;
use covenant_settlement::InMemorySettlement;
use covenantd::http::{router, router_with_origins, HttpState};
use covenantd::Server;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;

fn protocol_info_fixture_v1() -> serde_json::Value {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture_path = root
        .join("..")
        .join("covenant-ipc")
        .join("tests")
        .join("fixtures")
        .join("protocol-info.v1.json");
    let text = std::fs::read_to_string(&fixture_path).expect("read protocol info fixture");
    serde_json::from_str(&text).expect("parse protocol info fixture")
}

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

struct TestServer {
    base: String,
    token: String,
    agent_id: covenant_types::AgentId,
    audit: Arc<InMemoryAuditLog>,
    memory: Arc<InMemoryStore>,
    _handle: tokio::task::JoinHandle<()>,
}

async fn spawn_test_server() -> TestServer {
    let identity = Arc::new(LocalIdentity::generate("user@local"));
    let peers: Arc<dyn covenant_peer_auth::PeerRegistry> =
        Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new());
    let token = covenant_peer_auth::PeerToken::generate();
    let token_b58 = token.to_b58();
    peers
        .register(covenant_peer_auth::PeerEntry {
            token,
            agent_id: covenant_types::AgentId::new(identity.display(), identity.pubkey_bytes()),
            registered_at: 0,
        })
        .await
        .unwrap();
    let agent_id = covenant_types::AgentId::new(identity.display(), identity.pubkey_bytes());
    let audit = Arc::new(InMemoryAuditLog::new());
    let memory = Arc::new(InMemoryStore::new());
    let server = Server::new(
        Arc::new(Router::from_cards(vec![stub_card()])),
        Arc::new(MockRunner::new("mocked summary")),
        memory.clone(),
        Arc::new(InMemorySettlement::new()),
        audit.clone(),
        Arc::new(InMemoryCapabilityStore::new()),
        Arc::new(MockEmbedder::new(64)),
        identity,
        Arc::new(covenant_memory::IgnoreSet::default()),
        Arc::new(covenant_mcp::ToolRegistry::from_tools(vec![Arc::new(
            covenant_mcp::native::EchoTool,
        )])),
        Arc::new(covenant_a2a::InMemoryMailbox::new()),
        peers,
        Arc::new(covenant_budget::InMemoryLedger::new()),
    );
    let (live_traces_tx, _) = tokio::sync::broadcast::channel(16);
    let app = router(HttpState {
        server,
        live_traces_tx,
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let _handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    TestServer {
        base: format!("http://{addr}"),
        token: token_b58,
        agent_id,
        audit,
        memory,
        _handle,
    }
}

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let s = spawn_test_server().await;
    // /health is the one route that does not require Authorization.
    let r = reqwest::get(format!("{}/health", s.base)).await.unwrap();
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn version_endpoint_returns_protocol_info_without_bearer() {
    let s = spawn_test_server().await;
    let r = reqwest::get(format!("{}/version", s.base)).await.unwrap();
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body, protocol_info_fixture_v1());
}

#[tokio::test]
async fn protected_route_rejects_without_bearer() {
    let s = spawn_test_server().await;
    let r = reqwest::get(format!("{}/tools", s.base)).await.unwrap();
    assert_eq!(r.status(), 401);
}

#[tokio::test]
async fn protected_route_rejects_with_unknown_token() {
    let s = spawn_test_server().await;
    let stray = covenant_peer_auth::PeerToken::generate().to_b58();
    let r = reqwest::Client::new()
        .get(format!("{}/tools", s.base))
        .bearer_auth(&stray)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
}

#[tokio::test]
async fn auth_failure_lands_in_audit_log() {
    let s = spawn_test_server().await;
    // Three different failure modes — three audit rows.
    let _ = reqwest::get(format!("{}/tools", s.base)).await.unwrap();
    let _ = reqwest::Client::new()
        .get(format!("{}/tools", s.base))
        .bearer_auth(covenant_peer_auth::PeerToken::generate().to_b58())
        .send()
        .await
        .unwrap();
    let _ = reqwest::Client::new()
        .get(format!("{}/tools", s.base))
        .header(reqwest::header::AUTHORIZATION, "Bearer not-base58!")
        .send()
        .await
        .unwrap();
    let events = s.audit.recent(20).await.unwrap();
    let failures: Vec<String> = events
        .iter()
        .filter_map(|e| match &e.kind {
            covenant_audit::AuditKind::AuthenticationFailed { transport, reason } => {
                assert_eq!(transport, "http");
                Some(reason.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        failures.len(),
        3,
        "expected three auth failures: {failures:?}"
    );
    assert!(failures.iter().any(|r| r.contains("missing")));
    assert!(failures.iter().any(|r| r.contains("unknown")));
    assert!(failures.iter().any(|r| r.contains("malformed")));
}

#[tokio::test]
async fn audit_verify_round_trip_after_authenticated_event() {
    let TestServer { base, token, .. } = spawn_test_server().await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();

    let grant: serde_json::Value = client
        .post(format!("{base}/capabilities/grant"))
        .json(&json!({ "action": "tool.call.echo" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(grant["kind"], "capability_granted");

    let report: serde_json::Value = client
        .get(format!("{base}/audit/verify"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(report["kind"], "audit_integrity");
    assert_eq!(report["report"]["valid"], true);
    assert_eq!(report["report"]["events"], report["report"]["anchors"]);
}

#[tokio::test]
async fn intent_rejects_when_capabilities_missing() {
    let TestServer { base, token, .. } = spawn_test_server().await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();
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
    assert!(r["message"].as_str().unwrap().contains("memory.write"));
}

#[tokio::test]
async fn intent_round_trip_after_grant() {
    let TestServer { base, token, .. } = spawn_test_server().await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();

    // Grant the caps the matched agent and memory surfaces require.
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
    for action in ["memory.write", "memory.read", "chain.receipts"] {
        let g: serde_json::Value = client
            .post(format!("{base}/capabilities/grant"))
            .json(&json!({ "action": action }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(g["kind"], "capability_granted");
    }

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

    // Capabilities recent should show the dispatch, write/read, and receipt-read grants.
    let caps: serde_json::Value = client
        .get(format!("{base}/capabilities/recent?limit=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(caps["capabilities"].as_array().unwrap().len(), 4);

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
async fn memory_repair_round_trip_after_grant() {
    let TestServer {
        base,
        token,
        agent_id,
        memory,
        ..
    } = spawn_test_server().await;
    let id = uuid::Uuid::new_v4();
    let parent = uuid::Uuid::new_v4();
    memory
        .put(covenant_types::MemoryRecord {
            id,
            tier: covenant_types::MemoryTier::Working,
            owner: agent_id,
            text: "stale parent".into(),
            embedding: vec![1.0],
            metadata: json!({}),
            created_at: 1,
            parent: Some(parent),
        })
        .await
        .unwrap();

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();

    let grant: serde_json::Value = client
        .post(format!("{base}/capabilities/grant"))
        .json(&json!({ "action": "memory.repair.dry_run" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(grant["kind"], "capability_granted");

    let repaired: serde_json::Value = client
        .post(format!("{base}/memory/repair"))
        .json(&json!({
            "mode": "dry_run",
            "command": {
                "action": "detach_parent",
                "id": id,
                "expected_parent": parent
            },
            "reason": "verified stale parent"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(repaired["kind"], "memory_repaired");
    assert_eq!(repaired["outcome"]["id"], id.to_string());
    assert_eq!(repaired["outcome"]["mode"], "dry_run");
    assert_eq!(repaired["outcome"]["would_change"], true);
    assert_eq!(repaired["outcome"]["changed"], false);
    assert_eq!(memory.get(id).await.unwrap().unwrap().parent, Some(parent));
}

#[tokio::test]
async fn memory_compact_round_trip_after_grant() {
    let TestServer {
        base,
        token,
        agent_id,
        memory,
        ..
    } = spawn_test_server().await;
    let id = uuid::Uuid::new_v4();
    memory
        .put(covenant_types::MemoryRecord {
            id,
            tier: covenant_types::MemoryTier::Working,
            owner: agent_id,
            text: "expired working context".into(),
            embedding: vec![1.0],
            metadata: json!({}),
            created_at: 1,
            parent: None,
        })
        .await
        .unwrap();

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();

    let grant: serde_json::Value = client
        .post(format!("{base}/capabilities/grant"))
        .json(&json!({ "action": "memory.compact.dry_run" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(grant["kind"], "capability_granted");

    let compacted: serde_json::Value = client
        .post(format!("{base}/memory/compact"))
        .json(&json!({
            "mode": "dry_run",
            "policy": {
                "delete_working_before_ms": 10
            },
            "reason": "routine compaction"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(compacted["kind"], "memory_compacted");
    assert_eq!(compacted["outcome"]["mode"], "dry_run");
    assert_eq!(compacted["outcome"]["would_change"], true);
    assert_eq!(compacted["outcome"]["changed"], false);
    assert_eq!(compacted["outcome"]["deleted"], json!([id]));
    assert!(memory.get(id).await.unwrap().is_some());
}

#[tokio::test]
async fn tools_list_and_call_round_trip() {
    let TestServer { base, token, .. } = spawn_test_server().await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();

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

async fn spawn_test_server_with_origins(origins: Vec<&'static str>) -> TestServer {
    use axum::http::HeaderValue;
    let identity = Arc::new(LocalIdentity::generate("user@local"));
    let peers: Arc<dyn covenant_peer_auth::PeerRegistry> =
        Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new());
    let token = covenant_peer_auth::PeerToken::generate();
    let token_b58 = token.to_b58();
    peers
        .register(covenant_peer_auth::PeerEntry {
            token,
            agent_id: covenant_types::AgentId::new(identity.display(), identity.pubkey_bytes()),
            registered_at: 0,
        })
        .await
        .unwrap();
    let agent_id = covenant_types::AgentId::new(identity.display(), identity.pubkey_bytes());
    let audit = Arc::new(InMemoryAuditLog::new());
    let memory = Arc::new(InMemoryStore::new());
    let server = Server::new(
        Arc::new(Router::from_cards(vec![stub_card()])),
        Arc::new(MockRunner::new("mocked summary")),
        memory.clone(),
        Arc::new(InMemorySettlement::new()),
        audit.clone(),
        Arc::new(InMemoryCapabilityStore::new()),
        Arc::new(MockEmbedder::new(64)),
        identity,
        Arc::new(covenant_memory::IgnoreSet::default()),
        Arc::new(covenant_mcp::ToolRegistry::from_tools(vec![Arc::new(
            covenant_mcp::native::EchoTool,
        )])),
        Arc::new(covenant_a2a::InMemoryMailbox::new()),
        peers,
        Arc::new(covenant_budget::InMemoryLedger::new()),
    );
    let origins_hv: Vec<HeaderValue> = origins.into_iter().map(HeaderValue::from_static).collect();
    let (live_traces_tx, _) = tokio::sync::broadcast::channel(16);
    let app = router_with_origins(
        HttpState {
            server,
            live_traces_tx,
        },
        origins_hv,
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let _handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    TestServer {
        base: format!("http://{addr}"),
        token: token_b58,
        agent_id,
        audit,
        memory,
        _handle,
    }
}

#[tokio::test]
async fn cors_preflight_allows_configured_origin() {
    let s = spawn_test_server_with_origins(vec!["http://localhost:3000"]).await;
    let r = reqwest::Client::new()
        .request(reqwest::Method::OPTIONS, format!("{}/health", s.base))
        .header("Origin", "http://localhost:3000")
        .header("Access-Control-Request-Method", "GET")
        .send()
        .await
        .unwrap();
    let allow_origin = r
        .headers()
        .get("access-control-allow-origin")
        .map(|v| v.to_str().unwrap().to_string());
    assert_eq!(
        allow_origin.as_deref(),
        Some("http://localhost:3000"),
        "configured origin must be reflected in the preflight response"
    );
}

#[tokio::test]
async fn cors_preflight_rejects_unconfigured_origin() {
    let s = spawn_test_server_with_origins(vec!["http://localhost:3000"]).await;
    let r = reqwest::Client::new()
        .request(reqwest::Method::OPTIONS, format!("{}/health", s.base))
        .header("Origin", "http://evil.example.com")
        .header("Access-Control-Request-Method", "GET")
        .send()
        .await
        .unwrap();
    // tower-http's CORS layer omits the Allow-Origin header when the
    // request origin isn't in the allow-list. The browser treats that
    // as a CORS failure and refuses to expose the response to JS.
    assert!(
        r.headers().get("access-control-allow-origin").is_none(),
        "unconfigured origin must not be reflected"
    );
}

#[tokio::test]
async fn cors_actual_request_carries_allow_origin_for_configured_origin() {
    // Defence-in-depth: the preflight test alone doesn't prove the
    // simple-request path also tags responses correctly. A `GET
    // /health` from an allowed origin must include
    // `Access-Control-Allow-Origin` in its response headers.
    let s = spawn_test_server_with_origins(vec!["http://localhost:3000"]).await;
    let r = reqwest::Client::new()
        .get(format!("{}/health", s.base))
        .header("Origin", "http://localhost:3000")
        .send()
        .await
        .unwrap();
    let allow_origin = r
        .headers()
        .get("access-control-allow-origin")
        .map(|v| v.to_str().unwrap().to_string());
    assert_eq!(allow_origin.as_deref(), Some("http://localhost:3000"));
}

#[tokio::test]
async fn protected_route_accepts_body_up_to_ipc_max_frame() {
    // Transport-parity guard: HTTP and IPC must accept the same payload
    // size. axum's 2 MiB default would silently 413 intents that the
    // IPC daemon would happily process; the explicit DefaultBodyLimit
    // bumps it to covenant_ipc::MAX_FRAME (8 MiB).
    let s = spawn_test_server().await;
    // Fits inside the IPC cap; the per-route handler will reject the
    // shape, but axum must NOT reject on size first.
    let body = serde_json::json!({
        "text": "x".repeat(4 * 1024 * 1024)
    })
    .to_string();
    let r = reqwest::Client::new()
        .post(format!("{}/intent", s.base))
        .bearer_auth(&s.token)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_ne!(
        r.status(),
        413,
        "DefaultBodyLimit must accept 4 MiB; got 413"
    );
}

#[tokio::test]
async fn protected_route_rejects_body_above_ipc_max_frame() {
    // Symmetric to the accept test: a payload above the cap must 413
    // before the route handler runs, matching what IPC's FrameTooLarge
    // returns at the same byte threshold.
    let s = spawn_test_server().await;
    let oversize_text = "x".repeat(9 * 1024 * 1024);
    let body = serde_json::json!({ "text": oversize_text }).to_string();
    let r = reqwest::Client::new()
        .post(format!("{}/intent", s.base))
        .bearer_auth(&s.token)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        413,
        "body above MAX_FRAME must 413; got {}",
        r.status()
    );
}

struct FailingPeerRegistry;

fn outage_err() -> covenant_peer_auth::PeerError {
    covenant_peer_auth::PeerError::Io(std::io::Error::new(
        std::io::ErrorKind::Other,
        "registry storage outage (test fixture)",
    ))
}

#[async_trait::async_trait]
impl covenant_peer_auth::PeerRegistry for FailingPeerRegistry {
    async fn register(
        &self,
        _entry: covenant_peer_auth::PeerEntry,
    ) -> Result<(), covenant_peer_auth::PeerError> {
        Err(outage_err())
    }
    async fn resolve(
        &self,
        _token: &covenant_peer_auth::PeerToken,
    ) -> Result<Option<covenant_types::AgentId>, covenant_peer_auth::PeerError> {
        Err(outage_err())
    }
    async fn revoke(
        &self,
        _token: &covenant_peer_auth::PeerToken,
    ) -> Result<bool, covenant_peer_auth::PeerError> {
        Err(outage_err())
    }
    async fn recent(
        &self,
        _limit: usize,
    ) -> Result<Vec<covenant_peer_auth::PeerEntry>, covenant_peer_auth::PeerError> {
        Err(outage_err())
    }
    async fn list_summaries(
        &self,
        _limit: usize,
        _pubkey_prefix: Option<&str>,
        _status_filter: Option<covenant_peer_auth::PeerStatusFilter>,
    ) -> Result<(Vec<covenant_peer_auth::PeerSummary>, bool), covenant_peer_auth::PeerError> {
        Err(outage_err())
    }
    async fn purge_revoked_older_than(
        &self,
        _before_ms: u64,
    ) -> Result<u64, covenant_peer_auth::PeerError> {
        Err(outage_err())
    }
    async fn revoke_by_token_prefix(
        &self,
        _prefix: &str,
        _limit: usize,
    ) -> Result<covenant_peer_auth::RevokeOutcome, covenant_peer_auth::PeerError> {
        Err(outage_err())
    }
    async fn find_unique_live_by_token_prefix(
        &self,
        _prefix: &str,
    ) -> Result<Option<covenant_peer_auth::PeerSummary>, covenant_peer_auth::PeerError> {
        Err(outage_err())
    }
}

async fn spawn_test_server_with_failing_registry() -> TestServer {
    let identity = Arc::new(LocalIdentity::generate("user@local"));
    let peers: Arc<dyn covenant_peer_auth::PeerRegistry> = Arc::new(FailingPeerRegistry);
    let agent_id = covenant_types::AgentId::new(identity.display(), identity.pubkey_bytes());
    let audit = Arc::new(InMemoryAuditLog::new());
    let memory = Arc::new(InMemoryStore::new());
    let server = Server::new(
        Arc::new(Router::from_cards(vec![stub_card()])),
        Arc::new(MockRunner::new("mocked summary")),
        memory.clone(),
        Arc::new(InMemorySettlement::new()),
        audit.clone(),
        Arc::new(InMemoryCapabilityStore::new()),
        Arc::new(MockEmbedder::new(64)),
        identity,
        Arc::new(covenant_memory::IgnoreSet::default()),
        Arc::new(covenant_mcp::ToolRegistry::from_tools(vec![Arc::new(
            covenant_mcp::native::EchoTool,
        )])),
        Arc::new(covenant_a2a::InMemoryMailbox::new()),
        peers,
        Arc::new(covenant_budget::InMemoryLedger::new()),
    );
    let (live_traces_tx, _) = tokio::sync::broadcast::channel(16);
    let app = router(HttpState {
        server,
        live_traces_tx,
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let _handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    TestServer {
        base: format!("http://{addr}"),
        token: covenant_peer_auth::PeerToken::generate().to_b58(),
        agent_id,
        audit,
        memory,
        _handle,
    }
}

#[tokio::test]
async fn bearer_auth_surfaces_peer_registry_outage_as_503_with_distinct_audit() {
    // Audit-triage guard: a storage outage in the peer registry used
    // to be silently rolled into "unknown or revoked token" 401s.
    // After the fix, the wire response is 503 and the audit row has a
    // distinct reason so operators triaging probe storms can split the
    // two failure classes.
    let s = spawn_test_server_with_failing_registry().await;
    let any_token = covenant_peer_auth::PeerToken::generate().to_b58();
    let r = reqwest::Client::new()
        .get(format!("{}/tools", s.base))
        .bearer_auth(&any_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        503,
        "storage outage must surface as 503, not 401"
    );
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["kind"], "error");
    assert_eq!(body["message"], "peer registry unavailable");

    let events = s.audit.recent(20).await.unwrap();
    let saw_outage_reason = events.iter().any(|e| {
        matches!(
            &e.kind,
            covenant_audit::AuditKind::AuthenticationFailed { transport, reason }
                if transport == "http" && reason == "peer registry unavailable"
        )
    });
    assert!(
        saw_outage_reason,
        "expected an 'peer registry unavailable' audit entry; got: {events:?}"
    );
}
