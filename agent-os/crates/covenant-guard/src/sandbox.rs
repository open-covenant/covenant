//! The OS sandbox: blast-radius containment, and the reason the cap can't be
//! bypassed.
//!
//! On macOS the agent runs under a generated Seatbelt profile: writes are
//! confined to the workspace and the session temp dir, the agent's own config
//! files are read-only (so it can't rewire the base URL or disable the guard's
//! hooks), key material is unreadable, and all network egress is denied except
//! the loopback proxy port. Later-matching deny rules win in Seatbelt, so the
//! config-write carve-outs override the broader `~/.claude` write allowance.
//!
//! On Linux the agent runs under bubblewrap: the host filesystem is bind-mounted
//! read-only, the workspace and session temp are rewritten writable, credential
//! stores are shadowed with empty tmpfs, and the network is a fresh namespace
//! whose only route out is the guard's proxy, reached through a bind-mounted
//! unix socket (see `relay`). Same guarantees, built the other way around:
//! Seatbelt is allow-then-deny, bubblewrap is construct-a-namespace.

use std::path::{Path, PathBuf};

/// Everything the sandbox needs to know about this run's layout. `guard_state`
/// is the secrets dir: on macOS it is read-denied, on Linux it is shadowed.
pub struct SandboxLayout {
    pub workspace: PathBuf,
    pub session_tmp: PathBuf,
    pub guard_state: PathBuf,
}

/// The inputs `wrap` needs. Some fields are platform-specific and ignored on the
/// other platform (`macos_profile` on Linux; `bridge_sock`/`covguard_exe` on
/// macOS).
pub struct Plan<'a> {
    pub layout: &'a SandboxLayout,
    pub proxy_port: u16,
    pub allow_localhost: bool,
    pub agent_argv: &'a [String],
    pub macos_profile: &'a Path,
    pub bridge_sock: &'a Path,
    pub covguard_exe: &'a Path,
}

/// Credential dirs and files hidden from the agent on both platforms.
const CRED_DIRS: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".config/gh",
    ".config/gcloud",
    ".config/solana",
    ".docker",
    ".kube",
];
const CRED_FILES: &[&str] = &[".netrc", ".git-credentials", ".npmrc"];

/// The Seatbelt profile. Parameters are substituted by `sandbox-exec -D`.
/// `(allow default)` with targeted denies keeps a real coding agent working (it
/// spawns node, ripgrep, git, reads the repo) while closing the paths that
/// matter. Validated against a live `claude -p` session.
const SEATBELT_PROFILE: &str = r#"(version 1)
(allow default)

; signals: the agent may not signal anything outside its own process group,
; so it can't take down the guard that supervises it
(deny signal (target others))

; network: the agent's only outbound route is the guard's proxy. PROXY_HOSTPORT
; is `localhost:<port>` by default (so a loopback relay can't be used to tunnel
; past the meter, and a parallel run's proxy can't be borrowed), or `localhost:*`
; when --allow-localhost is set for tasks that talk to a local dev server.
; Inbound stays open so the agent can bind its own servers.
(deny network-outbound (remote ip))
(allow network-outbound (remote ip (param "PROXY_HOSTPORT")))
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

; carve-out: the agent may not edit its own policy: settings, hook config, or
; installed plugins. This is what makes the proxy and the sandbox unbypassable
; from inside the run.
(deny file-write*
  (literal (param "AGENT_SETTINGS"))
  (literal (param "AGENT_SETTINGS_LOCAL"))
  (subpath (param "AGENT_PLUGINS"))
)

; reads: key material and the guard's own state stay dark. Default-allow reads
; with a denylist of the common credential stores: the agent needs to read the
; repo and toolchain, but not tokens.
(deny file-read*
  (subpath (param "SSH_DIR"))
  (subpath (param "AWS_DIR"))
  (subpath (param "GNUPG_DIR"))
  (subpath (param "SOLANA_DIR"))
  (subpath (param "GUARD_STATE"))
  (subpath (param "GH_CONFIG"))
  (subpath (param "DOCKER_DIR"))
  (subpath (param "KUBE_DIR"))
  (subpath (param "GCLOUD_DIR"))
  (literal (param "NETRC"))
  (literal (param "GIT_CRED"))
  (literal (param "NPMRC"))
)
"#;

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
}

fn on_path(bin: &str) -> bool {
    std::env::var("PATH")
        .is_ok_and(|path| std::env::split_paths(&path).any(|d| d.join(bin).is_file()))
}

/// Whether the OS sandbox is available on this platform.
pub fn is_supported() -> bool {
    cfg!(target_os = "macos") || cfg!(target_os = "linux")
}

/// Write the Seatbelt profile into the run's state dir and return its path.
/// macOS only; a no-op path on Linux.
pub fn write_profile(dir: &Path) -> std::io::Result<PathBuf> {
    let path = dir.join("guard.sb");
    std::fs::write(&path, SEATBELT_PROFILE)?;
    Ok(path)
}

/// Build the full argv that runs the agent inside the sandbox. Returns an error
/// on an unsupported platform, or on Linux when bubblewrap is missing, unless
/// `COVGUARD_NO_SANDBOX=1` opts out (spend cap and receipt still apply; blast
/// radius containment does not).
pub fn wrap(plan: &Plan) -> anyhow::Result<Vec<String>> {
    if cfg!(target_os = "macos") {
        return Ok(wrap_macos(plan));
    }
    if cfg!(target_os = "linux") {
        if on_path("bwrap") {
            return Ok(wrap_linux(plan));
        }
        if std::env::var("COVGUARD_NO_SANDBOX").as_deref() == Ok("1") {
            return Ok(plan.agent_argv.to_vec());
        }
        anyhow::bail!(
            "bubblewrap (bwrap) not found. Install it (apt install bubblewrap, dnf install bubblewrap) \
             or set COVGUARD_NO_SANDBOX=1 to run without the sandbox (spend cap and receipt still apply)."
        );
    }
    if std::env::var("COVGUARD_NO_SANDBOX").as_deref() == Ok("1") {
        return Ok(plan.agent_argv.to_vec());
    }
    anyhow::bail!("covguard's OS sandbox supports macOS and Linux; set COVGUARD_NO_SANDBOX=1 to run without it.")
}

fn proxy_hostport(plan: &Plan) -> String {
    if plan.allow_localhost {
        "localhost:*".to_string()
    } else {
        format!("localhost:{}", plan.proxy_port)
    }
}

fn wrap_macos(plan: &Plan) -> Vec<String> {
    let h = home();
    let layout = plan.layout;
    let d = |k: &str, v: &Path| format!("{k}={}", v.display());
    let dh = |k: &str, sub: &str| format!("{k}={}", h.join(sub).display());

    let mut argv = vec![
        "sandbox-exec".into(),
        "-f".into(),
        plan.macos_profile.to_string_lossy().into_owned(),
        "-D".into(),
        d("WORKSPACE", &layout.workspace),
        "-D".into(),
        d("SESSION_TMP", &layout.session_tmp),
        "-D".into(),
        d("GUARD_STATE", &layout.guard_state),
        "-D".into(),
        format!(
            "DARWIN_TMP={}",
            std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into())
        ),
        "-D".into(),
        dh("DARWIN_CACHE", "Library/Caches"),
        "-D".into(),
        dh("AGENT_STATE", ".claude"),
        "-D".into(),
        dh("AGENT_STATE_JSON", ".claude.json"),
        "-D".into(),
        dh("AGENT_SETTINGS", ".claude/settings.json"),
        "-D".into(),
        dh("AGENT_SETTINGS_LOCAL", ".claude/settings.local.json"),
        "-D".into(),
        dh("AGENT_PLUGINS", ".claude/plugins"),
        "-D".into(),
        format!("PROXY_HOSTPORT={}", proxy_hostport(plan)),
        "-D".into(),
        dh("SSH_DIR", ".ssh"),
        "-D".into(),
        dh("AWS_DIR", ".aws"),
        "-D".into(),
        dh("GNUPG_DIR", ".gnupg"),
        "-D".into(),
        dh("SOLANA_DIR", ".config/solana"),
        "-D".into(),
        dh("GH_CONFIG", ".config/gh"),
        "-D".into(),
        dh("GCLOUD_DIR", ".config/gcloud"),
        "-D".into(),
        dh("DOCKER_DIR", ".docker"),
        "-D".into(),
        dh("KUBE_DIR", ".kube"),
        "-D".into(),
        dh("NETRC", ".netrc"),
        "-D".into(),
        dh("GIT_CRED", ".git-credentials"),
        "-D".into(),
        dh("NPMRC", ".npmrc"),
    ];
    argv.extend(plan.agent_argv.iter().cloned());
    argv
}

fn wrap_linux(plan: &Plan) -> Vec<String> {
    let h = home();
    let hp = |sub: &str| h.join(sub).to_string_lossy().into_owned();
    let ws = plan.layout.workspace.to_string_lossy().into_owned();
    let tmp = plan.layout.session_tmp.to_string_lossy().into_owned();

    let mut v: Vec<String> = Vec::new();
    let arg = |v: &mut Vec<String>, parts: &[&str]| v.extend(parts.iter().map(|s| s.to_string()));

    arg(
        &mut v,
        &[
            "bwrap",
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--tmpfs",
            "/tmp",
        ],
    );

    // Writable: the workspace, this run's temp, the agent's own state, and the
    // per-user cache node/tools scribble in.
    v.extend(["--bind".into(), ws.clone(), ws.clone()]);
    v.extend(["--bind".into(), tmp.clone(), tmp.clone()]);
    for p in [
        hp(".claude"),
        hp(".claude.json"),
        hp(".cache"),
        hp(".config/claude"),
    ] {
        v.extend(["--bind-try".into(), p.clone(), p]);
    }

    // Config stays read-only so the agent can't rewire the base URL or disable
    // the guard's hooks.
    for f in [
        hp(".claude/settings.json"),
        hp(".claude/settings.local.json"),
    ] {
        v.extend(["--ro-bind-try".into(), f.clone(), f]);
    }

    // Shadow credential stores and the guard's secrets with empty tmpfs, and
    // blank the single-file credentials over /dev/null. Only touch paths that
    // exist: bubblewrap can't create a mount point under the read-only root, and
    // a path that isn't there has nothing to hide.
    let tmpfs_if_present = |v: &mut Vec<String>, p: PathBuf| {
        if p.exists() {
            v.extend(["--tmpfs".into(), p.to_string_lossy().into_owned()]);
        }
    };
    for d in CRED_DIRS {
        tmpfs_if_present(&mut v, h.join(d));
    }
    tmpfs_if_present(&mut v, plan.layout.guard_state.clone());
    tmpfs_if_present(&mut v, h.join(".claude/plugins"));
    for f in CRED_FILES {
        let p = h.join(f);
        if p.exists() {
            v.extend([
                "--ro-bind-try".into(),
                "/dev/null".into(),
                p.to_string_lossy().into_owned(),
            ]);
        }
    }

    arg(
        &mut v,
        &[
            "--unshare-net",
            "--unshare-pid",
            "--unshare-ipc",
            "--unshare-uts",
            "--unshare-cgroup-try",
            "--die-with-parent",
            "--new-session",
            "--chdir",
        ],
    );
    v.push(ws);

    // Start the in-namespace relay, wait briefly for it to bind, then become the
    // agent. The relay forwards the proxy port to the host bridge over the
    // bind-mounted unix socket; external addresses have no route.
    let script = format!(
        "{exe} __relay 127.0.0.1:{port} {sock} & sleep 0.3; exec \"$@\"",
        exe = shell_quote(&plan.covguard_exe.to_string_lossy()),
        port = plan.proxy_port,
        sock = shell_quote(&plan.bridge_sock.to_string_lossy()),
    );
    v.extend(["/bin/sh".into(), "-c".into(), script, "sh".into()]);
    v.extend(plan.agent_argv.iter().cloned());
    v
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_plan<'a>(
        agent: &'a [String],
        profile: &'a Path,
        sock: &'a Path,
        exe: &'a Path,
        layout: &'a SandboxLayout,
    ) -> Plan<'a> {
        Plan {
            layout,
            proxy_port: 8080,
            allow_localhost: false,
            agent_argv: agent,
            macos_profile: profile,
            bridge_sock: sock,
            covguard_exe: exe,
        }
    }

    #[test]
    fn seatbelt_profile_has_the_load_bearing_denies() {
        assert!(SEATBELT_PROFILE.contains("(deny network-outbound (remote ip))"));
        assert!(SEATBELT_PROFILE.contains("(deny signal (target others))"));
        assert!(SEATBELT_PROFILE.contains("AGENT_SETTINGS"));
        assert!(SEATBELT_PROFILE.contains("SSH_DIR"));
        let deny = SEATBELT_PROFILE
            .find("(deny network-outbound (remote ip))")
            .unwrap();
        let allow = SEATBELT_PROFILE
            .find("(allow network-outbound (remote ip (param \"PROXY_HOSTPORT\")))")
            .unwrap();
        assert!(allow > deny, "loopback allow must follow the ip deny");
    }

    #[test]
    fn macos_argv_shape() {
        let layout = SandboxLayout {
            workspace: "/tmp/ws".into(),
            session_tmp: "/tmp/ws/.tmp".into(),
            guard_state: "/tmp/guard".into(),
        };
        let agent = vec!["claude".to_string(), "-p".to_string()];
        let argv = wrap_macos(&sample_plan(
            &agent,
            Path::new("/tmp/guard.sb"),
            Path::new("/x.sock"),
            Path::new("/covguard"),
            &layout,
        ));
        assert_eq!(argv[0], "sandbox-exec");
        assert_eq!(argv.last().unwrap(), "-p");
        assert!(argv.iter().any(|a| a == "PROXY_HOSTPORT=localhost:8080"));
    }

    #[test]
    fn linux_argv_isolates_and_relays() {
        let layout = SandboxLayout {
            workspace: "/home/u/ws".into(),
            session_tmp: "/home/u/.covguard/runs/1/tmp".into(),
            guard_state: "/home/u/.covguard/secrets".into(),
        };
        let agent = vec!["claude".to_string(), "-p".to_string(), "hi".to_string()];
        let argv = wrap_linux(&sample_plan(
            &agent,
            Path::new("/x.sb"),
            Path::new("/home/u/.covguard/runs/1/tmp/net/bridge.sock"),
            Path::new("/usr/local/bin/covguard"),
            &layout,
        ));
        assert_eq!(argv[0], "bwrap");
        assert!(
            argv.iter().any(|a| a == "--unshare-net"),
            "network must be isolated"
        );
        assert!(argv.iter().any(|a| a == "--die-with-parent"));
        // workspace is writable; session tmp (holding the bridge socket) too
        assert!(argv
            .windows(3)
            .any(|w| w[0] == "--bind" && w[1] == "/home/u/ws"));
        assert!(argv
            .windows(3)
            .any(|w| w[0] == "--bind" && w[1] == "/home/u/.covguard/runs/1/tmp"));
        // the relay is wired into the exec script and the agent follows
        let script = argv
            .iter()
            .find(|a| a.contains("__relay"))
            .expect("relay in script");
        assert!(script.contains("__relay 127.0.0.1:8080"));
        assert_eq!(argv.last().unwrap(), "hi");
    }
}
