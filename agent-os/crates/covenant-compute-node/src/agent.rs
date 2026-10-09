//! The agent executor: one backend for both halves of agent work.
//!
//! A build ([`JobKind::AgentTask`]) checks the task's repository out, runs
//! Claude Code headless under `covguard` with the checkout as its only
//! writable workspace, and answers with the change as a binary-safe patch.
//! covguard is the policy layer: it pins the agent's egress to its own
//! metering proxy, caps the run's model spend, and (given a token) holds the
//! model credential so the agent never sees it.
//!
//! A check ([`JobKind::AgentCheck`]) applies another operator's patch to a
//! fresh checkout and runs the buyer's acceptance commands in a container
//! with no network, a read-only rootfs and every capability dropped, then
//! answers with a verdict. The commands are a stranger's code against a
//! stranger's patch, so they never run on the host.
//!
//! The repository's git directory lives outside the workspace and is never
//! handed to the agent. Everything the node itself runs against the tree
//! (`add`, `diff`, `apply`) reads that pristine directory with global and
//! system config switched off, so an agent that plants `core.fsmonitor` or
//! a filter driver in a `.git` of its own has planted it nowhere the node
//! looks.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine as _;
use covenant_compute_protocol::{
    agent_check_output, agent_task_output, check_commands, parse_agent_check, parse_agent_task,
    sha256_hex, AcceptanceSpec, AgentCheckVerdict, AgentSkill, AgentTaskOutput, AgentTaskSpec,
    CommandOutcome, HiddenChecks, JobEnvelopePayload, JobKind, RepoSource, MAX_OUTPUT_TAIL_BYTES,
    MAX_PATCH_BYTES, MAX_SUMMARY_BYTES,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use covenant_runtime::{preempt_subprocess_pg, SubprocessTracker, TrackedSubprocess};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::build_container::{
    builder_dockerfile, builder_image, container_names, wrapper_argv, Builder, ContainerBuilds,
};
use crate::executor::{configure_process_group, ExecutionOutcome, ExecutorError, JobExecutor};

/// Time kept back from a build's deadline for producing the patch and
/// answering, so a run that uses its whole window still gets paid for.
const BUILD_TAIL: Duration = Duration::from_secs(45);
/// Docker's own exit code for "the engine could not run the container".
/// That is the checker's failure, not the patch's, so it is an executor
/// error rather than evidence in a verdict.
const DOCKER_ENGINE_FAILURE: i32 = 125;

#[derive(Clone)]
pub struct AgentConfig {
    pub covguard_bin: String,
    pub claude_bin: String,
    pub git_bin: String,
    pub container_runtime: String,
    /// The model a build drives when the task names none.
    pub default_model: Option<String>,
    /// covguard's hard spend cap per build, in USD, as its CLI takes it.
    pub budget_usd: String,
    /// A model credential for covguard's proxy to hold and inject, read by
    /// covguard from this file so it never appears in a process list the
    /// agent can see. `None` forwards the agent's own login (a Claude
    /// subscription).
    pub credential: Option<AgentCredential>,
    /// Where per-job checkouts live. Must be visible to the container
    /// engine: on a VM-backed engine that means a shared path under the
    /// user's home, not the system temp dir.
    pub work_dir: PathBuf,
    /// Images a check may run in. A check naming any other image is refused.
    pub check_images: Vec<String>,
    pub check_memory: String,
    pub check_cpus: String,
    pub check_pids: u32,
    /// Distinguishes this node's containers on a shared engine.
    pub instance_tag: String,
    /// The node key, which signs each check's vote so the coordinator can
    /// put it on chain as cast.
    pub voter: Arc<LocalIdentity>,
    /// Where builds run: on this machine under covguard's sandbox, or in a
    /// container that sees only the checkout.
    pub builder: Builder,
}

/// Where the builds' model credential lives, and what it needs alongside.
#[derive(Clone)]
pub struct AgentCredential {
    pub file: PathBuf,
    /// An API key rather than a subscription sign-in token.
    pub api_key: bool,
    /// The workspace a key that is not scoped to one bills to.
    pub workspace: Option<String>,
}

impl AgentCredential {
    /// Checks the file holds a token without keeping it: covguard reads it
    /// again for each build.
    pub fn load(file: PathBuf, workspace: Option<String>) -> Result<Self, String> {
        let token = std::fs::read_to_string(&file)
            .map_err(|e| format!("read the agent credential at {}: {e}", file.display()))?;
        let token = token.trim();
        if token.is_empty() {
            return Err(format!(
                "the agent credential at {} is empty",
                file.display()
            ));
        }
        Ok(Self {
            file,
            api_key: !token.starts_with("sk-ant-oat"),
            workspace,
        })
    }
}

pub struct AgentExecutor {
    config: AgentConfig,
    tracker: Arc<SubprocessTracker>,
    preempt_grace: Duration,
}

/// What a finished build run reported.
struct AgentRun {
    summary: String,
    model: Option<String>,
    spend_micro_usd: u64,
    guard_run_id: Option<String>,
}

impl AgentExecutor {
    pub fn new(
        config: AgentConfig,
        tracker: Arc<SubprocessTracker>,
        preempt_grace: Duration,
    ) -> Self {
        Self {
            config,
            tracker,
            preempt_grace,
        }
    }

    /// Pulls every allowed check image that is not already present, so the
    /// first check a buyer pays for does not spend its window downloading.
    pub async fn ensure_images_present(&self) {
        self.pull_check_images().await;
        if let Builder::Container(builds) = &self.config.builder {
            self.prepare_container_builds(builds).await;
        }
    }

    /// The internal network, the forwarder image and a builder image per
    /// check image. Each step logs and carries on: a build that needs a
    /// missing piece fails on its own, with a reason.
    async fn prepare_container_builds(&self, builds: &ContainerBuilds) {
        let runtime = &self.config.container_runtime;
        let quiet = |args: &[&str]| {
            let mut cmd = Command::new(runtime);
            cmd.args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            cmd
        };
        let exists = quiet(&["network", "inspect", &builds.network])
            .status()
            .await
            .is_ok_and(|s| s.success());
        if !exists
            && !quiet(&["network", "create", "--internal", &builds.network])
                .status()
                .await
                .is_ok_and(|s| s.success())
        {
            tracing::warn!(network = %builds.network, "could not create the build network");
        }
        let have_forwarder = quiet(&["image", "inspect", &builds.forwarder_image])
            .status()
            .await
            .is_ok_and(|s| s.success());
        if !have_forwarder
            && !quiet(&["pull", "--quiet", &builds.forwarder_image])
                .status()
                .await
                .is_ok_and(|s| s.success())
        {
            tracing::warn!(image = %builds.forwarder_image, "could not pull the proxy forwarder");
        }
        for check_image in &self.config.check_images {
            let image = builder_image(check_image);
            if quiet(&["image", "inspect", &image])
                .status()
                .await
                .is_ok_and(|s| s.success())
            {
                continue;
            }
            tracing::info!(%image, "building the builder image");
            let child = Command::new(runtime)
                .args(["build", "--quiet", "-t", &image, "-"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn();
            let built = match child {
                Ok(mut child) => {
                    if let Some(mut stdin) = child.stdin.take() {
                        use tokio::io::AsyncWriteExt;
                        let _ = stdin
                            .write_all(builder_dockerfile(check_image).as_bytes())
                            .await;
                    }
                    child.wait_with_output().await
                }
                Err(e) => Err(e),
            };
            match built {
                Ok(out) if out.status.success() => {}
                Ok(out) => tracing::warn!(
                    %image,
                    stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                    "builder image build failed; builds in it will fail until it exists"
                ),
                Err(e) => tracing::warn!(%image, error = %e, "builder image build did not start"),
            }
        }
    }

    fn build_container_name(&self, job: &JobEnvelopePayload) -> String {
        format!("compute-build-{}-{}", self.config.instance_tag, job.job_id)
    }

    /// Whatever state a containerized build ended in, its containers go.
    async fn remove_build_containers(&self, job: &JobEnvelopePayload) {
        let names = container_names(&self.build_container_name(job));
        let _ = Command::new(&self.config.container_runtime)
            .args(["rm", "-f", &names[0], &names[1]])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }

    async fn pull_check_images(&self) {
        for image in &self.config.check_images {
            let present = Command::new(&self.config.container_runtime)
                .args(["image", "inspect", image])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await
                .is_ok_and(|s| s.success());
            if present {
                continue;
            }
            tracing::info!(%image, "pulling check image");
            let pulled = Command::new(&self.config.container_runtime)
                .args(["pull", "--quiet", image])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .await;
            match pulled {
                Ok(out) if out.status.success() => {}
                Ok(out) => tracing::warn!(
                    %image,
                    stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                    "check image pull failed; checks in it will fail until it is present"
                ),
                Err(e) => tracing::warn!(%image, error = %e, "check image pull did not start"),
            }
        }
    }

    async fn build(
        &self,
        job: &JobEnvelopePayload,
        until: Instant,
    ) -> Result<Vec<Content>, ExecutorError> {
        let spec = parse_agent_task(&job.input).map_err(invalid)?;
        let dir = self.job_dir(job, "build")?;
        let (git_dir, tree) = self.checkout(&spec.repo, dir.path(), until).await?;

        let wall = until
            .saturating_duration_since(Instant::now())
            .saturating_sub(BUILD_TAIL);
        if wall < Duration::from_secs(30) {
            return Err(ExecutorError::Failed(
                "too little of the job's window is left to run the agent".into(),
            ));
        }
        if matches!(self.config.builder, Builder::Container(_))
            && !self.config.check_images.contains(&spec.acceptance.image)
        {
            return Err(ExecutorError::Failed(format!(
                "this node builds only in its check images, and {} is not one",
                spec.acceptance.image
            )));
        }
        let budget = build_budget(&self.config.budget_usd, job.price_micro_usdc)
            .map_err(ExecutorError::Failed)?;
        let model = spec
            .model
            .clone()
            .or_else(|| self.config.default_model.clone());
        let run = self
            .run_agent(
                job,
                &tree,
                &spec.acceptance.image,
                &task_prompt(&spec),
                model.as_deref(),
                &budget,
                wall,
                until,
            )
            .await;
        if matches!(self.config.builder, Builder::Container(_)) {
            self.remove_build_containers(job).await;
        }
        let run = run?;

        let commit = spec.repo.commit();
        self.git(&git_dir, Some(&tree), &["add", "-A"], until)
            .await?;
        // An honest build never ships edits to the paths it is judged by;
        // whatever the agent did there is put back before the diff, so only
        // a node that skips this step can be caught by the checker for it.
        let touched = self.changed_paths(&git_dir, &tree, commit, until).await?;
        let reverted: Vec<&String> = touched
            .iter()
            .filter(|p| spec.acceptance.protects(p))
            .collect();
        if !reverted.is_empty() {
            tracing::warn!(job_id = %job.job_id, paths = ?reverted, "reverting the agent's edits to protected paths");
            let mut args = vec!["reset", "-q", commit, "--"];
            args.extend(reverted.iter().map(|p| p.as_str()));
            self.git(&git_dir, Some(&tree), &args, until).await?;
        }
        let files_changed = self
            .changed_paths(&git_dir, &tree, commit, until)
            .await?
            .len();
        let patch = self
            .git(
                &git_dir,
                Some(&tree),
                &[
                    "diff",
                    "--cached",
                    "--binary",
                    "--no-color",
                    "--no-ext-diff",
                    "--no-textconv",
                    commit,
                ],
                until,
            )
            .await?;
        if patch.is_empty() {
            return Err(ExecutorError::Failed(
                "the agent made no changes to the repository".into(),
            ));
        }
        if patch.len() > MAX_PATCH_BYTES {
            return Err(ExecutorError::Failed(format!(
                "the agent's change is {} bytes, over the {MAX_PATCH_BYTES}-byte patch cap",
                patch.len()
            )));
        }

        agent_task_output(AgentTaskOutput {
            base_commit: commit.to_string(),
            patch_sha256: sha256_hex(&patch),
            patch_b64: base64::engine::general_purpose::STANDARD.encode(&patch),
            files_changed: u32::try_from(files_changed).unwrap_or(u32::MAX),
            summary: run.summary,
            model: run.model,
            spend_micro_usd: run.spend_micro_usd,
            guard_run_id: run.guard_run_id,
        })
        .map_err(|e| ExecutorError::Failed(format!("build result: {e}")))
    }

    async fn check(
        &self,
        job: &JobEnvelopePayload,
        until: Instant,
    ) -> Result<Vec<Content>, ExecutorError> {
        let spec = parse_agent_check(&job.input).map_err(invalid)?;
        let acceptance = &spec.acceptance;
        if !self.config.check_images.contains(&acceptance.image) {
            return Err(ExecutorError::Failed(format!(
                "this node does not run checks in {}",
                acceptance.image
            )));
        }
        let patch = base64::engine::general_purpose::STANDARD
            .decode(&spec.patch_b64)
            .map_err(|e| ExecutorError::Failed(format!("patch is not base64: {e}")))?;
        if sha256_hex(&patch) != spec.patch_sha256 {
            return Err(ExecutorError::Failed(
                "the patch does not match the digest it was ordered under".into(),
            ));
        }

        let dir = self.job_dir(job, "check")?;
        let (git_dir, tree) = self.checkout(&spec.repo, dir.path(), until).await?;
        let patch_file = dir.path().join("change.patch");
        std::fs::write(&patch_file, &patch)
            .map_err(|e| ExecutorError::Failed(format!("write patch: {e}")))?;
        let patch_arg = patch_file.to_string_lossy().into_owned();
        let budget_end =
            (Instant::now() + Duration::from_secs(u64::from(acceptance.timeout_secs))).min(until);
        let tests = acceptance.skill == AgentSkill::CodeTests;

        // code.tests first runs the suite on the commit as it is: tests that
        // only fail because the suite was already red have caught nothing.
        // The tree is then put back exactly as checked out, so nothing the
        // run left behind reaches the patched runs.
        let baseline = if tests {
            let visible: Vec<&str> = acceptance.commands.iter().map(String::as_str).collect();
            let outcomes = self
                .run_phase(job, 0, acceptance, &tree, &visible, budget_end)
                .await?;
            let commit = spec.repo.commit();
            self.git(
                &git_dir,
                Some(&tree),
                &["reset", "-q", "--hard", commit],
                until,
            )
            .await?;
            self.git(&git_dir, Some(&tree), &["clean", "-q", "-fdx"], until)
                .await?;
            outcomes
        } else {
            Vec::new()
        };

        let verdict = |applied, violations, commands, fixed| {
            let verdict = if tests {
                AgentCheckVerdict::catching(
                    spec.task_job_id,
                    spec.patch_sha256.clone(),
                    applied,
                    violations,
                    baseline.clone(),
                    commands,
                    fixed,
                )
            } else {
                AgentCheckVerdict::new(
                    spec.task_job_id,
                    spec.patch_sha256.clone(),
                    applied,
                    violations,
                    commands,
                )
            };
            verdict.sign_vote(&self.config.voter)
        };
        if tests && !baseline.iter().all(CommandOutcome::passed) {
            tracing::info!(job_id = %job.job_id, "the suite fails before the new tests; nothing to catch");
            return finish(verdict(true, Vec::new(), Vec::new(), Vec::new()));
        }
        let applied = self
            .git(
                &git_dir,
                Some(&tree),
                &["apply", "--index", "--whitespace=nowarn", &patch_arg],
                until,
            )
            .await;
        if let Err(e) = applied {
            tracing::info!(job_id = %job.job_id, error = %e, "patch does not apply");
            return finish(verdict(false, Vec::new(), Vec::new(), Vec::new()));
        }
        let violations: Vec<String> = self
            .changed_paths(&git_dir, &tree, spec.repo.commit(), until)
            .await?
            .into_iter()
            .filter(|p| acceptance.protects(p))
            .collect();
        if !violations.is_empty() {
            return finish(verdict(true, violations, Vec::new(), Vec::new()));
        }

        // code.tests: the new tests must fail on the commit's bug before the
        // fix goes in. code.change goes straight to every command.
        let caught = if tests {
            let visible: Vec<&str> = acceptance.commands.iter().map(String::as_str).collect();
            let outcomes = self
                .run_phase(job, 100, acceptance, &tree, &visible, budget_end)
                .await?;
            if outcomes.last().is_none_or(|o| !o.caught()) {
                return finish(verdict(true, Vec::new(), outcomes, Vec::new()));
            }
            outcomes
        } else {
            Vec::new()
        };

        if let Some(hidden) = &spec.hidden {
            if let Err(e) = place_hidden_files(&tree, hidden) {
                tracing::info!(job_id = %job.job_id, error = %e, "hidden checks could not be placed");
                return finish(verdict(false, Vec::new(), caught, Vec::new()));
            }
        }

        let commands = check_commands(acceptance, spec.hidden.as_ref());
        let outcomes = self
            .run_phase(job, 200, acceptance, &tree, &commands, budget_end)
            .await?;
        if tests {
            finish(verdict(true, Vec::new(), caught, outcomes))
        } else {
            finish(verdict(true, Vec::new(), outcomes, Vec::new()))
        }
    }

    /// Runs `commands` in order, stopping at the first that does not pass.
    /// `first` numbers the containers so phases never reuse a name.
    async fn run_phase(
        &self,
        job: &JobEnvelopePayload,
        first: usize,
        acceptance: &AcceptanceSpec,
        tree: &Path,
        commands: &[&str],
        budget_end: Instant,
    ) -> Result<Vec<CommandOutcome>, ExecutorError> {
        let mut outcomes = Vec::with_capacity(commands.len());
        for (index, command) in commands.iter().enumerate() {
            let outcome = self
                .run_check_command(job, first + index, acceptance, tree, command, budget_end)
                .await?;
            let passed = outcome.passed();
            outcomes.push(outcome);
            if !passed {
                break;
            }
        }
        Ok(outcomes)
    }

    /// A fresh per-job directory under the work dir, removed when dropped.
    fn job_dir(
        &self,
        job: &JobEnvelopePayload,
        what: &str,
    ) -> Result<tempfile::TempDir, ExecutorError> {
        std::fs::create_dir_all(&self.config.work_dir)
            .map_err(|e| ExecutorError::Failed(format!("work dir: {e}")))?;
        tempfile::Builder::new()
            .prefix(&format!("{}-{what}-", job.job_id))
            .tempdir_in(&self.config.work_dir)
            .map_err(|e| ExecutorError::Failed(format!("job dir: {e}")))
    }

    /// Checks `repo` out at its commit into `root/repo`, with the git
    /// directory kept apart at `root/git`. Verifies the checkout landed on
    /// exactly the commit the job names.
    async fn checkout(
        &self,
        repo: &RepoSource,
        root: &Path,
        until: Instant,
    ) -> Result<(PathBuf, PathBuf), ExecutorError> {
        let git_dir = root.join("git");
        let tree = root.join("repo");
        std::fs::create_dir_all(&tree)
            .map_err(|e| ExecutorError::Failed(format!("checkout dir: {e}")))?;
        let init_target = git_dir.to_string_lossy().into_owned();
        self.git_in(&["init", "-q", "--bare", &init_target], root, until)
            .await?;
        let commit = repo.commit();
        match repo {
            RepoSource::Git { url, .. } => {
                let shallow = self
                    .git(
                        &git_dir,
                        None,
                        &["fetch", "-q", "--depth", "1", "--no-tags", url, commit],
                        until,
                    )
                    .await;
                if shallow.is_err() {
                    // Not every host serves a commit by id; fall back to the
                    // branches and find the commit among them.
                    self.git(
                        &git_dir,
                        None,
                        &[
                            "fetch",
                            "-q",
                            "--no-tags",
                            url,
                            "+refs/heads/*:refs/heads/*",
                        ],
                        until,
                    )
                    .await?;
                }
            }
            RepoSource::Bundle { bundle_b64, .. } => {
                let bundle = base64::engine::general_purpose::STANDARD
                    .decode(bundle_b64)
                    .map_err(|e| ExecutorError::Failed(format!("bundle is not base64: {e}")))?;
                let path = root.join("repo.bundle");
                std::fs::write(&path, bundle)
                    .map_err(|e| ExecutorError::Failed(format!("write bundle: {e}")))?;
                let path = path.to_string_lossy().into_owned();
                // Unbundling writes the objects without touching refs, so a
                // bundle carrying only HEAD (or no branch at all) still lands
                // the commit, and checkout below finds it by id.
                self.git(&git_dir, None, &["bundle", "unbundle", &path], until)
                    .await?;
            }
        }
        self.git(
            &git_dir,
            Some(&tree),
            &["checkout", "-q", "-f", "--detach", commit],
            until,
        )
        .await
        .map_err(|e| {
            ExecutorError::Failed(format!("the repository has no commit {commit}: {e}"))
        })?;
        let head = self
            .git(&git_dir, Some(&tree), &["rev-parse", "HEAD"], until)
            .await?;
        if String::from_utf8_lossy(&head).trim() != commit {
            return Err(ExecutorError::Failed(format!(
                "checkout landed on {} instead of {commit}",
                String::from_utf8_lossy(&head).trim()
            )));
        }
        Ok((git_dir, tree))
    }

    /// Paths the index differs from `commit` in, as git prints them.
    async fn changed_paths(
        &self,
        git_dir: &Path,
        tree: &Path,
        commit: &str,
        until: Instant,
    ) -> Result<Vec<String>, ExecutorError> {
        let out = self
            .git(
                git_dir,
                Some(tree),
                &[
                    "diff",
                    "--cached",
                    "--name-only",
                    "-z",
                    "--no-renames",
                    commit,
                ],
                until,
            )
            .await?;
        Ok(out
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect())
    }

    /// Runs git against the pristine `git_dir`. With a work tree, git also
    /// runs from inside it: `apply` resolves a patch's paths against the
    /// current directory, so from anywhere else it writes the change outside
    /// the checkout.
    async fn git(
        &self,
        git_dir: &Path,
        work_tree: Option<&Path>,
        args: &[&str],
        until: Instant,
    ) -> Result<Vec<u8>, ExecutorError> {
        let mut full = vec![format!("--git-dir={}", git_dir.display())];
        if let Some(tree) = work_tree {
            full.push(format!("--work-tree={}", tree.display()));
        }
        full.extend(args.iter().map(|a| a.to_string()));
        let refs: Vec<&str> = full.iter().map(String::as_str).collect();
        let cwd = work_tree.unwrap_or_else(|| git_dir.parent().unwrap_or(git_dir));
        self.git_in(&refs, cwd, until).await
    }

    /// Runs git with nothing of the operator's or the repository's
    /// configuration that could run a program: no global or system config,
    /// no hooks, no fsmonitor, no prompts, and no local or external
    /// transport beyond https and bundles.
    async fn git_in(
        &self,
        args: &[&str],
        cwd: &Path,
        until: Instant,
    ) -> Result<Vec<u8>, ExecutorError> {
        let mut cmd = Command::new(&self.config.git_bin);
        cmd.current_dir(cwd)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.bare=false",
                "-c",
                "protocol.allow=never",
                "-c",
                "protocol.https.allow=always",
                "-c",
                "protocol.file.allow=always",
                "-c",
                "advice.detachedHead=false",
            ])
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "/usr/bin/false")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let left = until.saturating_duration_since(Instant::now());
        let out = tokio::time::timeout(left, cmd.output())
            .await
            .map_err(|_| ExecutorError::Timeout(left))?
            .map_err(|e| ExecutorError::Failed(format!("spawn git: {e}")))?;
        if !out.status.success() {
            return Err(ExecutorError::Failed(format!(
                "git {} exited with {}: {}",
                args.first().copied().unwrap_or_default(),
                out.status,
                tail_str(&String::from_utf8_lossy(&out.stderr), 512)
            )));
        }
        Ok(out.stdout)
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_agent(
        &self,
        job: &JobEnvelopePayload,
        tree: &Path,
        image: &str,
        prompt: &str,
        model: Option<&str>,
        budget_usd: &str,
        wall: Duration,
        until: Instant,
    ) -> Result<AgentRun, ExecutorError> {
        let mut cmd = Command::new(&self.config.covguard_bin);
        cmd.args([
            "run",
            "--json",
            "--budget",
            budget_usd,
            "--wall",
            &format!("{}s", wall.as_secs()),
            "--workspace",
            &tree.to_string_lossy(),
        ]);
        if let Some(credential) = &self.config.credential {
            cmd.arg("--auth-token-file").arg(&credential.file);
        }
        let mut agent: Vec<String> = vec![
            match self.config.builder {
                Builder::Host => self.config.claude_bin.clone(),
                Builder::Container(_) => "claude".into(),
            },
            "-p".into(),
            prompt.into(),
            "--dangerously-skip-permissions".into(),
            "--output-format".into(),
            "json".into(),
        ];
        if let Some(model) = model {
            agent.extend(["--model".into(), model.into()]);
        }
        cmd.arg("--");
        match &self.config.builder {
            Builder::Host => {
                cmd.args(&agent);
            }
            Builder::Container(builds) => {
                let node = std::env::current_exe()
                    .map_err(|e| ExecutorError::Failed(format!("locate this binary: {e}")))?;
                cmd.args(wrapper_argv(
                    &node,
                    builds,
                    &self.build_container_name(job),
                    tree,
                    image,
                    &agent,
                ));
            }
        }
        // The node's own configuration stays out of the agent's reach, and
        // the agent makes no calls the guard's egress pin would refuse.
        for (key, _) in std::env::vars() {
            if key.starts_with("COVENANT_") {
                cmd.env_remove(key);
            }
        }
        if let Some(credential) = &self.config.credential {
            // An API key puts the agent in API-key mode: the placeholder is
            // all it holds, and the proxy swaps in the real key. A
            // subscription token keeps the agent's own sign-in mode.
            if credential.api_key {
                cmd.env("ANTHROPIC_API_KEY", "covguard-proxy-injected");
            }
            if let Some(workspace) = &credential.workspace {
                cmd.env(
                    "ANTHROPIC_CUSTOM_HEADERS",
                    format!("anthropic-workspace-id: {workspace}"),
                );
            }
        }
        cmd.current_dir(tree)
            .env("DISABLE_AUTOUPDATER", "1")
            .env("DISABLE_TELEMETRY", "1")
            .env("DISABLE_ERROR_REPORTING", "1")
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        configure_process_group(&mut cmd);

        let mut child = cmd.spawn().map_err(|e| {
            ExecutorError::Failed(format!("spawn {}: {e}", self.config.covguard_bin))
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
        let mut stdout = child.stdout.take().expect("stdout piped");
        let mut stderr = child.stderr.take().expect("stderr piped");
        let stdout_task = tokio::spawn(async move { read_tail(&mut stdout, 1024 * 1024).await });
        let stderr_task = tokio::spawn(async move { read_tail(&mut stderr, 4096).await });

        let left = until.saturating_duration_since(Instant::now());
        let status = match tokio::time::timeout(left, child.wait()).await {
            Ok(status) => status.map_err(|e| ExecutorError::Failed(format!("wait: {e}")))?,
            Err(_) => {
                if let Some(pid) = pid {
                    preempt_subprocess_pg(pid, self.preempt_grace).await;
                }
                return Err(ExecutorError::Timeout(left));
            }
        };
        let stdout = stdout_task.await.unwrap_or_default();
        let stderr = stderr_task.await.unwrap_or_default();
        let stdout = String::from_utf8_lossy(&stdout);
        if !status.success() {
            return Err(ExecutorError::Failed(format!(
                "the agent run exited with {status}: {}",
                tail_str(&String::from_utf8_lossy(&stderr), 1024)
            )));
        }
        parse_agent_run(&stdout)
    }

    async fn run_check_command(
        &self,
        job: &JobEnvelopePayload,
        index: usize,
        acceptance: &AcceptanceSpec,
        tree: &Path,
        command: &str,
        budget_end: Instant,
    ) -> Result<CommandOutcome, ExecutorError> {
        let name = format!(
            "compute-check-{}-{}-{index}",
            self.config.instance_tag, job.job_id
        );
        let owner = std::fs::metadata(tree)
            .map(|m| format!("{}:{}", m.uid(), m.gid()))
            .map_err(|e| ExecutorError::Failed(format!("stat checkout: {e}")))?;
        let mut cmd = Command::new(&self.config.container_runtime);
        cmd.args(["run", "--rm", "--name", &name, "--network", "none"])
            .args(["--memory", &self.config.check_memory])
            .args(["--cpus", &self.config.check_cpus])
            .args(["--pids-limit", &self.config.check_pids.max(1).to_string()])
            .args(["--read-only", "--cap-drop", "ALL"])
            .args(["--security-opt", "no-new-privileges"])
            .args(["--tmpfs", "/tmp:rw,nosuid,size=256m"])
            .args(["--user", &owner])
            .args(["-v", &format!("{}:/work:rw", tree.display())])
            .args([
                "-w",
                "/work",
                "-e",
                "HOME=/tmp",
                "-e",
                "PYTHONDONTWRITEBYTECODE=1",
            ])
            .arg(&acceptance.image)
            .args(["sh", "-c", &format!("exec 2>&1\n{command}")])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        configure_process_group(&mut cmd);

        let started = Instant::now();
        let mut child = cmd.spawn().map_err(|e| {
            ExecutorError::Failed(format!("spawn {}: {e}", self.config.container_runtime))
        })?;
        let mut stdout = child.stdout.take().expect("stdout piped");
        let mut stderr = child.stderr.take().expect("stderr piped");
        let stdout_task =
            tokio::spawn(async move { read_tail(&mut stdout, MAX_OUTPUT_TAIL_BYTES).await });
        let stderr_task = tokio::spawn(async move { read_tail(&mut stderr, 1024).await });

        let left = budget_end.saturating_duration_since(Instant::now());
        let waited = tokio::time::timeout(left, child.wait()).await;
        let (exit_code, timed_out) = match waited {
            Ok(Ok(status)) => (status.code().unwrap_or(-1), false),
            Ok(Err(e)) => return Err(ExecutorError::Failed(format!("wait for check: {e}"))),
            Err(_) => {
                self.kill_container(&name).await;
                if let Some(pid) = child.id() {
                    preempt_subprocess_pg(pid, self.preempt_grace).await;
                }
                (-1, true)
            }
        };
        let output = stdout_task.await.unwrap_or_default();
        let engine_err = stderr_task.await.unwrap_or_default();
        if exit_code == DOCKER_ENGINE_FAILURE {
            return Err(ExecutorError::Failed(format!(
                "the container engine could not run the check: {}",
                tail_str(&String::from_utf8_lossy(&engine_err), 512)
            )));
        }
        Ok(CommandOutcome {
            command: command.to_string(),
            exit_code,
            duration_ms: started.elapsed().as_millis() as u64,
            timed_out,
            output_tail: tail_str(&String::from_utf8_lossy(&output), MAX_OUTPUT_TAIL_BYTES),
        })
    }

    async fn kill_container(&self, name: &str) {
        let kill = Command::new(&self.config.container_runtime)
            .args(["kill", name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if tokio::time::timeout(Duration::from_secs(10), kill)
            .await
            .is_err()
        {
            tracing::warn!(
                container = name,
                "check container kill did not return in 10s"
            );
        }
    }

    async fn probe(&self, bin: &str, args: &[&str]) -> Result<(), ExecutorError> {
        let run = Command::new(bin)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output();
        match tokio::time::timeout(Duration::from_secs(10), run).await {
            Ok(Ok(out)) if out.status.success() => Ok(()),
            Ok(Ok(out)) => Err(ExecutorError::Failed(format!(
                "{bin} {} exited with {}: {}",
                args.join(" "),
                out.status,
                tail_str(&String::from_utf8_lossy(&out.stderr), 256)
            ))),
            Ok(Err(e)) => Err(ExecutorError::Failed(format!("{bin} is not runnable: {e}"))),
            Err(_) => Err(ExecutorError::Failed(format!(
                "{bin} {} did not answer in 10s",
                args.join(" ")
            ))),
        }
    }
}

#[async_trait]
impl JobExecutor for AgentExecutor {
    async fn execute(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let started = Instant::now();
        let until = started + deadline;
        let output = match job.kind {
            JobKind::AgentTask => self.build(job, until).await?,
            JobKind::AgentCheck => self.check(job, until).await?,
            other => {
                return Err(ExecutorError::Failed(format!(
                    "the agent executor does not run {other:?} jobs"
                )))
            }
        };
        Ok(ExecutionOutcome {
            output,
            wall_ms: started.elapsed().as_millis() as u64,
            tokens_in: None,
            tokens_out: None,
            finish_reason: None,
        })
    }

    /// Every tool both halves need must answer, and the container engine's
    /// daemon must be up: a stopped engine fails here instead of failing
    /// every check the matcher sends.
    async fn health(&self) -> Result<(), ExecutorError> {
        self.probe(&self.config.git_bin, &["--version"]).await?;
        self.probe(&self.config.covguard_bin, &["version"]).await?;
        if matches!(self.config.builder, Builder::Host) {
            self.probe(&self.config.claude_bin, &["--version"]).await?;
        }
        self.probe(&self.config.container_runtime, &["version"])
            .await
    }
}

fn invalid(e: covenant_compute_protocol::ProtocolError) -> ExecutorError {
    ExecutorError::Failed(format!("job input: {e}"))
}

fn finish(verdict: AgentCheckVerdict) -> Result<Vec<Content>, ExecutorError> {
    agent_check_output(verdict).map_err(|e| ExecutorError::Failed(format!("verdict: {e}")))
}

/// The least model spend a build is started with. Below it the agent cannot
/// read a repository and reply, so the job is refused rather than run to a
/// certain failure.
const MIN_BUILD_BUDGET_MICRO_USD: u64 = 50_000;

/// The build's spend cap: the operator's own cap, or five-eighths of the
/// buyer's offer if that is less. A passing build is charged its spend with
/// a fifth on top, plus its checks, and never more than the offer, so the
/// agent must not spend what the offer cannot pay back.
fn build_budget(configured_usd: &str, offer_micro_usdc: u64) -> Result<String, String> {
    let cap = configured_usd
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|usd| usd.is_finite() && *usd > 0.0)
        .map(|usd| (usd * 1_000_000.0) as u64)
        .ok_or_else(|| format!("the build budget {configured_usd:?} is not a dollar amount"))?;
    let budget = cap.min(offer_micro_usdc.saturating_mul(5) / 8);
    if budget < MIN_BUILD_BUDGET_MICRO_USD {
        return Err(format!(
            "an offer of {offer_micro_usdc} micro-USDC leaves {budget} micro-USD for the build, \
             under the {MIN_BUILD_BUDGET_MICRO_USD} a build needs"
        ));
    }
    Ok(format!("{}.{:06}", budget / 1_000_000, budget % 1_000_000))
}

/// The instructions a build hands the agent: the buyer's task, then what
/// the work will be judged by, so the agent can run the same checks itself.
fn task_prompt(spec: &AgentTaskSpec) -> String {
    let acceptance = &spec.acceptance;
    let tests = acceptance.skill == AgentSkill::CodeTests;
    let mut prompt = format!(
        "You are working in a checkout of a git repository at commit {}. Complete the task \
         below by editing files in this directory.\n\nTask:\n{}\n\nThe work is judged by \
         {}these commands, run from the repository root in the `{}` container image with no \
         network access:\n",
        spec.repo.commit(),
        spec.task.trim(),
        if tests { "" } else { "whether it passes " },
        acceptance.image
    );
    for command in &acceptance.commands {
        prompt.push_str(&format!("  $ {command}\n"));
    }
    if tests {
        prompt.push_str(
            "\nThis is a testing task. Write tests that catch the bug the task describes: with \
             your tests added, the commands above must fail on this commit because of that bug, \
             and pass once the bug is fixed. They pass today without your tests, and a fix you \
             cannot see is applied afterwards to check that your tests pass with it. Add or \
             change test files only: do not fix the bug, and do not weaken or remove existing \
             tests.\n",
        );
    } else if acceptance.hidden_sha256.is_some() {
        prompt.push_str(
            "\nFurther tests you cannot see will also run against your change, so make it \
             correct in general rather than only for the visible tests.\n",
        );
    }
    if !acceptance.protected_paths.is_empty() {
        prompt.push_str(&format!(
            "\nDo not modify these paths; edits to them are discarded: {}\n",
            acceptance.protected_paths.join(", ")
        ));
    }
    prompt.push_str(if tests {
        "\nDo not commit. Leave your changes in the working tree. When you are done, reply \
         with a short summary of the tests you added and why they fail on this commit."
    } else {
        "\nDo not commit. Leave your changes in the working tree. When you are done, reply \
         with a short summary of what you changed."
    });
    prompt
}

/// Writes the buyer's hidden files into the patched checkout. The builder's
/// patch can plant a symlink where a hidden file goes, and following it
/// would write outside the checkout on the operator's own disk, so every
/// directory on the way must be a plain directory and the file itself must
/// not be a link.
fn place_hidden_files(tree: &Path, hidden: &HiddenChecks) -> Result<(), String> {
    for file in &hidden.files {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&file.content_b64)
            .map_err(|e| format!("{} is not base64: {e}", file.path))?;
        let relative = Path::new(&file.path);
        let mut dir = tree.to_path_buf();
        if let Some(parent) = relative.parent() {
            for part in parent.components() {
                dir.push(part);
                match std::fs::symlink_metadata(&dir) {
                    Ok(meta) if meta.is_dir() => {}
                    Ok(_) => return Err(format!("{} is not a plain directory", dir.display())),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        std::fs::create_dir(&dir).map_err(|e| e.to_string())?
                    }
                    Err(e) => return Err(e.to_string()),
                }
            }
        }
        let target = tree.join(relative);
        if std::fs::symlink_metadata(&target).is_ok_and(|m| !m.is_file()) {
            return Err(format!("{} exists and is not a plain file", file.path));
        }
        std::fs::write(&target, bytes).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Reads the build's stdout: Claude Code's one-line JSON result and, as the
/// last line, covguard's run summary. The guard's metered spend is preferred
/// to the agent's own estimate; a run the guard stopped is a failed build.
fn parse_agent_run(stdout: &str) -> Result<AgentRun, ExecutorError> {
    let mut result: Option<serde_json::Value> = None;
    let mut guard: Option<serde_json::Value> = None;
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if value.get("type").and_then(|t| t.as_str()) == Some("result") {
            result = Some(value);
        } else if value.get("run_id").is_some() && value.get("outcome").is_some() {
            guard = Some(value);
        }
    }
    let result = result.ok_or_else(|| {
        ExecutorError::Failed("the agent produced no result; it may have been stopped".into())
    })?;
    if result.get("is_error").and_then(|v| v.as_bool()) == Some(true) {
        return Err(ExecutorError::Failed(format!(
            "the agent ended in error: {}",
            result
                .get("subtype")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
        )));
    }
    if let Some(guard) = &guard {
        let outcome = guard.get("outcome").and_then(|v| v.as_str()).unwrap_or("");
        if outcome != "completed" {
            return Err(ExecutorError::Failed(format!(
                "the guard stopped the agent: {outcome}"
            )));
        }
    }
    let spent_usd = guard
        .as_ref()
        .and_then(|g| g.get("spent_usd"))
        .or_else(|| result.get("total_cost_usd"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let model = result
        .get("modelUsage")
        .and_then(|m| m.as_object())
        .and_then(|usage| {
            usage
                .iter()
                .max_by(|a, b| {
                    let cost = |v: &serde_json::Value| {
                        v.get("costUSD").and_then(|c| c.as_f64()).unwrap_or(0.0)
                    };
                    cost(a.1).total_cmp(&cost(b.1))
                })
                .map(|(name, _)| name.clone())
        });
    let summary = result
        .get("result")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    Ok(AgentRun {
        summary: truncate_utf8(summary.trim(), MAX_SUMMARY_BYTES),
        model,
        spend_micro_usd: (spent_usd.max(0.0) * 1_000_000.0).round() as u64,
        guard_run_id: guard
            .as_ref()
            .and_then(|g| g.get("run_id"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

/// Reads a stream to its end, keeping only the last `keep` bytes.
async fn read_tail(stream: &mut (impl tokio::io::AsyncRead + Unpin), keep: usize) -> Vec<u8> {
    let mut tail = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    while let Ok(n) = stream.read(&mut chunk).await {
        if n == 0 {
            break;
        }
        tail.extend_from_slice(&chunk[..n]);
        if tail.len() > keep * 2 {
            let cut = tail.len() - keep;
            tail.drain(..cut);
        }
    }
    if tail.len() > keep {
        let cut = tail.len() - keep;
        tail.drain(..cut);
    }
    tail
}

/// The last `max` bytes of `s`, cut on a character boundary.
fn tail_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

/// The first `max` bytes of `s`, cut on a character boundary.
fn truncate_utf8(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_build_spends_inside_the_offer() {
        assert_eq!(build_budget("0.50", 700_000).unwrap(), "0.437500");
        assert_eq!(build_budget("0.50", 10_000_000).unwrap(), "0.500000");
        assert!(
            build_budget("0.50", 20_000).is_err(),
            "too little to start a build"
        );
        assert!(build_budget("nonsense", 700_000).is_err());
    }

    use super::*;

    #[test]
    fn a_run_reports_the_guards_spend_and_the_costliest_model() {
        let stdout = concat!(
            r#"{"type":"result","subtype":"success","is_error":false,"result":"  done  ","total_cost_usd":0.5,"modelUsage":{"claude-haiku-4-5":{"costUSD":0.01},"claude-sonnet-5-5":{"costUSD":0.2}}}"#,
            "\n",
            r#"{"run_id":"r-1","outcome":"completed","spent_usd":0.0822}"#,
            "\n"
        );
        let run = parse_agent_run(stdout).unwrap();
        assert_eq!(run.summary, "done");
        assert_eq!(run.spend_micro_usd, 82_200);
        assert_eq!(run.model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(run.guard_run_id.as_deref(), Some("r-1"));
    }

    #[test]
    fn a_run_the_guard_stopped_is_a_failed_build() {
        let stdout = concat!(
            r#"{"type":"result","subtype":"success","is_error":false,"result":"partial"}"#,
            "\n",
            r#"{"run_id":"r-2","outcome":"killed:budget","spent_usd":2.0}"#
        );
        let err = parse_agent_run(stdout).err().expect("must fail");
        assert!(err.to_string().contains("killed:budget"), "{err}");
    }

    #[test]
    fn a_run_without_a_result_or_ending_in_error_fails() {
        assert!(parse_agent_run("covguard: noise\n").is_err());
        let errored = r#"{"type":"result","subtype":"error_max_turns","is_error":true}"#;
        let err = parse_agent_run(errored).err().expect("must fail");
        assert!(err.to_string().contains("error_max_turns"), "{err}");
    }

    #[test]
    fn the_prompt_names_the_checks_and_the_protected_paths() {
        let spec = AgentTaskSpec {
            task: "implement slugify".into(),
            repo: RepoSource::Bundle {
                bundle_b64: "QUJD".into(),
                commit: "0123456789abcdef0123456789abcdef01234567".into(),
            },
            acceptance: AcceptanceSpec {
                skill: AgentSkill::CodeChange,
                image: "python:3.12-slim".into(),
                commands: vec!["python -m unittest".into()],
                timeout_secs: 60,
                protected_paths: vec!["tests/".into()],
                hidden_sha256: None,
            },
            runtime: covenant_compute_protocol::AgentRuntime::ClaudeCode,
            model: None,
        };
        let prompt = task_prompt(&spec);
        assert!(prompt.contains("implement slugify"));
        assert!(prompt.contains("$ python -m unittest"));
        assert!(prompt.contains("python:3.12-slim"));
        assert!(prompt.contains("tests/"));
    }

    #[test]
    fn tails_cut_on_character_boundaries() {
        assert_eq!(tail_str("héllo", 4), "llo");
        assert_eq!(truncate_utf8("héllo", 2), "h");
    }

    #[test]
    fn hidden_files_land_inside_the_checkout_and_never_through_a_link() {
        let root = tempfile::tempdir().unwrap();
        let tree = root.path().join("repo");
        std::fs::create_dir_all(tree.join("tests")).unwrap();
        let hidden = |path: &str| HiddenChecks {
            files: vec![covenant_compute_protocol::HiddenFile {
                path: path.into(),
                content_b64: base64::engine::general_purpose::STANDARD.encode("x = 1\n"),
            }],
            commands: vec![],
        };
        place_hidden_files(&tree, &hidden("tests/hidden/test_x.py")).unwrap();
        assert!(tree.join("tests/hidden/test_x.py").is_file());

        let outside = root.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, tree.join("planted")).unwrap();
        assert!(place_hidden_files(&tree, &hidden("planted/test_y.py")).is_err());
        assert!(
            !outside.join("test_y.py").exists(),
            "nothing written through the link"
        );

        std::os::unix::fs::symlink(outside.join("target.py"), tree.join("tests/link.py")).unwrap();
        assert!(place_hidden_files(&tree, &hidden("tests/link.py")).is_err());
        assert!(!outside.join("target.py").exists());
    }
}
