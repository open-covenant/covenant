//! Live integration test: the operator capability-usage query reports each
//! grant's own scope verbatim, distinct across two grants that share one action,
//! sourced through the real grant path and read back from the durable grant
//! ledger across a restart.
//!
//! The in-process unit tests construct grant state directly and prove the scope
//! cannot transpose between two grants for one action. This drives the other half
//! through the real daemon: unlike the subject — which `GrantCapability` always
//! assigns from the authenticated peer, so a live grant is held by a single
//! identity — the operator chooses each grant's scope, so the live path can issue
//! two `tool.call.echo` grants with distinct scopes and pin anti-transposition end
//! to end. `tool.call.echo` keeps the `tool` field bound to the action suffix, so
//! the two scopes differ on an `arguments.allow` constraint; that difference also
//! gives the grants distinct ed25519 signatures, the query's join key.
//!
//! The query must report each entry's own scope verbatim, joined on the signature,
//! so a scope dropped, defaulted, or swapped between the two grants through the
//! real grant→record→query round-trip is caught.
//!
//! A kill/respawn against the same HOME re-asserts both scopes, showing they are
//! read from the durable grant ledger and not an in-memory value a restart would
//! reset.
//!
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_capability_usage_scope_visibility -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, CapabilityUsageEntry, Request, Response};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
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

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
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

async fn authenticate(stream: &mut UnixStream, home: &Path) {
    let token_b58 = read_operator_token(home).await;
    match req(stream, Request::Authenticate { token_b58 }).await {
        Response::Authenticated { .. } => {}
        other => panic!("authenticate failed: {other:?}"),
    }
}

fn spawn_daemon(home: &Path, port: u16) -> Child {
    let exe = env!("CARGO_BIN_EXE_covenantd");
    Command::new(exe)
        .env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd")
}

/// Grants `tool.call.echo` with `scope` as the authenticated operator and returns
/// the grant's signature — the usage-query join key.
async fn grant_scoped(stream: &mut UnixStream, scope: serde_json::Value) -> String {
    match req(
        stream,
        Request::GrantCapability {
            action: "tool.call.echo".into(),
            scope: Some(scope),
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted {
            signature_b58,
            action,
            ..
        } => {
            assert_eq!(action, "tool.call.echo");
            signature_b58
        }
        other => panic!("grant tool.call.echo failed: {other:?}"),
    }
}

async fn usage_snapshot(stream: &mut UnixStream) -> Vec<CapabilityUsageEntry> {
    match req(stream, Request::CapabilityUsage).await {
        Response::CapabilityUsage { grants } => grants,
        other => panic!("expected CapabilityUsage, got {other:?}"),
    }
}

/// The grant identified by `signature_b58` — the ed25519 join key — or a panic if
/// the snapshot does not carry it.
fn entry_for<'a>(grants: &'a [CapabilityUsageEntry], signature_b58: &str) -> &'a CapabilityUsageEntry {
    grants
        .iter()
        .find(|g| g.signature_b58 == signature_b58)
        .unwrap_or_else(|| panic!("grant {signature_b58} must appear in the usage snapshot"))
}

#[tokio::test]
#[ignore = "live: spawns covenantd twice + verifies each capability-usage entry reports its own grant scope verbatim, distinct across two grants for one action, and survives restart"]
async fn live_covenantd_capability_usage_scope_visibility_across_restart() {
    let home = tempfile::tempdir().expect("tempdir");
    let sock = home.path().join("sock");

    // Two distinct, both-valid scopes for ONE action. `validate_tool_scope` pins
    // the `tool` field to the `tool.call.echo` suffix, so the scopes differ on an
    // `arguments.allow` constraint — enough to give them distinct signatures and
    // to catch a scope that swaps or collapses between the two grants.
    let scope_alpha = serde_json::json!({ "version": 1, "tool": "echo" });
    let scope_beta = serde_json::json!({
        "version": 1,
        "tool": "echo",
        "arguments": { "allow": { "text": "ping" } }
    });
    assert_ne!(scope_alpha, scope_beta);

    let sig_alpha;
    let sig_beta;

    // ── Phase 1: grant both as the operator and confirm each usage entry reports
    //     its own signed scope verbatim, with the scopes not transposed.
    {
        let mut child = spawn_daemon(home.path(), pick_free_port());
        if !wait_for_sock(&sock).await {
            let _ = child.kill().await;
            panic!("daemon #1 never created its socket at {}", sock.display());
        }
        let mut stream = UnixStream::connect(&sock).await.expect("connect #1");
        authenticate(&mut stream, home.path()).await;

        sig_alpha = grant_scoped(&mut stream, scope_alpha.clone()).await;
        sig_beta = grant_scoped(&mut stream, scope_beta.clone()).await;
        // Distinct scopes for one action sign to distinct grants — the join key
        // the query must keep them apart by.
        assert_ne!(
            sig_alpha, sig_beta,
            "two scopes for one action produce distinct signatures",
        );

        let grants = usage_snapshot(&mut stream).await;
        assert_scope(entry_for(&grants, &sig_alpha), &scope_alpha);
        assert_scope(entry_for(&grants, &sig_beta), &scope_beta);
        assert_not_transposed(&grants, &sig_alpha, &sig_beta);

        drop(stream);
        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    // SIGKILL leaves the socket file behind; remove it so wait_for_sock after
    // daemon #2 is a real readiness signal, not the stale file from #1.
    let _ = std::fs::remove_file(&sock);

    // ── Phase 2: respawn against the same HOME. Each scope must be re-read from the
    //     durable grant ledger, so both are identical and still not transposed — an
    //     in-memory scope would not survive the restart.
    {
        let mut child = spawn_daemon(home.path(), pick_free_port());
        if !wait_for_sock(&sock).await {
            let _ = child.kill().await;
            panic!("daemon #2 never created its socket at {}", sock.display());
        }
        let mut stream = UnixStream::connect(&sock).await.expect("connect #2");
        authenticate(&mut stream, home.path()).await;

        let grants = usage_snapshot(&mut stream).await;
        assert_scope(entry_for(&grants, &sig_alpha), &scope_alpha);
        assert_scope(entry_for(&grants, &sig_beta), &scope_beta);
        assert_not_transposed(&grants, &sig_alpha, &sig_beta);

        drop(stream);
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

/// The entry must report the exact scope the grant was signed with — not merely a
/// present-but-defaulted value — so a scope dropped or mis-sourced through the real
/// grant path is caught.
fn assert_scope(entry: &CapabilityUsageEntry, expected: &serde_json::Value) {
    assert_eq!(
        &entry.scope, expected,
        "the usage entry reports the grant's signed scope verbatim",
    );
    // Guard the "present but defaulted/null" failure mode directly: a real
    // tool.call.echo scope keeps the tool bound to the action suffix.
    assert_eq!(
        entry.scope.get("tool").and_then(|v| v.as_str()),
        Some("echo"),
        "the scope is the real signed scope, not a dropped or defaulted value",
    );
}

/// The two grants share one action, so a join keyed on the action — rather than the
/// signature — would swap or collapse their scopes. Assert each keeps its own.
fn assert_not_transposed(grants: &[CapabilityUsageEntry], sig_alpha: &str, sig_beta: &str) {
    assert_ne!(
        entry_for(grants, sig_alpha).scope,
        entry_for(grants, sig_beta).scope,
        "scopes stay bound to their own grant rather than transposing onto the shared action",
    );
}
