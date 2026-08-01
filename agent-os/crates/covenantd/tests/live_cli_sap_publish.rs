//! Live CLI coverage for the parked `covenant sap publish` compatibility
//! command and its local manifest validation. The daemon is pointed at a
//! success worker; the CLI must surface the stable parked error without a
//! receipt. Build the CLI first (`cargo build -p covenant`), then run with
//! `cargo test -p covenantd --test live_cli_sap_publish -- --ignored live_`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().unwrap().port()
}

fn covenant_cli_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/covenant")
        .canonicalize()
        .expect("covenant CLI binary not built; run `cargo build -p covenant` first")
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
    for _ in 0..50 {
        if let Ok(s) = std::fs::read_to_string(&path) {
            if !s.trim().is_empty() {
                return;
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
    let daemon_exe = env!("CARGO_BIN_EXE_covenantd");
    let mut cmd = Command::new(daemon_exe);
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
    wait_for_operator_token(home).await;
    child
}

/// Spawn covenantd with the SAP bridge enabled and pointed at an executable
/// shell stub in `home` that drains stdin and prints a fixed success envelope.
async fn spawn_with_stub_worker(home: &Path) -> Child {
    let stub = home.join("sap-worker.sh");
    std::fs::write(
        &stub,
        "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"ok\":true,\"data\":{\"agentPda\":\"AgentPda111\",\"signature\":\"PublishSig222\"}}'\n",
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

/// Run `covenant sap publish <extra...>` against the daemon at `home`,
/// returning (success, stdout, stderr).
async fn run_publish(cli: &Path, home: &Path, extra: &[&str]) -> (bool, String, String) {
    let mut args = vec!["sap", "publish"];
    args.extend_from_slice(extra);
    let output = Command::new(cli)
        .args(&args)
        .env("COVENANT_HOME", home)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

#[tokio::test]
#[ignore = "live: proves `covenant sap publish` surfaces the parked daemon boundary"]
async fn live_cli_sap_publish_surfaces_parked_boundary() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_with_stub_worker(home.path()).await;
    let cli = covenant_cli_bin();

    let manifest = home.path().join("manifest.json");
    std::fs::write(
        &manifest,
        r#"{"name":"covenant-demo","capabilities":[],"pricing":[],"protocols":["a2a"]}"#,
    )
    .expect("write manifest");
    let mpath = manifest.to_str().expect("manifest path utf8");

    let (ok, stdout, stderr) =
        run_publish(&cli, home.path(), &["--manifest", mpath, "--json"]).await;
    assert!(
        !ok,
        "parked sap publish --json unexpectedly succeeded: stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.contains(covenantd::SAP_DIRECT_PUBLISH_PARKED),
        "CLI must preserve the daemon's parked reason: {stderr:?}"
    );

    let (ok, stdout, stderr) = run_publish(&cli, home.path(), &["--manifest", mpath]).await;
    assert!(
        !ok,
        "parked sap publish unexpectedly succeeded: stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.contains(covenantd::SAP_DIRECT_PUBLISH_PARKED),
        "CLI must preserve the daemon's parked reason: {stderr:?}"
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts `covenant sap publish` rejects a malformed manifest file locally, before the daemon"]
async fn live_cli_sap_publish_rejects_malformed_manifest_file() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_with_stub_worker(home.path()).await;
    let cli = covenant_cli_bin();

    let manifest = home.path().join("bad-manifest.json");
    std::fs::write(&manifest, "{ not json").expect("write manifest");
    let mpath = manifest.to_str().expect("manifest path utf8");

    let (ok, stdout, stderr) = run_publish(&cli, home.path(), &["--manifest", mpath]).await;
    assert!(
        !ok,
        "a malformed manifest file must fail: stdout={stdout:?} stderr={stderr:?}"
    );
    // The CLI's parse-roundtrip rejects the file locally with a `parse manifest`
    // context. A regression that dropped it would instead surface the daemon's
    // own `daemon error: invalid manifest JSON`, so we pin both directions.
    assert!(
        stderr.contains("parse manifest"),
        "the CLI must reject the bad file locally with a parse-manifest context: {stderr:?}"
    );
    assert!(
        !stderr.contains("daemon error"),
        "the bad file must never reach the daemon — local pre-validation should reject first: {stderr:?}"
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
