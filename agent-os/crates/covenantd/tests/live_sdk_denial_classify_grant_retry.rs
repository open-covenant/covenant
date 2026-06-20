//! Live integration test: spawns covenantd against a tempdir HOME and drives the
//! `covenant-sdk` `Client` through its flagship denied -> classify -> grant ->
//! retry loop against the *real* daemon.
//!
//! The SDK crate's unit tests exercise every `Client` method and the `denial()`
//! classifier against a *fake* daemon (`Harness`) whose denial messages are
//! hardcoded fixtures. Because the SDK borrows the daemon's `Request`/`Response`
//! types at compile time, the wire shapes cannot drift — but the SDK's
//! string-parsing of the daemon's *actual* denial wording
//! (`parse_required_capability`, anchored on `covenant capabilities grant
//! <action>`) is only ever checked against those fixtures. If the daemon's
//! tool-call denial message ever drops the grant hint, the unit fixtures keep
//! passing while the real loop silently breaks and `grant_capability` gets fed
//! the wrong action.
//!
//! This test pins that boundary end-to-end: an un-granted operator calling
//! `echo` must surface as `SdkError::Denied { capability: Some("tool.call.echo"),
//! kind: MissingCapability }` — proving the SDK classified the real daemon's
//! denial — and the subsequent self-grant + retry must echo the text.
//!
//! Hermetic — no external services, no tool execution beyond the built-in echo.
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_sdk_denial_classify_grant_retry -- --ignored live_`.

use covenant_sdk::{Client, Content, DenialKind, SdkError};
use serde_json::json;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
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

async fn wait_for_operator_token(home: &Path) {
    let path = home.join("peers").join("operator.token");
    for _ in 0..100 {
        if let Ok(s) = std::fs::read_to_string(&path) {
            if !s.trim().is_empty() {
                return;
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
    wait_for_operator_token(home).await;
    child
}

#[tokio::test]
#[ignore = "live: spawns covenantd + drives covenant-sdk Client through denied/grant/retry"]
async fn live_sdk_denial_classify_grant_retry_against_real_daemon() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;

    let mut client = Client::connect_with_token_file(home.path())
        .await
        .expect("SDK connects + authenticates against the real daemon");

    // The real wire round-trip parses, and the built-in echo is discoverable.
    let tools = client.list_tools().await.expect("list_tools");
    assert!(
        tools.iter().any(|t| t.name == "echo"),
        "echo must be advertised: {:?}",
        tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>()
    );

    // The flagship pin: the operator holds no capabilities (the trust root is
    // not bypassed), so calling echo is denied — and the SDK must CLASSIFY the
    // real daemon's denial message, extracting exactly "tool.call.echo". The
    // unit tests cannot catch a drift between the daemon's wording and this
    // parser; only the live round-trip can.
    let err = client
        .call_tool("echo", json!({ "text": "hi" }))
        .await
        .expect_err("un-granted call_tool must be denied");
    match err {
        SdkError::Denied {
            capability: Some(action),
            kind: DenialKind::MissingCapability,
            ..
        } if action == "tool.call.echo" => {}
        other => panic!(
            "expected Denied{{capability:Some(\"tool.call.echo\"), MissingCapability}} \
             against the real daemon wording, got {other:?}"
        ),
    }

    // The action extracted above is the exact argument grant_capability wants,
    // so the self-grant closes the loop and the retry echoes the text.
    client
        .grant_capability("tool.call.echo", None, None)
        .await
        .expect("operator self-grants tool.call.echo");

    let out = client
        .call_tool("echo", json!({ "text": "hi" }))
        .await
        .expect("granted call_tool succeeds");
    assert!(!out.is_error, "post-grant echo must not be a tool error");
    assert_eq!(out.content, vec![Content::text("hi")]);

    drop(client);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
