//! Agent runtime for Covenant.
//!
//! Spawns an agent as a subprocess, feeds the [`Intent`] as one JSON
//! line on stdin, reads the [`AgentResult`] as one JSON line on
//! stdout, and enforces the per-task budget declared in the agent's
//! manifest (`resources.cpu_ms_per_task`) via two paths: the daemon's
//! projection tick preempts subprocesses on projected overshoot through
//! [`preempt_subprocess_pg`] (`SIGTERM` with grace, then `SIGKILL`), and
//! a wall-clock kill at `cpu_ms_per_task` fires as the final backstop.
//!
//! The base implementation is `trusted-local` execution, not sandbox-grade
//! isolation. Three concrete backends implement the [`Runner`] trait:
//! [`SubprocessRunner`] for the trusted-local path, [`GvisorRunner`] for
//! sandboxed dispatch via the `runsc` OCI runtime, and [`HermesRunner`]
//! for the Hermes agent runtime. Additional backends plug in via the
//! same trait without changing the dispatch contract.

// Production builds reject unsafe code by default. Two narrowly scoped
// surfaces hold a function-level `#[allow(unsafe_code)]`: the test that
// reads the kernel-reported process group id via `libc::getpgid` (failure
// mode 5 of the budget-hard-preempt-subprocess parent task) and
// `preempt_subprocess_pg`, which calls `libc::kill` on a process group
// the daemon already owns the pid of. Every other call site stays under
// the crate-wide deny.
#![cfg_attr(not(test), deny(unsafe_code))]

use async_trait::async_trait;
use covenant_manifest::{FilesystemPolicy, NetworkPolicy, Runtime as RuntimeKind, SandboxBackend};
use covenant_router::AgentCard;
use covenant_types::Intent;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tracing::{debug, warn};
use uuid::Uuid;

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
    /// Workspace files captured at the end of a Hermes run (the gateway
    /// snapshots them before the ephemeral sandbox is destroyed) so the UI
    /// can show a file tree / preview of what was built. Empty for runners
    /// that don't build in a sandbox.
    #[serde(default)]
    pub files: Vec<BuildFile>,
}

/// A single workspace file captured from a finished run. `content` is UTF-8
/// (binaries are skipped) and may be `truncated` if the file was large.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildFile {
    pub path: String,
    pub content: String,
    #[serde(default)]
    pub truncated: bool,
}

/// A [`RuntimeTrace`] streamed live during a run, tagged with the intent it
/// belongs to (and its issuer) so the daemon can fold it into the audit chain
/// the moment it arrives — turning the task page into a live "watch it work"
/// view instead of dumping the whole trail when the run finishes.
#[derive(Debug, Clone)]
pub struct StreamedTrace {
    pub intent_id: uuid::Uuid,
    pub issuer: covenant_types::AgentId,
    pub trace: RuntimeTrace,
}

/// Channel the daemon hands to the Hermes runner to receive [`StreamedTrace`]s
/// as the run streams them. The daemon drains the receiver and writes each as
/// an audit row.
pub type RuntimeEventSink = tokio::sync::mpsc::UnboundedSender<StreamedTrace>;

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
    /// Hermes wrote a file inside the sandbox workspace. Low-volume
    /// and structural (distinct from `message.delta` / `reasoning.available`
    /// which the runtime intentionally drops at the SSE seam in
    /// `hermes::map_hermes_event` because they are too high-volume to
    /// audit). `path` is the sandbox-relative path; `bytes` is the file
    /// size as reported by the gateway, kept `u64` so a multi-GB write
    /// never silently truncates.
    HermesFileWritten {
        run_id: String,
        path: String,
        bytes: u64,
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
    /// The remote run reached a terminal `failed` state — an agent
    /// outcome, distinct from a transport/gateway fault ([`Self::Remote`]).
    #[error("remote run failed: {message}")]
    RunFailed { message: String },
    /// The remote run was cancelled (by operator stop or upstream).
    /// An outcome, not a transport fault.
    #[error("remote run was cancelled")]
    RunCancelled,
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

/// In-memory record of a running subprocess managed by the daemon. The
/// fields are the minimum the budget-hard-preempt path needs: `pid` for
/// the signal target, `agent_id` for audit attribution, and
/// `started_at_ms` for projection extrapolation. This struct is the
/// stable primitive that future sub-slices (projection, signal dispatch,
/// audit emission) join against; this slice defines the type without
/// integrating it into the runner spawn path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedSubprocess {
    pub agent_id: String,
    pub pid: u32,
    pub started_at_ms: u64,
}

/// Daemon-side in-memory tracker of subprocesses spawned for in-flight
/// intents. Keyed by `intent_id: Uuid` so the budget-projection tick
/// (sub-slice B) and signal dispatcher (sub-slice C) can look up the
/// spawn record without scanning the entire process table.
///
/// In-memory only by design: a daemon restart loses the tracker and the
/// orphan subprocesses outlive their preempt window. Recovery via
/// pidfile or `/proc` scan is a separate followup slice documented in
/// the parent task's expected failure mode 3.
#[derive(Debug, Default)]
pub struct SubprocessTracker {
    entries: RwLock<HashMap<Uuid, TrackedSubprocess>>,
}

impl SubprocessTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a tracker entry. If `intent_id` is already present the
    /// existing entry is overwritten — this should not happen under
    /// normal dispatch flow and may indicate a race in the runner
    /// integration that this slice intentionally does not yet wire.
    pub fn register(&self, intent_id: Uuid, entry: TrackedSubprocess) {
        let mut guard = self
            .entries
            .write()
            .expect("subprocess tracker rwlock poisoned");
        guard.insert(intent_id, entry);
    }

    /// Removes the tracker entry for `intent_id` if present, returning
    /// the previous value for callers that want to audit the lifetime.
    pub fn unregister(&self, intent_id: &Uuid) -> Option<TrackedSubprocess> {
        let mut guard = self
            .entries
            .write()
            .expect("subprocess tracker rwlock poisoned");
        guard.remove(intent_id)
    }

    pub fn get(&self, intent_id: &Uuid) -> Option<TrackedSubprocess> {
        let guard = self
            .entries
            .read()
            .expect("subprocess tracker rwlock poisoned");
        guard.get(intent_id).cloned()
    }

    pub fn len(&self) -> usize {
        let guard = self
            .entries
            .read()
            .expect("subprocess tracker rwlock poisoned");
        guard.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns a copy of every `(intent_id, entry)` pair. The read
    /// guard is dropped before the Vec is returned so a long-running
    /// snapshot caller — for instance, the daemon-side budget
    /// projection tick that walks each entry and awaits a budget
    /// ledger lookup — cannot stall register/unregister. Order is
    /// unspecified; callers must not depend on insertion order.
    pub fn snapshot(&self) -> Vec<(Uuid, TrackedSubprocess)> {
        let guard = self
            .entries
            .read()
            .expect("subprocess tracker rwlock poisoned");
        guard.iter().map(|(k, v)| (*k, v.clone())).collect()
    }
}

/// Outcome of one [`preempt_subprocess_pg`] call. The daemon-side
/// budget-preempt path maps each variant to either `AuditKind::BudgetPreempted`
/// or `AuditKind::BudgetPreemptFailed`:
///
/// - [`PreemptOutcome::AlreadyDead`] → `BudgetPreempted { signal_sent: "none" }`
///   (the group had no members when the dispatcher fired; the daemon may
///   also choose to emit `BudgetPreemptFailed { errno: ESRCH }`; the
///   dispatcher does not encode that policy decision).
/// - [`PreemptOutcome::PermissionDenied`] → `BudgetPreemptFailed { errno: EPERM }`
///   (security incident: the daemon lacks signal-send permission for that
///   pid; the errno is surfaced so the audit row is identical to the
///   syscall return).
/// - [`PreemptOutcome::ExitedDuringGrace`] → `BudgetPreempted { signal_sent: "SIGTERM" }`
///   (the SIGTERM landed and the group exited inside `grace`; no SIGKILL
///   was sent).
/// - [`PreemptOutcome::SigKilled`] → `BudgetPreempted { signal_sent: "SIGKILL" }`
///   (grace expired with the group still alive; SIGKILL was dispatched).
/// - [`PreemptOutcome::UnsupportedPlatform`] is the explicit non-Unix
///   stub; the daemon must not treat it as a successful preempt.
///
/// Serde is pinned (`tag = "kind"`, `snake_case`) so daemon-side audit
/// emitters can deserialize the outcome from a wire envelope without
/// reaching into this crate's enum shape. The `errno` field on
/// `PermissionDenied` matches the runtime crate's `i32` representation
/// of the libc errno.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PreemptOutcome {
    AlreadyDead,
    PermissionDenied { errno: i32 },
    ExitedDuringGrace,
    SigKilled,
    UnsupportedPlatform,
}

/// Sends SIGTERM to the process group led by `pid`, polls every 50ms up
/// to `grace` for the group to drain, then sends SIGKILL if any member
/// is still alive. Returns a [`PreemptOutcome`] describing what
/// happened; emitting the resulting audit row is the daemon's
/// responsibility.
///
/// `pid` is the process group leader's pid (the spawned subprocess'
/// own pid, because the runner calls `process_group(0)` on the
/// `std::process::Command`). The function targets `-pid` so the signal
/// reaches every grandchild the agent forked into that group, not just
/// the immediate child.
///
/// The poll loop uses `libc::kill(-pid, 0)` for liveness — the only
/// non-blocking way to detect group death without owning the child
/// handle (which the runner's `child.wait()` still owns). `waitpid` is
/// intentionally avoided here so this function composes cleanly with
/// the existing runner spawn path.
///
/// Total elapsed time is tracked against `grace` via
/// [`std::time::Instant`] so a heavily loaded scheduler cannot push
/// SIGKILL past the operator-configured budget by more than the
/// remaining poll interval.
#[cfg(unix)]
#[allow(unsafe_code)]
pub async fn preempt_subprocess_pg(pid: u32, grace: Duration) -> PreemptOutcome {
    use std::io;

    // `libc::kill(-pid, ...)` signals every member of the process group
    // whose pgid == pid. The runner's `configure_process_group(0)` makes
    // that pgid equal to the spawned child's pid.
    let target = -(pid as i32);

    // SAFETY: `kill` is async-signal-safe and operates on a pid the
    // daemon already chose to spawn under its own user. No memory is
    // touched by this syscall; the only side effect is the signal
    // delivery itself.
    let rc = unsafe { libc::kill(target, libc::SIGTERM) };
    if rc == -1 {
        let errno = io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EINVAL);
        if errno == libc::ESRCH {
            return PreemptOutcome::AlreadyDead;
        }
        return PreemptOutcome::PermissionDenied { errno };
    }

    let start = std::time::Instant::now();
    let tick = Duration::from_millis(50);
    while start.elapsed() < grace {
        let remaining = grace.saturating_sub(start.elapsed());
        tokio::time::sleep(tick.min(remaining)).await;
        // SAFETY: signal 0 performs the same permission and existence
        // check as a real signal without delivering one. Safe under the
        // same justification as the SIGTERM call above.
        let rc = unsafe { libc::kill(target, 0) };
        if rc == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return PreemptOutcome::ExitedDuringGrace;
        }
    }

    // SAFETY: see SIGTERM call. SIGKILL is unblockable; the only
    // failure paths are ESRCH (the group drained between the last poll
    // and now — race, treat as ExitedDuringGrace) or EPERM (which
    // cannot occur if the initial SIGTERM succeeded under the same uid;
    // we treat any non-ESRCH error as "we sent SIGKILL" because the
    // intent was to kill and the audit row should reflect that).
    let rc = unsafe { libc::kill(target, libc::SIGKILL) };
    if rc == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        return PreemptOutcome::ExitedDuringGrace;
    }
    PreemptOutcome::SigKilled
}

#[cfg(not(unix))]
pub async fn preempt_subprocess_pg(_pid: u32, _grace: Duration) -> PreemptOutcome {
    PreemptOutcome::UnsupportedPlatform
}

#[derive(Default)]
pub struct SubprocessRunner {
    tracker: Option<Arc<SubprocessTracker>>,
}

/// RAII guard that keeps a [`SubprocessTracker`] entry alive for the
/// duration of one dispatch. The runner constructs this immediately
/// after `cmd.spawn` returns with a known pid and binds it to `_guard`
/// for the rest of `run()`; on every exit path (success, error,
/// timeout-driven kill, panic) the Drop impl removes the entry so the
/// tracker cannot leak rows for completed dispatches. Failure mode 1
/// of the parent task closes here.
struct TrackerGuard<'a> {
    tracker: &'a SubprocessTracker,
    intent_id: Uuid,
}

impl Drop for TrackerGuard<'_> {
    fn drop(&mut self) {
        self.tracker.unregister(&self.intent_id);
    }
}

impl SubprocessRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a runner that registers each spawned subprocess in the
    /// provided [`SubprocessTracker`] keyed by `intent.id`. The runner
    /// holds the `Arc` so the daemon can keep its own clone to walk
    /// later (projection tick, dispatcher consumer).
    pub fn with_tracker(tracker: Arc<SubprocessTracker>) -> Self {
        Self {
            tracker: Some(tracker),
        }
    }

    fn ensure_allowed(&self, card: &AgentCard) -> Result<(), RunnerError> {
        if card.manifest.sandbox.required {
            return Err(RunnerError::SandboxRequired {
                agent: card.id.clone(),
                required: card.manifest.sandbox.backend,
            });
        }
        Ok(())
    }

    /// Configures the underlying std command so the spawned subprocess
    /// becomes the leader of a new process group. This is the
    /// load-bearing precondition for hard-preempt: a future
    /// `libc::kill(-pid, SIGTERM)` will then reach every grandchild the
    /// agent forked, not just the immediate child. Without this, an
    /// agent that forks before exiting leaves orphan processes running
    /// past the budget window.
    ///
    /// Tokio's `Command` wraps `std::process::Command`; the Unix
    /// extension's `process_group` must be set on the std command
    /// *before* `spawn`. Calling it via `as_std_mut()` here keeps the
    /// rest of the spawn path on the tokio Command.
    #[cfg(unix)]
    fn configure_process_group(cmd: &mut Command) {
        use std::os::unix::process::CommandExt;
        // `process_group(0)` requests a new process group whose group
        // id equals the child's pid — i.e. the child becomes its own
        // group leader. Subsequent signals to `-pid` target every
        // process in that group.
        cmd.as_std_mut().process_group(0);
    }

    #[cfg(not(unix))]
    fn configure_process_group(_cmd: &mut Command) {
        // Process groups are a POSIX concept; on non-Unix platforms
        // the budget-hard-preempt path falls back to per-pid kill,
        // which loses the grandchild guarantee. The covenantd
        // documentation calls this out as a platform gap.
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
        Self::configure_process_group(&mut cmd);

        debug!(agent = %card.id, entry = %card.manifest.agent.entry, "spawning agent");
        let mut child = cmd.spawn()?;

        // Track the subprocess for the dispatch's lifetime when the
        // runner was constructed with a tracker. The guard's Drop impl
        // unregisters on every exit path (success, error, timeout-driven
        // kill); the binding to `_tracker_guard` must outlive the
        // child.wait() below so a stdin-write or readback failure does
        // not silently drop the registration while the child is still
        // alive.
        let _tracker_guard = match (self.tracker.as_ref(), child.id()) {
            (Some(tracker), Some(pid)) => {
                tracker.register(
                    intent.id,
                    TrackedSubprocess {
                        agent_id: card.id.clone(),
                        pid,
                        started_at_ms: SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0),
                    },
                );
                Some(TrackerGuard {
                    tracker,
                    intent_id: intent.id,
                })
            }
            _ => None,
        };

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
    tracker: Option<Arc<SubprocessTracker>>,
}

impl GvisorRunner {
    pub fn new(rootfs: impl Into<PathBuf>) -> Self {
        Self {
            runsc_path: PathBuf::from("runsc"),
            rootfs: rootfs.into(),
            scratch_root: std::env::temp_dir().join("covenant-gvisor"),
            tracker: None,
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
            tracker: None,
        }
    }

    /// Builds a gVisor runner that registers the spawned `runsc` pid in
    /// the provided [`SubprocessTracker`] keyed by `intent.id`. The
    /// runsc process is the kernel-visible child; the agent process
    /// inside the sandbox is in a different pid namespace and is not
    /// directly signalable from the host. Signaling runsc's process
    /// group (after `configure_process_group(0)` runs) propagates
    /// termination into the sandbox.
    pub fn with_paths_and_tracker(
        runsc_path: impl Into<PathBuf>,
        rootfs: impl Into<PathBuf>,
        scratch_root: impl Into<PathBuf>,
        tracker: Arc<SubprocessTracker>,
    ) -> Self {
        Self {
            runsc_path: runsc_path.into(),
            rootfs: rootfs.into(),
            scratch_root: scratch_root.into(),
            tracker: Some(tracker),
        }
    }

    /// Mirrors `SubprocessRunner::configure_process_group`. Without
    /// this, `runsc` inherits the daemon's process group and a future
    /// `libc::kill(-runsc_pid, SIGTERM)` either misses runsc or hits
    /// the daemon itself. POSIX-only; non-Unix platforms have no
    /// process groups.
    #[cfg(unix)]
    fn configure_process_group(cmd: &mut Command) {
        use std::os::unix::process::CommandExt;
        cmd.as_std_mut().process_group(0);
    }

    #[cfg(not(unix))]
    fn configure_process_group(_cmd: &mut Command) {}

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
        let mut cmd = Command::new(&self.runsc_path);
        cmd.arg("run")
            .arg("--bundle")
            .arg(&bundle_dir)
            .arg(&bundle_id)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        Self::configure_process_group(&mut cmd);

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                Self::cleanup_bundle(&bundle_dir);
                return Err(RunnerError::Io(e));
            }
        };

        let _tracker_guard = match (self.tracker.as_ref(), child.id()) {
            (Some(tracker), Some(pid)) => {
                tracker.register(
                    intent.id,
                    TrackedSubprocess {
                        agent_id: card.id.clone(),
                        pid,
                        started_at_ms: SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0),
                    },
                );
                Some(TrackerGuard {
                    tracker,
                    intent_id: intent.id,
                })
            }
            _ => None,
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
                files: Vec::new(),
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
            files: vec![],
        };
        let wire = serde_json::to_value(&empty).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({"text": "hi", "sources": [], "runtime_events": [], "files": []}),
            "empty sources must serialize as an explicit empty array; a future skip_serializing_if would silently drop the key",
        );
    }

    #[test]
    fn runtime_trace_serde_pins_snake_case_tag_and_per_variant_field_set() {
        // RuntimeTrace is the mid-run signal enum the
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

        let file_written = RuntimeTrace::HermesFileWritten {
            run_id: "r1".into(),
            path: "src/main.rs".into(),
            bytes: 1_024,
        };
        let file_written_wire = serde_json::to_value(&file_written).unwrap();
        assert_eq!(
            file_written_wire,
            serde_json::json!({
                "type": "hermes_file_written",
                "run_id": "r1",
                "path": "src/main.rs",
                "bytes": 1_024,
            }),
            "HermesFileWritten slug + field names must match the SSE decoder; \
             a rename of bytes to size or a narrowing of bytes from u64 would \
             surface here before it silently truncated a real audit row",
        );
        assert_eq!(
            serde_json::from_value::<RuntimeTrace>(file_written_wire).unwrap(),
            file_written,
            "HermesFileWritten round-trip must reconstruct the original variant",
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
        // MockRunner::new constructs an AgentResult with
        // text=input, sources=empty Vec, runtime_events=empty Vec.
        // MockRunner::run clones this canned response on
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
             clone()s self.response on every call. A \
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
        let r = SubprocessRunner::new()
            .run(&card, &dummy_intent())
            .await
            .unwrap();
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
        let result = SubprocessRunner::new().run(&card, &dummy_intent()).await;
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
        let r = SubprocessRunner::new().run(&card, &dummy_intent()).await;
        assert!(matches!(r, Err(RunnerError::Timeout(_))));
    }

    #[tokio::test]
    async fn subprocess_runner_tracker_records_entry_during_dispatch_and_clears_after() {
        // Spawns a slow-but-completing script and polls the tracker
        // concurrently. While the dispatch is in flight there must be
        // exactly one entry keyed by intent.id with the spawned pid and
        // a fresh started_at_ms; after the dispatch returns the tracker
        // must be empty. Failure mode 1 of the wire-tracker slice closes
        // here: a RegisterGuard whose Drop ran before child.wait()
        // would surface as an empty tracker during the poll.
        let dir = tempdir().unwrap();
        let script = dir.path().join("slow_ok.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\ncat >/dev/null\nsleep 0.3\nprintf '%s\\n' '{\"text\":\"ok\",\"sources\":[]}'\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let manifest_toml = r#"
[agent]
id = "tracked"
name = "Tracked"
version = "0.0.1"
runtime = "rust-bin"
entry = "./slow_ok.sh"

[resources]
cpu_ms_per_task = 5000
"#;
        let card = card_for(manifest_toml, dir.path().to_path_buf());
        let mut intent = dummy_intent();
        intent.id = Uuid::new_v4();

        let tracker = Arc::new(SubprocessTracker::new());
        let runner = SubprocessRunner::with_tracker(tracker.clone());
        let intent_id = intent.id;
        let probe = tracker.clone();

        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let dispatch = tokio::spawn(async move { runner.run(&card, &intent).await });

        let mut seen = None;
        let deadline = std::time::Instant::now() + Duration::from_millis(800);
        while std::time::Instant::now() < deadline {
            if let Some(entry) = probe.get(&intent_id) {
                seen = Some(entry);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let entry = seen.expect(
            "tracker must record the spawned subprocess while the dispatch is in flight; a missing entry indicates the RegisterGuard was dropped before child.wait() or the runner never reached the spawn path",
        );
        assert!(entry.pid > 0, "tracker pid must be > 0 (got {})", entry.pid);
        assert_eq!(
            entry.agent_id, "tracked",
            "tracker agent_id must match card.id verbatim"
        );
        assert!(
            entry.started_at_ms >= started,
            "tracker started_at_ms must be >= dispatch start ({} vs {})",
            entry.started_at_ms,
            started
        );

        let result = dispatch
            .await
            .expect("dispatch task did not panic")
            .expect("dispatch must complete successfully (script returns valid JSON)");
        assert_eq!(result.text, "ok");
        assert!(
            tracker.is_empty(),
            "tracker must be empty after dispatch returns (len={}); the TrackerGuard's Drop impl must run on the happy path",
            tracker.len()
        );
    }

    #[tokio::test]
    async fn subprocess_runner_tracker_clears_on_timeout_kill_path() {
        // The TrackerGuard must unregister on every exit path, not just
        // the happy path. A subprocess that never returns within
        // cpu_ms_per_task ends in RunnerError::Timeout and child.kill();
        // after the run future returns, the tracker must be empty.
        // Failure mode 1 close-out: a guard that lived only in the
        // success arm would surface here as len()==1.
        let dir = tempdir().unwrap();
        let script = dir.path().join("never.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let manifest_toml = r#"
[agent]
id = "never"
name = "Never"
version = "0.0.1"
runtime = "rust-bin"
entry = "./never.sh"

[resources]
cpu_ms_per_task = 150
"#;
        let card = card_for(manifest_toml, dir.path().to_path_buf());
        let mut intent = dummy_intent();
        intent.id = Uuid::new_v4();
        let tracker = Arc::new(SubprocessTracker::new());
        let runner = SubprocessRunner::with_tracker(tracker.clone());

        let result = runner.run(&card, &intent).await;
        assert!(
            matches!(result, Err(RunnerError::Timeout(_))),
            "dispatch must end in Timeout; got {result:?}"
        );
        assert!(
            tracker.is_empty(),
            "tracker must be empty after the timeout-kill path (len={}); a RegisterGuard scoped only inside the happy branch would leak an entry here",
            tracker.len()
        );
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
        let r = SubprocessRunner::new().run(&card, &dummy_intent()).await;
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
        let result = SubprocessRunner::new().run(&card, &dummy_intent()).await;
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
        // documented default in covenant-manifest — a manifest with no
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

    #[cfg(unix)]
    #[tokio::test]
    async fn gvisor_runner_configure_process_group_places_runsc_in_new_group() {
        // GvisorRunner spawns runsc on the host; runsc is the
        // kernel-visible child. For the budget-preempt path to
        // SIGTERM/SIGKILL the sandbox via libc::kill(-runsc_pid, SIG),
        // runsc must be the leader of its own process group — exactly
        // the same invariant SubprocessRunner pins for trusted-local
        // dispatch. The test uses Command directly so it doesn't
        // depend on a real runsc binary; it exercises the same
        // configure_process_group function the gVisor spawn path calls,
        // so a refactor that dropped the call from the spawn path
        // (but kept the function) would surface in the spawn-path
        // smoke tests that already exist, while this test pins the
        // function's behavior.
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("30");
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        cmd.kill_on_drop(true);
        GvisorRunner::configure_process_group(&mut cmd);

        let mut child = cmd.spawn().expect("spawn sleep");
        let pid = child.id().expect("child pid available before reap");

        let pgid = unsafe { libc::getpgid(pid as libc::pid_t) };
        assert_ne!(
            pgid, -1,
            "getpgid(child_pid) must succeed; errno path indicates the child exited before the syscall or the kernel rejected the request"
        );
        assert_eq!(
            pgid as u32, pid,
            "GvisorRunner::configure_process_group(0) must make the spawned runsc the leader of a new process group (pgid == pid); without this a future libc::kill(-runsc_pid, SIGTERM) issued by the preempt dispatcher would target the daemon's own group, hitting either too narrowly (only the immediate runsc process) or too broadly (the daemon itself), and the sandbox-required budget-preempt guarantee silently degrades to backend-dependent"
        );

        child.kill().await.expect("kill spawned sleep");
        let _ = child.wait().await;
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
        // covenant_runtime documents two loud-fail safety nets:
        // SubprocessRunner::run and GvisorRunner::args_for. Both
        // surfaces emit the SAME
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

        let runner = SubprocessRunner::new();
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
                 inside SubprocessRunner.run is the documented \
                 loud-fail safety net; \
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
                 inside GvisorRunner::args_for mirrors the \
                 SubprocessRunner safety net; \
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

        let inner =
            serde_json::from_str::<serde_json::Value>("not json").expect_err("parse must fail");
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

    #[test]
    fn subprocess_tracker_snapshot_returns_all_entries_with_dropped_read_guard() {
        // Mirrors StreamTracker::snapshot's deadlock-freedom pin. The
        // projection tick (next slice) will iterate snapshot()
        // entries and call async budget-ledger lookups per entry; if
        // snapshot held its read guard across that await, the daemon
        // would deadlock as soon as the runner spawned a new
        // subprocess and the runner's register() blocked on the
        // tracker's write lock. Asserting that a synchronous
        // register from the same thread after snapshot returns
        // doesn't deadlock is the load-bearing pin.
        let tracker = SubprocessTracker::new();
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let id_c = Uuid::new_v4();
        let entry_for = |agent: &str, pid: u32| TrackedSubprocess {
            agent_id: agent.into(),
            pid,
            started_at_ms: 1_700_000_000_000,
        };
        tracker.register(id_a, entry_for("a@local", 100));
        tracker.register(id_b, entry_for("b@local", 200));
        tracker.register(id_c, entry_for("c@local", 300));

        let mut snap = tracker.snapshot();
        snap.sort_by_key(|(id, _)| *id);
        assert_eq!(
            snap.len(),
            3,
            "snapshot must include every registered entry; len mismatch means iter dropped a row or the read guard returned early"
        );
        let mut keys: Vec<Uuid> = snap.iter().map(|(id, _)| *id).collect();
        keys.sort();
        let mut expected = vec![id_a, id_b, id_c];
        expected.sort();
        assert_eq!(
            keys, expected,
            "snapshot must include exactly the registered intent_ids"
        );

        // Sentinel for the deadlock-freedom pin: if snapshot held the
        // read guard across return, this register would deadlock. The
        // test framework's wall-clock supervision would catch a hang,
        // but the absence of a hang here is the pin.
        let id_d = Uuid::new_v4();
        tracker.register(id_d, entry_for("d@local", 400));
        assert_eq!(tracker.len(), 4);
    }

    #[test]
    fn subprocess_tracker_registers_and_unregisters_entries() {
        // Asserts the SubprocessTracker primitive used by future
        // budget-hard-preempt sub-slices (projection, signal dispatch).
        // Sub-slice (A) ships the tracker type without integrating it
        // into the runner spawn path; this test pins the lifecycle
        // semantics (register → get → unregister → returns prior
        // value → entry is gone) so the integration slice cannot
        // silently regress register/unregister ordering.
        let tracker = SubprocessTracker::new();
        assert!(tracker.is_empty());

        let intent_id = Uuid::nil();
        let entry = TrackedSubprocess {
            agent_id: "agent.research@local".into(),
            pid: 12345,
            started_at_ms: 1_700_000_000_000,
        };
        tracker.register(intent_id, entry.clone());
        assert_eq!(tracker.len(), 1);
        assert_eq!(tracker.get(&intent_id), Some(entry.clone()));

        let removed = tracker.unregister(&intent_id);
        assert_eq!(removed, Some(entry));
        assert!(tracker.is_empty());
        assert_eq!(tracker.get(&intent_id), None);
        assert_eq!(tracker.unregister(&intent_id), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn subprocess_runner_spawn_places_child_in_its_own_process_group() {
        // The kernel-reported pgid of the spawned child must equal the
        // child's own pid — i.e. the child is the leader of a new
        // process group. Without this, a future
        // libc::kill(-pid, SIGTERM) would target the daemon's own
        // group and either hit too narrowly (only the immediate
        // child) or too broadly (the daemon itself), and the
        // budget-hard-preempt guarantee silently degrades.
        //
        // The test uses Command directly so it doesn't depend on the
        // agent-manifest scaffolding; it exercises the same
        // configure_process_group call SubprocessRunner uses on the
        // spawn path, so a refactor that dropped the call from one
        // site but not the other still fails here.
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("30");
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        cmd.kill_on_drop(true);
        SubprocessRunner::configure_process_group(&mut cmd);

        let mut child = cmd.spawn().expect("spawn sleep");
        let pid = child.id().expect("child pid available before reap");

        // SAFETY: getpgid is a thread-safe POSIX syscall on a pid we
        // just spawned and have not yet reaped, so the pid is
        // guaranteed to refer to a live process or to a zombie we
        // still own. errno on failure is read out-of-band via -1.
        let pgid = unsafe { libc::getpgid(pid as libc::pid_t) };
        assert_ne!(
            pgid, -1,
            "getpgid(child_pid) must succeed; errno path indicates the child exited before the syscall or the kernel rejected the request"
        );
        assert_eq!(
            pgid as u32, pid,
            "configure_process_group(0) must make the spawned child the leader of a new process group (pgid == pid); a refactor that dropped as_std_mut().process_group(0) or moved it after spawn would surface here. Without this, libc::kill(-pid, SIGTERM) in the future signal-dispatch slice would not reach grandchildren forked by the subprocess."
        );

        child.kill().await.expect("kill spawned sleep");
        let _ = child.wait().await;
    }

    #[test]
    fn preempt_outcome_serde_pins_tag_and_snake_case_discriminator() {
        // PreemptOutcome is the runtime crate's contract with the
        // daemon's audit emitter: the daemon round-trips outcomes through
        // serde (e.g. for ipc transport, sled state, or test fixtures)
        // and maps each variant to a BudgetPreempted or
        // BudgetPreemptFailed audit row. A refactor that renamed a
        // variant or changed the tag would silently retarget the
        // mapping and cause one outcome to be audited as another.
        let cases = [
            (
                PreemptOutcome::AlreadyDead,
                serde_json::json!({"kind": "already_dead"}),
            ),
            (
                PreemptOutcome::PermissionDenied { errno: 1 },
                serde_json::json!({"kind": "permission_denied", "errno": 1}),
            ),
            (
                PreemptOutcome::ExitedDuringGrace,
                serde_json::json!({"kind": "exited_during_grace"}),
            ),
            (
                PreemptOutcome::SigKilled,
                serde_json::json!({"kind": "sig_killed"}),
            ),
            (
                PreemptOutcome::UnsupportedPlatform,
                serde_json::json!({"kind": "unsupported_platform"}),
            ),
        ];
        for (variant, wire) in cases {
            assert_eq!(serde_json::to_value(variant).unwrap(), wire);
            let back: PreemptOutcome = serde_json::from_value(wire).unwrap();
            assert_eq!(back, variant);
        }

        // Rejected rename: any other tag string must fail to deserialize.
        // Without this the daemon-side mapping silently degrades when a
        // refactor introduces an aliased variant under a different name.
        for unknown in ["AlreadyDead", "already-dead", "sigterm", "killed"] {
            let bad = serde_json::json!({"kind": unknown});
            assert!(
                serde_json::from_value::<PreemptOutcome>(bad).is_err(),
                "PreemptOutcome must reject the rename {unknown:?}; the daemon-side audit mapping joins on the snake_case tag and a stray alias would mis-attribute a preempt"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preempt_subprocess_pg_returns_already_dead_when_pid_no_longer_exists() {
        // Spawn a process, let it exit, then preempt the (now-defunct)
        // group. The kernel reports ESRCH on the initial SIGTERM and
        // the dispatcher must surface that as AlreadyDead — the
        // daemon's audit emitter will translate this into either
        // BudgetPreempted{signal_sent="none"} or BudgetPreemptFailed{errno=ESRCH}
        // depending on policy, but the dispatcher itself does not
        // encode that decision.
        let mut cmd = tokio::process::Command::new("true");
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        SubprocessRunner::configure_process_group(&mut cmd);
        let mut child = cmd.spawn().expect("spawn true");
        let pid = child.id().expect("pid before wait");
        // Reap so the kernel actually clears the pid; an unreaped
        // zombie still answers kill(_, 0) without ESRCH.
        let _ = child.wait().await;

        let outcome = preempt_subprocess_pg(pid, Duration::from_millis(200)).await;
        assert_eq!(
            outcome,
            PreemptOutcome::AlreadyDead,
            "preempt_subprocess_pg must return AlreadyDead when SIGTERM hits ESRCH on the initial kill; a refactor that swallowed ESRCH and proceeded to the poll loop would surface here as SigKilled (because the loop's kill(_, 0) on a reaped pid also returns ESRCH but only after the timeout) or as SigKilled outright"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preempt_subprocess_pg_returns_sig_killed_when_group_survives_grace() {
        // A subprocess that ignores SIGTERM (via `trap '' TERM`) must
        // be SIGKILLed once grace expires. This is the load-bearing
        // case: it proves the dispatcher does not stop after SIGTERM,
        // and that the SIGKILL syscall lands on a process group whose
        // leader is the spawned pid (no negation bug).
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg("trap '' TERM; while true; do sleep 0.1; done");
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        cmd.kill_on_drop(true);
        SubprocessRunner::configure_process_group(&mut cmd);
        let mut child = cmd.spawn().expect("spawn ignore-term loop");
        let pid = child.id().expect("pid before wait");

        let start = std::time::Instant::now();
        let outcome = preempt_subprocess_pg(pid, Duration::from_millis(250)).await;
        let elapsed = start.elapsed();
        let _ = child.wait().await;

        assert_eq!(
            outcome,
            PreemptOutcome::SigKilled,
            "preempt_subprocess_pg must escalate to SIGKILL when SIGTERM is trapped and ignored; a refactor that mis-negated the kill target (kill(pid) instead of kill(-pid)) would silently fail to reach the trap-installed process group and the loop would survive both signals, producing a UB outcome"
        );
        assert!(
            elapsed >= Duration::from_millis(250),
            "dispatcher must respect the grace window even when the group is unresponsive (elapsed = {elapsed:?})"
        );
        assert!(
            elapsed < Duration::from_millis(1500),
            "dispatcher must not block much past grace; a poll loop that ignored the elapsed bound would surface here as a runaway (elapsed = {elapsed:?})"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preempt_subprocess_pg_returns_exited_during_grace_when_group_drains_inside_window() {
        // `sleep` terminates immediately on SIGTERM (POSIX default).
        // The dispatcher's poll observes ESRCH only once the child is
        // reaped — a zombie still counts as a group member for
        // kill(-pid, 0). The daemon's runner reaps via child.wait()
        // concurrently; this test models that by running child.wait()
        // alongside preempt_subprocess_pg in tokio::join!.
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("60");
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        cmd.kill_on_drop(true);
        SubprocessRunner::configure_process_group(&mut cmd);
        let mut child = cmd.spawn().expect("spawn sleep");
        let pid = child.id().expect("pid before wait");

        let (outcome, _exit) = tokio::join!(
            preempt_subprocess_pg(pid, Duration::from_millis(1500)),
            child.wait(),
        );

        assert_eq!(
            outcome,
            PreemptOutcome::ExitedDuringGrace,
            "preempt_subprocess_pg must return ExitedDuringGrace when the cooperative SIGTERM lands and the runner concurrently reaps the child inside grace; SigKilled here would indicate the poll loop never observed ESRCH (a saturating_sub or off-by-one bug in the elapsed-bound check) or that the SIGKILL was dispatched anyway (a refactor that dropped the grace early-exit)"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preempt_subprocess_pg_signals_grandchildren_via_negated_pid() {
        // Pins failure mode 2 of budget-hard-preempt-dispatcher-fn:
        // kill(-pid, ...) must reach grandchildren forked inside the
        // process group. A regression that wrote kill(pid, ...) would
        // signal only the immediate child and the grandchild loop
        // would survive. The grandchild here is a `sleep` inheriting
        // the parent's pgid; if it survives SIGKILL on -pid, this
        // test hangs or fails.
        //
        // The shell uses `exec sleep` so the parent shell does not stay
        // around to be the only thing that receives the signal; the
        // grandchild becomes the only group member. After preempt the
        // grandchild's pgid (which equals the original parent's pid)
        // must answer ESRCH.
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg("sleep 30 & sleep 30");
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        cmd.kill_on_drop(true);
        SubprocessRunner::configure_process_group(&mut cmd);
        let mut child = cmd.spawn().expect("spawn shell-with-fork");
        let pid = child.id().expect("pid before wait");

        let outcome = preempt_subprocess_pg(pid, Duration::from_millis(250)).await;
        let _ = child.wait().await;
        assert!(
            matches!(outcome, PreemptOutcome::ExitedDuringGrace | PreemptOutcome::SigKilled),
            "dispatcher must terminate a process group with forked children (got {outcome:?}); AlreadyDead here would mean the SIGTERM raced past spawn (unlikely) or the negation was dropped and we hit a different pid"
        );

        // After preempt, kill(-pid, 0) must report ESRCH — meaning
        // every member of the group, including the backgrounded
        // sleeper, is gone. SAFETY: same justification as the
        // dispatcher's own kill(0) probe.
        let probe = unsafe { libc::kill(-(pid as i32), 0) };
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        assert_eq!(
            (probe, errno),
            (-1, libc::ESRCH),
            "after preempt_subprocess_pg returns, every member of the process group must be gone (kill(-pid, 0) must return ESRCH); a surviving grandchild would surface here as rc=0 and prove the dispatcher did not negate the pid"
        );
    }

    #[cfg(not(unix))]
    #[tokio::test]
    async fn preempt_subprocess_pg_returns_unsupported_platform_on_non_unix() {
        // The non-unix stub must be explicit. A caller on Windows must
        // see UnsupportedPlatform — silently returning AlreadyDead
        // would let a daemon emit BudgetPreempted on a platform where
        // no signal was ever sent.
        let outcome = preempt_subprocess_pg(0, Duration::from_millis(10)).await;
        assert_eq!(outcome, PreemptOutcome::UnsupportedPlatform);
    }
}
