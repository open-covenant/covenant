//! Hiring a coding agent: one task against a repository, paid only once
//! another operator's check of the work passes. Shared by the CLI's `agent`
//! command and the MCP `compute.agent` tool, so both read a repository,
//! hide tests and settle the same way.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use covenant_compute_protocol::{
    agent_task_input, parse_agent_task_output, AcceptanceSpec, AgentCheckVerdict, AgentRuntime,
    AgentSkill, AgentTaskOutput, AgentTaskSpec, CommandOutcome, HiddenChecks, HiddenFile, JobKind,
    RepoSource,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::ToolSpec;
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    dispatch_agent_task, fetch_check_report, BuyerConfig, BuyerError, DispatchOutcome, JobRequest,
    VoteRoundView,
};

pub const AGENT_TOOL: &str = "compute.agent";
const DEFAULT_CHECK_IMAGE: &str = "python:3.12-slim";
const DEFAULT_CHECK_TIMEOUT_SECS: u32 = 300;
pub const DEFAULT_AGENT_DEADLINE_MS: u64 = 1_800_000;
/// The offer a task is posted with when the caller names none. A ceiling,
/// not a price: a passing task is charged what its build spent plus its
/// checks, and a failing one nothing.
pub const DEFAULT_AGENT_OFFER_MICRO_USDC: u64 = 700_000;

/// A task as a caller states it: where the code is, what to do, and what
/// the work must pass.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentArgs {
    /// A local git repository (sent as a bundle of its committed history)
    /// or a public https URL.
    pub repo: String,
    /// The commit to work from; a local repository's HEAD by default.
    #[serde(default)]
    pub commit: Option<String>,
    pub task: String,
    /// Commands the work must pass, run from the repository root with no
    /// network, in order.
    pub accept: Vec<String>,
    #[serde(default)]
    pub check_image: Option<String>,
    #[serde(default)]
    pub check_timeout_secs: Option<u32>,
    /// Paths the work may not change.
    #[serde(default)]
    pub protect: Vec<String>,
    /// Test files read from the working tree and shown only to checkers.
    #[serde(default)]
    pub hidden: Vec<String>,
    /// Commands run after the visible ones, also kept from the builder.
    #[serde(default)]
    pub hidden_accept: Vec<String>,
    /// `code.change` (the default) or `code.tests`.
    #[serde(default)]
    pub skill: Option<String>,
    /// For `code.tests`: files whose working-tree versions fix the bug the
    /// tests must catch. Only checkers see them.
    #[serde(default)]
    pub fix: Vec<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub price_micro_usdc: Option<u64>,
    #[serde(default)]
    pub deadline_ms: Option<u64>,
    /// Apply an accepted patch to the local repository's working tree.
    #[serde(default)]
    pub apply: bool,
}

/// A task ready to sign: the spec the builder sees and the checks it
/// does not.
#[derive(Debug, Clone)]
pub struct PreparedTask {
    pub spec: AgentTaskSpec,
    pub hidden: Option<HiddenChecks>,
}

/// How a hire ended.
#[derive(Debug)]
pub enum AgentOutcome {
    /// The work passed its check and the builder was paid. `patch` is the
    /// decoded change, ready for `git apply`.
    Accepted {
        outcome: Box<DispatchOutcome>,
        built: AgentTaskOutput,
        patch: Vec<u8>,
        verdict: Option<AgentCheckVerdict>,
        round: Option<VoteRoundView>,
        /// What the task was charged, which is at most the offer.
        charged_micro_usdc: Option<u64>,
    },
    /// Nothing was paid: the work failed its check, or no check could be
    /// completed, or the task never ran. The verdict says why when there
    /// is one.
    NotPaid {
        job_id: Uuid,
        status: String,
        reason: Option<String>,
        verdict: Option<AgentCheckVerdict>,
        round: Option<VoteRoundView>,
    },
}

pub fn prepare_agent_task(args: &AgentArgs) -> Result<PreparedTask, BuyerError> {
    let invalid = |msg: String| BuyerError::Protocol(msg);
    if args.accept.is_empty() {
        return Err(invalid(
            "name at least one acceptance command: work with nothing to pass is accepted unseen"
                .into(),
        ));
    }
    let remote = args.repo.starts_with("https://");
    let repo = if remote {
        RepoSource::Git {
            url: args.repo.clone(),
            commit: args
                .commit
                .clone()
                .ok_or_else(|| invalid("a repository URL needs the commit to work from".into()))?,
        }
    } else {
        local_bundle(Path::new(&args.repo), args.commit.as_deref())?
    };
    let skill = match args.skill.as_deref() {
        None | Some("code.change") => AgentSkill::CodeChange,
        Some("code.tests") => AgentSkill::CodeTests,
        Some(other) => {
            return Err(invalid(format!(
                "unknown skill {other:?}; this network takes code.change and code.tests"
            )))
        }
    };
    match (skill, args.fix.is_empty()) {
        (AgentSkill::CodeTests, true) => {
            return Err(invalid(
                "code.tests needs --fix: the file versions that fix the bug, which the tests \
                 must pass once applied"
                    .into(),
            ))
        }
        (AgentSkill::CodeChange, false) => {
            return Err(invalid("--fix belongs to code.tests".into()))
        }
        _ => {}
    }
    let hidden = if args.hidden.is_empty() && args.hidden_accept.is_empty() && args.fix.is_empty() {
        None
    } else {
        let root = if remote {
            PathBuf::from(".")
        } else {
            PathBuf::from(&args.repo)
        };
        Some(read_hidden_checks(
            &root,
            &repo,
            &args.hidden,
            &args.fix,
            args.hidden_accept.clone(),
        )?)
    };
    let spec = AgentTaskSpec {
        task: args.task.clone(),
        repo,
        acceptance: AcceptanceSpec {
            skill,
            image: args
                .check_image
                .clone()
                .unwrap_or_else(|| DEFAULT_CHECK_IMAGE.into()),
            commands: args.accept.clone(),
            timeout_secs: args
                .check_timeout_secs
                .unwrap_or(DEFAULT_CHECK_TIMEOUT_SECS),
            protected_paths: args.protect.clone(),
            hidden_sha256: hidden.as_ref().map(HiddenChecks::digest),
        },
        runtime: AgentRuntime::ClaudeCode,
        model: args.model.clone(),
    };
    spec.validate().map_err(|e| invalid(e.to_string()))?;
    Ok(PreparedTask { spec, hidden })
}

/// Posts the task and waits for its checked result.
pub async fn hire_agent(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
    task: PreparedTask,
    price_micro_usdc: u64,
    deadline_ms: u64,
) -> Result<AgentOutcome, BuyerError> {
    let input = agent_task_input(task.spec).map_err(|e| BuyerError::Protocol(e.to_string()))?;
    let request = JobRequest {
        kind: JobKind::AgentTask,
        input: vec![input],
        model: Some(AgentRuntime::ClaudeCode.label().into()),
        gpu_class: None,
        min_vram_gb: None,
        min_reputation_bps: None,
        price_micro_usdc,
        deadline_ms,
    };
    let outcome = match dispatch_agent_task(
        http,
        config,
        buyer_identity,
        request,
        task.hidden.as_ref(),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(BuyerError::NotServed {
            job_id,
            status,
            reason,
            ..
        }) => {
            let check = fetch_check_report(http, config, buyer_identity, job_id).await?;
            return Ok(AgentOutcome::NotPaid {
                job_id,
                status,
                reason,
                verdict: check.verdict,
                round: check.round,
            });
        }
        Err(e) => return Err(e),
    };
    let built = parse_agent_task_output(&outcome.output)
        .map_err(|e| BuyerError::Verification(format!("agent result: {e}")))?;
    let patch = base64::engine::general_purpose::STANDARD
        .decode(&built.patch_b64)
        .map_err(|e| BuyerError::Verification(format!("agent patch: {e}")))?;
    if covenant_compute_protocol::sha256_hex(&patch) != built.patch_sha256 {
        return Err(BuyerError::Verification(
            "the patch does not match the digest the checker vouched for".into(),
        ));
    }
    let check =
        fetch_check_report(http, config, buyer_identity, outcome.receipt.receipt.job_id).await?;
    Ok(AgentOutcome::Accepted {
        outcome: Box::new(outcome),
        built,
        patch,
        verdict: check.verdict,
        round: check.round,
        charged_micro_usdc: check.charged_micro_usdc,
    })
}

/// Applies an accepted patch to a local repository's working tree.
pub fn apply_patch(repo: &Path, patch: &[u8]) -> Result<(), BuyerError> {
    use std::io::Write as _;
    let mut child = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["apply", "--whitespace=nowarn", "-"])
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| BuyerError::Protocol(format!("run git apply: {e}")))?;
    child
        .stdin
        .take()
        .expect("stdin piped")
        .write_all(patch)
        .map_err(|e| BuyerError::Protocol(format!("feed git apply: {e}")))?;
    let out = child
        .wait_with_output()
        .map_err(|e| BuyerError::Protocol(format!("git apply: {e}")))?;
    if !out.status.success() {
        return Err(BuyerError::Protocol(format!(
            "the accepted patch does not apply to the working tree: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// Bundles a local repository's history up to HEAD, so the agent works
/// from the exact commit without the repository needing to be public.
pub fn local_bundle(path: &Path, commit: Option<&str>) -> Result<RepoSource, BuyerError> {
    let git = |args: &[&str]| -> Result<Vec<u8>, BuyerError> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .map_err(|e| BuyerError::Protocol(format!("run git: {e}")))?;
        if !out.status.success() {
            return Err(BuyerError::Protocol(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(out.stdout)
    };
    let text = |bytes: Vec<u8>| String::from_utf8_lossy(&bytes).trim().to_string();
    let head = text(git(&["rev-parse", "HEAD"])?);
    let commit = match commit {
        Some(c) => {
            let full = text(git(&["rev-parse", "--verify", &format!("{c}^{{commit}}")])?);
            git(&["merge-base", "--is-ancestor", &full, "HEAD"]).map_err(|_| {
                BuyerError::Protocol("the commit must be HEAD or one of its ancestors".into())
            })?;
            full
        }
        None => head,
    };
    let bytes = git(&["bundle", "create", "-q", "-", "HEAD"])?;
    Ok(RepoSource::Bundle {
        bundle_b64: base64::engine::general_purpose::STANDARD.encode(bytes),
        commit,
    })
}

/// Reads hidden test files and, for `code.tests`, the fix from the buyer's
/// working tree. A hidden test committed at the task's commit is refused:
/// the builder checks the commit out, so a committed "hidden" test is one it
/// can read.
pub fn read_hidden_checks(
    root: &Path,
    repo: &RepoSource,
    paths: &[String],
    fix: &[String],
    commands: Vec<String>,
) -> Result<HiddenChecks, BuyerError> {
    let mut files = Vec::with_capacity(paths.len() + fix.len());
    for path in paths {
        if let RepoSource::Bundle { commit, .. } = repo {
            let committed = std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(["cat-file", "-e", &format!("{commit}:{path}")])
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if committed {
                return Err(BuyerError::Protocol(format!(
                    "hidden test {path} is committed at the task's commit, so the builder would \
                     see it; keep hidden tests out of the commit"
                )));
            }
        }
        let bytes = std::fs::read(root.join(path))
            .map_err(|e| BuyerError::Protocol(format!("read hidden test {path}: {e}")))?;
        files.push(HiddenFile {
            path: path.clone(),
            content_b64: base64::engine::general_purpose::STANDARD.encode(bytes),
        });
    }
    files.extend(read_fix(root, repo, fix)?);
    let hidden = HiddenChecks { files, commands };
    hidden
        .validate()
        .map_err(|e| BuyerError::Protocol(e.to_string()))?;
    Ok(hidden)
}

/// The fix a `code.tests` task is judged against: each file as it stands in
/// the working tree, which must differ from the commit, or there is no bug
/// for the tests to catch.
fn read_fix(
    root: &Path,
    repo: &RepoSource,
    paths: &[String],
) -> Result<Vec<HiddenFile>, BuyerError> {
    let mut files = Vec::with_capacity(paths.len());
    for path in paths {
        let bytes = std::fs::read(root.join(path))
            .map_err(|e| BuyerError::Protocol(format!("read fix {path}: {e}")))?;
        if let RepoSource::Bundle { commit, .. } = repo {
            let committed = std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(["show", &format!("{commit}:{path}")])
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| o.stdout);
            if committed.as_deref() == Some(bytes.as_slice()) {
                return Err(BuyerError::Protocol(format!(
                    "fix {path} is the same as at the commit; edit it to the fixed version \
                     without committing"
                )));
            }
        }
        files.push(HiddenFile {
            path: path.clone(),
            content_b64: base64::engine::general_purpose::STANDARD.encode(bytes),
        });
    }
    Ok(files)
}

/// The chain's count in one line, with the transaction that records it.
pub fn describe_round(round: &VoteRoundView) -> String {
    let passes = round.votes.iter().filter(|v| v.passed).count();
    format!(
        "votes counted on chain: {} ({passes} pass, {} fail), settled in transaction {}",
        round.result,
        round.votes.len() - passes,
        round.signature
    )
}

/// A verdict as a short human-readable account: what ran and how it went,
/// with the tail of whatever failed.
pub fn describe_verdict(verdict: &AgentCheckVerdict) -> String {
    if !verdict.applied {
        return "check: the patch did not apply to the commit".into();
    }
    if !verdict.protected_violations.is_empty() {
        return format!(
            "check: the patch changed protected paths: {}",
            verdict.protected_violations.join(", ")
        );
    }
    let mut out = format!(
        "check: {}",
        if verdict.passed { "passed" } else { "failed" }
    );
    if verdict.skill == AgentSkill::CodeChange {
        describe_runs(&mut out, &verdict.commands, "  ");
        return out;
    }
    for (heading, runs) in [
        ("on the commit as it is (must pass):", &verdict.baseline),
        (
            "with the new tests (must fail, catching the bug):",
            &verdict.commands,
        ),
        (
            "with the new tests and your fix (must pass):",
            &verdict.fixed,
        ),
    ] {
        if runs.is_empty() {
            continue;
        }
        out.push_str(&format!("\n  {heading}"));
        describe_runs(&mut out, runs, "    ");
    }
    out
}

/// Each command with its result, and the tail of any that did not pass.
fn describe_runs(out: &mut String, outcomes: &[CommandOutcome], indent: &str) {
    for outcome in outcomes {
        let result = if outcome.timed_out {
            "timed out".to_string()
        } else {
            format!("exit {}", outcome.exit_code)
        };
        out.push_str(&format!(
            "\n{indent}$ {}  ({result}, {:.1}s)",
            outcome.command,
            outcome.duration_ms as f64 / 1000.0
        ));
        if !outcome.passed() {
            let tail: Vec<&str> = outcome.output_tail.trim_end().lines().collect();
            for line in &tail[tail.len().saturating_sub(20)..] {
                out.push_str(&format!("\n{indent}  {line}"));
            }
        }
    }
}

pub fn agent_tool_spec(max_price_micro_usdc: u64) -> ToolSpec {
    ToolSpec {
        name: AGENT_TOOL.into(),
        description: format!(
            "Hire a coding agent on the Covenant compute network to do one task in a git \
             repository. The agent works on another operator's machine and returns a patch. \
             A different operator then applies it to a clean copy of the commit and runs your \
             acceptance commands with no network. You pay only if they pass; otherwise \
             nothing is charged and the failing output is returned. The repository is sent as \
             its committed history, so commit what the agent should see and keep secrets out. \
             Price per call is capped at {max_price_micro_usdc} micro-USDC."
        ),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "repo": {
                    "type": "string",
                    "description": "Path to a local git repository, or a public https URL"
                },
                "commit": {
                    "type": "string",
                    "description": "Commit to work from. Defaults to the local repository's HEAD; required with a URL"
                },
                "task": {
                    "type": "string",
                    "description": "What the agent should do, in plain language"
                },
                "accept": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Commands the work must pass, run in order from the repository root with no network (e.g. \"python -m unittest -v\")"
                },
                "check_image": {
                    "type": "string",
                    "description": "Container image the commands run in. Default python:3.12-slim"
                },
                "check_timeout_secs": {
                    "type": "integer",
                    "description": "How long the commands may take, all together. Default 300"
                },
                "protect": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Paths the agent may not change, such as tests/"
                },
                "hidden": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Test files in the local working tree (not committed) that only the checker sees"
                },
                "hidden_accept": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Commands run after the visible ones, also kept from the agent"
                },
                "skill": {
                    "type": "string",
                    "enum": ["code.change", "code.tests"],
                    "description": "code.change (default): change the code until the commands pass. code.tests: write tests that catch a bug; they must fail on the commit and pass with your fix"
                },
                "fix": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "code.tests only: files in the local working tree (edited, not committed) that fix the bug. Only the checker sees them"
                },
                "model": {
                    "type": "string",
                    "description": "Model for the agent to drive. The operator's default otherwise"
                },
                "price_micro_usdc": {
                    "type": "integer",
                    "description": "Offered price in micro-USDC; defaults to the cheapest matching operator's ask"
                },
                "deadline_ms": {
                    "type": "integer",
                    "description": "Deadline for the build and its check together. Default 1800000 (30 minutes)"
                },
                "apply": {
                    "type": "boolean",
                    "description": "Apply an accepted patch to the local repository's working tree"
                }
            },
            "required": ["repo", "task", "accept"]
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_with_bug() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(
            dir.path().join("slugify.py"),
            "def slugify(t):\n    return t\n",
        )
        .unwrap();
        git(&["add", "."]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "bug",
        ]);
        dir
    }

    fn tests_args(repo: &Path) -> AgentArgs {
        AgentArgs {
            repo: repo.display().to_string(),
            commit: None,
            task: "write a regression test for the lowercase bug".into(),
            accept: vec!["python -m unittest".into()],
            check_image: None,
            check_timeout_secs: None,
            protect: vec!["slugify.py".into()],
            hidden: vec![],
            hidden_accept: vec![],
            skill: Some("code.tests".into()),
            fix: vec![],
            model: None,
            price_micro_usdc: None,
            deadline_ms: None,
            apply: false,
        }
    }

    #[test]
    fn code_tests_sends_the_working_tree_fix_to_checkers_only() {
        let repo = repo_with_bug();
        let mut args = tests_args(repo.path());
        assert!(prepare_agent_task(&args).is_err(), "code.tests needs a fix");

        args.fix = vec!["slugify.py".into()];
        assert!(
            prepare_agent_task(&args).is_err(),
            "a fix identical to the commit fixes nothing"
        );

        let fixed = "def slugify(t):\n    return t.lower()\n";
        std::fs::write(repo.path().join("slugify.py"), fixed).unwrap();
        let task = prepare_agent_task(&args).unwrap();
        assert_eq!(task.spec.acceptance.skill, AgentSkill::CodeTests);
        let hidden = task.hidden.expect("the fix travels as hidden checks");
        assert_eq!(hidden.files.len(), 1);
        assert_eq!(hidden.files[0].path, "slugify.py");
        assert_eq!(
            hidden.files[0].content_b64,
            base64::engine::general_purpose::STANDARD.encode(fixed)
        );
        assert_eq!(task.spec.acceptance.hidden_sha256, Some(hidden.digest()));

        args.skill = None;
        assert!(
            prepare_agent_task(&args).is_err(),
            "--fix belongs to code.tests"
        );
        args.skill = Some("code.audit".into());
        args.fix.clear();
        assert!(
            prepare_agent_task(&args).is_err(),
            "an unknown skill is refused"
        );
    }
}
