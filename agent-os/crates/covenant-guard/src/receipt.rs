//! The receipt: a signed, verifiable record of one guarded run.
//!
//! The signed core carries everything worth trusting: what ran, what it cost
//! against the cap, what it changed, and the root of the event chain. The
//! signer's public key is inside the signed core, so the signature binds the
//! key to the contents. `verify` re-canonicalizes the core and checks the
//! signature; the chain root lets a reader re-check the underlying events.

use crate::chain::Entry;
use covenant_identity::{verify_b58, LocalIdentity};
use serde::{Deserialize, Serialize};

/// Token totals across the run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

/// Per-model spend line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelLine {
    pub model: String,
    pub calls: u64,
    pub cost_usd: f64,
}

/// One changed file, from `git diff --numstat`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub added: i64,
    pub removed: i64,
}

/// The signed portion of the receipt. Every field here is covered by the
/// signature; nothing else is trusted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptCore {
    pub version: u32,
    pub run_id: String,
    pub tool: String,
    pub argv: Vec<String>,
    pub workspace: String,
    pub models: Vec<String>,
    pub started_ms: u64,
    pub ended_ms: u64,
    pub duration_s: f64,
    /// `completed`, `killed:budget`, `killed:wall`, `exit:<n>`, or `error:<msg>`.
    pub outcome: String,
    pub budget_usd: f64,
    pub spent_usd: f64,
    /// True when `spent_usd` is a list-price reconstruction rather than the
    /// provider's billed figure.
    pub spend_estimated: bool,
    pub calls: u64,
    pub tokens: Tokens,
    pub per_model: Vec<ModelLine>,
    pub files_changed: Vec<FileChange>,
    /// Commands the agent reported running, via hooks. Agent-reported, not
    /// guard-observed: the proxy-measured spend and the git-measured file
    /// changes are the fields that don't depend on the agent's honesty.
    pub commands: Vec<String>,
    pub network: Vec<String>,
    pub chain_root: String,
    pub event_count: u64,
    pub signer_pubkey_b58: String,
}

/// A receipt is its signed core plus the signature over that core.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub core: ReceiptCore,
    pub signature_b58: String,
}

fn canonical(core: &ReceiptCore) -> Vec<u8> {
    serde_jcs::to_vec(core).expect("jcs canonicalization of the receipt core")
}

/// The base58 public key covguard signs receipts with, loading or creating the
/// local identity. None only if the key store can't be read or written. Use it
/// to compare against a receipt's `signer` when checking your own runs.
pub fn local_signer_pubkey() -> Option<String> {
    let path = crate::home_dir().join("secrets").join("signing.key");
    LocalIdentity::load_or_create(&path, "covenant-guard")
        .ok()
        .map(|id| bs58::encode(id.pubkey_bytes()).into_string())
}

/// Sign a core with the guard's identity, producing a complete receipt.
pub fn sign(mut core: ReceiptCore, id: &LocalIdentity) -> Receipt {
    core.signer_pubkey_b58 = bs58::encode(id.pubkey_bytes()).into_string();
    let sig = id.sign(&canonical(&core));
    let signature_b58 = bs58::encode(sig.to_bytes()).into_string();
    Receipt {
        core,
        signature_b58,
    }
}

/// Why a receipt failed to verify.
#[derive(Debug)]
pub enum VerifyError {
    Signature,
    Chain(String),
    ChainRootMismatch { signed: String, recomputed: String },
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifyError::Signature => write!(f, "signature does not match the receipt contents"),
            VerifyError::Chain(e) => write!(f, "event chain is broken: {e}"),
            VerifyError::ChainRootMismatch { signed, recomputed } => write!(
                f,
                "chain root in the receipt ({signed}) does not match the events ({recomputed})"
            ),
        }
    }
}

/// Check the signature over the receipt core. This is the minimum a reader
/// needs. It confirms the contents were signed by the named key and haven't
/// changed since.
pub fn verify_signature(receipt: &Receipt) -> Result<(), VerifyError> {
    verify_b58(
        &receipt.core.signer_pubkey_b58,
        &canonical(&receipt.core),
        &receipt.signature_b58,
    )
    .map_err(|_| VerifyError::Signature)
}

/// Full check: signature, then that the receipt's `chain_root` really is the
/// head of the supplied events. Use when the event log is kept alongside the
/// receipt.
pub fn verify_with_events(receipt: &Receipt, events: &[Entry]) -> Result<(), VerifyError> {
    verify_signature(receipt)?;
    let root = crate::chain::verify(events).map_err(|e| VerifyError::Chain(e.to_string()))?;
    if root != receipt.core.chain_root {
        return Err(VerifyError::ChainRootMismatch {
            signed: receipt.core.chain_root.clone(),
            recomputed: root,
        });
    }
    Ok(())
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Render the receipt as a self-contained HTML card: the shareable artifact.
/// Typography matches opencovenant.org: a system sans/mono pair, uppercase
/// extralight headings on wide tracking, grayscale.
pub fn to_html(receipt: &Receipt) -> String {
    let c = &receipt.core;
    let pct = if c.budget_usd > 0.0 {
        ((c.spent_usd / c.budget_usd) * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    let over = c.outcome.starts_with("killed");
    let bar_color = if over { "#111" } else { "#555" };
    let est = if c.spend_estimated { " est." } else { "" };
    let headline = match c.outcome.as_str() {
        "completed" => "Ran clean, under cap.".to_string(),
        o if o.starts_with("killed:budget") => "Stopped at the cap.".to_string(),
        o if o.starts_with("killed:wall") => "Stopped at the time limit.".to_string(),
        o => format!("Finished: {}.", esc(o)),
    };

    let files: String = if c.files_changed.is_empty() {
        "<div class=\"muted\">no files changed</div>".to_string()
    } else {
        c.files_changed
            .iter()
            .map(|f| {
                format!(
                    "<div class=\"row\"><span class=\"path\">{}</span><span class=\"nums\"><span class=\"add\">+{}</span> <span class=\"del\">−{}</span></span></div>",
                    esc(&f.path), f.added, f.removed
                )
            })
            .collect()
    };

    let models: String = if c.per_model.is_empty() {
        "<div class=\"muted\">-</div>".to_string()
    } else {
        c.per_model
            .iter()
            .map(|m| {
                format!(
                    "<div class=\"row\"><span>{}</span><span class=\"nums\">{} calls · ${:.2}</span></div>",
                    esc(&m.model), m.calls, m.cost_usd
                )
            })
            .collect()
    };

    let net = if c.network.is_empty() {
        "none".to_string()
    } else {
        esc(&c.network.join(", "))
    };

    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>covguard receipt {run_id}</title>
<style>
:root {{ color-scheme: light; }}
* {{ box-sizing: border-box; }}
body {{ margin:0; background:#e9e9e7; font-family: system-ui,-apple-system,Segoe UI,sans-serif; color:#111; padding:32px; }}
.card {{ max-width:680px; margin:0 auto; background:#fafafa; border:1px solid #d4d4d2; }}
.head {{ padding:24px 28px; border-bottom:1px solid #e2e2e0; }}
.brand {{ font-family: ui-monospace,SFMono-Regular,Menlo,monospace; font-size:11px; letter-spacing:.28em; text-transform:uppercase; font-weight:300; color:#777; }}
h1 {{ margin:8px 0 4px; font-size:26px; font-weight:200; letter-spacing:-.01em; }}
.sub {{ font-family: ui-monospace,monospace; font-size:12px; color:#666; }}
.money {{ padding:24px 28px; border-bottom:1px solid #e2e2e0; }}
.money .big {{ font-size:34px; font-weight:200; letter-spacing:-.02em; }}
.money .of {{ color:#888; font-weight:200; }}
.track {{ margin-top:14px; height:6px; background:#e0e0de; }}
.fill {{ height:6px; background:{bar_color}; width:{pct:.1}%; }}
.grid {{ display:grid; grid-template-columns:1fr 1fr; }}
.block {{ padding:20px 28px; border-bottom:1px solid #e2e2e0; }}
.block.r {{ border-left:1px solid #e2e2e0; }}
.label {{ font-size:10px; letter-spacing:.22em; text-transform:uppercase; color:#999; font-weight:400; margin-bottom:10px; }}
.row {{ display:flex; justify-content:space-between; gap:12px; font-family: ui-monospace,monospace; font-size:12.5px; padding:3px 0; }}
.path {{ overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }}
.nums {{ color:#666; white-space:nowrap; }}
.add {{ color:#2f7d3a; }} .del {{ color:#a23; }}
.muted {{ color:#999; font-family: ui-monospace,monospace; font-size:12.5px; }}
.stat {{ font-family: ui-monospace,monospace; font-size:13px; }}
.stat b {{ font-weight:600; }}
.foot {{ padding:18px 28px; background:#f2f2f0; }}
.foot .label {{ margin-bottom:6px; }}
.hash {{ font-family: ui-monospace,monospace; font-size:11px; color:#555; word-break:break-all; }}
.cmd {{ font-family: ui-monospace,monospace; font-size:11.5px; color:#444; padding:2px 0; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }}
</style></head>
<body><div class="card">
  <div class="head">
    <div class="brand">Covenant Guard · Receipt</div>
    <h1>{headline}</h1>
    <div class="sub">{tool} · {workspace}</div>
  </div>
  <div class="money">
    <div class="label">Spend{est}</div>
    <div><span class="big">${spent:.2}</span> <span class="of">of ${budget:.2} cap</span></div>
    <div class="track"><div class="fill"></div></div>
  </div>
  <div class="grid">
    <div class="block"><div class="label">Models</div>{models}</div>
    <div class="block r"><div class="label">Run</div>
      <div class="stat">duration <b>{duration:.0}s</b></div>
      <div class="stat">turns <b>{calls}</b></div>
      <div class="stat">tokens <b>{tok}</b></div>
      <div class="stat">network <b>{net}</b></div>
    </div>
  </div>
  <div class="block"><div class="label">Files changed</div>{files}</div>
  {commands}
  <div class="foot">
    <div class="label">Verified record</div>
    <div class="hash">chain {chain_root}</div>
    <div class="hash">signed by {pubkey}</div>
    <div class="hash">covguard verify re-checks this receipt from the events</div>
  </div>
</div></body></html>"#,
        run_id = esc(&c.run_id),
        bar_color = bar_color,
        pct = pct,
        headline = headline,
        tool = esc(&c.tool),
        workspace = esc(&c.workspace),
        est = est,
        spent = c.spent_usd,
        budget = c.budget_usd,
        models = models,
        duration = c.duration_s,
        calls = c.calls,
        tok = c.tokens.input + c.tokens.output + c.tokens.cache_read + c.tokens.cache_creation,
        net = net,
        files = files,
        commands = if c.commands.is_empty() {
            String::new()
        } else {
            format!(
                "<div class=\"block\"><div class=\"label\">Commands · agent-reported ({})</div>{}</div>",
                c.commands.len(),
                c.commands.iter().take(12).map(|x| format!("<div class=\"cmd\">{}</div>", esc(x))).collect::<String>()
            )
        },
        chain_root = esc(&c.chain_root),
        pubkey = esc(&c.signer_pubkey_b58),
    )
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

/// Render the receipt as a 1200×630 social card (SVG). This is the artifact
/// meant to be screenshotted and shared: the money row is the hero, the rest is
/// supporting evidence. Fonts are named so a headless renderer resolves them to
/// system faces; the same file opens crisp in any browser.
pub fn to_svg(receipt: &Receipt) -> String {
    let c = &receipt.core;
    let pct = if c.budget_usd > 0.0 {
        ((c.spent_usd / c.budget_usd) * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    let over = c.outcome.starts_with("killed");
    let bar_w = (pct / 100.0 * 1000.0).round();
    let bar_fill = if over { "#111111" } else { "#555555" };
    let headline = match c.outcome.as_str() {
        "completed" => "Ran clean, under cap.",
        o if o.starts_with("killed:budget") => "Stopped at the spend cap.",
        o if o.starts_with("killed:wall") => "Stopped at the time limit.",
        _ => "Run complete.",
    };
    let ws_name = std::path::Path::new(&c.workspace)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&c.workspace);
    let net = if c.network.is_empty() {
        "none".to_string()
    } else {
        c.network.join(", ")
    };
    let est = if c.spend_estimated { " (est.)" } else { "" };
    let sans = "font-family='Helvetica Neue, Helvetica, Arial, sans-serif'";
    let mono = "font-family='Menlo, Courier New, monospace'";

    let stat = |x: i32, label: &str, value: &str| {
        format!(
            "<text x='{x}' y='476' {mono} font-size='16' fill='#999' letter-spacing='2'>{}</text>\
             <text x='{x}' y='508' {sans} font-size='30' font-weight='300' fill='#111'>{}</text>",
            esc(&label.to_uppercase()),
            esc(value)
        )
    };

    format!(
        r#"<svg xmlns='http://www.w3.org/2000/svg' width='1200' height='630' viewBox='0 0 1200 630'>
<rect width='1200' height='630' fill='#e9e9e7'/>
<rect x='60' y='48' width='1080' height='534' fill='#fafafa' stroke='#d4d4d2'/>
<text x='100' y='118' {mono} font-size='19' fill='#777' letter-spacing='7'>COVENANT GUARD · RECEIPT</text>
<text x='100' y='188' {sans} font-size='52' font-weight='200' fill='#111'>{headline}</text>
<text x='100' y='224' {mono} font-size='19' fill='#666'>{tool} · {ws}</text>
<text x='100' y='300' {mono} font-size='16' fill='#999' letter-spacing='3'>SPEND{est}</text>
<text x='100' y='366' {sans} font-size='74' font-weight='200' fill='#111'>${spent:.2}</text>
<text x='{spent_end}' y='366' {sans} font-size='30' font-weight='200' fill='#8a8a8a'>of ${budget:.2} cap</text>
<rect x='100' y='392' width='1000' height='8' fill='#e0e0de'/>
<rect x='100' y='392' width='{bar_w}' height='8' fill='{bar_fill}'/>
{turns}{files}{dur}{netstat}
<text x='100' y='556' {mono} font-size='14' fill='#555'>chain {root} · signed {signer} · covguard verify</text>
</svg>"#,
        headline = esc(headline),
        tool = esc(&c.tool),
        ws = esc(ws_name),
        est = est,
        spent = c.spent_usd,
        // Nudge the "of $cap" label to sit after the big number.
        spent_end = 100 + 130 + format!("{:.2}", c.spent_usd).len() as i32 * 40,
        budget = c.budget_usd,
        bar_w = bar_w,
        bar_fill = bar_fill,
        turns = stat(100, "turns", &c.calls.to_string()),
        files = stat(360, "files", &c.files_changed.len().to_string()),
        dur = stat(560, "duration", &format!("{:.0}s", c.duration_s)),
        netstat = stat(760, "network", &net),
        root = esc(&short(&c.chain_root, 12)),
        signer = esc(&short(&c.signer_pubkey_b58, 8)),
    )
}

/// Rasterize an SVG card to a PNG at `path`, resolving text against system
/// fonts. Kept separate from `to_svg` so a run never depends on the renderer.
/// The SVG is always written; the PNG is on demand.
pub fn render_png(svg: &str, path: &std::path::Path) -> Result<(), String> {
    use resvg::{tiny_skia, usvg};
    let mut opt = usvg::Options::default();
    opt.fontdb_mut().load_system_fonts();
    let tree = usvg::Tree::from_str(svg, &opt).map_err(|e| format!("parse svg: {e}"))?;
    let size = tree.size().to_int_size();
    let mut pixmap = tiny_skia::Pixmap::new(size.width(), size.height())
        .ok_or_else(|| "zero-size pixmap".to_string())?;
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
    pixmap.save_png(path).map_err(|e| format!("write png: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_core() -> ReceiptCore {
        ReceiptCore {
            version: 1,
            run_id: "r1".into(),
            tool: "claude".into(),
            argv: vec!["claude".into(), "-p".into(), "hi".into()],
            workspace: "/tmp/ws".into(),
            models: vec!["claude-opus-4-8".into()],
            started_ms: 1000,
            ended_ms: 4000,
            duration_s: 3.0,
            outcome: "completed".into(),
            budget_usd: 10.0,
            spent_usd: 0.15,
            spend_estimated: true,
            calls: 2,
            tokens: Tokens {
                input: 100,
                output: 40,
                cache_read: 0,
                cache_creation: 0,
            },
            per_model: vec![ModelLine {
                model: "claude-opus-4-8".into(),
                calls: 2,
                cost_usd: 0.15,
            }],
            files_changed: vec![FileChange {
                path: "src/x.rs".into(),
                added: 3,
                removed: 1,
            }],
            commands: vec!["cargo test".into()],
            network: vec!["api.anthropic.com".into()],
            chain_root: crate::chain::GENESIS.into(),
            event_count: 3,
            signer_pubkey_b58: String::new(),
        }
    }

    #[test]
    fn sign_then_verify_roundtrips() {
        let id = LocalIdentity::generate("covenant-guard");
        let r = sign(sample_core(), &id);
        assert!(!r.signature_b58.is_empty());
        assert!(!r.core.signer_pubkey_b58.is_empty());
        verify_signature(&r).unwrap();
    }

    #[test]
    fn tampering_with_spend_breaks_signature() {
        let id = LocalIdentity::generate("covenant-guard");
        let mut r = sign(sample_core(), &id);
        r.core.spent_usd = 0.00;
        assert!(matches!(verify_signature(&r), Err(VerifyError::Signature)));
    }

    #[test]
    fn html_shows_spend_and_cap() {
        let id = LocalIdentity::generate("covenant-guard");
        let r = sign(sample_core(), &id);
        let html = to_html(&r);
        assert!(html.contains("$0.15"));
        assert!(html.contains("of $10.00 cap"));
        assert!(html.contains("Covenant Guard"));
    }

    #[test]
    fn svg_card_is_well_formed_and_shows_money() {
        let id = LocalIdentity::generate("covenant-guard");
        let r = sign(sample_core(), &id);
        let svg = to_svg(&r);
        assert!(svg.starts_with("<svg"));
        assert!(svg.trim_end().ends_with("</svg>"));
        assert!(svg.contains("$0.15"));
        assert!(svg.contains("of $10.00 cap"));
        assert!(svg.contains("COVENANT GUARD"));
    }
}
