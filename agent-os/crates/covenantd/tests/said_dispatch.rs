//! Covenantd-level dispatch coverage for the SAID bridge verbs. The
//! offline arms — the unconfigured status snapshot, the "bridge not
//! wired" guard shared by every bridge-backed verb, the anchor-status
//! cursor read, and the fixture-mode anchor round-trip (claim, JSONL
//! write, confirm, status reflection) — run without any socket. The
//! REST envelope mappings (lookup, inbox, free-tier, send) drive a
//! one-shot loopback listener so both the success decode and the
//! verb-prefixed `http <status>` error mapping are exercised without
//! the public SAID API. Paid on-chain verbs are covered at the
//! closed-gate rejection and, via a shell-stub worker, the success
//! field-copy onto each `Response::Said*` variant; the real worker
//! subprocess and live REST against api.saidprotocol.com stay
//! operator-gated and live in the `covenant-said-bridge` crate tests.

use covenant_identity::LocalIdentity;
use covenant_ipc::{Request, Response};
use covenant_llm::MockEmbedder;
use covenant_memory::InMemoryStore;
use covenant_router::Router;
use covenant_runtime::MockRunner;
use covenant_said_bridge::config::{DEFAULT_REST_TIMEOUT, DEFAULT_WORKER_TIMEOUT};
use covenant_said_bridge::{
    Cluster, Config, PaidGates, SaidBridge, DEFAULT_SAID_API_BASE_URL,
    DEFAULT_SAID_DEVNET_PROGRAM_ID,
};
use covenant_settlement::InMemorySettlement;
use covenant_types::AgentId;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

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

/// Enabled bridge with every paid gate open and the worker subprocess
/// replaced by a shell stub that drains stdin and prints `envelope_json`
/// verbatim — the success shape `worker::invoke` parses. Mirrors the
/// crate's `worker_roundtrip` stub so the paid on-chain success arms are
/// exercised at the covenantd dispatch boundary without a real worker,
/// signer, or paid transaction. `Config::from_env` can't carry the
/// space-laden `sh -c` command (it splits `COVENANT_SAID_WORKER_CMD` on
/// whitespace), so the config is built as a literal.
fn worker_stub_bridge(envelope_json: &str) -> SaidBridge {
    let script = format!("cat >/dev/null; printf '%s' '{envelope_json}'");
    let config = Config {
        enabled: true,
        cluster: Cluster::Devnet,
        program_id: DEFAULT_SAID_DEVNET_PROGRAM_ID.into(),
        rpc_url: "https://api.devnet.solana.com".into(),
        api_base_url: DEFAULT_SAID_API_BASE_URL.into(),
        paid: PaidGates {
            register: true,
            verify: true,
            anchor: true,
            validate_work: true,
        },
        worker_command: vec!["sh".into(), "-c".into(), script],
        worker_timeout: DEFAULT_WORKER_TIMEOUT,
        rest_timeout: DEFAULT_REST_TIMEOUT,
    };
    SaidBridge::new(config).unwrap()
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

struct Captured {
    method: String,
    path: String,
    body: String,
}

/// One-shot loopback HTTP server: binds an ephemeral port, answers a single
/// request with `status` + `body`, and hands back the parsed request so a
/// test can assert what the dispatch layer drove over REST. Mirrors the
/// `covenant-said-bridge` crate harness so dispatch tests stay socket-real
/// without a mock framework or the public SAID API.
async fn serve_once(status: &'static str, body: &'static str) -> (String, JoinHandle<Captured>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let captured = read_request(&mut stream).await;
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.expect("write");
        stream.flush().await.expect("flush");
        captured
    });
    (format!("http://{addr}"), handle)
}

async fn read_request(stream: &mut TcpStream) -> Captured {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    let header_end = loop {
        let n = stream.read(&mut chunk).await.expect("read headers");
        if n == 0 {
            break buf.len();
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut request_line = head.lines().next().unwrap_or_default().split_whitespace();
    let method = request_line.next().unwrap_or_default().to_owned();
    let path = request_line.next().unwrap_or_default().to_owned();
    let content_length = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
        })
        .unwrap_or(0);
    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk).await.expect("read body");
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Captured {
        method,
        path,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

fn bridge_at(base: &str) -> SaidBridge {
    SaidBridge::new(Config::from_env([
        ("COVENANT_SOLANA_CLUSTER", "devnet"),
        ("COVENANT_SAID_ENABLED", "1"),
        ("COVENANT_SAID_API_BASE_URL", base),
    ]))
    .unwrap()
}

#[tokio::test]
async fn said_lookup_maps_agent_onto_response() {
    let (base, http) = serve_once(
        "200 OK",
        r#"{"wallet":"Wallet111","owner":"Owner222","name":"scout","isVerified":true,"reputationScore":4.5,"feedbackCount":12,"activityCount":99,"metadataUri":"https://example.test/a.json","registeredAt":"2026-01-01T00:00:00Z"}"#,
    )
    .await;
    let server = server(None, Some(bridge_at(&base)));
    let resp = server
        .respond(
            Request::SaidLookup {
                wallet: "Wallet111".into(),
            },
            &peer(),
        )
        .await;
    let req = http.await.expect("server");

    assert_eq!(req.method, "GET");
    assert_eq!(req.path, "/api/agents/Wallet111");
    assert_eq!(
        resp,
        Response::SaidAgent {
            wallet: "Wallet111".into(),
            pda: None,
            owner: Some("Owner222".into()),
            name: Some("scout".into()),
            description: None,
            metadata_uri: Some("https://example.test/a.json".into()),
            is_verified: true,
            sponsored: false,
            reputation_score: 4.5,
            feedback_count: 12,
            activity_count: 99,
            registered_at: Some("2026-01-01T00:00:00Z".into()),
        }
    );
}

#[tokio::test]
async fn said_inbox_maps_messages_onto_response() {
    let (base, http) = serve_once(
        "200 OK",
        r#"{"chain":"solana","address":"Addr1","messages":[{"id":"m1","sourceChain":"base","sourceAddress":"0xabc","targetChain":"solana","targetAddress":"Addr1","payload":{"hello":"world"},"createdAt":1700000000}]}"#,
    )
    .await;
    let server = server(None, Some(bridge_at(&base)));
    let resp = server
        .respond(
            Request::SaidInbox {
                chain: "solana".into(),
                address: "Addr1".into(),
            },
            &peer(),
        )
        .await;
    let req = http.await.expect("server");

    assert_eq!(req.method, "GET");
    assert_eq!(req.path, "/xchain/inbox/solana/Addr1");
    match resp {
        Response::SaidInbox {
            chain,
            address,
            messages,
        } => {
            assert_eq!(chain, "solana");
            assert_eq!(address, "Addr1");
            assert_eq!(messages.len(), 1);
            let msg = &messages[0];
            assert_eq!(msg.id, "m1");
            assert_eq!(msg.source_chain, "base");
            assert_eq!(msg.target_address, "Addr1");
            assert_eq!(msg.created_at, Some(1700000000));
        }
        other => panic!("expected SaidInbox, got {other:?}"),
    }
}

#[tokio::test]
async fn said_free_tier_maps_status_onto_response() {
    let (base, http) = serve_once(
        "200 OK",
        r#"{"address":"Addr1","used":3,"remaining":7,"limit":10,"paidPrice":"0.01","paymentChains":[{"name":"base","network":"mainnet"}]}"#,
    )
    .await;
    let server = server(None, Some(bridge_at(&base)));
    let resp = server
        .respond(
            Request::SaidFreeTier {
                address: "Addr1".into(),
            },
            &peer(),
        )
        .await;
    let req = http.await.expect("server");

    assert_eq!(req.method, "GET");
    assert_eq!(req.path, "/xchain/free-tier/Addr1");
    assert_eq!(
        resp,
        Response::SaidFreeTier {
            address: "Addr1".into(),
            used: 3,
            remaining: 7,
            limit: 10,
            paid_price: Some("0.01".into()),
            payment_chains: vec!["base".into()],
        }
    );
}

#[tokio::test]
async fn said_send_posts_payload_and_maps_receipt() {
    let (base, http) = serve_once(
        "200 OK",
        r#"{"messageId":"msg-1","freeTierRemaining":9,"deliveredAt":1700000001}"#,
    )
    .await;
    let server = server(None, Some(bridge_at(&base)));
    let resp = server
        .respond(
            Request::SaidSend {
                source_chain: "solana".into(),
                source_address: "Addr1".into(),
                target_chain: "base".into(),
                target_address: "0xabc".into(),
                payload_json: r#"{"ping":1}"#.into(),
            },
            &peer(),
        )
        .await;
    let req = http.await.expect("server");

    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/xchain/message");
    assert!(
        req.body.contains(r#""sourceChain":"solana""#),
        "body: {}",
        req.body
    );
    assert_eq!(
        resp,
        Response::SaidSent {
            message_id: "msg-1".into(),
            free_tier_remaining: Some(9),
            delivered_at: Some(1700000001),
        }
    );
}

#[tokio::test]
async fn said_send_invalid_payload_json_is_rejected_before_network() {
    let server = server(None, Some(devnet_bridge(true)));
    let resp = server
        .respond(
            Request::SaidSend {
                source_chain: "solana".into(),
                source_address: "Addr1".into(),
                target_chain: "base".into(),
                target_address: "0xabc".into(),
                payload_json: "{not json".into(),
            },
            &peer(),
        )
        .await;
    match resp {
        Response::Error { message } => {
            assert!(message.starts_with("invalid payload JSON:"), "{message}");
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn said_lookup_http_error_maps_to_verb_prefixed_error() {
    let (base, http) = serve_once("404 Not Found", r#"{"error":"no such agent"}"#).await;
    let server = server(None, Some(bridge_at(&base)));
    let resp = server
        .respond(
            Request::SaidLookup {
                wallet: "Missing111".into(),
            },
            &peer(),
        )
        .await;
    let req = http.await.expect("server");

    assert_eq!(req.method, "GET");
    assert_eq!(req.path, "/api/agents/Missing111");
    assert_eq!(
        resp,
        Response::Error {
            message: r#"said lookup: http 404: {"error":"no such agent"}"#.into(),
        }
    );
}

#[tokio::test]
async fn said_inbox_http_error_maps_to_verb_prefixed_error() {
    let (base, http) = serve_once("404 Not Found", r#"{"error":"unknown chain"}"#).await;
    let server = server(None, Some(bridge_at(&base)));
    let resp = server
        .respond(
            Request::SaidInbox {
                chain: "dogecoin".into(),
                address: "Addr1".into(),
            },
            &peer(),
        )
        .await;
    let req = http.await.expect("server");

    assert_eq!(req.method, "GET");
    assert_eq!(req.path, "/xchain/inbox/dogecoin/Addr1");
    assert_eq!(
        resp,
        Response::Error {
            message: r#"said inbox: http 404: {"error":"unknown chain"}"#.into(),
        }
    );
}

#[tokio::test]
async fn said_free_tier_payment_required_maps_to_verb_prefixed_error() {
    let (base, http) =
        serve_once("402 Payment Required", r#"{"error":"free tier exhausted"}"#).await;
    let server = server(None, Some(bridge_at(&base)));
    let resp = server
        .respond(
            Request::SaidFreeTier {
                address: "Addr1".into(),
            },
            &peer(),
        )
        .await;
    let req = http.await.expect("server");

    assert_eq!(req.method, "GET");
    assert_eq!(req.path, "/xchain/free-tier/Addr1");
    assert_eq!(
        resp,
        Response::Error {
            message: r#"said free-tier: http 402: {"error":"free tier exhausted"}"#.into(),
        }
    );
}

#[tokio::test]
async fn said_send_http_error_maps_to_verb_prefixed_error() {
    let (base, http) = serve_once("500 Internal Server Error", r#"{"error":"relay down"}"#).await;
    let server = server(None, Some(bridge_at(&base)));
    let resp = server
        .respond(
            Request::SaidSend {
                source_chain: "solana".into(),
                source_address: "Addr1".into(),
                target_chain: "base".into(),
                target_address: "0xabc".into(),
                payload_json: r#"{"ping":1}"#.into(),
            },
            &peer(),
        )
        .await;
    let req = http.await.expect("server");

    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/xchain/message");
    assert_eq!(
        resp,
        Response::Error {
            message: r#"said send: http 500: {"error":"relay down"}"#.into(),
        }
    );
}

#[tokio::test]
async fn paid_verbs_with_closed_gate_report_gated_error() {
    let server = server(None, Some(devnet_bridge(true)));
    let peer = peer();
    let cases = [
        (
            Request::SaidRegisterOnChain {
                metadata_uri: "ipfs://card".into(),
            },
            "said register: paid instruction register_agent is gated off",
        ),
        (
            Request::SaidGetVerified,
            "said get_verified: paid instruction get_verified is gated off",
        ),
        (
            Request::SaidValidateWork {
                agent: "agent".into(),
                task_hash_hex: "00".repeat(32),
                passed: true,
                evidence_uri: "ipfs://evidence".into(),
            },
            "said validate_work: paid instruction validate_work is gated off",
        ),
    ];

    for (req, expected) in cases {
        let resp = server.respond(req, &peer).await;
        assert_eq!(
            resp,
            Response::Error {
                message: expected.into(),
            }
        );
    }
}

#[tokio::test]
async fn said_register_on_chain_maps_success_onto_response() {
    let bridge = worker_stub_bridge(
        r#"{"ok":true,"data":{"agentPda":"Agent111","owner":"Own222","signature":"sig333"}}"#,
    );
    let server = server(None, Some(bridge));
    let resp = server
        .respond(
            Request::SaidRegisterOnChain {
                metadata_uri: "ipfs://card".into(),
            },
            &peer(),
        )
        .await;
    assert_eq!(
        resp,
        Response::SaidOnChainRegistered {
            agent_pda: "Agent111".into(),
            owner: "Own222".into(),
            signature: "sig333".into(),
        }
    );
}

#[tokio::test]
async fn said_get_verified_maps_success_onto_response() {
    let bridge = worker_stub_bridge(r#"{"ok":true,"data":{"signature":"sigV","slot":42}}"#);
    let server = server(None, Some(bridge));
    let resp = server.respond(Request::SaidGetVerified, &peer()).await;
    assert_eq!(
        resp,
        Response::SaidVerified {
            signature: "sigV".into(),
            slot: 42,
        }
    );
}

#[tokio::test]
async fn said_validate_work_maps_success_onto_response() {
    let bridge = worker_stub_bridge(
        r#"{"ok":true,"data":{"validationPda":"Val111","validator":"Ver222","signature":"sigW"}}"#,
    );
    let server = server(None, Some(bridge));
    let resp = server
        .respond(
            Request::SaidValidateWork {
                agent: "AgentPda".into(),
                task_hash_hex: "ab".repeat(32),
                passed: true,
                evidence_uri: "ipfs://evidence".into(),
            },
            &peer(),
        )
        .await;
    assert_eq!(
        resp,
        Response::SaidValidationPosted {
            validation_pda: "Val111".into(),
            validator: "Ver222".into(),
            signature: "sigW".into(),
        }
    );
}
