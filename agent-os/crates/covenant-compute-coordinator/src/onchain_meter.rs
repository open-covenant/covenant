//! The on-chain lease meter seam.
//!
//! A `LeaseSession` job already meters off-chain: the coordinator
//! stamps `accepted_at_ms` when the assignee accepts, and settlement
//! bills `LeaseTerms::metered_micro_usdc(now - accepted_at_ms)` out of
//! the window the buyer escrowed (`crate::http::submit_result`). That
//! meter is a coordinator claim. The `compute-lease` program under
//! `programs/compute-lease` is the same meter with the same arithmetic
//! — its charge is byte-identical to `LeaseTerms::metered_micro_usdc`
//! and it caps at the same 86_400s — running against a real escrow
//! vault, with each observation folded into a provenance hash chain a
//! buyer can recompute. Per-second ticks are affordable because they
//! run inside a MagicBlock Ephemeral Rollup, not on L1.
//!
//! This module is the seam between the two, and nothing more. It owns
//! four moments in a lease's life:
//!
//! 1. **open + delegate**, when the assignee accepts — the same write
//!    that stamps the off-chain meter's `t0`, so both meters start from
//!    one timestamp.
//! 2. **tick**, from [`spawn_periodic_lease_meter`], which walks every
//!    session the coordinator currently observes as running and pushes
//!    the cumulative elapsed. Cumulative, never a delta: a dropped,
//!    duplicated or restart-skipped tick self-heals on the next pass,
//!    so this task needs no cursor and no journal of its own.
//! 3. **undelegate + settle**, on the metered settlement path, from the
//!    same elapsed the job record pins as its explanation of the
//!    charge.
//! 4. **void**, on every path where the marketplace refunds the buyer in
//!    full — a lapsed deadline, a failed receipt, an expiry sweep, or an
//!    operator that rejects a lease it had accepted. The charge becomes
//!    zero and the whole vault goes home, so a refund can never leave a
//!    funded vault for the operator to settle in its own favour.
//!
//! Two implementations, following [`crate::payout`] exactly:
//! [`NoopLeaseMeter`] records the intended calls and touches no chain
//! (what the hermetic tests exercise), and [`SidecarLeaseMeter`] shells
//! out to a signing sidecar over stdin/stdout. This crate never links
//! `solana-sdk` and never holds a key — it only knows a binary's path,
//! the same posture [`crate::payout::SidecarPayout`] takes toward
//! `covenant-x402-signer`.
//!
//! Off by default. `CoordinatorConfig::lease_meter` is `None` unless a
//! deployment sets one, and with it unset every entry point here
//! returns immediately, so lease settlement behaves exactly as it does
//! today.
//!
//! # What this seam does not solve yet
//!
//! - **Marketplace fees are not collected on-chain.** `settle_lease`
//!   pays the operator the whole metered charge and returns the rest to
//!   the renter; there is no fee recipient in its account set. A
//!   deployment running a chain-settling meter with a non-zero fee
//!   would book a fee it never received, so `main.rs` refuses that
//!   combination at boot.
//!
//! # The keys this needs
//!
//! The program binds a lease to the coordinator named at open: `tick`,
//! `delegate_lease`, `undelegate_lease` and `void_lease` all require that
//! key's signature, so the sidecar is handed two keypair paths rather
//! than one. The renter's key funds and escrows; the coordinator's key
//! meters and never touches the vault. Pointing both at the same file
//! collapses the separation the program is enforcing, which is the whole
//! reason a renter cannot under-report their own usage.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use uuid::Uuid;

use crate::jobs::{JobPhase, JobRecord};
use crate::state::CoordinatorState;

/// Domain separator for the tick receipt hash. Versioned: changing what
/// goes into the preimage changes the chain a buyer recomputes, so it
/// changes the name too.
pub const LEASE_TICK_DOMAIN: &str = "covenant.compute.lease-tick.v1";

#[derive(Debug, thiserror::Error)]
pub enum LeaseMeterError {
    /// The call definitively did not reach the chain. Retrying is safe.
    #[error("{0}")]
    Backend(String),
    /// The backend cannot say whether the instruction landed — the
    /// sidecar died, timed out confirming, or lost its response after
    /// the transaction may have reached the cluster. Ticks are
    /// idempotent (cumulative, monotonic) so an unresolved tick is
    /// harmless; an unresolved settle is money that may have moved.
    #[error("lease meter outcome unknown: {message}")]
    Unresolved {
        message: String,
        tx_signature: Option<String>,
    },
}

/// Everything `open_lease` needs, read off the job record at accept.
#[derive(Debug, Clone, PartialEq)]
pub struct LeaseOpen {
    pub job_id: Uuid,
    /// The buyer who signed the envelope. The vault itself is funded by
    /// the deployment's renter key, which is the renter on chain and takes
    /// the vault's refund; the buyer's refund runs through their escrow
    /// hold like any other job's.
    pub renter_pubkey_b58: String,
    /// The assignee's registered payout address, pinned at assignment.
    pub operator_payout_address: String,
    pub rate_micro_usdc_per_sec: u64,
    pub max_duration_secs: u64,
    /// Coordinator-clock time the assignee accepted: the meter's `t0`,
    /// and the exact value settlement bills from.
    pub accepted_at_ms: u64,
}

impl LeaseOpen {
    /// The whole window the buyer escrowed — what the vault is funded
    /// with and what the on-chain charge clamps at. Saturating: the
    /// protocol already refused an envelope whose ceiling overflows.
    pub fn funded_micro_usdc(&self) -> u64 {
        self.rate_micro_usdc_per_sec
            .saturating_mul(self.max_duration_secs)
    }
}

/// One coordinator observation of a running session — what a tick
/// commits, and what the final undelegate settles on.
///
/// The elapsed and the charge are here because they are the meter; the
/// operator, the endpoint and the observation timestamp are here
/// because they are the only things a reader could not already derive
/// from committed state. Hashing job_id / metered_ms / charged alone
/// would fold in nothing the `LeaseMeter` account does not already say.
#[derive(Debug, Clone, PartialEq)]
pub struct LeaseObservation {
    pub job_id: Uuid,
    /// Coordinator-clock time of the observation itself.
    pub observed_at_ms: u64,
    /// Cumulative run since accept, on the coordinator's clock.
    pub elapsed_ms: u64,
    /// What that run costs under the buyer's signed terms.
    pub charged_micro_usdc: u64,
    /// Who was serving the session at this instant.
    pub operator_pubkey_b58: String,
    /// Where it was reachable, as published by the serving node. Empty
    /// when the node has not published an endpoint yet.
    pub endpoint: String,
    /// Whether the buyer had already asked to end the session.
    pub close_requested: bool,
}

impl LeaseObservation {
    /// The 32-byte receipt hash this observation folds into the lease's
    /// provenance root.
    ///
    /// Domain-separated, newline-joined ASCII, one field per line, in
    /// declaration order — so a buyer holding the same fields
    /// recomputes the same bytes without a serializer. Absent optionals
    /// are the empty string; the trailing newline is part of the
    /// preimage.
    pub fn receipt_hash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(LEASE_TICK_DOMAIN.as_bytes());
        hasher.update(b"\n");
        for field in [
            self.job_id.hyphenated().to_string(),
            self.observed_at_ms.to_string(),
            self.elapsed_ms.to_string(),
            self.charged_micro_usdc.to_string(),
            self.operator_pubkey_b58.clone(),
            self.endpoint.clone(),
            u8::from(self.close_requested).to_string(),
        ] {
            hasher.update(field.as_bytes());
            hasher.update(b"\n");
        }
        hasher.finalize().into()
    }
}

/// What an undelegate + settle actually committed.
#[derive(Debug, Clone, PartialEq)]
pub struct LeaseSettlement {
    pub job_id: Uuid,
    pub metered_ms: u64,
    /// What the vault paid the operator.
    pub charged_micro_usdc: u64,
    /// The settle transaction. `Some` means the operator has been paid
    /// on-chain and the off-chain payout push must not fire — `None`
    /// means a meter that recorded the intent without moving anything,
    /// so the off-chain path stays in charge, exactly as
    /// [`crate::payout::PayoutRecord::tx_signature`] reads.
    pub tx_signature: Option<String>,
}

/// The on-chain meter, behind a trait so the coordinator crate can hold
/// one without linking a chain client.
///
/// Every method is idempotent per `job_id` against the implementation's
/// own ledger: a second open is refused, a tick on a lease that is
/// concluding or already settled is skipped, and a second conclude
/// returns the first settlement instead of settling twice.
#[async_trait]
pub trait LeaseMeter: Send + Sync {
    /// Opens the escrow vault for an accepted lease and delegates it to
    /// the rollup validator so ticks can run there. Returns the opening
    /// transaction when the backend submitted one.
    async fn open_and_delegate(&self, open: &LeaseOpen) -> Result<Option<String>, LeaseMeterError>;

    /// Pushes one cumulative observation. `Ok(false)` means this meter
    /// holds no tickable lease for the job — never opened, already
    /// concluding, or already settled — which is an ordinary outcome,
    /// not a failure.
    async fn tick(&self, observation: &LeaseObservation) -> Result<bool, LeaseMeterError>;

    /// Commits the meter back to L1 and settles the vault: the operator
    /// is paid what the meter says, the renter gets the rest. `Ok(None)`
    /// means this meter holds no open lease for the job.
    async fn undelegate_and_settle(
        &self,
        observation: &LeaseObservation,
    ) -> Result<Option<LeaseSettlement>, LeaseMeterError>;

    /// Voids the lease and returns the whole vault to the renter, paying
    /// the operator nothing.
    ///
    /// The marketplace refunds a buyer in full on four terminal paths — a
    /// deadline that lapsed, a receipt that came back failed, a session
    /// swept expired, or an operator that rejected a lease it had already
    /// accepted. The on-chain charge has to become zero to match, or an
    /// operator could settle the still-funded vault for whatever seconds
    /// the meter took before the refund. `void_lease` forces the
    /// charge to zero regardless of the meter, so this needs no observed
    /// elapsed. `Ok(None)` means this meter holds no open lease for the
    /// job — never opened, or already concluded.
    async fn void(&self, job_id: Uuid) -> Result<Option<LeaseSettlement>, LeaseMeterError>;

    /// Takes back a lease opened before this process restarted. The
    /// ledger lives in memory, so without this a restart would stop the
    /// ticks and send the conclusion off chain for every lease still
    /// running. Adopting one whose open never landed costs nothing: the
    /// signer reads the lease from chain at every step and refuses it.
    fn adopt(&self, open: &LeaseOpen);

    /// One line for the boot log.
    fn describe(&self) -> String;
}

/// Per-job bookkeeping shared by both implementations: which leases are
/// open, which are mid-conclusion (so a periodic tick cannot race the
/// settlement it would overshoot), and which have settled.
#[derive(Default)]
struct LeaseLedger {
    open: HashMap<Uuid, LeaseOpen>,
    concluding: HashSet<Uuid>,
    settled: HashMap<Uuid, LeaseSettlement>,
}

impl LeaseLedger {
    /// Claims the job for an open. `false` when one is already open or
    /// settled — a lost-ack accept retry must not mint a second vault.
    fn claim_open(&mut self, open: &LeaseOpen) -> bool {
        if self.open.contains_key(&open.job_id) || self.settled.contains_key(&open.job_id) {
            return false;
        }
        self.open.insert(open.job_id, open.clone());
        true
    }

    fn tickable(&self, job_id: Uuid) -> bool {
        self.open.contains_key(&job_id) && !self.concluding.contains(&job_id)
    }

    /// Fences the job against further ticks and hands back its open
    /// terms. `None` when there is nothing to conclude — no lease open, or
    /// one already concluding. A conclude or a void releases the ledger
    /// lock across its sidecar call, so a second terminal call landing in
    /// that window (a settle racing the deadline sweep's void) must find
    /// the lease already claimed and fire nothing, or the program takes
    /// two terminal instructions for one vault. The first to claim the
    /// fence wins; the loser reads `None` and leaves the money to it.
    fn begin_conclude(&mut self, job_id: Uuid) -> Option<LeaseOpen> {
        if self.concluding.contains(&job_id) {
            return None;
        }
        let open = self.open.get(&job_id)?.clone();
        self.concluding.insert(job_id);
        Some(open)
    }

    fn finish_conclude(&mut self, job_id: Uuid, settlement: LeaseSettlement) {
        self.open.remove(&job_id);
        self.concluding.remove(&job_id);
        self.settled.insert(job_id, settlement);
    }

    /// Unfences a conclusion that failed, so a later attempt can retry.
    fn abandon_conclude(&mut self, job_id: Uuid) {
        self.concluding.remove(&job_id);
    }

    /// Re-admits a lease opened before a restart. A job this ledger has
    /// already settled or still holds is left as it is.
    fn adopt(&mut self, open: &LeaseOpen) {
        if !self.settled.contains_key(&open.job_id) {
            self.open.entry(open.job_id).or_insert_with(|| open.clone());
        }
    }
}

/// Records the intended calls; touches no chain and moves no money.
///
/// The default a deployment gets when it wants the seam exercised
/// without a signer, and what the hermetic tests assert against — the
/// [`crate::payout::MockPayout`] role. Because every settlement it
/// returns carries `tx_signature: None`, the off-chain payout push
/// stays in charge and lease settlement is bit-for-bit what it is with
/// no meter configured at all.
#[derive(Default)]
pub struct NoopLeaseMeter {
    ledger: Mutex<LeaseLedger>,
    opened: Mutex<Vec<LeaseOpen>>,
    ticks: Mutex<Vec<LeaseObservation>>,
    concluded: Mutex<Vec<LeaseObservation>>,
    voided: Mutex<Vec<Uuid>>,
}

impl NoopLeaseMeter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The leases this meter was asked to open, in order.
    pub fn opened(&self) -> Vec<LeaseOpen> {
        self.opened.lock().clone()
    }

    /// The observations this meter was asked to tick, in order.
    pub fn ticks(&self) -> Vec<LeaseObservation> {
        self.ticks.lock().clone()
    }

    /// The final observations this meter was asked to settle on.
    pub fn concluded(&self) -> Vec<LeaseObservation> {
        self.concluded.lock().clone()
    }

    /// The jobs this meter was asked to void, in order.
    pub fn voided(&self) -> Vec<Uuid> {
        self.voided.lock().clone()
    }
}

#[async_trait]
impl LeaseMeter for NoopLeaseMeter {
    async fn open_and_delegate(&self, open: &LeaseOpen) -> Result<Option<String>, LeaseMeterError> {
        if !self.ledger.lock().claim_open(open) {
            return Err(LeaseMeterError::Backend(format!(
                "job {}: a lease is already open or settled",
                open.job_id
            )));
        }
        self.opened.lock().push(open.clone());
        Ok(None)
    }

    async fn tick(&self, observation: &LeaseObservation) -> Result<bool, LeaseMeterError> {
        if !self.ledger.lock().tickable(observation.job_id) {
            return Ok(false);
        }
        self.ticks.lock().push(observation.clone());
        Ok(true)
    }

    async fn undelegate_and_settle(
        &self,
        observation: &LeaseObservation,
    ) -> Result<Option<LeaseSettlement>, LeaseMeterError> {
        let job_id = observation.job_id;
        {
            let ledger = self.ledger.lock();
            if let Some(settled) = ledger.settled.get(&job_id) {
                return Ok(Some(settled.clone()));
            }
        }
        if self.ledger.lock().begin_conclude(job_id).is_none() {
            return Ok(None);
        }
        self.concluded.lock().push(observation.clone());
        let settlement = LeaseSettlement {
            job_id,
            metered_ms: observation.elapsed_ms,
            charged_micro_usdc: observation.charged_micro_usdc,
            tx_signature: None,
        };
        self.ledger
            .lock()
            .finish_conclude(job_id, settlement.clone());
        Ok(Some(settlement))
    }

    async fn void(&self, job_id: Uuid) -> Result<Option<LeaseSettlement>, LeaseMeterError> {
        {
            let ledger = self.ledger.lock();
            if let Some(settled) = ledger.settled.get(&job_id) {
                return Ok(Some(settled.clone()));
            }
        }
        if self.ledger.lock().begin_conclude(job_id).is_none() {
            return Ok(None);
        }
        self.voided.lock().push(job_id);
        let settlement = LeaseSettlement {
            job_id,
            metered_ms: 0,
            charged_micro_usdc: 0,
            tx_signature: None,
        };
        self.ledger
            .lock()
            .finish_conclude(job_id, settlement.clone());
        Ok(Some(settlement))
    }

    fn adopt(&self, open: &LeaseOpen) {
        self.ledger.lock().adopt(open);
    }

    fn describe(&self) -> String {
        "noop (records intended lease-meter calls, touches no chain)".into()
    }
}

/// Config for [`SidecarLeaseMeter`]. The keypair path and the RPC
/// endpoints are forwarded to the sidecar's environment when it is
/// spawned; this process never opens the keypair file, only knows where
/// it is.
#[derive(Debug, Clone)]
pub struct SidecarLeaseMeterConfig {
    /// Path to a binary speaking the `lease-open` / `lease-tick` /
    /// `lease-conclude` stdin/stdout protocol below.
    pub signer_binary: PathBuf,
    /// The deployed meter: the standalone `compute-lease` program or the
    /// settlement program that carries it.
    pub program_id: String,
    /// The SPL mint the vault escrows in.
    pub mint: String,
    /// L1 endpoint for open, delegate and settle.
    pub rpc_url: String,
    /// Rollup endpoint for ticks and undelegate.
    pub er_rpc_url: String,
    /// The rollup validator the lease is pinned to. Pinned rather than
    /// routed: a session's meter must not move hosts mid-lease.
    pub er_validator: String,
    /// JSON keypair that funds the escrow and signs as the renter. Read
    /// only by the sidecar.
    pub renter_keypair_path: String,
    /// JSON keypair that signs every meter instruction. The program will
    /// not accept a tick, a delegation, a conclusion or a void from any
    /// other key, and this one can never move the vault.
    pub coordinator_keypair_path: String,
}

/// Real meter: shells out to a signing sidecar, one process per call.
///
/// The protocol mirrors [`crate::payout::SidecarPayout`] exactly. The
/// coordinator spawns the binary with a subcommand naming the step,
/// writes one JSON request object on stdin and closes it, and reads one
/// JSON object back:
///
/// - exit 0, `{"signature": "<base58>"}` — the instruction landed;
/// - nonzero, `{"error": "...", "stage": "not_submitted"}` — nothing
///   reached the cluster, safe to retry;
/// - anything else — treated as unresolved, the conservative reading
///   for a transaction that may be live.
///
/// Guardrails enforced here, before any process is spawned:
/// **destination is the registered payout address** (base58, 32 bytes,
/// non-empty); **the window is a real window** (non-zero rate, non-zero
/// duration inside the protocol's own `MAX_LEASE_DURATION_SECS`); and
/// **one vault per job** — a second open is refused rather than
/// funding a duplicate, and a concluded lease never settles twice.
pub struct SidecarLeaseMeter {
    config: Arc<SidecarLeaseMeterConfig>,
    ledger: Arc<Mutex<LeaseLedger>>,
}

/// How long a conclusion waits on a terminal call already in flight for
/// the same lease before reading its outcome as unknown.
const IN_FLIGHT_WAIT: Duration = Duration::from_secs(120);

enum Claim {
    /// This call holds the fence and runs the terminal step.
    Won,
    /// No lease is open for the job, or (for a void) another terminal
    /// call holds it.
    Nothing,
    /// An earlier terminal step already concluded it.
    Settled(LeaseSettlement),
}

#[derive(Debug, serde::Deserialize)]
struct SidecarResponse {
    signature: String,
}

#[derive(Debug, serde::Deserialize)]
struct SidecarFailure {
    error: String,
    stage: String,
    #[serde(default)]
    signature: Option<String>,
}

impl SidecarLeaseMeter {
    pub fn new(config: SidecarLeaseMeterConfig) -> Self {
        Self {
            config: Arc::new(config),
            ledger: Arc::default(),
        }
    }

    /// The settlement recorded for `job_id`, if one has concluded.
    pub fn settlement_for(&self, job_id: Uuid) -> Option<LeaseSettlement> {
        self.ledger.lock().settled.get(&job_id).cloned()
    }

    /// The fields every request carries: which program, which mint,
    /// which rollup, which lease.
    fn envelope(&self, job_id: Uuid) -> serde_json::Value {
        serde_json::json!({
            "program_id": self.config.program_id,
            "mint": self.config.mint,
            "er_validator": self.config.er_validator,
            "job_id": job_id.simple().to_string(),
        })
    }

    /// Claims the lease for a terminal step.
    ///
    /// A conclusion waits out one already in flight: a result redelivered
    /// while the first conclusion is still running must not read the
    /// fence as "no lease" and let the off-chain push pay an operator the
    /// vault is about to pay. A void does not wait; the call holding the
    /// fence decides the money, and a sweep should not stall on it.
    async fn claim_terminal(&self, job_id: Uuid, wait: bool) -> Result<Claim, LeaseMeterError> {
        let deadline = tokio::time::Instant::now() + IN_FLIGHT_WAIT;
        loop {
            {
                let mut ledger = self.ledger.lock();
                if let Some(settled) = ledger.settled.get(&job_id) {
                    return Ok(Claim::Settled(settled.clone()));
                }
                if !wait || !ledger.concluding.contains(&job_id) {
                    return Ok(match ledger.begin_conclude(job_id) {
                        Some(_) => Claim::Won,
                        None => Claim::Nothing,
                    });
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(LeaseMeterError::Unresolved {
                    message: format!(
                        "job {job_id}: an earlier conclusion of this lease has not resolved"
                    ),
                    tx_signature: None,
                });
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Runs a terminal step to completion whatever happens to the caller.
    /// The coordinator awaits this inside an HTTP handler that is dropped
    /// when its client stops waiting; a signer left running with nobody to
    /// record its outcome is how a vault settles unseen.
    async fn run_terminal(
        &self,
        step: &'static str,
        job_id: Uuid,
        request: serde_json::Value,
        metered_ms: u64,
        charged_micro_usdc: u64,
    ) -> Result<Option<LeaseSettlement>, LeaseMeterError> {
        let config = self.config.clone();
        let ledger = self.ledger.clone();
        tokio::spawn(async move {
            match run_signer(&config, step, job_id, request).await {
                Ok(signature) => {
                    let settlement = LeaseSettlement {
                        job_id,
                        metered_ms,
                        charged_micro_usdc,
                        tx_signature: Some(signature),
                    };
                    ledger.lock().finish_conclude(job_id, settlement.clone());
                    Ok(Some(settlement))
                }
                Err(e) => {
                    // Only a definitive refusal reopens the lease. An
                    // unresolved step may have moved the vault, so the
                    // fence stays up until someone reads the chain.
                    if matches!(e, LeaseMeterError::Backend(_)) {
                        ledger.lock().abandon_conclude(job_id);
                    }
                    Err(e)
                }
            }
        })
        .await
        .unwrap_or_else(|e| {
            Err(LeaseMeterError::Unresolved {
                message: format!("{step} job {job_id}: signer task failed: {e}"),
                tx_signature: None,
            })
        })
    }
}

/// Spawns the sidecar for one step and returns the transaction it
/// submitted. `step` is both the subcommand and the label in every error.
async fn run_signer(
    config: &SidecarLeaseMeterConfig,
    step: &str,
    job_id: Uuid,
    request: serde_json::Value,
) -> Result<String, LeaseMeterError> {
    let label = format!("{step} job {job_id}");
    let payload = serde_json::to_vec(&request)
        .map_err(|e| LeaseMeterError::Backend(format!("{label}: encode request: {e}")))?;

    let mut child = Command::new(&config.signer_binary)
        .arg(step)
        .env_clear()
        .env(
            "COVENANT_COMPUTE_LEASE_KEYPAIR",
            &config.renter_keypair_path,
        )
        .env(
            "COVENANT_COMPUTE_LEASE_COORDINATOR_KEYPAIR",
            &config.coordinator_keypair_path,
        )
        .env("COVENANT_COMPUTE_LEASE_RPC_URL", &config.rpc_url)
        .env("COVENANT_COMPUTE_LEASE_ER_RPC_URL", &config.er_rpc_url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            LeaseMeterError::Backend(format!(
                "{label}: spawn lease signer {:?}: {e}",
                config.signer_binary
            ))
        })?;

    {
        let mut stdin = child.stdin.take().ok_or_else(|| {
            LeaseMeterError::Backend(format!("{label}: lease signer stdin unavailable"))
        })?;
        stdin
            .write_all(&payload)
            .await
            .map_err(|e| LeaseMeterError::Backend(format!("{label}: write request: {e}")))?;
        // Drop closes stdin so the one-shot sidecar sees EOF.
    }

    // Past this point the request has left the process: any failure the
    // sidecar did not classify has to be read as possibly live.
    let output = child
        .wait_with_output()
        .await
        .map_err(|e| LeaseMeterError::Unresolved {
            message: format!("{label}: await lease signer: {e}"),
            tx_signature: None,
        })?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if let Ok(failure) = serde_json::from_str::<SidecarFailure>(stdout.trim()) {
            if failure.stage == "not_submitted" {
                return Err(LeaseMeterError::Backend(format!(
                    "{label}: lease signer refused: {}",
                    failure.error
                )));
            }
            return Err(LeaseMeterError::Unresolved {
                message: format!("{label}: {}", failure.error),
                tx_signature: failure.signature,
            });
        }
        return Err(LeaseMeterError::Unresolved {
            message: format!(
                "{label}: lease signer exited {} without naming a stage: {}",
                output.status,
                stderr.trim()
            ),
            tx_signature: None,
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let response: SidecarResponse =
        serde_json::from_str(stdout.trim()).map_err(|e| LeaseMeterError::Unresolved {
            message: format!("{label}: decode lease signer response: {e}"),
            tx_signature: None,
        })?;
    Ok(response.signature)
}

#[async_trait]
impl LeaseMeter for SidecarLeaseMeter {
    async fn open_and_delegate(&self, open: &LeaseOpen) -> Result<Option<String>, LeaseMeterError> {
        let job_id = open.job_id;
        if open.operator_payout_address.is_empty() {
            return Err(LeaseMeterError::Backend(format!(
                "job {job_id}: refusing to open — the assigned operator has no registered \
                 payout_address"
            )));
        }
        let decoded = bs58::decode(&open.operator_payout_address)
            .into_vec()
            .map_err(|e| {
                LeaseMeterError::Backend(format!(
                    "job {job_id}: operator payout address {:?} is not valid base58: {e}",
                    open.operator_payout_address
                ))
            })?;
        if decoded.len() != 32 {
            return Err(LeaseMeterError::Backend(format!(
                "job {job_id}: operator payout address {:?} decodes to {} bytes, not a 32-byte \
                 pubkey",
                open.operator_payout_address,
                decoded.len()
            )));
        }
        if open.rate_micro_usdc_per_sec == 0 || open.max_duration_secs == 0 {
            return Err(LeaseMeterError::Backend(format!(
                "job {job_id}: refusing to open a lease with no rate or no window"
            )));
        }
        if open.max_duration_secs > covenant_compute_protocol::MAX_LEASE_DURATION_SECS {
            return Err(LeaseMeterError::Backend(format!(
                "job {job_id}: window of {}s exceeds the {}s cap",
                open.max_duration_secs,
                covenant_compute_protocol::MAX_LEASE_DURATION_SECS
            )));
        }
        if !self.ledger.lock().claim_open(open) {
            return Err(LeaseMeterError::Backend(format!(
                "job {job_id}: a lease is already open or settled"
            )));
        }

        let mut request = self.envelope(job_id);
        request["renter"] = open.renter_pubkey_b58.clone().into();
        request["operator"] = open.operator_payout_address.clone().into();
        request["rate_micro_usdc_per_sec"] = open.rate_micro_usdc_per_sec.into();
        request["max_duration_secs"] = open.max_duration_secs.into();
        request["accepted_at_ms"] = open.accepted_at_ms.into();

        match run_signer(&self.config, "lease-open", job_id, request).await {
            Ok(signature) => Ok(Some(signature)),
            Err(e) => {
                // A definitively-refused open never minted a vault, so
                // release the claim and let a retry through. An
                // unresolved one may have: it stays claimed, which
                // stops a second funding attempt.
                if matches!(e, LeaseMeterError::Backend(_)) {
                    self.ledger.lock().open.remove(&job_id);
                }
                Err(e)
            }
        }
    }

    async fn tick(&self, observation: &LeaseObservation) -> Result<bool, LeaseMeterError> {
        let job_id = observation.job_id;
        if !self.ledger.lock().tickable(job_id) {
            return Ok(false);
        }
        let mut request = self.envelope(job_id);
        request["metered_ms"] = observation.elapsed_ms.into();
        request["receipt_hash_hex"] = hex_lower(&observation.receipt_hash()).into();
        run_signer(&self.config, "lease-tick", job_id, request).await?;
        Ok(true)
    }

    async fn undelegate_and_settle(
        &self,
        observation: &LeaseObservation,
    ) -> Result<Option<LeaseSettlement>, LeaseMeterError> {
        let job_id = observation.job_id;
        match self.claim_terminal(job_id, true).await? {
            Claim::Settled(settled) => return Ok(Some(settled)),
            Claim::Nothing => return Ok(None),
            Claim::Won => {}
        }
        let mut request = self.envelope(job_id);
        request["metered_ms"] = observation.elapsed_ms.into();
        request["receipt_hash_hex"] = hex_lower(&observation.receipt_hash()).into();
        self.run_terminal(
            "lease-conclude",
            job_id,
            request,
            observation.elapsed_ms,
            observation.charged_micro_usdc,
        )
        .await
    }

    async fn void(&self, job_id: Uuid) -> Result<Option<LeaseSettlement>, LeaseMeterError> {
        match self.claim_terminal(job_id, false).await? {
            Claim::Settled(settled) => return Ok(Some(settled)),
            Claim::Nothing => return Ok(None),
            Claim::Won => {}
        }
        // No elapsed rides a void: the program zeroes the charge whatever
        // the meter says, so the request is just the envelope naming the
        // lease.
        self.run_terminal("lease-void", job_id, self.envelope(job_id), 0, 0)
            .await
    }

    fn adopt(&self, open: &LeaseOpen) {
        self.ledger.lock().adopt(open);
    }

    fn describe(&self) -> String {
        format!(
            "sidecar (program {}, mint {}, rollup {})",
            self.config.program_id, self.config.mint, self.config.er_rpc_url
        )
    }
}

fn hex_lower(bytes: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(64);
    for byte in bytes {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

/// The coordinator-observed elapsed run of a lease at `now_ms` — the
/// one expression both meters bill from, and the same one
/// `submit_result` and the lease view already use. Saturating: a clock
/// that steps backwards reads zero elapsed, never a huge one.
pub fn observed_elapsed_ms(accepted_at_ms: u64, now_ms: u64) -> u64 {
    now_ms.saturating_sub(accepted_at_ms)
}

/// Builds the observation for one job record at `now_ms`.
///
/// `None` when the record is not a metered lease session: a different
/// job kind, a lease that was never accepted (no `t0`, so nothing to
/// meter), or a lease whose signed terms are unreadable — the fund path
/// never guesses a rate.
pub fn observe_lease(record: &JobRecord, now_ms: u64) -> Option<LeaseObservation> {
    if record.envelope.payload.kind != covenant_compute_protocol::JobKind::LeaseSession {
        return None;
    }
    let accepted_at_ms = record.accepted_at_ms?;
    let terms = covenant_compute_protocol::parse_lease_terms(&record.envelope.payload.input)
        .ok()
        .flatten()?;
    let elapsed_ms = observed_elapsed_ms(accepted_at_ms, now_ms);
    Some(LeaseObservation {
        job_id: record.envelope.payload.job_id,
        observed_at_ms: now_ms,
        elapsed_ms,
        charged_micro_usdc: terms.metered_micro_usdc(elapsed_ms),
        operator_pubkey_b58: record.operator_pubkey_b58.clone(),
        endpoint: record
            .lease_access
            .as_ref()
            .map(|a| a.endpoint.clone())
            .unwrap_or_default(),
        close_requested: record.close_requested_at_ms.is_some(),
    })
}

/// The same observation, built for a lease that is concluding on an
/// elapsed already decided by the caller — `submit_result` pins that
/// figure onto the record as its explanation of the charge, so the
/// on-chain meter has to commit the identical number or the two
/// disagree about what was billed.
pub fn observe_lease_at_elapsed(
    record: &JobRecord,
    now_ms: u64,
    elapsed_ms: u64,
) -> Option<LeaseObservation> {
    let terms = covenant_compute_protocol::parse_lease_terms(&record.envelope.payload.input)
        .ok()
        .flatten()?;
    let mut observation = observe_lease(record, now_ms)?;
    observation.elapsed_ms = elapsed_ms;
    observation.charged_micro_usdc = terms.metered_micro_usdc(elapsed_ms);
    Some(observation)
}

/// Opens and delegates the on-chain lease for a job the assignee just
/// accepted. A no-op when no meter is configured, and for every job
/// that is not a lease session.
///
/// A failure here is logged, never propagated: the accept has already
/// landed, the off-chain meter is already running, and refusing the
/// operator's acknowledgement after the fact would leave the job
/// stranded between two states. The session then runs off-chain only,
/// which the tick pass reports every cycle.
pub async fn open_lease_onchain(state: &CoordinatorState, job_id: Uuid) {
    let Some(meter) = state.lease_meter() else {
        return;
    };
    let Some(record) = state.jobs().get(job_id) else {
        return;
    };
    if record.envelope.payload.kind != covenant_compute_protocol::JobKind::LeaseSession {
        return;
    }
    let Some(accepted_at_ms) = record.accepted_at_ms else {
        return;
    };
    let terms = match covenant_compute_protocol::parse_lease_terms(&record.envelope.payload.input) {
        Ok(Some(terms)) => terms,
        Ok(None) | Err(_) => {
            tracing::error!(%job_id, "lease accepted with unreadable terms; no on-chain lease opened");
            return;
        }
    };
    let open = LeaseOpen {
        job_id,
        renter_pubkey_b58: record.envelope.payload.buyer.pubkey_base58(),
        operator_payout_address: record.payout_address.clone(),
        rate_micro_usdc_per_sec: terms.rate_micro_usdc_per_sec,
        max_duration_secs: terms.max_duration_secs,
        accepted_at_ms,
    };
    match meter.open_and_delegate(&open).await {
        Ok(signature) => tracing::info!(
            %job_id,
            funded_micro_usdc = open.funded_micro_usdc(),
            tx_signature = signature.as_deref().unwrap_or(""),
            "on-chain lease opened and delegated"
        ),
        Err(e) => tracing::error!(
            %job_id,
            error = %e,
            "on-chain lease open failed; the session runs on the off-chain meter only"
        ),
    }
}

/// Where concluding a lease on-chain left the operator's payout.
#[derive(Debug)]
pub enum LeaseConclusion {
    /// The vault has not paid the operator and can no longer pay them,
    /// or there is no on-chain lease at all: no meter configured, not a
    /// lease, none open for the job, or a conclude the signer refused
    /// after voiding the lease. The off-chain push is the payout.
    OffChain,
    /// The meter concluded. A `tx_signature` means the vault paid the
    /// operator and that settle is the payout.
    Settled(LeaseSettlement),
    /// The vault may have paid the operator. Pushing as well could pay
    /// twice, so the payout waits for someone to read the chain.
    Unresolved {
        message: String,
        tx_signature: Option<String>,
    },
}

/// Commits the meter and settles the vault for a lease that has
/// concluded, on the elapsed the record bills.
pub async fn conclude_lease_onchain(
    state: &CoordinatorState,
    record: &JobRecord,
    now_ms: u64,
    elapsed_ms: u64,
) -> LeaseConclusion {
    let Some(meter) = state.lease_meter() else {
        return LeaseConclusion::OffChain;
    };
    let Some(observation) = observe_lease_at_elapsed(record, now_ms, elapsed_ms) else {
        return LeaseConclusion::OffChain;
    };
    let job_id = observation.job_id;
    match meter.undelegate_and_settle(&observation).await {
        Ok(Some(settlement)) => {
            tracing::info!(
                %job_id,
                metered_ms = settlement.metered_ms,
                charged_micro_usdc = settlement.charged_micro_usdc,
                tx_signature = settlement.tx_signature.as_deref().unwrap_or(""),
                "on-chain lease settled"
            );
            LeaseConclusion::Settled(settlement)
        }
        Ok(None) => LeaseConclusion::OffChain,
        Err(LeaseMeterError::Backend(message)) => {
            tracing::error!(
                %job_id,
                error = %message,
                "on-chain lease settle refused; the off-chain payout stands"
            );
            LeaseConclusion::OffChain
        }
        Err(LeaseMeterError::Unresolved {
            message,
            tx_signature,
        }) => {
            tracing::error!(
                %job_id,
                error = %message,
                tx_signature = tx_signature.as_deref().unwrap_or(""),
                "on-chain lease settle outcome unknown; the payout is held until the chain is read"
            );
            LeaseConclusion::Unresolved {
                message,
                tx_signature,
            }
        }
    }
}

/// Holds a job's payout because its lease vault may already have paid
/// the operator. Opens the job's transfer bracket and suspends it, which
/// stops the push and every retry sweep until an admin reads the chain
/// and resolves it (`POST /admin/transfers/{id}/resolve`): confirmed
/// with the settle signature if the vault paid, not landed if it did not.
pub fn hold_payout_for_chain(
    state: &CoordinatorState,
    job_id: Uuid,
    payout_address: &str,
    amount_micro_usdc: u64,
    receipt: &covenant_compute_protocol::SignedWorkReceipt,
    detail: &str,
    tx_signature: Option<&str>,
) {
    use crate::attempts::BeginOutcome;
    use crate::journal::TransferAttemptKind;

    match state.attempts().begin(
        job_id,
        TransferAttemptKind::JobPayout,
        amount_micro_usdc,
        payout_address,
        &receipt.payout_memo(),
        crate::epoch_ms(),
    ) {
        Ok(BeginOutcome::Proceed | BeginOutcome::Open(_)) => {}
        Err(e) => {
            tracing::error!(
                %job_id,
                error = %e,
                "could not journal the held lease payout; a retry sweep may still push it"
            );
            return;
        }
    }
    let detail = format!("on-chain lease settle outcome unknown: {detail}");
    if let Err(e) = state.attempts().suspend(job_id, &detail, tx_signature) {
        tracing::error!(%job_id, error = %e, "could not journal the held lease payout");
    }
}

/// Voids the on-chain lease for a job the marketplace refunded in full —
/// a lapsed deadline, a failed receipt, an expiry sweep, or an operator
/// that rejected a lease it had accepted. The whole vault returns to the
/// renter and the operator earns nothing, so the on-chain charge matches
/// the off-chain refund instead of leaving a funded vault the operator
/// could still settle for the seconds the meter took before the refund.
///
/// A no-op when no meter is configured, for every job that is not a
/// lease session, and for a lease that never opened on-chain (an offer
/// swept before it was accepted). A failure is logged, never propagated:
/// the off-chain refund is authoritative, and an unvoided vault is an
/// operational incident to alert on, not a reason to fail the refund.
pub async fn void_lease_onchain(state: &CoordinatorState, record: &JobRecord) {
    let Some(meter) = state.lease_meter() else {
        return;
    };
    if record.envelope.payload.kind != covenant_compute_protocol::JobKind::LeaseSession {
        return;
    }
    let job_id = record.envelope.payload.job_id;
    match meter.void(job_id).await {
        Ok(Some(settlement)) => tracing::info!(
            %job_id,
            tx_signature = settlement.tx_signature.as_deref().unwrap_or(""),
            "on-chain lease voided; the whole vault returns to the renter"
        ),
        Ok(None) => {}
        Err(e) => tracing::error!(
            %job_id,
            error = %e,
            "on-chain lease void failed; the vault stays funded until it is voided by hand"
        ),
    }
}

/// Hands every lease still running back to the meter after a restart,
/// so its ticks resume and its conclusion still settles on-chain.
/// Returns how many were adopted.
pub fn adopt_live_leases(state: &CoordinatorState) -> usize {
    let Some(meter) = state.lease_meter() else {
        return 0;
    };
    let mut adopted = 0;
    for (job_id, record) in state.jobs().live_leases() {
        let Some(accepted_at_ms) = record.accepted_at_ms else {
            continue;
        };
        let Ok(Some(terms)) =
            covenant_compute_protocol::parse_lease_terms(&record.envelope.payload.input)
        else {
            continue;
        };
        meter.adopt(&LeaseOpen {
            job_id,
            renter_pubkey_b58: record.envelope.payload.buyer.pubkey_base58(),
            operator_payout_address: record.payout_address.clone(),
            rate_micro_usdc_per_sec: terms.rate_micro_usdc_per_sec,
            max_duration_secs: terms.max_duration_secs,
            accepted_at_ms,
        });
        adopted += 1;
    }
    adopted
}

/// Pushes one cumulative observation to every lease session the
/// coordinator currently sees running. Returns how many ticks the meter
/// accepted.
///
/// A skipped job is ordinary: a lease that never opened on-chain, or
/// one whose settlement is already in flight, is not tickable and says
/// so without erroring.
pub async fn tick_live_leases(state: &CoordinatorState, now_ms: u64) -> usize {
    let Some(meter) = state.lease_meter() else {
        return 0;
    };
    let mut ticked = 0usize;
    for (job_id, record) in state.jobs().live_leases() {
        let Some(observation) = observe_lease(&record, now_ms) else {
            continue;
        };
        match meter.tick(&observation).await {
            Ok(true) => ticked += 1,
            Ok(false) => {}
            Err(e) => tracing::warn!(
                %job_id,
                error = %e,
                elapsed_ms = observation.elapsed_ms,
                "lease meter tick failed; the next pass re-sends the cumulative total"
            ),
        }
    }
    ticked
}

/// Spawns the periodic tick pass, the same shape as
/// [`crate::sweep::spawn_periodic_payout_retry`]. Runs until the
/// returned handle is aborted or dropped; `main.rs` only starts one
/// when a deployment sets a cadence.
pub fn spawn_periodic_lease_meter(
    state: CoordinatorState,
    interval: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            let ticked = tick_live_leases(&state, crate::epoch_ms()).await;
            if ticked > 0 {
                tracing::debug!(count = ticked, "lease meter tick");
            }
        }
    })
}

/// Whether a record is a lease session the tick pass should meter:
/// accepted, with a `t0` to meter from. A buyer's close request does
/// not exclude it — the session is still running and still costing
/// them, and the meter's own conclusion fence is what stops a tick from
/// racing the settlement. Shared with
/// [`crate::jobs::JobBook::live_leases`] so the worklist and the
/// per-job check can never drift.
pub(crate) fn is_live_lease(record: &JobRecord) -> bool {
    matches!(record.phase, JobPhase::Accepted)
        && record.envelope.payload.kind == covenant_compute_protocol::JobKind::LeaseSession
        && record.accepted_at_ms.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
    use covenant_compute_protocol::{
        lease_input, CapabilityRequirement, EscrowHoldAttestation, FundingSource,
        JobEnvelopePayload, JobKind, LeaseAccess, LeaseTerms, SignedJobEnvelope,
    };
    use covenant_identity::LocalIdentity;
    use covenant_mcp::Content;

    #[test]
    fn elapsed_is_the_coordinator_clock_since_accept() {
        assert_eq!(observed_elapsed_ms(1_000, 4_500), 3_500);
        assert_eq!(observed_elapsed_ms(0, 0), 0);
    }

    #[test]
    fn a_clock_that_steps_backwards_reads_zero_not_an_enormous_run() {
        // Saturating, not wrapping: an NTP correction must never bill a
        // buyer for 584 million years.
        assert_eq!(observed_elapsed_ms(9_000, 1_000), 0);
    }

    #[test]
    fn the_tick_receipt_hash_is_stable_and_field_sensitive() {
        let base = LeaseObservation {
            job_id: Uuid::from_u128(1),
            observed_at_ms: 1_700_000_000_000,
            elapsed_ms: 12_000,
            charged_micro_usdc: 1_200,
            operator_pubkey_b58: "op".into(),
            endpoint: "ssh://host:22".into(),
            close_requested: false,
        };
        let hash = base.receipt_hash();
        assert_eq!(base.receipt_hash(), hash, "the same observation rehashes");

        for mutate in [
            (|o: &mut LeaseObservation| o.observed_at_ms += 1) as fn(&mut LeaseObservation),
            |o| o.elapsed_ms += 1,
            |o| o.charged_micro_usdc += 1,
            |o| o.operator_pubkey_b58 = "other".into(),
            |o| o.endpoint = "ssh://other:22".into(),
            |o| o.close_requested = true,
            |o| o.job_id = Uuid::from_u128(2),
        ] {
            let mut changed = base.clone();
            mutate(&mut changed);
            assert_ne!(
                changed.receipt_hash(),
                hash,
                "every field must reach the preimage"
            );
        }
    }

    #[test]
    fn field_boundaries_cannot_be_slid() {
        // Newline-joined, so no pair of adjacent fields can be shifted
        // into each other to forge a matching hash.
        let a = LeaseObservation {
            job_id: Uuid::from_u128(1),
            observed_at_ms: 1,
            elapsed_ms: 2,
            charged_micro_usdc: 3,
            operator_pubkey_b58: "ab".into(),
            endpoint: "c".into(),
            close_requested: false,
        };
        let mut b = a.clone();
        b.operator_pubkey_b58 = "a".into();
        b.endpoint = "bc".into();
        assert_ne!(a.receipt_hash(), b.receipt_hash());
    }

    #[tokio::test]
    async fn the_noop_meter_records_the_lifecycle_and_signs_nothing() {
        let meter = NoopLeaseMeter::new();
        let job_id = Uuid::from_u128(7);
        let open = LeaseOpen {
            job_id,
            renter_pubkey_b58: "buyer".into(),
            operator_payout_address: "operator".into(),
            rate_micro_usdc_per_sec: 100,
            max_duration_secs: 600,
            accepted_at_ms: 1_000,
        };
        assert_eq!(meter.open_and_delegate(&open).await.unwrap(), None);
        assert_eq!(open.funded_micro_usdc(), 60_000);

        let observation = LeaseObservation {
            job_id,
            observed_at_ms: 3_000,
            elapsed_ms: 2_000,
            charged_micro_usdc: 200,
            operator_pubkey_b58: "operator".into(),
            endpoint: String::new(),
            close_requested: false,
        };
        assert!(meter.tick(&observation).await.unwrap());

        let settlement = meter
            .undelegate_and_settle(&observation)
            .await
            .unwrap()
            .expect("an open lease concludes");
        assert_eq!(settlement.charged_micro_usdc, 200);
        assert_eq!(
            settlement.tx_signature, None,
            "a meter that moved no money must not claim a transaction — that flag is what \
             keeps the off-chain payout push in charge"
        );

        assert_eq!(meter.opened().len(), 1);
        assert_eq!(meter.ticks().len(), 1);
        assert_eq!(meter.concluded().len(), 1);
    }

    #[tokio::test]
    async fn a_second_open_for_one_job_is_refused() {
        let meter = NoopLeaseMeter::new();
        let open = LeaseOpen {
            job_id: Uuid::from_u128(9),
            renter_pubkey_b58: "buyer".into(),
            operator_payout_address: "operator".into(),
            rate_micro_usdc_per_sec: 1,
            max_duration_secs: 10,
            accepted_at_ms: 0,
        };
        meter.open_and_delegate(&open).await.unwrap();
        assert!(
            meter.open_and_delegate(&open).await.is_err(),
            "a lost-ack accept retry must not fund a second vault"
        );
        assert_eq!(meter.opened().len(), 1);
    }

    #[tokio::test]
    async fn ticks_stop_at_the_conclusion_and_a_second_settle_is_the_first() {
        let meter = NoopLeaseMeter::new();
        let job_id = Uuid::from_u128(11);
        let observation = LeaseObservation {
            job_id,
            observed_at_ms: 2_000,
            elapsed_ms: 1_000,
            charged_micro_usdc: 100,
            operator_pubkey_b58: "operator".into(),
            endpoint: String::new(),
            close_requested: false,
        };
        assert!(
            !meter.tick(&observation).await.unwrap(),
            "a job with no open lease is skipped, not an error"
        );

        meter
            .open_and_delegate(&LeaseOpen {
                job_id,
                renter_pubkey_b58: "buyer".into(),
                operator_payout_address: "operator".into(),
                rate_micro_usdc_per_sec: 100,
                max_duration_secs: 600,
                accepted_at_ms: 1_000,
            })
            .await
            .unwrap();
        assert!(meter.tick(&observation).await.unwrap());

        let first = meter
            .undelegate_and_settle(&observation)
            .await
            .unwrap()
            .unwrap();
        assert!(
            !meter.tick(&observation).await.unwrap(),
            "a settled lease is no longer tickable, so a periodic pass cannot overshoot the \
             figure the record pinned"
        );
        let second = meter
            .undelegate_and_settle(&observation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first, second, "a re-conclude returns the first settlement");
        assert_eq!(meter.concluded().len(), 1);
    }

    #[tokio::test]
    async fn a_void_charges_nothing_and_fences_the_lease_against_further_ticks() {
        let meter = NoopLeaseMeter::new();
        let job_id = Uuid::from_u128(21);
        let observation = LeaseObservation {
            job_id,
            observed_at_ms: 5_000,
            elapsed_ms: 4_000,
            charged_micro_usdc: 400,
            operator_pubkey_b58: "operator".into(),
            endpoint: String::new(),
            close_requested: false,
        };
        meter
            .open_and_delegate(&LeaseOpen {
                job_id,
                renter_pubkey_b58: "buyer".into(),
                operator_payout_address: "operator".into(),
                rate_micro_usdc_per_sec: 100,
                max_duration_secs: 600,
                accepted_at_ms: 1_000,
            })
            .await
            .unwrap();
        assert!(meter.tick(&observation).await.unwrap());

        let settlement = meter
            .void(job_id)
            .await
            .unwrap()
            .expect("an open lease voids");
        assert_eq!(
            settlement.charged_micro_usdc, 0,
            "a void pays the operator nothing, however long the meter ran"
        );
        assert_eq!(settlement.metered_ms, 0);
        assert_eq!(settlement.tx_signature, None);
        assert!(
            !meter.tick(&observation).await.unwrap(),
            "a voided lease is no longer tickable, so a periodic pass cannot re-charge it"
        );
        assert_eq!(meter.voided(), vec![job_id]);
    }

    #[tokio::test]
    async fn a_second_void_returns_the_first_and_a_void_after_settle_never_re_pays() {
        let meter = NoopLeaseMeter::new();
        let job_id = Uuid::from_u128(23);
        meter
            .open_and_delegate(&LeaseOpen {
                job_id,
                renter_pubkey_b58: "buyer".into(),
                operator_payout_address: "operator".into(),
                rate_micro_usdc_per_sec: 100,
                max_duration_secs: 600,
                accepted_at_ms: 0,
            })
            .await
            .unwrap();

        let first = meter.void(job_id).await.unwrap().unwrap();
        let second = meter.void(job_id).await.unwrap().unwrap();
        assert_eq!(first, second, "a re-void returns the first settlement");
        assert_eq!(meter.voided().len(), 1, "the second void moves no money");
    }

    #[tokio::test]
    async fn voiding_a_job_with_no_open_lease_is_skipped_not_an_error() {
        let meter = NoopLeaseMeter::new();
        assert_eq!(
            meter.void(Uuid::from_u128(29)).await.unwrap(),
            None,
            "a lease that never opened on-chain — an offer swept before accept — has no vault \
             to void"
        );
        assert!(meter.voided().is_empty());
    }

    #[test]
    fn a_conclusion_in_flight_fences_a_second_conclude_or_void() {
        // A conclude or a void holds no lock across its sidecar call, so a
        // second terminal call can land while the first is in flight — a
        // settle racing the deadline sweep's void. The fence must let only
        // the first through, or the program takes two terminal
        // instructions for one vault.
        let mut ledger = LeaseLedger::default();
        let job_id = Uuid::from_u128(31);
        let open = LeaseOpen {
            job_id,
            renter_pubkey_b58: "buyer".into(),
            operator_payout_address: "operator".into(),
            rate_micro_usdc_per_sec: 1,
            max_duration_secs: 10,
            accepted_at_ms: 0,
        };
        assert!(ledger.claim_open(&open));
        assert!(
            ledger.begin_conclude(job_id).is_some(),
            "the first terminal call claims the fence"
        );
        assert!(
            ledger.begin_conclude(job_id).is_none(),
            "a second call while the first is in flight concludes nothing"
        );
        // A definitive backend refusal abandons the fence, so a genuine
        // retry can still conclude the lease.
        ledger.abandon_conclude(job_id);
        assert!(
            ledger.begin_conclude(job_id).is_some(),
            "an abandoned conclusion reopens to a retry"
        );
    }

    #[tokio::test]
    async fn the_sidecar_meter_refuses_a_bad_payout_address_before_spawning_anything() {
        let meter = SidecarLeaseMeter::new(SidecarLeaseMeterConfig {
            // Deliberately absent: the guardrails must fire before any
            // process is spawned, so this path never needs a binary.
            signer_binary: PathBuf::from("/nonexistent/lease-signer"),
            program_id: "CLSeVNrRi4TpXsXAkAuLh58kGCCAd1w1bj2CcEhTEESd".into(),
            mint: "11111111111111111111111111111111".into(),
            rpc_url: "http://127.0.0.1:1".into(),
            er_rpc_url: "http://127.0.0.1:2".into(),
            er_validator: "MEUGGrYPxKk17hCr7wpT6s8dtNokZj5U2L57vjYMS8e".into(),
            renter_keypair_path: "/nonexistent/renter.json".into(),
            coordinator_keypair_path: "/nonexistent/coordinator.json".into(),
        });
        let base = LeaseOpen {
            job_id: Uuid::from_u128(13),
            renter_pubkey_b58: "buyer".into(),
            operator_payout_address: String::new(),
            rate_micro_usdc_per_sec: 100,
            max_duration_secs: 600,
            accepted_at_ms: 0,
        };
        let empty = meter.open_and_delegate(&base).await.unwrap_err();
        assert!(matches!(empty, LeaseMeterError::Backend(_)));
        assert!(empty.to_string().contains("payout_address"));

        let mut short = base.clone();
        short.operator_payout_address = "abc".into();
        let err = meter.open_and_delegate(&short).await.unwrap_err();
        assert!(err.to_string().contains("not a 32-byte pubkey"), "{err}");

        let mut free = base.clone();
        free.operator_payout_address = "11111111111111111111111111111111".into();
        free.rate_micro_usdc_per_sec = 0;
        let err = meter.open_and_delegate(&free).await.unwrap_err();
        assert!(err.to_string().contains("no rate or no window"), "{err}");

        let mut long = free.clone();
        long.rate_micro_usdc_per_sec = 1;
        long.max_duration_secs = covenant_compute_protocol::MAX_LEASE_DURATION_SECS + 1;
        let err = meter.open_and_delegate(&long).await.unwrap_err();
        assert!(err.to_string().contains("exceeds"), "{err}");

        assert!(
            meter.settlement_for(base.job_id).is_none(),
            "a refused open records nothing"
        );
    }

    #[tokio::test]
    async fn a_refused_open_leaves_no_claim_behind() {
        // A guardrail rejection minted no vault, so the job must stay
        // openable — otherwise a fixed registration could never be
        // retried.
        let meter = NoopLeaseMeter::new();
        let job_id = Uuid::from_u128(17);
        let open = LeaseOpen {
            job_id,
            renter_pubkey_b58: "buyer".into(),
            operator_payout_address: "operator".into(),
            rate_micro_usdc_per_sec: 100,
            max_duration_secs: 600,
            accepted_at_ms: 0,
        };
        meter.open_and_delegate(&open).await.unwrap();
        let observation = LeaseObservation {
            job_id,
            observed_at_ms: 1_000,
            elapsed_ms: 1_000,
            charged_micro_usdc: 100,
            operator_pubkey_b58: "operator".into(),
            endpoint: String::new(),
            close_requested: false,
        };
        meter.undelegate_and_settle(&observation).await.unwrap();
        assert!(
            meter.open_and_delegate(&open).await.is_err(),
            "a settled lease must never reopen under the same job id"
        );
    }

    #[test]
    fn hex_is_lowercase_and_fixed_width() {
        assert_eq!(hex_lower(&[0u8; 32]), "0".repeat(64));
        let mut bytes = [0u8; 32];
        bytes[31] = 0xab;
        assert!(hex_lower(&bytes).ends_with("ab"));
    }

    fn lease_terms() -> LeaseTerms {
        LeaseTerms {
            max_duration_secs: 600,
            rate_micro_usdc_per_sec: 50,
            client_public_key: None,
        }
    }

    /// A `JobRecord` shaped the way the coordinator actually holds a lease
    /// session — signed envelope, escrow hold, `Accepted` phase — so the
    /// observation builders run against a real record, not a hand-set
    /// `LeaseObservation`. `sign` performs no validation, so the guard
    /// cases (wrong kind, no accept, no lease block) can be built too.
    fn lease_job_record(
        kind: JobKind,
        input: Vec<Content>,
        accepted_at_ms: Option<u64>,
        endpoint: Option<&str>,
        close_requested_at_ms: Option<u64>,
    ) -> JobRecord {
        let buyer = LocalIdentity::generate("buyer@local");
        let coordinator = LocalIdentity::generate("coordinator@local");
        let job_id = Uuid::from_u128(0x1ea5e);
        let issued_at_ms = 1_000_000;
        let payload = JobEnvelopePayload {
            job_id,
            buyer: buyer.agent_id(),
            kind,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: None,
                kind,
                max_duration_secs: 600,
                min_reputation_bps: None,
            },
            input,
            price_micro_usdc: 30_000,
            deadline_ms: 10_000_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "onchain-meter-test"),
            issued_at_ms,
            referral_code: None,
            stream: false,
        };
        let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
        let escrow_hold = EscrowHoldAttestation::sign(
            job_id,
            30_000,
            FundingSource::Organic,
            issued_at_ms,
            &coordinator,
        )
        .unwrap();
        JobRecord {
            operator_pubkey_b58: "operator-pubkey".into(),
            payout_address: "operator-payout".into(),
            envelope,
            escrow_hold,
            phase: JobPhase::Accepted,
            receipt: None,
            output: None,
            fee_micro_usdc: 0,
            referral_code: None,
            partner_share_micro_usdc: 0,
            buyer_referral_code: None,
            buyer_partner_share_micro_usdc: 0,
            payout: None,
            concluded_at_ms: None,
            refund_reason: None,
            dispute: None,
            offered_at_ms: issued_at_ms,
            pinned: false,
            accepted_at_ms,
            metered_elapsed_ms: None,
            close_requested_at_ms,
            lease_access: endpoint.map(|endpoint| LeaseAccess {
                job_id,
                endpoint: endpoint.into(),
                ready_at_ms: issued_at_ms,
                note: None,
            }),
        }
    }

    fn accepted_lease(accepted_at_ms: u64) -> JobRecord {
        lease_job_record(
            JobKind::LeaseSession,
            vec![lease_input(lease_terms()).unwrap()],
            Some(accepted_at_ms),
            None,
            None,
        )
    }

    #[test]
    fn observe_lease_meters_the_run_since_accept() {
        let record = lease_job_record(
            JobKind::LeaseSession,
            vec![lease_input(lease_terms()).unwrap()],
            Some(1_000_000),
            Some("ssh://host:22"),
            Some(1_050_000),
        );
        let now = 1_120_000; // 120s after accept
        let obs = observe_lease(&record, now).expect("an accepted lease meters");

        assert_eq!(obs.observed_at_ms, now);
        assert_eq!(obs.elapsed_ms, 120_000);
        // The on-chain charge is the identical arithmetic the off-chain
        // books bill from the identical terms — byte-for-byte, or the two
        // meters disagree about what the buyer owes.
        assert_eq!(
            obs.charged_micro_usdc,
            lease_terms().metered_micro_usdc(120_000)
        );
        assert_eq!(obs.charged_micro_usdc, 6_000);
        assert_eq!(obs.operator_pubkey_b58, "operator-pubkey");
        assert_eq!(obs.endpoint, "ssh://host:22");
        assert!(
            obs.close_requested,
            "a pending close still meters — the session is still costing the buyer"
        );
    }

    #[test]
    fn observe_lease_leaves_the_endpoint_empty_until_the_node_publishes_one() {
        let obs = observe_lease(&accepted_lease(1_000_000), 1_001_000).unwrap();
        assert_eq!(
            obs.endpoint, "",
            "an unpublished session has no address to fold into the receipt"
        );
        assert!(!obs.close_requested);
    }

    #[test]
    fn observe_lease_never_bills_past_the_escrowed_window() {
        // 10_000s of wall clock against a 600s window.
        let obs = observe_lease(&accepted_lease(1_000_000), 1_000_000 + 10_000_000).unwrap();
        assert_eq!(
            obs.elapsed_ms, 10_000_000,
            "the observation still reports the real run"
        );
        let ceiling = lease_terms().rate_micro_usdc_per_sec * lease_terms().max_duration_secs;
        assert_eq!(
            obs.charged_micro_usdc, ceiling,
            "but the charge clamps to the whole escrowed window, never above it"
        );
        assert_eq!(obs.charged_micro_usdc, 30_000);
    }

    #[test]
    fn observe_lease_reads_zero_when_the_clock_stepped_backwards() {
        let obs = observe_lease(&accepted_lease(9_000_000), 1_000_000).unwrap();
        assert_eq!(obs.elapsed_ms, 0);
        assert_eq!(
            obs.charged_micro_usdc, 0,
            "a backwards NTP correction bills nothing, never a 584-million-year run"
        );
    }

    #[test]
    fn observe_lease_skips_anything_that_is_not_a_metered_lease() {
        let now = 2_000_000;

        let not_a_lease = lease_job_record(
            JobKind::BatchJob,
            vec![Content::text("work")],
            Some(1_000_000),
            None,
            None,
        );
        assert!(
            observe_lease(&not_a_lease, now).is_none(),
            "a batch job is not metered on the lease seam"
        );

        let never_accepted = lease_job_record(
            JobKind::LeaseSession,
            vec![lease_input(lease_terms()).unwrap()],
            None,
            None,
            None,
        );
        assert!(
            observe_lease(&never_accepted, now).is_none(),
            "no accept means no t0 to meter from"
        );

        let unreadable_terms = lease_job_record(
            JobKind::LeaseSession,
            vec![Content::text("no lease block")],
            Some(1_000_000),
            None,
            None,
        );
        assert!(
            observe_lease(&unreadable_terms, now).is_none(),
            "the fund path never guesses a rate for a lease it cannot read"
        );
    }

    #[test]
    fn observe_lease_at_elapsed_bills_the_pinned_figure_not_the_clock() {
        let record = accepted_lease(1_000_000);
        // The clock says 300s have passed, but settlement pinned 120s as the
        // run the off-chain books already billed. The on-chain meter has to
        // commit the pinned figure, or the two disagree about the charge.
        let now = 1_300_000;
        let pinned_elapsed = 120_000;

        let clocked = observe_lease(&record, now).unwrap();
        assert_eq!(
            clocked.elapsed_ms, 300_000,
            "the raw observation follows the clock"
        );

        let pinned = observe_lease_at_elapsed(&record, now, pinned_elapsed).unwrap();
        assert_eq!(
            pinned.observed_at_ms, now,
            "the observation is still stamped at now"
        );
        assert_eq!(
            pinned.elapsed_ms, pinned_elapsed,
            "but the metered run is the caller's pinned figure"
        );
        assert_eq!(
            pinned.charged_micro_usdc,
            lease_terms().metered_micro_usdc(pinned_elapsed)
        );
        assert_eq!(pinned.charged_micro_usdc, 6_000);
        assert_ne!(
            pinned.charged_micro_usdc, clocked.charged_micro_usdc,
            "pinning overrides the clock-derived charge, not just the elapsed"
        );
    }

    #[test]
    fn observe_lease_at_elapsed_inherits_the_lease_guards() {
        let never_accepted = lease_job_record(
            JobKind::LeaseSession,
            vec![lease_input(lease_terms()).unwrap()],
            None,
            None,
            None,
        );
        assert!(
            observe_lease_at_elapsed(&never_accepted, 2_000_000, 60_000).is_none(),
            "the pinning path cannot fabricate a charge for a lease that never accepted"
        );
    }

    /// A signer that answers every step with a signature and takes a
    /// second over each conclusion, counting them in a file beside itself.
    /// The coordinator clears the environment, so it names its tools by path.
    #[cfg(unix)]
    fn slow_signer(dir: &std::path::Path) -> SidecarLeaseMeterConfig {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("signer.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             /bin/cat > /dev/null\n\
             if [ \"$1\" = lease-conclude ]; then\n\
             \x20 echo x >> \"${0%/*}/concludes\"\n\
             \x20 /bin/sleep 1\n\
             fi\n\
             echo '{\"signature\":\"sig-'\"$1\"'\"}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        SidecarLeaseMeterConfig {
            signer_binary: script,
            program_id: "program".into(),
            mint: "mint".into(),
            rpc_url: "http://l1.invalid".into(),
            er_rpc_url: "http://rollup.invalid".into(),
            er_validator: "validator".into(),
            renter_keypair_path: "renter.json".into(),
            coordinator_keypair_path: "coordinator.json".into(),
        }
    }

    #[cfg(unix)]
    fn concludes(dir: &std::path::Path) -> usize {
        std::fs::read_to_string(dir.join("concludes"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    #[cfg(unix)]
    fn open_lease_for(job_id: Uuid) -> LeaseOpen {
        LeaseOpen {
            job_id,
            renter_pubkey_b58: "buyer".into(),
            operator_payout_address: "11111111111111111111111111111111".into(),
            rate_micro_usdc_per_sec: 100,
            max_duration_secs: 600,
            accepted_at_ms: 0,
        }
    }

    #[cfg(unix)]
    fn observation_for(job_id: Uuid) -> LeaseObservation {
        LeaseObservation {
            job_id,
            observed_at_ms: 1_000,
            elapsed_ms: 1_000,
            charged_micro_usdc: 100,
            operator_pubkey_b58: "operator".into(),
            endpoint: String::new(),
            close_requested: false,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_redelivered_conclusion_waits_for_the_one_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        let meter = SidecarLeaseMeter::new(slow_signer(dir.path()));
        let job_id = Uuid::from_u128(41);
        meter
            .open_and_delegate(&open_lease_for(job_id))
            .await
            .unwrap();
        let observation = observation_for(job_id);

        let (first, second) = tokio::join!(
            meter.undelegate_and_settle(&observation),
            meter.undelegate_and_settle(&observation)
        );
        let first = first.unwrap().expect("the first conclusion settles");
        let second = second
            .unwrap()
            .expect("the redelivery reports that settlement instead of falling back off chain");
        assert_eq!(first, second);
        assert_eq!(first.tx_signature.as_deref(), Some("sig-lease-conclude"));
        assert_eq!(concludes(dir.path()), 1, "one vault, one conclusion");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_conclusion_whose_caller_gave_up_still_lands() {
        let dir = tempfile::tempdir().unwrap();
        let meter = SidecarLeaseMeter::new(slow_signer(dir.path()));
        let job_id = Uuid::from_u128(42);
        meter
            .open_and_delegate(&open_lease_for(job_id))
            .await
            .unwrap();
        let observation = observation_for(job_id);

        // What happens to the handler awaiting this when the operator's
        // request times out.
        let dropped = tokio::time::timeout(
            Duration::from_millis(100),
            meter.undelegate_and_settle(&observation),
        )
        .await;
        assert!(dropped.is_err());

        let settled = meter
            .undelegate_and_settle(&observation)
            .await
            .unwrap()
            .expect("the retry reports the conclusion its dropped caller started");
        assert_eq!(settled.tx_signature.as_deref(), Some("sig-lease-conclude"));
        assert_eq!(meter.settlement_for(job_id), Some(settled));
        assert_eq!(concludes(dir.path()), 1);
    }

    #[tokio::test]
    async fn a_restart_hands_running_leases_back_to_the_meter() {
        let meter = Arc::new(NoopLeaseMeter::new());
        let identity = LocalIdentity::generate("coordinator@adopt");
        let audit: Arc<dyn covenant_audit::AuditLog> =
            Arc::new(covenant_audit::InMemoryAuditLog::new());
        let state = CoordinatorState::new(
            identity,
            crate::CoordinatorConfig {
                lease_meter: Some(meter.clone()),
                ..crate::CoordinatorConfig::default()
            },
            Arc::new(crate::NoReputation),
            Arc::new(crate::MockPayout::new()),
            audit,
        );
        let running = accepted_lease(1_000_000);
        let job_id = running.envelope.payload.job_id;
        state.jobs().insert(job_id, running.clone()).unwrap();
        let observation = observe_lease(&running, 1_060_000).unwrap();
        assert!(
            !meter.tick(&observation).await.unwrap(),
            "a fresh process knows no lease until it adopts the running ones"
        );

        assert_eq!(adopt_live_leases(&state), 1);
        assert!(meter.tick(&observation).await.unwrap(), "ticks resume");
        let settled = meter.undelegate_and_settle(&observation).await.unwrap();
        assert!(settled.is_some(), "and the conclusion settles on-chain");
        assert!(
            meter.opened().is_empty(),
            "adopting opens nothing: the vault already exists"
        );

        assert_eq!(adopt_live_leases(&state), 1);
        assert!(
            !meter.tick(&observation).await.unwrap(),
            "adopting again never reopens a lease that already settled"
        );
    }
}
