//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::IgnoreCheck` over the raw IPC socket, asserting the daemon's
//! `Response::IgnoreReport` redacts a credential path and admits benign text
//! against the same boot-seeded ruleset.
//!
//! The verb is covered today over the CLI (`live_cli_ignore_check_json.rs`,
//! `live_cli_ignore_dispatch.rs`) but never over the raw Unix socket the CLI is
//! built on. This pins that wire contract — the `Response::IgnoreReport`
//! variant (`ignored`, `matched_pattern`, `rules_loaded`;
//! covenant-ipc/src/lib.rs:839) and the content-redaction floor a fresh daemon
//! ships with: on first boot it seeds `default_ignorefile()` (which carries
//! `**/id_rsa*` and `**/.ssh/**`, covenantd/src/main.rs:758/:760) and loads it,
//! so a credential path is ignored and a benign intent is not.
//!
//! Both cases run on one authenticated connection so they exercise the same
//! loaded ruleset. `rules_loaded` is asserted `> 0` rather than to the exact
//! seeded count, to avoid coupling the test to the rule total.
//!
//! Hermetic — the ruleset is seeded into the tempdir and matched offline.
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_ignore_check -- --ignored live_`.

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
#[ignore = "live: spawns covenantd + drives Request::IgnoreCheck over the socket for a credential path and a benign intent"]
async fn live_ipc_ignore_check_redacts_credential_path_and_admits_benign_text() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    // A credential path must be redacted by the boot-seeded ruleset. A dropped
    // ruleset (rules_loaded: 0) or a narrowed glob would silently let the daemon
    // ingest ~/.ssh/id_rsa, so the report must mark it ignored with a concrete
    // matched pattern.
    match req(
        &mut stream,
        Request::IgnoreCheck {
            text: "summarise ~/.ssh/id_rsa".into(),
        },
    )
    .await
    {
        Response::IgnoreReport {
            ignored,
            matched_pattern,
            rules_loaded,
        } => {
            assert!(
                ignored,
                "a credential path must be ignored by the default ruleset"
            );
            assert!(
                rules_loaded > 0,
                "the daemon must boot with a loaded ignore ruleset, got {rules_loaded}"
            );
            let pattern = matched_pattern.expect("an ignored path must report the matched pattern");
            assert!(
                !pattern.trim().is_empty(),
                "the matched pattern must be a concrete rule, not blank: {pattern:?}"
            );
        }
        other => panic!("expected Response::IgnoreReport, got {other:?}"),
    }

    // Benign intent text must pass the same loaded ruleset untouched.
    match req(
        &mut stream,
        Request::IgnoreCheck {
            text: "summarise public roadmap".into(),
        },
    )
    .await
    {
        Response::IgnoreReport {
            ignored,
            matched_pattern,
            rules_loaded,
        } => {
            assert!(!ignored, "benign intent text must not be ignored");
            assert!(
                matched_pattern.is_none(),
                "an admitted intent must not report a matched pattern: {matched_pattern:?}"
            );
            assert!(
                rules_loaded > 0,
                "the ruleset stays loaded across requests, got {rules_loaded}"
            );
        }
        other => panic!("expected Response::IgnoreReport, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
