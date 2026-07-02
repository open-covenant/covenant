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
        "card" => cmd_card(rest),
        "mcp" => covenant_guard::mcp::serve().map(|_| 0),
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

/// Load the event log next to a receipt. `Ok(None)` means genuinely absent (a
/// receipt shared on its own still verifies by signature). `Err` means the log
/// is present but unreadable or malformed; that must not silently downgrade to
/// a signature-only pass, or corrupting the log would become a way to defeat the
/// chained-receipt guarantee.
fn load_events(receipt_path: &Path) -> Result<Option<Vec<Entry>>, String> {
    let Some(parent) = receipt_path.parent() else {
        return Ok(None);
    };
    let events_path = parent.join("events.jsonl");
    let text = match std::fs::read_to_string(&events_path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", events_path.display())),
    };
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Entry>(line) {
            Ok(entry) => out.push(entry),
            Err(e) => return Err(format!("events.jsonl line {} is malformed: {e}", i + 1)),
        }
    }
    Ok(Some(out))
}

fn cmd_verify(args: &[String]) -> anyhow::Result<i32> {
    // The receipt path is the first argument that is neither a flag nor a flag's
    // value.
    let signer_val_idx = args.iter().position(|a| a == "--signer").map(|i| i + 1);
    let path = args
        .iter()
        .enumerate()
        .find(|(i, a)| !a.starts_with("--") && Some(*i) != signer_val_idx)
        .map(|(_, a)| a.clone())
        .ok_or_else(|| {
            anyhow::anyhow!("usage: covguard verify <receipt.json> [--signer <pubkey>]")
        })?;
    let expect_signer = signer_val_idx.and_then(|i| args.get(i)).cloned();
    let path = PathBuf::from(path);
    let bytes = std::fs::read(&path)
        .map_err(|_| anyhow::anyhow!("receipt not found: {}", path.display()))?;
    let receipt: Receipt = serde_json::from_slice(&bytes)
        .map_err(|e| anyhow::anyhow!("not a valid receipt ({}): {e}", path.display()))?;
    let c = &receipt.core;

    // The signature proves the receipt came from the named key and hasn't
    // changed; it does not prove that key is one you trust. --signer pins it.
    if let Some(expected) = &expect_signer {
        if &c.signer_pubkey_b58 != expected {
            println!(
                "FAIL  signed by {}, not the expected {expected}",
                c.signer_pubkey_b58
            );
            return Ok(1);
        }
    }

    let events = match load_events(&path) {
        Ok(e) => e,
        Err(e) => {
            println!("FAIL  {e}");
            return Ok(1);
        }
    };

    let outcome = match &events {
        Some(events) => receipt::verify_with_events(&receipt, events).map(|()| {
            format!(
                "signature valid, {} events chain to the signed root",
                events.len()
            )
        }),
        None => receipt::verify_signature(&receipt).map(|()| {
            "signature valid (no event log alongside the receipt; chain not re-checked)".to_string()
        }),
    };
    match outcome {
        Ok(detail) => {
            println!("PASS  {detail}");
            report(c);
            if expect_signer.is_none() {
                println!("  note     PASS means signed by the key above and internally consistent, not that the key is trusted. Pass --signer to pin the expected guard");
            }
            Ok(0)
        }
        Err(e) => {
            println!("FAIL  {e}");
            Ok(1)
        }
    }
}

fn report(c: &receipt::ReceiptCore) {
    println!("  run      {}", c.run_id);
    println!("  agent    {} ({})", c.tool, c.outcome);
    println!(
        "  spend    ${:.2} of ${:.2} cap{}",
        c.spent_usd,
        c.budget_usd,
        if c.spend_estimated { " (est.)" } else { "" }
    );
    println!(
        "  turns    {}   files {}   {:.0}s",
        c.calls,
        c.files_changed.len(),
        c.duration_s
    );
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
                println!("no receipts yet. Try: covguard run --budget 5 -- claude -p \"...\"");
            }
            for (id, r) in rows {
                let short: String = id.chars().take(8).collect();
                println!(
                    "  {short:8}  {:14}  ${:>7.2}/{:<6.2}  {}",
                    r.core.outcome, r.core.spent_usd, r.core.budget_usd, r.core.tool
                );
            }
            Ok(0)
        }
        "show" => {
            let which = args.get(1).map(|s| s.as_str()).unwrap_or("last");
            let id = resolve_id(which)?;
            let rp = receipts_dir().join(&id).join("receipt.json");
            let bytes = std::fs::read(&rp).map_err(|_| {
                anyhow::anyhow!("no receipt for '{id}' (looked in {})", rp.display())
            })?;
            let r: Receipt = serde_json::from_slice(&bytes)?;
            report(&r.core);
            println!(
                "  html     {}",
                receipts_dir().join(&id).join("receipt.html").display()
            );
            Ok(0)
        }
        "open" => {
            let which = args.get(1).map(|s| s.as_str()).unwrap_or("last");
            let id = resolve_id(which)?;
            let html = receipts_dir().join(&id).join("receipt.html");
            if !html.exists() {
                anyhow::bail!("no receipt for '{id}'");
            }
            let opener = if cfg!(target_os = "macos") {
                "open"
            } else {
                "xdg-open"
            };
            let _ = std::process::Command::new(opener).arg(&html).status();
            println!("{}", html.display());
            Ok(0)
        }
        other => {
            anyhow::bail!(
                "usage: covguard receipts [list | show <id|last> | open <id|last>] (got '{other}')"
            );
        }
    }
}

fn cmd_card(args: &[String]) -> anyhow::Result<i32> {
    // Id is optional; defaults to the most recent run.
    let which = args
        .first()
        .filter(|a| !a.starts_with("--"))
        .map(|s| s.as_str())
        .unwrap_or("last");
    let id = resolve_id(which)?;
    let dir = receipts_dir().join(&id);
    let rp = dir.join("receipt.json");
    let bytes = std::fs::read(&rp)
        .map_err(|_| anyhow::anyhow!("no receipt for '{id}' (looked in {})", rp.display()))?;
    let r: Receipt = serde_json::from_slice(&bytes)?;
    let svg = receipt::to_svg(&r);
    let svg_path = dir.join("receipt.svg");
    std::fs::write(&svg_path, &svg)?;

    let png_out = args
        .iter()
        .position(|a| a == "--png")
        .and_then(|i| args.get(i + 1));
    if let Some(out) = png_out {
        match receipt::render_png(&svg, Path::new(out)) {
            Ok(()) => println!("{out}"),
            Err(e) => anyhow::bail!("png render failed: {e}"),
        }
    } else {
        println!("{}", svg_path.display());
    }
    Ok(0)
}

fn cmd_doctor() -> anyhow::Result<i32> {
    let mut ok = true;
    let line = |mark: &str, label: &str, note: &str| {
        if note.is_empty() {
            println!("  [{mark}] {label}");
        } else {
            println!("  [{mark}] {label}  {note}");
        }
    };
    let check =
        |label: &str, pass: bool, note: &str| line(if pass { "ok" } else { "!!" }, label, note);
    let opt = |label: &str, present: bool, note: &str| {
        line(if present { "ok" } else { "--" }, label, note)
    };

    // OS sandbox
    if cfg!(target_os = "macos") {
        let sb = std::process::Command::new("sandbox-exec")
            .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        check(
            "os sandbox (Seatbelt)",
            sb,
            if sb {
                ""
            } else {
                "sandbox-exec failed; runs fail closed"
            },
        );
        ok &= sb;
    } else {
        check(
            "os sandbox",
            false,
            "macOS-only in this build; set COVGUARD_NO_SANDBOX=1 to run without it",
        );
    }

    // Agents on PATH
    let has = |bin: &str| which_on_path(bin);
    let claude = has("claude");
    let codex = has("codex");
    check(
        "claude on PATH",
        claude,
        if claude {
            ""
        } else {
            "install Claude Code to guard it"
        },
    );
    opt("codex on PATH", codex, "second host, optional");

    // Guard signer identity, shown so you can compare it against a receipt's
    // signer when checking your own runs.
    match receipt::local_signer_pubkey() {
        Some(pk) => check("guard signer key", true, &pk),
        None => {
            ok = false;
            check(
                "guard signer key",
                false,
                "could not load or create the signing key",
            );
        }
    }

    // curl for hook wiring
    let curl = has("curl");
    opt(
        "curl (hook wiring)",
        curl,
        "optional; receipt still records spend without it",
    );

    println!();
    if ok && claude {
        println!("  ready. try: covguard run --budget 5 -- claude -p \"say hello\" --dangerously-skip-permissions");
    } else {
        println!("  not ready. resolve the [!!] items above.");
    }
    Ok(if ok { 0 } else { 1 })
}

fn which_on_path(bin: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let p = dir.join(bin);
        p.is_file()
    })
}
