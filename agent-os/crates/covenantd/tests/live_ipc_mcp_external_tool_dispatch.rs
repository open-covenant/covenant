//! Live integration test: spawns covenantd against a tempdir HOME whose
//! `secrets.toml` registers the in-repo `covenant-mcp-fake-server` as an
//! external MCP server, then drives one of that server's tools through the
//! daemon's `Request::CallTool` dispatch over the unix socket.
//!
//! This pins the *seam* between two paths that are each already live-covered
//! in isolation: the stdio transport (covered at the covenant-mcp crate level
//! by live_stdio.rs) and the daemon tool-call dispatch (covered for the
//! built-in echo/clock tools). No existing test loads an external MCP server
//! through the daemon, so the config -> spawn -> bootstrap -> register ->
//! dispatch wiring is otherwise unproven end to end: a daemon that silently
//! skipped the server, registered the tool under the wrong name, or returned a
//! stub instead of routing to the subprocess would pass every other test. The
//! exact `pong: <text>` echo and computed `sum` are what rule that out.
//!
//! Hermetic — the fake server reads only its stdin and emits canned JSON-RPC,
//! no network. `#[ignore]`'d and additionally skipped when the fixture binary
//! is not built. Run with:
//! `cargo test -p covenantd --test live_ipc_mcp_external_tool_dispatch -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_mcp::Content;
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

#[tokio::test]
#[ignore = "live: spawns covenantd + the covenant-mcp-fake-server subprocess; opt-in via --ignored live_"]
async fn live_ipc_mcp_external_tool_dispatches_through_daemon() {
    let Some(fake_server) = fake_server_bin() else {
        eprintln!(
            "covenant-mcp-fake-server not built; skipping external-MCP daemon dispatch live test \
             (build it or set COVENANT_LIVE_MCP_FAKE_SERVER)"
        );
        return;
    };

    let home = tempfile::tempdir().expect("tempdir");

    // Register the fake server as an external MCP server before the daemon
    // starts. The daemon parses this at startup (main.rs), spawns it as a
    // subprocess, and folds its tools into the registry under
    // `mcp_<tool_prefix>_<upstream>` names.
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

    // ── The external server's tools surface in the daemon registry.
    //    This is the proof that config -> spawn -> bootstrap -> register ran;
    //    a server skipped by the warn-and-skip arms would leave these absent.
    match req(&mut stream, Request::ListTools).await {
        Response::ToolList { tools } => {
            let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
            for expected in ["mcp_fake_ping", "mcp_fake_sum", "mcp_fake_fail"] {
                assert!(
                    names.contains(&expected),
                    "external MCP tool {expected} must be registered through the daemon, got {names:?}"
                );
            }
        }
        other => panic!("ListTools must return a ToolList, got {other:?}"),
    }

    // The `tool.call.<name>` gate applies to every caller, the operator
    // included (a bare call is denied first, as live_http_tools_call.rs pins),
    // so grant each external tool to the operator before invoking it. An
    // unscoped-arguments grant (`tool` only, no `arguments.allow`) authorizes
    // any arguments for that tool.
    for tool in ["mcp_fake_ping", "mcp_fake_sum"] {
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

    // ── A text tool round-trips through the real subprocess. The exact echo
    //    rules out a stub: the fake server replies `pong: <text>`.
    match req(
        &mut stream,
        Request::CallTool {
            name: "mcp_fake_ping".into(),
            arguments: serde_json::json!({ "text": "covenant" }),
        },
    )
    .await
    {
        Response::ToolResult { content, is_error } => {
            assert!(!is_error, "mcp_fake_ping must not be an error result");
            match content.first().expect("ping returned content") {
                Content::Text { text } => assert_eq!(
                    text, "pong: covenant",
                    "daemon must route to the real subprocess, not a stub"
                ),
                other => panic!("unexpected ping content: {other:?}"),
            }
        }
        other => panic!("mcp_fake_ping must return a ToolResult, got {other:?}"),
    }

    // ── A JSON tool round-trips with the server-computed value. `2 + 5 == 7`
    //    is produced inside the fake server, so a matching result proves the
    //    arguments crossed the transport and the response came back.
    match req(
        &mut stream,
        Request::CallTool {
            name: "mcp_fake_sum".into(),
            arguments: serde_json::json!({ "a": 2, "b": 5 }),
        },
    )
    .await
    {
        Response::ToolResult { content, is_error } => {
            assert!(!is_error, "mcp_fake_sum must not be an error result");
            match content.first().expect("sum returned content") {
                Content::Json { value } => assert_eq!(
                    value["sum"], 7,
                    "daemon must surface the subprocess-computed sum"
                ),
                other => panic!("unexpected sum content: {other:?}"),
            }
        }
        other => panic!("mcp_fake_sum must return a ToolResult, got {other:?}"),
    }

    let _ = child.kill().await;
}
