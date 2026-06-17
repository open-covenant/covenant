//! Live integration test: the operator capability-usage query attributes each
//! grant to the agent that holds it (`subject_display` + `subject_pubkey_b58`),
//! sourced through the real grant path and read back from the durable grant
//! ledger across a restart.
//!
//! The in-process unit tests construct grant state directly and prove the
//! subject cannot transpose between two distinct holders (alice vs bob) or
//! collapse onto the grantor. This drives the other half through the real
//! daemon: `Request::GrantCapability` assigns the subject from the
//! authenticated peer, so a grant issued by the operator is held by the
//! operator's own identity — the daemon identity persisted at
//! `<home>/identity/local.key`. The query must then report that identity as the
//! grant's subject, by display and by base58 pubkey, joined on the ed25519
//! signature. (Subject and grantor coincide on this path, so distinct-subject
//! transposition stays the unit tests' job; the live value is that the subject
//! survives the real grant→record→query round-trip without being dropped,
//! zeroed, or detached from the authenticated identity.)
//!
//! A kill/respawn against the same HOME re-asserts the subject, showing it is
//! read from the durable grant ledger and the reloaded identity, not an
//! in-memory value a restart would reset.
//!
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_capability_usage_subject_attribution -- --ignored live_`.

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

/// Authenticates and returns the display the daemon bound to the connection —
/// the authoritative human label of the identity every operator grant is
/// attributed to.
async fn authenticate(stream: &mut UnixStream, home: &Path) -> String {
    let token_b58 = read_operator_token(home).await;
    match req(stream, Request::Authenticate { token_b58 }).await {
        Response::Authenticated { display } => display,
        other => panic!("authenticate failed: {other:?}"),
    }
}

/// The operator's base58 pubkey, derived from the same persisted signing key the
/// daemon loads — the authoritative reference for the subject the grant ledger
/// must report. Read after the socket appears, so the key file is fully written.
fn operator_pubkey_b58(home: &Path) -> String {
    let key_path = home.join("identity").join("local.key");
    let identity = covenant_identity::LocalIdentity::load_or_create(&key_path, "user@local")
        .expect("load persisted daemon identity");
    covenant_types::AgentId::new(identity.display(), identity.pubkey_bytes()).pubkey_base58()
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

/// Grants `tool.call.echo` as the authenticated operator and returns the
/// grant's signature (the usage-query join key) and the subject display the
/// grant response itself reports.
async fn grant_echo(stream: &mut UnixStream) -> (String, String) {
    match req(
        stream,
        Request::GrantCapability {
            action: "tool.call.echo".into(),
            scope: Some(serde_json::json!({ "version": 1, "tool": "echo" })),
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted {
            signature_b58,
            subject_display,
            action,
        } => {
            assert_eq!(action, "tool.call.echo");
            (signature_b58, subject_display)
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

/// The grant identified by `signature_b58` — the ed25519 join key — or a panic
/// if the snapshot does not carry it.
fn entry_for<'a>(grants: &'a [CapabilityUsageEntry], signature_b58: &str) -> &'a CapabilityUsageEntry {
    grants
        .iter()
        .find(|g| g.signature_b58 == signature_b58)
        .unwrap_or_else(|| panic!("grant {signature_b58} must appear in the usage snapshot"))
}

#[tokio::test]
#[ignore = "live: spawns covenantd twice + verifies the capability-usage entry attributes the grant to the authenticated operator's identity and survives restart"]
async fn live_covenantd_capability_usage_subject_attribution_across_restart() {
    let home = tempfile::tempdir().expect("tempdir");
    let sock = home.path().join("sock");

    let signature_b58;
    let operator_display;
    let operator_pubkey;

    // ── Phase 1: grant as the operator and confirm the usage entry attributes
    //     the grant to that authenticated identity, by display and by pubkey.
    {
        let mut child = spawn_daemon(home.path(), pick_free_port());
        if !wait_for_sock(&sock).await {
            let _ = child.kill().await;
            panic!("daemon #1 never created its socket at {}", sock.display());
        }
        let mut stream = UnixStream::connect(&sock).await.expect("connect #1");
        operator_display = authenticate(&mut stream, home.path()).await;
        operator_pubkey = operator_pubkey_b58(home.path());

        let (sig, granted_subject_display) = grant_echo(&mut stream).await;
        signature_b58 = sig;
        // The grant path attributes the subject to the authenticated peer, so the
        // grant response already names the operator as the holder.
        assert_eq!(
            granted_subject_display, operator_display,
            "GrantCapability attributes the subject to the authenticated operator",
        );

        assert_subject(
            entry_for(&usage_snapshot(&mut stream).await, &signature_b58),
            &operator_display,
            &operator_pubkey,
        );

        drop(stream);
        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    // SIGKILL leaves the socket file behind; remove it so wait_for_sock after
    // daemon #2 is a real readiness signal, not the stale file from #1.
    let _ = std::fs::remove_file(&sock);

    // ── Phase 2: respawn against the same HOME. The subject must be re-read from
    //     the durable grant ledger and the reloaded identity, so it is identical —
    //     an in-memory attribution would not survive the restart.
    {
        let mut child = spawn_daemon(home.path(), pick_free_port());
        if !wait_for_sock(&sock).await {
            let _ = child.kill().await;
            panic!("daemon #2 never created its socket at {}", sock.display());
        }
        let mut stream = UnixStream::connect(&sock).await.expect("connect #2");
        let display_after = authenticate(&mut stream, home.path()).await;
        assert_eq!(
            display_after, operator_display,
            "the reloaded identity keeps the same display across restart",
        );

        assert_subject(
            entry_for(&usage_snapshot(&mut stream).await, &signature_b58),
            &operator_display,
            &operator_pubkey,
        );

        drop(stream);
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

fn assert_subject(entry: &CapabilityUsageEntry, expected_display: &str, expected_pubkey_b58: &str) {
    assert_eq!(
        entry.subject_display, expected_display,
        "the usage entry attributes the grant to the granting identity's display",
    );
    assert_eq!(
        entry.subject_pubkey_b58, expected_pubkey_b58,
        "the usage entry attributes the grant to the granting identity's pubkey",
    );
    // Guard the "dropped or zeroed" failure mode directly: a subject lost through
    // the real grant path would surface as an empty or all-zero pubkey rather
    // than the live 32-byte ed25519 key.
    assert!(
        !entry.subject_display.is_empty(),
        "the subject display is populated",
    );
    let pubkey_bytes = bs58::decode(&entry.subject_pubkey_b58)
        .into_vec()
        .expect("subject pubkey is base58");
    assert_eq!(pubkey_bytes.len(), 32, "subject pubkey is a 32-byte ed25519 key");
    assert!(
        pubkey_bytes.iter().any(|&b| b != 0),
        "subject pubkey is not the zeroed identity",
    );
}
