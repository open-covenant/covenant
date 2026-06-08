//! Covenantd-level dispatch coverage for the SAID bridge verbs that do
//! not touch the network: the unconfigured status snapshot, the
//! "bridge not wired" guard shared by every bridge-backed verb, the
//! anchor-status cursor read, and the fixture-mode anchor round-trip
//! (claim, JSONL write, confirm, status reflection). The live
//! REST/worker/on-chain arms stay out of scope here — they are
//! operator-gated behind the paid flags and covered in the
//! `covenant-said-bridge` crate tests.

use covenant_identity::LocalIdentity;
use covenant_ipc::{Request, Response};
use covenant_llm::MockEmbedder;
use covenant_memory::InMemoryStore;
use covenant_router::Router;
use covenant_runtime::MockRunner;
use covenant_said_bridge::{Config, SaidBridge};
use covenant_settlement::InMemorySettlement;
use covenant_types::AgentId;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::tempdir;

fn server(home: Option<PathBuf>, bridge: Option<SaidBridge>) -> covenantd::Server {
    let router = Arc::new(Router::from_cards(vec![]));
    let runner = Arc::new(MockRunner::new("stub"));
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
    let budget: Arc<dyn covenant_budget::BudgetLedger> =
        Arc::new(covenant_budget::InMemoryLedger::new());

    let mut server = covenantd::Server::new(
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
    if let Some(home) = home {
        server = server.with_home(home);
    }
    if let Some(bridge) = bridge {
        server = server.with_said_bridge(bridge);
    }
    server
}

fn peer() -> AgentId {
    let id = LocalIdentity::generate("peer@local");
    AgentId::new(id.display(), id.pubkey_bytes())
}

fn devnet_bridge(enabled: bool) -> SaidBridge {
    let mut env = vec![("COVENANT_SOLANA_CLUSTER", "devnet")];
    if enabled {
        env.push(("COVENANT_SAID_ENABLED", "1"));
    }
    SaidBridge::new(Config::from_env(env)).unwrap()
}

const NOT_WIRED: &str = "said bridge is not wired into this daemon";

#[tokio::test]
async fn said_status_without_bridge_reports_disabled_snapshot() {
    let server = server(None, None);
    let resp = server.respond(Request::SaidStatus, &peer()).await;
    assert_eq!(
        resp,
        Response::SaidStatus {
            enabled: false,
            cluster: String::new(),
            program_id: String::new(),
            rpc_url: String::new(),
            api_base_url: String::new(),
            paid_gates: "none".into(),
            has_signer: false,
        }
    );
}

#[tokio::test]
async fn bridge_backed_verbs_without_bridge_report_not_wired() {
    let server = server(None, None);
    let peer = peer();
    let requests = [
        Request::SaidLookup {
            wallet: "wallet".into(),
        },
        Request::SaidAnchor {
            start_audit_index: 0,
            end_audit_index: 1,
            merkle_root_hex: "ab".repeat(32),
            live: false,
        },
        Request::SaidInbox {
            chain: "solana".into(),
            address: "addr".into(),
        },
        Request::SaidFreeTier {
            address: "addr".into(),
        },
        Request::SaidSend {
            source_chain: "solana".into(),
            source_address: "from".into(),
            target_chain: "base".into(),
            target_address: "to".into(),
            payload_json: "{}".into(),
        },
        Request::SaidRegisterOnChain {
            metadata_uri: "ipfs://card".into(),
        },
        Request::SaidGetVerified,
        Request::SaidValidateWork {
            agent: "agent".into(),
            task_hash_hex: "00".repeat(32),
            passed: true,
            evidence_uri: "ipfs://evidence".into(),
        },
    ];

    for req in requests {
        let resp = server.respond(req, &peer).await;
        assert_eq!(
            resp,
            Response::Error {
                message: NOT_WIRED.into(),
            }
        );
    }
}

#[tokio::test]
async fn said_anchor_status_without_home_errors() {
    let server = server(None, None);
    let resp = server
        .respond(Request::SaidAnchorStatus { recent_limit: 10 }, &peer())
        .await;
    assert_eq!(
        resp,
        Response::Error {
            message: "daemon home is not set; cannot resolve $COVENANT_HOME/said".into(),
        }
    );
}

#[tokio::test]
async fn said_anchor_status_with_home_reports_empty_cursor() {
    let dir = tempdir().unwrap();
    let server = server(Some(dir.path().to_path_buf()), None);
    let resp = server
        .respond(Request::SaidAnchorStatus { recent_limit: 10 }, &peer())
        .await;
    assert_eq!(
        resp,
        Response::SaidAnchorStatus {
            next_index: 0,
            last_confirmed_index: None,
            pending: 0,
            recent: vec![],
        }
    );
    assert!(dir.path().join("said").join("cursor.db").exists());
}

#[tokio::test]
async fn said_status_with_bridge_reflects_config() {
    let config = Config::from_env([
        ("COVENANT_SOLANA_CLUSTER", "devnet"),
        ("COVENANT_SAID_ENABLED", "1"),
        (
            "COVENANT_SAID_PROGRAM_ID",
            "Prog1111111111111111111111111111111111111111",
        ),
        ("COVENANT_SAID_RPC_URL", "https://rpc.example"),
        ("COVENANT_SAID_API_BASE_URL", "https://api.example"),
        ("COVENANT_SAID_ALLOW_PAID_ANCHOR", "1"),
    ]);
    let bridge = SaidBridge::new(config).unwrap();
    let server = server(None, Some(bridge));

    // `has_signer` reads the process-global COVENANT_SAID_KEYPAIR env, so
    // it is intentionally left unchecked to keep the test env-independent.
    match server.respond(Request::SaidStatus, &peer()).await {
        Response::SaidStatus {
            enabled,
            cluster,
            program_id,
            rpc_url,
            api_base_url,
            paid_gates,
            has_signer: _,
        } => {
            assert!(enabled);
            assert_eq!(cluster, "devnet");
            assert_eq!(program_id, "Prog1111111111111111111111111111111111111111");
            assert_eq!(rpc_url, "https://rpc.example");
            assert_eq!(api_base_url, "https://api.example");
            assert_eq!(paid_gates, "anchor");
        }
        other => panic!("expected SaidStatus, got {other:?}"),
    }
}

#[tokio::test]
async fn said_anchor_fixture_mode_confirms_and_reports() {
    let dir = tempdir().unwrap();
    let server = server(Some(dir.path().to_path_buf()), Some(devnet_bridge(true)));
    let peer = peer();

    let resp = server
        .respond(
            Request::SaidAnchor {
                start_audit_index: 0,
                end_audit_index: 7,
                merkle_root_hex: "ab".repeat(32),
                live: false,
            },
            &peer,
        )
        .await;
    assert_eq!(
        resp,
        Response::SaidAnchored {
            anchor_index: 0,
            start_seq: 0,
            end_seq: 7,
            merkle_root_hex: "ab".repeat(32),
            tx_sig: "fixture:0".into(),
            slot: 0,
            fixture: true,
        }
    );

    let pending = dir.path().join("said").join("anchor_pending.jsonl");
    let lines = std::fs::read_to_string(&pending).unwrap();
    assert_eq!(lines.lines().count(), 1);

    match server
        .respond(Request::SaidAnchorStatus { recent_limit: 10 }, &peer)
        .await
    {
        Response::SaidAnchorStatus {
            next_index,
            last_confirmed_index,
            pending,
            recent,
        } => {
            assert_eq!(next_index, 1);
            assert_eq!(last_confirmed_index, Some(0));
            assert_eq!(pending, 0);
            assert_eq!(recent.len(), 1);
            let row = &recent[0];
            assert_eq!(row.anchor_index, 0);
            assert_eq!(row.start_seq, 0);
            assert_eq!(row.end_seq, 7);
            assert_eq!(row.merkle_root_hex, "ab".repeat(32));
            assert_eq!(row.tx_sig, "fixture:0");
            assert_eq!(row.slot, 0);
        }
        other => panic!("expected SaidAnchorStatus, got {other:?}"),
    }
}

#[tokio::test]
async fn said_anchor_disabled_bridge_reports_disabled() {
    let dir = tempdir().unwrap();
    let server = server(Some(dir.path().to_path_buf()), Some(devnet_bridge(false)));
    let resp = server
        .respond(
            Request::SaidAnchor {
                start_audit_index: 0,
                end_audit_index: 7,
                merkle_root_hex: "ab".repeat(32),
                live: false,
            },
            &peer(),
        )
        .await;
    assert_eq!(
        resp,
        Response::Error {
            message: "said anchor: said bridge is disabled".into(),
        }
    );
}

#[tokio::test]
async fn said_anchor_inverted_range_is_rejected() {
    let dir = tempdir().unwrap();
    let server = server(Some(dir.path().to_path_buf()), Some(devnet_bridge(true)));
    let resp = server
        .respond(
            Request::SaidAnchor {
                start_audit_index: 5,
                end_audit_index: 1,
                merkle_root_hex: "ab".repeat(32),
                live: false,
            },
            &peer(),
        )
        .await;
    match resp {
        Response::Error { message } => {
            assert!(
                message.starts_with("said anchor: invalid input:"),
                "{message}"
            );
            assert!(message.contains("start_audit_index"), "{message}");
        }
        other => panic!("expected Error, got {other:?}"),
    }
}
