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

    #[test]
    fn agent_result_serde_pins_required_text_and_default_sources() {
        // AgentResult is the JSONL line every external agent process
        // emits on stdout. `text` has no #[serde(default)] so an agent
        // that omits it must surface MalformedStdout, not silently
        // produce an empty-string result. `sources` has #[serde(default)]
        // so agents that have not been updated to emit sources still
        // decode against the documented backwards-compatibility surface.
        let no_sources: AgentResult = serde_json::from_str("{\"text\":\"hi\"}").expect(
            "AgentResult missing the sources field must decode via #[serde(default)] to an empty Vec; dropping the default would silently break agents that have not been updated to emit sources",
        );
        assert_eq!(no_sources.text, "hi");
        assert!(
            no_sources.sources.is_empty(),
            "sources must default to an empty Vec; otherwise a missing sources field deserializes to something other than the documented empty-list contract",
        );

        let populated: AgentResult =
            serde_json::from_str("{\"text\":\"hi\",\"sources\":[\"a\",\"b\"]}").unwrap();
        assert_eq!(populated.text, "hi");
        assert_eq!(populated.sources, vec!["a".to_string(), "b".to_string()]);

        // Missing the required text field must fail loud so a refactor
        // that adds #[serde(default)] to text (silently producing an
        // empty-string result for misbehaving agents) is rejected here.
        assert!(
            serde_json::from_str::<AgentResult>("{\"sources\":[]}").is_err(),
            "AgentResult without a text field must fail to decode; otherwise an agent that omits text silently writes an empty result into the daemon",
        );

        // The struct has no skip_serializing_if on sources, so an
        // empty sources Vec MUST appear in the serialized output as an
        // empty array. A refactor that adds skip_serializing_if would
        // change the on-wire shape and break any consumer that grep
        // matches on the sources field — pin the explicit presence.
        let empty = AgentResult {
            text: "hi".into(),
            sources: vec![],
        };
        let wire = serde_json::to_value(&empty).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({"text": "hi", "sources": []}),
            "empty sources must serialize as an explicit empty array; a future skip_serializing_if would silently drop the key",
        );
    }

    #[test]
    fn parse_result_pins_first_non_empty_line_fallback_and_malformed_stdout() {
        let leading_blank =
            b"\n{\"text\":\"hello\",\"sources\":[]}\nthen garbage that must be ignored\n";
        let parsed = parse_result(leading_blank).expect(
            "the first non-empty line must be picked even when the buffer starts with a blank newline; otherwise agents that flush a leading newline are silently broken",
        );
        assert_eq!(
            parsed.text, "hello",
            "the picked line must decode to the first non-empty AgentResult; a refactor that took the last line would silently swap the result for trailing debug output",
        );

        let no_newline = b"{\"text\":\"single-line\",\"sources\":[]}";
        let parsed = parse_result(no_newline).expect(
            "a buffer with no newline at all must still decode via the single-slice split; dropping this would break agents that omit a trailing newline",
        );
        assert_eq!(
            parsed.text, "single-line",
            "the no-newline buffer must decode as one whole line; otherwise the AgentResult is silently dropped on every newline-less stdout",
        );

        let only_newlines = b"\n\n";
        let err = parse_result(only_newlines)
            .expect_err("a buffer made of only newline separators leaves every split slice empty; find() returns None and the fallback returns the whole buffer, which is not valid AgentResult JSON");
        match err {
            RunnerError::MalformedStdout { .. } => {}
            other => panic!(
                "the fallback branch must map an unparseable whole-buffer to RunnerError::MalformedStdout so the daemon-side branch on this variant still fires; got: {other:?}"
            ),
        }

        let not_json = b"not-json";
        let err = parse_result(not_json).expect_err(
            "a non-JSON single line must error: parse_result must never silently coerce arbitrary stdout into a default AgentResult",
        );
        match err {
            RunnerError::MalformedStdout { .. } => {}
            other => panic!(
                "a serde_json::from_slice failure must map to RunnerError::MalformedStdout; otherwise the daemon loses the diagnostic that classifies malformed agent output. got: {other:?}"
            ),
        }
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
    fn gvisor_ensure_allowed_pins_backend_mismatch_arm_with_reason() {
        // GvisorRunner::ensure_allowed (line 193-217) is the
        // defensive-second-line check that rejects manifests the gVisor
        // runner cannot execute safely. Three rejection arms, each with
        // a unique reason string:
        //
        //   (1) backend != LinuxGvisor → "manifest does not select linux-gvisor"
        //   (2) filesystem != ReadOnlyPackage → "initial gVisor runner only supports read-only-package filesystem policy"
        //   (3) resources.network != Off → "initial gVisor runner only supports network=off"
        //
        // gvisor_runner_rejects_unenforced_policies covers arms 2 and 3
        // but pins only that the error variant is
        // RunnerError::UnsupportedSandboxPolicy { .. } without
        // verifying any reason string, and arm 1 (backend mismatch) is
        // not covered at all. SandboxBackend::TrustedLocal is the
        // documented default (line 99-101) — a manifest with no
        // [sandbox] section parses as a valid manifest with
        // backend=TrustedLocal; the manifest-level guard
        // rejects_required_trusted_local_sandbox (covenant-manifest)
        // refuses the combined-failure case (sandbox.required=true +
        // backend=trusted-local), but a non-required TrustedLocal
        // manifest is a valid input that, if ever routed to GvisorRunner
        // via a dispatch-layer regression or a new code path that
        // bypassed runner selection, would be silently wrapped in the
        // OCI bundle if this defensive check ever dropped the backend
        // equality. Pin the arm explicitly with the load-bearing reason
        // substring so a refactor that consolidates the three arms into
        // a generic 'unsupported' template fails loud here.
        let dir = tempdir().unwrap();
        let rootfs = tempdir().unwrap();
        let scratch = tempdir().unwrap();
        let runner = GvisorRunner::with_paths("runsc", rootfs.path(), scratch.path());

        // Minimal manifest: no [sandbox] section -> backend defaults to
        // TrustedLocal, sandbox.required defaults to false (so manifest
        // validation passes); filesystem default is ReadOnlyPackage and
        // network default is Off, so arms 2 and 3 cannot fire — the
        // only possible rejection inside ensure_allowed is the backend
        // mismatch arm we are pinning.
        let card = card_for(
            r#"
[agent]
id = "trusted-locale"
name = "Trusted Locale"
version = "0.0.1"
runtime = "rust-bin"
entry = "./agent.sh"

[resources]
cpu_ms_per_task = 5000
"#,
            dir.path().to_path_buf(),
        );

        match runner.oci_config(&card) {
            Err(RunnerError::UnsupportedSandboxPolicy {
                agent,
                backend,
                reason,
            }) => {
                assert_eq!(
                    agent, "trusted-locale",
                    "UnsupportedSandboxPolicy must carry the offending \
                     agent.id so operator dashboards can attribute the \
                     routing failure to a specific manifest; a refactor \
                     that dropped the agent field or substituted a \
                     generic placeholder would silently break audit \
                     correlation on misrouted manifests",
                );
                assert_eq!(
                    backend,
                    SandboxBackend::LinuxGvisor,
                    "UnsupportedSandboxPolicy.backend must surface the \
                     RUNNER's required backend (LinuxGvisor), not the \
                     manifest's declared backend (TrustedLocal); the \
                     field is documented as the runner-side expectation \
                     for operator triage — a refactor that flipped it \
                     to the manifest's value would silently invert the \
                     meaning of every existing UnsupportedSandboxPolicy \
                     diagnostic and break dashboards that group by \
                     expected runner",
                );
                assert!(
                    reason.contains("manifest does not select linux-gvisor"),
                    "the backend-mismatch arm's reason string must \
                     contain the substring 'manifest does not select \
                     linux-gvisor' — a refactor that consolidated the \
                     three rejection arms into a generic 'unsupported \
                     backend' template (e.g., during a DRY-cleanup fan-\
                     out) would break operator-dashboard grep workflows \
                     that distinguish backend-mismatch routing errors \
                     from filesystem-policy or network-policy mismatches \
                     and break on-call alerting that classifies sandbox-\
                     policy failures by reason category. got: {reason}",
                );
            }
            other => panic!(
                "expected RunnerError::UnsupportedSandboxPolicy for a \
                 TrustedLocal-backed manifest routed through GvisorRunner; \
                 a refactor that dropped the backend equality check inside \
                 ensure_allowed (line 194-200) would silently let \
                 oci_config canonicalize paths and write the OCI bundle \
                 for a non-gVisor manifest; got: {other:?}"
            ),
        }
    }

    #[test]
    fn gvisor_ensure_allowed_pins_filesystem_and_network_arm_reasons() {
        // GvisorRunner::ensure_allowed (line 193-217) rejects manifests
        // that select gVisor but violate the v0 filesystem or network
        // policy. Each arm carries a unique load-bearing reason string:
        //
        //   (1) backend != LinuxGvisor       -> "manifest does not select linux-gvisor"
        //   (2) filesystem != ReadOnlyPackage -> "initial gVisor runner only supports read-only-package filesystem policy"
        //   (3) resources.network != Off      -> "initial gVisor runner only supports network=off"
        //
        // gvisor_ensure_allowed_pins_backend_mismatch_arm_with_reason
        // (line 884) pins arm 1's reason substring.
        // gvisor_runner_rejects_unenforced_policies (line 828) exercises
        // arms 2 and 3 but only asserts the error variant via matches!()
        // — the reason strings themselves remain unpinned. This pin
        // closes both arms with a substring match identical in shape to
        // the existing arm-1 pin so a refactor that consolidates the
        // three reason strings into a generic 'unsupported sandbox
        // policy' template (during a DRY-cleanup fan-out) cannot ship
        // without breaking the operator-dashboard grep contract.
        //
        // The first fixture isolates arm 2 by satisfying arm 3
        // (network=off is the [resources] default but pinned explicitly
        // here so a future default change cannot silently shift which
        // arm fires); the second fixture isolates arm 3 by satisfying
        // arm 2 (filesystem=read-only-package).
        let dir = tempdir().unwrap();
        let rootfs = tempdir().unwrap();
        let scratch = tempdir().unwrap();
        let runner = GvisorRunner::with_paths("runsc", rootfs.path(), scratch.path());

        // Arm 2: filesystem=host violates ReadOnlyPackage; backend and
        // network are both compliant so this is the ONLY arm that can
        // fire inside ensure_allowed.
        let host_fs = card_for(
            r#"
[agent]
id = "host-fs-agent"
name = "Host FS Agent"
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
        match runner.oci_config(&host_fs) {
            Err(RunnerError::UnsupportedSandboxPolicy {
                agent,
                backend,
                reason,
            }) => {
                assert_eq!(
                    agent, "host-fs-agent",
                    "UnsupportedSandboxPolicy.agent must carry the offending \
                     agent.id for filesystem-arm rejections — operator \
                     dashboards group sandbox-policy failures by agent.id \
                     to attribute the misconfiguration to a specific \
                     manifest; cross-binds the identical assertion on the \
                     backend-mismatch arm pin",
                );
                assert_eq!(
                    backend,
                    SandboxBackend::LinuxGvisor,
                    "UnsupportedSandboxPolicy.backend must surface the \
                     RUNNER's required backend (LinuxGvisor), not the \
                     manifest's filesystem policy that triggered the \
                     rejection — the field is documented as the runner-\
                     side expectation across all three arms; cross-binds \
                     the identical assertion on the backend-mismatch arm \
                     pin",
                );
                assert!(
                    reason.contains(
                        "initial gVisor runner only supports read-only-package filesystem policy"
                    ),
                    "the filesystem-arm reason must contain the substring \
                     'initial gVisor runner only supports read-only-\
                     package filesystem policy' — a refactor that \
                     consolidated the three rejection arms into a generic \
                     'unsupported sandbox policy: filesystem = host, \
                     expected read-only-package' template (during a DRY-\
                     cleanup pass) would silently break on-call alerting \
                     that grep's this substring to classify and route \
                     filesystem-policy violations to the sandbox-policy \
                     runbook; the variant-only ancestor in \
                     gvisor_runner_rejects_unenforced_policies (line 828) \
                     would still pass because it never reads the reason. \
                     got: {reason}"
                );
            }
            other => panic!(
                "expected RunnerError::UnsupportedSandboxPolicy with the \
                 filesystem-arm reason for filesystem=host; a refactor \
                 that dropped the filesystem equality check inside \
                 ensure_allowed (line 201-208) would silently let \
                 oci_config canonicalize paths and write a host-fs OCI \
                 bundle that bypasses the v0 read-only-package \
                 invariant; got: {other:?}"
            ),
        }

        // Arm 3: network=outbound-https-only violates Off; backend and
        // filesystem are both compliant so this is the ONLY arm that
        // can fire inside ensure_allowed.
        let networked = card_for(
            r#"
[agent]
id = "networked-agent"
name = "Networked Agent"
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
        match runner.oci_config(&networked) {
            Err(RunnerError::UnsupportedSandboxPolicy {
                agent,
                backend,
                reason,
            }) => {
                assert_eq!(agent, "networked-agent");
                assert_eq!(backend, SandboxBackend::LinuxGvisor);
                assert!(
                    reason.contains("initial gVisor runner only supports network=off"),
                    "the network-arm reason must contain the substring \
                     'initial gVisor runner only supports network=off' — \
                     a refactor that consolidated the three rejection \
                     arms into a generic 'unsupported sandbox policy: \
                     network = outbound-https-only, expected off' \
                     template would silently break the on-call grep that \
                     distinguishes network-policy violations from \
                     filesystem-policy and backend-mismatch failures, \
                     even though the variant-only ancestor in \
                     gvisor_runner_rejects_unenforced_policies would \
                     still pass. got: {reason}"
                );
            }
            other => panic!(
                "expected RunnerError::UnsupportedSandboxPolicy with the \
                 network-arm reason for network=outbound-https-only; a \
                 refactor that dropped the network equality check inside \
                 ensure_allowed (line 209-215) would silently let \
                 oci_config write an OCI bundle whose linux.namespaces \
                 still includes the network namespace but whose [resources] \
                 declared outbound HTTPS — gVisor would honor the \
                 namespace isolation but the v0 contract that 'gVisor runs \
                 only network=off agents' would be silently violated; got: \
                 {other:?}"
            ),
        }
    }

    #[test]
    fn gvisor_runner_redacts_host_paths_from_stderr() {
        let package = PathBuf::from("/tmp/covenant-agent-package");
        let bundle = PathBuf::from("/tmp/covenant-agent-bundle");
        let stderr = "failed /tmp/covenant-agent-package using /tmp/covenant-agent-bundle";
        let redacted = GvisorRunner::redact_stderr(stderr, &[&package, &bundle]);
        assert_eq!(redacted, "failed <redacted-path> using <redacted-path>");
    }

    #[test]
    fn redact_stderr_pins_multi_occurrence_no_match_passthrough_and_empty_path_skip() {
        // GvisorRunner::redact_stderr (line 308-323) is the helper that
        // scrubs host paths from a sandboxed agent's stderr before the
        // daemon surfaces the error to operator dashboards or audit
        // rows. The existing gvisor_runner_redacts_host_paths_from_stderr
        // pin covers the happy path (two distinct paths, each appearing
        // once, both replaced). Three deterministic branches remain
        // unpinned and are this slice's target:
        //
        //   (1) Multi-occurrence replacement — str::replace replaces
        //       all occurrences, so the same path appearing twice must
        //       redact both copies. A refactor to .replacen(s, repl, 1)
        //       would leak the second copy silently.
        //   (2) No-match pass-through — a path that does not appear in
        //       stderr must leave stderr identical. A refactor that
        //       inserted a 'no redaction needed' breadcrumb or
        //       truncated stderr would actively obscure the real cause
        //       of agent failures.
        //   (3) Empty-path skip — line 312's `if !path.is_empty()`
        //       branch is defensive: a PathBuf::new() has empty
        //       .display() and str::replace("", repl) would insert the
        //       replacement between every character of stderr. A
        //       refactor that dropped the empty check would render the
        //       entire stderr as a sequence of '<redacted-path>'
        //       tokens, hiding the real diagnostic.
        //
        // The $HOME redaction branch (line 316-321) reads global env
        // state via std::env::var_os and is intentionally NOT covered
        // by this slice because std::env::set_var is racy under
        // cargo's default parallel test execution; covering it
        // requires serial-test infrastructure that should be a
        // separate slice.
        let path = PathBuf::from("/tmp/covenant-agent-bundle");
        let stderr =
            "ENOENT /tmp/covenant-agent-bundle; also /tmp/covenant-agent-bundle failed to unmount";
        let redacted = GvisorRunner::redact_stderr(stderr, &[&path]);
        assert_eq!(
            redacted, "ENOENT <redacted-path>; also <redacted-path> failed to unmount",
            "redact_stderr must replace every occurrence of each \
             path — a refactor to replacen(s, repl, 1) would leak \
             the second copy of /tmp/covenant-agent-bundle through \
             to operator dashboards and audit rows, breaking the \
             documented 'scrub all host paths' contract",
        );

        let missing = PathBuf::from("/usr/local/some-path-not-in-stderr");
        let plain_stderr = "agent missing required env var FOO";
        let passthrough = GvisorRunner::redact_stderr(plain_stderr, &[&missing]);
        assert_eq!(
            passthrough, plain_stderr,
            "redact_stderr must return stderr verbatim when no path \
             occurs in it — a refactor that emitted a default \
             'no redaction needed' breadcrumb or truncated stderr to \
             a fixed length would obscure the actionable diagnostic \
             that pointed at the env-var fix",
        );

        let real_path = PathBuf::from("/tmp/covenant-real");
        let empty_path = PathBuf::new();
        let mixed_stderr = "failed /tmp/covenant-real with kernel error";
        let mixed = GvisorRunner::redact_stderr(mixed_stderr, &[&empty_path, &real_path]);
        assert_eq!(
            mixed, "failed <redacted-path> with kernel error",
            "redact_stderr must skip empty paths (line 312) and only \
             redact the real one; a refactor that dropped the empty-\
             path guard would let str::replace(\"\", repl) insert \
             '<redacted-path>' between every character of stderr, \
             rendering the output as meaningless redacted-path noise \
             instead of the real error message",
        );
    }

    #[test]
    fn gvisor_args_for_pins_each_runtime_kind_interpreter_prefix() {
        // GvisorRunner::args_for (line 219-226) is the documented bridge
        // from manifest RuntimeKind to the OCI process.args vector that
        // gVisor's runsc hands to the sandboxed init process. Three arms:
        //
        //   RustBin → [/workspace/<entry>]
        //   Python3 → ["python3", /workspace/<entry>]
        //   Node    → ["node",    /workspace/<entry>]
        //
        // gvisor_runner_builds_restrictive_oci_config exercises the
        // RustBin arm via `config["process"]["args"][0] ==
        // "/workspace/agent.sh"`. The Python3 and Node arms — which
        // prepend the interpreter so the OCI exec resolves the script
        // through the language runtime rather than treating it as a
        // native binary — are not pinned. SubprocessRunner carries the
        // same three-arm dispatch (line 105-117) and only the RustBin
        // arm is exercised end-to-end via subprocess_runner_executes_real_script.
        //
        // A refactor that swapped the Python3 and Node interpreter
        // strings (e.g., during a code-style cleanup that touches both
        // arms together) would silently run python agents through
        // `node` and vice versa; the existing RustBin-only pin still
        // passes; the regression surfaces only as a confusing
        // 'command not found' deep inside runsc logs that operators
        // cannot reproduce without a sandbox host. Pin each arm at the
        // helper-function boundary so the dispatch contract is loud.
        let dir = tempdir().unwrap();

        let manifest_for = |runtime: &str| -> String {
            format!(
                r#"
[agent]
id = "sandboxed"
name = "Sandboxed"
version = "0.0.1"
runtime = "{runtime}"
entry = "./agent.sh"

[resources]
cpu_ms_per_task = 5000
network = "off"

[sandbox]
required = true
backend = "linux-gvisor"
filesystem = "read-only-package"
"#
            )
        };

        let rust_card = card_for(&manifest_for("rust-bin"), dir.path().to_path_buf());
        let py_card = card_for(&manifest_for("python3"), dir.path().to_path_buf());
        let node_card = card_for(&manifest_for("node"), dir.path().to_path_buf());

        assert_eq!(
            GvisorRunner::args_for(&rust_card),
            vec!["/workspace/agent.sh".to_string()],
            "RustBin arm must surface the bare workspace-prefixed entry \
             — a refactor that prepended an interpreter (e.g., 'cargo \
             run' or '/bin/sh') would silently wrap every native-binary \
             agent in an unwanted launcher and the gVisor OCI exec \
             would try to resolve the launcher inside the read-only \
             rootfs with no parse-time signal at the runner layer",
        );

        assert_eq!(
            GvisorRunner::args_for(&py_card),
            vec!["python3".to_string(), "/workspace/agent.sh".to_string()],
            "Python3 arm must prepend the literal 'python3' before the \
             workspace-prefixed entry — a refactor that swapped this \
             arm with the Node arm (e.g., during a fan-out cleanup that \
             touched both interpreter strings together) would silently \
             run python agents through `node`; the existing RustBin-only \
             pin gvisor_runner_builds_restrictive_oci_config would still \
             pass and the regression would surface only as a confusing \
             'command not found' inside runsc logs that operators \
             cannot reproduce without a sandbox host",
        );

        assert_eq!(
            GvisorRunner::args_for(&node_card),
            vec!["node".to_string(), "/workspace/agent.sh".to_string()],
            "Node arm must prepend the literal 'node' before the \
             workspace-prefixed entry — a refactor that dropped the \
             explicit prepend (e.g., to unify all arms on a single \
             Command-style entry path during a cleanup) would make \
             args_for return ['/workspace/agent.sh'] for Node even \
             though no shebang resolution exists inside the \
             read-only-package OCI rootfs that points the .js file at \
             a JavaScript interpreter; the sandbox would try to exec \
             the file as a native binary and gVisor would surface \
             ENOEXEC with no parse-time signal at the runner layer",
        );
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
