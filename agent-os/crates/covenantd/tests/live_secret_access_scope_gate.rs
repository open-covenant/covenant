//! Live integration test: the secret broker's name-scope gate binds a
//! `secret.access` grant to its named secret through the real daemon socket.
//!
//! `get_secret` layers two gates: a capability gate (`secret.access`) and,
//! behind it, a name-scope gate that admits only the secret name the grant's
//! scope names (`{"version":1,"name":"X"}` admits only `X`). The scope gate runs
//! **before** the not-configured guard, so an out-of-scope denial is auditable
//! even on a daemon with no secret backend wired — which is the production
//! daemon, because `secret_source` is wired only via the in-process
//! `Server::with_secret_source` builder, never from env or disk. A live request
//! therefore always stops at the not-configured guard *after* the scope gate.
//!
//! This pins, end to end over the socket:
//!  * a peer with no `secret.access` grant is refused by the capability gate;
//!  * an OUT-OF-SCOPE name under a name-scoped grant is refused by the scope gate
//!    ("outside this grant's scope") and records a `SecretAccessDenied` audit row
//!    naming the secret — never its value;
//!  * the IN-SCOPE name passes the scope gate and reaches the distinct
//!    "not configured" guard, proving both the name binding and the
//!    scope-gate-before-not-configured ordering.
//!
//! The behavior is unit-covered in-process
//! (`get_secret_enforces_name_scope_and_audits_access`, with a `MapSecretSource`)
//! but never through the real socket. A regression that bypassed the name-scope
//! gate would leak an out-of-scope secret the moment a backend is wired; one that
//! moved the not-configured guard ahead of the scope gate would erase the
//! `SecretAccessDenied` accountability row for every out-of-scope attempt.
//!
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_secret_access_scope_gate -- --ignored live_`.

use covenant_audit::{AuditEvent, AuditKind};
use covenant_ipc::{read_frame, write_frame, Request, Response};
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

async fn get_secret(stream: &mut UnixStream, name: &str) -> Response {
    req(stream, Request::GetSecret { name: name.into() }).await
}

async fn recent_audit(stream: &mut UnixStream) -> Vec<AuditEvent> {
    match req(
        stream,
        Request::RecentAudit {
            limit: 100,
            since_ms: None,
            prefer_stream: None,
        },
    )
    .await
    {
        Response::AuditEvents { events } => events,
        other => panic!("expected AuditEvents, got {other:?}"),
    }
}

/// Most recent `SecretAccessDenied` row, as `(secret_name, reason)`.
fn latest_secret_denied(events: &[AuditEvent]) -> Option<(String, String)> {
    events.iter().rev().find_map(|e| match &e.kind {
        AuditKind::SecretAccessDenied {
            secret_name,
            reason,
            ..
        } => Some((secret_name.clone(), reason.clone())),
        _ => None,
    })
}

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies the secret-broker name-scope gate over the socket"]
async fn live_covenantd_secret_access_name_scope_gate() {
    const IN_SCOPE: &str = "db_password";
    const OUT_OF_SCOPE: &str = "api_key";

    let home = tempfile::tempdir().expect("tempdir");
    let sock = home.path().join("sock");

    let mut child = spawn_daemon(home.path(), pick_free_port());
    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon never created its socket at {}", sock.display());
    }
    let mut stream = UnixStream::connect(&sock).await.expect("connect");
    authenticate(&mut stream, home.path()).await;

    // ── Phase 1: with no secret.access grant the capability gate refuses the
    //     request before the scope or not-configured paths can run. The message
    //     names the missing capability and is distinct from the scope-denial and
    //     not-configured messages below, so a skipped capability gate would not
    //     produce it.
    match get_secret(&mut stream, IN_SCOPE).await {
        Response::Error { message } => {
            assert!(
                message.contains("requires capability") && message.contains("secret.access"),
                "an ungranted peer must be refused by the capability gate: {message}"
            );
            assert!(
                !message.contains("outside this grant's scope")
                    && !message.contains("not configured"),
                "the capability denial must not reach the scope or not-configured paths: {message}"
            );
        }
        other => panic!("expected a capability-gate Error, got {other:?}"),
    }

    // ── Phase 2: grant secret.access scoped to exactly one secret name. The
    //     subject is the authenticated operator, so list_for_subject finds it at
    //     dispatch time.
    match req(
        &mut stream,
        Request::GrantCapability {
            action: "secret.access".into(),
            scope: Some(serde_json::json!({ "version": 1, "name": IN_SCOPE })),
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { .. } => {}
        other => panic!("granting name-scoped secret.access failed: {other:?}"),
    }

    // ── Phase 3: an OUT-OF-SCOPE name under the name-scoped grant clears the
    //     capability gate but is refused by the scope gate. This is the secret
    //     non-leak invariant: a grant scoped to `db_password` cannot read
    //     `api_key`. The refusal must name the scope ("outside this grant's
    //     scope"), not fall through to the not-configured guard — which is what a
    //     bypassed scope gate (or one reordered behind the not-configured guard)
    //     would do.
    match get_secret(&mut stream, OUT_OF_SCOPE).await {
        Response::Error { message } => {
            assert!(
                message.contains("outside this grant's scope"),
                "an out-of-scope secret must be refused by the name-scope gate: {message}"
            );
            assert!(
                !message.contains("not configured"),
                "the scope denial must fire before the not-configured guard: {message}"
            );
        }
        other => panic!("expected a scope-gate Error for the out-of-scope name, got {other:?}"),
    }

    // The out-of-scope attempt is recorded by NAME on the operator's feed even
    // though no backend is wired — the scope gate runs ahead of the
    // not-configured guard precisely so the denial is auditable. A reorder that
    // returned "not configured" first would drop this row, erasing the
    // accountability trail for a scope-violation attempt. Phase 1's capability
    // denial does not write a SecretAccessDenied row, so this is the only one.
    let events = recent_audit(&mut stream).await;
    let (denied_name, reason) =
        latest_secret_denied(&events).expect("the out-of-scope read must record SecretAccessDenied");
    assert_eq!(
        denied_name, OUT_OF_SCOPE,
        "the audit row must name the out-of-scope secret that was refused"
    );
    assert!(
        reason.contains("outside this grant's scope"),
        "the audit reason must record the scope violation: {reason}"
    );
    // The broker never released a secret, so no SecretAccessGranted row exists —
    // the gate is not a blanket allow that merely failed downstream.
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.kind, AuditKind::SecretAccessGranted { .. })),
        "no secret was released, so no SecretAccessGranted row may appear: {events:?}"
    );

    // ── Phase 4: the IN-SCOPE name clears BOTH the capability gate and the
    //     name-scope gate, so it reaches the not-configured guard and returns the
    //     DISTINCT "not configured" message. That this name gets a different
    //     refusal than the out-of-scope name in Phase 3 is the positive control:
    //     the scope gate admitted the bound name (so the gate discriminates by
    //     name, not a blanket deny) and the not-configured guard sits AFTER it.
    match get_secret(&mut stream, IN_SCOPE).await {
        Response::Error { message } => {
            assert!(
                message.contains("not configured"),
                "the in-scope name must pass the scope gate and reach the not-configured guard: \
                 {message}"
            );
            assert!(
                !message.contains("outside this grant's scope")
                    && !message.contains("requires capability"),
                "the in-scope name must not be refused by the capability or scope gate: {message}"
            );
        }
        other => panic!("expected a not-configured Error for the in-scope name, got {other:?}"),
    }

    // ── Phase 5: the denials are scoped to the verb, not the connection — a bare
    //     Ping still round-trips.
    match req(&mut stream, Request::Ping).await {
        Response::Pong => {}
        other => panic!("session must stay usable after the denials, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
