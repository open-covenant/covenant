//! Live integration coverage for the parked `SapPublishAuditRoot`
//! compatibility request. It pins the same stable refusal with the bridge on
//! or off and before root validation or worker invocation. Hermetic and
//! ignored; run with
//! `cargo test -p covenantd --test live_ipc_sap_publish_audit_root -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use std::os::unix::fs::PermissionsExt;
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

/// Spawn covenantd against `home`, clearing inherited SAP/cluster env so the
/// bridge config is host-independent, then applying `env` overrides.
async fn spawn_daemon(home: &Path, env: &[(&str, &str)]) -> Child {
    let port = pick_free_port();
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let mut cmd = Command::new(exe);
    cmd.env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home)
        .env_remove("COVENANT_SOLANA_CLUSTER")
        .env_remove("COVENANT_SAP_ENABLED")
        .env_remove("COVENANT_SAP_DEVNET_ENABLED")
        .env_remove("COVENANT_SAP_KEYPAIR");
    for (key, value) in env {
        cmd.env(key, value);
    }
    let child = cmd
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

/// Spawn with an enabled bridge and a worker that would return success if the
/// parked daemon boundary accidentally invoked it.
async fn spawn_with_stub_worker(home: &Path) -> Child {
    let stub = home.join("sap-worker.sh");
    std::fs::write(
        &stub,
        "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"ok\":true,\"data\":{\"ledgerPda\":\"LedgerPda111\",\"signature\":\"AuditRootSig222\"}}'\n",
    )
    .expect("write stub worker");
    let mut perms = std::fs::metadata(&stub)
        .expect("stat stub worker")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&stub, perms).expect("chmod stub worker");

    spawn_daemon(
        home,
        &[
            ("COVENANT_SAP_ENABLED", "true"),
            (
                "COVENANT_SAP_WORKER_CMD",
                stub.to_str().expect("stub path utf8"),
            ),
        ],
    )
    .await
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

/// A canonical 64-char lowercase-hex root that clears `validate_root_hash_hex`.
fn valid_root_hash_hex() -> String {
    "ab".repeat(32)
}

fn audit_root_request(root_hash_hex: String) -> Request {
    Request::SapPublishAuditRoot {
        root_hash_hex,
        release_target: "covenant".into(),
        release_subject: "witness-loop".into(),
        release_scope: "audit".into(),
    }
}

#[tokio::test]
#[ignore = "live: proves SapPublishAuditRoot is parked before an enabled success worker"]
async fn live_ipc_sap_publish_audit_root_is_parked_before_worker_invocation() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_with_stub_worker(home.path()).await;

    let mut stream = authenticated_stream(home.path()).await;
    match req(&mut stream, audit_root_request(valid_root_hash_hex())).await {
        Response::Error { message } => {
            assert_eq!(message, covenantd::SAP_DIRECT_PUBLISH_PARKED)
        }
        other => panic!("expected parked Response::Error, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: proves SapPublishAuditRoot returns the same parked error with SAP disabled"]
async fn live_ipc_sap_publish_audit_root_is_parked_before_bridge_state() {
    let home = tempfile::tempdir().expect("tempdir");
    // No COVENANT_SAP_ENABLED: main.rs still wires the bridge (lib.rs:310), so
    // require_enabled is what rejects, not the unit-test-only "not wired" arm.
    let mut child = spawn_daemon(home.path(), &[]).await;

    let mut stream = authenticated_stream(home.path()).await;
    match req(&mut stream, audit_root_request(valid_root_hash_hex())).await {
        Response::Error { message } => {
            assert_eq!(message, covenantd::SAP_DIRECT_PUBLISH_PARKED);
        }
        other => panic!("expected parked Response::Error, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: proves SapPublishAuditRoot is parked before root validation"]
async fn live_ipc_sap_publish_audit_root_parks_before_root_validation() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_with_stub_worker(home.path()).await;

    // "zz" is the right length but non-hex; require_enabled passes (bridge on)
    // so validate_root_hash_hex is the gate that rejects, before the stub spawns.
    let mut stream = authenticated_stream(home.path()).await;
    match req(&mut stream, audit_root_request("zz".repeat(32))).await {
        Response::Error { message } => {
            assert_eq!(message, covenantd::SAP_DIRECT_PUBLISH_PARKED);
        }
        other => panic!("expected parked Response::Error, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
