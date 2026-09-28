//! Real speech-to-text executor: transcription over a local whisper.cpp
//! install (design-01 §3's "real model backend" seam, in the shape audio
//! needs). Where [`crate::ollama`] and [`crate::openai_compat`] speak HTTP
//! to a resident model server, whisper.cpp is a local command-line tool,
//! so this backend runs it the way [`crate::executor::SubprocessJobExecutor`]
//! runs a job: the clip is written to a scratch file, `whisper-cli` is
//! spawned in its own process group, tracked in the node's
//! [`SubprocessTracker`], and hard-preempted via [`preempt_subprocess_pg`]
//! if it outlives the job deadline.
//!
//! Trust shape: the buyer supplies audio *data*, never a command. The
//! fixed `whisper-cli` invocation is the node's own; the only buyer-
//! controlled argument is the language code, which is validated to a bare
//! token before it reaches the command line. That is a strictly smaller
//! untrusted surface than the batch subprocess backend, which runs a
//! stranger's shell command outright.
//!
//! Metering is by envelope: the job settles at the price the buyer's
//! signed envelope held, the same charge posture every backend takes, so
//! the transcript carries no token meter.

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine as _;
use covenant_compute_protocol::{
    parse_transcription_input, transcription_output, JobEnvelopePayload, JobKind,
    TranscriptionSegment,
};
use covenant_runtime::{preempt_subprocess_pg, SubprocessTracker, TrackedSubprocess};
use tokio::process::Command;

use crate::executor::{
    configure_process_group, read_stdout_capped, ExecutionOutcome, ExecutorError, JobExecutor,
};

/// The whisper.cpp CLI a node runs when the operator names no path. On
/// `PATH` after `brew install whisper-cpp` or a source build's `make
/// install`.
pub const DEFAULT_WHISPER_BIN: &str = "whisper-cli";

pub struct WhisperExecutor {
    /// The `whisper-cli` binary — a bare name resolved on `PATH`, or an
    /// absolute path.
    binary: String,
    /// The ggml model file (`ggml-base.en.bin`) the CLI loads per job.
    model_path: String,
    /// The model name this node advertises and stamps on the transcript,
    /// decoupled from the on-disk file so a buyer asks for a stable id
    /// (`whisper-1`) rather than a local path.
    model_name: String,
    tracker: Arc<SubprocessTracker>,
    preempt_grace: Duration,
}

impl WhisperExecutor {
    pub fn new(
        binary: impl Into<String>,
        model_path: impl Into<String>,
        model_name: impl Into<String>,
        tracker: Arc<SubprocessTracker>,
        preempt_grace: Duration,
    ) -> Self {
        Self {
            binary: binary.into(),
            model_path: model_path.into(),
            model_name: model_name.into(),
            tracker,
            preempt_grace,
        }
    }
}

/// Refuses a transcript that came back empty or whitespace-only. Real
/// speech transcribes to words; an empty result is silence, a clip the
/// backend could not decode, or a model that produced nothing — none of
/// which is an answer the buyer can use, so failing the job refunds them
/// rather than settling an `Ok` receipt over "". The same fail-closed
/// posture [`crate::executor::ensure_answered`] takes toward an empty
/// completion.
fn ensure_transcribed(text: &str) -> Result<(), ExecutorError> {
    if text.trim().is_empty() {
        return Err(ExecutorError::Failed(
            "whisper returned an empty transcript: silence, an undecodable clip, or no speech"
                .into(),
        ));
    }
    Ok(())
}

/// A spoken-language argument must be a bare token — the ISO codes
/// whisper accepts (`en`, `de`), or `auto` — before it reaches the
/// command line. The buyer controls this field, so a value like
/// `--output-file` or a path must never be handed to `whisper-cli` as an
/// argument that changes what it does. Fail the job on anything else.
fn validate_language(language: &str) -> Result<(), ExecutorError> {
    let ok = !language.is_empty()
        && language.len() <= 16
        && language.chars().all(|c| c.is_ascii_alphabetic());
    if ok {
        Ok(())
    } else {
        Err(ExecutorError::Failed(format!(
            "transcription language {language:?} is not a bare language code"
        )))
    }
}

/// Collapses whisper's segment-broken output into one clean line: the CLI
/// prints each segment on its own line with `-nt`, and a transcript reads
/// as a single run of text, so runs of whitespace (the segment breaks
/// included) fold to single spaces and the ends are trimmed.
fn normalize_transcript(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Memory ceiling on the `-oj` sidecar read. The plain path caps the
/// transcript at `read_stdout_capped`'s 4 MiB; the sidecar carries that same
/// text plus per-segment timing metadata, so this sits well above it — a
/// transcript the plain path would accept is never refused here for its
/// timings — while a runaway file (a long clip's transcript) fails the job
/// instead of reading unbounded, buyer-controlled bytes into node memory.
const MAX_SEGMENTS_JSON_BYTES: u64 = 32 * 1024 * 1024;

/// Reads whisper.cpp's `-oj` sidecar into the transcript and its timed
/// segments. Usually small, but its size follows the clip's length, so the
/// read is bounded like the plain path's stdout — off the async runtime, the
/// same blocking-offload posture the clip write takes.
async fn read_segments(
    path: std::path::PathBuf,
) -> Result<(String, Option<Vec<TranscriptionSegment>>, Option<String>), ExecutorError> {
    let json = tokio::task::spawn_blocking(move || {
        let len = std::fs::metadata(&path)
            .map_err(|e| ExecutorError::Failed(format!("whisper wrote no segments file: {e}")))?
            .len();
        if len > MAX_SEGMENTS_JSON_BYTES {
            return Err(ExecutorError::Failed(format!(
                "whisper segments file is {len} bytes, over the \
                 {MAX_SEGMENTS_JSON_BYTES}-byte ceiling"
            )));
        }
        std::fs::read_to_string(&path)
            .map_err(|e| ExecutorError::Failed(format!("whisper wrote no segments file: {e}")))
    })
    .await
    .map_err(|e| ExecutorError::Failed(format!("segments read: {e}")))??;
    let (transcript, segments, language) = parse_whisper_json(&json)?;
    Ok((transcript, Some(segments), language))
}

/// Parses the fields this backend needs out of whisper.cpp's JSON: each
/// `transcription[]` entry's millisecond `offsets` and `text`, and the
/// `result.language` whisper resolved for the clip. The transcript is
/// those texts joined and cleaned, so a timestamped job and a plain one
/// agree on the words. A segment whose `to` precedes its `from` — never
/// seen from whisper, but the [`TranscriptionSegment`] contract must not
/// promise timings it can't hold — is clamped so `end_ms >= start_ms`.
/// The language is `None` when the sidecar names none.
fn parse_whisper_json(
    json: &str,
) -> Result<(String, Vec<TranscriptionSegment>, Option<String>), ExecutorError> {
    #[derive(serde::Deserialize)]
    struct Doc {
        transcription: Vec<Seg>,
        #[serde(default)]
        result: Option<ResultMeta>,
    }
    #[derive(serde::Deserialize)]
    struct ResultMeta {
        #[serde(default)]
        language: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Seg {
        offsets: Offsets,
        text: String,
    }
    #[derive(serde::Deserialize)]
    struct Offsets {
        from: u64,
        to: u64,
    }
    let doc: Doc = serde_json::from_str(json)
        .map_err(|e| ExecutorError::Failed(format!("whisper json: {e}")))?;
    let language = doc
        .result
        .and_then(|r| r.language)
        .filter(|l| !l.is_empty());
    let segments: Vec<TranscriptionSegment> = doc
        .transcription
        .into_iter()
        .map(|s| TranscriptionSegment {
            start_ms: s.offsets.from,
            end_ms: s.offsets.to.max(s.offsets.from),
            text: normalize_transcript(&s.text),
        })
        .collect();
    let transcript = normalize_transcript(
        &segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
    );
    Ok((transcript, segments, language))
}

#[async_trait]
impl JobExecutor for WhisperExecutor {
    async fn execute(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        if job.kind != JobKind::Transcription {
            return Err(ExecutorError::Failed(format!(
                "the whisper executor serves transcription jobs only, not {:?}",
                job.kind
            )));
        }
        let request = parse_transcription_input(&job.input)
            .map_err(|e| ExecutorError::Failed(e.to_string()))?;
        if let Some(language) = &request.language {
            validate_language(language)?;
        }
        let audio = base64::engine::general_purpose::STANDARD
            .decode(request.audio_base64.as_bytes())
            .map_err(|e| ExecutorError::Failed(format!("audio is not valid base64: {e}")))?;

        let suffix = request
            .format
            .as_deref()
            .filter(|f| f.chars().all(|c| c.is_ascii_alphanumeric()) && !f.is_empty())
            .map(|f| format!(".{f}"))
            .unwrap_or_else(|| ".wav".into());
        let want_segments = request.timestamps;
        let (clip, json_dir) = tokio::task::spawn_blocking(move || {
            let file = tempfile::Builder::new()
                .prefix("compute-audio-")
                .suffix(&suffix)
                .tempfile()?;
            std::io::Write::write_all(&mut file.as_file(), &audio)?;
            // Timestamps come back through whisper's `-oj` sidecar, written
            // into its own scratch dir so it can't collide with the clip.
            let dir = if want_segments {
                Some(tempfile::tempdir()?)
            } else {
                None
            };
            Ok::<_, std::io::Error>((file, dir))
        })
        .await
        .map_err(|e| ExecutorError::Failed(format!("scratch write: {e}")))?
        .map_err(|e| ExecutorError::Failed(format!("scratch audio file: {e}")))?;
        let json_base = json_dir.as_ref().map(|d| d.path().join("segments"));

        let mut cmd = Command::new(&self.binary);
        cmd.arg("-m")
            .arg(&self.model_path)
            .arg("-f")
            .arg(clip.path())
            .arg("-np")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        match &json_base {
            // `-oj` writes the timed segments to `<base>.json`; the transcript
            // is read back from there, not stdout.
            Some(base) => {
                cmd.arg("-oj").arg("-of").arg(base);
            }
            // No timestamps asked for: whisper prints the bare transcript and
            // this backend reads it straight off stdout, as before.
            None => {
                cmd.arg("-nt");
            }
        }
        if let Some(language) = &request.language {
            cmd.arg("-l").arg(language);
        }
        if request.translate {
            cmd.arg("-tr");
        }
        configure_process_group(&mut cmd);

        let started = Instant::now();
        let mut child = cmd
            .spawn()
            .map_err(|e| ExecutorError::Failed(format!("spawn {}: {e}", self.binary)))?;
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
        let stderr_task = tokio::spawn(crate::executor::drain_stderr_tail(stderr));
        let mut stdout = child.stdout.take().expect("stdout piped");

        let outcome = match tokio::time::timeout(deadline, read_stdout_capped(&mut stdout)).await {
            Ok(Ok(buf)) => match tokio::time::timeout(self.preempt_grace, child.wait()).await {
                Ok(Ok(status)) if status.success() => {
                    let transcript_and_segments = match &json_base {
                        Some(base) => read_segments(base.with_extension("json")).await,
                        None => Ok((
                            normalize_transcript(&String::from_utf8_lossy(&buf)),
                            None,
                            None,
                        )),
                    };
                    transcript_and_segments.and_then(|(transcript, segments, detected_language)| {
                        ensure_transcribed(&transcript).map(|()| ExecutionOutcome {
                            output: vec![transcription_output(
                                self.model_name.clone(),
                                transcript,
                                // The language whisper actually read, from its
                                // `-oj` sidecar. Fall back to the request's only
                                // when the sidecar named none — the no-timestamps
                                // path reads the bare transcript off stdout and
                                // has no sidecar to detect from, so it reports the
                                // forced `-l` code or `None` for auto-detect.
                                detected_language.or_else(|| request.language.clone()),
                                segments,
                            )],
                            wall_ms: started.elapsed().as_millis() as u64,
                            tokens_in: None,
                            tokens_out: None,
                            finish_reason: None,
                        })
                    })
                }
                Ok(Ok(status)) => {
                    let tail = stderr_task.await.unwrap_or_default();
                    Err(ExecutorError::Failed(format!(
                        "whisper-cli exited with {status}: {tail}"
                    )))
                }
                Ok(Err(e)) => Err(ExecutorError::Failed(format!("wait: {e}"))),
                Err(_) => {
                    if let Some(pid) = pid {
                        preempt_subprocess_pg(pid, self.preempt_grace).await;
                    }
                    Err(ExecutorError::Failed(
                        "whisper-cli closed stdout but did not exit; process group preempted"
                            .into(),
                    ))
                }
            },
            Ok(Err(e)) => {
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
        // Hold the scratch clip until the CLI has finished reading it.
        drop(clip);
        outcome
    }

    /// The model file is the one part of this backend that changes under
    /// the operator's feet — a moved or deleted `ggml-*.bin` turns every
    /// matched job into a fault. The serve loop asks between jobs; a
    /// missing model reports the node offline until the file returns,
    /// rather than bleeding one reputation fault per matched job. The
    /// binary is proven at benchmark-on-register and does not vanish
    /// mid-serving, so reachability here is the model file alone.
    async fn health(&self) -> Result<(), ExecutorError> {
        if Path::new(&self.model_path).is_file() {
            Ok(())
        } else {
            Err(ExecutorError::Failed(format!(
                "whisper model {} is not a readable file",
                self.model_path
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
    use covenant_compute_protocol::{
        parse_transcription_output, transcription_input, CapabilityRequirement, TranscriptionInput,
    };
    use covenant_types::AgentId;
    use uuid::Uuid;

    fn transcription_job(audio_base64: String) -> JobEnvelopePayload {
        JobEnvelopePayload {
            job_id: Uuid::new_v4(),
            buyer: AgentId::new("buyer@local", [1u8; 32]),
            kind: JobKind::Transcription,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: Some("whisper-1".into()),
                kind: JobKind::Transcription,
                max_duration_secs: 30,
                min_reputation_bps: None,
            },
            input: transcription_input(TranscriptionInput::new(audio_base64)),
            price_micro_usdc: 10,
            deadline_ms: 30_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "whisper-test"),
            issued_at_ms: 0,
            referral_code: None,
            stream: false,
        }
    }

    #[tokio::test]
    async fn health_fails_when_the_model_file_is_missing() {
        let executor = WhisperExecutor::new(
            DEFAULT_WHISPER_BIN,
            "/nonexistent/ggml-model.bin",
            "whisper-1",
            Arc::new(SubprocessTracker::new()),
            Duration::from_secs(2),
        );
        let err = executor
            .health()
            .await
            .expect_err("missing model is unhealthy");
        assert!(err.to_string().contains("readable file"), "{err}");
    }

    #[tokio::test]
    async fn refuses_a_job_that_is_not_a_transcription() {
        let executor = WhisperExecutor::new(
            DEFAULT_WHISPER_BIN,
            "/nonexistent/ggml-model.bin",
            "whisper-1",
            Arc::new(SubprocessTracker::new()),
            Duration::from_secs(2),
        );
        let mut job = transcription_job("YWJj".into());
        job.kind = JobKind::Embedding;
        let err = executor
            .execute(&job, Duration::from_secs(5))
            .await
            .expect_err("only transcription jobs are served");
        assert!(err.to_string().contains("transcription jobs only"), "{err}");
    }

    #[tokio::test]
    async fn read_segments_bounds_the_sidecar_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("segments.json");

        // A normal sidecar reads back its transcript, timings and language.
        std::fs::write(
            &path,
            r#"{"transcription":[{"offsets":{"from":0,"to":600},"text":"covenant compute"}],"result":{"language":"en"}}"#,
        )
        .unwrap();
        let (transcript, segments, language) = read_segments(path.clone()).await.unwrap();
        assert_eq!(transcript, "covenant compute");
        assert_eq!(segments.unwrap().len(), 1);
        assert_eq!(language.as_deref(), Some("en"));

        // A file over the ceiling fails the job on the size check, before any
        // of its bytes are read into memory. A sparse `set_len` reports the
        // size without allocating it.
        let over = std::fs::File::create(&path).unwrap();
        over.set_len(MAX_SEGMENTS_JSON_BYTES + 1).unwrap();
        drop(over);
        let err = read_segments(path).await.expect_err("over the ceiling");
        assert!(err.to_string().contains("ceiling"), "{err}");
    }

    /// The whole executor against a real whisper.cpp install: the bundled
    /// benchmark clip speaks "covenant compute", so a real transcript of
    /// it must carry those words. Point `COVENANT_COMPUTE_TEST_WHISPER_MODEL`
    /// at a `ggml-*.bin` (and optionally `_BIN` at the CLI) and run with
    /// `--ignored`.
    #[tokio::test]
    #[ignore = "needs whisper-cli and a ggml model; set COVENANT_COMPUTE_TEST_WHISPER_MODEL"]
    async fn transcribes_a_real_clip_end_to_end() {
        let model = std::env::var("COVENANT_COMPUTE_TEST_WHISPER_MODEL")
            .expect("set COVENANT_COMPUTE_TEST_WHISPER_MODEL to a ggml-*.bin path");
        let binary = std::env::var("COVENANT_COMPUTE_TEST_WHISPER_BIN")
            .unwrap_or_else(|_| DEFAULT_WHISPER_BIN.into());
        let audio_base64 = base64::engine::general_purpose::STANDARD
            .encode(include_bytes!("whisper_benchmark.wav"));
        let executor = WhisperExecutor::new(
            binary,
            model,
            "whisper-1",
            Arc::new(SubprocessTracker::new()),
            Duration::from_secs(2),
        );
        let outcome = executor
            .execute(&transcription_job(audio_base64), Duration::from_secs(30))
            .await
            .expect("the clip transcribes");
        let result = parse_transcription_output(&outcome.output).expect("transcription output");
        assert!(
            result
                .transcript
                .to_ascii_lowercase()
                .contains("covenant compute"),
            "expected the spoken words, got {:?}",
            result.transcript
        );
        assert_eq!(result.model, "whisper-1");
    }

    #[test]
    fn empty_transcripts_are_refused() {
        assert!(ensure_transcribed("hello there").is_ok());
        assert!(ensure_transcribed("   \n ").is_err());
        assert!(ensure_transcribed("").is_err());
    }

    #[test]
    fn a_language_code_must_be_a_bare_token() {
        for good in ["en", "de", "auto", "EN"] {
            assert!(validate_language(good).is_ok(), "{good}");
        }
        // A flag or a path smuggled through the language field is refused
        // before it reaches the command line.
        for bad in ["--output-file", "en;rm -rf", "../etc", "e n", ""] {
            assert!(validate_language(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_transcript_folds_to_one_clean_line() {
        assert_eq!(
            normalize_transcript("  And so my fellow\n Americans,   ask not \n"),
            "And so my fellow Americans, ask not"
        );
    }

    #[test]
    fn whisper_json_parses_into_clean_segments() {
        // The exact shape whisper.cpp's `-oj` writes: integer-ms offsets and
        // a leading-space text per segment.
        let json = r#"{
            "result": {"language": "en"},
            "transcription": [
                {"offsets": {"from": 0, "to": 1060}, "text": " Covenant"},
                {"offsets": {"from": 1060, "to": 3200}, "text": " Compute "}
            ]
        }"#;
        let (transcript, segments, language) =
            parse_whisper_json(json).expect("valid whisper json");
        assert_eq!(transcript, "Covenant Compute");
        assert_eq!(
            language.as_deref(),
            Some("en"),
            "the detected language is read from result.language, not echoed from the request"
        );
        assert_eq!(segments.len(), 2);
        assert_eq!((segments[0].start_ms, segments[0].end_ms), (0, 1060));
        assert_eq!(segments[0].text, "Covenant");
        assert_eq!((segments[1].start_ms, segments[1].end_ms), (1060, 3200));
        assert_eq!(segments[1].text, "Compute");
    }

    #[test]
    fn whisper_json_without_a_result_block_reports_no_language() {
        let json = r#"{"transcription":[{"offsets":{"from":0,"to":10},"text":"hi"}]}"#;
        let (_transcript, _segments, language) = parse_whisper_json(json).expect("valid");
        assert_eq!(language, None, "an absent result block names no language");
    }

    #[test]
    fn a_backwards_segment_offset_is_clamped() {
        let json = r#"{"transcription":[{"offsets":{"from":500,"to":100},"text":"x"}]}"#;
        let (_transcript, segments, _language) = parse_whisper_json(json).expect("valid");
        assert!(segments[0].end_ms >= segments[0].start_ms);
        assert_eq!(segments[0].end_ms, 500);
    }

    #[test]
    fn malformed_whisper_json_fails_the_job() {
        assert!(parse_whisper_json("not json").is_err());
        // A JSON object without the `transcription` array is not a whisper
        // result — fail rather than settle an empty transcript.
        assert!(parse_whisper_json(r#"{"result":{}}"#).is_err());
    }

    /// The timestamped path against a real whisper.cpp install: the bundled
    /// clip speaks "covenant compute", so real segments must come back
    /// covering those words and advancing in time. Point
    /// `COVENANT_COMPUTE_TEST_WHISPER_MODEL` at a `ggml-*.bin` and run with
    /// `--ignored`.
    #[tokio::test]
    #[ignore = "needs whisper-cli and a ggml model; set COVENANT_COMPUTE_TEST_WHISPER_MODEL"]
    async fn timestamps_come_back_as_real_segments() {
        let model = std::env::var("COVENANT_COMPUTE_TEST_WHISPER_MODEL")
            .expect("set COVENANT_COMPUTE_TEST_WHISPER_MODEL to a ggml-*.bin path");
        let binary = std::env::var("COVENANT_COMPUTE_TEST_WHISPER_BIN")
            .unwrap_or_else(|_| DEFAULT_WHISPER_BIN.into());
        let audio_base64 = base64::engine::general_purpose::STANDARD
            .encode(include_bytes!("whisper_benchmark.wav"));
        let executor = WhisperExecutor::new(
            binary,
            model,
            "whisper-1",
            Arc::new(SubprocessTracker::new()),
            Duration::from_secs(2),
        );
        let mut input = TranscriptionInput::new(audio_base64);
        input.timestamps = true;
        let mut job = transcription_job(String::new());
        job.input = transcription_input(input);

        let outcome = executor
            .execute(&job, Duration::from_secs(30))
            .await
            .expect("the clip transcribes");
        let result = parse_transcription_output(&outcome.output).expect("transcription output");
        let segments = result.segments.expect("timestamps were requested");
        assert!(
            !segments.is_empty(),
            "a spoken clip has at least one segment"
        );
        assert!(
            segments.iter().all(|s| s.end_ms >= s.start_ms),
            "every segment ends no earlier than it starts: {segments:?}"
        );
        assert!(
            result
                .transcript
                .to_ascii_lowercase()
                .contains("covenant compute"),
            "expected the spoken words, got {:?}",
            result.transcript
        );
    }
}
