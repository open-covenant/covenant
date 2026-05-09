//! Live integration test for `covenant capabilities revoke`.
//!
//! Hermetic and opt-in: spawns a real daemon, grants one fixture
//! capability through the CLI, revokes the returned signature through the
//! CLI, then confirms the live capability set is empty.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
}

fn covenant_cli_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/covenant")
        .canonicalize()
        .expect("covenant CLI binary not built; run `cargo build -p covenant` first")
}

fn signature_from_grant(stdout: &str) -> &str {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("signature: "))
        .expect("grant stdout must contain a signature line")
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

async fn wait_for_operator_token(home: &std::path::Path) {
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

#[tokio::test]
#[ignore = "live: spawns covenantd + runs `covenant capabilities revoke` subprocess"]
async fn live_cli_capabilities_revoke_round_trip() {
    let home = tempfile::tempdir().expect("tempdir");
    let port = pick_free_port();
    let daemon_exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(daemon_exe)
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
    wait_for_operator_token(home.path()).await;

    let cli_exe = covenant_cli_bin();
    let action = "tool.call.echo";
    let grant = Command::new(&cli_exe)
        .arg("capabilities")
        .arg("grant")
        .arg(action)
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI (grant)");
    let grant_stdout = String::from_utf8_lossy(&grant.stdout);
    let grant_stderr = String::from_utf8_lossy(&grant.stderr);
    assert!(
        grant.status.success(),
        "grant CLI failed: status={:?} stdout={grant_stdout:?} stderr={grant_stderr:?}",
        grant.status
    );
    let signature = signature_from_grant(&grant_stdout);

    let revoke = Command::new(&cli_exe)
        .arg("capabilities")
        .arg("revoke")
        .arg(signature)
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI (revoke)");
    let revoke_stdout = String::from_utf8_lossy(&revoke.stdout);
    let revoke_stderr = String::from_utf8_lossy(&revoke.stderr);
    assert!(
        revoke.status.success(),
        "revoke CLI failed: status={:?} stdout={revoke_stdout:?} stderr={revoke_stderr:?}",
        revoke.status
    );
    assert!(
        revoke_stdout.contains(&format!("revoked: {signature}")),
        "revoke stdout did not confirm signature; stdout={revoke_stdout:?} stderr={revoke_stderr:?}"
    );

    let recent = Command::new(&cli_exe)
        .arg("capabilities")
        .arg("recent")
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI (capabilities recent)");
    let recent_stdout = String::from_utf8_lossy(&recent.stdout);
    let recent_stderr = String::from_utf8_lossy(&recent.stderr);
    assert!(
        recent.status.success(),
        "recent CLI failed: status={:?} stdout={recent_stdout:?} stderr={recent_stderr:?}",
        recent.status
    );
    assert!(
        recent_stdout.contains("(no capabilities granted)"),
        "revoked capability still visible; stdout={recent_stdout:?} stderr={recent_stderr:?}"
    );
    assert!(
        !recent_stdout.contains(action),
        "revoked action still visible; stdout={recent_stdout:?} stderr={recent_stderr:?}"
    );

    let _ = child.kill().await;
}
