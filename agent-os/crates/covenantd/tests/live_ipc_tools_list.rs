//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::ListTools` over the raw IPC socket, asserting the daemon returns a
//! `Response::ToolList` that includes the built-in `echo` tool and that every
//! `ToolSpec` is well-formed (non-empty name and description, object schema).
//!
//! The verb is covered today over the CLI (`live_cli_tools_list_json.rs`) but
//! never over the raw Unix socket the CLI is built on. This pins that wire
//! contract — the tool inventory MCP clients discover before they call a tool.
//!
//! Hermetic — no external services, no tool execution. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_tools_list -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().unwrap().port()
}

async fn wait_for_sock(path: &Path) -> bool {
    for _ in 0..100 {
        if path.exists() {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn read_operator_token(home: &Path) -> String {
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

async fn spawn_daemon(home: &Path) -> Child {
    let port = pick_free_port();
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let child = Command::new(exe)
        .env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");
    if !wait_for_sock(&home.join("sock")).await {
        panic!("daemon never created its socket");
    }
    child
}

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

async fn authenticated_stream(home: &Path) -> UnixStream {
    let mut stream = UnixStream::connect(home.join("sock"))
        .await
        .expect("connect socket");
    let token = read_operator_token(home).await;
    match req(&mut stream, Request::Authenticate { token_b58: token }).await {
        Response::Authenticated { .. } => {}
        other => panic!("authenticate failed: {other:?}"),
    }
    stream
}

#[tokio::test]
#[ignore = "live: spawns covenantd + queries Request::ListTools over the socket"]
async fn live_ipc_tools_list_includes_builtin_echo() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;

    let mut stream = authenticated_stream(home.path()).await;
    let tools = match req(&mut stream, Request::ListTools).await {
        Response::ToolList { tools } => tools,
        other => panic!("expected Response::ToolList, got {other:?}"),
    };

    assert!(
        tools.iter().any(|t| t.name == "echo"),
        "the tool inventory must include the built-in echo tool: {:?}",
        tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>()
    );
    for tool in &tools {
        assert!(!tool.name.is_empty(), "every tool must carry a name");
        assert!(
            !tool.description.is_empty(),
            "tool {} must carry a description",
            tool.name
        );
        assert!(
            tool.input_schema.is_object(),
            "tool {} input_schema must be a JSON object (empty object = no arguments)",
            tool.name
        );
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
