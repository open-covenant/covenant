//! Typed agent work over the envelope's `Vec<Content>`.
//!
//! A [`JobKind::AgentTask`](crate::JobKind::AgentTask) job hires a coding
//! agent: a task in prose, the repository it applies to, and the acceptance
//! checks the work must pass. The operator runs the agent and answers with a
//! patch against the named commit. A
//! [`JobKind::AgentCheck`](crate::JobKind::AgentCheck) job is the other half:
//! a different operator applies that patch to a fresh copy of the same commit,
//! runs the same checks with no network, and answers with a verdict. The
//! coordinator pays for the first job only once a check it ordered passes.
//!
//! Each direction travels as one `Content::Json` block keyed by name, the
//! same posture as [`crate::lease`] and [`crate::speech`], so the envelope's
//! wire form and every existing signature are untouched. Nothing here
//! carries a float: outputs are bound into receipts by a hash over their
//! JSON, so every number is an integer.

use covenant_mcp::Content;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::sign::ProtocolError;
use covenant_identity::{verify_b58, LocalIdentity};

/// Cap on the task prose. A task is instructions, not a payload; anything
/// the agent needs to read belongs in the repository.
pub const MAX_TASK_BYTES: usize = 32 * 1024;
/// Cap on an inline repository bundle, measured on its base64 text. A check
/// job carries the bundle and the builder's patch together, and both must
/// fit the 8 MiB frame with room to spare.
pub const MAX_BUNDLE_B64_BYTES: usize = 3 * 1024 * 1024;
/// Cap on the raw bytes of the patch a builder returns.
pub const MAX_PATCH_BYTES: usize = 1024 * 1024;
/// Cap on the base64 text of that patch: the encoding of [`MAX_PATCH_BYTES`].
pub const MAX_PATCH_B64_BYTES: usize = MAX_PATCH_BYTES.div_ceil(3) * 4;
pub const MAX_ACCEPTANCE_COMMANDS: usize = 8;
pub const MAX_COMMAND_BYTES: usize = 2048;
pub const MAX_PROTECTED_PATHS: usize = 32;
pub const MAX_HIDDEN_FILES: usize = 32;
/// Cap on the hidden files' base64 text, all files together (~512 KiB
/// decoded): tests, not fixtures.
pub const MAX_HIDDEN_B64_BYTES: usize = 700 * 1024;
const MAX_PATH_BYTES: usize = 256;
/// The longest the acceptance checks may run, all commands together.
pub const MAX_ACCEPTANCE_TIMEOUT_SECS: u32 = 1800;
/// How much of each command's output a verdict keeps: enough to read the
/// failing assertion, not a log store.
pub const MAX_OUTPUT_TAIL_BYTES: usize = 4096;
/// Cap on the builder's closing summary.
pub const MAX_SUMMARY_BYTES: usize = 4096;
const MAX_REPO_URL_BYTES: usize = 512;
const MAX_LABEL_BYTES: usize = 128;
/// Room a task's deadline must leave beyond the build window and the checks
/// themselves: cloning, applying the patch, matching a checker, and
/// settling. A deadline the work consumes whole refunds every task that
/// used its full window, after the builder did the work.
pub const AGENT_CHECK_SLACK_MS: u64 = 300_000;

/// The agent harness a builder runs. One variant today; the label is what
/// a node advertises in `models_served` and a task names as its required
/// model, so routing reaches only nodes that run the harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRuntime {
    ClaudeCode,
}

impl AgentRuntime {
    pub fn label(self) -> &'static str {
        match self {
            AgentRuntime::ClaudeCode => "claude-code",
        }
    }
}

/// Where the code comes from. A public git URL is fetched by the operator;
/// a bundle travels inline, which is how a buyer hands over a private
/// repository without handing over a credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum RepoSource {
    Git { url: String, commit: String },
    Bundle { bundle_b64: String, commit: String },
}

impl RepoSource {
    pub fn commit(&self) -> &str {
        match self {
            RepoSource::Git { commit, .. } | RepoSource::Bundle { commit, .. } => commit,
        }
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_commit("repo commit", self.commit())?;
        match self {
            RepoSource::Git { url, .. } => validate_repo_url(url),
            RepoSource::Bundle { bundle_b64, .. } => {
                validate_b64("repo bundle", bundle_b64, MAX_BUNDLE_B64_BYTES)
            }
        }
    }
}

/// What the work must pass. The checker runs `commands` in order inside
/// `image` with no network, the patched repository as the working
/// directory; every command must exit 0 inside `timeout_secs` overall.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptanceSpec {
    /// What the work is, which decides how a check reads the commands.
    /// Omitted for `code.change`, so nodes that predate skills still read
    /// those tasks.
    #[serde(default, skip_serializing_if = "AgentSkill::is_change")]
    pub skill: AgentSkill,
    pub image: String,
    pub commands: Vec<String>,
    pub timeout_secs: u32,
    /// Repo-relative paths the work may not touch (`tests/`, `Cargo.lock`).
    /// A builder that edits the checks it is judged by has not passed them,
    /// so a patch touching any of these fails its check unrun.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protected_paths: Vec<String>,
    /// Commitment (lowercase hex sha256, [`HiddenChecks::digest`]) to checks
    /// the buyer hands the coordinator apart from the task. Builders see
    /// only that they exist; checkers run them after the visible commands,
    /// and the commitment keeps anyone from swapping them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden_sha256: Option<String>,
}

/// The kinds of agent work a task can ask for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentSkill {
    /// Change the code so every command passes.
    #[default]
    #[serde(rename = "code.change")]
    CodeChange,
    /// Write tests that catch a described bug. Accepted when the commands
    /// pass on the commit as it is, fail once the new tests are in, and pass
    /// again with the buyer's fix, which travels as hidden files the builder
    /// never sees.
    #[serde(rename = "code.tests")]
    CodeTests,
}

impl AgentSkill {
    pub fn label(self) -> &'static str {
        match self {
            AgentSkill::CodeChange => "code.change",
            AgentSkill::CodeTests => "code.tests",
        }
    }

    fn is_change(&self) -> bool {
        *self == AgentSkill::CodeChange
    }
}

/// One file the hidden checks add to the checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HiddenFile {
    pub path: String,
    pub content_b64: String,
}

/// Checks a buyer keeps from the builder: files written into the checkout
/// after the builder's patch (tests it never saw), and commands run after
/// the visible ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HiddenChecks {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<HiddenFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
}

impl HiddenChecks {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.files.is_empty() && self.commands.is_empty() {
            return Err(ProtocolError::Invalid(
                "hidden checks need at least one file or command".into(),
            ));
        }
        if self.files.len() > MAX_HIDDEN_FILES {
            return Err(ProtocolError::Invalid(format!(
                "hidden checks carry {} files, past the {MAX_HIDDEN_FILES} limit",
                self.files.len()
            )));
        }
        let mut total = 0usize;
        for file in &self.files {
            validate_repo_path(&file.path)?;
            total += file.content_b64.len();
            if file.content_b64.is_empty() {
                continue;
            }
            validate_b64("hidden file", &file.content_b64, MAX_HIDDEN_B64_BYTES)?;
        }
        if total > MAX_HIDDEN_B64_BYTES {
            return Err(ProtocolError::Invalid(format!(
                "hidden files are {total} base64 bytes together, over the \
                 {MAX_HIDDEN_B64_BYTES} cap"
            )));
        }
        validate_commands("hidden", &self.commands, true)
    }

    /// The commitment a task carries for these checks.
    pub fn digest(&self) -> String {
        sha256_hex(&serde_json::to_vec(self).unwrap_or_default())
    }
}

/// The commands a check runs, in order: the visible ones, then the hidden.
pub fn check_commands<'a>(
    acceptance: &'a AcceptanceSpec,
    hidden: Option<&'a HiddenChecks>,
) -> Vec<&'a str> {
    acceptance
        .commands
        .iter()
        .chain(hidden.into_iter().flat_map(|h| h.commands.iter()))
        .map(String::as_str)
        .collect()
}

impl AcceptanceSpec {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_image(&self.image)?;
        if self.commands.is_empty() {
            return Err(ProtocolError::Invalid(
                "acceptance needs at least one command: work with nothing to pass is \
                 accepted unseen"
                    .into(),
            ));
        }
        validate_commands("acceptance", &self.commands, false)?;
        if let Some(digest) = &self.hidden_sha256 {
            validate_sha256_hex("hidden_sha256", digest)?;
        }
        if self.skill == AgentSkill::CodeTests && self.hidden_sha256.is_none() {
            return Err(ProtocolError::Invalid(
                "code.tests needs the fix as hidden files: tests that catch the bug are the \
                 ones that pass once it is fixed"
                    .into(),
            ));
        }
        if self.timeout_secs == 0 || self.timeout_secs > MAX_ACCEPTANCE_TIMEOUT_SECS {
            return Err(ProtocolError::Invalid(format!(
                "acceptance timeout_secs {} is outside 1..={MAX_ACCEPTANCE_TIMEOUT_SECS}",
                self.timeout_secs
            )));
        }
        if self.protected_paths.len() > MAX_PROTECTED_PATHS {
            return Err(ProtocolError::Invalid(format!(
                "acceptance protects {} paths, past the {MAX_PROTECTED_PATHS} limit",
                self.protected_paths.len()
            )));
        }
        for path in &self.protected_paths {
            validate_repo_path(path)?;
        }
        Ok(())
    }

    /// Whether these hidden checks can serve this acceptance: `code.tests`
    /// needs the fix among them as files.
    pub fn admits_hidden(&self, hidden: &HiddenChecks) -> Result<(), ProtocolError> {
        if self.skill == AgentSkill::CodeTests && hidden.files.is_empty() {
            return Err(ProtocolError::Invalid(
                "code.tests needs the fix among the hidden checks as files".into(),
            ));
        }
        Ok(())
    }

    /// Whether `path` (repo-relative, as git prints it) is one the work may
    /// not touch: the protected path itself or anything beneath it.
    pub fn protects(&self, path: &str) -> bool {
        self.protected_paths.iter().any(|p| {
            let p = p.trim_end_matches('/');
            path == p
                || path
                    .strip_prefix(p)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
    }
}

/// A buyer's agent task. Rejects unknown fields: a typo'd `acceptance` on
/// paid input must fail loudly, not run with no checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTaskSpec {
    pub task: String,
    pub repo: RepoSource,
    pub acceptance: AcceptanceSpec,
    pub runtime: AgentRuntime,
    /// The model the harness should drive. Advisory: a node runs its own
    /// default when absent, and the output reports what actually ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl AgentTaskSpec {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.task.trim().is_empty() {
            return Err(ProtocolError::Invalid("an agent task needs a task".into()));
        }
        if self.task.len() > MAX_TASK_BYTES {
            return Err(ProtocolError::Invalid(format!(
                "the task is {} bytes, over the {MAX_TASK_BYTES}-byte cap",
                self.task.len()
            )));
        }
        if self.task.contains('\0') {
            return Err(ProtocolError::Invalid(
                "the task contains a NUL byte".into(),
            ));
        }
        self.repo.validate()?;
        self.acceptance.validate()?;
        if let Some(model) = &self.model {
            validate_label("the agent model", model)?;
        }
        Ok(())
    }
}

/// What a builder hands back: its change as a binary-safe `git diff`
/// against the task's commit, and what the run cost it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTaskOutput {
    pub base_commit: String,
    pub patch_b64: String,
    /// Lowercase hex sha256 of the decoded patch bytes. The check job
    /// names the patch by this, and the verdict repeats it, so a verdict
    /// can only ever vouch for the exact bytes the builder signed.
    pub patch_sha256: String,
    pub files_changed: u32,
    /// The agent's closing message, trimmed to [`MAX_SUMMARY_BYTES`].
    pub summary: String,
    /// The model the harness reported running, when it reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The builder's own model spend for the run, as its guard metered it.
    pub spend_micro_usd: u64,
    /// The guard's run id, naming the builder's local signed run record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_run_id: Option<String>,
}

impl AgentTaskOutput {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_commit("base commit", &self.base_commit)?;
        if self.patch_b64.is_empty() {
            return Err(ProtocolError::Invalid(
                "an agent result carries an empty patch: the agent changed nothing".into(),
            ));
        }
        validate_b64("patch", &self.patch_b64, MAX_PATCH_B64_BYTES)?;
        validate_sha256_hex("patch_sha256", &self.patch_sha256)?;
        if self.summary.len() > MAX_SUMMARY_BYTES {
            return Err(ProtocolError::Invalid(format!(
                "the summary is {} bytes, over the {MAX_SUMMARY_BYTES}-byte cap",
                self.summary.len()
            )));
        }
        if let Some(model) = &self.model {
            validate_label("the reported model", model)?;
        }
        if let Some(run) = &self.guard_run_id {
            validate_label("the guard run id", run)?;
        }
        Ok(())
    }
}

/// The coordinator's order to check one builder's patch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCheckSpec {
    /// The task job whose patch this checks.
    pub task_job_id: Uuid,
    pub repo: RepoSource,
    pub acceptance: AcceptanceSpec,
    pub patch_b64: String,
    pub patch_sha256: String,
    /// The buyer's hidden checks, when the task committed to some. Present
    /// exactly when `acceptance.hidden_sha256` is, and matching it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<HiddenChecks>,
}

impl AgentCheckSpec {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.repo.validate()?;
        self.acceptance.validate()?;
        validate_b64("patch", &self.patch_b64, MAX_PATCH_B64_BYTES)?;
        validate_sha256_hex("patch_sha256", &self.patch_sha256)?;
        match (&self.acceptance.hidden_sha256, &self.hidden) {
            (None, None) => Ok(()),
            (Some(commitment), Some(hidden)) => {
                hidden.validate()?;
                self.acceptance.admits_hidden(hidden)?;
                if hidden.digest() != *commitment {
                    return Err(ProtocolError::Invalid(
                        "hidden checks do not match the task's commitment".into(),
                    ));
                }
                Ok(())
            }
            (Some(_), None) => Err(ProtocolError::Invalid(
                "the task committed to hidden checks the check does not carry".into(),
            )),
            (None, Some(_)) => Err(ProtocolError::Invalid(
                "hidden checks arrived for a task that committed to none".into(),
            )),
        }
    }
}

/// One acceptance command as the checker ran it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandOutcome {
    pub command: String,
    /// The command's exit code; `-1` when it was killed or never exited.
    pub exit_code: i32,
    pub duration_ms: u64,
    pub timed_out: bool,
    /// The last [`MAX_OUTPUT_TAIL_BYTES`] of its combined output.
    pub output_tail: String,
}

impl CommandOutcome {
    pub fn passed(&self) -> bool {
        self.exit_code == 0 && !self.timed_out
    }

    /// A real failure: the command ran to the end and said no. A timeout
    /// proves nothing about the code under test.
    pub fn caught(&self) -> bool {
        self.exit_code != 0 && !self.timed_out
    }
}

/// A checker's answer. `passed` is derived, not claimed: it must equal what
/// the evidence shows for the task's skill (for `code.change`, the patch
/// applied without touching a protected path and every command passed),
/// and a verdict whose flag disagrees with its own evidence is refused on
/// parse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCheckVerdict {
    #[serde(default, skip_serializing_if = "AgentSkill::is_change")]
    pub skill: AgentSkill,
    pub task_job_id: Uuid,
    pub patch_sha256: String,
    pub applied: bool,
    /// Protected paths the patch touched; any entry fails the check.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protected_violations: Vec<String>,
    pub passed: bool,
    /// The commands with the patch applied. For `code.tests` these must end
    /// in a real failure: the new tests catching the bug.
    pub commands: Vec<CommandOutcome>,
    /// `code.tests`: the visible commands on the commit before the patch,
    /// which must all pass, so a suite that was already red proves nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub baseline: Vec<CommandOutcome>,
    /// `code.tests`: every command with the patch and the buyer's fix, which
    /// must all pass, so the new tests fail on the bug and nothing else.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixed: Vec<CommandOutcome>,
    /// The checker's signature on [`agent_vote_message`] for this verdict, so
    /// the coordinator can put the vote on chain as the checker cast it.
    /// Absent from nodes that predate vote rounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vote_signature_b58: Option<String>,
}

impl AgentCheckVerdict {
    /// Builds a verdict whose `passed` follows from its evidence.
    pub fn new(
        task_job_id: Uuid,
        patch_sha256: String,
        applied: bool,
        protected_violations: Vec<String>,
        commands: Vec<CommandOutcome>,
    ) -> Self {
        let mut verdict = Self {
            skill: AgentSkill::CodeChange,
            task_job_id,
            patch_sha256,
            applied,
            protected_violations,
            passed: false,
            commands,
            baseline: Vec::new(),
            fixed: Vec::new(),
            vote_signature_b58: None,
        };
        verdict.passed = verdict.earned();
        verdict
    }

    /// A `code.tests` verdict: the commands before the patch, with it, and
    /// with it and the fix.
    #[allow(clippy::too_many_arguments)]
    pub fn catching(
        task_job_id: Uuid,
        patch_sha256: String,
        applied: bool,
        protected_violations: Vec<String>,
        baseline: Vec<CommandOutcome>,
        commands: Vec<CommandOutcome>,
        fixed: Vec<CommandOutcome>,
    ) -> Self {
        let mut verdict = Self::new(
            task_job_id,
            patch_sha256,
            applied,
            protected_violations,
            commands,
        );
        verdict.skill = AgentSkill::CodeTests;
        verdict.baseline = baseline;
        verdict.fixed = fixed;
        verdict.passed = verdict.earned();
        verdict
    }

    /// Signs this verdict's vote with the checker's node key.
    pub fn sign_vote(mut self, checker: &LocalIdentity) -> Self {
        if let Some(message) = agent_vote_message(self.task_job_id, &self.patch_sha256, self.passed)
        {
            let signature = checker.sign(&message).to_bytes();
            self.vote_signature_b58 = Some(bs58::encode(signature).into_string());
        }
        self
    }

    /// Whether the vote carries `checker_b58`'s signature on exactly this
    /// task, patch and verdict.
    pub fn vote_signed_by(&self, checker_b58: &str) -> bool {
        let (Some(signature), Some(message)) = (
            self.vote_signature_b58.as_deref(),
            agent_vote_message(self.task_job_id, &self.patch_sha256, self.passed),
        ) else {
            return false;
        };
        verify_b58(checker_b58, &message, signature).is_ok()
    }

    fn earned(&self) -> bool {
        let clean = self.applied && self.protected_violations.is_empty();
        let all_pass = |outcomes: &[CommandOutcome]| {
            !outcomes.is_empty() && outcomes.iter().all(CommandOutcome::passed)
        };
        match self.skill {
            AgentSkill::CodeChange => clean && all_pass(&self.commands),
            AgentSkill::CodeTests => {
                let caught = match self.commands.split_last() {
                    Some((last, before)) => {
                        last.caught() && before.iter().all(CommandOutcome::passed)
                    }
                    None => false,
                };
                clean && all_pass(&self.baseline) && caught && all_pass(&self.fixed)
            }
        }
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256_hex("patch_sha256", &self.patch_sha256)?;
        for phase in [&self.commands, &self.baseline, &self.fixed] {
            if phase.len() > 2 * MAX_ACCEPTANCE_COMMANDS {
                return Err(ProtocolError::Invalid(format!(
                    "a verdict reports {} commands in one run, more than a check can run",
                    phase.len()
                )));
            }
        }
        if self.skill == AgentSkill::CodeChange
            && !(self.baseline.is_empty() && self.fixed.is_empty())
        {
            return Err(ProtocolError::Invalid(
                "a code.change verdict reports runs only code.tests makes".into(),
            ));
        }
        for outcome in self
            .commands
            .iter()
            .chain(&self.baseline)
            .chain(&self.fixed)
        {
            if outcome.output_tail.len() > MAX_OUTPUT_TAIL_BYTES {
                return Err(ProtocolError::Invalid(format!(
                    "a command's output tail is {} bytes, over the {MAX_OUTPUT_TAIL_BYTES}-byte cap",
                    outcome.output_tail.len()
                )));
            }
        }
        if self.protected_violations.len() > MAX_PROTECTED_PATHS * 8 {
            return Err(ProtocolError::Invalid(
                "a verdict lists more protected-path violations than it can carry".into(),
            ));
        }
        if self
            .vote_signature_b58
            .as_ref()
            .is_some_and(|s| s.len() > MAX_VOTE_SIGNATURE_B58)
        {
            return Err(ProtocolError::Invalid(
                "a vote signature is longer than an ed25519 signature can encode to".into(),
            ));
        }
        let earned = self.earned();
        if self.passed != earned {
            return Err(ProtocolError::Invalid(format!(
                "a verdict claims passed={} but its evidence says {earned}",
                self.passed
            )));
        }
        Ok(())
    }
}

/// What a checker signs for one verdict: this domain, the task id, the patch
/// digest and one byte for the verdict. Byte for byte what the settlement
/// program's `record_vote` checks, and signed raw rather than as JSON so the
/// Ed25519 program can verify it on chain.
pub const AGENT_VOTE_DOMAIN: &[u8; 31] = b"covenant.compute.agent-vote.v1\n";
pub const AGENT_VOTE_MESSAGE_LEN: usize = 31 + 16 + 32 + 1;
const MAX_VOTE_SIGNATURE_B58: usize = 96;

/// The vote bytes for a verdict, or `None` when the patch digest is not 64
/// hex characters.
pub fn agent_vote_message(
    task_job_id: Uuid,
    patch_sha256: &str,
    passed: bool,
) -> Option<[u8; AGENT_VOTE_MESSAGE_LEN]> {
    if patch_sha256.len() != 64 {
        return None;
    }
    let mut message = [0u8; AGENT_VOTE_MESSAGE_LEN];
    message[..31].copy_from_slice(AGENT_VOTE_DOMAIN);
    message[31..47].copy_from_slice(task_job_id.as_bytes());
    for (i, pair) in patch_sha256.as_bytes().chunks(2).enumerate() {
        let pair = std::str::from_utf8(pair).ok()?;
        message[47 + i] = u8::from_str_radix(pair, 16).ok()?;
    }
    message[79] = u8::from(passed);
    Some(message)
}

/// Lowercase hex sha256 of `bytes`, the digest a patch is named by.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[derive(Serialize, Deserialize)]
struct TaskBlock {
    agent_task: AgentTaskSpec,
}

#[derive(Serialize, Deserialize)]
struct TaskOutputBlock {
    agent_task_result: AgentTaskOutput,
}

#[derive(Serialize, Deserialize)]
struct CheckBlock {
    agent_check: AgentCheckSpec,
}

#[derive(Serialize, Deserialize)]
struct VerdictBlock {
    agent_check_verdict: AgentCheckVerdict,
}

pub fn agent_task_input(spec: AgentTaskSpec) -> Result<Content, ProtocolError> {
    spec.validate()?;
    pack(TaskBlock { agent_task: spec })
}

/// Reads the task out of job input. Required on every `AgentTask`
/// envelope, so absence is an error here, unlike the advisory blocks.
pub fn parse_agent_task(input: &[Content]) -> Result<AgentTaskSpec, ProtocolError> {
    let block: TaskBlock = find(input, "agent_task")?;
    block.agent_task.validate()?;
    Ok(block.agent_task)
}

pub fn agent_task_output(output: AgentTaskOutput) -> Result<Vec<Content>, ProtocolError> {
    output.validate()?;
    Ok(vec![pack(TaskOutputBlock {
        agent_task_result: output,
    })?])
}

pub fn parse_agent_task_output(output: &[Content]) -> Result<AgentTaskOutput, ProtocolError> {
    let block: TaskOutputBlock = find(output, "agent_task_result")?;
    block.agent_task_result.validate()?;
    Ok(block.agent_task_result)
}

pub fn agent_check_input(spec: AgentCheckSpec) -> Result<Content, ProtocolError> {
    spec.validate()?;
    pack(CheckBlock { agent_check: spec })
}

pub fn parse_agent_check(input: &[Content]) -> Result<AgentCheckSpec, ProtocolError> {
    let block: CheckBlock = find(input, "agent_check")?;
    block.agent_check.validate()?;
    Ok(block.agent_check)
}

pub fn agent_check_output(verdict: AgentCheckVerdict) -> Result<Vec<Content>, ProtocolError> {
    verdict.validate()?;
    Ok(vec![pack(VerdictBlock {
        agent_check_verdict: verdict,
    })?])
}

pub fn parse_agent_check_verdict(output: &[Content]) -> Result<AgentCheckVerdict, ProtocolError> {
    let block: VerdictBlock = find(output, "agent_check_verdict")?;
    block.agent_check_verdict.validate()?;
    Ok(block.agent_check_verdict)
}

fn pack<T: Serialize>(block: T) -> Result<Content, ProtocolError> {
    serde_json::to_value(block)
        .map(Content::json)
        .map_err(|e| ProtocolError::Invalid(format!("agent block: {e}")))
}

fn find<T: serde::de::DeserializeOwned>(input: &[Content], key: &str) -> Result<T, ProtocolError> {
    for content in input {
        let Content::Json { value } = content else {
            continue;
        };
        if value.get(key).is_none() {
            continue;
        }
        return serde_json::from_value(value.clone())
            .map_err(|e| ProtocolError::Invalid(format!("{key}: {e}")));
    }
    Err(ProtocolError::Invalid(format!("no {key} block in the job")))
}

fn validate_commit(what: &str, commit: &str) -> Result<(), ProtocolError> {
    let hex = commit
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !hex || !(commit.len() == 40 || commit.len() == 64) {
        return Err(ProtocolError::Invalid(format!(
            "{what} must be a full lowercase hex object id (40 or 64 characters), got {commit:?}"
        )));
    }
    Ok(())
}

fn validate_commands(
    what: &str,
    commands: &[String],
    may_be_empty: bool,
) -> Result<(), ProtocolError> {
    if commands.is_empty() && !may_be_empty {
        return Err(ProtocolError::Invalid(format!(
            "{what} needs at least one command"
        )));
    }
    if commands.len() > MAX_ACCEPTANCE_COMMANDS {
        return Err(ProtocolError::Invalid(format!(
            "{what} lists {} commands, past the {MAX_ACCEPTANCE_COMMANDS} limit",
            commands.len()
        )));
    }
    for command in commands {
        if command.trim().is_empty() {
            return Err(ProtocolError::Invalid(format!(
                "an {what} command is empty"
            )));
        }
        if command.len() > MAX_COMMAND_BYTES {
            return Err(ProtocolError::Invalid(format!(
                "an {what} command of {} bytes exceeds the {MAX_COMMAND_BYTES}-byte cap",
                command.len()
            )));
        }
        if command.contains('\0') {
            return Err(ProtocolError::Invalid(format!(
                "an {what} command contains a NUL byte"
            )));
        }
    }
    Ok(())
}

fn validate_repo_path(path: &str) -> Result<(), ProtocolError> {
    let bad = path.trim().is_empty()
        || path.len() > MAX_PATH_BYTES
        || path.starts_with('/')
        || path.split('/').any(|part| part == "..")
        || path.chars().any(|c| c.is_control());
    if bad {
        return Err(ProtocolError::Invalid(format!(
            "protected path {path:?} must be a plain repo-relative path"
        )));
    }
    Ok(())
}

fn validate_sha256_hex(what: &str, value: &str) -> Result<(), ProtocolError> {
    let hex = value
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !hex || value.len() != 64 {
        return Err(ProtocolError::Invalid(format!(
            "{what} must be 64 lowercase hex characters"
        )));
    }
    Ok(())
}

/// A repository URL the operator will fetch on its own machine: https only,
/// so a task cannot point an operator at its local files or an ssh remote
/// that would use the operator's own keys, and no userinfo, so a credential
/// never rides a buyer-signed field into an operator's logs.
fn validate_repo_url(url: &str) -> Result<(), ProtocolError> {
    let Some(rest) = url.strip_prefix("https://") else {
        return Err(ProtocolError::Invalid(
            "a repo url must start with https://".into(),
        ));
    };
    if url.len() > MAX_REPO_URL_BYTES {
        return Err(ProtocolError::Invalid(format!(
            "a repo url is longer than {MAX_REPO_URL_BYTES} bytes"
        )));
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(ProtocolError::Invalid(
            "a repo url contains whitespace or a control character".into(),
        ));
    }
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err(ProtocolError::Invalid(
            "a repo url must name a host and carry no credentials".into(),
        ));
    }
    Ok(())
}

/// A container image reference: the characters docker accepts in a
/// repository, tag, or digest, and nothing a shell or a log would read
/// differently.
fn validate_image(image: &str) -> Result<(), ProtocolError> {
    let allowed = |c: char| {
        c.is_ascii_lowercase()
            || c.is_ascii_digit()
            || matches!(c, '.' | '_' | '-' | '/' | ':' | '@')
    };
    if image.is_empty() || image.len() > MAX_LABEL_BYTES || !image.chars().all(allowed) {
        return Err(ProtocolError::Invalid(format!(
            "acceptance image {image:?} is not a plain image reference"
        )));
    }
    Ok(())
}

fn validate_label(what: &str, value: &str) -> Result<(), ProtocolError> {
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/');
    if value.is_empty() || value.len() > MAX_LABEL_BYTES || !value.chars().all(allowed) {
        return Err(ProtocolError::Invalid(format!(
            "{what} {value:?} is not a plain label"
        )));
    }
    Ok(())
}

/// Length and alphabet only. Decoding is the consumer's job, at the moment
/// it needs the bytes; this keeps an obviously wrong blob from being priced.
fn validate_b64(what: &str, value: &str, cap: usize) -> Result<(), ProtocolError> {
    if value.is_empty() {
        return Err(ProtocolError::Invalid(format!("the {what} is empty")));
    }
    if value.len() > cap {
        return Err(ProtocolError::Invalid(format!(
            "the {what} is {} base64 bytes, over the {cap} cap",
            value.len()
        )));
    }
    if !value.len().is_multiple_of(4)
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
    {
        return Err(ProtocolError::Invalid(format!(
            "the {what} is not standard padded base64"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const SHA: &str = "aa00000000000000000000000000000000000000000000000000000000000000";

    fn acceptance() -> AcceptanceSpec {
        AcceptanceSpec {
            skill: AgentSkill::CodeChange,
            image: "python:3.12-slim".into(),
            commands: vec!["python -m unittest -v".into()],
            timeout_secs: 120,
            protected_paths: vec!["tests/".into()],
            hidden_sha256: None,
        }
    }

    fn task() -> AgentTaskSpec {
        AgentTaskSpec {
            task: "make the slugify tests pass".into(),
            repo: RepoSource::Bundle {
                bundle_b64: "AAAA".into(),
                commit: COMMIT.into(),
            },
            acceptance: acceptance(),
            runtime: AgentRuntime::ClaudeCode,
            model: Some("claude-sonnet-5-5".into()),
        }
    }

    fn output() -> AgentTaskOutput {
        AgentTaskOutput {
            base_commit: COMMIT.into(),
            patch_b64: "ZGlmZg==".into(),
            patch_sha256: SHA.into(),
            files_changed: 1,
            summary: "implemented slugify".into(),
            model: Some("claude-sonnet-5-5".into()),
            spend_micro_usd: 41_000,
            guard_run_id: Some("run-1".into()),
        }
    }

    fn passing(command: &str) -> CommandOutcome {
        CommandOutcome {
            command: command.into(),
            exit_code: 0,
            duration_ms: 900,
            timed_out: false,
            output_tail: "OK".into(),
        }
    }

    #[test]
    fn a_task_round_trips_through_its_input_block() {
        let packed = agent_task_input(task()).unwrap();
        assert_eq!(
            parse_agent_task(&[Content::text("noise"), packed]).unwrap(),
            task()
        );
    }

    #[test]
    fn a_task_block_is_required() {
        let err = parse_agent_task(&[Content::text("no block")]).unwrap_err();
        assert!(err.to_string().contains("no agent_task block"), "{err}");
    }

    #[test]
    fn a_task_refuses_an_unknown_field() {
        let mut value = serde_json::to_value(TaskBlock { agent_task: task() }).unwrap();
        value["agent_task"]["acceptence"] = serde_json::json!({});
        assert!(parse_agent_task(&[Content::json(value)]).is_err());
    }

    #[test]
    fn repo_urls_are_https_without_credentials() {
        let git = |url: &str| RepoSource::Git {
            url: url.into(),
            commit: COMMIT.into(),
        };
        assert!(git("https://github.com/open-covenant/demo.git")
            .validate()
            .is_ok());
        for bad in [
            "http://github.com/x/y",
            "file:///etc",
            "git@github.com:x/y.git",
            "https://user:token@github.com/x/y",
            "https:///nohost",
            "https://github.com/x y",
        ] {
            assert!(git(bad).validate().is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn commits_are_full_lowercase_object_ids() {
        for bad in ["abc", &COMMIT.to_uppercase(), &format!("{COMMIT}0")] {
            let repo = RepoSource::Git {
                url: "https://github.com/x/y".into(),
                commit: bad.to_string(),
            };
            assert!(repo.validate().is_err(), "{bad} must be refused");
        }
        let sha256_repo = RepoSource::Git {
            url: "https://github.com/x/y".into(),
            commit: "a".repeat(64),
        };
        assert!(sha256_repo.validate().is_ok());
    }

    #[test]
    fn acceptance_needs_bounded_commands_and_a_plain_image() {
        let mut none = acceptance();
        none.commands.clear();
        assert!(none.validate().is_err());

        let mut many = acceptance();
        many.commands = vec!["true".into(); MAX_ACCEPTANCE_COMMANDS + 1];
        assert!(many.validate().is_err());

        let mut long = acceptance();
        long.timeout_secs = MAX_ACCEPTANCE_TIMEOUT_SECS + 1;
        assert!(long.validate().is_err());

        for bad in ["", "Python:3", "python:3.12;rm -rf /", "img $(x)"] {
            let mut image = acceptance();
            image.image = bad.into();
            assert!(image.validate().is_err(), "{bad:?} must be refused");
        }
        let mut digest = acceptance();
        digest.image = "python@sha256:abcdef0123".into();
        assert!(digest.validate().is_ok());
    }

    #[test]
    fn bundles_are_bounded_padded_base64() {
        let bundle = |b: &str| RepoSource::Bundle {
            bundle_b64: b.into(),
            commit: COMMIT.into(),
        };
        assert!(bundle("AAA").validate().is_err());
        assert!(bundle("AA-_").validate().is_err());
        assert!(bundle(&"A".repeat(MAX_BUNDLE_B64_BYTES + 4))
            .validate()
            .is_err());
        assert!(bundle("QUJD").validate().is_ok());
    }

    #[test]
    fn an_empty_patch_is_no_result() {
        let mut empty = output();
        empty.patch_b64.clear();
        let err = agent_task_output(empty).unwrap_err();
        assert!(err.to_string().contains("changed nothing"), "{err}");
    }

    #[test]
    fn a_task_output_round_trips() {
        let packed = agent_task_output(output()).unwrap();
        assert_eq!(parse_agent_task_output(&packed).unwrap(), output());
    }

    #[test]
    fn a_verdict_derives_passed_from_its_evidence() {
        let id = Uuid::nil();
        let ok = AgentCheckVerdict::new(id, SHA.into(), true, vec![], vec![passing("t")]);
        assert!(ok.passed);

        let mut failed = passing("t");
        failed.exit_code = 1;
        assert!(!AgentCheckVerdict::new(id, SHA.into(), true, vec![], vec![failed]).passed);

        let mut slow = passing("t");
        slow.timed_out = true;
        assert!(!AgentCheckVerdict::new(id, SHA.into(), true, vec![], vec![slow]).passed);

        assert!(!AgentCheckVerdict::new(id, SHA.into(), false, vec![], vec![passing("t")]).passed);
        assert!(!AgentCheckVerdict::new(id, SHA.into(), true, vec![], vec![]).passed);
        let touched = vec!["tests/test_slug.py".to_string()];
        assert!(!AgentCheckVerdict::new(id, SHA.into(), true, touched, vec![passing("t")]).passed);
    }

    #[test]
    fn protected_paths_cover_the_path_and_what_is_under_it() {
        let spec = AcceptanceSpec {
            protected_paths: vec!["tests/".into(), "Cargo.lock".into(), "src/check".into()],
            ..acceptance()
        };
        assert!(spec.protects("tests/test_slug.py"));
        assert!(spec.protects("tests"));
        assert!(spec.protects("Cargo.lock"));
        assert!(spec.protects("src/check/mod.rs"));
        assert!(!spec.protects("src/checker.rs"));
        assert!(!spec.protects("testsuite/x.py"));
        assert!(!spec.protects("slug.py"));
    }

    #[test]
    fn protected_paths_are_plain_relative_paths() {
        for bad in ["", "/etc", "../x", "a/../b", "a\nb"] {
            let spec = AcceptanceSpec {
                protected_paths: vec![bad.into()],
                ..acceptance()
            };
            assert!(spec.validate().is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn a_verdict_claiming_more_than_its_evidence_is_refused() {
        let mut failed = passing("t");
        failed.exit_code = 2;
        let mut lying = AgentCheckVerdict::new(Uuid::nil(), SHA.into(), true, vec![], vec![failed]);
        lying.passed = true;
        let packed = serde_json::to_value(VerdictBlock {
            agent_check_verdict: lying,
        })
        .unwrap();
        let err = parse_agent_check_verdict(&[Content::json(packed)]).unwrap_err();
        assert!(err.to_string().contains("evidence"), "{err}");
    }

    fn failing(command: &str) -> CommandOutcome {
        let mut outcome = passing(command);
        outcome.exit_code = 1;
        outcome
    }

    #[test]
    fn tests_are_accepted_only_when_they_catch_the_bug_and_nothing_else() {
        let id = Uuid::nil();
        let verdict = |baseline, commands, fixed| {
            AgentCheckVerdict::catching(id, SHA.into(), true, vec![], baseline, commands, fixed)
        };
        let ok = verdict(vec![passing("t")], vec![failing("t")], vec![passing("t")]);
        assert!(ok.passed);
        assert!(ok.validate().is_ok());

        // Already red before the new tests: proves nothing.
        assert!(!verdict(vec![failing("t")], vec![failing("t")], vec![passing("t")]).passed);
        // The new tests do not fail on the bug.
        assert!(!verdict(vec![passing("t")], vec![passing("t")], vec![passing("t")]).passed);
        // They fail, but still fail with the fix: they catch something else.
        assert!(!verdict(vec![passing("t")], vec![failing("t")], vec![failing("t")]).passed);
        // A timeout is not a caught bug.
        let mut slow = failing("t");
        slow.timed_out = true;
        assert!(!verdict(vec![passing("t")], vec![slow], vec![passing("t")]).passed);
        // Nothing run in a phase.
        assert!(!verdict(vec![], vec![failing("t")], vec![passing("t")]).passed);
        assert!(!verdict(vec![passing("t")], vec![failing("t")], vec![]).passed);
        // Touching a protected path still fails it.
        let touched = AgentCheckVerdict::catching(
            id,
            SHA.into(),
            true,
            vec!["slugify.py".into()],
            vec![passing("t")],
            vec![failing("t")],
            vec![passing("t")],
        );
        assert!(!touched.passed);
    }

    #[test]
    fn a_change_verdict_carries_no_test_runs_and_skills_round_trip() {
        let mut change =
            AgentCheckVerdict::new(Uuid::nil(), SHA.into(), true, vec![], vec![passing("t")]);
        assert!(change.validate().is_ok());
        change.baseline = vec![passing("t")];
        assert!(change.validate().is_err());

        let json = serde_json::to_value(AgentCheckVerdict::new(
            Uuid::nil(),
            SHA.into(),
            true,
            vec![],
            vec![passing("t")],
        ))
        .unwrap();
        assert!(
            json.get("skill").is_none(),
            "code.change stays readable by older nodes"
        );

        let tests = AgentCheckVerdict::catching(
            Uuid::nil(),
            SHA.into(),
            true,
            vec![],
            vec![passing("t")],
            vec![failing("t")],
            vec![passing("t")],
        );
        let json = serde_json::to_value(&tests).unwrap();
        assert_eq!(json["skill"], "code.tests");
        let back: AgentCheckVerdict = serde_json::from_value(json).unwrap();
        assert_eq!(back, tests);
    }

    #[test]
    fn code_tests_needs_the_fix_as_hidden_files() {
        let mut acceptance = AcceptanceSpec {
            skill: AgentSkill::CodeTests,
            image: "python:3.12-slim".into(),
            commands: vec!["python -m unittest".into()],
            timeout_secs: 60,
            protected_paths: vec![],
            hidden_sha256: None,
        };
        assert!(acceptance.validate().is_err());
        acceptance.hidden_sha256 = Some("ab".repeat(32));
        assert!(acceptance.validate().is_ok());
        let commands_only = HiddenChecks {
            files: vec![],
            commands: vec!["python -m unittest".into()],
        };
        assert!(acceptance.admits_hidden(&commands_only).is_err());
        let fix = HiddenChecks {
            files: vec![HiddenFile {
                path: "slugify.py".into(),
                content_b64: "eA==".into(),
            }],
            commands: vec![],
        };
        assert!(acceptance.admits_hidden(&fix).is_ok());
    }

    #[test]
    fn a_vote_message_is_the_bytes_the_settlement_program_checks() {
        let task = Uuid::from_bytes([7u8; 16]);
        let patch = "09".repeat(32);
        let message = agent_vote_message(task, &patch, true).unwrap();
        assert_eq!(&message[..31], b"covenant.compute.agent-vote.v1\n");
        assert_eq!(&message[31..47], &[7u8; 16]);
        assert_eq!(&message[47..79], &[9u8; 32]);
        assert_eq!(message[79], 1);
        assert_eq!(agent_vote_message(task, &patch, false).unwrap()[79], 0);
        assert!(agent_vote_message(task, "09", true).is_none());
        assert!(agent_vote_message(task, &"zz".repeat(32), true).is_none());
    }

    #[test]
    fn a_signed_vote_verifies_only_for_its_checker_and_its_verdict() {
        let checker = LocalIdentity::generate("checker");
        let checker_b58 = bs58::encode(checker.pubkey_bytes()).into_string();
        let other = bs58::encode(LocalIdentity::generate("other").pubkey_bytes()).into_string();
        let verdict =
            AgentCheckVerdict::new(Uuid::new_v4(), SHA.into(), true, vec![], vec![passing("t")])
                .sign_vote(&checker);
        assert!(verdict.vote_signed_by(&checker_b58));
        assert!(!verdict.vote_signed_by(&other));

        let mut flipped = verdict.clone();
        flipped.commands.clear();
        flipped.passed = false;
        assert!(!flipped.vote_signed_by(&checker_b58));

        let unsigned = AgentCheckVerdict::new(Uuid::new_v4(), SHA.into(), true, vec![], vec![]);
        assert!(!unsigned.vote_signed_by(&checker_b58));
        let packed = agent_check_output(verdict.clone()).unwrap();
        assert_eq!(parse_agent_check_verdict(&packed).unwrap(), verdict);
    }

    #[test]
    fn a_check_round_trips() {
        let spec = AgentCheckSpec {
            task_job_id: Uuid::nil(),
            repo: task().repo,
            acceptance: acceptance(),
            patch_b64: "ZGlmZg==".into(),
            patch_sha256: SHA.into(),
            hidden: None,
        };
        let packed = agent_check_input(spec.clone()).unwrap();
        assert_eq!(parse_agent_check(&[packed]).unwrap(), spec);
    }

    #[test]
    fn sha256_hex_names_bytes_the_way_shasum_does() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn the_patch_cap_matches_its_encoding() {
        assert_eq!(MAX_PATCH_B64_BYTES, 1_398_104);
    }

    fn hidden() -> HiddenChecks {
        HiddenChecks {
            files: vec![HiddenFile {
                path: "tests/hidden/test_edges.py".into(),
                content_b64: "aW1wb3J0IHVuaXR0ZXN0Cg==".into(),
            }],
            commands: vec!["python -m unittest tests.hidden.test_edges".into()],
        }
    }

    #[test]
    fn a_check_carries_exactly_the_hidden_checks_the_task_committed_to() {
        let mut acceptance = acceptance();
        acceptance.hidden_sha256 = Some(hidden().digest());
        let spec = |hidden: Option<HiddenChecks>| AgentCheckSpec {
            task_job_id: Uuid::nil(),
            repo: task().repo,
            acceptance: acceptance.clone(),
            patch_b64: "ZGlmZg==".into(),
            patch_sha256: SHA.into(),
            hidden,
        };
        assert!(spec(Some(hidden())).validate().is_ok());
        assert!(spec(None).validate().is_err(), "committed but missing");
        let mut swapped = hidden();
        swapped.commands = vec!["true".into()];
        assert!(
            spec(Some(swapped)).validate().is_err(),
            "swapped after the commitment"
        );

        let uncommitted = AgentCheckSpec {
            acceptance: self::acceptance(),
            ..spec(Some(hidden()))
        };
        assert!(
            uncommitted.validate().is_err(),
            "hidden checks with no commitment"
        );
    }

    #[test]
    fn hidden_checks_are_bounded_and_need_something_to_run() {
        assert!(hidden().validate().is_ok());
        assert!(HiddenChecks {
            files: vec![],
            commands: vec![]
        }
        .validate()
        .is_err());
        let mut escape = hidden();
        escape.files[0].path = "../outside.py".into();
        assert!(escape.validate().is_err());
        let files_only = HiddenChecks {
            files: hidden().files,
            commands: vec![],
        };
        assert!(
            files_only.validate().is_ok(),
            "visible commands can pick hidden files up"
        );
    }

    #[test]
    fn a_check_runs_the_visible_commands_then_the_hidden_ones() {
        let h = hidden();
        assert_eq!(
            check_commands(&acceptance(), Some(&h)),
            vec![
                "python -m unittest -v",
                "python -m unittest tests.hidden.test_edges"
            ]
        );
        assert_eq!(
            check_commands(&acceptance(), None),
            vec!["python -m unittest -v"]
        );
    }
}
