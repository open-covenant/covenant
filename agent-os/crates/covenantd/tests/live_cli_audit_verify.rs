//! Live integration test for `covenant audit verify`.
//!
//! Hermetic and opt-in: spawns a real daemon against a temporary
//! `COVENANT_HOME`, seeds one audit row through the real CLI, then runs
//! the audit-integrity command through the same CLI/IPC/auth boundary.

use serde_json::Value;
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
#[ignore = "live: spawns covenantd + runs `covenant audit verify` subprocess"]
async fn live_cli_audit_verify_round_trip() {
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
    let grant = Command::new(&cli_exe)
        .arg("capabilities")
        .arg("grant")
        .arg("tool.call.echo")
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

    let verify = Command::new(&cli_exe)
        .arg("audit")
        .arg("verify")
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI (audit verify)");
    let stdout = String::from_utf8_lossy(&verify.stdout);
    let stderr = String::from_utf8_lossy(&verify.stderr);
    assert!(
        verify.status.success(),
        "audit verify CLI failed: status={:?} stdout={stdout:?} stderr={stderr:?}",
        verify.status
    );

    let report: Value =
        serde_json::from_slice(&verify.stdout).expect("audit verify stdout must be JSON");
    assert_eq!(report["valid"], true, "report={report:?}");
    assert!(
        report["events"].as_u64().unwrap_or(0) > 0,
        "report={report:?}"
    );
    assert_eq!(report["events"], report["anchors"], "report={report:?}");
    assert_eq!(
        report["root_hash_hex"].as_str().unwrap_or_default().len(),
        64,
        "report={report:?}"
    );
    assert_eq!(report["failures"].as_array().map(Vec::len), Some(0));
    assert!(home
        .path()
        .join("audit")
        .join("events.chain.jsonl")
        .exists());

    let verify_json = Command::new(&cli_exe)
        .arg("audit")
        .arg("verify")
        .arg("--json")
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI (audit verify --json)");
    let json_stdout = String::from_utf8_lossy(&verify_json.stdout);
    let json_stderr = String::from_utf8_lossy(&verify_json.stderr);
    assert!(
        verify_json.status.success(),
        "audit verify --json CLI failed: status={:?} stdout={json_stdout:?} stderr={json_stderr:?}",
        verify_json.status
    );

    let envelope: Value = serde_json::from_slice(&verify_json.stdout)
        .expect("audit verify --json stdout must be JSON");
    assert_eq!(envelope["kind"], "audit_integrity", "envelope={envelope:?}");
    let report = &envelope["report"];
    assert_eq!(report["valid"], true, "envelope={envelope:?}");
    assert!(
        report["events"].as_u64().unwrap_or(0) > 0,
        "envelope={envelope:?}"
    );
    assert_eq!(report["events"], report["anchors"], "envelope={envelope:?}");

    let _ = child.kill().await;
}
