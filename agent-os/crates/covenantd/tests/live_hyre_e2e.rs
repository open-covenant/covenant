//! Daemon-driven e2e for the Hyre x402 provider profile. Spawns the real
//! covenantd with the Hyre profile enabled (manifest pointed at a local
//! wiremock so boot is hermetic) and drives the IPC socket end to end:
//!
//!   ListTools → the generated `hyre.*` paid tools are advertised (the
//!               profile booted: env → catalog refresh → with_hyre → specs).
//!   CallTool  → `hyre.defi.tvl` without a grant is rejected by the
//!               capability gate naming `tool.call.hyre.defi.tvl`.
//!   CallTool  → with the grant, the call routes past the gate into the
//!               DaemonHyreExecutor (x402 dispatch enabled), where the
//!               budget pre-check refuses a payer with no capacity — proving
//!               the full daemon path runs without ever spending USDC.
//!
//! Hermetic: the only "network" is a wiremock serving Hyre's vendored
//! OpenAPI manifest at boot, and no paid call ever leaves the host (the
//! budget gate fires before the signer sidecar). NOT `#[ignore]`'d — runs
//! in CI. The real paid path (signer sidecar → PayAI → on-chain settle) is
//! exercised by the env-gated `covenant-hyre` live examples.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use serde_json::json;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::time::sleep;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn wait_for_sock(p: &std::path::Path) -> bool {
    for _ in 0..100 {
        if p.exists() {
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
            let t = s.trim();
            if !t.is_empty() {
                return t.to_string();
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

async fn operator(sock: &std::path::Path, token: &str) -> UnixStream {
    let mut s = UnixStream::connect(sock).await.expect("connect");
    match req(
        &mut s,
        Request::Authenticate {
            token_b58: token.to_string(),
        },
    )
    .await
    {
        Response::Authenticated { .. } => s,
        other => panic!("operator auth failed: {other:?}"),
    }
}

#[tokio::test]
async fn live_covenantd_hyre_profile_wires_and_gates_end_to_end() {
    // Boot-time catalog refresh hits {base_url}/openapi.json. Serve the
    // vendored manifest from a local mock so the test never touches the
    // real Hyre host.
    let hyre = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/openapi.json"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(covenant_hyre::VENDORED_MANIFEST)
                .insert_header("content-type", "application/json"),
        )
        .mount(&hyre)
        .await;

    let home = tempfile::tempdir().expect("tempdir");
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(exe)
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .env("COVENANT_HYRE_ENABLED", "1")
        .env("COVENANT_HYRE_BASE_URL", hyre.uri())
        // Enable outbound x402 dispatch so a granted call reaches the
        // executor. The signer binary is never spawned: the budget
        // pre-check refuses the operator (no capacity) first.
        .env("COVENANT_X402_ENABLED", "1")
        .env("COVENANT_X402_SIGNER_BINARY", "/nonexistent-x402-signer")
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
    let token = read_operator_token(home.path()).await;

    // ListTools: the Hyre paid tools are advertised.
    {
        let mut s = operator(&sock, &token).await;
        match req(&mut s, Request::ListTools).await {
            Response::ToolList { tools } => {
                let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
                assert!(
                    names.contains(&"hyre.defi.tvl"),
                    "hyre paid tools must be advertised, got {names:?}"
                );
                assert!(
                    names.contains(&"hyre.ask"),
                    "the high-level hyre.ask tool must be advertised, got {names:?}"
                );
            }
            other => panic!("ListTools failed: {other:?}"),
        }
    }

    // Capability gate: ungranted paid call is rejected by name.
    {
        let mut s = operator(&sock, &token).await;
        match req(
            &mut s,
            Request::CallTool {
                name: "hyre.defi.tvl".into(),
                arguments: json!({}),
            },
        )
        .await
        {
            Response::Error { message } => assert!(
                message.contains("tool.call.hyre.defi.tvl"),
                "ungranted paid call must name the capability, got {message:?}"
            ),
            other => panic!("ungranted paid call must be rejected, got {other:?}"),
        }
    }

    // Granted: routes past the gate into the executor, where the budget
    // pre-check refuses the operator (no capacity) before any signer or
    // network activity. Proves the full daemon path runs.
    {
        let mut s = operator(&sock, &token).await;
        match req(
            &mut s,
            Request::GrantCapability {
                action: "tool.call.hyre.defi.tvl".into(),
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
            &mut s,
            Request::CallTool {
                name: "hyre.defi.tvl".into(),
                arguments: json!({}),
            },
        )
        .await
        {
            // The executor refuses a payer with no budget capacity. The
            // grant cleared the gate, so this is the executor stage, not
            // the capability check.
            Response::ToolResult { content, is_error } => {
                assert!(is_error, "no-capacity payer must yield an error result");
                let blob = serde_json::to_string(&content).unwrap().to_lowercase();
                assert!(
                    blob.contains("capacity") || blob.contains("budget"),
                    "granted call must reach the executor budget gate, got {blob}"
                );
            }
            // Some builds surface the executor refusal as a top-level error;
            // either way it must be an executor-stage message, not the gate.
            Response::Error { message } => {
                let m = message.to_lowercase();
                assert!(
                    m.contains("capacity") || m.contains("budget"),
                    "granted call must reach the executor, got {message:?}"
                );
                assert!(
                    !message.contains("tool.call.hyre"),
                    "granted call must not be a capability rejection, got {message:?}"
                );
            }
            other => panic!("granted call gave unexpected response: {other:?}"),
        }
    }

    let _ = child.kill().await;
}
