//! Container-isolated job execution (B6): the untrusted job runs in an
//! OCI container instead of as the node's own user. What the operator
//! gets over [`crate::executor::SubprocessJobExecutor`]:
//!
//! - kernel namespace isolation with a read-only image rootfs, all
//!   capabilities dropped, `no-new-privileges`, and a pids ceiling
//!   (fork bombs die in the container, not on the node)
//! - default-deny egress: `--network none` unless the operator names a
//!   network — a stranger's job cannot phone home by default
//! - memory/cpu ceilings enforced by the container runtime, not trusted
//!   to the job
//! - a fresh per-job scratch dir bind-mounted as the workdir (and
//!   `HOME`), removed when the job ends
//! - the gVisor seam: `oci_runtime: Some("runsc")` runs the same
//!   container under user-space kernel emulation on hosts that have it
//!   (`--runtime runsc`); Docker Desktop's VM already gives macOS a
//!   non-host kernel
//!
//! GPU passthrough is a flag away: `gpus: Some("all")` adds `--gpus all`
//! so a sandboxed job can reach the host's NVIDIA devices, off by
//! default (a stranger's job gets a device only when the operator opts
//! in) and reliant on the host having the NVIDIA container toolkit or
//! CDI installed — the one piece design-01 §7 flagged as host-specific.
//!
//! Deliberately NOT covered: the image supply chain (which image to
//! trust is an operator decision with no safe default — the executor
//! refuses to boot without one).
//!
//! Deadline enforcement is outside-in, like the subprocess executor:
//! the client process is tracked and the container itself is killed by
//! name on timeout — `--rm` then reaps it. The job's stdout rides the
//! client's stdout with the same 4 MiB fail-don't-truncate ceiling and
//! the same stderr tail drain.

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use covenant_compute_protocol::JobEnvelopePayload;
use covenant_mcp::Content;
use covenant_runtime::{preempt_subprocess_pg, SubprocessTracker, TrackedSubprocess};
use tokio::process::Command;

use crate::executor::{
    configure_process_group, drain_stderr_tail, read_stdout_capped, ExecutionOutcome,
    ExecutorError, JobExecutor,
};

#[derive(Debug, Clone)]
pub struct ContainerConfig {
    /// The container CLI — `docker` (default) or `podman`; both speak
    /// the same `run`/`kill` surface used here.
    pub runtime_binary: String,
    /// Image the job command runs in. Required: which rootfs to trust
    /// is an operator decision with no safe default.
    pub image: String,
    /// Extra OCI runtime for `--runtime` — `Some("runsc")` is the
    /// gVisor hook on hosts that have it installed. `None` uses the
    /// engine's default.
    pub oci_runtime: Option<String>,
    /// GPU passthrough spec, passed verbatim to `--gpus` (docker and
    /// modern podman): `Some("all")` exposes every device, `Some("0")`
    /// or `Some("device=0,1")` a subset. `None` (the default) gives the
    /// container no GPU — a stranger's job gets a device only when the
    /// operator opts in, and only on a host with the NVIDIA container
    /// toolkit (or CDI) installed. The isolation posture is otherwise
    /// unchanged: the job still runs read-only, capability-dropped, and
    /// (unless the operator names a network) egress-denied.
    pub gpus: Option<String>,
    /// Docker network the job joins. `none` (the default) is the
    /// default-deny egress posture; naming a network is the operator's
    /// explicit opt-in to selective egress.
    pub network: String,
    /// Memory ceiling in the runtime's syntax, e.g. `512m`.
    pub memory: String,
    /// CPU ceiling in the runtime's syntax, e.g. `1`.
    pub cpus: String,
    /// Max processes inside the container.
    pub pids_limit: u32,
    /// `--user uid:gid` inside the container. `None` keeps the image's
    /// default user: forcing e.g. `nobody` breaks bind-mount writes on
    /// hosts whose scratch dir the uid can't touch, so hardening this
    /// is opt-in where the operator knows it works.
    pub user: Option<String>,
    /// Distinguishes this node's containers on a shared engine —
    /// container names are `compute-job-<tag>-<job_id>` and the boot
    /// orphan sweep only touches its own tag, so two nodes on one
    /// docker daemon never reap each other's jobs. The node binary
    /// passes a prefix of the operator pubkey.
    pub instance_tag: String,
}

impl Default for ContainerConfig {
    fn default() -> Self {
        Self {
            runtime_binary: "docker".into(),
            image: String::new(),
            oci_runtime: None,
            gpus: None,
            network: "none".into(),
            memory: "512m".into(),
            cpus: "1".into(),
            pids_limit: 256,
            user: None,
            instance_tag: "solo".into(),
        }
    }
}

/// Mount point of the per-job scratch dir inside the container; also
/// the job's workdir and `HOME`.
const SCRATCH_MOUNT: &str = "/scratch";

/// A failure to spawn the container runtime binary. The common one by
/// far — an operator who chose `--executor container` on a host without
/// an engine — is the runtime simply not being installed; name docker
/// and podman and the way out rather than surfacing a raw `No such file
/// or directory`. Any other spawn error keeps its caller's context.
fn runtime_spawn_error(binary: &str, context: &str, e: &std::io::Error) -> ExecutorError {
    if e.kind() == std::io::ErrorKind::NotFound {
        return ExecutorError::Failed(format!(
            "container runtime {binary:?} is not installed or not on PATH — install docker or \
             podman (set COVENANT_COMPUTE_NODE_CONTAINER_RUNTIME=podman to use podman), or \
             re-run `covenant-compute-node setup --executor subprocess`"
        ));
    }
    ExecutorError::Failed(format!("{context}: {e}"))
}

pub struct ContainerJobExecutor {
    config: ContainerConfig,
    tracker: Arc<SubprocessTracker>,
    preempt_grace: Duration,
}

impl ContainerJobExecutor {
    pub fn new(
        config: ContainerConfig,
        tracker: Arc<SubprocessTracker>,
        preempt_grace: Duration,
    ) -> Self {
        Self {
            config,
            tracker,
            preempt_grace,
        }
    }

    /// The full `run` argument vector — pure, so the isolation posture
    /// is unit-testable without a container engine.
    fn run_args(&self, container_name: &str, scratch: &Path, command: &str) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![
            "run".into(),
            "--rm".into(),
            "--name".into(),
            container_name.into(),
            "--network".into(),
            self.config.network.clone().into(),
            "--memory".into(),
            self.config.memory.clone().into(),
            "--cpus".into(),
            self.config.cpus.clone().into(),
            "--pids-limit".into(),
            // A pids-limit of 0 tells docker/podman "unlimited" — the
            // opposite of a cap for a stranger's job. Floor to a positive
            // value so a zero (typo, or an unset knob that parsed to 0)
            // can never disable the fork-bomb bound this cap exists for.
            self.config.pids_limit.max(1).to_string().into(),
            "--read-only".into(),
            "--cap-drop".into(),
            "ALL".into(),
            "--security-opt".into(),
            "no-new-privileges".into(),
            "--tmpfs".into(),
            "/tmp:rw,noexec,nosuid,size=64m".into(),
            "-v".into(),
            format!("{}:{SCRATCH_MOUNT}:rw", scratch.display()).into(),
            "-w".into(),
            SCRATCH_MOUNT.into(),
            "-e".into(),
            format!("HOME={SCRATCH_MOUNT}").into(),
        ];
        if let Some(user) = &self.config.user {
            args.push("--user".into());
            args.push(user.clone().into());
        }
        if let Some(runtime) = &self.config.oci_runtime {
            args.push("--runtime".into());
            args.push(runtime.clone().into());
        }
        if let Some(gpus) = &self.config.gpus {
            args.push("--gpus".into());
            args.push(gpus.clone().into());
        }
        args.push(self.config.image.clone().into());
        args.push("sh".into());
        args.push("-c".into());
        args.push(command.into());
        args
    }

    /// Kills the container by name, bounded — the client process alone
    /// dying would leave the container running, so every abort path
    /// goes through here. A kill error is expected when the container
    /// already exited (or never started) and is ignored.
    async fn kill_container(&self, name: &str) {
        let kill = Command::new(&self.config.runtime_binary)
            .args(["kill", name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output();
        if tokio::time::timeout(Duration::from_secs(10), kill)
            .await
            .is_err()
        {
            tracing::warn!(container = name, "container kill did not return in 10s");
        }
    }

    /// [`Self::kill_container`] plus a hard preempt of the client's
    /// own process group — belt and braces for the abort paths.
    async fn abort(&self, name: &str, client_pid: Option<u32>) {
        self.kill_container(name).await;
        if let Some(pid) = client_pid {
            preempt_subprocess_pg(pid, self.preempt_grace).await;
        }
    }

    /// This node's container-name prefix; every job name starts with it
    /// and the orphan sweep matches exactly it.
    fn name_prefix(&self) -> String {
        format!("compute-job-{}-", self.config.instance_tag)
    }

    /// Kills any job container a previous node process left running.
    /// A crashed node cannot run `kill_on_drop`, and killing the
    /// docker CLI only detaches — the container itself keeps running
    /// under the engine, a stranger's workload unsupervised until
    /// someone notices. The node binary calls this once at boot,
    /// before serving; names carry [`ContainerConfig::instance_tag`],
    /// so the sweep never touches another node's live jobs. Returns
    /// how many were reaped.
    pub async fn reap_orphans(&self) -> usize {
        let ps = Command::new(&self.config.runtime_binary)
            .args([
                "ps",
                "-q",
                "--filter",
                &format!("name={}", self.name_prefix()),
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        let ids: Vec<String> = match tokio::time::timeout(Duration::from_secs(10), ps).await {
            Ok(Ok(out)) => String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect(),
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "orphan sweep could not list containers");
                return 0;
            }
            Err(_) => {
                tracing::warn!("orphan sweep timed out listing containers");
                return 0;
            }
        };
        if ids.is_empty() {
            return 0;
        }
        tracing::warn!(
            count = ids.len(),
            "reaping job containers a previous node instance left running"
        );
        let kill = Command::new(&self.config.runtime_binary)
            .arg("kill")
            .args(&ids)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output();
        if tokio::time::timeout(Duration::from_secs(30), kill)
            .await
            .is_err()
        {
            tracing::warn!("orphan kill did not return in 30s");
        }
        ids.len()
    }

    /// Pull the configured image at boot when it is not already present,
    /// so the first job does not pay the pull inside its own deadline. A
    /// cold node serving a multi-gigabyte image would otherwise fault on
    /// its first job — the pull outlasting the deadline reads as slow
    /// work, not a slow download, and costs the operator the fault.
    ///
    /// Best-effort by design: a present image is left untouched (so a
    /// warmed node still boots with no registry round-trip, matching
    /// `docker run`'s own pull-on-miss policy), and a failed pull is
    /// logged, never fatal — `docker run` retries on miss and a genuinely
    /// unresolvable image surfaces per-job with its real error.
    pub async fn ensure_image_present(&self) {
        let present = tokio::time::timeout(
            Duration::from_secs(10),
            Command::new(&self.config.runtime_binary)
                .args(["image", "inspect", &self.config.image])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .output(),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .map(|o| o.status.success())
        .unwrap_or(false);
        if present {
            return;
        }
        tracing::info!(
            image = %self.config.image,
            "pulling the job image so the first job does not wait on it"
        );
        let pull = Command::new(&self.config.runtime_binary)
            .args(["pull", &self.config.image])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output();
        match tokio::time::timeout(Duration::from_secs(600), pull).await {
            Ok(Ok(out)) if out.status.success() => {
                tracing::info!(image = %self.config.image, "job image ready");
            }
            Ok(Ok(out)) => {
                tracing::warn!(
                    image = %self.config.image,
                    status = %out.status,
                    "could not pull the job image at boot; the first job will retry the pull and \
                     may fault if it runs past its deadline: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    image = %self.config.image,
                    "could not run the image pull at boot: {e}"
                );
            }
            Err(_) => {
                tracing::warn!(
                    image = %self.config.image,
                    "image pull did not finish in 10m at boot; continuing"
                );
            }
        }
    }
}

#[async_trait]
impl JobExecutor for ContainerJobExecutor {
    /// `<runtime> version` queries the engine daemon for real — the
    /// same daemon `run` needs — so a stopped Docker Desktop fails here
    /// instead of failing (and faulting) every job the matcher sends.
    async fn health(&self) -> Result<(), ExecutorError> {
        let version = Command::new(&self.config.runtime_binary)
            .arg("version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output();
        match tokio::time::timeout(Duration::from_secs(5), version).await {
            Ok(Ok(out)) if out.status.success() => Ok(()),
            Ok(Ok(out)) => Err(ExecutorError::Failed(format!(
                "{} version exited with {}: {}",
                self.config.runtime_binary,
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ))),
            Ok(Err(e)) => Err(runtime_spawn_error(
                &self.config.runtime_binary,
                &format!("spawn {} version", self.config.runtime_binary),
                &e,
            )),
            Err(_) => Err(ExecutorError::Failed(format!(
                "{} version did not answer in 5s",
                self.config.runtime_binary
            ))),
        }
    }

    async fn execute(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let command_text = job
            .input
            .iter()
            .find_map(|c| match c {
                Content::Text { text } => Some(text.clone()),
                Content::Json { .. } => None,
            })
            .ok_or_else(|| ExecutorError::Failed("no Content::Text command in job.input".into()))?;

        let scratch = tempfile::Builder::new()
            .prefix("compute-job-")
            .tempdir()
            .map_err(|e| ExecutorError::Failed(format!("scratch dir: {e}")))?;
        let container_name = format!("{}{}", self.name_prefix(), job.job_id);

        // The client inherits the node's env on purpose: it is the
        // node's own trusted CLI and needs its HOME/DOCKER_* context.
        // The JOB sees none of it — `run` passes only the `-e` vars.
        let mut cmd = Command::new(&self.config.runtime_binary);
        cmd.args(self.run_args(&container_name, scratch.path(), &command_text))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        configure_process_group(&mut cmd);

        let started = Instant::now();
        let mut child = cmd.spawn().map_err(|e| {
            runtime_spawn_error(
                &self.config.runtime_binary,
                &format!("spawn {}", self.config.runtime_binary),
                &e,
            )
        })?;
        let pid = child.id();
        if let Some(pid) = pid {
            self.tracker.register(
                job.job_id,
                TrackedSubprocess {
                    agent_id: job.buyer.display.clone(),
                    pid,
                    started_at_ms: 0,
                },
            );
        }

        let stderr = child.stderr.take().expect("stderr piped");
        let stderr_task = tokio::spawn(drain_stderr_tail(stderr));
        let mut stdout = child.stdout.take().expect("stdout piped");

        let outcome = match tokio::time::timeout(deadline, read_stdout_capped(&mut stdout)).await {
            Ok(Ok(buf)) => match tokio::time::timeout(self.preempt_grace, child.wait()).await {
                Ok(Ok(status)) if status.success() => Ok(ExecutionOutcome {
                    output: vec![Content::text(String::from_utf8_lossy(&buf).into_owned())],
                    wall_ms: started.elapsed().as_millis() as u64,
                    tokens_in: None,
                    tokens_out: None,
                    finish_reason: None,
                }),
                Ok(Ok(status)) => {
                    let tail = stderr_task.await.unwrap_or_default();
                    Err(ExecutorError::Failed(format!(
                        "container job exited with {status}: {tail}"
                    )))
                }
                // The client reported an error reaping the container: it
                // may still be running, so kill it by name like every
                // other abort path — `kill_on_drop` reaches only the CLI
                // client, which detaches without stopping the container.
                Ok(Err(e)) => {
                    self.abort(&container_name, pid).await;
                    Err(ExecutorError::Failed(format!("wait: {e}")))
                }
                Err(_) => {
                    self.abort(&container_name, pid).await;
                    Err(ExecutorError::Failed(
                        "container closed stdout but did not exit; killed".into(),
                    ))
                }
            },
            Ok(Err(e)) => {
                self.abort(&container_name, pid).await;
                Err(e)
            }
            Err(_) => {
                self.abort(&container_name, pid).await;
                Err(ExecutorError::Timeout(deadline))
            }
        };

        self.tracker.unregister(&job.job_id);
        // Reclaim the scratch dir explicitly rather than leaning on the
        // TempDir's Drop, which swallows a removal error. In the default
        // posture the container runs as the image's root user over the rw
        // bind mount, so a job that writes under HOME (a package cache,
        // say) can leave root-owned files this non-root process cannot
        // unlink; a silently dropped remove then leaks the tree under the
        // system temp dir on every such job. Naming the path an operator
        // can reclaim beats hiding it, the way an orphaned container is
        // flagged rather than swallowed.
        let scratch_path = scratch.keep();
        if let Err(e) = std::fs::remove_dir_all(&scratch_path) {
            tracing::warn!(
                scratch = %scratch_path.display(),
                error = %e,
                "job scratch dir not fully reclaimed; a container that ran as root may have \
                 left files this node cannot delete — reclaim the path, or set \
                 COVENANT_COMPUTE_NODE_CONTAINER_USER to this node's uid:gid so job files are \
                 node-owned"
            );
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
    use covenant_compute_protocol::{CapabilityRequirement, JobKind};
    use covenant_types::AgentId;
    use uuid::Uuid;

    fn executor(config: ContainerConfig) -> ContainerJobExecutor {
        ContainerJobExecutor::new(
            config,
            Arc::new(SubprocessTracker::new()),
            Duration::from_secs(1),
        )
    }

    fn args_of(config: ContainerConfig) -> Vec<String> {
        executor(config)
            .run_args("compute-job-x", Path::new("/tmp/scratch"), "echo hi")
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[tokio::test]
    async fn health_reflects_the_engine_daemon_answer() {
        // `true`/`false` stand in for the engine CLI: exit 0 is a
        // daemon that answered `version`, nonzero is one that didn't,
        // and a missing binary fails at spawn.
        executor(ContainerConfig {
            runtime_binary: "true".into(),
            ..ContainerConfig::default()
        })
        .health()
        .await
        .expect("exit 0 is a healthy engine");

        assert!(executor(ContainerConfig {
            runtime_binary: "false".into(),
            ..ContainerConfig::default()
        })
        .health()
        .await
        .is_err());

        let err = executor(ContainerConfig {
            runtime_binary: "/nonexistent/compute-engine".into(),
            ..ContainerConfig::default()
        })
        .health()
        .await
        .expect_err("no binary to spawn");
        assert!(
            err.to_string().contains("not installed or not on PATH")
                && err.to_string().contains("docker or podman"),
            "a missing runtime must name the fix, not a raw spawn error: {err}"
        );
    }

    #[test]
    fn a_missing_runtime_error_names_the_fix() {
        let not_found = std::io::Error::from(std::io::ErrorKind::NotFound);
        let err = runtime_spawn_error("docker", "spawn docker version", &not_found);
        let s = err.to_string();
        assert!(s.contains("not installed or not on PATH"), "got: {s}");
        assert!(
            s.contains("docker or podman") && s.contains("--executor subprocess"),
            "the fix must name both engines and the subprocess fallback: {s}"
        );
        // A different spawn error keeps its caller's context verbatim.
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let err = runtime_spawn_error("podman", "spawn podman version", &denied);
        assert!(
            err.to_string().contains("spawn podman version"),
            "a non-NotFound spawn error keeps its context: {err}"
        );
    }

    #[test]
    fn the_default_posture_is_deny_by_construction() {
        let args = args_of(ContainerConfig {
            image: "alpine:3.20".into(),
            ..ContainerConfig::default()
        });
        let joined = args.join(" ");
        assert!(
            joined.contains("--network none"),
            "default-deny egress: {joined}"
        );
        assert!(joined.contains("--read-only"), "read-only rootfs: {joined}");
        assert!(
            joined.contains("--cap-drop ALL"),
            "no capabilities: {joined}"
        );
        assert!(joined.contains("--security-opt no-new-privileges"));
        assert!(joined.contains("--pids-limit 256"));
        assert!(joined.contains("--memory 512m"));
        assert!(
            !joined.contains("--runtime "),
            "no OCI runtime flag unless configured: {joined}"
        );
        assert!(
            !joined.contains("--gpus"),
            "no GPU reaches a stranger's job unless the operator opts in: {joined}"
        );
        assert!(
            !joined.contains("--user"),
            "image default user unless opted in"
        );
    }

    #[test]
    fn a_zero_pids_limit_floors_to_a_real_cap_never_unlimited() {
        // `--pids-limit 0` is "unlimited" to docker/podman, so a zero
        // (an unset knob that parsed to 0, or a typo) must not reach the
        // runtime as-is and lift the fork-bomb bound on a stranger's job.
        let args = args_of(ContainerConfig {
            image: "alpine:3.20".into(),
            pids_limit: 0,
            ..ContainerConfig::default()
        });
        let joined = args.join(" ");
        assert!(
            joined.contains("--pids-limit 1"),
            "floored, not 0: {joined}"
        );
        assert!(
            !joined.contains("--pids-limit 0"),
            "0 would disable the cap: {joined}"
        );
    }

    #[test]
    fn gpu_passthrough_is_a_flag_away_and_stays_before_the_image() {
        let args = args_of(ContainerConfig {
            image: "nvidia/cuda:12.4.1-runtime-ubuntu22.04".into(),
            gpus: Some("all".into()),
            ..ContainerConfig::default()
        });
        let joined = args.join(" ");
        assert!(joined.contains("--gpus all"), "gpus opt-in: {joined}");
        // The flag is an engine argument, so it must land before the
        // image or docker reads it as part of the in-container command.
        let gpus_at = args.iter().position(|a| a == "--gpus").unwrap();
        let image_at = args
            .iter()
            .position(|a| a == "nvidia/cuda:12.4.1-runtime-ubuntu22.04")
            .unwrap();
        assert!(
            gpus_at < image_at,
            "--gpus must precede the image: {joined}"
        );
    }

    #[test]
    fn the_image_boundary_stops_option_smuggling() {
        // Everything after the image name is the in-container command:
        // a hostile command string must land AFTER the image, never be
        // parsed as a docker flag.
        let args = args_of(ContainerConfig {
            image: "alpine:3.20".into(),
            ..ContainerConfig::default()
        });
        let image_at = args.iter().position(|a| a == "alpine:3.20").unwrap();
        assert_eq!(args[image_at + 1], "sh");
        assert_eq!(args[image_at + 2], "-c");
        assert_eq!(args[image_at + 3], "echo hi");
        assert_eq!(args.len(), image_at + 4, "nothing rides after the command");
    }

    #[test]
    fn the_gvisor_seam_and_user_hardening_are_flags_away() {
        let args = args_of(ContainerConfig {
            image: "alpine:3.20".into(),
            oci_runtime: Some("runsc".into()),
            user: Some("65534:65534".into()),
            network: "egress-allowed".into(),
            ..ContainerConfig::default()
        });
        let joined = args.join(" ");
        assert!(joined.contains("--runtime runsc"));
        assert!(joined.contains("--user 65534:65534"));
        assert!(joined.contains("--network egress-allowed"));
    }

    #[test]
    fn container_names_carry_the_instance_tag_so_sweeps_stay_scoped() {
        let mine = executor(ContainerConfig {
            image: "alpine:3.20".into(),
            instance_tag: "a1b2c3d4".into(),
            ..ContainerConfig::default()
        });
        let sibling = executor(ContainerConfig {
            image: "alpine:3.20".into(),
            instance_tag: "e5f6a7b8".into(),
            ..ContainerConfig::default()
        });
        assert_eq!(mine.name_prefix(), "compute-job-a1b2c3d4-");
        assert!(
            !sibling.name_prefix().starts_with(&mine.name_prefix()),
            "one node's sweep filter must never match another's jobs"
        );
    }

    #[test]
    fn scratch_is_mounted_writable_as_workdir_and_home() {
        let args = args_of(ContainerConfig {
            image: "alpine:3.20".into(),
            ..ContainerConfig::default()
        });
        let joined = args.join(" ");
        assert!(joined.contains("/tmp/scratch:/scratch:rw"));
        assert!(joined.contains("-w /scratch"));
        assert!(joined.contains("HOME=/scratch"));
    }

    #[tokio::test]
    async fn a_job_without_a_text_command_fails_before_any_engine_call() {
        // runtime_binary deliberately nonexistent: reaching spawn would
        // fail loudly and differently than the expected refusal.
        let executor = executor(ContainerConfig {
            runtime_binary: "/nonexistent/engine".into(),
            image: "alpine:3.20".into(),
            ..ContainerConfig::default()
        });
        let job = JobEnvelopePayload {
            job_id: Uuid::new_v4(),
            buyer: AgentId::new("buyer@local", [1u8; 32]),
            kind: JobKind::BatchJob,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: None,
                kind: JobKind::BatchJob,
                max_duration_secs: 5,
                min_reputation_bps: None,
            },
            input: vec![Content::json(serde_json::json!({"not": "a command"}))],
            price_micro_usdc: 10,
            deadline_ms: 1_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "container-test"),
            issued_at_ms: 0,
            referral_code: None,
            stream: false,
        };
        let err = executor
            .execute(&job, Duration::from_secs(1))
            .await
            .expect_err("no command, no run");
        assert!(err.to_string().contains("no Content::Text"), "got: {err}");
    }

    // The tests below drive the real engine. Everything above proves the
    // argument vector and refusals without one; these prove the posture
    // actually holds against a live daemon — the one hop a pure-`run_args`
    // test cannot cover. Ignored by default (they need docker/podman and
    // pull `alpine:3.20`); run with `cargo test -- --ignored`.

    /// A `BatchJob` whose single input is a shell command for the
    /// container's `sh -c`, exactly what the node builds for a command job.
    fn text_job(command: &str) -> JobEnvelopePayload {
        JobEnvelopePayload {
            job_id: Uuid::new_v4(),
            buyer: AgentId::new("buyer@local", [1u8; 32]),
            kind: JobKind::BatchJob,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: None,
                kind: JobKind::BatchJob,
                max_duration_secs: 60,
                min_reputation_bps: None,
            },
            input: vec![Content::text(command.to_string())],
            price_micro_usdc: 10,
            deadline_ms: 60_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "container-live"),
            issued_at_ms: 0,
            referral_code: None,
            stream: false,
        }
    }

    /// A container executor on the tiny `alpine:3.20` image with a unique
    /// instance tag, so one live test's containers and orphan sweep never
    /// collide with another's (or with a real node sharing the daemon).
    fn live_executor(instance_tag: &str) -> ContainerJobExecutor {
        ContainerJobExecutor::new(
            ContainerConfig {
                image: "alpine:3.20".into(),
                instance_tag: instance_tag.into(),
                ..ContainerConfig::default()
            },
            Arc::new(SubprocessTracker::new()),
            Duration::from_secs(2),
        )
    }

    fn output_text(outcome: &ExecutionOutcome) -> String {
        outcome
            .output
            .iter()
            .find_map(|c| match c {
                Content::Text { text } => Some(text.clone()),
                Content::Json { .. } => None,
            })
            .unwrap_or_default()
    }

    fn unique_tag() -> String {
        Uuid::new_v4()
            .to_string()
            .replace('-', "")
            .chars()
            .take(12)
            .collect()
    }

    async fn container_running(name: &str) -> bool {
        let out = Command::new("docker")
            .args(["ps", "-q", "--filter", &format!("name={name}")])
            .output()
            .await
            .expect("docker ps");
        !String::from_utf8_lossy(&out.stdout).trim().is_empty()
    }

    fn scratch_dirs() -> std::collections::HashSet<std::path::PathBuf> {
        std::fs::read_dir(std::env::temp_dir())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("compute-job-"))
            })
            .collect()
    }

    #[tokio::test]
    #[ignore = "requires a working docker engine and pulls alpine:3.20"]
    async fn live_container_runs_a_command_and_returns_its_stdout() {
        let exec = live_executor(&unique_tag());
        let outcome = exec
            .execute(
                &text_job("echo compute-container-ok"),
                Duration::from_secs(60),
            )
            .await
            .expect("a well-formed command runs in the container");
        assert!(
            output_text(&outcome).contains("compute-container-ok"),
            "stdout must carry the command output: {:?}",
            outcome.output
        );
    }

    #[tokio::test]
    #[ignore = "requires a working docker engine and pulls alpine:3.20"]
    async fn live_container_scratch_is_writable_and_the_rootfs_is_read_only() {
        let exec = live_executor(&unique_tag());
        // The bind-mounted scratch (workdir + HOME) must accept a write
        // and the image rootfs must reject one — the core sandbox line.
        let cmd = "echo scratch-ok > ./probe && cat ./probe && \
                   (echo x > /etc/compute-probe 2>/dev/null && echo WROTE-ROOTFS || echo rootfs-readonly)";
        let outcome = exec
            .execute(&text_job(cmd), Duration::from_secs(60))
            .await
            .expect("the job runs");
        let text = output_text(&outcome);
        assert!(
            text.contains("scratch-ok"),
            "scratch must be writable: {text:?}"
        );
        assert!(
            text.contains("rootfs-readonly"),
            "the rootfs must be read-only: {text:?}"
        );
        assert!(
            !text.contains("WROTE-ROOTFS"),
            "a write to the rootfs must fail: {text:?}"
        );
    }

    #[tokio::test]
    #[ignore = "requires a working docker engine and pulls alpine:3.20"]
    async fn live_container_reclaims_the_scratch_dir_after_a_job() {
        let exec = live_executor(&unique_tag());
        // A unique marker in a node-owned scratch file, so the assertion
        // attributes a surviving tree to THIS job and never to a live
        // test running its own job beside it.
        let marker = format!("reclaim-probe-{}", unique_tag());
        exec.execute(
            &text_job(&format!("echo x > ./{marker}")),
            Duration::from_secs(60),
        )
        .await
        .expect("the job runs");
        // The explicit reclaim must remove the bind-mounted scratch dir;
        // no surviving compute-job-* tree may still hold the marker.
        let leaked = scratch_dirs().iter().any(|d| d.join(&marker).exists());
        assert!(
            !leaked,
            "the job's scratch dir was not reclaimed after it ran"
        );
    }

    #[tokio::test]
    #[ignore = "requires a working docker engine and pulls alpine:3.20"]
    async fn live_container_denies_egress_by_default() {
        let exec = live_executor(&unique_tag());
        // A raw IP (no DNS) so the probe fails on the missing route, not on
        // name resolution; `--network none` must make it unreachable. The
        // command exits 0 either way so stdout carries the verdict.
        let cmd = "if wget -T 2 -q -O- http://1.1.1.1 >/dev/null 2>&1; \
                   then echo REACHED-NET; else echo egress-denied; fi";
        let outcome = exec
            .execute(&text_job(cmd), Duration::from_secs(60))
            .await
            .expect("the job runs");
        let text = output_text(&outcome);
        assert!(
            text.contains("egress-denied"),
            "the default posture must deny egress: {text:?}"
        );
        assert!(
            !text.contains("REACHED-NET"),
            "a stranger's job must not reach the network: {text:?}"
        );
    }

    #[tokio::test]
    #[ignore = "requires a working docker engine and pulls alpine:3.20"]
    async fn live_container_kills_a_job_that_overruns_its_deadline() {
        let exec = live_executor(&unique_tag());
        let job = text_job("sleep 120");
        let name = format!("{}{}", exec.name_prefix(), job.job_id);
        let started = Instant::now();
        let err = exec
            .execute(&job, Duration::from_secs(2))
            .await
            .expect_err("a job past its deadline is killed, not awaited");
        assert!(matches!(err, ExecutorError::Timeout(_)), "got: {err:?}");
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the deadline kill must return promptly, took {:?}",
            started.elapsed()
        );
        assert!(
            !container_running(&name).await,
            "the overrunning container {name} must be killed, not left running"
        );
        let _ = Command::new("docker")
            .args(["rm", "-f", &name])
            .output()
            .await;
    }

    #[tokio::test]
    #[ignore = "requires a working docker engine and pulls alpine:3.20"]
    async fn live_container_surfaces_a_nonzero_exit_with_the_stderr_tail() {
        let exec = live_executor(&unique_tag());
        let err = exec
            .execute(
                &text_job("echo boom-on-stderr 1>&2; exit 3"),
                Duration::from_secs(60),
            )
            .await
            .expect_err("a nonzero container exit is a failed job");
        assert!(matches!(err, ExecutorError::Failed(_)), "got: {err:?}");
        assert!(
            err.to_string().contains("boom-on-stderr"),
            "the stderr tail must reach the operator: {err}"
        );
    }

    #[tokio::test]
    #[ignore = "requires a working docker engine and pulls alpine:3.20"]
    async fn live_container_reaps_an_orphan_a_previous_instance_left_running() {
        let exec = live_executor(&unique_tag());
        let name = format!("{}orphan", exec.name_prefix());
        // Stand in for a container a crashed node process left behind.
        let run = Command::new("docker")
            .args([
                "run",
                "-d",
                "--rm",
                "--name",
                &name,
                "alpine:3.20",
                "sleep",
                "300",
            ])
            .output()
            .await
            .expect("start the orphan");
        assert!(
            run.status.success(),
            "docker run -d failed: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        assert!(
            container_running(&name).await,
            "the orphan must be running before the sweep"
        );

        let reaped = exec.reap_orphans().await;
        assert!(
            reaped >= 1,
            "the sweep must reap the orphan, reaped {reaped}"
        );
        assert!(
            !container_running(&name).await,
            "the orphan must be gone after the sweep"
        );
        let _ = Command::new("docker")
            .args(["rm", "-f", &name])
            .output()
            .await;
    }

    #[tokio::test]
    #[ignore = "requires a working docker engine and pulls hello-world"]
    async fn live_container_ensure_image_present_warms_a_missing_image() {
        // Drop the tiny hello-world image if present, then prove the boot
        // warmup leaves it present locally — so the first real job never
        // pays the pull inside its own deadline.
        let _ = Command::new("docker")
            .args(["rmi", "-f", "hello-world:latest"])
            .output()
            .await;
        let exec = ContainerJobExecutor::new(
            ContainerConfig {
                image: "hello-world:latest".into(),
                instance_tag: unique_tag(),
                ..ContainerConfig::default()
            },
            Arc::new(SubprocessTracker::new()),
            Duration::from_secs(2),
        );
        exec.ensure_image_present().await;
        let inspect = Command::new("docker")
            .args(["image", "inspect", "hello-world:latest"])
            .output()
            .await
            .expect("docker image inspect");
        assert!(
            inspect.status.success(),
            "the boot warmup must leave the configured image present locally"
        );
    }

    #[tokio::test]
    #[ignore = "requires a working docker engine and pulls alpine:3.20"]
    async fn live_container_refuses_output_past_the_ceiling_and_reaps_the_container() {
        let exec = live_executor(&unique_tag());
        // 5 MiB of output, past the 4 MiB stdout ceiling: a stranger's
        // job must not flood the node's memory, and the flood must fail
        // the job rather than truncate to a paid answer.
        let job = text_job("dd if=/dev/zero bs=1048576 count=5 2>/dev/null | tr '\\0' a");
        let name = format!("{}{}", exec.name_prefix(), job.job_id);
        let err = exec
            .execute(&job, Duration::from_secs(60))
            .await
            .expect_err("output past the ceiling is refused, not truncated to a paid answer");
        assert!(matches!(err, ExecutorError::Failed(_)), "got: {err:?}");
        assert!(
            err.to_string().contains("ceiling"),
            "the refusal must name the output ceiling: {err}"
        );
        assert!(
            !container_running(&name).await,
            "the flooding container {name} must be reaped, not left running"
        );
        let _ = Command::new("docker")
            .args(["rm", "-f", &name])
            .output()
            .await;
    }
}
