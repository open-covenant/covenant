//! Real text-to-speech executor: synthesis over a local `say`-style CLI
//! (design-01 §3's "real model backend" seam, in the shape audio output
//! needs, and the mirror of [`crate::whisper`]). Where [`crate::ollama`]
//! and [`crate::openai_compat`] speak HTTP to a resident model server, a
//! TTS engine here is a local command-line tool, so this backend runs it
//! the way [`crate::executor::SubprocessJobExecutor`] runs a job: the text
//! is written to a scratch file, the synthesizer is spawned in its own
//! process group, tracked in the node's [`SubprocessTracker`], and
//! hard-preempted via [`preempt_subprocess_pg`] if it outlives the job
//! deadline.
//!
//! The default backend is macOS's `say`, which needs no model download —
//! the reference synthesizer this crate proves the path against. A Linux
//! operator points [`SayExecutor::new`] at a `say`-compatible wrapper
//! (piper, espeak-ng behind a thin shim) with the same `-v`/`-o`/`-f`
//! contract, the same "operator brings a backend" posture whisper.cpp
//! takes for speech-to-text.
//!
//! Trust shape: the buyer supplies text *data* and, at most, a voice name,
//! never a command. The text is read from a file, never placed on the
//! command line; the voice is validated to a bare token before it reaches
//! the synthesizer as the argument to `-v`. That is a strictly smaller
//! untrusted surface than the batch subprocess backend, which runs a
//! stranger's shell command outright.
//!
//! Metering is by envelope: the job settles at the price the buyer's
//! signed envelope held, the same charge posture every backend takes, so
//! the clip carries no token meter.

use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine as _;
use covenant_compute_protocol::{
    parse_speech_input, speech_output, JobEnvelopePayload, JobKind, SpeechInput,
    MAX_AUDIO_B64_BYTES,
};
use covenant_runtime::{preempt_subprocess_pg, SubprocessTracker, TrackedSubprocess};
use tokio::process::Command;

use crate::executor::{configure_process_group, ExecutionOutcome, ExecutorError, JobExecutor};

/// The synthesizer a node runs when the operator names no path. macOS ships
/// `say` at this name; a Linux operator sets an absolute path to a
/// compatible wrapper.
pub const DEFAULT_SAY_BIN: &str = "say";

/// `say`'s natural speaking rate in words per minute, the baseline a
/// requested [`SpeechInput::speed`] multiplier scales. Its real default
/// drifts a little by voice; this is the documented ~175 wpm the rate flag
/// is measured against, so `speed: 1.0` leaves the voice at its own pace
/// and only an explicit multiplier moves it.
const SAY_BASE_WPM: f32 = 175.0;

pub struct SayExecutor {
    /// The `say` binary — a bare name resolved on `PATH`, or an absolute
    /// path to a compatible synthesizer.
    binary: String,
    /// The model name this node advertises and stamps on the clip,
    /// decoupled from the OS tool so a buyer asks for a stable id
    /// (`say-1`) rather than naming the local binary.
    model_name: String,
    /// The voice used when a request names none. `None` lets the
    /// synthesizer pick its own system default.
    default_voice: Option<String>,
    tracker: Arc<SubprocessTracker>,
    preempt_grace: Duration,
}

impl SayExecutor {
    pub fn new(
        binary: impl Into<String>,
        model_name: impl Into<String>,
        default_voice: Option<String>,
        tracker: Arc<SubprocessTracker>,
        preempt_grace: Duration,
    ) -> Self {
        Self {
            binary: binary.into(),
            model_name: model_name.into(),
            default_voice,
            tracker,
            preempt_grace,
        }
    }
}

/// The container a synthesis request asks for, resolved to how `say` is
/// told to write it. Only the forms `say` produces natively are offered;
/// a request for anything else fails the job rather than hand back a
/// different container under the buyer's chosen name.
#[derive(Debug)]
struct AudioFormat {
    /// `say`'s `--file-format` value.
    file_format: &'static str,
    /// The scratch file extension `say` keys the container off.
    extension: &'static str,
    /// The label stamped on the result and read by the front door.
    label: &'static str,
    /// The forced sample rate, when this backend pins one.
    sample_rate_hz: Option<u32>,
}

/// Resolves a requested container (or the WAV default) to how `say` writes
/// it. WAV is pinned to 16-bit mono at a known rate — the form every client
/// and the whisper backend decode; AIFF is offered at the voice's native
/// rate. Any other container fails loudly.
fn resolve_format(requested: Option<&str>) -> Result<AudioFormat, ExecutorError> {
    match requested.map(|f| f.trim().to_ascii_lowercase()).as_deref() {
        None | Some("") | Some("wav") | Some("wave") => Ok(AudioFormat {
            file_format: "WAVE",
            extension: "wav",
            label: "wav",
            sample_rate_hz: Some(22_050),
        }),
        Some("aiff") | Some("aif") => Ok(AudioFormat {
            file_format: "AIFF",
            extension: "aiff",
            label: "aiff",
            sample_rate_hz: None,
        }),
        Some(other) => Err(ExecutorError::Failed(format!(
            "this speech backend produces wav or aiff, not {other:?}"
        ))),
    }
}

/// A voice name reaches `say` as the argument to `-v`, so like the whisper
/// backend's language field it is validated to a bare token first: a value
/// like `-o` or a path must never be handed to the synthesizer as
/// something that changes what it does. Real voice names are single words,
/// sometimes hyphenated (`Ting-Ting`); anything with a control character, a
/// leading dash, or past a sane length is refused and the job fails on the
/// buyer's input rather than mis-executing.
fn validate_voice(voice: &str) -> Result<(), ExecutorError> {
    let mut chars = voice.chars();
    let ok = voice.len() <= 64
        && chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && voice
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(ExecutorError::Failed(format!(
            "voice {voice:?} is not a bare voice name"
        )))
    }
}

/// Turns a requested speed multiple into `say`'s words-per-minute rate.
/// `None` leaves the voice at its own pace; a multiplier scales
/// [`SAY_BASE_WPM`] and is floored at 1 so it is always a positive integer
/// the flag accepts. The buyer's multiplier is already range-checked at
/// [`parse_speech_input`], so this only converts.
fn rate_wpm(speed: Option<f32>) -> Option<u32> {
    speed.map(|s| ((SAY_BASE_WPM * s).round() as u32).max(1))
}

/// Refuses a clip that came back empty. Real synthesis writes audio; an
/// empty file is a voice that produced nothing or a backend that failed
/// silently — not an answer the buyer can play, so failing the job refunds
/// them rather than settling an `Ok` receipt over zero bytes.
fn ensure_synthesized(audio: &[u8]) -> Result<(), ExecutorError> {
    if audio.is_empty() {
        return Err(ExecutorError::Failed(
            "the synthesizer produced no audio: an unknown voice, or a backend that wrote nothing"
                .into(),
        ));
    }
    Ok(())
}

/// Refuses a clip too large to ride back inline. The output travels in one
/// unsplit result bounded by the shared IPC/HTTP frame cap
/// (`covenant-ipc`'s `MAX_FRAME`), so its base64 audio is held to the same
/// inline-audio ceiling the network uses the other direction for a
/// transcription's input ([`MAX_AUDIO_B64_BYTES`]). Uncompressed WAV is
/// dense — a few minutes of speech clears the cap — so a long enough text
/// at a slow enough speed lands here, and the job fails with a reason the
/// buyer can act on rather than the result 413-ing at the coordinator
/// after the work is already done. Longer copy belongs split across
/// requests, or a content-addressed blob path (an open seam).
fn ensure_within_inline_cap(base64_len: usize) -> Result<(), ExecutorError> {
    if base64_len > MAX_AUDIO_B64_BYTES {
        return Err(ExecutorError::Failed(format!(
            "the synthesized clip is {base64_len} base64 bytes, over the {MAX_AUDIO_B64_BYTES} \
             inline cap: send shorter text or a faster speed"
        )));
    }
    Ok(())
}

/// Memory ceiling on the synthesized-clip read, the raw-byte projection of the
/// base64 inline cap: a clip rides back as base64 bounded by
/// [`MAX_AUDIO_B64_BYTES`], so its raw bytes can never exceed three-quarters of
/// that and still fit the wire. A file past this projection could not ride back
/// anyway, and uncompressed audio is dense — a long text at a slow speed is tens
/// of MB — so [`read_clip`] fails the job on this size check before any bytes
/// reach node memory, rather than reading a runaway clip in only to reject its
/// base64 afterwards.
const MAX_AUDIO_FILE_BYTES: u64 = (MAX_AUDIO_B64_BYTES / 4 * 3) as u64;

/// Reads the synthesized clip, bounding the read the way the plain
/// transcription path bounds its stdout: a file over [`MAX_AUDIO_FILE_BYTES`]
/// fails on a size check before it is read. Runs off the async runtime, the
/// same blocking-offload posture the text write takes.
async fn read_clip(path: std::path::PathBuf) -> Result<Vec<u8>, ExecutorError> {
    tokio::task::spawn_blocking(move || {
        let len = std::fs::metadata(&path)
            .map_err(|e| {
                ExecutorError::Failed(format!(
                    "the synthesizer exited cleanly but wrote no clip: {e}"
                ))
            })?
            .len();
        if len > MAX_AUDIO_FILE_BYTES {
            return Err(ExecutorError::Failed(format!(
                "the synthesized clip is {len} bytes, over the {MAX_AUDIO_FILE_BYTES}-byte \
                 ceiling: send shorter text or a faster speed"
            )));
        }
        std::fs::read(&path).map_err(|e| ExecutorError::Failed(format!("clip read: {e}")))
    })
    .await
    .map_err(|e| ExecutorError::Failed(format!("clip read: {e}")))?
}

#[async_trait]
impl JobExecutor for SayExecutor {
    async fn execute(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        if job.kind != JobKind::SpeechSynthesis {
            return Err(ExecutorError::Failed(format!(
                "the say executor serves speech-synthesis jobs only, not {:?}",
                job.kind
            )));
        }
        let request: SpeechInput =
            parse_speech_input(&job.input).map_err(|e| ExecutorError::Failed(e.to_string()))?;

        let voice = request.voice.clone().or_else(|| self.default_voice.clone());
        if let Some(voice) = &voice {
            validate_voice(voice)?;
        }
        let format = resolve_format(request.format.as_deref())?;
        let rate = rate_wpm(request.speed);

        let text = request.text.clone();
        let extension = format.extension;
        let (dir, text_path, out_path) = tokio::task::spawn_blocking(move || {
            let dir = tempfile::Builder::new().prefix("compute-tts-").tempdir()?;
            let text_path = dir.path().join("input.txt");
            std::fs::write(&text_path, text.as_bytes())?;
            let out_path = dir.path().join(format!("out.{extension}"));
            Ok::<_, std::io::Error>((dir, text_path, out_path))
        })
        .await
        .map_err(|e| ExecutorError::Failed(format!("scratch write: {e}")))?
        .map_err(|e| ExecutorError::Failed(format!("scratch text file: {e}")))?;

        let mut cmd = Command::new(&self.binary);
        cmd.arg("-o")
            .arg(&out_path)
            .arg("--file-format")
            .arg(format.file_format);
        // WAV is pinned to a known 16-bit-mono rate so the clip decodes
        // the same everywhere; AIFF rides the voice's native rate.
        if format.sample_rate_hz.is_some() {
            cmd.arg("--data-format").arg("LEI16@22050");
        }
        if let Some(voice) = &voice {
            cmd.arg("-v").arg(voice);
        }
        if let Some(rate) = rate {
            cmd.arg("-r").arg(rate.to_string());
        }
        cmd.arg("-f")
            .arg(&text_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
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

        let outcome = match tokio::time::timeout(deadline, child.wait()).await {
            Ok(Ok(status)) if status.success() => read_clip(out_path).await.and_then(|audio| {
                ensure_synthesized(&audio)?;
                let audio_base64 = base64::engine::general_purpose::STANDARD.encode(&audio);
                ensure_within_inline_cap(audio_base64.len())?;
                Ok(ExecutionOutcome {
                    output: vec![speech_output(
                        self.model_name.clone(),
                        audio_base64,
                        format.label,
                        format.sample_rate_hz,
                    )],
                    wall_ms: started.elapsed().as_millis() as u64,
                    tokens_in: None,
                    tokens_out: None,
                    finish_reason: None,
                })
            }),
            Ok(Ok(status)) => {
                let tail = stderr_task.await.unwrap_or_default();
                Err(ExecutorError::Failed(format!(
                    "{} exited with {status}: {tail}",
                    self.binary
                )))
            }
            Ok(Err(e)) => Err(ExecutorError::Failed(format!("wait: {e}"))),
            Err(_) => {
                if let Some(pid) = pid {
                    preempt_subprocess_pg(pid, self.preempt_grace).await;
                }
                Err(ExecutorError::Timeout(deadline))
            }
        };

        self.tracker.unregister(&job.job_id);
        // Hold the scratch dir until the synthesizer has finished with it.
        drop(dir);
        outcome
    }

    /// `say` is a fixed OS tool with no model file to move under the
    /// operator's feet — the one part the whisper backend re-checks between
    /// jobs. Proven runnable at benchmark-on-register and stable
    /// thereafter, so there is nothing to re-verify here; a mid-serving
    /// disappearance surfaces as a failed job, the same as any backend that
    /// stops responding.
    async fn health(&self) -> Result<(), ExecutorError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
    use covenant_compute_protocol::{parse_speech_output, speech_input, CapabilityRequirement};
    use covenant_types::AgentId;
    use uuid::Uuid;

    fn speech_job(text: &str) -> JobEnvelopePayload {
        JobEnvelopePayload {
            job_id: Uuid::new_v4(),
            buyer: AgentId::new("buyer@local", [1u8; 32]),
            kind: JobKind::SpeechSynthesis,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: Some("say-1".into()),
                kind: JobKind::SpeechSynthesis,
                max_duration_secs: 30,
                min_reputation_bps: None,
            },
            input: speech_input(SpeechInput::new(text)),
            price_micro_usdc: 10,
            deadline_ms: 30_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "say-test"),
            issued_at_ms: 0,
            referral_code: None,
            stream: false,
        }
    }

    fn executor(binary: &str) -> SayExecutor {
        SayExecutor::new(
            binary,
            "say-1",
            None,
            Arc::new(SubprocessTracker::new()),
            Duration::from_secs(2),
        )
    }

    #[tokio::test]
    async fn refuses_a_job_that_is_not_speech() {
        let mut job = speech_job("hello");
        job.kind = JobKind::Embedding;
        let err = executor(DEFAULT_SAY_BIN)
            .execute(&job, Duration::from_secs(5))
            .await
            .expect_err("only speech jobs are served");
        assert!(
            err.to_string().contains("speech-synthesis jobs only"),
            "{err}"
        );
    }

    #[test]
    fn a_voice_must_be_a_bare_name() {
        for good in ["Alex", "Samantha", "Ting-Ting", "en_US", "Voice2"] {
            assert!(validate_voice(good).is_ok(), "{good}");
        }
        // A flag or a path smuggled through the voice field is refused
        // before it reaches the command line.
        for bad in [
            "-o",
            "--file-format",
            "../etc/passwd",
            "a;rm -rf",
            "",
            "a b\nc",
        ] {
            assert!(validate_voice(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_wav_and_aiff_are_offered() {
        assert_eq!(resolve_format(None).unwrap().label, "wav");
        assert_eq!(resolve_format(Some("wav")).unwrap().label, "wav");
        assert_eq!(resolve_format(Some("WAV")).unwrap().label, "wav");
        assert_eq!(resolve_format(Some("aiff")).unwrap().label, "aiff");
        let err = resolve_format(Some("mp3")).unwrap_err().to_string();
        assert!(err.contains("wav or aiff"), "{err}");
    }

    #[test]
    fn speed_scales_the_base_rate() {
        assert_eq!(rate_wpm(None), None);
        assert_eq!(rate_wpm(Some(1.0)), Some(175));
        assert_eq!(rate_wpm(Some(2.0)), Some(350));
        // A very slow multiple still floors at a positive integer.
        assert_eq!(rate_wpm(Some(0.001)), Some(1));
    }

    #[test]
    fn empty_audio_is_refused() {
        assert!(ensure_synthesized(b"RIFF....").is_ok());
        assert!(ensure_synthesized(b"").is_err());
    }

    #[test]
    fn a_clip_past_the_inline_cap_is_refused() {
        // A clip up to the ceiling rides back; one byte past it fails with
        // a reason the buyer can act on, before the result is handed to a
        // coordinator that would reject the oversized body.
        assert!(ensure_within_inline_cap(1_024).is_ok());
        assert!(ensure_within_inline_cap(MAX_AUDIO_B64_BYTES).is_ok());
        let err = ensure_within_inline_cap(MAX_AUDIO_B64_BYTES + 1)
            .unwrap_err()
            .to_string();
        assert!(err.contains("inline cap"), "{err}");
        assert!(err.contains("shorter text"), "{err}");
    }

    #[tokio::test]
    async fn read_clip_bounds_the_synthesized_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.wav");

        // A normal clip reads back its bytes.
        std::fs::write(&path, b"RIFF\0\0\0\0WAVEfmt ").unwrap();
        assert_eq!(
            read_clip(path.clone()).await.unwrap(),
            b"RIFF\0\0\0\0WAVEfmt "
        );

        // A file over the ceiling fails on the size check, before any of its
        // bytes are read. A sparse `set_len` reports the size without
        // allocating it.
        let over = std::fs::File::create(&path).unwrap();
        over.set_len(MAX_AUDIO_FILE_BYTES + 1).unwrap();
        drop(over);
        let err = read_clip(path).await.expect_err("over the ceiling");
        assert!(err.to_string().contains("ceiling"), "{err}");
    }

    #[tokio::test]
    async fn read_clip_reports_a_synthesizer_that_wrote_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let err = read_clip(dir.path().join("absent.wav"))
            .await
            .expect_err("no file");
        assert!(err.to_string().contains("wrote no clip"), "{err}");
    }

    /// The whole executor against a real `say` install: it synthesizes a
    /// clip, and the bytes come back a real, non-empty WAV (a `RIFF`/`WAVE`
    /// container). macOS ships `say`; run with `--ignored`.
    #[tokio::test]
    #[ignore = "needs a local `say` synthesizer (macOS); run with --ignored"]
    async fn synthesizes_a_real_clip_end_to_end() {
        let binary = std::env::var("COVENANT_COMPUTE_TEST_SAY_BIN")
            .unwrap_or_else(|_| DEFAULT_SAY_BIN.into());
        let outcome = executor(&binary)
            .execute(
                &speech_job("covenant compute speaks"),
                Duration::from_secs(30),
            )
            .await
            .expect("the text synthesizes");
        let result = parse_speech_output(&outcome.output).expect("speech output");
        assert_eq!(result.model, "say-1");
        assert_eq!(result.format, "wav");
        let audio = base64::engine::general_purpose::STANDARD
            .decode(result.audio_base64.as_bytes())
            .expect("valid base64 audio");
        assert!(audio.len() > 44, "a real clip is larger than a WAV header");
        assert_eq!(&audio[0..4], b"RIFF", "a WAV clip starts with RIFF");
        assert_eq!(&audio[8..12], b"WAVE", "a WAV clip names the WAVE form");
    }
}
