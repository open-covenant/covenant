//! Background-service install: `covenant-compute-node service
//! install` registers the node with the user's service manager
//! (launchd on macOS, a systemd user unit on Linux) so a plugged-in
//! machine keeps earning across logouts, crashes and reboots instead
//! of only while a terminal stays open.
//!
//! The service definition is deliberately thin: it pins the binary
//! path, the node home and the installing shell's PATH (service
//! managers strip PATH down to the system default, which would hide
//! `ollama` and `docker` from the executors). Everything else —
//! trust anchors, executor, pricing — stays in `node.env`, the same
//! file a terminal boot reads, so there is exactly one place a node
//! is configured. Install refuses until `setup` has written that
//! file: a service that boots straight into "trust anchor missing"
//! is a crash loop, not a node.
//!
//! Rendering is pure and tested; only the `launchctl`/`systemctl`
//! calls touch the system, and uninstall removes exactly what
//! install wrote (the node home — identity, earnings, audit chain —
//! is never touched).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::setup::ENV_FILE;

pub const SERVICE_LABEL: &str = "com.covenant.compute-node";
pub const SERVICE_USAGE: &str = "usage: covenant-compute-node service <install|uninstall|status>";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
    Install,
    Uninstall,
    Status,
}

pub fn parse_service_args(args: &[String]) -> Result<ServiceAction, String> {
    match args {
        [action] => match action.as_str() {
            "install" => Ok(ServiceAction::Install),
            "uninstall" => Ok(ServiceAction::Uninstall),
            "status" => Ok(ServiceAction::Status),
            other => Err(format!("unknown service action {other:?}\n{SERVICE_USAGE}")),
        },
        [] => Err(SERVICE_USAGE.into()),
        _ => Err(format!("expected exactly one action\n{SERVICE_USAGE}")),
    }
}

fn xml_escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// launchd user agent. `KeepAlive.SuccessfulExit=false` restarts a
/// crashed node but lets a deliberate clean exit stand;
/// `ThrottleInterval` keeps a genuinely broken boot from spinning.
///
/// `service.log` (launchd's stdout/stderr sink) catches only a panic or a
/// pre-startup error — launchd never rotates it, so the node's own log,
/// pointed at `{home}/logs` via `COVENANT_COMPUTE_NODE_LOG_DIR`, carries
/// the running output in dated, count-capped files instead.
pub fn launchd_plist(exe: &Path, home: &Path, path_var: &str) -> String {
    let exe = xml_escape(&exe.display().to_string());
    let home = xml_escape(&home.display().to_string());
    let path_var = xml_escape(path_var);
    let log_dir = format!("{home}/logs");
    let log = format!("{home}/service.log");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{SERVICE_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>COVENANT_COMPUTE_NODE_HOME</key>
        <string>{home}</string>
        <key>COVENANT_COMPUTE_NODE_LOG_DIR</key>
        <string>{log_dir}</string>
        <key>PATH</key>
        <string>{path_var}</string>
    </dict>
    <key>WorkingDirectory</key>
    <string>{home}</string>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ThrottleInterval</key>
    <integer>30</integer>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#
    )
}

/// Quotes a systemd unit value: C-style escaping for backslash and
/// quote, plus `%` doubled to `%%`. systemd expands `%` as a specifier
/// prefix (`%h`, `%i`, …) while parsing `ExecStart=`/`Environment=`, and
/// quoting does not suppress that — only `%%` yields a literal `%`. Rare
/// on Unix, but a `PATH` entry can carry one.
fn unit_quote(raw: &str) -> String {
    format!(
        "\"{}\"",
        raw.replace('%', "%%")
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    )
}

/// systemd user unit, the Linux mirror of the launch agent.
pub fn systemd_unit(exe: &Path, home: &Path, path_var: &str) -> String {
    let exec = unit_quote(&exe.display().to_string());
    let home_str = home.display().to_string();
    let home_env = unit_quote(&format!("COVENANT_COMPUTE_NODE_HOME={home_str}"));
    let path_env = unit_quote(&format!("PATH={path_var}"));
    let workdir = unit_quote(&home_str);
    format!(
        "[Unit]\n\
         Description=Covenant Compute operator node\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         ExecStart={exec}\n\
         Environment={home_env}\n\
         Environment={path_env}\n\
         WorkingDirectory={workdir}\n\
         Restart=on-failure\n\
         RestartSec=10\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

/// Install refuses until `setup` has written `node.env`: the service
/// manager restarts a failing boot forever, and a node with no trust
/// anchors fails every boot.
fn ensure_configured(home: &Path) -> Result<(), String> {
    if home.join(ENV_FILE).is_file() {
        return Ok(());
    }
    Err(format!(
        "{} has no {ENV_FILE} — run `covenant-compute-node setup` first, then install",
        home.display()
    ))
}

fn user_home() -> Result<PathBuf, String> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| "HOME not set".to_string())
}

fn current_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("resolve current executable: {e}"))
}

fn run(cmd: &mut Command) -> Result<String, String> {
    let rendered = format!("{cmd:?}");
    let output = cmd.output().map_err(|e| format!("spawn {rendered}: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(format!(
            "{rendered} failed ({}): {}{}",
            output.status,
            stdout.trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

pub fn run_service(home: &Path, action: ServiceAction, out: &mut impl Write) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return launchd(home, action, out);
    #[cfg(target_os = "linux")]
    return systemd(home, action, out);
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (home, action, out);
        Err("service install supports macOS (launchd) and Linux (systemd) only".into())
    }
}

#[cfg(target_os = "macos")]
fn launchd_target() -> Result<(String, String), String> {
    let uid = run(Command::new("id").arg("-u"))?.trim().to_string();
    let target = format!("gui/{uid}/{SERVICE_LABEL}");
    Ok((uid, target))
}

#[cfg(target_os = "macos")]
fn launchd_loaded(target: &str) -> bool {
    Command::new("launchctl")
        .args(["print", target])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn launchd(home: &Path, action: ServiceAction, out: &mut impl Write) -> Result<(), String> {
    let plist_path = user_home()?
        .join("Library/LaunchAgents")
        .join(format!("{SERVICE_LABEL}.plist"));

    match action {
        ServiceAction::Install => {
            // Refuse before touching the system: everything below
            // mutates launchd state or the filesystem.
            ensure_configured(home)?;
            let (uid, target) = launchd_target()?;
            let loaded = launchd_loaded(&target);
            let exe = current_exe()?;
            let path_var = std::env::var("PATH").unwrap_or_default();
            if let Some(dir) = plist_path.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("create {}: {e}", dir.display()))?;
            }
            std::fs::write(&plist_path, launchd_plist(&exe, home, &path_var))
                .map_err(|e| format!("write {}: {e}", plist_path.display()))?;
            if loaded {
                run(Command::new("launchctl").args(["bootout", &target]))?;
            }
            run(Command::new("launchctl").args([
                "bootstrap",
                &format!("gui/{uid}"),
                &plist_path.display().to_string(),
            ]))?;
            let _ = writeln!(
                out,
                "installed and started {SERVICE_LABEL}\n  agent:  {}\n  logs:   {}/logs (rotated daily; a crash lands in {}/service.log)\n  note:   runs while you are logged in; on a dedicated machine, turn on automatic login so it keeps earning after a reboot\n  remove: covenant-compute-node service uninstall",
                plist_path.display(),
                home.display(),
                home.display()
            );
        }
        ServiceAction::Uninstall => {
            let (_uid, target) = launchd_target()?;
            let loaded = launchd_loaded(&target);
            if loaded {
                run(Command::new("launchctl").args(["bootout", &target]))?;
            }
            let existed = plist_path.is_file();
            if existed {
                std::fs::remove_file(&plist_path)
                    .map_err(|e| format!("remove {}: {e}", plist_path.display()))?;
            }
            let _ = writeln!(
                out,
                "{} — the node home (identity, earnings, audit chain) is untouched",
                if loaded || existed {
                    "service removed"
                } else {
                    "nothing installed"
                }
            );
        }
        ServiceAction::Status => {
            let (uid, target) = launchd_target()?;
            let loaded = launchd_loaded(&target);
            if !plist_path.is_file() && !loaded {
                let _ = writeln!(out, "not installed");
                return Ok(());
            }
            if !loaded {
                let _ = writeln!(
                    out,
                    "installed at {} but not loaded — reinstall or `launchctl bootstrap gui/{uid} {}`",
                    plist_path.display(),
                    plist_path.display()
                );
                return Ok(());
            }
            let print = run(Command::new("launchctl").args(["print", &target]))?;
            let mut summary: Vec<&str> = print
                .lines()
                .map(str::trim)
                .filter(|l| l.starts_with("state = ") || l.starts_with("pid = "))
                .collect();
            summary.sort();
            summary.dedup();
            let _ = writeln!(out, "loaded as {target}");
            for line in summary {
                let _ = writeln!(out, "  {line}");
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn systemd(home: &Path, action: ServiceAction, out: &mut impl Write) -> Result<(), String> {
    let unit_name = "covenant-compute-node.service";
    let unit_path = user_home()?.join(".config/systemd/user").join(unit_name);

    match action {
        ServiceAction::Install => {
            ensure_configured(home)?;
            let exe = current_exe()?;
            let path_var = std::env::var("PATH").unwrap_or_default();
            if let Some(dir) = unit_path.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("create {}: {e}", dir.display()))?;
            }
            std::fs::write(&unit_path, systemd_unit(&exe, home, &path_var))
                .map_err(|e| format!("write {}: {e}", unit_path.display()))?;
            run(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
            run(Command::new("systemctl").args(["--user", "enable", "--now", unit_name]))?;
            let _ = writeln!(
                out,
                "installed and started {unit_name}\n  unit:   {}\n  logs:   journalctl --user -u {unit_name}\n  note:   `loginctl enable-linger` keeps it running while you are logged out\n  remove: covenant-compute-node service uninstall",
                unit_path.display()
            );
        }
        ServiceAction::Uninstall => {
            let existed = unit_path.is_file();
            if existed {
                let _ =
                    run(Command::new("systemctl").args(["--user", "disable", "--now", unit_name]));
                std::fs::remove_file(&unit_path)
                    .map_err(|e| format!("remove {}: {e}", unit_path.display()))?;
                run(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
            }
            let _ = writeln!(
                out,
                "{} — the node home (identity, earnings, audit chain) is untouched",
                if existed {
                    "service removed"
                } else {
                    "nothing installed"
                }
            );
        }
        ServiceAction::Status => {
            if !unit_path.is_file() {
                let _ = writeln!(out, "not installed");
                return Ok(());
            }
            let state = Command::new("systemctl")
                .args(["--user", "is-active", unit_name])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_else(|e| format!("systemctl unavailable: {e}"));
            let _ = writeln!(
                out,
                "installed at {}\n  state: {state}",
                unit_path.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_the_three_actions_and_rejects_the_rest() {
        assert_eq!(
            parse_service_args(&strings(&["install"])).unwrap(),
            ServiceAction::Install
        );
        assert_eq!(
            parse_service_args(&strings(&["uninstall"])).unwrap(),
            ServiceAction::Uninstall
        );
        assert_eq!(
            parse_service_args(&strings(&["status"])).unwrap(),
            ServiceAction::Status
        );
        assert!(parse_service_args(&[]).unwrap_err().contains("usage"));
        assert!(parse_service_args(&strings(&["restart"]))
            .unwrap_err()
            .contains("unknown service action"));
        assert!(parse_service_args(&strings(&["install", "now"]))
            .unwrap_err()
            .contains("exactly one action"));
    }

    #[test]
    fn plist_pins_binary_home_path_and_restart_policy() {
        let plist = launchd_plist(
            Path::new("/opt/compute/covenant-compute-node"),
            Path::new("/var/compute-home"),
            "/opt/homebrew/bin:/usr/bin:/bin",
        );
        assert!(plist.contains("<string>com.covenant.compute-node</string>"));
        assert!(plist.contains("<string>/opt/compute/covenant-compute-node</string>"));
        assert!(plist.contains("<key>COVENANT_COMPUTE_NODE_HOME</key>"));
        assert!(plist.contains("<string>/var/compute-home</string>"));
        assert!(plist.contains("<key>COVENANT_COMPUTE_NODE_LOG_DIR</key>"));
        assert!(plist.contains("<string>/var/compute-home/logs</string>"));
        assert!(plist.contains("<string>/opt/homebrew/bin:/usr/bin:/bin</string>"));
        assert!(plist.contains("<key>SuccessfulExit</key>"));
        assert!(plist.contains("<string>/var/compute-home/service.log</string>"));
    }

    #[test]
    fn plist_escapes_xml_metacharacters() {
        let plist = launchd_plist(Path::new("/tmp/a&b/node"), Path::new("/tmp/<home>"), "/bin");
        assert!(plist.contains("/tmp/a&amp;b/node"));
        assert!(plist.contains("/tmp/&lt;home&gt;"));
        assert!(!plist.contains("/tmp/<home>"));
    }

    #[test]
    fn unit_quotes_exec_and_environment() {
        let unit = systemd_unit(
            Path::new("/opt/key stone/node"),
            Path::new("/data/node home"),
            "/usr/bin:/bin",
        );
        assert!(unit.contains("ExecStart=\"/opt/key stone/node\""));
        assert!(unit.contains("Environment=\"COVENANT_COMPUTE_NODE_HOME=/data/node home\""));
        assert!(unit.contains("Environment=\"PATH=/usr/bin:/bin\""));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn unit_doubles_percent_so_systemd_reads_it_literally() {
        // An unescaped `%` starts a systemd specifier (%h, %i, …) while
        // the unit is parsed, mangling the value; only `%%` is a literal.
        let unit = systemd_unit(
            Path::new("/opt/node"),
            Path::new("/data/home"),
            "/usr/bin:/opt/pct%20dir:/bin",
        );
        assert!(
            unit.contains("Environment=\"PATH=/usr/bin:/opt/pct%%20dir:/bin\""),
            "a literal percent must be doubled for systemd: {unit}"
        );
    }

    #[test]
    fn install_refuses_an_unconfigured_home() {
        let dir = tempfile::tempdir().unwrap();
        let mut out = Vec::new();
        let err = run_service(dir.path(), ServiceAction::Install, &mut out).unwrap_err();
        assert!(err.contains("node.env"), "{err}");
        assert!(err.contains("setup"), "{err}");
    }
}
