//! Live CLI coverage for `covenant audit purge --json`.

use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
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

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_string()).collect()
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

fn audit_events(value: &Value) -> &[Value] {
    value["events"]
        .as_array()
        .expect("audit recent events must be an array")
}

fn audit_event_ids(value: &Value) -> Vec<String> {
    audit_events(value)
        .iter()
        .map(|event| {
            event["id"]
                .as_str()
                .expect("audit event id must be a string")
                .to_owned()
        })
        .collect()
}

fn capability_event_id(value: &Value, action: &str) -> String {
    audit_events(value)
        .iter()
        .find(|event| {
            event["kind"]["type"].as_str() == Some("capability_granted")
                && event["kind"]["action"].as_str() == Some(action)
        })
        .and_then(|event| event["id"].as_str())
        .unwrap_or_else(|| panic!("missing capability_granted row for {action}: {value:?}"))
        .to_owned()
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
    run_cli_ok(
        &cli_exe,
        home.path(),
        &args(&["capabilities", "grant", "tool.call.echo"]),
    )
    .await;
    let seed_recent_stdout = run_cli_ok(
        &cli_exe,
        home.path(),
        &args(&["audit", "recent", "--limit", "20", "--json"]),
    )
    .await;
    let seed_recent: Value =
        serde_json::from_str(seed_recent_stdout.trim()).expect("audit recent JSON after seed");
    let seed_id = capability_event_id(&seed_recent, "tool.call.echo");

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("unix time")
        .as_millis() as u64;
    let allowed_before_ms = now_ms + 1_000;
    let denied_before_ms = now_ms + 30_000;

    let scope = format!(r#"{{"version":1,"before_ms":{allowed_before_ms}}}"#);
    let mut grant_purge = args(&["capabilities", "grant", "audit.purge", "--scope"]);
    grant_purge.push(scope);
    grant_purge.push("--json".into());
    run_cli_ok(&cli_exe, home.path(), &grant_purge).await;

    let before_recent_stdout = run_cli_ok(
        &cli_exe,
        home.path(),
        &args(&["audit", "recent", "--limit", "20", "--json"]),
    )
    .await;
    let before_recent: Value =
        serde_json::from_str(before_recent_stdout.trim()).expect("audit recent JSON before purge");
    let before_ids = audit_event_ids(&before_recent);
    assert!(
        !before_ids.is_empty(),
        "test must seed audit rows before rejected purge: {before_recent:?}"
    );

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
    assert!(
        denied_stdout.trim().is_empty(),
        "failed purge must not emit success JSON: {denied_stdout:?}"
    );
    assert!(
        denied_stderr.contains("daemon error")
            && denied_stderr.contains("capability scope")
            && denied_stderr.contains(&format!("before_ms {denied_before_ms}")),
        "failed purge should classify the scope rejection: {denied_stderr:?}"
    );

    let after_recent_stdout = run_cli_ok(
        &cli_exe,
        home.path(),
        &args(&["audit", "recent", "--limit", "20", "--json"]),
    )
    .await;
    let after_recent: Value =
        serde_json::from_str(after_recent_stdout.trim()).expect("audit recent JSON after reject");
    let after_events = audit_events(&after_recent);
    for id in &before_ids {
        assert!(
            after_events
                .iter()
                .any(|event| event["id"].as_str() == Some(id.as_str())),
            "rejected purge must preserve pre-existing audit row {id}: {after_recent:?}"
        );
    }
    assert!(
        after_events.iter().any(|event| {
            event["kind"]["type"].as_str() == Some("capability_scope_rejected")
                && event["kind"]["action"].as_str() == Some("audit.purge")
                && event["kind"]["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains(&format!("before_ms {denied_before_ms}")))
        }),
        "rejected purge should record an audit scope rejection: {after_recent:?}"
    );

    let mut allowed_cmd = args(&["audit", "purge", "--before-ms"]);
    allowed_cmd.push(allowed_before_ms.to_string());
    allowed_cmd.push("--json".into());
    let stdout = run_cli_ok(&cli_exe, home.path(), &allowed_cmd).await;

    let value: Value =
        serde_json::from_str(stdout.trim()).expect("audit purge --json must be valid JSON");
    assert_eq!(value["kind"].as_str(), Some("audit_purged"));
    assert_eq!(value["before_ms"].as_u64(), Some(allowed_before_ms));
    assert!(
        value["purged"].as_u64().is_some_and(|purged| purged > 0),
        "allowed audit purge should remove at least one seeded row: {value:?}"
    );

    let after_allowed_stdout = run_cli_ok(
        &cli_exe,
        home.path(),
        &args(&["audit", "recent", "--limit", "20", "--json"]),
    )
    .await;
    let after_allowed: Value = serde_json::from_str(after_allowed_stdout.trim())
        .expect("audit recent JSON after allowed purge");
    assert!(
        !audit_event_ids(&after_allowed).contains(&seed_id),
        "allowed purge should remove seeded audit row {seed_id}: {after_allowed:?}"
    );

    let _ = child.kill().await;
}
