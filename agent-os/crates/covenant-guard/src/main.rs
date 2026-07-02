//! covguard entry point.

use covenant_guard::chain::Entry;
use covenant_guard::receipt::{self, Receipt};
use covenant_guard::{cli, home_dir, run};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };

    let result = match cmd {
        "run" => cmd_run(rest).await,
        "verify" => cmd_verify(rest),
        "receipts" => cmd_receipts(rest),
        "doctor" => cmd_doctor(),
        "version" | "--version" | "-V" => {
            println!("covguard {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        "" | "help" | "--help" | "-h" => {
            println!("{}", cli::USAGE);
            Ok(0)
        }
        other => {
            eprintln!("covguard: unknown command '{other}'\n\n{}", cli::USAGE);
            Ok(2)
        }
    };

    match result {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            eprintln!("covguard: {e}");
            ExitCode::from(1)
        }
    }
}

async fn cmd_run(args: &[String]) -> anyhow::Result<i32> {
    let cfg = cli::parse_run(args)?;
    let outcome = run::run(cfg).await?;
    Ok(outcome.exit_code)
}

fn load_events(receipt_path: &Path) -> Option<Vec<Entry>> {
    let events_path = receipt_path.parent()?.join("events.jsonl");
    let text = std::fs::read_to_string(events_path).ok()?;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str::<Entry>(line).ok()?);
    }
    Some(out)
}

fn cmd_verify(args: &[String]) -> anyhow::Result<i32> {
    let path = args.first().ok_or_else(|| anyhow::anyhow!("usage: covguard verify <receipt.json>"))?;
    let path = PathBuf::from(path);
    let receipt: Receipt = serde_json::from_slice(&std::fs::read(&path)?)?;
    let c = &receipt.core;

    // Signature is the minimum; the event chain is the stronger check when the
    // log sits next to the receipt.
    match load_events(&path) {
        Some(events) => match receipt::verify_with_events(&receipt, &events) {
            Ok(()) => {
                println!("PASS  signature valid, {} events chain to the signed root", events.len());
                report(c);
                Ok(0)
            }
            Err(e) => {
                println!("FAIL  {e}");
                Ok(1)
            }
        },
        None => match receipt::verify_signature(&receipt) {
            Ok(()) => {
                println!("PASS  signature valid (event log not found alongside receipt; chain not re-checked)");
                report(c);
                Ok(0)
            }
            Err(e) => {
                println!("FAIL  {e}");
                Ok(1)
            }
        },
    }
}

fn report(c: &receipt::ReceiptCore) {
    println!("  run      {}", c.run_id);
    println!("  agent    {} ({})", c.tool, c.outcome);
    println!("  spend    ${:.2} of ${:.2} cap{}", c.spent_usd, c.budget_usd, if c.spend_estimated { " (est.)" } else { "" });
    println!("  turns    {}   files {}   {:.0}s", c.calls, c.files_changed.len(), c.duration_s);
    println!("  signer   {}", c.signer_pubkey_b58);
    println!("  root     {}", c.chain_root);
}

fn receipts_dir() -> PathBuf {
    home_dir().join("receipts")
}

fn resolve_id(which: &str) -> anyhow::Result<String> {
    if which == "last" {
        let last = std::fs::read_to_string(receipts_dir().join("last"))
            .map_err(|_| anyhow::anyhow!("no runs recorded yet"))?;
        Ok(last.trim().to_string())
    } else {
        Ok(which.to_string())
    }
}

fn cmd_receipts(args: &[String]) -> anyhow::Result<i32> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" => {
            let dir = receipts_dir();
            let mut rows: Vec<(String, Receipt)> = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for e in entries.flatten() {
                    let rp = e.path().join("receipt.json");
                    if let Ok(bytes) = std::fs::read(&rp) {
                        if let Ok(r) = serde_json::from_slice::<Receipt>(&bytes) {
                            rows.push((e.file_name().to_string_lossy().to_string(), r));
                        }
                    }
                }
            }
            rows.sort_by_key(|(_, r)| r.core.started_ms);
            if rows.is_empty() {
                println!("no receipts yet — try: covguard run --budget 5 -- claude -p \"...\"");
            }
            for (id, r) in rows {
                println!("  {:8}  {:14}  ${:>7.2}/{:<6.2}  {}", &id[..id.len().min(8)], r.core.outcome, r.core.spent_usd, r.core.budget_usd, r.core.tool);
            }
            Ok(0)
        }
        "show" => {
            let which = args.get(1).map(|s| s.as_str()).unwrap_or("last");
            let id = resolve_id(which)?;
            let rp = receipts_dir().join(&id).join("receipt.json");
            let r: Receipt = serde_json::from_slice(&std::fs::read(&rp)?)?;
            report(&r.core);
            println!("  html     {}", receipts_dir().join(&id).join("receipt.html").display());
            Ok(0)
        }
        "open" => {
            let which = args.get(1).map(|s| s.as_str()).unwrap_or("last");
            let id = resolve_id(which)?;
            let html = receipts_dir().join(&id).join("receipt.html");
            if !html.exists() {
                anyhow::bail!("no receipt for '{id}'");
            }
            let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
            let _ = std::process::Command::new(opener).arg(&html).status();
            println!("{}", html.display());
            Ok(0)
        }
        other => {
            anyhow::bail!("usage: covguard receipts [list | show <id|last> | open <id|last>] (got '{other}')");
        }
    }
}

fn cmd_doctor() -> anyhow::Result<i32> {
    let mut ok = true;
    let check = |label: &str, pass: bool, note: &str| {
        println!("  [{}] {label}{}", if pass { "ok" } else { "!!" }, if note.is_empty() { String::new() } else { format!(" — {note}") });
    };

    // OS sandbox
    if cfg!(target_os = "macos") {
        let sb = std::process::Command::new("sandbox-exec")
            .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        check("os sandbox (Seatbelt)", sb, if sb { "" } else { "sandbox-exec failed — runs will fail closed" });
        ok &= sb;
    } else {
        check("os sandbox", false, "macOS-only in this build; set COVGUARD_NO_SANDBOX=1 to run without it");
    }

    // Agents on PATH
    let has = |bin: &str| which_on_path(bin);
    let claude = has("claude");
    let codex = has("codex");
    check("claude on PATH", claude, if claude { "" } else { "install Claude Code to guard it" });
    check("codex on PATH", codex, "second host, optional");

    // Signing key
    let keyp = home_dir().join("signing.key");
    let key_ok = keyp.exists() || std::fs::create_dir_all(home_dir()).is_ok();
    check("signing key dir", key_ok, &keyp.display().to_string());

    // curl for hook wiring
    let curl = has("curl");
    check("curl (hook wiring)", curl, "optional; receipt still records spend without it");

    println!();
    if ok && claude {
        println!("  ready — try: covguard run --budget 5 -- claude -p \"say hello\" --dangerously-skip-permissions");
    } else {
        println!("  not ready — resolve the [!!] items above");
    }
    Ok(if ok { 0 } else { 1 })
}

fn which_on_path(bin: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else { return false };
    std::env::split_paths(&path).any(|dir| {
        let p = dir.join(bin);
        p.is_file()
    })
}
