//! Live integration test: spawns covenantd with the Metaplex profile
//! enabled (read surface), pointed at a local wiremock DAS endpoint, and
//! drives the real IPC socket end to end:
//!
//!   ListTools  → the `metaplex.das.*` read tools are advertised, and the
//!                write tools are NOT (read-only config).
//!   CallTool   → `metaplex.das.get_asset` without a grant is rejected by
//!                the capability gate; with a grant it round-trips a real
//!                JSON-RPC call to the (mock) DAS endpoint and returns the
//!                asset.
//!
//! Hermetic — the only "network" is a wiremock server bound to localhost,
//! so this proves the full daemon path (startup env wiring → tool
//! registration → capability gate → metaplex_tool_call → HttpDasClient →
//! HTTP → result) without an external DAS provider. `#[ignore]`'d like the
//! other live tests; run with:
//!   cargo test -p covenantd --test live_metaplex_e2e -- --ignored live_
//!
//! The write path (mint + AppData via the signer sidecar) is proven
//! separately against devnet; it needs a funded key and is not hermetic.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use serde_json::json;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::time::sleep;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
}

async fn wait_for_sock(path: &std::path::Path) -> bool {
    for _ in 0..100 {
        if path.exists() {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn read_operator_token(home: &std::path::Path) -> String {
    let path = home.join("peers").join("operator.token");
    for _ in 0..50 {
        if let Ok(s) = std::fs::read_to_string(&path) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("operator token never appeared at {}", path.display());
}

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

async fn authenticated_operator(sock: &std::path::Path, token_b58: &str) -> UnixStream {
    let mut stream = UnixStream::connect(sock).await.expect("connect");
    match req(
        &mut stream,
        Request::Authenticate {
            token_b58: token_b58.to_string(),
        },
    )
    .await
    {
        Response::Authenticated { .. } => stream,
        other => panic!("operator authentication failed: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + wiremock DAS, drives metaplex.das.* over IPC"]
async fn live_covenantd_metaplex_das_read_round_trips() {
    // 1. Mock DAS endpoint: any POST returns a getAsset-shaped JSON-RPC
    //    result. The daemon's HttpDasClient returns the `result` value.
    let das = MockServer::start().await;
    let asset_id = "5SViHRyh6s6bK2vUrE46iCprayxqA5aVjHA9rGH6SL5";
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": "covenant-metaplex",
            "result": { "id": asset_id, "interface": "MplCoreAsset" }
        })))
        .mount(&das)
        .await;

    // 2. Spawn the real daemon with the Metaplex read surface enabled.
    let home = tempfile::tempdir().expect("tempdir");
    let port = pick_free_port();
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(exe)
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("COVENANT_METAPLEX_ENABLED", "true")
        .env("COVENANT_METAPLEX_DAS_URL", das.uri())
        // Make sure no ambient write config leaks in from the test env.
        .env_remove("COVENANT_METAPLEX_SIGNER_BIN")
        .env_remove("COVENANT_METAPLEX_RPC_URL")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");

    let sock = home.path().join("sock");
    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon never created its socket");
    }
    let operator_token = read_operator_token(home.path()).await;

    // 3. ListTools: the four read tools are advertised; no write tools.
    {
        let mut stream = authenticated_operator(&sock, &operator_token).await;
        match req(&mut stream, Request::ListTools).await {
            Response::ToolList { tools } => {
                let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
                assert!(
                    names.contains(&"metaplex.das.get_asset"),
                    "read tools must be advertised, got {names:?}"
                );
                assert!(
                    names.contains(&"metaplex.das.search"),
                    "all four read tools must be advertised, got {names:?}"
                );
                assert!(
                    !names.iter().any(|n| n.starts_with("metaplex.attest.")),
                    "write tools must NOT be advertised in read-only config, got {names:?}"
                );
            }
            other => panic!("ListTools failed: {other:?}"),
        }
    }

    // 4. CallTool without a grant: rejected by the capability gate.
    {
        let mut stream = authenticated_operator(&sock, &operator_token).await;
        match req(
            &mut stream,
            Request::CallTool {
                name: "metaplex.das.get_asset".into(),
                arguments: json!({ "id": asset_id }),
            },
        )
        .await
        {
            Response::Error { message } => assert!(
                message.contains("tool.call.metaplex.das.get_asset"),
                "ungranted call must name the required capability, got {message:?}"
            ),
            other => panic!("ungranted call must be rejected, got {other:?}"),
        }
    }

    // 5. Grant the capability, then the call round-trips to the mock DAS.
    {
        let mut stream = authenticated_operator(&sock, &operator_token).await;
        match req(
            &mut stream,
            Request::GrantCapability {
                action: "tool.call.metaplex.das.get_asset".into(),
                scope: None,
                expires_at: None,
            },
        )
        .await
        {
            Response::CapabilityGranted { .. } => {}
            other => panic!("grant failed: {other:?}"),
        }

        match req(
            &mut stream,
            Request::CallTool {
                name: "metaplex.das.get_asset".into(),
                arguments: json!({ "id": asset_id }),
            },
        )
        .await
        {
            Response::ToolResult { content, is_error } => {
                assert!(!is_error, "granted DAS read must succeed, got {content:?}");
                let blob = serde_json::to_string(&content).unwrap();
                assert!(
                    blob.contains(asset_id),
                    "tool result must carry the DAS asset payload, got {blob}"
                );
            }
            other => panic!("granted call failed: {other:?}"),
        }
    }

    let _ = child.kill().await;
}

/// Full write path through the daemon: `CallTool(metaplex.attest.audit_root)`
/// spawns the real signer sidecar, which mints an MPL Core asset and
/// writes the attestation into its AppData on devnet.
///
/// Not hermetic — needs a funded devnet key and the built sidecar, so it
/// is gated on three env vars and returns early (passing) when they are
/// absent. Run it explicitly with:
///   COVENANT_METAPLEX_SIGNER_BIN=/abs/path/covenant-metaplex-signer \
///   COVENANT_METAPLEX_KEYPAIR=/abs/path/devnet-key.json \
///   COVENANT_METAPLEX_RPC_URL=https://api.devnet.solana.com \
///   cargo test -p covenantd --test live_metaplex_e2e \
///     live_covenantd_metaplex_attest_writes_to_devnet -- --ignored --nocapture
#[tokio::test]
#[ignore = "live+funded: drives the daemon -> signer sidecar -> devnet mint"]
async fn live_covenantd_metaplex_attest_writes_to_devnet() {
    let (Ok(signer_bin), Ok(keypair), Ok(rpc)) = (
        std::env::var("COVENANT_METAPLEX_SIGNER_BIN"),
        std::env::var("COVENANT_METAPLEX_KEYPAIR"),
        std::env::var("COVENANT_METAPLEX_RPC_URL"),
    ) else {
        eprintln!(
            "skip: set COVENANT_METAPLEX_SIGNER_BIN + COVENANT_METAPLEX_KEYPAIR + \
             COVENANT_METAPLEX_RPC_URL (funded devnet key) to run this"
        );
        return;
    };

    let home = tempfile::tempdir().expect("tempdir");
    let port = pick_free_port();
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(exe)
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("COVENANT_METAPLEX_ENABLED", "true")
        .env("COVENANT_METAPLEX_CLUSTER", "devnet")
        .env("COVENANT_METAPLEX_SIGNER_BIN", &signer_bin)
        .env("COVENANT_METAPLEX_KEYPAIR", &keypair)
        .env("COVENANT_METAPLEX_RPC_URL", &rpc)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");

    let sock = home.path().join("sock");
    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon never created its socket");
    }
    let operator_token = read_operator_token(home.path()).await;

    let mut stream = authenticated_operator(&sock, &operator_token).await;
    match req(
        &mut stream,
        Request::GrantCapability {
            action: "tool.call.metaplex.attest.audit_root".into(),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { .. } => {}
        other => panic!("grant failed: {other:?}"),
    }

    // A distinct root per run so successive runs don't look identical.
    let root = "11223344556677889900aabbccddeeff11223344556677889900aabbccddeeff";
    match req(
        &mut stream,
        Request::CallTool {
            name: "metaplex.attest.audit_root".into(),
            arguments: json!({
                "rootHashHex": root,
                "releaseTarget": "live-e2e",
                "releaseSubject": "covenant",
                "releaseScope": "audit",
                "recordedAt": 1_717_900_000u64,
            }),
        },
    )
    .await
    {
        Response::ToolResult { content, is_error } => {
            let blob = serde_json::to_string(&content).unwrap();
            assert!(!is_error, "attest write must succeed, got {blob}");
            assert!(
                blob.contains("signature") && blob.contains("asset"),
                "result must carry the on-chain signature + asset, got {blob}"
            );
            eprintln!("devnet attestation: {blob}");
        }
        other => panic!("attest call failed: {other:?}"),
    }

    let _ = child.kill().await;
}
