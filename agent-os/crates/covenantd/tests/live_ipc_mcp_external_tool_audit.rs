//! Live integration test: a call to an external MCP server's tool lands a
//! `ToolCallCompleted` audit record on the daemon's tamper-proof chain — the
//! tool-execution-accountability boundary extended to tools loaded from an
//! independently-maintained MCP server.
//!
//! Builds on the slice-1 fixture (`live_ipc_mcp_external_tool_dispatch.rs`):
//! registers the in-repo `covenant-mcp-fake-server` via `secrets.toml`, spawns
//! covenantd, then as operator calls one external tool that succeeds and one
//! that returns a tool-level error result, and reads `Request::RecentAudit`. It
//! asserts the feed carries
//!   - a `ToolCallCompleted` naming `mcp_fake_ping` with outcome `Ok`,
//!   - a `ToolCallCompleted` naming `mcp_fake_fail` with outcome `ErrorResult`
//!     (a successful JSON-RPC response carrying `isError`, *not* a `Failed`
//!     transport break),
//!   - hashed arguments (a non-empty hex digest), and never the raw argument
//!     value anywhere in the serialized feed (the redaction barrier).
//!
//! `Server::call_tool` records `ToolCallCompleted` on the generic dispatch path
//! that external `mcp_*` tools fall through (the `metaplex.`/hyre prefixes are
//! special-cased; `mcp_` is not), so a daemon that dispatched the external tool
//! without recording it, leaked raw arguments onto the chain, or collapsed the
//! tool-reported error into `Failed` would pass every existing audit test —
//! those cover only the built-in tools. This pins the external-tool case.
//!
//! Hermetic — the fake server reads only its stdin and emits canned JSON-RPC.
//! `#[ignore]`'d and additionally skipped when the fixture binary is not built.
//! Run with:
//! `cargo test -p covenantd --test live_ipc_mcp_external_tool_audit -- --ignored live_`.

use covenant_audit::{AuditKind, ToolCallOutcome};
use covenant_ipc::{read_frame, write_frame, Request, Response};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::time::sleep;

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

async fn authenticated_stream(sock: &std::path::Path, token_b58: &str) -> UnixStream {
    let mut stream = UnixStream::connect(sock).await.expect("connect");
    let request = Request::Authenticate {
        token_b58: token_b58.to_string(),
    };
    match req(&mut stream, request).await {
        Response::Authenticated { .. } => stream,
        other => panic!("authentication failed: {other:?}"),
    }
}

/// Resolve the built `covenant-mcp-fake-server` binary. `CARGO_BIN_EXE_*` is
/// only exposed to tests in the binary's own crate, so a covenantd test must
/// find the workspace binary itself: it sits next to this test binary's
/// `target/<profile>/` directory (current_exe is `.../target/<profile>/deps/<name>`).
/// An explicit `COVENANT_LIVE_MCP_FAKE_SERVER` path overrides the search.
/// Returns `None` when the fixture is not built so a `-p covenantd`-only run
/// skips rather than fails; `cargo test --workspace` builds it because
/// covenant-mcp's own live_stdio test references it via CARGO_BIN_EXE.
fn fake_server_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("COVENANT_LIVE_MCP_FAKE_SERVER") {
        let p = PathBuf::from(p);
        return p.exists().then_some(p);
    }
    let exe = std::env::current_exe().ok()?;
    let target_dir = exe.parent()?.parent()?; // deps -> target/<profile>
    let bin = target_dir.join("covenant-mcp-fake-server");
    bin.exists().then_some(bin)
}

/// TOML literal string ('...') so the resolved absolute path is written
/// verbatim with no escape processing. Workspace target paths contain no
/// single quotes; if one ever did, fail loudly rather than emit broken TOML.
fn toml_literal(path: &std::path::Path) -> String {
    let s = path.to_str().expect("fixture path is valid UTF-8");
    assert!(
        !s.contains('\''),
        "fixture path contains a single quote, cannot be a TOML literal string: {s}"
    );
    format!("'{s}'")
}

fn is_hex_digest(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit())
}

#[tokio::test]
#[ignore = "live: spawns covenantd + the covenant-mcp-fake-server subprocess; opt-in via --ignored live_"]
async fn live_ipc_mcp_external_tool_call_lands_audit_record() {
    let Some(fake_server) = fake_server_bin() else {
        eprintln!(
            "covenant-mcp-fake-server not built; skipping external-MCP audit live test \
             (build it or set COVENANT_LIVE_MCP_FAKE_SERVER)"
        );
        return;
    };

    let home = tempfile::tempdir().expect("tempdir");

    let secrets = format!(
        "[embed]\n\
         provider = \"mock\"\n\
         \n\
         [[mcp.server]]\n\
         name = \"fake\"\n\
         command = {}\n\
         args = [\"--multi-tool\", \"--string-ids\"]\n\
         tool_prefix = \"fake\"\n",
        toml_literal(&fake_server)
    );
    std::fs::write(home.path().join("secrets.toml"), secrets).expect("write secrets.toml");

    let port = pick_free_port();
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(exe)
        .env("COVENANT_HOME", home.path())
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");

    let sock = home.path().join("sock");
    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon never created its socket at {}", sock.display());
    }

    let operator_token = read_operator_token(home.path()).await;
    let mut stream = authenticated_stream(&sock, &operator_token).await;

    // The `tool.call.<name>` gate applies to the operator too, so grant each
    // external tool before invoking it (an unscoped-arguments grant authorizes
    // any arguments for that tool).
    for tool in ["mcp_fake_ping", "mcp_fake_fail"] {
        match req(
            &mut stream,
            Request::GrantCapability {
                action: format!("tool.call.{tool}"),
                scope: Some(serde_json::json!({ "version": 1, "tool": tool })),
                expires_at: None,
            },
        )
        .await
        {
            Response::CapabilityGranted { .. } => {}
            other => panic!("grant tool.call.{tool} failed: {other:?}"),
        }
    }

    // A successful external call. `audit-me` is a distinctive argument value so
    // the redaction assertion below is meaningful — it must never surface raw.
    match req(
        &mut stream,
        Request::CallTool {
            name: "mcp_fake_ping".into(),
            arguments: serde_json::json!({ "text": "audit-me" }),
        },
    )
    .await
    {
        Response::ToolResult { is_error, .. } => {
            assert!(!is_error, "mcp_fake_ping must return a success result")
        }
        other => panic!("mcp_fake_ping must return a ToolResult, got {other:?}"),
    }

    // A tool-level error: the fake server replies with a well-formed JSON-RPC
    // result carrying isError=true. The daemon must classify this ErrorResult
    // (the tool ran and reported failure), not Failed (the transport broke).
    match req(
        &mut stream,
        Request::CallTool {
            name: "mcp_fake_fail".into(),
            arguments: serde_json::json!({}),
        },
    )
    .await
    {
        Response::ToolResult { is_error, .. } => {
            assert!(is_error, "mcp_fake_fail must return a tool-level error result")
        }
        other => panic!("mcp_fake_fail must return a ToolResult, got {other:?}"),
    }

    // ── Both calls must land on the operator's own audit feed. A large limit
    //    over a freshly-spawned daemon guarantees both records are in window.
    match req(
        &mut stream,
        Request::RecentAudit {
            limit: 100,
            since_ms: None,
            prefer_stream: None,
        },
    )
    .await
    {
        Response::AuditEvents { events } => {
            // The redaction barrier: arguments are hashed, never persisted raw.
            // Check the whole serialized feed so a leak in any event fails here.
            let serialized = serde_json::to_string(&events).expect("serialize audit events");
            assert!(
                !serialized.contains("audit-me"),
                "raw tool arguments must never reach the audit chain: {serialized}"
            );

            let completions: Vec<(&str, ToolCallOutcome, &str)> = events
                .iter()
                .filter_map(|e| match &e.kind {
                    AuditKind::ToolCallCompleted {
                        tool,
                        outcome,
                        arguments_hash_hex,
                        ..
                    } => Some((tool.as_str(), *outcome, arguments_hash_hex.as_str())),
                    _ => None,
                })
                .collect();

            let ping = completions
                .iter()
                .find(|(tool, ..)| *tool == "mcp_fake_ping")
                .unwrap_or_else(|| {
                    panic!("audit feed must record the external mcp_fake_ping call: {completions:?}")
                });
            assert_eq!(
                ping.1,
                ToolCallOutcome::Ok,
                "a successful external tool call must record outcome Ok"
            );
            assert!(
                is_hex_digest(ping.2),
                "external tool arguments must be hashed to a hex digest, got {:?}",
                ping.2
            );

            let fail = completions
                .iter()
                .find(|(tool, ..)| *tool == "mcp_fake_fail")
                .unwrap_or_else(|| {
                    panic!("audit feed must record the external mcp_fake_fail call: {completions:?}")
                });
            assert_eq!(
                fail.1,
                ToolCallOutcome::ErrorResult,
                "an external tool returning isError must record ErrorResult, not Failed"
            );
            assert!(
                is_hex_digest(fail.2),
                "external tool arguments must be hashed to a hex digest, got {:?}",
                fail.2
            );
        }
        other => panic!("RecentAudit must return AuditEvents, got {other:?}"),
    }

    let _ = child.kill().await;
}
