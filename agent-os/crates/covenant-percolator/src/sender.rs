//! Thin reference sender for keeper transactions.
//!
//! The keeper's decision + governance layer is wire-agnostic; this
//! module turns one or more `Instruction`s into a confirmed Solana
//! transaction. It's intentionally minimal: one trait, one default
//! RPC implementation, one in-memory test double. Operators who
//! want Jito bundles, custom relays, or per-region failover wire
//! their own [`Sender`] impl — the keeper doesn't care which.
//!
//! Idempotency lives upstream: the keeper's atomic
//! `BudgetLedger::try_debit` and the paired settlement receipt
//! guarantee at-most-once *execution* before this sender runs. If
//! a submission fails after the debit, the operator's audit chain
//! shows the budget spend without a chain confirmation — the
//! reconciliation worker compensates by either re-submitting (same
//! receipt id) or refunding (compensating debit) per their policy.
//!
//! Available under `--features solana-rpc`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::RpcSendTransactionConfig;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::instruction::Instruction;
use solana_sdk::signature::{Keypair, Signature, Signer};
use solana_sdk::transaction::Transaction;

#[derive(Debug, thiserror::Error)]
pub enum SenderError {
    #[error("rpc: {0}")]
    Rpc(String),
    #[error("blockhash fetch failed: {0}")]
    Blockhash(String),
    #[error("exhausted {attempts} attempts; last error: {last}")]
    Exhausted { attempts: u32, last: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitOutcome {
    pub signature: Signature,
    /// The slot the RPC reported confirmation at (best-effort —
    /// `send_and_confirm` does not always return it).
    pub slot: Option<u64>,
    /// Number of `submit` attempts the RPC sender made (1 = first try
    /// succeeded; >1 = transient errors were retried).
    pub attempts: u32,
}

#[async_trait]
pub trait Sender: Send + Sync {
    /// Submit an instruction bundle as a single transaction signed
    /// by `signer`, wait for confirmation, return the outcome.
    /// `extra_signers` covers cases like a separate fee payer or
    /// the percolator authority key for `PushAuthMark`.
    async fn submit(
        &self,
        ixs: Vec<Instruction>,
        signer: &Keypair,
        extra_signers: &[&Keypair],
    ) -> Result<SubmitOutcome, SenderError>;
}

/// `RpcSender` — submit via a Solana RPC URL, confirm at a given
/// commitment, retry transient errors with linear backoff. Used by
/// the keeper binary when `RPC_URL` is set; tests use
/// [`RecordingSender`] instead.
pub struct RpcSender {
    rpc: Arc<RpcClient>,
    commitment: CommitmentConfig,
    max_attempts: u32,
    backoff: Duration,
    send_config: RpcSendTransactionConfig,
}

impl RpcSender {
    pub fn new(url: impl Into<String>) -> Self {
        let commitment = CommitmentConfig::confirmed();
        Self {
            rpc: Arc::new(RpcClient::new_with_commitment(url.into(), commitment)),
            commitment,
            max_attempts: 4,
            backoff: Duration::from_millis(800),
            send_config: RpcSendTransactionConfig {
                skip_preflight: false,
                preflight_commitment: Some(commitment.commitment),
                ..Default::default()
            },
        }
    }

    pub fn with_max_attempts(mut self, n: u32) -> Self {
        self.max_attempts = n.max(1);
        self
    }

    pub fn with_backoff(mut self, d: Duration) -> Self {
        self.backoff = d;
        self
    }

    pub fn with_commitment(mut self, c: CommitmentConfig) -> Self {
        self.commitment = c;
        self.send_config.preflight_commitment = Some(c.commitment);
        self
    }

    pub fn with_skip_preflight(mut self, skip: bool) -> Self {
        self.send_config.skip_preflight = skip;
        self
    }
}

/// Linear backoff between attempts: backoff × attempt_number. Chosen
/// over exponential because we cap at `max_attempts` (default 4) — a
/// few retries is enough to ride a blockhash refresh; longer hangs
/// should propagate to the operator.
fn classify_transient(s: &str) -> bool {
    // The errors a routine keeper expects to see during normal Solana
    // operation. Anything else surfaces to the caller without retry.
    s.contains("BlockhashNotFound")
        || s.contains("blockhash not found")
        || s.contains("Node is unhealthy")
        || s.contains("RpcResponseError { code: -32005")
        || s.contains("Connection reset")
        || s.contains("timed out")
        || s.contains("WouldBlock")
}

#[async_trait]
impl Sender for RpcSender {
    async fn submit(
        &self,
        ixs: Vec<Instruction>,
        signer: &Keypair,
        extra_signers: &[&Keypair],
    ) -> Result<SubmitOutcome, SenderError> {
        let payer = signer.pubkey();
        let mut last_err: Option<String> = None;
        for attempt in 1..=self.max_attempts {
            // Blockhash fetch is part of the retry loop — a flaky
            // RPC during blockhash fetch is exactly as transient as
            // a flaky RPC during send_and_confirm, and a one-shot
            // failure here used to kill the whole submission.
            let blockhash = match self.rpc.get_latest_blockhash().await {
                Ok(h) => h,
                Err(e) => {
                    let msg = e.to_string();
                    if !classify_transient(&msg) {
                        return Err(SenderError::Blockhash(msg));
                    }
                    last_err = Some(format!("blockhash: {msg}"));
                    if attempt < self.max_attempts {
                        tokio::time::sleep(self.backoff * attempt).await;
                    }
                    continue;
                }
            };

            let mut signers: Vec<&Keypair> = Vec::with_capacity(1 + extra_signers.len());
            signers.push(signer);
            signers.extend_from_slice(extra_signers);
            let tx =
                Transaction::new_signed_with_payer(&ixs, Some(&payer), &signers, blockhash);

            match self
                .rpc
                .send_and_confirm_transaction_with_spinner_and_config(
                    &tx,
                    self.commitment,
                    self.send_config,
                )
                .await
            {
                Ok(sig) => {
                    return Ok(SubmitOutcome {
                        signature: sig,
                        slot: None,
                        attempts: attempt,
                    });
                }
                Err(e) => {
                    let msg = e.to_string();
                    if !classify_transient(&msg) {
                        return Err(SenderError::Rpc(msg));
                    }
                    last_err = Some(msg);
                    if attempt < self.max_attempts {
                        tokio::time::sleep(self.backoff * attempt).await;
                    }
                }
            }
        }
        Err(SenderError::Exhausted {
            attempts: self.max_attempts,
            last: last_err.unwrap_or_else(|| "no error captured".into()),
        })
    }
}

/// In-memory recording sender for tests. Stores every submitted
/// bundle (cloned). Configurable failure script lets us assert
/// retry behavior in tests without needing a real RPC.
pub struct RecordingSender {
    inner: tokio::sync::Mutex<RecordingInner>,
}

#[derive(Default)]
struct RecordingInner {
    submissions: Vec<Vec<Instruction>>,
    /// Errors to return on the next N attempts, popped front each
    /// time `submit` is called. Empty = succeed.
    failure_script: std::collections::VecDeque<SenderError>,
}

impl Default for RecordingSender {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordingSender {
    pub fn new() -> Self {
        Self {
            inner: tokio::sync::Mutex::new(RecordingInner::default()),
        }
    }

    pub async fn with_failures<I: IntoIterator<Item = SenderError>>(self, errs: I) -> Self {
        {
            let mut g = self.inner.lock().await;
            g.failure_script.extend(errs);
        }
        self
    }

    pub async fn submissions(&self) -> Vec<Vec<Instruction>> {
        self.inner.lock().await.submissions.clone()
    }
}

#[async_trait]
impl Sender for RecordingSender {
    async fn submit(
        &self,
        ixs: Vec<Instruction>,
        signer: &Keypair,
        _extra_signers: &[&Keypair],
    ) -> Result<SubmitOutcome, SenderError> {
        let mut g = self.inner.lock().await;
        if let Some(err) = g.failure_script.pop_front() {
            return Err(err);
        }
        g.submissions.push(ixs);
        Ok(SubmitOutcome {
            signature: dummy_sig(signer),
            slot: Some(0),
            attempts: 1,
        })
    }
}

fn dummy_sig(signer: &Keypair) -> Signature {
    // Deterministic placeholder. Real signing happens inside
    // RpcSender; the recording sender only needs a non-default
    // value so tests can distinguish submissions.
    let bytes = signer.pubkey().to_bytes();
    let mut sig = [0u8; 64];
    sig[..32].copy_from_slice(&bytes);
    sig[32..].copy_from_slice(&bytes);
    Signature::from(sig)
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::instruction::AccountMeta;
    use solana_sdk::pubkey::Pubkey;

    fn ix() -> Instruction {
        Instruction {
            program_id: Pubkey::new_unique(),
            accounts: vec![AccountMeta::new(Pubkey::new_unique(), false)],
            data: vec![1, 2, 3],
        }
    }

    #[tokio::test]
    async fn recording_sender_logs_each_submission() {
        let s = RecordingSender::new();
        let kp = Keypair::new();
        let r1 = s.submit(vec![ix()], &kp, &[]).await.unwrap();
        let r2 = s.submit(vec![ix(), ix()], &kp, &[]).await.unwrap();
        assert_eq!(r1.attempts, 1);
        assert_eq!(r2.attempts, 1);
        let subs = s.submissions().await;
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[1].len(), 2);
    }

    #[tokio::test]
    async fn recording_sender_returns_scripted_failure_then_succeeds() {
        let s = RecordingSender::new()
            .with_failures([SenderError::Rpc("transient".into())])
            .await;
        let kp = Keypair::new();
        let err = s.submit(vec![ix()], &kp, &[]).await.unwrap_err();
        assert!(matches!(err, SenderError::Rpc(_)));
        let ok = s.submit(vec![ix()], &kp, &[]).await.unwrap();
        assert_eq!(ok.attempts, 1);
        assert_eq!(s.submissions().await.len(), 1);
    }

    #[test]
    fn classify_transient_covers_common_solana_errors() {
        assert!(classify_transient("RpcError: BlockhashNotFound"));
        assert!(classify_transient("blockhash not found in slot"));
        assert!(classify_transient("Node is unhealthy"));
        assert!(classify_transient("connection timed out"));
        assert!(!classify_transient("AccountInUse"));
        assert!(!classify_transient("InsufficientFundsForFee"));
    }
}
