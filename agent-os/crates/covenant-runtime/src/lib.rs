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
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tracing::{debug, warn};

mod hermes;
pub use hermes::{HermesCapabilities, HermesRunner};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentResult {
    pub text: String,
    #[serde(default)]
    pub sources: Vec<String>,
    /// Runner-emitted events captured during the dispatch. Subprocess
    /// runners leave this empty; the Hermes runner populates it from the
    /// gateway's `/v1/runs/{id}/events` SSE stream. The daemon folds the
    /// vec into the hash-chained audit log after the dispatch returns.
    #[serde(default)]
    pub runtime_events: Vec<RuntimeTrace>,
}

/// Mid-run signals from a runner. Distinct from `AuditKind` so the
/// `covenant-runtime` crate stays free of an audit-log dependency; the
/// daemon converts each variant into the matching `AuditKind` row when
/// it folds the result into the chain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeTrace {
    /// Hermes invoked an internal tool. `preview` is the short label
    /// Hermes emits (e.g. `"ls"` for the terminal tool); the daemon
    /// hashes it before persisting so the chain never embeds raw tool
    /// input.
    HermesToolInvoked {
        run_id: String,
        tool: String,
        preview: String,
    },
    /// Hermes finished invoking a tool. `error` is `true` when the tool
    /// raised, `false` otherwise. Independent from `HermesRunFailed`.
    HermesToolCompleted {
        run_id: String,
        tool: String,
        duration_ms: u64,
        error: bool,
    },
    /// Hermes paused the run pending operator approval. Recorded so a
    /// stalled run is auditable even when no operator is watching.
    HermesApprovalRequested {
        run_id: String,
        choices: Vec<String>,
    },
    /// An operator (or auto-policy) answered the pending approval.
    /// `resolved` is the count Hermes reports as resolved by this
    /// response — kept as `u64` so an upstream change to the counter
    /// width never silently truncates an audit row.
    HermesApprovalResponded {
        run_id: String,
        choice: String,
        resolved: u64,
    },
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
    #[error("agent {agent} declares runtime {got:?} but this runner only handles {expected:?}")]
    WrongRuntime {
        agent: String,
        expected: &'static str,
        got: &'static str,
    },
    #[error(
        "agent {agent} declares runtime \"hermes\" but no Hermes gateway is configured — set HERMES_API_BASE_URL"
    )]
    HermesUnconfigured { agent: String },
    #[error("remote runtime: status={status} message={message}")]
    Remote { status: u16, message: String },
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
            RuntimeKind::Hermes => {
                // Routed via CompositeRunner under normal operation. If
                // SubprocessRunner is reached for a Hermes agent the
                // wiring is broken — fail loud rather than silently.
                return Err(RunnerError::WrongRuntime {
                    agent: card.id.clone(),
                    expected: "python3|node|rust-bin",
                    got: "hermes",
                });
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

    fn args_for(card: &AgentCard) -> Result<Vec<String>, RunnerError> {
        let entry = workspace_entry(&card.manifest.agent.entry);
        Ok(match card.manifest.agent.runtime {
            RuntimeKind::RustBin => vec![entry],
            RuntimeKind::Python3 => vec!["python3".into(), entry],
            RuntimeKind::Node => vec!["node".into(), entry],
            RuntimeKind::Hermes => {
                return Err(RunnerError::WrongRuntime {
                    agent: card.id.clone(),
                    expected: "python3|node|rust-bin",
                    got: "hermes",
                });
            }
        })
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
                "args": Self::args_for(card)?,
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
                runtime_events: Vec::new(),
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

/// Dispatches each agent to the runner appropriate for its declared
/// `agent.runtime`. Subprocess-style runtimes (python3, node, rust-bin)
/// go to the configured local backend (trusted-local subprocess or
/// linux-gvisor). Hermes runtime goes to a configured Hermes gateway;
/// missing configuration fails closed.
pub struct CompositeRunner {
    local: Arc<dyn Runner>,
    hermes: Option<Arc<dyn Runner>>,
}

impl CompositeRunner {
    pub fn new(local: Arc<dyn Runner>, hermes: Option<Arc<dyn Runner>>) -> Self {
        Self { local, hermes }
    }
}

#[async_trait]
impl Runner for CompositeRunner {
    async fn run(&self, card: &AgentCard, intent: &Intent) -> Result<AgentResult, RunnerError> {
        match card.manifest.agent.runtime {
            RuntimeKind::Hermes => match &self.hermes {
                Some(h) => h.run(card, intent).await,
                None => Err(RunnerError::HermesUnconfigured {
                    agent: card.id.clone(),
                }),
            },
            RuntimeKind::Python3 | RuntimeKind::Node | RuntimeKind::RustBin => {
                self.local.run(card, intent).await
            }
        }
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
            runtime_events: vec![],
        };
        let wire = serde_json::to_value(&empty).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({"text": "hi", "sources": [], "runtime_events": []}),
            "empty sources must serialize as an explicit empty array; a future skip_serializing_if would silently drop the key",
        );
    }

    #[test]
    fn runtime_trace_serde_pins_snake_case_tag_and_per_variant_field_set() {
        // RuntimeTrace (line 49-84) is the mid-run signal enum the
        // Hermes gateway emits over SSE and the daemon ingests via
        // serde_json::from_str. The enum carries
        //
        //     #[serde(tag = "type", rename_all = "snake_case")]
        //
        // so the four variants surface on the wire with a tagged
        // "type" field (NOT the externally-tagged default which would
        // wrap the variant under a JSON key named after the variant)
        // and snake_case slugs (NOT the camelCase the upstream MCP
        // JSON-RPC convention uses).
        //
        // The slugs are pinned at the covenant-audit layer through
        // AuditKind::Hermes* variants
        // (audit_kind_hermes_tool_invoked_serde_pins_four_field_variant
        // et al in covenant-audit/src/lib.rs), but those pins exercise
        // a DIFFERENT enum with its own serde attributes.
        // A refactor that changed RuntimeTrace's rename_all to
        // camelCase, dropped the tag attribute, renamed a field, or
        // added a stray #[serde(default)]/skip_serializing_if to a
        // required field would silently break SSE ingestion while
        // every audit-layer test continued to pass. The daemon's
        // runtime_trace_to_audit_kind_pins_each_variant_mapping_preview_redaction_and_responded_to_resolved_rename
        // pin in covenantd/src/lib.rs only exercises the in-process
        // struct mapping AFTER a successful SSE decode; the wire-form
        // decode itself is unpinned.

        let invoked = RuntimeTrace::HermesToolInvoked {
            run_id: "r1".into(),
            tool: "terminal".into(),
            preview: "ls".into(),
        };
        let invoked_wire = serde_json::to_value(&invoked).unwrap();
        assert_eq!(
            invoked_wire,
            serde_json::json!({
                "type": "hermes_tool_invoked",
                "run_id": "r1",
                "tool": "terminal",
                "preview": "ls",
            }),
            "HermesToolInvoked must serialize with type=hermes_tool_invoked \
             plus three flattened fields (run_id, tool, preview). A refactor \
             that changed rename_all to camelCase would emit hermesToolInvoked \
             and the daemon's SSE deserializer would fail with 'unknown variant'; \
             a refactor that dropped the tag attribute would emit the \
             externally-tagged default (variant name as a wrapping JSON key) \
             and the SSE consumer would also fail",
        );
        assert_eq!(
            serde_json::from_value::<RuntimeTrace>(invoked_wire.clone()).unwrap(),
            invoked,
            "round-trip must reconstruct the original variant verbatim",
        );

        // Closed-key-set anchor for HermesToolInvoked. A stray
        // #[serde(skip_serializing_if = ...)] on any field would drop
        // the key from the wire payload; this assertion catches that
        // class of regression directly.
        let invoked_obj = invoked_wire.as_object().unwrap();
        let invoked_keys: std::collections::BTreeSet<&str> =
            invoked_obj.keys().map(String::as_str).collect();
        let expected_invoked_keys: std::collections::BTreeSet<&str> =
            ["type", "run_id", "tool", "preview"].into_iter().collect();
        assert_eq!(
            invoked_keys, expected_invoked_keys,
            "HermesToolInvoked wire form must have exactly four keys; \
             a refactor that added a skip_serializing_if on any field, \
             or that introduced a new field without updating this pin, \
             would surface here as a key-set divergence",
        );

        let completed = RuntimeTrace::HermesToolCompleted {
            run_id: "r1".into(),
            tool: "terminal".into(),
            duration_ms: 42,
            error: false,
        };
        assert_eq!(
            serde_json::to_value(&completed).unwrap(),
            serde_json::json!({
                "type": "hermes_tool_completed",
                "run_id": "r1",
                "tool": "terminal",
                "duration_ms": 42,
                "error": false,
            }),
            "HermesToolCompleted slug, field names, and integer/bool wire types \
             must match the daemon's SSE decoder; a rename of duration_ms or \
             error (e.g., to durationMs camelCase) would silently drop the \
             field at decode time",
        );

        let requested = RuntimeTrace::HermesApprovalRequested {
            run_id: "r1".into(),
            choices: vec!["approve".into(), "deny".into()],
        };
        assert_eq!(
            serde_json::to_value(&requested).unwrap(),
            serde_json::json!({
                "type": "hermes_approval_requested",
                "run_id": "r1",
                "choices": ["approve", "deny"],
            }),
            "HermesApprovalRequested choices must remain a JSON array of strings; \
             a refactor that wrapped each choice in a label-value object pair \
             would break the SSE decoder and the audit-row representation",
        );

        let responded = RuntimeTrace::HermesApprovalResponded {
            run_id: "r1".into(),
            choice: "approve".into(),
            resolved: 1,
        };
        assert_eq!(
            serde_json::to_value(&responded).unwrap(),
            serde_json::json!({
                "type": "hermes_approval_responded",
                "run_id": "r1",
                "choice": "approve",
                "resolved": 1,
            }),
            "HermesApprovalResponded resolved must serialize as a JSON number \
             (u64 in Rust). A refactor that widened to u128 or narrowed to u32 \
             with truncation would surface here; a refactor that renamed the \
             field to 'count' under a 'clearer name' rationale would also \
             surface",
        );

        // Negative slug rejection: a camelCase tag must NOT decode.
        // Pins that rename_all stays a whitelist so a future
        // #[serde(other)] arm cannot silently absorb mis-cased
        // upstream payloads from a future Hermes gateway version.
        assert!(
            serde_json::from_value::<RuntimeTrace>(serde_json::json!({
                "type": "hermesToolInvoked",
                "run_id": "r1",
                "tool": "terminal",
                "preview": "ls",
            }))
            .is_err(),
            "camelCase slug 'hermesToolInvoked' must be rejected — pins that \
             rename_all = snake_case stays a whitelist. A dropped or flipped \
             rename_all attribute would let this decode and the daemon would \
             ingest events with the wrong variant identity",
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

    #[tokio::test]
    async fn mock_runner_run_pins_card_intent_independence_and_runtime_events_empty_default() {
        // MockRunner::new (line 505-515) constructs an AgentResult with
        // text=input, sources=empty Vec, runtime_events=empty Vec.
        // MockRunner::run (line 518-522) clones this canned response on
        // every call and intentionally ignores its _card and _intent
        // arguments (the underscore prefix documents the intent).
        //
        // mock_runner_returns_canned_response covers text-pass-through
        // and sources-empty for ONE card, ONE intent, ONE call. It
        // does NOT cover the runtime_events default (a recently-added
        // AgentResult field), card-independence, intent-independence,
        // or the multi-call stability that test fixtures rely on.
        // A refactor that began filtering by card.id would break every
        // fixture that uses MockRunner as a stand-in across agents; a
        // refactor that populated runtime_events with synthetic Hermes
        // traces would leak fake audit rows into integration tests.
        let dir = tempdir().unwrap();
        let card_a = card_for(
            r#"
[agent]
id = "alpha"
name = "Alpha"
version = "0.0.1"
runtime = "rust-bin"
entry = "./alpha"
"#,
            dir.path().to_path_buf(),
        );
        let card_b = card_for(
            r#"
[agent]
id = "beta"
name = "Beta"
version = "0.0.1"
runtime = "python3"
entry = "beta.py"

[capabilities]
required = ["tool.web_search"]
"#,
            dir.path().to_path_buf(),
        );

        let intent_a = Intent {
            id: Uuid::new_v4(),
            text: "first prompt".into(),
            issuer: AgentId::new("user-a@local", [1u8; 32]),
            issued_at: 1_000,
            priority: covenant_types::Priority::Normal,
            parent: None,
        };
        let intent_b = Intent {
            id: Uuid::new_v4(),
            text: "completely different prompt with different fields".into(),
            issuer: AgentId::new("user-b@local", [2u8; 32]),
            issued_at: 2_000,
            priority: covenant_types::Priority::High,
            parent: Some(Uuid::new_v4()),
        };

        let runner = MockRunner::new("canned response");

        let result_card_a_intent_a = runner.run(&card_a, &intent_a).await.unwrap();
        assert!(
            result_card_a_intent_a.runtime_events.is_empty(),
            "MockRunner::new must produce response.runtime_events as an \
             empty Vec — a refactor that populated runtime_events with \
             synthetic Hermes-style traces would silently leak fake \
             audit rows into every integration test that folds \
             runtime_events into the audit chain, eroding operator \
             trust in the chain's accuracy; got runtime_events.len() = {}",
            result_card_a_intent_a.runtime_events.len(),
        );

        let result_card_b_intent_a = runner.run(&card_b, &intent_a).await.unwrap();
        assert_eq!(
            result_card_a_intent_a, result_card_b_intent_a,
            "two distinct AgentCards (alpha vs beta, different id, \
             name, runtime, and capabilities) must yield byte-identical \
             AgentResult — MockRunner's _card argument is intentionally \
             ignored. A refactor that began filtering the canned \
             response by card.id (e.g., to make MockRunner 'more \
             realistic') would silently break every test fixture that \
             uses MockRunner as a stand-in across multiple agents",
        );

        let result_card_a_intent_b = runner.run(&card_a, &intent_b).await.unwrap();
        assert_eq!(
            result_card_a_intent_a, result_card_a_intent_b,
            "two distinct Intents (different id, text, issuer, \
             issued_at, priority, parent) must yield byte-identical \
             AgentResult — MockRunner's _intent argument is intentionally \
             ignored. A refactor that began varying the response by \
             intent.text would silently break the deterministic-stub \
             contract that test fixtures depend on for cross-intent \
             assertion stability",
        );

        let result_second_call = runner.run(&card_a, &intent_a).await.unwrap();
        assert_eq!(
            result_card_a_intent_a, result_second_call,
            "two sequential calls on the same MockRunner with the same \
             inputs must yield identical AgentResult — MockRunner \
             clone()s self.response on every call (line 520). A \
             refactor that swapped clone for take/replace under a \
             'avoid the clone allocation' rationale would silently \
             break every test fixture that calls .run twice and \
             asserts the result is stable across calls; got \
             first.text={:?} second.text={:?}",
            result_card_a_intent_a.text, result_second_call.text,
        );
        assert_eq!(
            result_second_call.text, "canned response",
            "the second call's text must still be 'canned response' — \
             cross-bind that the multi-call stability assertion above \
             is not tautological (i.e., both calls returning a moved-out \
             empty default would also assert equal)",
        );
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
        // GvisorRunner::ensure_allowed is the defensive-second-line
        // check that rejects manifests the gVisor runner cannot execute
        // safely. Three rejection arms, each with
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
                 GvisorRunner::ensure_allowed would silently let oci_config \
                 canonicalize paths and write the OCI bundle for a non-gVisor \
                 manifest; got: {other:?}"
            ),
        }
    }

    #[test]
    fn gvisor_ensure_allowed_pins_filesystem_and_network_arm_reasons() {
        // GvisorRunner::ensure_allowed rejects manifests that select
        // gVisor but violate the v0 filesystem or network policy. Each
        // arm carries a unique load-bearing reason string:
        //
        //   (1) backend != LinuxGvisor       -> "manifest does not select linux-gvisor"
        //   (2) filesystem != ReadOnlyPackage -> "initial gVisor runner only supports read-only-package filesystem policy"
        //   (3) resources.network != Off      -> "initial gVisor runner only supports network=off"
        //
        // gvisor_ensure_allowed_pins_backend_mismatch_arm_with_reason
        // pins arm 1's reason substring.
        // gvisor_runner_rejects_unenforced_policies exercises
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
                     gvisor_runner_rejects_unenforced_policies would still \
                     pass because it never reads the reason. got: {reason}"
                );
            }
            other => panic!(
                "expected RunnerError::UnsupportedSandboxPolicy with the \
                 filesystem-arm reason for filesystem=host; a refactor \
                 that dropped the filesystem equality check inside \
                 GvisorRunner::ensure_allowed would silently let oci_config \
                 canonicalize paths and write a host-fs OCI bundle that \
                 bypasses the v0 read-only-package invariant; got: {other:?}"
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
                 GvisorRunner::ensure_allowed would silently let oci_config \
                 write an OCI bundle whose linux.namespaces \
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
        // GvisorRunner::redact_stderr is the helper that scrubs host
        // paths from a sandboxed agent's stderr before the
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
        //   (3) Empty-path skip — the `if !path.is_empty()` branch
        //       inside redact_stderr is defensive: a PathBuf::new() has
        //       empty .display() and str::replace("", repl) would
        //       insert the replacement between every character of
        //       stderr. A refactor that dropped the empty check would
        //       render the entire stderr as a sequence of
        //       '<redacted-path>' tokens, hiding the real diagnostic.
        //
        // The $HOME redaction branch in redact_stderr reads global env
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
            "redact_stderr must skip empty paths via its \
             `if !path.is_empty()` branch and only redact the real one; \
             a refactor that dropped the empty-path guard would let \
             str::replace(\"\", repl) insert \
             '<redacted-path>' between every character of stderr, \
             rendering the output as meaningless redacted-path noise \
             instead of the real error message",
        );
    }

    #[test]
    fn gvisor_args_for_pins_each_runtime_kind_interpreter_prefix() {
        // GvisorRunner::args_for is the documented bridge from manifest
        // RuntimeKind to the OCI process.args vector that gVisor's runsc
        // hands to the sandboxed init process. Three arms:
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
        // same three-arm dispatch in its `match card.manifest.agent.runtime`
        // and only the RustBin arm is exercised end-to-end via
        // subprocess_runner_executes_real_script.
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
            GvisorRunner::args_for(&rust_card).unwrap(),
            vec!["/workspace/agent.sh".to_string()],
            "RustBin arm must surface the bare workspace-prefixed entry \
             — a refactor that prepended an interpreter (e.g., 'cargo \
             run' or '/bin/sh') would silently wrap every native-binary \
             agent in an unwanted launcher and the gVisor OCI exec \
             would try to resolve the launcher inside the read-only \
             rootfs with no parse-time signal at the runner layer",
        );

        assert_eq!(
            GvisorRunner::args_for(&py_card).unwrap(),
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
            GvisorRunner::args_for(&node_card).unwrap(),
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

    fn hermes_manifest() -> String {
        r#"
[agent]
id = "hermes-research"
name = "Research via Hermes"
version = "0.1.0"
runtime = "hermes"

[resources]
cpu_ms_per_task = 5000
network = "outbound-https-only"
"#
        .to_string()
    }

    fn subprocess_manifest() -> String {
        r#"
[agent]
id = "local-py"
name = "Local Python"
version = "0.1.0"
runtime = "python3"
entry = "main.py"

[resources]
cpu_ms_per_task = 5000
"#
        .to_string()
    }

    #[tokio::test]
    async fn composite_routes_subprocess_runtime_to_local_backend() {
        // The composite must keep the existing python3/node/rust-bin
        // dispatch untouched. If a refactor that adds a new runtime
        // accidentally widens the Hermes arm or narrows the local arm,
        // a hello-world manifest would silently fail dispatch. Pin the
        // happy path against a sentinel response from a mock local
        // runner.
        let local: Arc<dyn Runner> = Arc::new(MockRunner::new("from local"));
        let hermes: Arc<dyn Runner> = Arc::new(MockRunner::new("from hermes"));
        let composite = CompositeRunner::new(local, Some(hermes));
        let dir = tempdir().unwrap();
        let card = card_for(&subprocess_manifest(), dir.path().to_path_buf());

        let out = composite.run(&card, &dummy_intent()).await.unwrap();
        assert_eq!(out.text, "from local");
    }

    #[tokio::test]
    async fn composite_routes_hermes_runtime_to_hermes_runner() {
        let local: Arc<dyn Runner> = Arc::new(MockRunner::new("from local"));
        let hermes: Arc<dyn Runner> = Arc::new(MockRunner::new("from hermes"));
        let composite = CompositeRunner::new(local, Some(hermes));
        let dir = tempdir().unwrap();
        let card = card_for(&hermes_manifest(), dir.path().to_path_buf());

        let out = composite.run(&card, &dummy_intent()).await.unwrap();
        assert_eq!(out.text, "from hermes");
    }

    #[tokio::test]
    async fn composite_hermes_runtime_without_gateway_returns_hermes_unconfigured() {
        // The operator-error path. A manifest can declare runtime =
        // "hermes" before the gateway env is set; dispatch must fail
        // loud with a typed error rather than blanking into the
        // subprocess path or panicking on the missing runner.
        let local: Arc<dyn Runner> = Arc::new(MockRunner::new("from local"));
        let composite = CompositeRunner::new(local, None);
        let dir = tempdir().unwrap();
        let card = card_for(&hermes_manifest(), dir.path().to_path_buf());

        let err = composite.run(&card, &dummy_intent()).await.unwrap_err();
        match err {
            RunnerError::HermesUnconfigured { agent } => {
                assert_eq!(agent, "hermes-research");
            }
            other => panic!("expected HermesUnconfigured, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hermes_runner_rejects_non_hermes_runtime() {
        // The HermesRunner is wired as a sibling of SubprocessRunner;
        // if CompositeRunner is bypassed and a subprocess agent lands
        // on the Hermes path, fail loud with a typed error.
        let runner = HermesRunner::new("http://127.0.0.1:1", None).unwrap();
        let dir = tempdir().unwrap();
        let card = card_for(&subprocess_manifest(), dir.path().to_path_buf());

        let err = runner.run(&card, &dummy_intent()).await.unwrap_err();
        match err {
            RunnerError::WrongRuntime {
                agent,
                expected,
                got,
            } => {
                assert_eq!(agent, "local-py");
                assert_eq!(expected, "hermes");
                assert_eq!(got, "python3");
            }
            other => panic!("expected WrongRuntime, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_hermes_runners_reject_hermes_runtime_with_wrong_runtime_field_pins() {
        // covenant_runtime documents two loud-fail safety nets at lib.rs
        // lines 180-189 (SubprocessRunner::run) and lines 298-304
        // (GvisorRunner::args_for). Both surfaces emit the SAME
        // RunnerError::WrongRuntime shape for RuntimeKind::Hermes:
        //
        //   expected = "python3|node|rust-bin"
        //   got      = "hermes"
        //   agent    = card.id.clone()
        //
        // The doc comment on the SubprocessRunner arm reads: "Routed via
        // CompositeRunner under normal operation. If SubprocessRunner is
        // reached for a Hermes agent the wiring is broken — fail loud
        // rather than silently." GvisorRunner::args_for carries the
        // symmetric semantic for the gVisor sandbox dispatch path.
        //
        // Existing coverage pins the OPPOSITE direction
        // (HermesRunner rejecting non-Hermes via WrongRuntime; see
        // hermes_runner_rejects_non_hermes_runtime) and the
        // happy-path arms of args_for (RustBin/Python3/Node at
        // gvisor_args_for_pins_each_runtime_kind_interpreter_prefix —
        // the Hermes arm is explicitly skipped). The two
        // non-Hermes-rejects-Hermes paths have no direct test, so a
        // refactor that removed the early WrongRuntime branch under a
        // 'composite already routes correctly, this is dead code'
        // rationale would silently mute the loud-fail safety net.
        let dir = tempdir().unwrap();
        let card = card_for(&hermes_manifest(), dir.path().to_path_buf());

        let runner = SubprocessRunner;
        let err = runner.run(&card, &dummy_intent()).await.unwrap_err();
        match err {
            RunnerError::WrongRuntime {
                agent,
                expected,
                got,
            } => {
                assert_eq!(
                    agent, "hermes-research",
                    "SubprocessRunner.run must populate agent with \
                     card.id verbatim — a refactor that hard-coded a \
                     constant agent name for diagnostics would silently \
                     conflate WrongRuntime errors from different \
                     misconfigured manifests in operator dashboards",
                );
                assert_eq!(
                    expected, "python3|node|rust-bin",
                    "SubprocessRunner.run must emit expected = \
                     'python3|node|rust-bin' verbatim — a refactor that \
                     swapped this for 'subprocess|gvisor' under a \
                     'mirror the runner type rather than the runtime \
                     kinds' rationale would silently break operator \
                     dashboards that grep on the documented strings; \
                     the symmetric GvisorRunner::args_for arm below \
                     carries the same pair, so this pin anchors the \
                     contract for one surface while the next arm \
                     anchors it for the other",
                );
                assert_eq!(
                    got, "hermes",
                    "SubprocessRunner.run must emit got = 'hermes' \
                     verbatim — the string is the operator-facing \
                     diagnostic that points at the misconfigured \
                     manifest runtime field. A refactor that wired \
                     got from a debug-print of RuntimeKind would \
                     surface here if the Debug impl ever drifts",
                );
            }
            other => panic!(
                "SubprocessRunner.run with a Hermes manifest must \
                 surface RunnerError::WrongRuntime — the early branch \
                 at line 184 is the documented loud-fail safety net; \
                 a refactor that removed it under a 'composite already \
                 routes, this is dead code' rationale would surface \
                 here as an IO error or a successful spawn against a \
                 non-existent entry path. Got {other:?}"
            ),
        }

        let err = GvisorRunner::args_for(&card).unwrap_err();
        match err {
            RunnerError::WrongRuntime {
                agent,
                expected,
                got,
            } => {
                assert_eq!(agent, "hermes-research");
                assert_eq!(
                    expected, "python3|node|rust-bin",
                    "GvisorRunner::args_for must emit the same \
                     expected string as SubprocessRunner.run — both \
                     surfaces carry the documented loud-fail safety \
                     net for the same set of supported runtimes; a \
                     one-sided drift (e.g., one surface updated to \
                     include a future runtime kind while the other is \
                     forgotten) would surface here as a string \
                     mismatch against the SubprocessRunner-side pin",
                );
                assert_eq!(got, "hermes");
            }
            other => panic!(
                "GvisorRunner::args_for with a Hermes manifest must \
                 surface RunnerError::WrongRuntime — the early branch \
                 at line 298 mirrors the SubprocessRunner safety net; \
                 a refactor that removed it would let the args_for \
                 dispatch reach the end of the match without producing \
                 a value, which the compiler would catch via the \
                 exhaustive match. Got {other:?}"
            ),
        }
    }

    #[test]
    fn runner_error_sandbox_display_messages_pin_four_string_variant_format_strings() {
        let sandbox_required = format!(
            "{}",
            RunnerError::SandboxRequired {
                agent: "researcher@local".into(),
                required: SandboxBackend::LinuxGvisor,
            }
        );
        assert_eq!(
            sandbox_required,
            "agent researcher@local requires sandbox backend LinuxGvisor; active runner is trusted-local",
            "RunnerError::SandboxRequired Display drifted (typo or dropped 'active runner is trusted-local' qualifier regression class)"
        );

        let unsupported = format!(
            "{}",
            RunnerError::UnsupportedSandboxPolicy {
                agent: "researcher@local".into(),
                backend: SandboxBackend::LinuxGvisor,
                reason: "host network not supported".into(),
            }
        );
        assert_eq!(
            unsupported,
            "agent researcher@local cannot run on sandbox backend LinuxGvisor: host network not supported",
            "RunnerError::UnsupportedSandboxPolicy Display drifted (typo or slot-swap regression class — \
             reason must appear after the backend, separated by ': ')"
        );

        let wrong_runtime = format!(
            "{}",
            RunnerError::WrongRuntime {
                agent: "researcher@local".into(),
                expected: "subprocess",
                got: "hermes",
            }
        );
        assert_eq!(
            wrong_runtime,
            "agent researcher@local declares runtime \"hermes\" but this runner only handles \"subprocess\"",
            "RunnerError::WrongRuntime Display drifted (typo, slot-swap, or debug-vs-display regression class — \
             got is what the AGENT declared; expected is what the RUNNER handles)"
        );

        let hermes_unconfigured = format!(
            "{}",
            RunnerError::HermesUnconfigured {
                agent: "researcher@local".into(),
            }
        );
        assert_eq!(
            hermes_unconfigured,
            "agent researcher@local declares runtime \"hermes\" but no Hermes gateway is configured \
             — set HERMES_API_BASE_URL",
            "RunnerError::HermesUnconfigured Display drifted (typo or dropped 'HERMES_API_BASE_URL' env-var name regression class)"
        );
    }

    #[test]
    fn runner_error_execution_display_messages_pin_four_string_variant_format_strings() {
        let timeout = format!("{}", RunnerError::Timeout(Duration::from_secs(30)));
        assert!(
            timeout.contains("timed out after") && timeout.contains("30s"),
            "RunnerError::Timeout Display drifted (typo or {{0:?}} → {{0}} regression class): {timeout}"
        );

        let non_zero = format!(
            "{}",
            RunnerError::NonZeroExit {
                status: 137,
                stderr: "killed".into(),
            }
        );
        assert_eq!(
            non_zero, "agent exited non-zero: status=137, stderr=killed",
            "RunnerError::NonZeroExit Display drifted (typo or slot-swap regression class — \
             status= must precede stderr= and they must be separated by ', ')"
        );

        let not_executable = format!("{}", RunnerError::NotExecutable("researcher@local".into()));
        assert_eq!(
            not_executable,
            "agent researcher@local has no manifest or package_dir set; cannot execute",
            "RunnerError::NotExecutable Display drifted (typo or dropped 'no manifest or package_dir set' field-name hint regression class)"
        );

        let remote = format!(
            "{}",
            RunnerError::Remote {
                status: 503,
                message: "gateway unavailable".into(),
            }
        );
        assert_eq!(
            remote, "remote runtime: status=503 message=gateway unavailable",
            "RunnerError::Remote Display drifted (typo or dropped 'remote runtime:' prefix regression class — \
             would lose the local-vs-remote disambiguator)"
        );
        assert_ne!(
            remote, non_zero,
            "RunnerError::Remote must not converge with RunnerError::NonZeroExit \
             (prefix-convergence regression class would merge remote-gateway with local-subprocess incidents)"
        );
    }

    #[test]
    fn runner_error_malformed_stdout_display_message_pins_prefix_agent_result_qualifier_and_display_formatted_source(
    ) {
        let source =
            serde_json::from_str::<serde_json::Value>("not json").expect_err("parse must fail");
        let err = RunnerError::MalformedStdout { source };
        let message = format!("{err}");
        assert!(
            message.starts_with("agent stdout was not a valid AgentResult JSON line: "),
            "RunnerError::MalformedStdout must surface the literal source-of-failure prefix 'agent stdout was not a valid AgentResult JSON line: ' so audit-log filters can group runner-subprocess malformed-output incidents separately from manifest or capability parse incidents (dropped-prefix / dropped-wire-shape-qualifier regression class): {message}"
        );
        assert!(
            message.contains("agent stdout"),
            "RunnerError::MalformedStdout must surface the 'agent stdout' source-of-failure qualifier so audit-log filters can distinguish agent-subprocess malformed-output from sibling parse rejections (dropped-source-of-failure-qualifier regression class): {message}"
        );
        assert!(
            message.contains("AgentResult JSON line"),
            "RunnerError::MalformedStdout must surface the 'AgentResult JSON line' wire-shape qualifier so operators know the expected wire shape is the AgentResult JSON struct, not arbitrary JSON or multi-line text (dropped-wire-shape-qualifier regression class): {message}"
        );
        assert!(
            message.contains("expected"),
            "RunnerError::MalformedStdout must surface the serde_json::Error's Display rendering after the colon (serde renders parse failures with 'expected ...'); a refactor from {{source}} to {{source:?}} would surface Debug-rendered struct fields like 'Error(' or 'line:' instead (Debug-vs-Display formatting regression class on the source interpolation): {message}"
        );
        assert!(
            !message.contains("Error("),
            "RunnerError::MalformedStdout must NOT surface the serde_json::Error's Debug rendering; a Debug-formatted source would expose internal struct fields like 'Error(\"...\", line: N, column: M)' that can leak buffer position structs into operator logs (Debug-vs-Display formatting regression class on the source interpolation): {message}"
        );
    }

    #[test]
    fn runner_error_malformed_stdout_source_delegation_pin_returns_inner_serde_json_error_via_std_error_source(
    ) {
        use std::error::Error;
        let source =
            serde_json::from_str::<serde_json::Value>("not json").expect_err("parse must fail");
        let expected_display = format!("{source}");
        let err = RunnerError::MalformedStdout { source };
        let inner = err.source().expect(
            "RunnerError::MalformedStdout must surface the inner serde_json::Error via std::error::Error::source so anyhow chain printers, tracing's source-walking emitters, and daemon audit-log emitters that downcast source() to serde_json::Error to extract line/column can render the wrapper context AND the inner cause; a thiserror refactor that dropped the #[source] attribute on the source field (e.g., field rename without re-annotation, or removing the explicit #[source] attribute under a 'simpler error wrapping' rationale) would silently change source() to return None while leaving Display intact (dropped-source-attribute regression class)",
        );
        let inner_message = format!("{inner}");
        assert!(
            inner_message.contains("expected"),
            "RunnerError::MalformedStdout source() must return an error whose Display contains 'expected' — the serde_json::Error Display rendering for a parse failure; a refactor that wrapped the source in a different type (e.g., Box<dyn Error>) and dropped the literal serde_json parse-failure prose would silently mute the structural parse-failure discriminator in chain-walked output (concrete-source-type regression class): {inner_message}"
        );
        assert_eq!(
            inner_message, expected_display,
            "RunnerError::MalformedStdout source() Display must match a direct format!() of the same serde_json::Error verbatim; a refactor that swapped the source field type to Box<dyn Error + Send + Sync> or any other wrapper would silently break daemon-side callsite downcasts to the concrete serde_json::Error type used to extract line/column for audit-log triage (concrete-source-type regression class)"
        );
    }

    #[test]
    fn runner_error_io_and_serde_display_messages_pin_prefix_and_external_source_display_delegation(
    ) {
        let io_err = RunnerError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "agent.toml missing",
        ));
        let io_message = format!("{io_err}");
        assert!(
            io_message.starts_with("io: "),
            "RunnerError::Io must surface the literal 'io: ' bootstrap-stage prefix so audit-log filters can distinguish runner pipe/file-read faults from JSON-parse faults (dropped-prefix regression class): {io_message}"
        );
        assert!(
            io_message.contains("agent.toml missing"),
            "RunnerError::Io must surface the inner std::io::Error Display rendering after the colon ({{0}}, not {{0:?}}); a Debug refactor would render 'Custom {{ kind: NotFound, error: ... }}' instead of the message payload (Debug-vs-Display formatting regression class on the {{0}} interpolation): {io_message}"
        );
        assert!(
            !io_message.contains("Custom {") && !io_message.contains("Os {"),
            "RunnerError::Io must NOT surface the std::io::Error Debug rendering; a Debug refactor on {{0}} would expose internal struct fields like 'Custom {{ kind: ..., error: ... }}' or 'Os {{ code: ..., kind: ..., message: ... }}' (Debug-vs-Display formatting regression class on the {{0}} interpolation): {io_message}"
        );

        let serde_source =
            serde_json::from_str::<serde_json::Value>("not json").expect_err("parse must fail");
        let serde_err = RunnerError::Serde(serde_source);
        let serde_message = format!("{serde_err}");
        assert!(
            serde_message.starts_with("serde: "),
            "RunnerError::Serde must surface the literal 'serde: ' bootstrap-stage prefix so audit-log filters can distinguish runner JSON-parse faults from pipe/file-read faults (dropped-prefix regression class): {serde_message}"
        );
        assert!(
            serde_message.contains("expected"),
            "RunnerError::Serde must surface the inner serde_json::Error Display rendering after the colon (serde_json renders parse failures with 'expected ...' Display strings); a Debug refactor on {{0}} would render 'Error(\"...\", line: N, column: M)' instead (Debug-vs-Display formatting regression class on the {{0}} interpolation): {serde_message}"
        );
        assert!(
            !serde_message.contains("Error("),
            "RunnerError::Serde must NOT surface the serde_json::Error Debug rendering; a Debug refactor on {{0}} would expose 'Error(\"...\", line: N, column: M)' buffer-position structs (Debug-vs-Display formatting regression class on the {{0}} interpolation): {serde_message}"
        );

        assert_ne!(
            io_message, serde_message,
            "RunnerError::Io and RunnerError::Serde Display must not converge; merging the two prefixes would lose the pipe/file-read vs JSON-parse-fault discriminator (prefix-convergence regression class): io={io_message} serde={serde_message}"
        );
        assert!(
            !io_message.starts_with("serde:") && !serde_message.starts_with("io:"),
            "RunnerError::Io must not start with 'serde:' and RunnerError::Serde must not start with 'io:'; a sibling-prefix swap would silently mis-route incident triage (sibling-prefix-swap regression class): io={io_message} serde={serde_message}"
        );
        let malformed_prefix = "agent stdout was not a valid AgentResult JSON line:";
        assert!(
            !io_message.starts_with(malformed_prefix),
            "RunnerError::Io must not converge with RunnerError::MalformedStdout prefix; a pipe/file-read fault must not be mis-routed as an AgentResult-stdout-parse fault (MalformedStdout-convergence regression class): {io_message}"
        );
        assert!(
            !serde_message.starts_with(malformed_prefix),
            "RunnerError::Serde must not converge with RunnerError::MalformedStdout prefix; a generic JSON-parse fault must not be mis-routed as an AgentResult-stdout-parse fault (MalformedStdout-convergence regression class): {serde_message}"
        );
    }

    #[test]
    fn runner_error_io_source_delegation_pin_returns_inner_std_io_error_via_std_error_source() {
        use std::error::Error;

        let inner = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "agent stdin closed");
        let expected_display = format!("{inner}");
        let err = RunnerError::Io(inner);
        let source = err.source().expect(
            "covenant_runtime::RunnerError::Io must surface the inner std::io::Error via std::error::Error::source so daemon-side runtime retry-policy classifiers can walk the error chain and downcast source() to std::io::Error to extract io::ErrorKind for distinct decisions on subprocess IO (NotFound stops dispatch with operator-attention on a missing entry, PermissionDenied escalates as security-sensitive on a non-executable binary, BrokenPipe stops the in-flight call with retry-on-next-task semantics, Interrupted retries immediately); a refactor that converted the variant from #[from] to a hand-written Error impl returning None (under a 'simpler error wrapping' rationale) would silently change source() to return None while leaving Display intact (dropped-source-attribute regression class)",
        );
        assert_eq!(
            format!("{source}"),
            expected_display,
            "covenant_runtime::RunnerError::Io source() Display must match a direct format!() of the same std::io::Error verbatim; a refactor that swapped the inner field type to Box<dyn Error + Send + Sync> or any other wrapper would silently break daemon-side downcasts even though the wrapper's Display would continue to flow through {{0}} (concrete-source-type regression class)"
        );
        let kind = source.downcast_ref::<std::io::Error>().map(|e| e.kind());
        assert_eq!(
            kind,
            Some(std::io::ErrorKind::BrokenPipe),
            "covenant_runtime::RunnerError::Io source() must downcast_ref to std::io::Error so daemon-side runtime retry-policy classifiers can extract io::ErrorKind for retry decisions on subprocess IO; a refactor that wrapped the inner in a project-local newtype (e.g., RunnerIoError(std::io::Error) under a 'tag runner subprocess IO failures distinctly from sibling Io variants in other crates' rationale) would silently break downcast_ref::<std::io::Error>() at every downstream callsite that classifies subprocess IO faults (concrete-source-type downcast regression class)"
        );
    }

    #[test]
    fn runner_error_serde_source_delegation_pin_returns_inner_serde_json_error_via_std_error_source(
    ) {
        use std::error::Error;

        let inner = serde_json::from_str::<serde_json::Value>("not json")
            .expect_err("parse must fail");
        let expected_display = format!("{inner}");
        let err = RunnerError::Serde(inner);
        let source = err.source().expect(
            "covenant_runtime::RunnerError::Serde must surface the inner serde_json::Error via std::error::Error::source so daemon-side runtime diagnostics can walk the error chain and downcast source() to serde_json::Error to inspect line/column or classify() for generic JSON-parse-failure triage on the non-AgentResult-stdout JSON paths (manifest fragments, subprocess control messages, dispatcher state); a refactor that converted the variant from #[from] to a hand-written Error impl returning None (under a 'simpler error wrapping' rationale) would silently change source() to return None while leaving Display intact (dropped-source-attribute regression class). RunnerError::MalformedStdout has its own #[source]-attributed serde_json::Error for the AgentResult-stdout parse path; this pin is for the sibling generic-JSON-parse variant",
        );
        assert_eq!(
            format!("{source}"),
            expected_display,
            "covenant_runtime::RunnerError::Serde source() Display must match a direct format!() of the same serde_json::Error verbatim; a refactor that swapped the inner field type to Box<dyn Error + Send + Sync> or any other wrapper would silently break daemon-side downcasts even though the wrapper's Display would continue to flow through {{0}} (concrete-source-type regression class)"
        );
        assert!(
            source.downcast_ref::<serde_json::Error>().is_some(),
            "covenant_runtime::RunnerError::Serde source() must downcast_ref to serde_json::Error so daemon-side runtime diagnostics can call serde_json::Error::line/column/classify for generic JSON-parse-fault triage; a refactor that wrapped the inner in a project-local newtype (e.g., RunnerSerdeError(serde_json::Error) under a 'consolidate parse errors into one Wire variant or merge with MalformedStdout' rationale) would silently break downcast_ref::<serde_json::Error>() at every downstream callsite that classifies generic runtime JSON-parse faults AND would blur the AgentResult-stdout vs generic-JSON-parse discriminator pinned in runner_error_malformed_stdout_source_delegation_pin (concrete-source-type downcast regression class)"
        );
    }
}
