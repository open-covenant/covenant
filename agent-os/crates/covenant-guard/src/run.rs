//! The orchestrator: start the proxy, sandbox the agent, watch spend and the
//! clock, tear the process group down if either is exceeded, then write the
//! signed receipt. The guard is the parent process here — outside the sandbox
//! it puts the agent in — which is what lets it hold the credential, meter the
//! spend, and pull the plug.

use crate::chain::Chain;
use crate::cli::RunConfig;
use crate::ledger::Ledger;
use crate::proxy::Proxy;
use crate::receipt::{self, FileChange, ModelLine, ReceiptCore, Tokens};
use crate::sandbox::{self, SandboxLayout};
use covenant_identity::LocalIdentity;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;

/// What the run produced, enough for the caller to print and exit.
pub struct Outcome {
    pub receipt_dir: std::path::PathBuf,
    pub exit_code: i32,
    pub summary_json: String,
}

fn send_signal(pgid: i32, sig: i32) {
    // Negative pid signals the whole process group. The agent set its own group
    // at exec, so this reaches everything it spawned without touching the guard.
    unsafe {
        libc::kill(-pgid, sig);
    }
}

/// Collect the workspace diff for the receipt: tracked edits with line counts,
/// plus any new untracked files. Best-effort — a non-git workspace yields none.
fn workspace_changes(workspace: &Path) -> Vec<FileChange> {
    let mut out = Vec::new();
    let numstat = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["--no-optional-locks", "diff", "--numstat", "HEAD"])
        .output();
    if let Ok(o) = numstat {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let mut parts = line.split('\t');
            let added = parts.next().unwrap_or("0");
            let removed = parts.next().unwrap_or("0");
            if let Some(path) = parts.next() {
                out.push(FileChange {
                    path: path.to_string(),
                    added: added.parse().unwrap_or(0),
                    removed: removed.parse().unwrap_or(0),
                });
            }
        }
    }
    let untracked = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["--no-optional-locks", "ls-files", "--others", "--exclude-standard"])
        .output();
    if let Ok(o) = untracked {
        for path in String::from_utf8_lossy(&o.stdout).lines() {
            if path.is_empty() {
                continue;
            }
            // A new file has no diff base; count its lines as additions so the
            // receipt reads like a diff rather than showing +0.
            let added = std::fs::read(workspace.join(path))
                .map(|b| b.iter().filter(|&&c| c == b'\n').count() as i64 + if b.last() == Some(&b'\n') || b.is_empty() { 0 } else { 1 })
                .unwrap_or(0);
            out.push(FileChange { path: path.to_string(), added, removed: 0 });
        }
    }
    out
}

/// Write the hook settings file the agent is pointed at, so hook wiring never
/// touches the agent's own config. Returns the path.
fn write_hook_settings(dir: &Path, hook_url: &str) -> std::io::Result<std::path::PathBuf> {
    let cmd = format!("curl -s -m 2 -X POST --data-binary @- {hook_url} >/dev/null 2>&1 || true");
    let one = |event: &str| {
        serde_json::json!([{ "matcher": "", "hooks": [{ "type": "command", "command": cmd }], "_event": event }])
    };
    let settings = serde_json::json!({
        "hooks": { "PreToolUse": one("PreToolUse"), "Stop": one("Stop") }
    });
    let path = dir.join("settings.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&settings)?)?;
    Ok(path)
}

/// Run one guarded agent invocation to completion.
pub async fn run(cfg: RunConfig) -> anyhow::Result<Outcome> {
    let run_id = uuid::Uuid::new_v4().to_string();
    let home = crate::home_dir();
    let state_dir = home.join("runs").join(&run_id);
    let session_tmp = state_dir.join("tmp");
    let receipt_dir = home.join("receipts").join(&run_id);
    let secrets_dir = home.join("secrets");
    std::fs::create_dir_all(&session_tmp)?;
    std::fs::create_dir_all(&receipt_dir)?;

    // The signing key is the one true secret — the sandbox denies the agent
    // read access to `secrets/` and nothing else under the guard home, so the
    // agent can still read its own settings and session temp.
    let identity = LocalIdentity::load_or_create(&secrets_dir.join("signing.key"), "covenant-guard")?;

    let ledger = Arc::new(Ledger::new(cfg.budget_usd));
    let chain = Arc::new(Chain::new());
    let started = crate::now_ms();
    chain.append(
        started,
        "run_start",
        serde_json::json!({
            "run_id": run_id,
            "argv": cfg.agent_argv,
            "workspace": cfg.workspace.display().to_string(),
            "budget_usd": cfg.budget_usd,
            "wall_secs": cfg.wall_secs,
        }),
    );

    // Start the metering proxy on loopback.
    let proxy = Proxy::new(ledger.clone(), chain.clone(), cfg.inject_auth.clone());
    let (port, _serve) = crate::proxy::start(proxy.clone()).await?;
    let base_url = format!("http://127.0.0.1:{port}");
    let hook_url = format!("{base_url}/__guard/hook");

    // Wire hooks for Claude Code (best-effort; skipped for other agents or when
    // the caller already passes --settings).
    let mut agent_argv = cfg.agent_argv.clone();
    let agent_name = Path::new(&agent_argv[0]).file_name().and_then(|s| s.to_str()).unwrap_or("agent").to_string();
    if agent_name == "claude" && !agent_argv.iter().any(|a| a == "--settings") {
        if let Ok(settings) = write_hook_settings(&session_tmp, &hook_url) {
            agent_argv.push("--settings".into());
            agent_argv.push(settings.to_string_lossy().to_string());
        }
    }

    // Wrap the agent in the OS sandbox.
    let layout = SandboxLayout {
        workspace: cfg.workspace.clone(),
        session_tmp: session_tmp.clone(),
        guard_state: secrets_dir.clone(),
    };
    let profile = sandbox::write_profile(&state_dir)?;
    let sandboxed = sandbox::wrap(&layout, &profile, &agent_argv)?;
    if !sandbox::is_supported() {
        eprintln!("covguard: OS sandbox unavailable on this platform — running without blast-radius containment (spend cap and receipt still apply).");
    }

    // Spawn the agent as a new process group, pointed at the proxy.
    let mut command = Command::new(&sandboxed[0]);
    command
        .args(&sandboxed[1..])
        .current_dir(&cfg.workspace)
        .env("ANTHROPIC_BASE_URL", &base_url)
        .env("GUARD_HOOK_URL", &hook_url)
        .kill_on_drop(true);
    unsafe {
        command.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|e| anyhow::anyhow!("failed to launch agent '{}': {e}", sandboxed[0]))?;
    let pgid = child.id().ok_or_else(|| anyhow::anyhow!("agent exited before it could be supervised"))? as i32;

    // Watch: natural exit, budget trip, or the wall clock.
    let wall = tokio::time::sleep(Duration::from_secs(cfg.wall_secs));
    tokio::pin!(wall);

    let (outcome, exit_code) = tokio::select! {
        status = child.wait() => {
            match status {
                Ok(s) if s.success() => ("completed".to_string(), 0),
                Ok(s) => {
                    let code = s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0));
                    (format!("exit:{code}"), code)
                }
                Err(e) => (format!("error:{e}"), 1),
            }
        }
        _ = ledger.killed() => {
            let reason = ledger.kill_reason().unwrap_or_else(|| "budget".into());
            chain.append(crate::now_ms(), "kill", serde_json::json!({ "cause": "budget", "reason": reason }));
            terminate(&mut child, pgid).await;
            ("killed:budget".to_string(), 137)
        }
        _ = &mut wall => {
            ledger.trip("wall-clock limit reached");
            chain.append(crate::now_ms(), "kill", serde_json::json!({ "cause": "wall", "wall_secs": cfg.wall_secs }));
            terminate(&mut child, pgid).await;
            ("killed:wall".to_string(), 137)
        }
    };

    let ended = crate::now_ms();
    let duration_s = (ended.saturating_sub(started)) as f64 / 1000.0;

    // Assemble the receipt from the ledger, the chain, and the workspace diff.
    let per_model_map = ledger.per_model();
    let models: Vec<String> = per_model_map.keys().cloned().collect();
    let per_model: Vec<ModelLine> = per_model_map
        .iter()
        .map(|(m, t)| ModelLine { model: m.clone(), calls: t.calls, cost_usd: t.cost_usd })
        .collect();
    let tokens = per_model_map.values().fold(Tokens::default(), |mut acc, t| {
        acc.input += t.input;
        acc.output += t.output;
        acc.cache_read += t.cache_read;
        acc.cache_creation += t.cache_creation;
        acc
    });
    let commands: Vec<String> = chain
        .snapshot()
        .iter()
        .filter(|e| e.kind == "hook")
        .filter_map(|e| e.data.get("command").and_then(|c| c.as_str()).map(|s| s.to_string()))
        .collect();

    chain.append(
        ended,
        "run_end",
        serde_json::json!({
            "outcome": outcome,
            "spent_usd": ledger.spent(),
            "budget_usd": cfg.budget_usd,
            "duration_s": duration_s,
            "calls": ledger.calls(),
        }),
    );

    let core = ReceiptCore {
        version: 1,
        run_id: run_id.clone(),
        tool: agent_name.clone(),
        argv: cfg.agent_argv.clone(),
        workspace: cfg.workspace.display().to_string(),
        models,
        started_ms: started,
        ended_ms: ended,
        duration_s,
        outcome: outcome.clone(),
        budget_usd: cfg.budget_usd,
        spent_usd: ledger.spent(),
        spend_estimated: true,
        calls: ledger.calls(),
        tokens,
        per_model,
        files_changed: workspace_changes(&cfg.workspace),
        commands,
        network: proxy.network(),
        denials: Vec::new(),
        chain_root: chain.head(),
        event_count: chain.len(),
        signer_pubkey_b58: String::new(),
    };
    let receipt = receipt::sign(core, &identity);

    // Persist: the signed receipt, the human-readable card, and the raw events.
    std::fs::write(receipt_dir.join("receipt.json"), serde_json::to_vec_pretty(&receipt)?)?;
    std::fs::write(receipt_dir.join("receipt.html"), receipt::to_html(&receipt))?;
    let mut events_jsonl = String::new();
    for e in chain.snapshot() {
        events_jsonl.push_str(&serde_json::to_string(&e)?);
        events_jsonl.push('\n');
    }
    std::fs::write(receipt_dir.join("events.jsonl"), events_jsonl)?;
    std::fs::write(crate::home_dir().join("receipts").join("last"), &run_id)?;

    let summary = serde_json::json!({
        "run_id": run_id,
        "outcome": outcome,
        "spent_usd": (receipt.core.spent_usd * 10000.0).round() / 10000.0,
        "budget_usd": cfg.budget_usd,
        "calls": receipt.core.calls,
        "duration_s": (duration_s * 100.0).round() / 100.0,
        "receipt": receipt_dir.join("receipt.json").display().to_string(),
    });

    print_summary(&receipt, &receipt_dir);
    if cfg.json {
        println!("{summary}");
    }

    Ok(Outcome { receipt_dir, exit_code, summary_json: summary.to_string() })
}

/// Graceful then forceful teardown of the agent's process group.
async fn terminate(child: &mut tokio::process::Child, pgid: i32) {
    send_signal(pgid, libc::SIGTERM);
    match tokio::time::timeout(Duration::from_secs(3), child.wait()).await {
        Ok(_) => {}
        Err(_) => {
            send_signal(pgid, libc::SIGKILL);
            let _ = child.wait().await;
        }
    }
}

fn print_summary(receipt: &crate::receipt::Receipt, dir: &Path) {
    let c = &receipt.core;
    let verdict = match c.outcome.as_str() {
        "completed" => "ran clean, under cap".to_string(),
        o if o.starts_with("killed:budget") => "stopped at the spend cap".to_string(),
        o if o.starts_with("killed:wall") => "stopped at the time limit".to_string(),
        o => o.to_string(),
    };
    eprintln!();
    eprintln!("  covguard — {verdict}");
    eprintln!("  spend    ${:.2} of ${:.2} cap{}", c.spent_usd, c.budget_usd, if c.spend_estimated { " (est.)" } else { "" });
    eprintln!("  turns    {}   files {}   {:.0}s", c.calls, c.files_changed.len(), c.duration_s);
    eprintln!("  receipt  {}", dir.join("receipt.html").display());
    eprintln!("  verify   covguard verify {}", dir.join("receipt.json").display());
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn workspace_changes_on_non_git_dir_is_empty() {
        let d = tempdir().unwrap();
        assert!(workspace_changes(d.path()).is_empty());
    }

    #[test]
    fn hook_settings_written_with_both_events() {
        let d = tempdir().unwrap();
        let p = write_hook_settings(d.path(), "http://127.0.0.1:9/__guard/hook").unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert!(v["hooks"]["PreToolUse"].is_array());
        assert!(v["hooks"]["Stop"].is_array());
    }
}
