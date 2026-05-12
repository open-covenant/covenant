//! Agent runtime for Covenant.
//!
//! Spawns an agent as a subprocess, feeds the [`Intent`] as one JSON
//! line on stdin, reads the [`AgentResult`] as one JSON line on
//! stdout, and kills the process if it exceeds the wall-clock budget
//! declared in the agent's manifest (`resources.cpu_ms_per_task`).
//!
//! The base implementation enforces only the wall-clock timeout. It is
//! `trusted-local` execution, not sandbox-grade isolation. Stronger
//! backends plug in via the [`Runner`] trait without changing the dispatch
//! contract.

#![deny(unsafe_code)]

use async_trait::async_trait;
use covenant_manifest::{FilesystemPolicy, NetworkPolicy, Runtime as RuntimeKind, SandboxBackend};
use covenant_router::AgentCard;
use covenant_types::Intent;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
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
    #[error("agent stdout was not a valid AgentResult JSON line: {source}")]
    MalformedStdout {
        #[source]
        source: serde_json::Error,
    },
    #[error("agent {0} has no manifest or package_dir set; cannot execute")]
    NotExecutable(String),
    #[error("agent {agent} requires sandbox backend {required:?}; active runner is trusted-local")]
    SandboxRequired {
        agent: String,
        required: SandboxBackend,
    },
    #[error("agent {agent} cannot run on sandbox backend {backend:?}: {reason}")]
    UnsupportedSandboxPolicy {
        agent: String,
        backend: SandboxBackend,
        reason: String,
    },
}

#[async_trait]
pub trait Runner: Send + Sync {
    async fn run(&self, card: &AgentCard, intent: &Intent) -> Result<AgentResult, RunnerError>;
}

fn parse_result(stdout_buf: &[u8]) -> Result<AgentResult, RunnerError> {
    let line = stdout_buf
        .split(|b| *b == b'\n')
        .find(|l| !l.is_empty())
        .unwrap_or(stdout_buf);
    serde_json::from_slice(line).map_err(|source| RunnerError::MalformedStdout { source })
}

fn workspace_entry(entry: &str) -> String {
    let entry = entry.strip_prefix("./").unwrap_or(entry);
    format!("/workspace/{entry}")
}

pub struct SubprocessRunner;

impl SubprocessRunner {
    fn ensure_allowed(&self, card: &AgentCard) -> Result<(), RunnerError> {
        if card.manifest.sandbox.required {
            return Err(RunnerError::SandboxRequired {
                agent: card.id.clone(),
                required: card.manifest.sandbox.backend,
            });
        }
        Ok(())
    }
}

#[async_trait]
impl Runner for SubprocessRunner {
    async fn run(&self, card: &AgentCard, intent: &Intent) -> Result<AgentResult, RunnerError> {
        self.ensure_allowed(card)?;

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

        debug!(agent = %card.id, entry = %card.manifest.agent.entry, "spawning agent");
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

        parse_result(&stdout_buf)
    }
}

pub struct GvisorRunner {
    runsc_path: PathBuf,
    rootfs: PathBuf,
    scratch_root: PathBuf,
}

impl GvisorRunner {
    pub fn new(rootfs: impl Into<PathBuf>) -> Self {
        Self {
            runsc_path: PathBuf::from("runsc"),
            rootfs: rootfs.into(),
            scratch_root: std::env::temp_dir().join("covenant-gvisor"),
        }
    }

    pub fn with_paths(
        runsc_path: impl Into<PathBuf>,
        rootfs: impl Into<PathBuf>,
        scratch_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            runsc_path: runsc_path.into(),
            rootfs: rootfs.into(),
            scratch_root: scratch_root.into(),
        }
    }

    fn ensure_allowed(&self, card: &AgentCard) -> Result<(), RunnerError> {
        if card.manifest.sandbox.backend != SandboxBackend::LinuxGvisor {
            return Err(RunnerError::UnsupportedSandboxPolicy {
                agent: card.id.clone(),
                backend: SandboxBackend::LinuxGvisor,
                reason: "manifest does not select linux-gvisor".into(),
            });
        }
        if card.manifest.sandbox.filesystem != FilesystemPolicy::ReadOnlyPackage {
            return Err(RunnerError::UnsupportedSandboxPolicy {
                agent: card.id.clone(),
                backend: SandboxBackend::LinuxGvisor,
                reason: "initial gVisor runner only supports read-only-package filesystem policy"
                    .into(),
            });
        }
        if card.manifest.resources.network != NetworkPolicy::Off {
            return Err(RunnerError::UnsupportedSandboxPolicy {
                agent: card.id.clone(),
                backend: SandboxBackend::LinuxGvisor,
                reason: "initial gVisor runner only supports network=off".into(),
            });
        }
        Ok(())
    }

    fn args_for(card: &AgentCard) -> Vec<String> {
        let entry = workspace_entry(&card.manifest.agent.entry);
        match card.manifest.agent.runtime {
            RuntimeKind::RustBin => vec![entry],
            RuntimeKind::Python3 => vec!["python3".into(), entry],
            RuntimeKind::Node => vec!["node".into(), entry],
        }
    }

    fn bundle_id(card: &AgentCard) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        format!("covenant-{}-{nanos}", card.id)
    }

    fn oci_config(&self, card: &AgentCard) -> Result<Value, RunnerError> {
        self.ensure_allowed(card)?;
        let rootfs = self.rootfs.canonicalize()?;
        let package_dir = card.package_dir.canonicalize()?;
        let memory_bytes = card
            .manifest
            .resources
            .memory_mb
            .saturating_mul(1024 * 1024);

        Ok(json!({
            "ociVersion": "1.0.2",
            "process": {
                "terminal": false,
                "cwd": "/workspace",
                "args": Self::args_for(card),
                "env": [
                    "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
                ],
                "noNewPrivileges": true
            },
            "root": {
                "path": rootfs,
                "readonly": true
            },
            "mounts": [
                {
                    "destination": "/proc",
                    "type": "proc",
                    "source": "proc"
                },
                {
                    "destination": "/dev",
                    "type": "tmpfs",
                    "source": "tmpfs",
                    "options": ["nosuid", "strictatime", "mode=755", "size=65536k"]
                },
                {
                    "destination": "/workspace",
                    "type": "bind",
                    "source": package_dir,
                    "options": ["rbind", "ro"]
                }
            ],
            "linux": {
                "namespaces": [
                    { "type": "pid" },
                    { "type": "network" },
                    { "type": "ipc" },
                    { "type": "uts" },
                    { "type": "mount" }
                ],
                "resources": {
                    "memory": {
                        "limit": memory_bytes
                    }
                }
            }
        }))
    }

    fn write_bundle(&self, card: &AgentCard, bundle_id: &str) -> Result<PathBuf, RunnerError> {
        let bundle_dir = self.scratch_root.join(bundle_id);
        std::fs::create_dir_all(&bundle_dir)?;
        let config = self.oci_config(card)?;
        std::fs::write(
            bundle_dir.join("config.json"),
            serde_json::to_vec_pretty(&config)?,
        )?;
        Ok(bundle_dir)
    }

    fn redact_stderr(stderr: &str, paths: &[&Path]) -> String {
        let mut redacted = stderr.to_string();
        for path in paths {
            let path = path.display().to_string();
            if !path.is_empty() {
                redacted = redacted.replace(&path, "<redacted-path>");
            }
        }
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home).display().to_string();
            if !home.is_empty() {
                redacted = redacted.replace(&home, "$HOME");
            }
        }
        redacted
    }

    fn cleanup_bundle(bundle_dir: &Path) {
        let _ = std::fs::remove_dir_all(bundle_dir);
    }
}

#[async_trait]
impl Runner for GvisorRunner {
    async fn run(&self, card: &AgentCard, intent: &Intent) -> Result<AgentResult, RunnerError> {
        let timeout = Duration::from_millis(card.manifest.resources.cpu_ms_per_task);
        let bundle_id = Self::bundle_id(card);
        let bundle_dir = self.write_bundle(card, &bundle_id)?;

        debug!(agent = %card.id, sandbox = "linux-gvisor", "spawning sandboxed agent");
        let mut child = match Command::new(&self.runsc_path)
            .arg("run")
            .arg("--bundle")
            .arg(&bundle_dir)
            .arg(&bundle_id)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                Self::cleanup_bundle(&bundle_dir);
                return Err(RunnerError::Io(e));
            }
        };

        let mut stdin = child.stdin.take().expect("stdin piped");
        let mut stdout = child.stdout.take().expect("stdout piped");
        let mut stderr = child.stderr.take().expect("stderr piped");

        let intent_json = match serde_json::to_vec(intent) {
            Ok(json) => json,
            Err(e) => {
                Self::cleanup_bundle(&bundle_dir);
                return Err(RunnerError::Serde(e));
            }
        };
        if let Err(e) = stdin.write_all(&intent_json).await {
            Self::cleanup_bundle(&bundle_dir);
            return Err(RunnerError::Io(e));
        }
        if let Err(e) = stdin.write_all(b"\n").await {
            Self::cleanup_bundle(&bundle_dir);
            return Err(RunnerError::Io(e));
        }
        drop(stdin);

        let read_stdout = async {
            let mut buf = Vec::new();
            stdout.read_to_end(&mut buf).await?;
            Ok::<_, std::io::Error>(buf)
        };

        let stdout_buf = match tokio::time::timeout(timeout, read_stdout).await {
            Ok(Ok(buf)) => buf,
            Ok(Err(e)) => {
                Self::cleanup_bundle(&bundle_dir);
                return Err(RunnerError::Io(e));
            }
            Err(_) => {
                warn!(agent = %card.id, ?timeout, "sandboxed agent timed out");
                let _ = child.kill().await;
                Self::cleanup_bundle(&bundle_dir);
                return Err(RunnerError::Timeout(timeout));
            }
        };

        let status = match child.wait().await {
            Ok(status) => status,
            Err(e) => {
                Self::cleanup_bundle(&bundle_dir);
                return Err(RunnerError::Io(e));
            }
        };
        if !status.success() {
            let mut err = String::new();
            let _ = stderr.read_to_string(&mut err).await;
            let err = Self::redact_stderr(&err, &[&bundle_dir, &card.package_dir, &self.rootfs]);
            Self::cleanup_bundle(&bundle_dir);
            return Err(RunnerError::NonZeroExit {
                status: status.code().unwrap_or(-1),
                stderr: err,
            });
        }

        Self::cleanup_bundle(&bundle_dir);
        parse_result(&stdout_buf)
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

    #[test]
    fn workspace_entry_strips_one_leading_dot_slash_and_prepends_workspace_prefix() {
        assert_eq!(
            workspace_entry("./foo"),
            "/workspace/foo",
            "the documented manifest form ./foo must map cleanly to /workspace/foo so the OCI rootfs mount and the entry path agree",
        );
        assert_eq!(
            workspace_entry("foo"),
            "/workspace/foo",
            "an entry without a leading dot-slash must be a no-op for the strip arm; otherwise no-prefix manifests silently land on a different path than dot-slash ones",
        );
        assert_eq!(
            workspace_entry("nested/bin"),
            "/workspace/nested/bin",
            "multi-segment entries must be preserved verbatim under /workspace/; flattening them would break agents that ship a nested binary layout",
        );
        assert_eq!(
            workspace_entry("./"),
            "/workspace/",
            "a bare dot-slash must reduce to /workspace/ so the strip arm does not panic on the smallest legal dot-slash prefix",
        );
        assert_eq!(
            workspace_entry("././foo"),
            "/workspace/./foo",
            "the strip is intentionally non-recursive: a future refactor that loops the strip would silently rewrite manifests that embed a leading dot-slash sequence",
        );
    }

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

    fn sandbox_manifest(extra: &str) -> String {
        format!(
            r#"
[agent]
id = "sandboxed"
name = "Sandboxed"
version = "0.0.1"
runtime = "rust-bin"
entry = "./agent.sh"

[resources]
cpu_ms_per_task = 5000
network = "off"

[sandbox]
required = true
backend = "linux-gvisor"
filesystem = "read-only-package"
{extra}
"#
        )
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

    #[tokio::test]
    async fn subprocess_runner_surfaces_malformed_stdout() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("malformed.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' 'not-json'\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let manifest_toml = r#"
[agent]
id = "malformed"
name = "Malformed"
version = "0.0.1"
runtime = "rust-bin"
entry = "./malformed.sh"

[resources]
cpu_ms_per_task = 5000
"#;
        let card = card_for(manifest_toml, dir.path().to_path_buf());
        let result = SubprocessRunner.run(&card, &dummy_intent()).await;
        match result {
            Err(RunnerError::MalformedStdout { source }) => {
                assert!(source.is_syntax() || source.is_data());
            }
            other => panic!("unexpected: {other:?}"),
        }
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

    #[tokio::test]
    async fn subprocess_runner_rejects_sandbox_required_agent() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("agent.sh");
        std::fs::write(&script, "#!/bin/sh\nprintf '%s\\n' '{\"text\":\"nope\"}'\n").unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let manifest_toml = r#"
[agent]
id = "needs-sandbox"
name = "Needs Sandbox"
version = "0.0.1"
runtime = "rust-bin"
entry = "./agent.sh"

[sandbox]
required = true
backend = "linux-gvisor"
"#;
        let card = card_for(manifest_toml, dir.path().to_path_buf());
        let result = SubprocessRunner.run(&card, &dummy_intent()).await;
        match result {
            Err(RunnerError::SandboxRequired { agent, required }) => {
                assert_eq!(agent, "needs-sandbox");
                assert_eq!(required, SandboxBackend::LinuxGvisor);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn gvisor_runner_builds_restrictive_oci_config() {
        let dir = tempdir().unwrap();
        let rootfs = tempdir().unwrap();
        let scratch = tempdir().unwrap();
        std::fs::write(dir.path().join("agent.sh"), "#!/bin/sh\n").unwrap();

        let runner = GvisorRunner::with_paths("runsc", rootfs.path(), scratch.path());
        let card = card_for(&sandbox_manifest(""), dir.path().to_path_buf());
        let config = runner.oci_config(&card).unwrap();

        assert_eq!(config["process"]["cwd"], "/workspace");
        assert_eq!(config["process"]["args"][0], "/workspace/agent.sh");
        assert_eq!(
            config["process"]["env"][0],
            "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
        );
        assert_eq!(config["root"]["readonly"], true);

        let mounts = config["mounts"].as_array().unwrap();
        let workspace = mounts
            .iter()
            .find(|m| m["destination"] == "/workspace")
            .unwrap();
        let package_dir = dir.path().canonicalize().unwrap();
        let package_dir = package_dir.to_string_lossy();
        assert_eq!(workspace["source"].as_str(), Some(package_dir.as_ref()));
        assert_eq!(workspace["options"], json!(["rbind", "ro"]));

        let namespaces = config["linux"]["namespaces"].as_array().unwrap();
        assert!(namespaces.iter().any(|ns| ns["type"] == "network"));
        assert!(!config.to_string().contains("HOME="));
    }

    #[test]
    fn gvisor_runner_rejects_unenforced_policies() {
        let dir = tempdir().unwrap();
        let rootfs = tempdir().unwrap();
        let scratch = tempdir().unwrap();
        let runner = GvisorRunner::with_paths("runsc", rootfs.path(), scratch.path());

        let networked = card_for(
            r#"
[agent]
id = "sandboxed"
name = "Sandboxed"
version = "0.0.1"
runtime = "rust-bin"
entry = "./agent.sh"

[resources]
network = "outbound-https-only"

[sandbox]
required = true
backend = "linux-gvisor"
filesystem = "read-only-package"
"#,
            dir.path().to_path_buf(),
        );
        assert!(matches!(
            runner.oci_config(&networked),
            Err(RunnerError::UnsupportedSandboxPolicy { .. })
        ));

        let host_fs = card_for(
            r#"
[agent]
id = "sandboxed"
name = "Sandboxed"
version = "0.0.1"
runtime = "rust-bin"
entry = "./agent.sh"

[resources]
network = "off"

[sandbox]
required = true
backend = "linux-gvisor"
filesystem = "host"
"#,
            dir.path().to_path_buf(),
        );
        assert!(matches!(
            runner.oci_config(&host_fs),
            Err(RunnerError::UnsupportedSandboxPolicy { .. })
        ));
    }

    #[test]
    fn gvisor_runner_redacts_host_paths_from_stderr() {
        let package = PathBuf::from("/tmp/covenant-agent-package");
        let bundle = PathBuf::from("/tmp/covenant-agent-bundle");
        let stderr = "failed /tmp/covenant-agent-package using /tmp/covenant-agent-bundle";
        let redacted = GvisorRunner::redact_stderr(stderr, &[&package, &bundle]);
        assert_eq!(redacted, "failed <redacted-path> using <redacted-path>");
    }

    #[tokio::test]
    async fn gvisor_runner_cleans_bundle_when_runsc_is_missing() {
        let dir = tempdir().unwrap();
        let rootfs = tempdir().unwrap();
        let scratch = tempdir().unwrap();
        std::fs::write(dir.path().join("agent.sh"), "#!/bin/sh\n").unwrap();

        let runner = GvisorRunner::with_paths(
            scratch.path().join("missing-runsc"),
            rootfs.path(),
            scratch.path(),
        );
        let card = card_for(&sandbox_manifest(""), dir.path().to_path_buf());
        let result = runner.run(&card, &dummy_intent()).await;

        assert!(matches!(result, Err(RunnerError::Io(_))));
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    }
}
