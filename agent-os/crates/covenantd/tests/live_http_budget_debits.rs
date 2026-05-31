//! Live HTTP coverage for `GET /budget/debits` (`budget_debits`).
//!
//! `recent_debits` returns the `BudgetDebit` rows produced when a budgeted
//! agent is dispatched. It has unit coverage, but no test drives
//! `/budget/debits` over the HTTP gateway, leaving the `Debits` wire variant
//! (`kind: "debits"`, `debits[]`) and the dispatch→ledger accounting
//! unexercised on the HTTP path. A debit is recorded at pre-spawn admission
//! only when an agent actually matches and is dispatched — a no-match intent
//! records none — so the seed needs a real budgeted agent.
//!
//! Mirrors `live_budget_enforcement`: stages the `research` agent binary plus a
//! manifest pinning `budget_credits_per_hour` into the daemon's agents dir,
//! then dispatches one matching intent over HTTP and reads the ledger. The
//! research agent falls back to canned output when no model is configured, so
//! the dispatch completes hermetically with no network.
//!
//! Prerequisite: the `research` binary must be built first
//! (`cargo build -p research-agent`); the test stages `target/debug/research`.
//! `#[ignore]`'d, own tempdir per test for `--test-threads` safety. Run with
//! `cargo build -p research-agent` then
//! `cargo test -p covenantd --test live_http_budget_debits -- --ignored live_`.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
}

fn research_agent_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/research")
        .canonicalize()
        .expect("research binary not built; run `cargo build -p research-agent` first")
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
        if let Ok(text) = std::fs::read_to_string(&path) {
            let token = text.trim();
            if !token.is_empty() {
                return token.to_string();
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("operator token never appeared at {}", path.display());
}

async fn wait_for_http(base: &str) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .expect("reqwest client");
    for _ in 0..80 {
        match client.get(format!("{base}/health")).send().await {
            Ok(response) if response.status().is_success() => return,
            _ => sleep(Duration::from_millis(50)).await,
        }
    }
    panic!("http gateway never became healthy at {base}/health");
}

/// Stage the prebuilt research agent binary plus a manifest pinning a generous
/// per-hour budget so a single dispatch is comfortably admitted (recording one
/// debit) without tripping budget exhaustion.
fn stage_research_agent(home: &Path) {
    let agents_dir = home.join("agents").join("research");
    std::fs::create_dir_all(&agents_dir).expect("agents dir");
    let staged = agents_dir.join("research");
    std::fs::copy(research_agent_bin(), &staged).expect("stage research binary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&staged).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&staged, perms).unwrap();
    }
    let manifest = r#"
[agent]
id = "research"
name = "Research Agent"
version = "0.0.1"
runtime = "rust-bin"
entry = "research"

[capabilities]
required = ["tool.web_search"]

[settlement]
budget_credits_per_hour = 100
"#;
    std::fs::write(agents_dir.join("agent.toml"), manifest).expect("write manifest");
}

async fn spawn_http_daemon(home: &Path) -> (Child, String) {
    stage_research_agent(home);
    let port = pick_free_port();
    let base = format!("http://127.0.0.1:{port}");
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let child = Command::new(exe)
        .env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        // Keep the budget projection tick (hard-preempt) out of the test window
        // so it never races the single admitted dispatch under test.
        .env("COVENANT_BUDGET_PROJECTION_TICK_MS", "3600000")
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
    wait_for_http(&base).await;
    (child, base)
}

async fn operator_client(home: &Path) -> reqwest::Client {
    let token = read_operator_token(home).await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .expect("reqwest client")
}

async fn grant(client: &reqwest::Client, base: &str, action: &str) {
    let granted: Value = client
        .post(format!("{base}/capabilities/grant"))
        .json(&json!({ "action": action }))
        .send()
        .await
        .expect("send capability grant")
        .json()
        .await
        .expect("grant body json");
    assert_eq!(
        granted["kind"], "capability_granted",
        "grant {action} must succeed: {granted:?}",
    );
}

async fn debits(client: &reqwest::Client, base: &str) -> Value {
    client
        .get(format!("{base}/budget/debits"))
        .send()
        .await
        .expect("get budget/debits")
        .json()
        .await
        .expect("budget/debits body")
}

#[tokio::test]
#[ignore = "live: spawns covenantd with a budgeted research agent + asserts GET /budget/debits records the dispatch debit over HTTP"]
async fn live_http_budget_debits_records_dispatch_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    let baseline = debits(&client, &base).await;
    assert_eq!(
        baseline["kind"], "debits",
        "the operator read must answer the debits variant: {baseline:?}",
    );
    assert_eq!(
        baseline["debits"].as_array().map(|d| d.len()),
        Some(0),
        "a fresh daemon has dispatched nothing, so the ledger must be empty: {baseline:?}",
    );

    // The research agent requires tool.web_search; the dispatch stores a result
    // memory, which requires memory.write.
    grant(&client, &base, "tool.web_search").await;
    grant(&client, &base, "memory.write").await;

    let intent: Value = client
        .post(format!("{base}/intent"))
        .json(&json!({ "text": "find recent papers on agent memory" }))
        .send()
        .await
        .expect("post intent")
        .json()
        .await
        .expect("intent body");
    assert_eq!(
        intent["kind"], "intent_result",
        "the matching intent must dispatch to the budgeted agent: {intent:?}",
    );
    assert_eq!(
        intent["status"], "ok",
        "the research agent must run to completion (canned output offline): {intent:?}",
    );

    let ledger = debits(&client, &base).await;
    assert_eq!(
        ledger["kind"], "debits",
        "the post-dispatch read must still answer the debits variant: {ledger:?}",
    );
    let rows = ledger["debits"].as_array().expect("debits array");
    assert_eq!(
        rows.len(),
        1,
        "exactly one admitted dispatch must record exactly one debit: {ledger:?}",
    );
    let row = &rows[0];
    assert_eq!(
        row["agent"]["display"], "research@agent",
        "the debit must be attributed to the dispatched agent: {row:?}",
    );
    assert!(
        row["credits"].as_u64().is_some_and(|c| c > 0),
        "the debit must carry a positive credit cost: {row:?}",
    );
    assert!(
        row["at_ms"].as_u64().is_some_and(|t| t > 0),
        "the debit must carry a wall-clock timestamp: {row:?}",
    );
    assert!(
        row["paired_receipt"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "the debit must carry the paired settlement receipt id: {row:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
