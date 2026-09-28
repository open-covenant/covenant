//! Payout push.
//!
//! Once the coordinator verifies a `WorkReceipt` and releases the
//! escrow hold (`crate::escrow::CustodialEscrow::release`), it must
//! actually move funds: a real signed SPL/USDC transfer to the
//! operator's registered `payout_address`
//! (build-notes-phase1-foundation.md §1.6 step 7,
//! `covenant-x402::solana::SolanaSigner`/`build_transfer_transaction`,
//! `covenant-x402/src/solana.rs`, is the real primitive).
//!
//! [`MockPayout`] just records the intended transfer — what the
//! hermetic e2e test exercises, and still the default (see `main.rs`).
//! [`SidecarPayout`] is the real implementation: it shells out to the
//! `covenant-x402-signer` binary in its `payout` mode, the same
//! signing-isolation pattern every other on-chain-signing path in this
//! codebase uses (`covenantd/src/x402.rs`, `compute.rs`,
//! `metaplex.rs`) — this crate never links `covenant-x402` or
//! `solana-sdk` and never holds the funding key; see
//! `build-notes-phase1-payout.md` for the as-built protocol and the
//! guardrails below.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use covenant_compute_protocol::SignedWorkReceipt;
use parking_lot::Mutex;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum PayoutError {
    /// The transfer definitively did not move funds; the sweep may
    /// retry it.
    #[error("{0}")]
    Backend(String),
    /// The backend cannot say whether the transfer landed — the signer
    /// died, timed out confirming, or lost the response after the
    /// transaction may have reached the cluster. A blind retry risks
    /// paying twice; the obligation must be suspended until someone
    /// checks the chain (by memo, or by `tx_signature` when known).
    #[error("transfer outcome unknown: {message}")]
    Unresolved {
        message: String,
        tx_signature: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct PayoutRecord {
    pub job_id: Uuid,
    pub operator_pubkey_b58: String,
    pub payout_address: String,
    pub amount_micro_usdc: u64,
    pub recorded_at_ms: u64,
    /// The on-chain transaction signature, when a backend actually
    /// submitted one. `None` for `MockPayout` (nothing was submitted).
    pub tx_signature: Option<String>,
}

/// The backend's receipt of a transfer that honors a book obligation —
/// a buyer withdrawal today — rather than a work receipt. Same shape as
/// [`PayoutRecord`] minus the job/operator identities the obligation
/// doesn't have; the memo on the transfer names what it honors.
#[derive(Debug, Clone, PartialEq)]
pub struct TransferRecord {
    pub transfer_id: Uuid,
    pub recipient_address: String,
    pub amount_micro_usdc: u64,
    pub recorded_at_ms: u64,
    /// The on-chain transaction signature, when a backend actually
    /// submitted one. `None` for `MockPayout` (nothing was submitted).
    pub tx_signature: Option<String>,
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Coordinator-initiated, pull-based on a verified receipt (never the
/// operator pushing, never the operator holding funds or signing
/// anything on-chain — build-notes-phase1-foundation.md §1.6 step 7 /
/// §5's node-stays-solana-free recommendation).
#[async_trait]
pub trait Payout: Send + Sync {
    async fn pay(
        &self,
        job_id: Uuid,
        operator_pubkey_b58: &str,
        payout_address: &str,
        amount_micro_usdc: u64,
        receipt: &SignedWorkReceipt,
    ) -> Result<PayoutRecord, PayoutError>;

    /// A transfer honoring a book obligation (a buyer withdrawal)
    /// instead of a work receipt: same guardrails and money movement
    /// as [`Payout::pay`], with the caller-derived `memo` naming the
    /// obligation on-chain. Idempotent per `transfer_id` — a completed
    /// transfer returns its cached record instead of paying twice.
    async fn transfer(
        &self,
        transfer_id: Uuid,
        recipient_address: &str,
        amount_micro_usdc: u64,
        memo: &str,
    ) -> Result<TransferRecord, PayoutError>;

    /// The per-transfer ceiling this backend applies to obligation
    /// transfers ([`Payout::transfer`]) — buyer withdrawals and unbond
    /// refunds. `None` means no artificial limit; the amount is still
    /// bounded by the books. The request handlers consult this to refuse
    /// an over-cap request up front, so a debit is never taken for a
    /// push the sweep could only spin on. Distinct from the per-job
    /// payout cap, which never applies to a party's own principal.
    fn obligation_cap_micro_usdc(&self) -> Option<u64> {
        None
    }

    /// The per-job ceiling this backend applies to work-receipt payouts
    /// ([`Payout::pay`]) — the operator's net earnings on one job.
    /// `None` means no ceiling. `submit_job` consults this to refuse an
    /// offer whose net could never clear the cap up front, before any
    /// escrow hold, so the buyer's funds are never held for a job that
    /// would settle mechanically and then strand at the payout push (the
    /// same up-front refusal [`Payout::obligation_cap_micro_usdc`]
    /// already earns a withdrawal). Distinct from the obligation cap,
    /// which bounds a party's own principal, not job earnings.
    fn per_job_cap_micro_usdc(&self) -> Option<u64> {
        None
    }

    /// The SPL mint this backend pays in, base58. `None` when the backend
    /// moves no real token (the mock), so nothing on-chain can be cited.
    /// The public settlement feed pins it onto each proof so a reader can
    /// tell a USDC payout from one in a worthless mint.
    fn payout_mint_b58(&self) -> Option<&str> {
        None
    }
}

/// Records the intended transfer; moves no money. What this slice's
/// gate requires (`cargo tree -p covenant-compute-coordinator | grep
/// -i solana` must stay empty).
#[derive(Default)]
pub struct MockPayout {
    records: Mutex<Vec<PayoutRecord>>,
    transfers: Mutex<Vec<TransferRecord>>,
    obligation_cap: Option<u64>,
    per_job_cap: Option<u64>,
    fail_next_pays: Mutex<u32>,
    /// Set to make the mock behave like a chain-settling backend: it
    /// reports this mint and stamps every payout with a per-job
    /// signature, so a settled job earns a citable [`PayoutOutcome`] the
    /// public proof feed can serve.
    mint: Option<String>,
}

impl MockPayout {
    pub fn new() -> Self {
        Self::default()
    }

    /// A mock that reports a per-transfer obligation cap, for exercising
    /// the request handlers' up-front refusal without a real signer.
    pub fn with_obligation_cap(cap_micro_usdc: u64) -> Self {
        Self {
            obligation_cap: Some(cap_micro_usdc),
            ..Self::default()
        }
    }

    /// A mock that reports a per-job payout cap, for exercising
    /// `submit_job`'s up-front refusal of an offer whose net could never
    /// be paid out — without a real signer.
    pub fn with_per_job_cap(cap_micro_usdc: u64) -> Self {
        Self {
            per_job_cap: Some(cap_micro_usdc),
            ..Self::default()
        }
    }

    /// A mock that settles on a chain: it reports `mint_b58` and stamps
    /// every payout with a per-job transaction signature, the shape the
    /// public settlement feed cites.
    pub fn with_onchain_mint(mint_b58: impl Into<String>) -> Self {
        Self {
            mint: Some(mint_b58.into()),
            ..Self::default()
        }
    }

    pub fn records(&self) -> Vec<PayoutRecord> {
        self.records.lock().clone()
    }

    pub fn transfers(&self) -> Vec<TransferRecord> {
        self.transfers.lock().clone()
    }

    /// Makes the next `n` `pay` calls fail with a transient backend error
    /// before any succeeds — the crash/transient window that strands a
    /// completed job's first-chance payout for the retry sweep to heal.
    pub fn fail_next_pays(&self, n: u32) {
        *self.fail_next_pays.lock() = n;
    }
}

#[async_trait]
impl Payout for MockPayout {
    async fn pay(
        &self,
        job_id: Uuid,
        operator_pubkey_b58: &str,
        payout_address: &str,
        amount_micro_usdc: u64,
        _receipt: &SignedWorkReceipt,
    ) -> Result<PayoutRecord, PayoutError> {
        {
            let mut remaining = self.fail_next_pays.lock();
            if *remaining > 0 {
                *remaining -= 1;
                return Err(PayoutError::Backend(
                    "mock payout: injected transient push failure".into(),
                ));
            }
        }
        let record = PayoutRecord {
            job_id,
            operator_pubkey_b58: operator_pubkey_b58.to_string(),
            payout_address: payout_address.to_string(),
            amount_micro_usdc,
            recorded_at_ms: epoch_ms(),
            tx_signature: self.mint.as_ref().map(|_| format!("mock-onchain-{job_id}")),
        };
        self.records.lock().push(record.clone());
        Ok(record)
    }

    async fn transfer(
        &self,
        transfer_id: Uuid,
        recipient_address: &str,
        amount_micro_usdc: u64,
        _memo: &str,
    ) -> Result<TransferRecord, PayoutError> {
        let mut transfers = self.transfers.lock();
        if let Some(record) = transfers.iter().find(|r| r.transfer_id == transfer_id) {
            return Ok(record.clone());
        }
        let record = TransferRecord {
            transfer_id,
            recipient_address: recipient_address.to_string(),
            amount_micro_usdc,
            recorded_at_ms: epoch_ms(),
            tx_signature: None,
        };
        transfers.push(record.clone());
        Ok(record)
    }

    fn obligation_cap_micro_usdc(&self) -> Option<u64> {
        self.obligation_cap
    }

    fn per_job_cap_micro_usdc(&self) -> Option<u64> {
        self.per_job_cap
    }

    fn payout_mint_b58(&self) -> Option<&str> {
        self.mint.as_deref()
    }
}

/// Config for [`SidecarPayout`]. `funding_keypair_path` and `rpc_url`
/// are forwarded to the sidecar's own environment when it is spawned
/// — this process never opens the keypair file itself, only knows its
/// path (the same posture `covenantd`'s `SubprocessSigner`/
/// `SubprocessMetaplexSigner` already take toward their own signer
/// sidecars).
#[derive(Debug, Clone)]
pub struct SidecarPayoutConfig {
    /// Path to a built `covenant-x402-signer` binary (or anything
    /// speaking its `payout` stdin/stdout protocol).
    pub signer_binary: PathBuf,
    /// Solana RPC endpoint the sidecar submits and confirms against.
    /// No default anywhere in this path — see `build-notes-phase1-payout.md`.
    pub rpc_url: String,
    /// Path to the JSON keypair file that funds payouts. Read only by
    /// the sidecar process, never by the coordinator.
    pub funding_keypair_path: String,
    /// The SPL mint paid out in (a throwaway devnet mint today; the
    /// real USDC mint after the operator-gated mainnet cutover).
    pub mint: String,
    /// Hard per-payout ceiling in micro-USDC for job earnings
    /// ([`Payout::pay`]): `pay` refuses to move more than this regardless
    /// of what it's asked to pay — sizing this number is an operator
    /// call, not decided here. It bounds one automated per-job push
    /// derived from an operator's claim; it deliberately does NOT bound a
    /// party's own principal (a buyer withdrawal, an unbond refund),
    /// which flows through [`Payout::transfer`] under `obligation_cap`.
    pub cap_micro_usdc: u64,
    /// Per-transfer ceiling in micro-USDC for obligation transfers
    /// ([`Payout::transfer`]): buyer withdrawals and unbond refunds. `0`
    /// means no artificial per-transfer limit — a party's own money is
    /// already bounded by the books (deposits minus charges, staked
    /// principal minus slash), so the safe default is to let it move in
    /// one transfer. A non-zero value bounds a single obligation
    /// transfer; requests above it are refused at the request handler,
    /// never debited into a push the sweep can only spin on.
    pub obligation_cap_micro_usdc: u64,
}

#[derive(Default)]
struct Ledger {
    completed: HashMap<Uuid, PayoutRecord>,
    completed_transfers: HashMap<Uuid, TransferRecord>,
    in_flight: HashSet<Uuid>,
}

/// Real payout backend: shells out to the `covenant-x402-signer`
/// sidecar's `payout` mode for a signed, submitted, confirmed SPL
/// transfer. Enforces, in this impl (not just in tests):
///
/// - **amount = held, never the receipt's claimed price** — the
///   transfer always moves exactly the caller-supplied
///   `amount_micro_usdc` (the escrow-released amount per
///   `http.rs::submit_result`); `receipt` is never consulted for
///   sizing.
/// - **destination = the registered payout_address** — refuses an
///   empty or non-base58/non-32-byte address rather than let a
///   registry-lookup gap (e.g. an unregistered operator resolving to
///   `""`) silently target a bogus account.
/// - **a hard per-payout cap** (`cap_micro_usdc`) on job earnings, and a
///   separate **per-transfer obligation cap** (`obligation_cap_micro_usdc`,
///   off by default) on principal returns — the job cap never blocks a
///   buyer withdrawing their own balance or an operator reclaiming an
///   unbonded stake.
/// - **idempotent per job_id** — a job already paid returns the
///   cached record instead of paying again; a job whose payout is
///   already in flight (a concurrent duplicate call) is rejected
///   rather than racing a second transfer.
///
/// All four fail closed: on any guardrail violation this returns
/// `Err` before the sidecar is ever spawned.
pub struct SidecarPayout {
    config: SidecarPayoutConfig,
    ledger: Mutex<Ledger>,
}

#[derive(Debug, serde::Deserialize)]
struct SidecarResponse {
    signature: String,
}

/// The signer's structured failure line (stdout, alongside the nonzero
/// exit): `stage` says whether the transaction could be live on-chain.
/// Older signers print nothing parseable here — treated as unresolved,
/// the conservative reading for money that may have moved.
#[derive(Debug, serde::Deserialize)]
struct SidecarFailure {
    error: String,
    stage: String,
    #[serde(default)]
    signature: Option<String>,
}

impl SidecarPayout {
    pub fn new(config: SidecarPayoutConfig) -> Self {
        Self {
            config,
            ledger: Mutex::new(Ledger::default()),
        }
    }

    /// The recorded payout for `job_id`, if one has already completed.
    pub fn record_for(&self, job_id: Uuid) -> Option<PayoutRecord> {
        self.ledger.lock().completed.get(&job_id).cloned()
    }

    /// The per-transfer obligation ceiling, or `None` when unset (`0`) —
    /// principal returns then move in one transfer, bounded only by the
    /// books. See [`SidecarPayoutConfig::obligation_cap_micro_usdc`].
    fn obligation_cap(&self) -> Option<u64> {
        (self.config.obligation_cap_micro_usdc != 0)
            .then_some(self.config.obligation_cap_micro_usdc)
    }

    /// Guardrails + the one-shot sidecar invocation shared by
    /// [`Payout::pay`] and [`Payout::transfer`]: validates the
    /// recipient and amount, spawns the signer, and returns the
    /// submitted transaction's signature. `label` names the obligation
    /// in every error and in the sidecar's own tracing.
    async fn submit(
        &self,
        label: &str,
        recipient_address: &str,
        amount_micro_usdc: u64,
        memo: &str,
    ) -> Result<String, PayoutError> {
        let decoded = bs58::decode(recipient_address).into_vec().map_err(|e| {
            PayoutError::Backend(format!(
                "{label}: recipient {recipient_address:?} is not valid base58: {e}"
            ))
        })?;
        if decoded.len() != 32 {
            return Err(PayoutError::Backend(format!(
                "{label}: recipient {recipient_address:?} decodes to {} bytes, not a 32-byte pubkey",
                decoded.len()
            )));
        }

        if amount_micro_usdc == 0 {
            return Err(PayoutError::Backend(format!(
                "{label}: refusing a zero-amount transfer"
            )));
        }
        // The applicable ceiling is the caller's — the per-payout cap
        // for job earnings, the obligation cap for principal returns —
        // so this shared path never blocks a withdrawal with a limit
        // meant for a per-job push. Each caller checks before spawning.

        // `amount_micro_usdc` — the caller-supplied, books-derived
        // amount — is the only figure used to build the request below.
        // For a payout the receipt's only contribution is the memo
        // derived from it; the operator's claimed price never reaches
        // this code path at all.
        let request = serde_json::json!({
            "mint": self.config.mint,
            "destination_owner": recipient_address,
            "amount": amount_micro_usdc,
            "job_id": label,
            "memo": memo,
        });
        let payload = serde_json::to_vec(&request)
            .map_err(|e| PayoutError::Backend(format!("encode payout request: {e}")))?;

        let mut child = Command::new(&self.config.signer_binary)
            .arg("payout")
            .env_clear()
            .env(
                "COVENANT_X402_FUNDING_KEYPAIR",
                &self.config.funding_keypair_path,
            )
            .env("COVENANT_X402_RPC_URL", &self.config.rpc_url)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                PayoutError::Backend(format!(
                    "spawn payout signer {:?}: {e}",
                    self.config.signer_binary
                ))
            })?;

        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| PayoutError::Backend("payout signer stdin unavailable".into()))?;
            stdin
                .write_all(&payload)
                .await
                .map_err(|e| PayoutError::Backend(format!("write to payout signer: {e}")))?;
            // Drop closes stdin so the one-shot sidecar sees EOF.
        }

        // From here on the request has left this process: any failure
        // whose outcome the signer can't (or didn't) classify must be
        // treated as a transfer that may have landed.
        let output = child
            .wait_with_output()
            .await
            .map_err(|e| PayoutError::Unresolved {
                message: format!("await payout signer: {e}"),
                tx_signature: None,
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            // The signer names the failure's stage on stdout. Only a
            // definitive "not submitted" is retryable; anything else —
            // including a signer too old or too broken to say — could
            // be a live transaction.
            if let Ok(failure) = serde_json::from_str::<SidecarFailure>(stdout.trim()) {
                if failure.stage == "not_submitted" {
                    return Err(PayoutError::Backend(format!(
                        "{label}: payout signer refused: {}",
                        failure.error
                    )));
                }
                return Err(PayoutError::Unresolved {
                    message: format!("{label}: {}", failure.error),
                    tx_signature: failure.signature,
                });
            }
            return Err(PayoutError::Unresolved {
                message: format!(
                    "{label}: payout signer exited {} without naming a stage: {}",
                    output.status,
                    stderr.trim()
                ),
                tx_signature: None,
            });
        }
        // Exit 0 means the signer confirmed the transfer; losing the
        // response now loses the signature, not the money's whereabouts
        // — but without the signature the record can't be completed, so
        // it still reconciles as unresolved.
        let stdout = String::from_utf8_lossy(&output.stdout);
        let response: SidecarResponse =
            serde_json::from_str(stdout.trim()).map_err(|e| PayoutError::Unresolved {
                message: format!("{label}: decode payout signer response: {e}"),
                tx_signature: None,
            })?;
        Ok(response.signature)
    }

    async fn pay_uncached(
        &self,
        job_id: Uuid,
        operator_pubkey_b58: &str,
        payout_address: &str,
        amount_micro_usdc: u64,
        memo: &str,
    ) -> Result<PayoutRecord, PayoutError> {
        if payout_address.is_empty() {
            return Err(PayoutError::Backend(format!(
                "job {job_id}: refusing to pay — operator {operator_pubkey_b58} has no registered payout_address"
            )));
        }
        if amount_micro_usdc > self.config.cap_micro_usdc {
            return Err(PayoutError::Backend(format!(
                "job {job_id}: amount {amount_micro_usdc} micro-usdc exceeds the per-payout cap {}",
                self.config.cap_micro_usdc
            )));
        }
        let signature = self
            .submit(
                &format!("job {job_id}"),
                payout_address,
                amount_micro_usdc,
                memo,
            )
            .await?;
        Ok(PayoutRecord {
            job_id,
            operator_pubkey_b58: operator_pubkey_b58.to_string(),
            payout_address: payout_address.to_string(),
            amount_micro_usdc,
            recorded_at_ms: epoch_ms(),
            tx_signature: Some(signature),
        })
    }
}

#[async_trait]
impl Payout for SidecarPayout {
    async fn pay(
        &self,
        job_id: Uuid,
        operator_pubkey_b58: &str,
        payout_address: &str,
        amount_micro_usdc: u64,
        receipt: &SignedWorkReceipt,
    ) -> Result<PayoutRecord, PayoutError> {
        {
            let mut ledger = self.ledger.lock();
            if let Some(record) = ledger.completed.get(&job_id) {
                return Ok(record.clone());
            }
            if !ledger.in_flight.insert(job_id) {
                return Err(PayoutError::Backend(format!(
                    "payout for job {job_id} is already in flight"
                )));
            }
        }

        // Stamped onto the transfer as an SPL memo: the on-chain
        // transaction names the receipt it honors, so anyone holding
        // the signed receipt can verify the chain paid for this work.
        let memo = receipt.payout_memo();
        let outcome = self
            .pay_uncached(
                job_id,
                operator_pubkey_b58,
                payout_address,
                amount_micro_usdc,
                &memo,
            )
            .await;

        let mut ledger = self.ledger.lock();
        ledger.in_flight.remove(&job_id);
        if let Ok(record) = &outcome {
            ledger.completed.insert(job_id, record.clone());
        }
        outcome
    }

    fn obligation_cap_micro_usdc(&self) -> Option<u64> {
        self.obligation_cap()
    }

    fn per_job_cap_micro_usdc(&self) -> Option<u64> {
        Some(self.config.cap_micro_usdc)
    }

    fn payout_mint_b58(&self) -> Option<&str> {
        Some(&self.config.mint)
    }

    async fn transfer(
        &self,
        transfer_id: Uuid,
        recipient_address: &str,
        amount_micro_usdc: u64,
        memo: &str,
    ) -> Result<TransferRecord, PayoutError> {
        if let Some(cap) = self.obligation_cap() {
            if amount_micro_usdc > cap {
                return Err(PayoutError::Backend(format!(
                    "transfer {transfer_id}: amount {amount_micro_usdc} micro-usdc exceeds \
                     the per-transfer obligation cap {cap}"
                )));
            }
        }
        {
            let mut ledger = self.ledger.lock();
            if let Some(record) = ledger.completed_transfers.get(&transfer_id) {
                return Ok(record.clone());
            }
            if !ledger.in_flight.insert(transfer_id) {
                return Err(PayoutError::Backend(format!(
                    "transfer {transfer_id} is already in flight"
                )));
            }
        }

        let outcome = self
            .submit(
                &format!("transfer {transfer_id}"),
                recipient_address,
                amount_micro_usdc,
                memo,
            )
            .await
            .map(|signature| TransferRecord {
                transfer_id,
                recipient_address: recipient_address.to_string(),
                amount_micro_usdc,
                recorded_at_ms: epoch_ms(),
                tx_signature: Some(signature),
            });

        let mut ledger = self.ledger.lock();
        ledger.in_flight.remove(&transfer_id);
        if let Ok(record) = &outcome {
            ledger
                .completed_transfers
                .insert(transfer_id, record.clone());
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_a2a::A2ATaskStatus;
    use covenant_compute_protocol::{JobMeter, WorkReceiptPayload};
    use covenant_identity::LocalIdentity;

    #[tokio::test]
    async fn mock_payout_records_the_intended_transfer() {
        let payout = MockPayout::new();
        let operator = LocalIdentity::generate("operator@local");
        let receipt = SignedWorkReceipt::sign(
            WorkReceiptPayload {
                job_id: Uuid::new_v4(),
                operator: operator.agent_id(),
                job_hash_hex: "aa".repeat(32),
                result_hash_hex: "bb".repeat(32),
                meter: JobMeter {
                    wall_ms: 1,
                    tokens_in: None,
                    tokens_out: None,
                    gpu_seconds: None,
                    finish_reason: None,
                },
                price_micro_usdc: 5_000,
                status: A2ATaskStatus::Ok,
                executed_at_ms: 1,
                node_audit_root_hex: "cc".repeat(32),
            },
            &operator,
        )
        .unwrap();

        let job_id = receipt.receipt.job_id;
        payout
            .pay(job_id, "op-pubkey", "payout-addr", 5_000, &receipt)
            .await
            .unwrap();

        let records = payout.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].job_id, job_id);
        assert_eq!(records[0].amount_micro_usdc, 5_000);
        assert_eq!(records[0].payout_address, "payout-addr");
    }

    fn valid_payout_address() -> &'static str {
        "9VaDVp1Wb78G4Wm6VuTiMrpESjrUymXefQTHcJGRSTEA"
    }

    fn receipt_with_price(
        job_id: Uuid,
        operator: &LocalIdentity,
        price_micro_usdc: u64,
    ) -> SignedWorkReceipt {
        SignedWorkReceipt::sign(
            WorkReceiptPayload {
                job_id,
                operator: operator.agent_id(),
                job_hash_hex: "aa".repeat(32),
                result_hash_hex: "bb".repeat(32),
                meter: JobMeter {
                    wall_ms: 1,
                    tokens_in: None,
                    tokens_out: None,
                    gpu_seconds: None,
                    finish_reason: None,
                },
                price_micro_usdc,
                status: A2ATaskStatus::Ok,
                executed_at_ms: 1,
                node_audit_root_hex: "cc".repeat(32),
            },
            operator,
        )
        .unwrap()
    }

    /// Writes an executable shell-script stand-in for the sidecar
    /// binary. `Command::new(signer_binary).arg("payout")` is exactly
    /// how `SidecarPayout` invokes the real thing, and a `sh` script
    /// with a shebang answers to that the same way a real binary
    /// would — no network, no real signing.
    fn write_stub_signer(dir: &std::path::Path, script_body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(format!("stub-{}.sh", Uuid::new_v4()));
        std::fs::write(&path, format!("#!/bin/sh\n{script_body}\n")).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    fn config_for(
        dir: &std::path::Path,
        signer_binary: PathBuf,
        cap_micro_usdc: u64,
    ) -> SidecarPayoutConfig {
        // Obligations mirror the job cap here so the shared-guardrail
        // tests still exercise a bounded transfer path; the tests that
        // care about the two caps diverging use `config_with_caps`.
        config_with_caps(dir, signer_binary, cap_micro_usdc, cap_micro_usdc)
    }

    fn config_with_caps(
        dir: &std::path::Path,
        signer_binary: PathBuf,
        cap_micro_usdc: u64,
        obligation_cap_micro_usdc: u64,
    ) -> SidecarPayoutConfig {
        SidecarPayoutConfig {
            signer_binary,
            rpc_url: "https://unused.example".into(),
            funding_keypair_path: dir
                .join("unused-funding-keypair.json")
                .display()
                .to_string(),
            mint: "So11111111111111111111111111111111111111112".into(),
            cap_micro_usdc,
            obligation_cap_micro_usdc,
        }
    }

    #[tokio::test]
    async fn sidecar_payout_uses_the_held_amount_never_the_receipts_claimed_price() {
        let dir = tempfile::tempdir().unwrap();
        let capture = dir.path().join("captured-stdin.json");
        let stub = write_stub_signer(
            dir.path(),
            &format!(
                "cat > '{}'\nprintf '{{\"signature\":\"stub-sig\"}}'",
                capture.display()
            ),
        );
        let payout = SidecarPayout::new(config_for(dir.path(), stub, 1_000_000));

        let operator = LocalIdentity::generate("operator@local");
        let job_id = Uuid::new_v4();
        // The receipt claims a wildly different price than what's
        // actually held/escrowed — must never influence the transfer.
        let receipt = receipt_with_price(job_id, &operator, 999_999_999);

        let record = payout
            .pay(job_id, "op-pubkey", valid_payout_address(), 5_000, &receipt)
            .await
            .expect("pay");
        assert_eq!(record.amount_micro_usdc, 5_000);

        let captured: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&capture).unwrap()).unwrap();
        assert_eq!(captured["amount"], 5_000);
        assert_eq!(captured["destination_owner"], valid_payout_address());
        assert_eq!(
            captured["memo"],
            receipt.payout_memo().as_str(),
            "the sidecar request must carry the receipt-derived memo"
        );
    }

    #[tokio::test]
    async fn sidecar_payout_returns_the_sidecars_signature() {
        let dir = tempfile::tempdir().unwrap();
        let stub = write_stub_signer(
            dir.path(),
            "cat >/dev/null; printf '{\"signature\":\"devnet-sig-1\"}'",
        );
        let payout = SidecarPayout::new(config_for(dir.path(), stub, 1_000_000));

        let operator = LocalIdentity::generate("operator@local");
        let job_id = Uuid::new_v4();
        let receipt = receipt_with_price(job_id, &operator, 5_000);

        let record = payout
            .pay(job_id, "op-pubkey", valid_payout_address(), 5_000, &receipt)
            .await
            .expect("pay");
        assert_eq!(record.tx_signature, Some("devnet-sig-1".to_string()));
    }

    #[tokio::test]
    async fn sidecar_payout_rejects_an_empty_payout_address() {
        let dir = tempfile::tempdir().unwrap();
        let payout = SidecarPayout::new(config_for(
            dir.path(),
            dir.path().join("never-run"),
            1_000_000,
        ));
        let operator = LocalIdentity::generate("operator@local");
        let job_id = Uuid::new_v4();
        let receipt = receipt_with_price(job_id, &operator, 5_000);

        let err = payout
            .pay(job_id, "op-pubkey", "", 5_000, &receipt)
            .await
            .expect_err("empty address");
        assert!(
            matches!(&err, PayoutError::Backend(msg) if msg.contains("no registered payout_address"))
        );
    }

    #[tokio::test]
    async fn sidecar_payout_rejects_an_invalid_base58_payout_address() {
        let dir = tempfile::tempdir().unwrap();
        let payout = SidecarPayout::new(config_for(
            dir.path(),
            dir.path().join("never-run"),
            1_000_000,
        ));
        let operator = LocalIdentity::generate("operator@local");
        let job_id = Uuid::new_v4();
        let receipt = receipt_with_price(job_id, &operator, 5_000);

        let err = payout
            .pay(job_id, "op-pubkey", "not-valid-base58!!!", 5_000, &receipt)
            .await
            .expect_err("invalid base58");
        assert!(matches!(&err, PayoutError::Backend(msg) if msg.contains("not valid base58")));
    }

    #[tokio::test]
    async fn sidecar_payout_rejects_a_payout_address_of_the_wrong_length() {
        let dir = tempfile::tempdir().unwrap();
        let payout = SidecarPayout::new(config_for(
            dir.path(),
            dir.path().join("never-run"),
            1_000_000,
        ));
        let operator = LocalIdentity::generate("operator@local");
        let job_id = Uuid::new_v4();
        let receipt = receipt_with_price(job_id, &operator, 5_000);

        // Valid base58, but far too short to be a real 32-byte pubkey.
        let err = payout
            .pay(job_id, "op-pubkey", "abc", 5_000, &receipt)
            .await
            .expect_err("wrong length");
        assert!(matches!(&err, PayoutError::Backend(msg) if msg.contains("not a 32-byte pubkey")));
    }

    #[tokio::test]
    async fn sidecar_payout_rejects_amount_over_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let payout =
            SidecarPayout::new(config_for(dir.path(), dir.path().join("never-run"), 1_000));
        let operator = LocalIdentity::generate("operator@local");
        let job_id = Uuid::new_v4();
        let receipt = receipt_with_price(job_id, &operator, 1_001);

        let err = payout
            .pay(job_id, "op-pubkey", valid_payout_address(), 1_001, &receipt)
            .await
            .expect_err("over cap");
        assert!(
            matches!(&err, PayoutError::Backend(msg) if msg.contains("exceeds the per-payout cap"))
        );
    }

    #[tokio::test]
    async fn sidecar_payout_rejects_a_zero_amount() {
        let dir = tempfile::tempdir().unwrap();
        let payout = SidecarPayout::new(config_for(
            dir.path(),
            dir.path().join("never-run"),
            1_000_000,
        ));
        let operator = LocalIdentity::generate("operator@local");
        let job_id = Uuid::new_v4();
        let receipt = receipt_with_price(job_id, &operator, 0);

        let err = payout
            .pay(job_id, "op-pubkey", valid_payout_address(), 0, &receipt)
            .await
            .expect_err("zero amount");
        assert!(matches!(&err, PayoutError::Backend(msg) if msg.contains("zero-amount")));
    }

    #[tokio::test]
    async fn a_stageless_sidecar_death_reads_as_unresolved() {
        // A signer that dies without naming a stage on stdout could
        // have died before OR after submitting — the caller must treat
        // the transfer as possibly live, never blind-retry it.
        let dir = tempfile::tempdir().unwrap();
        let stub = write_stub_signer(
            dir.path(),
            "cat >/dev/null; echo 'devnet rpc unreachable' >&2; exit 7",
        );
        let payout = SidecarPayout::new(config_for(dir.path(), stub, 1_000_000));
        let operator = LocalIdentity::generate("operator@local");
        let job_id = Uuid::new_v4();
        let receipt = receipt_with_price(job_id, &operator, 5_000);

        let err = payout
            .pay(job_id, "op-pubkey", valid_payout_address(), 5_000, &receipt)
            .await
            .expect_err("nonzero exit");
        assert!(
            matches!(&err, PayoutError::Unresolved { message, .. } if message.contains("devnet rpc unreachable"))
        );
        // An unknown outcome must not be cached as completed either.
        assert!(payout.record_for(job_id).is_none());
    }

    #[tokio::test]
    async fn a_not_submitted_failure_stays_retryable_and_a_maybe_submitted_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let operator = LocalIdentity::generate("operator@local");

        let refused = write_stub_signer(
            dir.path(),
            r#"cat >/dev/null; printf '{"error":"blockhash expired","stage":"not_submitted"}'; exit 1"#,
        );
        let payout = SidecarPayout::new(config_for(dir.path(), refused, 1_000_000));
        let job_id = Uuid::new_v4();
        let receipt = receipt_with_price(job_id, &operator, 5_000);
        let err = payout
            .pay(job_id, "op-pubkey", valid_payout_address(), 5_000, &receipt)
            .await
            .expect_err("refused");
        assert!(matches!(&err, PayoutError::Backend(msg) if msg.contains("blockhash expired")));

        let ambiguous = write_stub_signer(
            dir.path(),
            r#"cat >/dev/null; printf '{"error":"confirm timed out","stage":"maybe_submitted","signature":"sig-live"}'; exit 1"#,
        );
        let payout = SidecarPayout::new(config_for(dir.path(), ambiguous, 1_000_000));
        let job_id = Uuid::new_v4();
        let receipt = receipt_with_price(job_id, &operator, 5_000);
        let err = payout
            .pay(job_id, "op-pubkey", valid_payout_address(), 5_000, &receipt)
            .await
            .expect_err("ambiguous");
        assert!(matches!(
            &err,
            PayoutError::Unresolved { message, tx_signature }
                if message.contains("confirm timed out")
                    && tx_signature.as_deref() == Some("sig-live")
        ));
    }

    #[tokio::test]
    async fn sidecar_payout_is_idempotent_per_job_id() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("invocations");
        let stub = write_stub_signer(
            dir.path(),
            &format!(
                "cat >/dev/null; echo x >> '{}'; printf '{{\"signature\":\"devnet-sig-once\"}}'",
                counter.display()
            ),
        );
        let payout = SidecarPayout::new(config_for(dir.path(), stub, 1_000_000));
        let operator = LocalIdentity::generate("operator@local");
        let job_id = Uuid::new_v4();
        let receipt = receipt_with_price(job_id, &operator, 5_000);

        let first = payout
            .pay(job_id, "op-pubkey", valid_payout_address(), 5_000, &receipt)
            .await
            .expect("first pay");
        let second = payout
            .pay(job_id, "op-pubkey", valid_payout_address(), 5_000, &receipt)
            .await
            .expect("second pay must not double-pay");

        assert_eq!(first, second);
        let invocations = std::fs::read_to_string(&counter).unwrap();
        assert_eq!(
            invocations.lines().count(),
            1,
            "the sidecar must be invoked exactly once across two pay() calls for the same job_id"
        );
    }

    #[tokio::test]
    async fn mock_transfer_records_once_per_transfer_id() {
        let payout = MockPayout::new();
        let transfer_id = Uuid::new_v4();
        let first = payout
            .transfer(
                transfer_id,
                "recipient-addr",
                7_000,
                "compute-withdrawal:v1:b:w",
            )
            .await
            .unwrap();
        let second = payout
            .transfer(
                transfer_id,
                "recipient-addr",
                7_000,
                "compute-withdrawal:v1:b:w",
            )
            .await
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(payout.transfers().len(), 1);
        assert_eq!(payout.transfers()[0].amount_micro_usdc, 7_000);
        assert_eq!(payout.transfers()[0].tx_signature, None);
    }

    #[tokio::test]
    async fn sidecar_transfer_carries_the_obligation_memo_and_signature() {
        let dir = tempfile::tempdir().unwrap();
        let capture = dir.path().join("captured-stdin.json");
        let stub = write_stub_signer(
            dir.path(),
            &format!(
                "cat > '{}'\nprintf '{{\"signature\":\"withdraw-sig-1\"}}'",
                capture.display()
            ),
        );
        let payout = SidecarPayout::new(config_for(dir.path(), stub, 1_000_000));

        let transfer_id = Uuid::new_v4();
        let memo = format!("compute-withdrawal:v1:buyer-pubkey:{transfer_id}");
        let record = payout
            .transfer(transfer_id, valid_payout_address(), 9_000, &memo)
            .await
            .expect("transfer");
        assert_eq!(record.tx_signature, Some("withdraw-sig-1".into()));
        assert_eq!(record.amount_micro_usdc, 9_000);

        let captured: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&capture).unwrap()).unwrap();
        assert_eq!(captured["amount"], 9_000);
        assert_eq!(captured["destination_owner"], valid_payout_address());
        assert_eq!(captured["memo"], memo.as_str());
    }

    #[tokio::test]
    async fn sidecar_transfer_shares_the_payout_guardrails() {
        let dir = tempfile::tempdir().unwrap();
        let payout =
            SidecarPayout::new(config_for(dir.path(), dir.path().join("never-run"), 1_000));

        let over_cap = payout
            .transfer(Uuid::new_v4(), valid_payout_address(), 1_001, "m")
            .await
            .expect_err("over cap");
        assert!(
            matches!(&over_cap, PayoutError::Backend(msg) if msg.contains("exceeds the per-transfer obligation cap"))
        );
        let zero = payout
            .transfer(Uuid::new_v4(), valid_payout_address(), 0, "m")
            .await
            .expect_err("zero");
        assert!(matches!(&zero, PayoutError::Backend(msg) if msg.contains("zero-amount")));
        let bogus = payout
            .transfer(Uuid::new_v4(), "abc", 500, "m")
            .await
            .expect_err("short address");
        assert!(
            matches!(&bogus, PayoutError::Backend(msg) if msg.contains("not a 32-byte pubkey"))
        );
    }

    #[tokio::test]
    async fn an_obligation_transfer_is_not_bound_by_the_per_job_payout_cap() {
        // A buyer withdrawal (or unbond refund) is the party's own
        // principal — the tight per-job payout cap must never block it.
        // This is the mainnet-observed defect: a 90000 withdrawal against
        // a 50000 job cap was refused and the sweep spun forever.
        let dir = tempfile::tempdir().unwrap();
        let stub = write_stub_signer(
            dir.path(),
            "cat >/dev/null; printf '{\"signature\":\"withdraw-over-job-cap\"}'",
        );
        // Tiny job cap, generous obligation cap: the two are independent.
        let payout = SidecarPayout::new(config_with_caps(dir.path(), stub, 50_000, 1_000_000));

        let record = payout
            .transfer(
                Uuid::new_v4(),
                valid_payout_address(),
                90_000,
                "compute-withdrawal:v1:b:w",
            )
            .await
            .expect("a book-bounded withdrawal above the job cap must still transfer");
        assert_eq!(record.amount_micro_usdc, 90_000);
        assert_eq!(record.tx_signature, Some("withdraw-over-job-cap".into()));

        // The same amount as a JOB payout is still refused — the job cap
        // is untouched, only obligations are exempt from it.
        let operator = LocalIdentity::generate("operator@local");
        let job_id = Uuid::new_v4();
        let receipt = receipt_with_price(job_id, &operator, 90_000);
        let err = payout
            .pay(
                job_id,
                "op-pubkey",
                valid_payout_address(),
                90_000,
                &receipt,
            )
            .await
            .expect_err("a job payout above the per-job cap stays refused");
        assert!(
            matches!(&err, PayoutError::Backend(msg) if msg.contains("exceeds the per-payout cap"))
        );
    }

    #[tokio::test]
    async fn an_unset_obligation_cap_lets_any_book_bounded_transfer_through() {
        let dir = tempfile::tempdir().unwrap();
        let stub = write_stub_signer(
            dir.path(),
            "cat >/dev/null; printf '{\"signature\":\"unbounded-withdraw\"}'",
        );
        // Job cap present; obligation cap 0 = unset (the default).
        let payout = SidecarPayout::new(config_with_caps(dir.path(), stub, 50_000, 0));
        assert_eq!(payout.obligation_cap_micro_usdc(), None);

        let record = payout
            .transfer(Uuid::new_v4(), valid_payout_address(), 5_000_000_000, "m")
            .await
            .expect("an unset obligation cap imposes no per-transfer limit");
        assert_eq!(record.amount_micro_usdc, 5_000_000_000);
    }

    #[tokio::test]
    async fn a_bounded_obligation_cap_refuses_an_over_cap_transfer() {
        let dir = tempfile::tempdir().unwrap();
        let payout = SidecarPayout::new(config_with_caps(
            dir.path(),
            dir.path().join("never-run"),
            50_000,
            1_000,
        ));
        assert_eq!(payout.obligation_cap_micro_usdc(), Some(1_000));

        let err = payout
            .transfer(Uuid::new_v4(), valid_payout_address(), 1_001, "m")
            .await
            .expect_err("over the obligation cap");
        assert!(
            matches!(&err, PayoutError::Backend(msg) if msg.contains("exceeds the per-transfer obligation cap"))
        );
    }

    #[tokio::test]
    async fn sidecar_transfer_is_idempotent_and_does_not_cache_failures() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("invocations");
        let stub = write_stub_signer(
            dir.path(),
            &format!(
                "cat >/dev/null; echo x >> '{}'; printf '{{\"signature\":\"once\"}}'",
                counter.display()
            ),
        );
        let payout = SidecarPayout::new(config_for(dir.path(), stub, 1_000_000));
        let transfer_id = Uuid::new_v4();

        // A guardrail failure is not cached: the same id can retry.
        payout
            .transfer(transfer_id, valid_payout_address(), 0, "m")
            .await
            .expect_err("zero refused");
        let first = payout
            .transfer(transfer_id, valid_payout_address(), 4_000, "m")
            .await
            .expect("first transfer");
        let second = payout
            .transfer(transfer_id, valid_payout_address(), 4_000, "m")
            .await
            .expect("second must not double-pay");
        assert_eq!(first, second);
        assert_eq!(
            std::fs::read_to_string(&counter).unwrap().lines().count(),
            1,
            "the sidecar must be invoked exactly once across two transfer() calls for the same id"
        );
    }
}
