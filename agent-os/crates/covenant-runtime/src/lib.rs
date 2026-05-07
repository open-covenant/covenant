//! Agent runtime for Covenant.
//!
//! Spawns an agent as a subprocess, feeds the [`Intent`] as one JSON
//! line on stdin, reads the [`AgentResult`] as one JSON line on
//! stdout, and kills the process if it exceeds the wall-clock budget
//! declared in the agent's manifest (`resources.cpu_ms_per_task`).
//!
//! The base implementation enforces only the wall-clock timeout;
//! sandboxing layers (gVisor, Firecracker) plug in via the [`Runner`]
//! trait without changing the dispatch contract.

#![deny(unsafe_code)]

use async_trait::async_trait;
use covenant_manifest::Runtime as RuntimeKind;
use covenant_router::AgentCard;
use covenant_types::Intent;
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tracing::{debug, warn};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentResult {
    pub text: String,
    #[serde(default)]
    pub sources: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("timed out after {0:?}")]
    Timeout(Duration),
    #[error("agent exited non-zero: status={status}, stderr={stderr}")]
    NonZeroExit { status: i32, stderr: String },
    #[error("agent {0} has no manifest or package_dir set; cannot execute")]
    NotExecutable(String),
}

#[async_trait]
pub trait Runner: Send + Sync {
    async fn run(&self, card: &AgentCard, intent: &Intent) -> Result<AgentResult, RunnerError>;
}

pub struct SubprocessRunner;

#[async_trait]
impl Runner for SubprocessRunner {
    async fn run(&self, card: &AgentCard, intent: &Intent) -> Result<AgentResult, RunnerError> {
        let entry_path = card.package_dir.join(&card.manifest.agent.entry);
        let timeout = Duration::from_millis(card.manifest.resources.cpu_ms_per_task);

        let mut cmd = match card.manifest.agent.runtime {
            RuntimeKind::RustBin => Command::new(&entry_path),
            RuntimeKind::Python3 => {
                let mut c = Command::new("python3");
                c.arg(&entry_path);
                c
            }
            RuntimeKind::Node => {
                let mut c = Command::new("node");
                c.arg(&entry_path);
                c
            }
        };
        cmd.current_dir(&card.package_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        debug!(agent = %card.id, entry = %entry_path.display(), "spawning agent");
        let mut child = cmd.spawn()?;

        let mut stdin = child.stdin.take().expect("stdin piped");
        let mut stdout = child.stdout.take().expect("stdout piped");
        let mut stderr = child.stderr.take().expect("stderr piped");

        let intent_json = serde_json::to_vec(intent)?;
        stdin.write_all(&intent_json).await?;
        stdin.write_all(b"\n").await?;
        drop(stdin);

        let read_stdout = async {
            let mut buf = Vec::new();
            stdout.read_to_end(&mut buf).await?;
            Ok::<_, std::io::Error>(buf)
        };

        let stdout_buf = match tokio::time::timeout(timeout, read_stdout).await {
            Ok(Ok(buf)) => buf,
            Ok(Err(e)) => return Err(RunnerError::Io(e)),
            Err(_) => {
                warn!(agent = %card.id, ?timeout, "agent timed out — killing");
                let _ = child.kill().await;
                return Err(RunnerError::Timeout(timeout));
            }
        };

        let status = child.wait().await?;
        if !status.success() {
            let mut err = String::new();
            let _ = stderr.read_to_string(&mut err).await;
            return Err(RunnerError::NonZeroExit {
                status: status.code().unwrap_or(-1),
                stderr: err,
            });
        }

        let line = stdout_buf
            .split(|b| *b == b'\n')
            .find(|l| !l.is_empty())
            .unwrap_or(&stdout_buf);
        let result: AgentResult = serde_json::from_slice(line)?;
        Ok(result)
    }
}

/// Test/double runner — returns a canned result.
pub struct MockRunner {
    pub response: AgentResult,
}

impl MockRunner {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            response: AgentResult {
                text: text.into(),
                sources: Vec::new(),
            },
        }
    }
}

#[async_trait]
impl Runner for MockRunner {
    async fn run(&self, _card: &AgentCard, _intent: &Intent) -> Result<AgentResult, RunnerError> {
        Ok(self.response.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_manifest::Manifest;
    use covenant_types::AgentId;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;
    use uuid::Uuid;

    fn dummy_intent() -> Intent {
        Intent {
            id: Uuid::nil(),
            text: "hi".into(),
            issuer: AgentId::new("user@local", [0u8; 32]),
            issued_at: 0,
            priority: covenant_types::Priority::Normal,
            parent: None,
        }
    }

    fn card_for(manifest_toml: &str, package_dir: std::path::PathBuf) -> AgentCard {
        let m = Manifest::parse(manifest_toml).unwrap();
        AgentCard::from_manifest_and_dir(m, package_dir)
    }

    #[tokio::test]
    async fn mock_runner_returns_canned_response() {
        let dir = tempdir().unwrap();
        let card = card_for(
            r#"
[agent]
id = "stub"
name = "Stub"
version = "0.0.1"
runtime = "rust-bin"
entry = "./fake"
"#,
            dir.path().to_path_buf(),
        );
        let r = MockRunner::new("hello there")
            .run(&card, &dummy_intent())
            .await
            .unwrap();
        assert_eq!(r.text, "hello there");
        assert!(r.sources.is_empty());
    }

    /// Drop a tiny POSIX shell script into a tempdir, point the manifest at
    /// it, and run it through `SubprocessRunner`. Confirms the stdin/stdout
    /// JSON contract end-to-end with a real subprocess.
    #[tokio::test]
    async fn subprocess_runner_executes_real_script() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("agent.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"text\":\"sh agent ok\",\"sources\":[\"s1\"]}'\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let manifest_toml = r#"
[agent]
id = "shellstub"
name = "Shell Stub"
version = "0.0.1"
runtime = "rust-bin"
entry = "./agent.sh"

[resources]
cpu_ms_per_task = 5000
"#;
        let card = card_for(manifest_toml, dir.path().to_path_buf());
        let r = SubprocessRunner.run(&card, &dummy_intent()).await.unwrap();
        assert_eq!(r.text, "sh agent ok");
        assert_eq!(r.sources, vec!["s1".to_string()]);
    }

    /// Long-running script + tight budget → `Timeout`, child killed.
    #[tokio::test]
    async fn subprocess_runner_kills_on_timeout() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("slow.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let manifest_toml = r#"
[agent]
id = "slow"
name = "Slow"
version = "0.0.1"
runtime = "rust-bin"
entry = "./slow.sh"

[resources]
cpu_ms_per_task = 150
"#;
        let card = card_for(manifest_toml, dir.path().to_path_buf());
        let r = SubprocessRunner.run(&card, &dummy_intent()).await;
        assert!(matches!(r, Err(RunnerError::Timeout(_))));
    }

    /// Failing script → `NonZeroExit`.
    #[tokio::test]
    async fn subprocess_runner_surfaces_nonzero_exit() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("bad.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\ncat >/dev/null\necho boom >&2\nexit 7\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let manifest_toml = r#"
[agent]
id = "bad"
name = "Bad"
version = "0.0.1"
runtime = "rust-bin"
entry = "./bad.sh"

[resources]
cpu_ms_per_task = 5000
"#;
        let card = card_for(manifest_toml, dir.path().to_path_buf());
        let r = SubprocessRunner.run(&card, &dummy_intent()).await;
        match r {
            Err(RunnerError::NonZeroExit { status, stderr }) => {
                assert_eq!(status, 7);
                assert!(stderr.contains("boom"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
