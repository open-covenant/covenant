//! Job execution, behind a trait so the real model-serving backend is
//! pluggable (design-01 §3 flags this explicitly — no `Runner` impl in
//! the codebase does GPU device passthrough). Container-grade
//! isolation lives in [`crate::container`]; this module holds the
//! trait, the mock, and the trusted-local subprocess backend.
//!
//! [`EchoExecutor`] is the mock used by the integration test.
//! [`SubprocessJobExecutor`] is real: it spawns the job's command in its
//! own process group and reuses `covenant-runtime`'s
//! [`covenant_runtime::SubprocessTracker`] and
//! [`covenant_runtime::preempt_subprocess_pg`] for hard, group-wide
//! deadline enforcement (`covenant-runtime/src/lib.rs:324-388,449-497`)
//! — the same tracker type and preempt function `covenantd`'s own
//! budget-projection tick calls (`covenantd/src/lib.rs:1743-1752`). It
//! does not reuse the `Runner` trait itself: `Runner::run(&AgentCard,
//! &Intent)` is shaped for an installed agent package, not a stranger's
//! dispatched job (design-01 §3) — this crate defines the job-shaped
//! equivalent instead.

use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use covenant_compute_protocol::{
    canonical_model, FinishReason, JobEnvelopePayload, ToolCall, ToolChoice, ToolChoiceMode,
};
use covenant_mcp::Content;
use covenant_runtime::{preempt_subprocess_pg, SubprocessTracker, TrackedSubprocess};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    #[error("timed out after {0:?}; process group was hard-preempted")]
    Timeout(Duration),
    #[error("execution failed: {0}")]
    Failed(String),
}

/// What an executor measured and produced. Wall time and token counts
/// come from the executor's own run, never from the buyer's request —
/// the same posture `covenantd/src/escrow.rs` takes toward completion
/// claims.
#[derive(Debug, Clone)]
pub struct ExecutionOutcome {
    pub output: Vec<Content>,
    pub wall_ms: u64,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    /// Why generation stopped, when the backend reports it (Ollama
    /// `done_reason`, an OpenAI-compatible `finish_reason`). `None` for
    /// embeddings, generic command jobs, and backends that omit it.
    pub finish_reason: Option<FinishReason>,
}

/// One model turn from a chat/generate backend call, before it is packed
/// into an [`ExecutionOutcome`]. `tool_calls` is empty for an ordinary
/// completion, so a non-tool job produces the same one-text-block output
/// it always has.
#[derive(Debug, Clone, Default)]
pub struct GenerationResult {
    pub text: String,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    pub finish_reason: Option<FinishReason>,
    pub tool_calls: Vec<ToolCall>,
    /// Per-token log probabilities, present only when the job asked for
    /// them. They ride the attested output, never the meter.
    pub logprobs: Option<Vec<covenant_compute_protocol::TokenLogprob>>,
}

/// Where a streaming execution emits incremental output. The node owns
/// the receiving end and relays batches to the coordinator; an executor
/// only ever sends. A failed send means nobody is listening anymore —
/// executors treat it as "stop emitting", never as a job failure.
pub type ChunkSink = tokio::sync::mpsc::Sender<String>;

/// Refuses a completion that carries neither prose nor tool calls — a
/// content-filter block or a backend that closed the stream having
/// emitted nothing. Whitespace-only text is no answer either: a receipt
/// over `"\n"` is a signed `Ok` the buyer paid for and got nothing usable
/// from, so it is refused the same as a byte-empty one. Failing the job
/// refunds the buyer; the alternative is settling that empty receipt.
/// Every generation path (both backends, streaming and not) runs a
/// completion through this before building an outcome, so whether an empty
/// answer is refused never turns on the `stream` flag.
pub(crate) fn ensure_answered(text: &str, tool_calls: &[ToolCall]) -> Result<(), ExecutorError> {
    if text.trim().is_empty() && tool_calls.is_empty() {
        return Err(ExecutorError::Failed(
            "backend returned an empty completion: no text and no tool calls".into(),
        ));
    }
    Ok(())
}

/// Refuses a completion that ignored a *constraining* `tool_choice`. A
/// buyer who set `required`, or named one function, paid to force a tool
/// call; one who set `none` paid for a plain answer with no call. A
/// backend with weak tool support can drop any of these — answering with
/// prose where a call was forced, calling some other function, or calling
/// a tool where none was allowed — and [`ensure_answered`] waves it
/// through because it only guards a wholly empty turn. Failing the job
/// here refunds the buyer instead of settling a completion that is not the
/// one they asked for. `auto` and an absent choice constrain nothing, so
/// they always pass.
pub(crate) fn ensure_tool_choice_honored(
    tool_choice: Option<&ToolChoice>,
    tool_calls: &[ToolCall],
) -> Result<(), ExecutorError> {
    match tool_choice {
        Some(ToolChoice::Mode(ToolChoiceMode::Required)) if tool_calls.is_empty() => {
            Err(ExecutorError::Failed(
                "backend ignored tool_choice=required and returned no tool call".into(),
            ))
        }
        Some(ToolChoice::Mode(ToolChoiceMode::None)) if !tool_calls.is_empty() => Err(
            ExecutorError::Failed("backend ignored tool_choice=none and called a tool".into()),
        ),
        Some(ToolChoice::Named(named)) => {
            let wanted = named.function.name.trim();
            if tool_calls.iter().any(|c| c.function.name.trim() == wanted) {
                Ok(())
            } else {
                Err(ExecutorError::Failed(format!(
                    "backend ignored tool_choice and did not call the required tool '{wanted}'"
                )))
            }
        }
        _ => Ok(()),
    }
}

/// Refuses an embedding output carrying a non-finite component. A backend
/// (or a JSON number too large to survive as an `f32`) can yield a
/// `NaN`/`inf` vector; signing it settles an `Ok` receipt the buyer pays
/// for and then cannot use — the buyer's own `parse_embedding_output`
/// rejects a non-finite value, so an unguarded node charges for an
/// embedding that reaches the buyer as a failure with nothing to dispute.
/// Failing the job here refunds instead, the same fail-closed posture
/// [`ensure_answered`] takes toward an empty completion. Both embedding
/// backends run a result through this before building an outcome.
pub(crate) fn ensure_finite_embeddings(embeddings: &[Vec<f32>]) -> Result<(), ExecutorError> {
    if embeddings.iter().flatten().any(|f| !f.is_finite()) {
        return Err(ExecutorError::Failed(
            "backend returned a non-finite embedding value".into(),
        ));
    }
    Ok(())
}

/// Refuses an embedding batch whose vectors are empty or not all one
/// width. The signed output reports a single `dimensions` — the first
/// vector's width, per [`covenant_compute_protocol::embedding_output`] —
/// so a backend that returns ragged vectors settles an `Ok` receipt over
/// a batch the buyer's own `parse_embedding_output` then rejects for a
/// width that disagrees with `dimensions`: paid for, unusable, nothing to
/// dispute. A batch of empty vectors is the same harm in the limit —
/// `dimensions` 0, no vector at all — and it slips the count and
/// finiteness checks (the counts match and there is nothing to be
/// non-finite), so it is refused here too. Failing the job refunds
/// instead, the same fail-closed posture [`ensure_finite_embeddings`]
/// takes toward a non-finite component. Both embedding backends run a
/// result through this before building an outcome.
pub(crate) fn ensure_uniform_embedding_width(embeddings: &[Vec<f32>]) -> Result<(), ExecutorError> {
    let mut widths = embeddings.iter().map(Vec::len);
    let Some(first) = widths.next() else {
        return Ok(());
    };
    if first == 0 {
        return Err(ExecutorError::Failed(
            "backend returned a zero-width embedding: no vector to embed with".into(),
        ));
    }
    if widths.any(|w| w != first) {
        return Err(ExecutorError::Failed(
            "backend returned embeddings of differing widths".into(),
        ));
    }
    Ok(())
}

/// Refuses a completion that dropped the buyer's paid-for logprobs. A
/// backend can accept the request and still answer without any — either
/// omitting the field, or (the gap a bare `is_none` check misses)
/// returning an empty array — and settle a plain completion at the full
/// price for a feature the buyer paid for and did not get. An answered
/// completion always scored at least one token, so an empty logprobs set
/// is a dropped feature, not a valid result. Failing the job refunds
/// instead, the same fail-closed posture [`ensure_answered`] takes toward
/// an empty completion. Runs only when the job asked for logprobs; both
/// backends check their result through this before building an outcome.
pub(crate) fn ensure_logprobs_delivered(
    requested: bool,
    logprobs: Option<&[covenant_compute_protocol::TokenLogprob]>,
) -> Result<(), ExecutorError> {
    if requested && logprobs.is_none_or(|l| l.is_empty()) {
        return Err(ExecutorError::Failed(
            "backend did not report the requested logprobs".into(),
        ));
    }
    Ok(())
}

#[async_trait]
pub trait JobExecutor: Send + Sync {
    /// Runs `job`, hard-bounded by `deadline`. Implementations must not
    /// let a hung job outlive `deadline` by more than their own grace
    /// window.
    async fn execute(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError>;

    /// Like [`JobExecutor::execute`], additionally emitting incremental
    /// output into `sink` as it is produced. The returned outcome must
    /// still carry the COMPLETE output: chunks are a live preview for
    /// the buyer, and the signed receipt derives from the outcome
    /// alone. The default ignores the sink and runs one-shot, so a
    /// backend that cannot stream still serves a streaming job — the
    /// envelope's `stream` flag is advisory by protocol design.
    async fn execute_streaming(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
        sink: ChunkSink,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        drop(sink);
        self.execute(job, deadline).await
    }

    /// Whether the backend can serve a job right now. The serve loop
    /// asks between jobs; an `Err` pauses job intake and reports the
    /// node offline until a later call succeeds, so an operator whose
    /// model server died doesn't bleed one reputation fault per matched
    /// job for the whole outage. Availability only — an individual
    /// job's failure is a job outcome, never a health verdict — and
    /// bounded in seconds: a wedged backend must stall the answer, not
    /// the loop. The default says always-ready, for executors whose
    /// backend is the local machine itself.
    async fn health(&self) -> Result<(), ExecutorError> {
        Ok(())
    }
}

/// The serving half of the honest-advertising contract
/// ([`crate::benchmark`] proves claims at registration; this re-proves
/// them between jobs): every model the node registered must still be on
/// the backend's live list, under the same `:latest` folding the
/// matcher routes by — a model the operator removed mid-serving would
/// otherwise fail every job the matcher keeps sending.
pub(crate) fn require_models_served(
    required: &[String],
    live: &[String],
) -> Result<(), ExecutorError> {
    for wanted in required {
        if !live
            .iter()
            .any(|m| canonical_model(m) == canonical_model(wanted))
        {
            return Err(ExecutorError::Failed(format!(
                "advertised model {wanted} is no longer on the backend (live: [{}])",
                live.join(", ")
            )));
        }
    }
    Ok(())
}

/// Mock/echo executor: returns `job.input` verbatim as the output,
/// measuring real (near-zero) wall time. No subprocess, no sandbox — the
/// executor the mocked integration test drives end-to-end.
#[derive(Debug, Default, Clone, Copy)]
pub struct EchoExecutor;

#[async_trait]
impl JobExecutor for EchoExecutor {
    async fn execute(
        &self,
        job: &JobEnvelopePayload,
        _deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let start = Instant::now();
        Ok(ExecutionOutcome {
            output: job.input.clone(),
            wall_ms: start.elapsed().as_millis() as u64,
            tokens_in: None,
            tokens_out: None,
            finish_reason: None,
        })
    }

    /// Echoes each input text block as one chunk, so federation tests
    /// exercise the whole chunk relay without a model server.
    async fn execute_streaming(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
        sink: ChunkSink,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        for block in &job.input {
            if let Content::Text { text } = block {
                if sink.send(text.clone()).await.is_err() {
                    break;
                }
            }
        }
        self.execute(job, deadline).await
    }
}

#[cfg(unix)]
pub(crate) fn configure_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.as_std_mut().process_group(0);
}

#[cfg(not(unix))]
pub(crate) fn configure_process_group(_cmd: &mut Command) {}

/// Ceiling on a job's stdout. Output rides back through the
/// coordinator's job record and journal, so an unbounded buffer is
/// both a node OOM and a coordinator-side amplification; a job that
/// exceeds it FAILS rather than silently truncating — a clipped result
/// under an `Ok` receipt would be paid-for data the buyer never got.
const MAX_STDOUT_BYTES: usize = 4 * 1024 * 1024;
/// How much trailing stderr is kept for failure detail.
const STDERR_TAIL_BYTES: usize = 4 * 1024;

/// Real executor for v1: the job's first `Content::Text` block is a
/// shell command, run under `sh -c`, tracked in the node's
/// [`SubprocessTracker`], and hard-preempted via
/// [`preempt_subprocess_pg`] if it outlives `deadline`.
///
/// A stranger's command gets a scrubbed environment (nothing of the
/// node's env — no `COVENANT_*` config, no operator `HOME` — just a
/// stock `PATH` and a `HOME` pointing into the scratch dir), a fresh
/// per-job scratch directory as its cwd (removed afterwards), a
/// bounded stdout, a drained-and-bounded stderr, and real exit-status
/// accounting: a non-zero exit is a failed job, never an empty-but-Ok
/// result the escrow would pay for.
///
/// Still not sandbox-grade isolation — the process runs as the node's
/// own user with open network egress and a readable filesystem. For
/// namespace isolation with default-deny egress, run
/// [`crate::container::ContainerJobExecutor`] instead
/// (`COVENANT_COMPUTE_NODE_EXECUTOR=container`); GPU passthrough
/// stays an open seam either way (design-01 §7).
pub struct SubprocessJobExecutor {
    tracker: Arc<SubprocessTracker>,
    preempt_grace: Duration,
}

impl SubprocessJobExecutor {
    pub fn new(tracker: Arc<SubprocessTracker>, preempt_grace: Duration) -> Self {
        Self {
            tracker,
            preempt_grace,
        }
    }
}

/// Reads `stdout` to EOF, failing as soon as more than
/// [`MAX_STDOUT_BYTES`] arrive — the child is still running when this
/// trips; the caller preempts it.
pub(crate) async fn read_stdout_capped(
    stdout: &mut (impl tokio::io::AsyncRead + Unpin),
) -> Result<Vec<u8>, ExecutorError> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let n = stdout
            .read(&mut chunk)
            .await
            .map_err(|e| ExecutorError::Failed(format!("io: {e}")))?;
        if n == 0 {
            return Ok(buf);
        }
        if buf.len() + n > MAX_STDOUT_BYTES {
            return Err(ExecutorError::Failed(format!(
                "output exceeded the {MAX_STDOUT_BYTES}-byte ceiling"
            )));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Reads an HTTP response body to EOF, failing as soon as more than
/// `cap` bytes arrive — the model-backend analog of
/// [`read_stdout_capped`]. The HTTP executors' non-streaming path used
/// to decode the body with an unbounded `resp.json()`, so a backend (a
/// remote openai-compatible endpoint especially) could return a body
/// past the streaming path's own ceiling and have it paid under an `Ok`
/// receipt, or simply OOM the node — the exact outcomes the streaming
/// assemblers already reject. Time stays bounded by the caller's
/// request `.timeout(deadline)`, which covers these incremental reads;
/// a stall there maps to `Timeout`, matching the streaming drain.
pub(crate) async fn read_body_capped(
    mut resp: reqwest::Response,
    cap: usize,
    deadline: Duration,
) -> Result<Vec<u8>, ExecutorError> {
    let mut buf = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(piece)) => {
                if buf.len() + piece.len() > cap {
                    return Err(ExecutorError::Failed(format!(
                        "backend response exceeded the {cap}-byte ceiling"
                    )));
                }
                buf.extend_from_slice(&piece);
            }
            Ok(None) => return Ok(buf),
            Err(e) if e.is_timeout() => return Err(ExecutorError::Timeout(deadline)),
            Err(e) => return Err(ExecutorError::Failed(format!("backend body: {e}"))),
        }
    }
}

/// The most of a non-2xx response body worth reading for an error
/// message. The error path only ever surfaces the first few hundred
/// characters, and a backend — a remote openai-compatible endpoint
/// especially — can answer an ordinary error (a missing model, a rate
/// limit, a proxy 502) with an arbitrarily large body, so reading it to
/// EOF is pure OOM surface with no diagnostic benefit.
const MAX_ERROR_SNIPPET_BYTES: usize = 16 * 1024;

/// Reads at most [`MAX_ERROR_SNIPPET_BYTES`] of an error response body,
/// stopping once it has that many rather than reading to EOF. The
/// error-path counterpart to [`read_body_capped`]: a success body that
/// overflows its ceiling fails the job (that data was paid for and must
/// not be silently truncated), but an error body is diagnostic only —
/// the status code, not the body, is the load-bearing part, so
/// truncating it is the right outcome. The request's own
/// `.timeout(deadline)` still bounds the read in time; this bounds it in
/// size, closing on the error path the same OOM vector the streaming and
/// non-streaming success paths already close with [`read_body_capped`].
pub(crate) async fn read_error_snippet(mut resp: reqwest::Response) -> String {
    let mut buf = Vec::new();
    while buf.len() < MAX_ERROR_SNIPPET_BYTES {
        match resp.chunk().await {
            Ok(Some(piece)) => {
                let room = MAX_ERROR_SNIPPET_BYTES - buf.len();
                if piece.len() > room {
                    buf.extend_from_slice(&piece[..room]);
                    break;
                }
                buf.extend_from_slice(&piece);
            }
            Ok(None) | Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Drains `stderr` continuously (a full pipe would block the child —
/// stderr-chatty jobs used to wedge until the deadline killed them),
/// keeping only the last [`STDERR_TAIL_BYTES`] for failure detail.
pub(crate) async fn drain_stderr_tail(
    mut stderr: impl tokio::io::AsyncRead + Unpin + Send,
) -> String {
    let mut tail: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8 * 1024];
    while let Ok(n) = stderr.read(&mut chunk).await {
        if n == 0 {
            break;
        }
        tail.extend_from_slice(&chunk[..n]);
        if tail.len() > STDERR_TAIL_BYTES {
            let cut = tail.len() - STDERR_TAIL_BYTES;
            tail.drain(..cut);
        }
    }
    String::from_utf8_lossy(&tail).trim().to_string()
}

#[async_trait]
impl JobExecutor for SubprocessJobExecutor {
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

        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(&command_text)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .current_dir(scratch.path())
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", scratch.path());
        configure_process_group(&mut cmd);

        let started = Instant::now();
        let mut child = cmd
            .spawn()
            .map_err(|e| ExecutorError::Failed(format!("spawn: {e}")))?;
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
            Ok(Ok(buf)) => {
                // Stdout hit EOF; bound the reap too — a child that
                // closed its pipes but refuses to exit must not hold
                // the job slot past its deadline machinery.
                match tokio::time::timeout(self.preempt_grace, child.wait()).await {
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
                            "command exited with {status}: {tail}"
                        )))
                    }
                    Ok(Err(e)) => Err(ExecutorError::Failed(format!("wait: {e}"))),
                    Err(_) => {
                        if let Some(pid) = pid {
                            preempt_subprocess_pg(pid, self.preempt_grace).await;
                        }
                        Err(ExecutorError::Failed(
                            "command closed stdout but did not exit; process group preempted"
                                .into(),
                        ))
                    }
                }
            }
            Ok(Err(e)) => {
                // Cap exceeded or read error — the child may well still
                // be running and writing.
                if let Some(pid) = pid {
                    preempt_subprocess_pg(pid, self.preempt_grace).await;
                }
                Err(e)
            }
            Err(_) => {
                if let Some(pid) = pid {
                    preempt_subprocess_pg(pid, self.preempt_grace).await;
                }
                Err(ExecutorError::Timeout(deadline))
            }
        };

        self.tracker.unregister(&job.job_id);
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

    fn job(command: &str, deadline_ms: u64) -> JobEnvelopePayload {
        JobEnvelopePayload {
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
            input: vec![Content::text(command)],
            price_micro_usdc: 10,
            deadline_ms,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "exec-test"),
            issued_at_ms: 0,
            referral_code: None,
            stream: false,
        }
    }

    fn tool_call(name: &str) -> ToolCall {
        ToolCall {
            id: "call_0".into(),
            kind: covenant_compute_protocol::ToolCallKind::Function,
            function: covenant_compute_protocol::FunctionCall {
                name: name.into(),
                arguments: "{}".into(),
            },
        }
    }

    fn force(name: &str) -> ToolChoice {
        ToolChoice::Named(covenant_compute_protocol::NamedToolChoice {
            kind: covenant_compute_protocol::ToolKind::Function,
            function: covenant_compute_protocol::NamedFunction { name: name.into() },
        })
    }

    #[test]
    fn tool_choice_honoring_guards_the_constraining_modes() {
        let required = ToolChoice::Mode(ToolChoiceMode::Required);
        // `required` passes with a call present, refunds with none.
        assert!(ensure_tool_choice_honored(Some(&required), &[tool_call("f")]).is_ok());
        assert!(ensure_tool_choice_honored(Some(&required), &[]).is_err());

        // A named choice passes only when that function was the one called.
        let weather = force("get_weather");
        assert!(ensure_tool_choice_honored(Some(&weather), &[tool_call("get_weather")]).is_ok());
        let err = ensure_tool_choice_honored(Some(&weather), &[tool_call("get_time")])
            .expect_err("a forced tool the backend skipped refunds");
        assert!(err.to_string().contains("get_weather"), "names it: {err}");

        // `none` forbids a call: a clean answer passes, a call refunds.
        let none = ToolChoice::Mode(ToolChoiceMode::None);
        assert!(ensure_tool_choice_honored(Some(&none), &[]).is_ok());
        assert!(ensure_tool_choice_honored(Some(&none), &[tool_call("f")]).is_err());

        // `auto` and an absent choice constrain nothing.
        let auto = ToolChoice::Mode(ToolChoiceMode::Auto);
        assert!(ensure_tool_choice_honored(Some(&auto), &[tool_call("f")]).is_ok());
        assert!(ensure_tool_choice_honored(None, &[]).is_ok());
    }

    #[test]
    fn a_non_finite_embedding_is_refused() {
        // Finite vectors pass; a NaN or inf component (a JSON number too
        // large for an f32 deserializes to inf) fails so the job refunds
        // rather than settling an Ok receipt over an unusable embedding.
        assert!(ensure_finite_embeddings(&[vec![0.1, 0.2], vec![0.3, 0.4]]).is_ok());
        assert!(ensure_finite_embeddings(&[]).is_ok());
        let err = ensure_finite_embeddings(&[vec![0.1, f32::INFINITY]]).unwrap_err();
        assert!(err.to_string().contains("non-finite"), "got: {err}");
        assert!(ensure_finite_embeddings(&[vec![f32::NAN]]).is_err());
    }

    #[test]
    fn a_ragged_embedding_batch_is_refused() {
        // A uniform batch (and the empty / single-vector edge cases) passes;
        // vectors of differing widths fail, since the signed `dimensions` is
        // the first vector's width and the buyer's parser rejects any vector
        // that disagrees with it — the node refunds rather than pay for it.
        assert!(ensure_uniform_embedding_width(&[vec![0.1, 0.2], vec![0.3, 0.4]]).is_ok());
        assert!(ensure_uniform_embedding_width(&[]).is_ok());
        assert!(ensure_uniform_embedding_width(&[vec![0.1, 0.2, 0.3]]).is_ok());
        let err = ensure_uniform_embedding_width(&[vec![0.1, 0.2], vec![0.3]]).unwrap_err();
        assert!(err.to_string().contains("width"), "got: {err}");
        // One empty vector per input clears the count and finiteness checks
        // but is no embedding at all — dimensions 0, unusable — so refuse it
        // rather than settle an Ok receipt the buyer pays for.
        let err = ensure_uniform_embedding_width(&[vec![], vec![]]).unwrap_err();
        assert!(err.to_string().contains("zero-width"), "got: {err}");
    }

    #[tokio::test]
    async fn echo_executor_returns_input_verbatim() {
        let executor = EchoExecutor;
        let j = job("unused", 1_000);
        let outcome = executor
            .execute(&j, Duration::from_millis(1_000))
            .await
            .unwrap();
        assert_eq!(outcome.output, j.input);
    }

    #[tokio::test]
    async fn echo_executor_streams_each_input_text_block_as_a_chunk() {
        let executor = EchoExecutor;
        let mut j = job("unused", 1_000);
        j.input = vec![Content::text("first"), Content::text("second")];
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let outcome = executor
            .execute_streaming(&j, Duration::from_millis(1_000), tx)
            .await
            .unwrap();
        assert_eq!(outcome.output, j.input);
        assert_eq!(rx.recv().await.as_deref(), Some("first"));
        assert_eq!(rx.recv().await.as_deref(), Some("second"));
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn the_default_streaming_impl_serves_one_shot_with_no_chunks() {
        let tracker = Arc::new(SubprocessTracker::new());
        let executor = SubprocessJobExecutor::new(tracker, Duration::from_secs(1));
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let outcome = executor
            .execute_streaming(
                &job("echo one-shot", 5_000),
                Duration::from_millis(2_000),
                tx,
            )
            .await
            .expect("a streaming ask still runs one-shot");
        assert_eq!(outcome.output, vec![Content::text("one-shot\n")]);
        assert!(
            rx.recv().await.is_none(),
            "the default impl must not emit chunks"
        );
    }

    #[tokio::test]
    async fn subprocess_executor_runs_a_real_command_and_captures_stdout() {
        let tracker = Arc::new(SubprocessTracker::new());
        let executor = SubprocessJobExecutor::new(tracker.clone(), Duration::from_secs(1));
        let j = job("echo hello-compute", 5_000);
        let outcome = executor
            .execute(&j, Duration::from_millis(2_000))
            .await
            .expect("command should complete");
        assert_eq!(outcome.output, vec![Content::text("hello-compute\n")]);
        assert!(tracker.is_empty(), "tracker entry must be cleaned up");
    }

    #[tokio::test]
    async fn subprocess_executor_hard_preempts_a_job_past_its_deadline() {
        let tracker = Arc::new(SubprocessTracker::new());
        let executor = SubprocessJobExecutor::new(tracker.clone(), Duration::from_millis(200));
        let j = job("sleep 30", 30_000);
        let result = executor.execute(&j, Duration::from_millis(100)).await;
        assert!(matches!(result, Err(ExecutorError::Timeout(_))));
        assert!(
            tracker.is_empty(),
            "tracker entry must be cleaned up even on hard preempt"
        );
    }

    #[tokio::test]
    async fn a_nonzero_exit_is_a_failed_job_not_an_empty_ok_result() {
        let tracker = Arc::new(SubprocessTracker::new());
        let executor = SubprocessJobExecutor::new(tracker, Duration::from_secs(1));
        let j = job("echo boom >&2; exit 3", 5_000);
        let err = executor
            .execute(&j, Duration::from_millis(2_000))
            .await
            .expect_err("a failing command must not produce a payable outcome");
        let msg = err.to_string();
        assert!(msg.contains('3'), "exit code surfaces: {msg}");
        assert!(msg.contains("boom"), "stderr tail surfaces: {msg}");
    }

    #[tokio::test]
    async fn the_job_environment_is_scrubbed_and_the_cwd_is_scratch() {
        let tracker = Arc::new(SubprocessTracker::new());
        let executor = SubprocessJobExecutor::new(tracker, Duration::from_secs(1));

        // Whatever the node process carries — operator HOME, COVENANT_*
        // config, cloud credentials — none of it may reach the child:
        // only the variables the executor sets (plus what sh itself
        // adds) exist.
        let outcome = executor
            .execute(&job("env", 5_000), Duration::from_millis(2_000))
            .await
            .unwrap();
        let Content::Text { text: env_out } = &outcome.output[0] else {
            panic!("text output expected");
        };
        let allowed = ["PATH=", "HOME=", "PWD=", "SHLVL=", "_=", "OLDPWD="];
        for line in env_out.lines().filter(|l| !l.is_empty()) {
            assert!(
                allowed.iter().any(|p| line.starts_with(p)),
                "leaked env var into an untrusted job: {line}"
            );
        }

        // cwd (and HOME) are a fresh scratch dir, not the node's own.
        let outcome = executor
            .execute(
                &job("pwd; printf %s \"$HOME\"", 5_000),
                Duration::from_millis(2_000),
            )
            .await
            .unwrap();
        let Content::Text { text } = &outcome.output[0] else {
            panic!("text output expected");
        };
        let node_cwd = std::env::current_dir().unwrap();
        let job_cwd = text.lines().next().unwrap_or_default();
        assert_ne!(job_cwd, node_cwd.to_string_lossy(), "job ran in node cwd");
        assert!(
            text.contains(job_cwd),
            "HOME should point into the scratch dir: {text}"
        );
        // The scratch dir is gone once the job is done.
        assert!(
            !std::path::Path::new(job_cwd).exists(),
            "scratch dir must not outlive the job"
        );
    }

    #[tokio::test]
    async fn output_past_the_ceiling_fails_the_job_and_kills_the_process() {
        let tracker = Arc::new(SubprocessTracker::new());
        let executor = SubprocessJobExecutor::new(tracker.clone(), Duration::from_millis(200));
        // 5 MiB > the 4 MiB ceiling; the writer would happily continue.
        let j = job("head -c 5242880 /dev/zero; sleep 30", 30_000);
        let err = executor
            .execute(&j, Duration::from_secs(10))
            .await
            .expect_err("oversized output must fail, not truncate");
        assert!(err.to_string().contains("ceiling"), "got: {err}");
        assert!(tracker.is_empty());
    }

    #[tokio::test]
    async fn the_default_health_answer_is_ready() {
        EchoExecutor
            .health()
            .await
            .expect("a local backend is always ready");
        let tracker = Arc::new(SubprocessTracker::new());
        SubprocessJobExecutor::new(tracker, Duration::from_secs(1))
            .health()
            .await
            .expect("the shell is not a backend that can go down");
    }

    #[test]
    fn model_presence_folds_latest_and_names_what_is_missing() {
        let live = vec!["qwen2.5-coder:latest".to_string(), "llama3:8b".to_string()];
        require_models_served(&["qwen2.5-coder".into()], &live).expect(":latest folds");
        require_models_served(&["llama3:8b".into()], &live).expect("exact tag matches");
        require_models_served(&[], &live).expect("nothing required, nothing missing");
        let err = require_models_served(&["mistral:7b".into()], &live).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("mistral:7b"), "names the lost model: {msg}");
        assert!(msg.contains("llama3:8b"), "shows what IS live: {msg}");
    }

    #[tokio::test]
    async fn a_stderr_flood_does_not_wedge_the_job() {
        let tracker = Arc::new(SubprocessTracker::new());
        let executor = SubprocessJobExecutor::new(tracker, Duration::from_secs(1));
        // 256 KiB of stderr is far past the OS pipe buffer: without a
        // concurrent drain the child blocks on write and the job used
        // to sit there until the deadline killed it.
        let j = job("head -c 262144 /dev/zero >&2; echo done", 5_000);
        let outcome = executor
            .execute(&j, Duration::from_millis(3_000))
            .await
            .expect("a chatty-stderr job must still complete");
        assert_eq!(outcome.output, vec![Content::text("done\n")]);
    }

    #[tokio::test]
    async fn an_error_body_is_read_bounded_not_to_eof() {
        use axum::routing::get;
        use axum::Router;

        // A backend (or a proxy fronting one) that answers a non-2xx with
        // a body far past the snippet cap. read_error_snippet must return
        // at most the cap, never the whole body — the OOM vector the
        // success paths already close with read_body_capped, closed here
        // for the error path too.
        let oversized = MAX_ERROR_SNIPPET_BYTES * 4;
        let router =
            Router::new().route("/boom", get(move || async move { "x".repeat(oversized) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/boom"))
            .send()
            .await
            .unwrap();
        let snippet = read_error_snippet(resp).await;
        assert_eq!(
            snippet.len(),
            MAX_ERROR_SNIPPET_BYTES,
            "an oversized error body is truncated to the cap, not read whole"
        );
        assert!(snippet.starts_with("xxxx"), "keeps the body's prefix");
    }

    #[tokio::test]
    async fn a_small_error_body_is_returned_whole() {
        use axum::routing::get;
        use axum::Router;

        // The common case: a real error envelope is well under the cap and
        // comes back intact, so error-detail extraction still works.
        let router = Router::new().route(
            "/err",
            get(|| async { r#"{"error":{"message":"model not found"}}"#.to_string() }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/err"))
            .send()
            .await
            .unwrap();
        let snippet = read_error_snippet(resp).await;
        assert_eq!(snippet, r#"{"error":{"message":"model not found"}}"#);
    }
}
