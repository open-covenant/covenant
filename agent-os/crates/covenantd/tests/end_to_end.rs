//! End-to-end test: spawn the daemon's listener loop on a tempdir socket
//! with a hand-built router, MockRunner, in-memory memory store, and
//! in-memory settlement store. Drive ping → intent → recent_memory →
//! recent_receipts through the wire and assert each path.

use covenant_identity::LocalIdentity;
use covenant_ipc::{protocol_info, read_frame, write_frame, Request, Response};
use covenant_llm::MockEmbedder;
use covenant_manifest::Manifest;
use covenant_memory::InMemoryStore;
use covenant_router::{AgentCard, Router};
use covenant_runtime::MockRunner;
use covenant_settlement::InMemorySettlement;
use covenant_types::{MemoryTier, ResourceKind};
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::net::{UnixListener, UnixStream};

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

#[tokio::test]
async fn protocol_info_probe_then_full_loop_ping_intent_memory_receipts() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("sock");
    let listener = UnixListener::bind(&sock).unwrap();

    let router = Arc::new(Router::from_cards(vec![stub_card()]));
    let runner = Arc::new(MockRunner::new("research stub processed: x"));
    let memory = Arc::new(InMemoryStore::new());
    let settlement = Arc::new(InMemorySettlement::new());
    let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
    let capabilities = Arc::new(covenant_permissions::InMemoryCapabilityStore::new());
    let embedder = Arc::new(MockEmbedder::new(64));
    let identity = Arc::new(LocalIdentity::generate("user@local"));
    let ignore = Arc::new(covenant_memory::IgnoreSet::default());
    let tools = Arc::new(covenant_mcp::ToolRegistry::default());
    let mailbox: Arc<dyn covenant_a2a::Mailbox> = Arc::new(covenant_a2a::InMemoryMailbox::new());
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
    let budget: Arc<dyn covenant_budget::BudgetLedger> =
        Arc::new(covenant_budget::InMemoryLedger::new());
    let server = covenantd::Server::new(
        router,
        runner,
        memory,
        settlement,
        audit,
        capabilities,
        embedder,
        identity,
        ignore,
        tools,
        mailbox,
        peers,
        budget,
    );

    let server_handle = tokio::spawn(async move { server.serve(listener).await });

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    write_frame(&mut stream, &Request::ProtocolInfo)
        .await
        .unwrap();
    assert_eq!(
        read_frame::<_, Response>(&mut stream).await.unwrap(),
        Response::ProtocolInfo {
            info: protocol_info()
        }
    );

    write_frame(&mut stream, &Request::Authenticate { token_b58 })
        .await
        .unwrap();
    match read_frame::<_, Response>(&mut stream).await.unwrap() {
        Response::Authenticated { display } => assert_eq!(display, "user@local"),
        other => panic!("auth failed: {other:?}"),
    }

    write_frame(&mut stream, &Request::Ping).await.unwrap();
    assert_eq!(
        read_frame::<_, Response>(&mut stream).await.unwrap(),
        Response::Pong
    );

    for action in [
        "tool.web_search",
        "memory.write",
        "memory.read",
        "chain.receipts",
    ] {
        write_frame(
            &mut stream,
            &Request::GrantCapability {
                action: action.into(),
                scope: None,
                expires_at: None,
            },
        )
        .await
        .unwrap();
        match read_frame::<_, Response>(&mut stream).await.unwrap() {
            Response::CapabilityGranted { .. } => {}
            other => panic!("unexpected grant response for {action}: {other:?}"),
        }
    }

    write_frame(
        &mut stream,
        &Request::SubmitIntent {
            text: "find recent papers on agent memory".into(),
        },
    )
    .await
    .unwrap();
    let intent_id = match read_frame::<_, Response>(&mut stream).await.unwrap() {
        Response::IntentResult {
            status,
            text,
            settlement,
            intent_id,
            ..
        } => {
            assert_eq!(status, "ok");
            assert_eq!(text, "research stub processed: x");
            assert!(settlement.is_none());
            intent_id
        }
        other => panic!("unexpected: {other:?}"),
    };

    write_frame(
        &mut stream,
        &Request::RecentMemory {
            tier: Some(MemoryTier::Working),
            limit: 5,
            prefer_stream: None,
        },
    )
    .await
    .unwrap();
    match read_frame::<_, Response>(&mut stream).await.unwrap() {
        Response::Memories { records } => {
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].id, intent_id);
            assert_eq!(records[0].text, "research stub processed: x");
            assert_eq!(records[0].tier, MemoryTier::Working);
        }
        other => panic!("unexpected: {other:?}"),
    }

    write_frame(
        &mut stream,
        &Request::RecentReceipts {
            limit: 5,
            since_ms: None,
        },
    )
    .await
    .unwrap();
    match read_frame::<_, Response>(&mut stream).await.unwrap() {
        Response::Receipts { receipts } => {
            assert_eq!(receipts.len(), 1);
            assert_eq!(receipts[0].resource, ResourceKind::Memory);
            assert!(receipts[0].credits_consumed >= 1);
            assert!(receipts[0].onchain_sig.is_none());
        }
        other => panic!("unexpected: {other:?}"),
    }

    drop(stream);
    server_handle.abort();
    let _ = server_handle.await;
}
