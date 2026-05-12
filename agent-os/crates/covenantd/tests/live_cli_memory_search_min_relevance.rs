//! Live CLI coverage for `covenant memory search --min-relevance`.
//!
//! Submits two intents whose texts differ entirely. The deterministic
//! mock embedder used by default produces identical vectors for the
//! same input text, so cosine(query, exact_match) ≈ 1.0 and cosine
//! against an unrelated record sits near 0. A `--min-relevance 0.5`
//! threshold must keep the exact-match record and drop the unrelated
//! one before the limit is applied.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
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

async fn run_cli(cli_exe: &Path, home: &Path, args: &[&str]) -> String {
    let output = Command::new(cli_exe)
        .args(args)
        .env("COVENANT_HOME", home)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "CLI failed for {args:?}: status={:?} stdout={stdout:?} stderr={stderr:?}",
        output.status
    );
    stdout
}

async fn run_cli_expect_failure(cli_exe: &Path, home: &Path, args: &[&str]) -> String {
    let output = Command::new(cli_exe)
        .args(args)
        .env("COVENANT_HOME", home)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI");
    assert!(
        !output.status.success(),
        "CLI must reject {args:?} but exited 0: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stderr).to_string()
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts memory search --min-relevance 0.5 keeps exact-match record"]
async fn live_cli_memory_search_min_relevance_drops_below_threshold() {
    let home = tempfile::tempdir().expect("tempdir");

    // Force the deterministic mock embedder so the cosine outcome
    // does not depend on a host's Ollama installation (which would
    // make two semantically similar texts both score above 0.5 and
    // turn this test into a flaky pass/fail). The mock seeds an LCG
    // from an FNV-1a hash, so two distinct inputs produce
    // uncorrelated 768-dim vectors with cosine ≈ 0.
    std::fs::write(
        home.path().join("secrets.toml"),
        b"[embed]\nprovider = \"mock\"\n",
    )
    .expect("write secrets.toml");

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
    run_cli(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.read"],
    )
    .await;
    run_cli(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.write"],
    )
    .await;

    // Submit one intent whose stored text the search query will match
    // verbatim (cosine ≈ 1.0 under the mock embedder) plus a handful
    // of distractors whose stored texts hash to uncorrelated vectors
    // (cosine ≈ N(0, ~0.04) under a 768-dim mock). The router prefixes
    // stored memory records with "phase 0 echo (no agent matched): "
    // when no agent picks the intent up.
    let target_intent = "minrelevance-target-record-fixture";
    let distractors = [
        "unrelated-distractor-aaa",
        "another-distractor-bbb",
        "yet-another-distractor-ccc",
        "fourth-distractor-ddd",
    ];
    run_cli(&cli_exe, home.path(), &["intent", target_intent]).await;
    for d in distractors {
        run_cli(&cli_exe, home.path(), &["intent", d]).await;
    }

    let stored_target = format!("phase 0 echo (no agent matched): {target_intent}");

    let unfiltered_stdout = run_cli(
        &cli_exe,
        home.path(),
        &[
            "memory",
            "search",
            &stored_target,
            "--tier",
            "working",
            "--limit",
            "20",
            "--json",
        ],
    )
    .await;
    let unfiltered: Value =
        serde_json::from_str(unfiltered_stdout.trim()).expect("unfiltered JSON");
    let unfiltered_texts: Vec<String> = unfiltered["records"]
        .as_array()
        .expect("records array")
        .iter()
        .filter_map(|r| r["text"].as_str().map(String::from))
        .collect();
    assert!(
        unfiltered_texts.iter().any(|t| t == &stored_target),
        "exact-match target must appear without the relevance filter; got {unfiltered_texts:?}",
    );
    assert!(
        unfiltered["min_relevance"].is_null(),
        "min_relevance must be null when the flag is absent; got {:?}",
        unfiltered["min_relevance"]
    );

    let filtered_stdout = run_cli(
        &cli_exe,
        home.path(),
        &[
            "memory",
            "search",
            &stored_target,
            "--tier",
            "working",
            "--limit",
            "20",
            "--min-relevance",
            "0.5",
            "--json",
        ],
    )
    .await;
    let filtered: Value = serde_json::from_str(filtered_stdout.trim()).expect("filtered JSON");
    assert_eq!(filtered["kind"], "memory_read");
    let filtered_floor = filtered["min_relevance"]
        .as_f64()
        .expect("min_relevance must be a number under the filter");
    assert!(
        (filtered_floor - 0.5).abs() < 1e-6,
        "min_relevance must round-trip the threshold; got {filtered_floor}",
    );
    let filtered_texts: Vec<String> = filtered["records"]
        .as_array()
        .expect("records array")
        .iter()
        .filter_map(|r| r["text"].as_str().map(String::from))
        .collect();
    assert!(
        filtered_texts.iter().any(|t| t == &stored_target),
        "exact-match target must survive the 0.5 threshold; got {filtered_texts:?}",
    );
    // Mock-embedded distractors carry cosines ≈ N(0, ~0.04) against
    // the target query, so all of them sit far below the 0.5 floor.
    // The filter must therefore reduce the result set to the single
    // exact-match row.
    let distractor_survivors: Vec<&String> = filtered_texts
        .iter()
        .filter(|t| *t != &stored_target)
        .collect();
    assert!(
        distractor_survivors.is_empty(),
        "distractors with near-zero cosine must be dropped by the 0.5 threshold; survivors: {distractor_survivors:?}",
    );

    for bad in ["-0.1", "1.5", "abc"] {
        let stderr = run_cli_expect_failure(
            &cli_exe,
            home.path(),
            &[
                "memory",
                "search",
                "any query",
                "--min-relevance",
                bad,
                "--json",
            ],
        )
        .await;
        assert!(
            stderr.contains("--min-relevance"),
            "out-of-range / non-numeric {bad:?} must be rejected with a --min-relevance error; got {stderr:?}",
        );
    }

    let _ = child.kill().await;
}
