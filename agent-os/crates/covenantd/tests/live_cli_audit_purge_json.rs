//! Live CLI coverage for `covenant audit purge --json`.

use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::process::Command;
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

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_string()).collect()
}

async fn run_cli_output(
    cli_exe: &std::path::Path,
    home: &std::path::Path,
    args: &[String],
) -> std::process::Output {
    Command::new(cli_exe)
        .args(args)
        .env("COVENANT_HOME", home)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI")
}

async fn run_cli_ok(cli_exe: &std::path::Path, home: &std::path::Path, args: &[String]) -> String {
    let output = run_cli_output(cli_exe, home, args).await;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "CLI failed for {args:?}: status={:?} stdout={stdout:?} stderr={stderr:?}",
        output.status
    );
    assert!(
        stderr.trim().is_empty(),
        "CLI command {args:?} must not emit stderr on success: {stderr:?}"
    );
    stdout
}

#[tokio::test]
#[ignore = "live: spawns covenantd + runs `covenant audit purge --json` subprocess"]
async fn live_cli_audit_purge_json_round_trip() {
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

    let marker = format!("audit-purge-scope-marker-{}", std::process::id());
    run_cli_ok(
        &cli_exe,
        home.path(),
        &args(&["capabilities", "grant", "tool.call.echo"]),
    )
    .await;
    let echo_args = serde_json::json!({ "text": marker }).to_string();
    let mut echo_cmd = args(&["tools", "call", "echo", "--args"]);
    echo_cmd.push(echo_args);
    echo_cmd.push("--json".into());
    let echo_stdout = run_cli_ok(&cli_exe, home.path(), &echo_cmd).await;
    let echo_value: Value =
        serde_json::from_str(echo_stdout.trim()).expect("echo --json must be valid JSON");
    assert_eq!(echo_value["kind"].as_str(), Some("tool_result"));

    let audit_file = home.path().join("audit").join("events.jsonl");
    let before_contents = std::fs::read_to_string(&audit_file).expect("read audit events");
    assert!(
        before_contents.contains(&marker),
        "expected audit log to contain echo marker before purge attempts"
    );

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("unix time")
        .as_millis() as u64;
    let allowed_before_ms = now_ms;
    let denied_before_ms = now_ms + 30_000;

    let scope = format!(r#"{{"version":1,"before_ms":{}}}"#, allowed_before_ms);
    let mut grant_purge = args(&["capabilities", "grant", "audit.purge", "--scope"]);
    grant_purge.push(scope);
    grant_purge.push("--json".into());
    run_cli_ok(&cli_exe, home.path(), &grant_purge).await;

    let mut denied_cmd = args(&["audit", "purge", "--before-ms"]);
    denied_cmd.push(denied_before_ms.to_string());
    denied_cmd.push("--json".into());
    let denied_output = run_cli_output(&cli_exe, home.path(), &denied_cmd).await;

    let denied_stdout = String::from_utf8_lossy(&denied_output.stdout).to_string();
    let denied_stderr = String::from_utf8_lossy(&denied_output.stderr).to_string();
    assert!(
        !denied_output.status.success(),
        "audit purge should fail when before_ms exceeds granted scope: status={:?} stdout={denied_stdout:?} stderr={denied_stderr:?}",
        denied_output.status
    );

    let after_denied_contents =
        std::fs::read_to_string(&audit_file).expect("read audit events after denied purge");
    assert!(
        after_denied_contents.contains(&marker),
        "denied audit purge must not delete existing audit rows"
    );

    let mut allowed_cmd = args(&["audit", "purge", "--before-ms"]);
    allowed_cmd.push(allowed_before_ms.to_string());
    allowed_cmd.push("--json".into());
    let stdout = run_cli_ok(&cli_exe, home.path(), &allowed_cmd).await;

    let value: Value =
        serde_json::from_str(stdout.trim()).expect("audit purge --json must be valid JSON");
    assert_eq!(value["kind"].as_str(), Some("audit_purged"));
    assert_eq!(value["before_ms"].as_u64(), Some(allowed_before_ms));

    let after_contents =
        std::fs::read_to_string(&audit_file).expect("read audit events after purge");
    assert!(
        !after_contents.contains(&marker),
        "allowed audit purge should remove the marker row"
    );

    let _ = child.kill().await;
}
