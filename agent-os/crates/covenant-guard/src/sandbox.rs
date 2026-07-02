//! The OS sandbox — blast-radius containment, and the reason the cap can't be
//! bypassed.
//!
//! On macOS the agent runs under a generated Seatbelt profile: writes are
//! confined to the workspace and the session temp dir, the agent's own config
//! files are read-only (so it can't rewire the base URL or disable the guard's
//! hooks), key material is unreadable, and all network egress is denied except
//! loopback — which is the only path to the metering proxy. Later-matching deny
//! rules win in Seatbelt, so the config-write carve-outs override the broader
//! `~/.claude` write allowance.
//!
//! Linux (bubblewrap + seccomp) is the M4 host; until then a non-macOS run
//! fails closed unless the operator explicitly opts out.

use std::path::{Path, PathBuf};

/// Everything the profile needs to know about this run's layout.
pub struct SandboxLayout {
    pub workspace: PathBuf,
    pub session_tmp: PathBuf,
    pub guard_state: PathBuf,
}

/// The Seatbelt profile. Parameters are substituted by `sandbox-exec -D`.
/// `(allow default)` with targeted denies keeps a real coding agent working
/// (it spawns node, ripgrep, git, reads the repo) while closing the paths that
/// matter. Validated against a live `claude -p` session.
const SEATBELT_PROFILE: &str = r#"(version 1)
(allow default)

; signals: the agent may not signal anything outside its own process group,
; so it can't take down the guard that supervises it
(deny signal (target others))

; network: loopback only, in both directions. The agent's sole route to the
; model API is the guard's proxy on 127.0.0.1
(deny network-outbound (remote ip))
(allow network-outbound (remote ip "localhost:*"))
(deny network-inbound (local ip))
(allow network-inbound (local ip "localhost:*"))

; writes: workspace, this run's temp, the per-user Darwin dirs the agent needs,
; and the agent's own state dir (but see the config carve-out below)
(deny file-write*)
(allow file-write*
  (subpath (param "WORKSPACE"))
  (subpath (param "SESSION_TMP"))
  (subpath (param "DARWIN_TMP"))
  (subpath (param "DARWIN_CACHE"))
  (subpath (param "AGENT_STATE"))
  (literal (param "AGENT_STATE_JSON"))
  (literal "/dev/null")
  (literal "/dev/zero")
  (literal "/dev/dtracehelper")
  (regex #"^/dev/ttys?[0-9]*$")
)

; carve-out: the agent may not edit its own policy — settings, hook config, or
; installed plugins. This is what makes the proxy and the sandbox unbypassable
; from inside the run.
(deny file-write*
  (literal (param "AGENT_SETTINGS"))
  (literal (param "AGENT_SETTINGS_LOCAL"))
  (subpath (param "AGENT_PLUGINS"))
)

; reads: key material and the guard's own state stay dark
(deny file-read*
  (subpath (param "SSH_DIR"))
  (subpath (param "AWS_DIR"))
  (subpath (param "GNUPG_DIR"))
  (subpath (param "SOLANA_DIR"))
  (subpath (param "GUARD_STATE"))
)
"#;

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
}

/// Whether an OS sandbox is available and required on this platform.
pub fn is_supported() -> bool {
    cfg!(target_os = "macos")
}

/// Write the profile into the run's state dir and return its path.
pub fn write_profile(dir: &Path) -> std::io::Result<PathBuf> {
    let path = dir.join("guard.sb");
    std::fs::write(&path, SEATBELT_PROFILE)?;
    Ok(path)
}

/// Build the full argv that runs `agent_argv` inside the sandbox.
///
/// On macOS this is `sandbox-exec -f <profile> -D KEY=VAL... <agent...>`. On
/// other platforms it returns an error unless `COVGUARD_NO_SANDBOX=1` is set,
/// in which case it returns the agent argv unchanged (and the caller warns).
pub fn wrap(layout: &SandboxLayout, profile_path: &Path, agent_argv: &[String]) -> anyhow::Result<Vec<String>> {
    if !cfg!(target_os = "macos") {
        if std::env::var("COVGUARD_NO_SANDBOX").as_deref() == Ok("1") {
            return Ok(agent_argv.to_vec());
        }
        anyhow::bail!(
            "covguard's OS sandbox is macOS-only in this build; Linux (bubblewrap) is on the way. \
             Set COVGUARD_NO_SANDBOX=1 to run without it (spend cap and receipt still apply, blast-radius containment does not)."
        );
    }

    let h = home();
    let p = |s: &str| s.to_string();
    let d = |k: &str, v: &Path| format!("{k}={}", v.display());
    let dh = |k: &str, sub: &str| format!("{k}={}", h.join(sub).display());

    let mut argv = vec![
        p("sandbox-exec"),
        p("-f"),
        p(profile_path.to_string_lossy().as_ref()),
        p("-D"),
        d("WORKSPACE", &layout.workspace),
        p("-D"),
        d("SESSION_TMP", &layout.session_tmp),
        p("-D"),
        d("GUARD_STATE", &layout.guard_state),
        p("-D"),
        format!("DARWIN_TMP={}", std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into())),
        p("-D"),
        dh("DARWIN_CACHE", "Library/Caches"),
        p("-D"),
        dh("AGENT_STATE", ".claude"),
        p("-D"),
        dh("AGENT_STATE_JSON", ".claude.json"),
        p("-D"),
        dh("AGENT_SETTINGS", ".claude/settings.json"),
        p("-D"),
        dh("AGENT_SETTINGS_LOCAL", ".claude/settings.local.json"),
        p("-D"),
        dh("AGENT_PLUGINS", ".claude/plugins"),
        p("-D"),
        dh("SSH_DIR", ".ssh"),
        p("-D"),
        dh("AWS_DIR", ".aws"),
        p("-D"),
        dh("GNUPG_DIR", ".gnupg"),
        p("-D"),
        dh("SOLANA_DIR", ".config/solana"),
    ];
    argv.extend(agent_argv.iter().cloned());
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_has_the_load_bearing_denies() {
        // If any of these disappear the sandbox stops being a boundary.
        assert!(SEATBELT_PROFILE.contains("(deny network-outbound (remote ip))"));
        assert!(SEATBELT_PROFILE.contains("(deny signal (target others))"));
        assert!(SEATBELT_PROFILE.contains("AGENT_SETTINGS"));
        assert!(SEATBELT_PROFILE.contains("SSH_DIR"));
        // The loopback allowance must come after the ip deny (last match wins).
        let deny = SEATBELT_PROFILE.find("(deny network-outbound (remote ip))").unwrap();
        let allow = SEATBELT_PROFILE.find("(allow network-outbound (remote ip \"localhost:*\"))").unwrap();
        assert!(allow > deny, "loopback allow must follow the ip deny");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn wrap_puts_agent_after_the_profile_flags() {
        let layout = SandboxLayout {
            workspace: PathBuf::from("/tmp/ws"),
            session_tmp: PathBuf::from("/tmp/ws/.tmp"),
            guard_state: PathBuf::from("/tmp/guard"),
        };
        let argv = wrap(&layout, Path::new("/tmp/guard.sb"), &["claude".into(), "-p".into()]).unwrap();
        assert_eq!(argv[0], "sandbox-exec");
        assert_eq!(argv.last().unwrap(), "-p");
        assert!(argv.iter().any(|a| a.starts_with("WORKSPACE=")));
    }
}
