//! The screening gate: turn a natural-language intent into a Bento verdict.
//!
//! Bento's `protect()` lives in their TypeScript SDK, which does MagicBlock
//! encryption and ed25519 request signing. Rather than reimplement that wire
//! protocol in Rust, the daemon spawns a one-shot Node sidecar that runs the
//! real SDK: it pipes a [`ScreenRequest`] as JSON to the sidecar's stdin and
//! reads an [`AnalysisResult`] back from stdout. The agent key never reaches
//! the daemon; the sidecar reads it from a file path the daemon forwards (the
//! same path-not-value posture as the x402 signer sidecar).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::config::{BentoConfig, DEFAULT_GUARD_TIMEOUT_MS};
use crate::types::{AnalysisResult, GateOutcome, Verdict};
use crate::{BentoError, Result};

/// Bento's block line on the 0-100 scale, from the live relayer config
/// (block_threshold 70000 of 100000); a blocked action is what earns a strike.
/// Informational here: the gate honors the verdict, it does not re-derive it
/// from the score.
pub const STRIKE_RISK_THRESHOLD: u8 = 70;

/// Margin added on top of the inner timeout before the Rust side hard-kills a
/// wedged sidecar, so the sidecar's own deadline fires first under normal slow
/// responses and the kill is only a backstop.
const KILL_MARGIN: Duration = Duration::from_secs(3);

/// What to screen. `intent` is a natural-language description of the action,
/// which is what `protect()` expects, not a raw transaction.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenRequest {
    pub intent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl ScreenRequest {
    pub fn new(intent: impl Into<String>) -> Self {
        Self {
            intent: intent.into(),
            agent_address: None,
            timeout_ms: None,
        }
    }
}

#[async_trait]
pub trait Guard: Send + Sync {
    async fn screen(&self, req: &ScreenRequest) -> Result<AnalysisResult>;
}

/// Runs the guard sidecar as a subprocess: one spawn per screen, request on
/// stdin, verdict on stdout, bounded by a hard deadline.
pub struct SubprocessGuard {
    program: PathBuf,
    args: Vec<String>,
    env: Vec<(String, String)>,
    timeout: Duration,
}

impl SubprocessGuard {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            timeout: Duration::from_millis(DEFAULT_GUARD_TIMEOUT_MS),
        }
    }

    /// Build from config, or `None` when no guard binary is set. Carries the
    /// sidecar's path-only environment and the screen deadline.
    pub fn from_config(cfg: &BentoConfig) -> Option<Self> {
        let program = cfg.guard_binary.as_ref()?;
        let mut g = Self::new(program).timeout(Duration::from_millis(cfg.guard_timeout_ms));
        for (k, v) in &cfg.guard_env {
            g = g.env(k, v);
        }
        Some(g)
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Set an environment variable for the spawned sidecar. The daemon's own
    /// environment is not inherited beyond this; only `PATH` and a keypair
    /// file path are forwarded, never the key.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

#[async_trait]
impl Guard for SubprocessGuard {
    async fn screen(&self, req: &ScreenRequest) -> Result<AnalysisResult> {
        // Give the sidecar an inner deadline too, so the SDK bounds itself.
        let mut req = req.clone();
        req.timeout_ms
            .get_or_insert(self.timeout.as_millis() as u64);
        let payload = serde_json::to_vec(&req)
            .map_err(|e| BentoError::Sidecar(format!("encode request: {e}")))?;

        let mut child = Command::new(&self.program)
            .args(&self.args)
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| BentoError::Sidecar(format!("spawn {:?}: {e}", self.program)))?;

        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| BentoError::Sidecar("sidecar stdin unavailable".into()))?;
            stdin
                .write_all(&payload)
                .await
                .map_err(|e| BentoError::Sidecar(format!("write to sidecar: {e}")))?;
            // Drop closes stdin so the one-shot sidecar sees EOF.
        }

        // Hard deadline. On elapse the wait future is dropped, and kill_on_drop
        // reaps the child, so a hung sidecar fails closed instead of stalling
        // the call (and never lingers with the agent key in its environment).
        let deadline = self.timeout + KILL_MARGIN;
        let output = match tokio::time::timeout(deadline, child.wait_with_output()).await {
            Ok(res) => res.map_err(|e| BentoError::Sidecar(format!("await sidecar: {e}")))?,
            Err(_) => return Err(BentoError::Sidecar(format!("timed out after {deadline:?}"))),
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(BentoError::Sidecar(format!(
                "exited {}: {}",
                output.status,
                stderr.trim()
            )));
        }

        serde_json::from_slice(&output.stdout)
            .map_err(|e| BentoError::Sidecar(format!("decode verdict: {e}")))
    }
}

/// Map a verdict to a caller action. Pure. Covenant honors Bento's call; it
/// does not block an ALLOW for a high score or vice versa.
pub fn gate_decision(result: &AnalysisResult) -> GateOutcome {
    match result.recommendation {
        Verdict::Allow => GateOutcome::Proceed,
        Verdict::Blocked => GateOutcome::Block,
        Verdict::Escalated => GateOutcome::Escalate,
    }
}

/// Whether this verdict's score is in strike territory. Informational: a
/// caller may log it, but the gate decision comes from the verdict.
pub fn is_strike(result: &AnalysisResult) -> bool {
    result.risk_score > STRIKE_RISK_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(recommendation: Verdict, risk_score: u8) -> AnalysisResult {
        AnalysisResult {
            recommendation,
            risk_score,
            reasoning: String::new(),
            action_id: None,
            approve_url: None,
            block_url: None,
            review_url: None,
            timestamp: None,
        }
    }

    #[test]
    fn gate_maps_each_verdict() {
        assert_eq!(
            gate_decision(&result(Verdict::Allow, 5)),
            GateOutcome::Proceed
        );
        assert_eq!(
            gate_decision(&result(Verdict::Blocked, 95)),
            GateOutcome::Block
        );
        assert_eq!(
            gate_decision(&result(Verdict::Escalated, 60)),
            GateOutcome::Escalate
        );
    }

    #[test]
    fn gate_honors_verdict_over_score() {
        // ALLOW wins even at a strike-level score.
        assert_eq!(
            gate_decision(&result(Verdict::Allow, 90)),
            GateOutcome::Proceed
        );
        assert!(is_strike(&result(Verdict::Allow, 90)));
        assert!(!is_strike(&result(Verdict::Blocked, 70)));
    }

    /// Exercises the real spawn/stdin/stdout/decode path against a shell stub:
    /// it drains the request on stdin (sh builtins only, no PATH) and prints a
    /// fixed verdict. Proves the sidecar contract without Bento's SDK.
    #[tokio::test]
    async fn subprocess_roundtrips_request_and_verdict() {
        let script = r#"while read _; do :; done; printf '%s' '{"recommendation":"ESCALATED","riskScore":84,"reasoning":"large transfer to a fresh wallet"}'"#;
        let guard = SubprocessGuard::new("/bin/sh").arg("-c").arg(script);
        let out = guard
            .screen(&ScreenRequest::new("send 5 SOL to an unknown address"))
            .await
            .unwrap();
        assert_eq!(out.recommendation, Verdict::Escalated);
        assert_eq!(gate_decision(&out), GateOutcome::Escalate);
        assert!(is_strike(&out));
    }

    #[tokio::test]
    async fn nonzero_exit_is_a_sidecar_error() {
        let guard = SubprocessGuard::new("/bin/sh").arg("-c").arg("exit 3");
        let err = guard.screen(&ScreenRequest::new("x")).await.unwrap_err();
        assert!(matches!(err, BentoError::Sidecar(_)));
    }

    #[tokio::test]
    async fn hung_sidecar_times_out_and_is_killed() {
        // A sidecar that never exits must not hang the screen: a short deadline
        // returns a Sidecar error (which fails closed) and kill_on_drop reaps it.
        let guard = SubprocessGuard::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30")
            .timeout(Duration::from_millis(150));
        let start = std::time::Instant::now();
        let err = guard.screen(&ScreenRequest::new("x")).await.unwrap_err();
        assert!(matches!(err, BentoError::Sidecar(_)));
        assert!(start.elapsed() < Duration::from_secs(5), "must not block");
    }
}
