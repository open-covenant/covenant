//! Live HTTP coverage for `POST /intents/resume` (`intents_resume`).
//!
//! The `intents resume` verb is live-tested over the CLI by
//! `live_cli_intents_resume.rs` and the IPC budget plumbing is pinned by
//! `live_budget_enforcement.rs`, but no test drives the resume over the HTTP
//! gateway. `resume_intent` has no capability gate — its security boundary is
//! the peer-pubkey audit filter (you can only resume your own
//! `BudgetExhausted` rows) — and it claims the saved pause checkpoint exactly
//! once before re-dispatching. Both properties are exercised here over HTTP.
//!
//! A resumable intent is created hermetically: a research agent manifest
//! pinned to `budget_credits_per_hour = 1` is staged, one intent is dispatched
//! to drain the single credit, and a second is rejected — minting the
//! `BudgetExhausted` audit row and its checkpoint.
//!
//! Two scenarios:
//! - locate + claim-once: the operator reads the `BudgetExhausted` row's
//!   `intent_id` from `GET /audit/recent`, then `POST /intents/resume`. The
//!   first resume locates and claims the checkpoint, re-dispatches, and hits
//!   the still-empty bucket (error names `budget`, NOT "no BudgetExhausted
//!   audit row"). A second resume of the same id is refused with "already
//!   claimed" — proving the claim is consumed once, over HTTP.
//! - not found: a fresh daemon rejects `POST /intents/resume` for a random id
//!   with "no BudgetExhausted audit row".
//!
//! Hermetic — the research agent falls back to canned text when no providers
//! are configured. `#[ignore]`'d. Each test uses its own tempdir. Build
//! prereq: `cargo build -p research-agent`. Run with
//! `cargo test -p covenantd --test live_http_intents_resume -- --ignored live_`.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::time::sleep;
use uuid::Uuid;

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

/// Stage a research-agent manifest pinned to a single budget credit so the
/// first dispatch drains the bucket and the second is rejected. Must run
/// before the daemon spawns — agent budgets are registered at startup.
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
budget_credits_per_hour = 1
"#;
    std::fs::write(agents_dir.join("agent.toml"), manifest).expect("write manifest");
}

async fn spawn_http_daemon(home: &Path) -> (Child, String) {
    let port = pick_free_port();
    let base = format!("http://127.0.0.1:{port}");
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let child = Command::new(exe)
        .env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        // The budget projection tick would otherwise fire mid-dispatch and
        // SIGKILL the first, validly-admitted single-credit run before it can
        // exhaust the budget. Push its period past the test window so it does
        // not race the admission path under test (see live_budget_enforcement).
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

async fn submit_intent(client: &reqwest::Client, base: &str, text: &str) -> Value {
    client
        .post(format!("{base}/intent"))
        .json(&json!({ "text": text }))
        .send()
        .await
        .expect("post intent")
        .json()
        .await
        .expect("intent body")
}

async fn post_resume(client: &reqwest::Client, base: &str, intent_id: &str) -> Value {
    client
        .post(format!("{base}/intents/resume"))
        .json(&json!({ "intent_id": intent_id }))
        .send()
        .await
        .expect("post intents resume")
        .json()
        .await
        .expect("intents resume body")
}

/// Read `GET /audit/recent` and return the `intent_id` of the single
/// `BudgetExhausted` row the rejected dispatch minted.
async fn budget_exhausted_intent_id(client: &reqwest::Client, base: &str) -> String {
    let audit: Value = client
        .get(format!("{base}/audit/recent?limit=100"))
        .send()
        .await
        .expect("get audit recent")
        .json()
        .await
        .expect("audit recent body");
    audit["events"]
        .as_array()
        .expect("audit carries an events array")
        .iter()
        .find(|event| {
            event.pointer("/kind/type").and_then(Value::as_str) == Some("budget_exhausted")
        })
        .and_then(|event| event.pointer("/kind/intent_id").and_then(Value::as_str))
        .map(String::from)
        .unwrap_or_else(|| panic!("no budget_exhausted audit row: {audit:?}"))
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts POST /intents/resume locates, claims, and refuses a second claim over HTTP"]
async fn live_http_intents_resume_locates_and_claims_checkpoint_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    stage_research_agent(home.path());
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    grant(&client, &base, "tool.web_search").await;
    grant(&client, &base, "memory.write").await;

    let first = submit_intent(&client, &base, "find recent papers on agent memory").await;
    assert_eq!(
        first["kind"], "intent_result",
        "the first dispatch must drain the single credit, not be rejected: {first:?}",
    );
    assert_eq!(
        first["status"], "ok",
        "the first dispatch must report ok: {first:?}",
    );

    let second = submit_intent(&client, &base, "find another batch of papers").await;
    assert_eq!(
        second["kind"], "error",
        "the second dispatch must be budget-rejected: {second:?}",
    );
    assert!(
        second["message"]
            .as_str()
            .unwrap_or_default()
            .contains("budget exhausted"),
        "the second dispatch must name budget exhaustion: {second:?}",
    );

    let intent_id = budget_exhausted_intent_id(&client, &base).await;

    // First resume: locates the row, claims the checkpoint, re-dispatches, and
    // hits the still-empty bucket. The error must name `budget` (the redispatch
    // failure) and NOT "no BudgetExhausted audit row" (which would mean resume
    // never found the checkpoint).
    let resumed = post_resume(&client, &base, &intent_id).await;
    assert_eq!(
        resumed["kind"], "error",
        "resume re-dispatch must hit the still-empty bucket: {resumed:?}",
    );
    let resumed_message = resumed["message"].as_str().unwrap_or_default();
    assert!(
        resumed_message.contains("budget"),
        "resume must re-dispatch and fail on budget, proving it located the row: {resumed:?}",
    );
    assert!(
        !resumed_message.contains("no BudgetExhausted audit row"),
        "resume must locate the operator's own checkpoint over HTTP: {resumed:?}",
    );

    // Second resume of the same id: the checkpoint was consumed by the first
    // claim, so the daemon refuses to re-dispatch it again.
    let again = post_resume(&client, &base, &intent_id).await;
    assert_eq!(
        again["kind"], "error",
        "a second resume of a claimed checkpoint must be refused: {again:?}",
    );
    assert!(
        again["message"]
            .as_str()
            .unwrap_or_default()
            .contains("already claimed"),
        "the claim-once guard must reject the duplicate resume over HTTP: {again:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts POST /intents/resume rejects an unknown intent id over HTTP"]
async fn live_http_intents_resume_unknown_intent_is_rejected_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    let unknown = Uuid::new_v4().to_string();
    let rejected = post_resume(&client, &base, &unknown).await;
    assert_eq!(
        rejected["kind"], "error",
        "resuming an unknown intent must be rejected: {rejected:?}",
    );
    assert!(
        rejected["message"]
            .as_str()
            .unwrap_or_default()
            .contains("no BudgetExhausted audit row"),
        "the rejection must name the missing checkpoint: {rejected:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
