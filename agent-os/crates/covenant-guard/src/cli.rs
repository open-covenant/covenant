//! Command-line surface. Hand-rolled to match the repo's CLI style and to keep
//! the guard binary free of a heavy argument-parser dependency.

use crate::proxy::Host;
use std::path::PathBuf;

pub const USAGE: &str = "\
covguard: run a coding agent unattended: capped, sandboxed, and receipted.

USAGE:
    covguard run [OPTIONS] -- <agent> [agent args...]
    covguard verify <receipt.json>
    covguard receipts [list | show <id|last> | open <id|last>]
    covguard card [<id>|last] [--png <out.png>]
    covguard mcp                       Run as an MCP server (stdio) exposing guard tools
    covguard doctor
    covguard version

RUN OPTIONS:
    --budget <USD>     Hard per-run spend cap, enforced outside the agent (default: 10.00)
    --wall <DURATION>  Wall-clock limit, e.g. 90m, 12h, 3600s (default: 12h)
    --workspace <DIR>  Directory the agent may write to (default: current directory)
    --auth-token <T>   Hold this token in the proxy and inject it; keeps the
                       credential out of the agent's environment. Omit to forward
                       the agent's own login (works with a Claude subscription).
    --host <claude|codex>  Which provider to meter (default: inferred from the
                       agent). codex is experimental: the OpenAI wiring is built
                       but not yet live-verified.
    --allow-localhost  Let the agent reach any loopback port (e.g. a local dev
                       server it starts). Default: egress is pinned to the proxy
                       so the meter can't be tunneled past.
    --json             Print a machine-readable summary line on exit

EXAMPLE:
    covguard run --budget 10 -- claude -p \"fix the flaky tests\" --dangerously-skip-permissions
";

/// Parsed configuration for a `run`.
pub struct RunConfig {
    pub budget_usd: f64,
    pub wall_secs: u64,
    pub workspace: PathBuf,
    pub inject_auth: Option<String>,
    /// Explicit provider; `None` means infer from the agent's name.
    pub host: Option<Host>,
    /// Widen sandbox egress to all loopback ports (for agents that talk to a
    /// local dev server they start). Off by default: egress is pinned to the
    /// proxy port so the meter can't be tunneled past.
    pub allow_localhost: bool,
    pub json: bool,
    pub agent_argv: Vec<String>,
}

const DEFAULT_BUDGET: f64 = 10.0;
const DEFAULT_WALL_SECS: u64 = 12 * 60 * 60;

/// Parse `<DURATION>`, a bare number of seconds, or a number suffixed `s`,
/// `m`, or `h`.
pub fn parse_duration(s: &str) -> anyhow::Result<u64> {
    let s = s.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix('h') {
        (n, 3600)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1)
    } else {
        (s, 1)
    };
    let n: u64 = num
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("bad duration: {s}"))?;
    n.checked_mul(mult)
        .ok_or_else(|| anyhow::anyhow!("duration too large: {s}"))
}

/// Parse the argument list following `run`.
pub fn parse_run(args: &[String]) -> anyhow::Result<RunConfig> {
    let mut budget_usd = DEFAULT_BUDGET;
    let mut wall_secs = DEFAULT_WALL_SECS;
    let mut workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut inject_auth = None;
    let mut host = None;
    let mut allow_localhost = false;
    let mut json = false;

    let mut i = 0;
    let mut agent_argv: Vec<String> = Vec::new();
    while i < args.len() {
        match args[i].as_str() {
            "--" => {
                agent_argv = args[i + 1..].to_vec();
                break;
            }
            "--budget" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow::anyhow!("--budget needs a value"))?;
                budget_usd = v
                    .parse()
                    .map_err(|_| anyhow::anyhow!("bad --budget: {v}"))?;
                i += 2;
            }
            "--wall" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow::anyhow!("--wall needs a value"))?;
                wall_secs = parse_duration(v)?;
                i += 2;
            }
            "--workspace" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow::anyhow!("--workspace needs a value"))?;
                workspace = PathBuf::from(v);
                i += 2;
            }
            "--auth-token" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow::anyhow!("--auth-token needs a value"))?;
                if v.is_empty() || v.chars().any(|c| c.is_control()) {
                    anyhow::bail!(
                        "--auth-token must be a non-empty token with no control characters"
                    );
                }
                inject_auth = Some(format!("Bearer {v}"));
                i += 2;
            }
            "--allow-localhost" => {
                allow_localhost = true;
                i += 1;
            }
            "--host" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow::anyhow!("--host needs a value"))?;
                host = Some(match v.as_str() {
                    "claude" | "anthropic" => Host::Anthropic,
                    "codex" | "openai" => Host::OpenAI,
                    other => anyhow::bail!("unknown --host '{other}' (expected claude|codex)"),
                });
                i += 2;
            }
            "--json" => {
                json = true;
                i += 1;
            }
            other => anyhow::bail!("unknown option before `--`: {other}\n\n{USAGE}"),
        }
    }

    if agent_argv.is_empty() {
        anyhow::bail!("no agent command given. Put it after `--`.\n\n{USAGE}");
    }
    if !budget_usd.is_finite() || budget_usd <= 0.0 {
        anyhow::bail!("--budget must be a positive, finite dollar amount");
    }
    if !workspace.exists() {
        anyhow::bail!("--workspace does not exist: {}", workspace.display());
    }
    let workspace = workspace.canonicalize().unwrap_or(workspace);

    Ok(RunConfig {
        budget_usd,
        wall_secs,
        workspace,
        inject_auth,
        host,
        allow_localhost,
        json,
        agent_argv,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90m").unwrap(), 5400);
        assert_eq!(parse_duration("12h").unwrap(), 43200);
        assert_eq!(parse_duration("500s").unwrap(), 500);
        assert_eq!(parse_duration("42").unwrap(), 42);
        assert!(parse_duration("nope").is_err());
    }

    #[test]
    fn parses_a_full_run() {
        let cfg = parse_run(&v(&[
            "--budget", "5.5", "--wall", "30m", "--", "claude", "-p", "hi",
        ]))
        .unwrap();
        assert_eq!(cfg.budget_usd, 5.5);
        assert_eq!(cfg.wall_secs, 1800);
        assert_eq!(cfg.agent_argv, v(&["claude", "-p", "hi"]));
        assert!(cfg.inject_auth.is_none());
    }

    #[test]
    fn requires_an_agent_command() {
        assert!(parse_run(&v(&["--budget", "5"])).is_err());
    }

    #[test]
    fn rejects_nonpositive_budget() {
        assert!(parse_run(&v(&["--budget", "0", "--", "claude"])).is_err());
    }

    #[test]
    fn rejects_non_finite_budget() {
        // NaN/inf pass a naive `<= 0.0` check but disable the cap; must reject.
        assert!(parse_run(&v(&["--budget", "nan", "--", "claude"])).is_err());
        assert!(parse_run(&v(&["--budget", "inf", "--", "claude"])).is_err());
        assert!(parse_run(&v(&["--budget", "1e999", "--", "claude"])).is_err());
    }

    #[test]
    fn duration_overflow_errors() {
        assert!(parse_duration("60000000000000000000h").is_err());
    }

    #[test]
    fn auth_token_becomes_bearer() {
        let cfg = parse_run(&v(&["--auth-token", "abc", "--", "claude"])).unwrap();
        assert_eq!(cfg.inject_auth.as_deref(), Some("Bearer abc"));
    }
}
