//! Durable coordinator state: an append-only JSONL journal the job
//! book and escrow write through, replayed at boot — a restart must
//! not lose escrow holds or in-flight jobs (the C2 seam flagged in
//! build-notes-phase1-coordinator.md). Same file idiom as
//! `covenant-audit`'s `JsonlAuditLog`: one JSON object per line, append
//! only, no rewrite-in-place.
//!
//! Entries are whole-state upserts keyed by `job_id` (last write wins
//! on replay), not deltas — replay can never diverge from what the
//! in-memory maps held, at the cost of re-writing a job's record on
//! each phase change. [`Journal::compact`] reclaims that cost: it
//! rewrites the file to one line per surviving fact (the C2 tail), so
//! steady-state size tracks the number of jobs ever seen, not the
//! number of phase changes.
//!
//! What deliberately does NOT persist: the operator registry (nodes
//! re-register on heartbeat rejection — the proven self-heal — and
//! session tokens should rotate on coordinator restart) and pending
//! long-poll queues (an offer lost in delivery is refunded by the
//! deadline sweep; a node already holding the offer can still submit
//! its result against the restored job record).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use covenant_compute_protocol::{EscrowStatus, FundingSource};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::accounts::WithdrawalState;
use crate::bond::{SlashRecord, UnbondState};
use crate::jobs::JobRecord;

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("journal io: {0}")]
    Io(#[from] std::io::Error),
    #[error("journal line {line} is corrupt: {source}")]
    Corrupt {
        line: usize,
        source: serde_json::Error,
    },
    #[error("journal encode: {0}")]
    Encode(serde_json::Error),
}

/// One escrow hold's durable state — mirrors the private `Hold` the
/// escrow keeps in memory. The buyer attribution is what lets a
/// balance be *derived* from the holds (deposits minus non-refunded
/// organic holds) instead of double-written: every fund transition
/// stays a single journal line, so there is no crash window where a
/// hold settled but its balance effect was lost. Empty on entries
/// journaled before buyer accounting existed — such holds are
/// unattributed and never count against anyone's balance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscrowHoldState {
    pub amount_micro_usdc: u64,
    pub funding_source: FundingSource,
    pub status: EscrowStatus,
    #[serde(default)]
    pub buyer_pubkey_b58: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum JournalEntry {
    Job {
        job_id: Uuid,
        record: Box<JobRecord>,
    },
    Escrow {
        job_id: Uuid,
        #[serde(flatten)]
        hold: EscrowHoldState,
    },
    /// An event, not an upsert: replay unions ids into the seen-set
    /// (the idempotency guard) and sums amounts into the buyer's
    /// deposited total.
    Deposit {
        deposit_id: String,
        buyer_pubkey_b58: String,
        amount_micro_usdc: u64,
    },
    /// Same event shape for the outflow side of the rev-share books: a
    /// recorded (out-of-band) partner payout, idempotent by
    /// `payout_id`, summed into the code's paid total on replay.
    PartnerPayout {
        payout_id: String,
        referral_code: String,
        amount_micro_usdc: u64,
    },
    /// An upsert like `Job`/`Escrow`: the whole withdrawal record,
    /// written at debit and again when a backend transfer honors it.
    /// Replay keeps the latest state per withdrawal id.
    Withdrawal {
        #[serde(flatten)]
        state: WithdrawalState,
    },
    /// An event like `Deposit`, for the stake side: a rail-verified
    /// bond post, idempotent by `bond_id`, summed into the operator's
    /// posted total on replay.
    BondPost {
        bond_id: String,
        operator_pubkey_b58: String,
        amount_micro_usdc: u64,
    },
    /// An event: one coordinator-proven slash, idempotent by its
    /// deterministic `slash_id`.
    BondSlash {
        #[serde(flatten)]
        record: SlashRecord,
    },
    /// An upsert like `Withdrawal`: the whole unbond record, written at
    /// request and again when the matured refund pushes.
    Unbond {
        #[serde(flatten)]
        state: UnbondState,
    },
    /// A one-way latch, not an upsert: the admin closed the bootstrap
    /// subsidy at runtime. Replay keeps the first close it sees, and
    /// once a journal holds this line no boot re-arms the subsidy from
    /// its environment — that silent re-arm is exactly what journaling
    /// the close exists to prevent.
    SubsidyClosed { closed_at_ms: u64 },
    /// An upsert keyed by `attempt_id`: the durable bracket around one
    /// on-chain push. `Attempted` is written and fsynced BEFORE the
    /// signer is spawned; `Resolved`/`Cleared` after the outcome is
    /// known. An `Attempted` line with no later state is the crash
    /// window a blind retry would double-spend — replay surfaces it so
    /// boot can suspend the obligation instead.
    TransferAttempt {
        #[serde(flatten)]
        state: TransferAttemptState,
    },
    /// An upsert keyed by `slash_id`: one proven fault's take from the
    /// operator's on-chain CVNT stake. `Attempted` is written before the
    /// stake signer is spawned, then `Slashed` or `Refused`. The latest
    /// state per id is the dedup record, so a replayed verdict never
    /// slashes twice.
    StakeSlash {
        #[serde(flatten)]
        state: StakeSlashState,
    },
}

/// The obligation class behind a transfer attempt — which book the
/// reconciler completes when a suspended transfer turns out to have
/// landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferAttemptKind {
    JobPayout,
    Withdrawal,
    UnbondRefund,
}

impl TransferAttemptKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            TransferAttemptKind::JobPayout => "job_payout",
            TransferAttemptKind::Withdrawal => "withdrawal",
            TransferAttemptKind::UnbondRefund => "unbond_refund",
        }
    }
}

/// Where one transfer attempt stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferAttemptStatus {
    /// The signer may have been spawned; the outcome is not yet
    /// durable. Terminal only after a crash — and then it means
    /// "suspend and reconcile", never "retry".
    Attempted,
    /// The transfer definitively landed (confirmed signature, or an
    /// admin confirmed it on-chain).
    Resolved,
    /// The transfer definitively did not land; the obligation is free
    /// to retry.
    Cleared,
}

/// One transfer attempt's durable state — see
/// [`JournalEntry::TransferAttempt`]. `attempt_id` is the obligation's
/// own id (job id, withdrawal id, unbond id), so at most one attempt
/// bracket exists per obligation and re-attempting after a `Cleared`
/// overwrites in place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferAttemptState {
    pub attempt_id: Uuid,
    /// Serialized as `obligation_kind`: this struct is flattened into
    /// [`JournalEntry`], whose own discriminant already claims `kind`.
    #[serde(rename = "obligation_kind")]
    pub kind: TransferAttemptKind,
    pub amount_micro_usdc: u64,
    pub recipient_address_b58: String,
    /// The on-chain memo the transfer carries — what an operator (or a
    /// chain query) reconciles a suspended attempt against.
    pub memo: String,
    pub attempted_at_ms: u64,
    pub status: TransferAttemptStatus,
    #[serde(default)]
    pub tx_signature: Option<String>,
    /// The last failure message, kept for the reconciling operator.
    #[serde(default)]
    pub detail: String,
}

/// Where one on-chain stake slash stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StakeSlashStatus {
    /// The signer may have been spawned and the outcome is not durable.
    /// Boot drives it again; the slash's memo makes that idempotent.
    Attempted,
    Slashed,
    /// The signer submitted nothing: no live stake, or the chain refused.
    Refused,
}

/// One stake slash's durable state — see [`JournalEntry::StakeSlash`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StakeSlashState {
    pub slash_id: String,
    pub operator_pubkey_b58: String,
    pub job_id: Uuid,
    /// Requested take, in the stake mint's base units. What landed is
    /// in the audit row.
    pub amount: u64,
    pub reason: String,
    pub status: StakeSlashStatus,
    #[serde(default)]
    pub tx_signature: Option<String>,
    #[serde(default)]
    pub detail: String,
}

/// Everything `Journal::load` recovered, ready to seed the in-memory
/// maps.
#[derive(Default)]
pub struct RestoredState {
    pub jobs: HashMap<Uuid, JobRecord>,
    pub holds: HashMap<Uuid, EscrowHoldState>,
    pub deposit_totals: HashMap<String, u64>,
    pub deposit_ids: HashSet<String>,
    pub partner_paid_totals: HashMap<String, u64>,
    pub partner_payout_ids: HashSet<String>,
    pub withdrawals: HashMap<Uuid, WithdrawalState>,
    pub bond_totals: HashMap<String, u64>,
    pub bond_ids: HashSet<String>,
    pub bond_slashes: Vec<SlashRecord>,
    pub unbonds: HashMap<Uuid, UnbondState>,
    /// When the subsidy kill-switch was closed at runtime, if ever —
    /// the first close on record.
    pub subsidy_closed_at_ms: Option<u64>,
    /// Latest state per transfer attempt. Ones still `Attempted` are
    /// crash windows for boot to suspend or auto-resolve.
    pub transfer_attempts: HashMap<Uuid, TransferAttemptState>,
    /// Latest state per stake slash.
    pub stake_slashes: HashMap<String, StakeSlashState>,
}

/// Append-side handle, shared by the job book and the escrow. Writes
/// are line-atomic under the mutex and fsynced before returning: a
/// mutation that can't be made durable fails loudly rather than
/// letting the in-memory state silently outrun the file — for fund
/// state, "maybe persisted" is worse than "down". The fsync is what
/// makes an acked fund transition survive an OS crash, not just a
/// process kill; appends happen per job phase change, so its
/// per-write cost is noise next to the transition's HTTP round-trip.
pub struct Journal {
    file: Mutex<File>,
    path: PathBuf,
    /// Whether the last fund-transition append reached the disk. A
    /// coordinator that can no longer persist is, by this journal's own
    /// contract, worse than down — every money mutation fails loudly
    /// rather than commit to memory alone. This flag lets `/health`
    /// report that state so a monitored deployment restarts or reroutes
    /// instead of sitting up while refusing all real work. It tracks the
    /// most recent write, so a transient volume problem that clears heals
    /// the signal on the next successful append.
    healthy: AtomicBool,
}

/// What one [`Journal::compact`] pass did, for the boot log and the
/// periodic task's telemetry.
#[derive(Debug, Clone, Copy)]
pub struct CompactStats {
    pub entries_before: usize,
    pub entries_after: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

impl CompactStats {
    pub fn shrank(&self) -> bool {
        self.entries_after < self.entries_before
    }
}

/// Bytes up to and including the last newline — everything after it is
/// a torn write, since the writer terminates every record with `\n`.
fn valid_prefix_len(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .rposition(|&b| b == b'\n')
        .map(|p| p + 1)
        .unwrap_or(0)
}

/// Parses newline-terminated journal bytes, skipping blank lines. A
/// line that fails to parse is real damage: surfaced with its 1-based
/// line number, never skipped.
fn parse_lines(bytes: &[u8]) -> impl Iterator<Item = Result<JournalEntry, JournalError>> + '_ {
    bytes
        .split(|&b| b == b'\n')
        .enumerate()
        .filter(|(_, line)| !line.iter().all(u8::is_ascii_whitespace))
        .map(|(i, line)| {
            serde_json::from_slice(line).map_err(|e| JournalError::Corrupt {
                line: i + 1,
                source: e,
            })
        })
}

impl Journal {
    /// Opens the append handle. Every record the writer produces ends
    /// in `\n`, so trailing bytes without one can only be a torn write
    /// from a crash mid-append — they are truncated away here so the
    /// next append starts a fresh line instead of gluing onto garbage
    /// (which would read as mid-file corruption on the replay after
    /// that).
    pub fn open(path: &Path) -> Result<Self, JournalError> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let valid_len = valid_prefix_len(&bytes);
                if valid_len < bytes.len() {
                    tracing::warn!(
                        dropped_bytes = bytes.len() - valid_len,
                        "truncating torn final journal record"
                    );
                    let f = OpenOptions::new().write(true).open(path)?;
                    f.set_len(valid_len as u64)?;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: Mutex::new(file),
            path: path.to_path_buf(),
            healthy: AtomicBool::new(true),
        })
    }

    /// Whether the last durable append succeeded — `false` once a
    /// fund-transition write fails to reach the disk, back to `true`
    /// after the next one that does. `/health` and `/metrics` read this
    /// so an operator's monitoring sees a coordinator that has stopped
    /// being able to persist.
    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Relaxed)
    }

    /// The journal file's size on disk, for a `/metrics` gauge an
    /// operator can alert on. It sawtooths: every append grows it, each
    /// compaction shrinks it back to one line per surviving fact. A
    /// figure that climbs without falling is the early signal that
    /// compaction has stopped keeping up, well before the volume fills
    /// and appends start failing. `None` if the file can't be stat'd.
    pub fn size_bytes(&self) -> Option<u64> {
        std::fs::metadata(&self.path).map(|m| m.len()).ok()
    }

    /// Replays `path` into a [`RestoredState`]. Trailing bytes after
    /// the last newline are a torn write from a crash mid-append and
    /// are dropped (that mutation never happened as far as any caller
    /// was told — the write errored); a newline-terminated line that
    /// fails to parse is real damage and refuses to load.
    pub fn load(path: &Path) -> Result<RestoredState, JournalError> {
        let mut restored = RestoredState::default();
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(restored),
            Err(e) => return Err(e.into()),
        };
        let valid_len = valid_prefix_len(&bytes);
        if valid_len < bytes.len() {
            tracing::warn!(
                dropped_bytes = bytes.len() - valid_len,
                "dropping torn final journal record"
            );
        }
        for entry in parse_lines(&bytes[..valid_len]) {
            match entry? {
                JournalEntry::Job { job_id, record } => {
                    restored.jobs.insert(job_id, *record);
                }
                JournalEntry::Escrow { job_id, hold } => {
                    restored.holds.insert(job_id, hold);
                }
                JournalEntry::Deposit {
                    deposit_id,
                    buyer_pubkey_b58,
                    amount_micro_usdc,
                } => {
                    if restored.deposit_ids.insert(deposit_id) {
                        let total = restored.deposit_totals.entry(buyer_pubkey_b58).or_default();
                        *total = total.saturating_add(amount_micro_usdc);
                    }
                }
                JournalEntry::PartnerPayout {
                    payout_id,
                    referral_code,
                    amount_micro_usdc,
                } => {
                    if restored.partner_payout_ids.insert(payout_id) {
                        let total = restored
                            .partner_paid_totals
                            .entry(referral_code)
                            .or_default();
                        *total = total.saturating_add(amount_micro_usdc);
                    }
                }
                JournalEntry::Withdrawal { state } => {
                    restored.withdrawals.insert(state.withdrawal_id, state);
                }
                JournalEntry::BondPost {
                    bond_id,
                    operator_pubkey_b58,
                    amount_micro_usdc,
                } => {
                    if restored.bond_ids.insert(bond_id) {
                        let total = restored.bond_totals.entry(operator_pubkey_b58).or_default();
                        *total = total.saturating_add(amount_micro_usdc);
                    }
                }
                JournalEntry::BondSlash { record } => {
                    if !restored
                        .bond_slashes
                        .iter()
                        .any(|s| s.slash_id == record.slash_id)
                    {
                        restored.bond_slashes.push(record);
                    }
                }
                JournalEntry::Unbond { state } => {
                    restored.unbonds.insert(state.unbond_id, state);
                }
                JournalEntry::SubsidyClosed { closed_at_ms } => {
                    restored.subsidy_closed_at_ms.get_or_insert(closed_at_ms);
                }
                JournalEntry::TransferAttempt { state } => {
                    restored.transfer_attempts.insert(state.attempt_id, state);
                }
                JournalEntry::StakeSlash { state } => {
                    restored.stake_slashes.insert(state.slash_id.clone(), state);
                }
            }
        }
        Ok(restored)
    }

    pub fn record_job(&self, job_id: Uuid, record: &JobRecord) -> Result<(), JournalError> {
        self.append(&JournalEntry::Job {
            job_id,
            record: Box::new(record.clone()),
        })
    }

    pub fn record_escrow(&self, job_id: Uuid, hold: &EscrowHoldState) -> Result<(), JournalError> {
        self.append(&JournalEntry::Escrow {
            job_id,
            hold: hold.clone(),
        })
    }

    pub fn record_deposit(
        &self,
        deposit_id: &str,
        buyer_pubkey_b58: &str,
        amount_micro_usdc: u64,
    ) -> Result<(), JournalError> {
        self.append(&JournalEntry::Deposit {
            deposit_id: deposit_id.into(),
            buyer_pubkey_b58: buyer_pubkey_b58.into(),
            amount_micro_usdc,
        })
    }

    pub fn record_partner_payout(
        &self,
        payout_id: &str,
        referral_code: &str,
        amount_micro_usdc: u64,
    ) -> Result<(), JournalError> {
        self.append(&JournalEntry::PartnerPayout {
            payout_id: payout_id.into(),
            referral_code: referral_code.into(),
            amount_micro_usdc,
        })
    }

    pub fn record_withdrawal(&self, state: &WithdrawalState) -> Result<(), JournalError> {
        self.append(&JournalEntry::Withdrawal {
            state: state.clone(),
        })
    }

    pub fn record_bond_post(
        &self,
        bond_id: &str,
        operator_pubkey_b58: &str,
        amount_micro_usdc: u64,
    ) -> Result<(), JournalError> {
        self.append(&JournalEntry::BondPost {
            bond_id: bond_id.into(),
            operator_pubkey_b58: operator_pubkey_b58.into(),
            amount_micro_usdc,
        })
    }

    pub fn record_bond_slash(&self, record: &SlashRecord) -> Result<(), JournalError> {
        self.append(&JournalEntry::BondSlash {
            record: record.clone(),
        })
    }

    pub fn record_unbond(&self, state: &UnbondState) -> Result<(), JournalError> {
        self.append(&JournalEntry::Unbond {
            state: state.clone(),
        })
    }

    pub fn record_subsidy_closed(&self, closed_at_ms: u64) -> Result<(), JournalError> {
        self.append(&JournalEntry::SubsidyClosed { closed_at_ms })
    }

    pub fn record_transfer_attempt(
        &self,
        state: &TransferAttemptState,
    ) -> Result<(), JournalError> {
        self.append(&JournalEntry::TransferAttempt {
            state: state.clone(),
        })
    }

    pub fn record_stake_slash(&self, state: &StakeSlashState) -> Result<(), JournalError> {
        self.append(&JournalEntry::StakeSlash {
            state: state.clone(),
        })
    }

    fn append(&self, entry: &JournalEntry) -> Result<(), JournalError> {
        let mut line = serde_json::to_vec(entry).map_err(JournalError::Encode)?;
        line.push(b'\n');
        let mut file = self.file.lock();
        // `File::flush` is a no-op (writes already sit in the kernel);
        // only fdatasync moves them to disk. Without it, a power loss
        // could silently drop fund transitions the coordinator already
        // acked to buyers and operators.
        let written = file.write_all(&line).and_then(|()| file.sync_data());
        // A failed encode can't happen for these types and says nothing
        // about the volume, so only the write+fsync outcome moves the
        // health flag.
        self.healthy.store(written.is_ok(), Ordering::Relaxed);
        written?;
        Ok(())
    }

    /// Rewrites the journal to one line per surviving fact — the
    /// latest upsert per job and hold, one line per deposit and
    /// partner-payout id (ids are the replay idempotency guard, so
    /// every id survives with its first-seen amount, exactly what
    /// replay honors). [`Journal::load`] of the compacted file
    /// recovers the identical [`RestoredState`].
    ///
    /// Runs entirely under the append mutex: writers block for the one
    /// pass instead of racing it. The rewrite lands in a temp file
    /// that is fsynced and atomically renamed over the journal, and
    /// the append handle swaps to the new file only after the rename —
    /// a crash or failure at any step leaves the original journal
    /// intact and the handle still pointed at it.
    pub fn compact(&self) -> Result<CompactStats, JournalError> {
        let mut guard = self.file.lock();

        let bytes = std::fs::read(&self.path)?;
        // Holding the only writer (every append is a full flushed
        // line), so a torn tail can only be pre-open damage that
        // `open` already truncated; the guard here is belt and braces.
        let valid = &bytes[..valid_prefix_len(&bytes)];
        let mut jobs: BTreeMap<Uuid, Box<JobRecord>> = BTreeMap::new();
        let mut holds: BTreeMap<Uuid, EscrowHoldState> = BTreeMap::new();
        let mut deposits: BTreeMap<String, (String, u64)> = BTreeMap::new();
        let mut partner_payouts: BTreeMap<String, (String, u64)> = BTreeMap::new();
        let mut withdrawals: BTreeMap<Uuid, WithdrawalState> = BTreeMap::new();
        let mut bond_posts: BTreeMap<String, (String, u64)> = BTreeMap::new();
        let mut bond_slashes: BTreeMap<String, SlashRecord> = BTreeMap::new();
        let mut unbonds: BTreeMap<Uuid, UnbondState> = BTreeMap::new();
        let mut subsidy_closed_at_ms: Option<u64> = None;
        let mut transfer_attempts: BTreeMap<Uuid, TransferAttemptState> = BTreeMap::new();
        let mut stake_slashes: BTreeMap<String, StakeSlashState> = BTreeMap::new();
        let mut entries_before = 0usize;
        for entry in parse_lines(valid) {
            entries_before += 1;
            match entry? {
                JournalEntry::Job { job_id, record } => {
                    jobs.insert(job_id, record);
                }
                JournalEntry::Escrow { job_id, hold } => {
                    holds.insert(job_id, hold);
                }
                JournalEntry::Deposit {
                    deposit_id,
                    buyer_pubkey_b58,
                    amount_micro_usdc,
                } => {
                    deposits
                        .entry(deposit_id)
                        .or_insert((buyer_pubkey_b58, amount_micro_usdc));
                }
                JournalEntry::PartnerPayout {
                    payout_id,
                    referral_code,
                    amount_micro_usdc,
                } => {
                    partner_payouts
                        .entry(payout_id)
                        .or_insert((referral_code, amount_micro_usdc));
                }
                JournalEntry::Withdrawal { state } => {
                    withdrawals.insert(state.withdrawal_id, state);
                }
                JournalEntry::BondPost {
                    bond_id,
                    operator_pubkey_b58,
                    amount_micro_usdc,
                } => {
                    bond_posts
                        .entry(bond_id)
                        .or_insert((operator_pubkey_b58, amount_micro_usdc));
                }
                JournalEntry::BondSlash { record } => {
                    bond_slashes
                        .entry(record.slash_id.clone())
                        .or_insert(record);
                }
                JournalEntry::Unbond { state } => {
                    unbonds.insert(state.unbond_id, state);
                }
                JournalEntry::SubsidyClosed { closed_at_ms } => {
                    subsidy_closed_at_ms.get_or_insert(closed_at_ms);
                }
                JournalEntry::TransferAttempt { state } => {
                    transfer_attempts.insert(state.attempt_id, state);
                }
                JournalEntry::StakeSlash { state } => {
                    stake_slashes.insert(state.slash_id.clone(), state);
                }
            }
        }

        let mut buf = Vec::with_capacity(bytes.len() / 2);
        let mut entries_after = 0usize;
        let mut push = |entry: &JournalEntry| -> Result<(), JournalError> {
            let line = serde_json::to_vec(entry).map_err(JournalError::Encode)?;
            buf.extend_from_slice(&line);
            buf.push(b'\n');
            entries_after += 1;
            Ok(())
        };
        for (job_id, record) in jobs {
            push(&JournalEntry::Job { job_id, record })?;
        }
        for (job_id, hold) in holds {
            push(&JournalEntry::Escrow { job_id, hold })?;
        }
        for (deposit_id, (buyer_pubkey_b58, amount_micro_usdc)) in deposits {
            push(&JournalEntry::Deposit {
                deposit_id,
                buyer_pubkey_b58,
                amount_micro_usdc,
            })?;
        }
        for (payout_id, (referral_code, amount_micro_usdc)) in partner_payouts {
            push(&JournalEntry::PartnerPayout {
                payout_id,
                referral_code,
                amount_micro_usdc,
            })?;
        }
        for (_, state) in withdrawals {
            push(&JournalEntry::Withdrawal { state })?;
        }
        for (bond_id, (operator_pubkey_b58, amount_micro_usdc)) in bond_posts {
            push(&JournalEntry::BondPost {
                bond_id,
                operator_pubkey_b58,
                amount_micro_usdc,
            })?;
        }
        for (_, record) in bond_slashes {
            push(&JournalEntry::BondSlash { record })?;
        }
        for (_, state) in unbonds {
            push(&JournalEntry::Unbond { state })?;
        }
        if let Some(closed_at_ms) = subsidy_closed_at_ms {
            push(&JournalEntry::SubsidyClosed { closed_at_ms })?;
        }
        // Only unresolved brackets survive: a resolved or cleared
        // attempt's fact of record lives on the obligation itself (the
        // job's payout, the withdrawal's push, the unbond's push).
        for (_, state) in transfer_attempts {
            if state.status == TransferAttemptStatus::Attempted {
                push(&JournalEntry::TransferAttempt { state })?;
            }
        }
        // Every slash survives, settled or not: its id is what keeps a
        // replayed verdict from taking the stake twice.
        for (_, state) in stake_slashes {
            push(&JournalEntry::StakeSlash { state })?;
        }

        let tmp_path = self.path.with_extension("jsonl.compacting");
        let mut tmp = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)?;
        tmp.write_all(&buf)?;
        tmp.sync_all()?;
        std::fs::rename(&tmp_path, &self.path)?;
        // The handle followed the rename to the journal's name; its
        // cursor sits at end-of-file, and the mutex keeps it the only
        // writer, so appends continue exactly like the O_APPEND handle
        // it replaces.
        *guard = tmp;

        Ok(CompactStats {
            entries_before,
            entries_after,
            bytes_before: bytes.len() as u64,
            bytes_after: buf.len() as u64,
        })
    }
}

/// Spawns a background task compacting `journal` every `interval` —
/// what `main.rs` runs so a long-lived coordinator's journal tracks
/// its job count, not its phase-change count. Failures are logged and
/// retried next tick; the journal stays correct either way, only
/// bigger.
pub fn spawn_periodic_compaction(
    journal: std::sync::Arc<Journal>,
    interval: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // The boot pass already ran inside `with_journal`.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            match journal.compact() {
                Ok(stats) if stats.shrank() => tracing::info!(
                    entries_before = stats.entries_before,
                    entries_after = stats.entries_after,
                    bytes_before = stats.bytes_before,
                    bytes_after = stats.bytes_after,
                    "journal compacted"
                ),
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "journal compaction failed; retrying next tick")
                }
            }
        }
    })
}

#[cfg(test)]
impl Journal {
    /// A journal backed by a read-only handle: every append fails with
    /// `EBADF`, the same shape a full or unwritable volume produces on
    /// the live path. Lets any module in the crate drive its
    /// journal-write-failure branch without reaching into these fields.
    pub(crate) fn read_only_for_test(path: &Path) -> Self {
        std::fs::File::create(path).unwrap();
        Self {
            file: Mutex::new(OpenOptions::new().read(true).open(path).unwrap()),
            path: path.to_path_buf(),
            healthy: AtomicBool::new(true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::JobPhase;
    use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
    use covenant_compute_protocol::{
        CapabilityRequirement, EscrowHoldAttestation, JobEnvelopePayload, JobKind,
        SignedJobEnvelope,
    };
    use covenant_identity::LocalIdentity;
    use covenant_mcp::Content;

    fn record(job_id: Uuid) -> JobRecord {
        let buyer = LocalIdentity::generate("buyer@journal");
        let coordinator = LocalIdentity::generate("coordinator@journal");
        let payload = JobEnvelopePayload {
            job_id,
            buyer: buyer.agent_id(),
            kind: JobKind::BatchJob,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: None,
                kind: JobKind::BatchJob,
                max_duration_secs: 5,
                min_reputation_bps: None,
            },
            input: vec![Content::text("work")],
            price_micro_usdc: 100,
            deadline_ms: 5_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "journal-test"),
            issued_at_ms: 0,
            referral_code: None,
            stream: false,
        };
        let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
        let escrow_hold =
            EscrowHoldAttestation::sign(job_id, 100, FundingSource::Organic, 0, &coordinator)
                .unwrap();
        JobRecord {
            operator_pubkey_b58: "operator-pubkey".into(),
            payout_address: "operator-payout".into(),
            envelope,
            escrow_hold,
            phase: JobPhase::Offered,
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
            offered_at_ms: 0,
            pinned: false,
            accepted_at_ms: None,
            metered_elapsed_ms: None,
            close_requested_at_ms: None,
            lease_access: None,
            check_jobs: Vec::new(),
            checks_task: None,
            hidden_checks: None,
            vote_round: None,
            rework: None,
        }
    }

    fn held(amount: u64) -> EscrowHoldState {
        EscrowHoldState {
            amount_micro_usdc: amount,
            funding_source: FundingSource::Organic,
            status: EscrowStatus::Held,
            buyer_pubkey_b58: "buyer-pubkey".into(),
        }
    }

    #[test]
    fn a_stake_slash_keeps_its_latest_state_through_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let journal = Journal::open(&path).unwrap();
        let attempted = StakeSlashState {
            slash_id: "canary:j1:op".into(),
            operator_pubkey_b58: "op".into(),
            job_id: Uuid::new_v4(),
            amount: 50,
            reason: "wrong answer".into(),
            status: StakeSlashStatus::Attempted,
            tx_signature: None,
            detail: String::new(),
        };
        let slashed = StakeSlashState {
            status: StakeSlashStatus::Slashed,
            tx_signature: Some("sig".into()),
            ..attempted.clone()
        };
        let pending = StakeSlashState {
            slash_id: "redundancy:j2:op".into(),
            ..attempted.clone()
        };
        journal.record_stake_slash(&attempted).unwrap();
        journal.record_stake_slash(&slashed).unwrap();
        journal.record_stake_slash(&pending).unwrap();

        let expect = |restored: RestoredState| {
            assert_eq!(restored.stake_slashes.len(), 2);
            assert_eq!(restored.stake_slashes["canary:j1:op"], slashed);
            assert_eq!(restored.stake_slashes["redundancy:j2:op"], pending);
        };
        expect(Journal::load(&path).unwrap());
        journal.compact().unwrap();
        expect(Journal::load(&path).unwrap());
    }

    #[test]
    fn append_then_load_round_trips_with_last_write_winning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let journal = Journal::open(&path).unwrap();

        let job_id = Uuid::new_v4();
        let mut rec = record(job_id);
        journal.record_job(job_id, &rec).unwrap();
        journal.record_escrow(job_id, &held(100)).unwrap();

        rec.phase = JobPhase::Completed;
        journal.record_job(job_id, &rec).unwrap();
        journal
            .record_escrow(
                job_id,
                &EscrowHoldState {
                    status: EscrowStatus::Released,
                    ..held(100)
                },
            )
            .unwrap();

        let restored = Journal::load(&path).unwrap();
        assert_eq!(restored.jobs.len(), 1);
        assert_eq!(restored.jobs[&job_id].phase, JobPhase::Completed);
        assert_eq!(restored.holds[&job_id].status, EscrowStatus::Released);
        assert_eq!(restored.holds[&job_id].amount_micro_usdc, 100);
        assert_eq!(restored.holds[&job_id].buyer_pubkey_b58, "buyer-pubkey");
    }

    #[test]
    fn deposit_replay_is_idempotent_by_deposit_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let journal = Journal::open(&path).unwrap();

        journal.record_deposit("sig-1", "buyer-a", 5_000).unwrap();
        journal.record_deposit("sig-2", "buyer-a", 2_000).unwrap();
        journal.record_deposit("sig-3", "buyer-b", 900).unwrap();
        // A duplicate line (e.g. a crash between memory-commit and the
        // caller's ack causing a re-record) must not double-count.
        journal.record_deposit("sig-1", "buyer-a", 5_000).unwrap();

        let restored = Journal::load(&path).unwrap();
        assert_eq!(restored.deposit_totals["buyer-a"], 7_000);
        assert_eq!(restored.deposit_totals["buyer-b"], 900);
        assert_eq!(restored.deposit_ids.len(), 3);
    }

    #[test]
    fn replayed_running_totals_saturate_like_the_live_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let journal = Journal::open(&path).unwrap();
        // The live deposit/bond/partner books saturate rather than wrap;
        // replay must reconstruct the same capped total, never wrap
        // (release) or panic (debug) on the summed lines.
        journal
            .record_deposit("sig-1", "whale", u64::MAX - 10)
            .unwrap();
        journal.record_deposit("sig-2", "whale", 100).unwrap();
        journal
            .record_bond_post("bond-1", "op", u64::MAX - 10)
            .unwrap();
        journal.record_bond_post("bond-2", "op", 100).unwrap();

        let restored = Journal::load(&path).unwrap();
        assert_eq!(restored.deposit_totals["whale"], u64::MAX);
        assert_eq!(restored.bond_totals["op"], u64::MAX);
    }

    #[test]
    fn a_missing_file_loads_as_empty_state() {
        let dir = tempfile::tempdir().unwrap();
        let restored = Journal::load(&dir.path().join("nope.jsonl")).unwrap();
        assert!(restored.jobs.is_empty());
        assert!(restored.holds.is_empty());
    }

    #[test]
    fn a_torn_final_line_is_dropped_but_mid_file_corruption_refuses_to_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let journal = Journal::open(&path).unwrap();
        let job_id = Uuid::new_v4();
        journal.record_escrow(job_id, &held(500)).unwrap();
        drop(journal);

        // Crash mid-append: a truncated trailing line.
        {
            use std::io::Write as _;
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(b"{\"kind\":\"escrow\",\"job_id\":\"trunca")
                .unwrap();
        }
        let restored = Journal::load(&path).unwrap();
        assert_eq!(restored.holds[&job_id].amount_micro_usdc, 500);

        // The same garbage mid-file is damage, not a torn write.
        {
            use std::io::Write as _;
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(b"\n{\"kind\":\"escrow\",\"job_id\":\"00000000-0000-0000-0000-000000000000\",\"amount_micro_usdc\":1,\"funding_source\":\"organic\",\"status\":\"held\"}\n")
                .unwrap();
        }
        assert!(matches!(
            Journal::load(&path),
            Err(JournalError::Corrupt { .. })
        ));
    }

    fn count_lines(path: &Path) -> usize {
        std::fs::read(path)
            .unwrap()
            .split(|&b| b == b'\n')
            .filter(|l| !l.is_empty())
            .count()
    }

    #[test]
    fn compaction_recovers_identical_state_from_one_line_per_fact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let journal = Journal::open(&path).unwrap();

        // Two jobs walking through phases (upsert churn), one hold
        // transition, duplicate deposits, and a partner payout.
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut rec_a = record(a);
        journal.record_job(a, &rec_a).unwrap();
        journal.record_escrow(a, &held(100)).unwrap();
        rec_a.phase = JobPhase::Accepted;
        journal.record_job(a, &rec_a).unwrap();
        rec_a.phase = JobPhase::Completed;
        journal.record_job(a, &rec_a).unwrap();
        journal
            .record_escrow(
                a,
                &EscrowHoldState {
                    status: EscrowStatus::Released,
                    ..held(100)
                },
            )
            .unwrap();
        let rec_b = record(b);
        journal.record_job(b, &rec_b).unwrap();
        journal.record_escrow(b, &held(700)).unwrap();
        journal.record_deposit("sig-1", "buyer-a", 5_000).unwrap();
        journal.record_deposit("sig-1", "buyer-a", 5_000).unwrap();
        journal.record_deposit("sig-2", "buyer-b", 900).unwrap();
        journal
            .record_partner_payout("pay-1", "code-x", 300)
            .unwrap();
        journal
            .record_partner_payout("pay-1", "code-x", 300)
            .unwrap();

        let before = Journal::load(&path).unwrap();
        let lines_before = count_lines(&path);
        let stats = journal.compact().unwrap();
        let after = Journal::load(&path).unwrap();

        assert_eq!(stats.entries_before, lines_before);
        assert_eq!(stats.entries_after, count_lines(&path));
        assert!(stats.shrank(), "12 lines describe 7 facts: {stats:?}");
        assert_eq!(stats.entries_after, 7);

        assert_eq!(after.jobs.len(), before.jobs.len());
        assert_eq!(after.jobs[&a].phase, JobPhase::Completed);
        assert_eq!(after.jobs[&b].phase, JobPhase::Offered);
        assert_eq!(after.holds, before.holds);
        assert_eq!(after.deposit_totals, before.deposit_totals);
        assert_eq!(after.deposit_ids, before.deposit_ids);
        assert_eq!(after.partner_paid_totals, before.partner_paid_totals);
        assert_eq!(after.partner_payout_ids, before.partner_payout_ids);
    }

    #[test]
    fn appends_after_compaction_land_in_the_compacted_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let journal = Journal::open(&path).unwrap();

        let first = Uuid::new_v4();
        journal.record_escrow(first, &held(100)).unwrap();
        journal.record_escrow(first, &held(150)).unwrap();
        journal.compact().unwrap();

        let second = Uuid::new_v4();
        journal.record_escrow(second, &held(700)).unwrap();
        journal.record_deposit("sig-9", "buyer-z", 42).unwrap();

        let restored = Journal::load(&path).unwrap();
        assert_eq!(restored.holds[&first].amount_micro_usdc, 150);
        assert_eq!(restored.holds[&second].amount_micro_usdc, 700);
        assert_eq!(restored.deposit_totals["buyer-z"], 42);
        assert_eq!(count_lines(&path), 3);
    }

    #[test]
    fn deposit_idempotency_ids_survive_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let journal = Journal::open(&path).unwrap();

        journal.record_deposit("sig-1", "buyer-a", 5_000).unwrap();
        journal.compact().unwrap();
        // A post-compaction re-record of the same id (the crash-replay
        // shape) must still count once — the id-set is the guard.
        journal.record_deposit("sig-1", "buyer-a", 5_000).unwrap();

        let restored = Journal::load(&path).unwrap();
        assert_eq!(restored.deposit_totals["buyer-a"], 5_000);
        assert!(restored.deposit_ids.contains("sig-1"));
        assert_eq!(restored.deposit_ids.len(), 1);
    }

    #[test]
    fn bond_facts_replay_dedup_and_survive_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let journal = Journal::open(&path).unwrap();

        journal.record_bond_post("sig-1", "op-a", 2_000).unwrap();
        journal.record_bond_post("sig-1", "op-a", 2_000).unwrap();
        let job_id = Uuid::new_v4();
        let slash = crate::bond::SlashRecord {
            slash_id: "canary:j1".into(),
            operator_pubkey_b58: "op-a".into(),
            amount_micro_usdc: 300,
            job_id,
            reason: "canary wrong-answer".into(),
            slashed_at_ms: 5,
        };
        journal.record_bond_slash(&slash).unwrap();
        journal.record_bond_slash(&slash).unwrap();
        let mut unbond = crate::bond::UnbondState {
            unbond_id: Uuid::new_v4(),
            operator_pubkey_b58: "op-a".into(),
            recipient_address_b58: "recipient".into(),
            amount_micro_usdc: 700,
            requested_at_ms: 6,
            matures_at_ms: 100,
            pushed: None,
        };
        journal.record_unbond(&unbond).unwrap();
        unbond.pushed = Some(crate::bond::BondRefundPush {
            tx_signature: Some("sig-r".into()),
            recorded_at_ms: 101,
            paid_micro_usdc: 700,
        });
        journal.record_unbond(&unbond).unwrap();

        let before = Journal::load(&path).unwrap();
        assert_eq!(before.bond_totals["op-a"], 2_000);
        assert_eq!(before.bond_slashes.len(), 1);
        assert_eq!(
            before.unbonds[&unbond.unbond_id]
                .pushed
                .as_ref()
                .unwrap()
                .paid_micro_usdc,
            700
        );

        let stats = journal.compact().unwrap();
        assert_eq!(stats.entries_after, 3, "6 lines describe 3 facts");
        let after = Journal::load(&path).unwrap();
        assert_eq!(after.bond_totals, before.bond_totals);
        assert_eq!(after.bond_ids, before.bond_ids);
        assert_eq!(after.bond_slashes, before.bond_slashes);
        assert_eq!(after.unbonds, before.unbonds);
    }

    #[test]
    fn a_subsidy_close_latches_on_replay_and_survives_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let journal = Journal::open(&path).unwrap();

        assert_eq!(Journal::load(&path).unwrap().subsidy_closed_at_ms, None);
        journal.record_subsidy_closed(41).unwrap();
        // A crash between journal-write and latch can re-record the
        // close on the retry; the first close on record stays the close.
        journal.record_subsidy_closed(99).unwrap();

        let before = Journal::load(&path).unwrap();
        assert_eq!(before.subsidy_closed_at_ms, Some(41));

        let stats = journal.compact().unwrap();
        assert_eq!(stats.entries_after, 1, "two lines describe one latch");
        assert_eq!(Journal::load(&path).unwrap().subsidy_closed_at_ms, Some(41));
    }

    #[test]
    fn compaction_keeps_the_first_seen_amount_for_a_duplicated_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let journal = Journal::open(&path).unwrap();

        // Pathological: same id, different amounts. Replay honors the
        // first line; compaction must preserve exactly that.
        journal.record_deposit("sig-1", "buyer-a", 5_000).unwrap();
        journal.record_deposit("sig-1", "buyer-a", 9_999).unwrap();
        let before = Journal::load(&path).unwrap();
        journal.compact().unwrap();
        let after = Journal::load(&path).unwrap();
        assert_eq!(before.deposit_totals["buyer-a"], 5_000);
        assert_eq!(after.deposit_totals["buyer-a"], 5_000);
    }

    #[test]
    fn compacting_an_empty_journal_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let journal = Journal::open(&path).unwrap();
        let stats = journal.compact().unwrap();
        assert_eq!(stats.entries_before, 0);
        assert_eq!(stats.entries_after, 0);
        assert!(!stats.shrank());
        assert!(Journal::load(&path).unwrap().jobs.is_empty());
    }

    #[test]
    fn open_truncates_a_torn_tail_so_the_next_append_starts_clean() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let first = Uuid::new_v4();
        {
            let journal = Journal::open(&path).unwrap();
            journal.record_escrow(first, &held(500)).unwrap();
        }
        {
            use std::io::Write as _;
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(b"{\"kind\":\"escrow\",\"job_id\":\"trunca")
                .unwrap();
        }

        // Reopening the writer heals the file; the next record must not
        // glue onto the torn bytes and read as corruption later.
        let second = Uuid::new_v4();
        {
            let journal = Journal::open(&path).unwrap();
            journal.record_escrow(second, &held(700)).unwrap();
        }
        let restored = Journal::load(&path).unwrap();
        assert_eq!(restored.holds.len(), 2);
        assert_eq!(restored.holds[&first].amount_micro_usdc, 500);
        assert_eq!(restored.holds[&second].amount_micro_usdc, 700);
    }

    #[test]
    fn a_fresh_journal_and_its_successful_appends_report_healthy() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(&dir.path().join("journal.jsonl")).unwrap();
        assert!(journal.is_healthy());
        journal.record_escrow(Uuid::new_v4(), &held(500)).unwrap();
        assert!(journal.is_healthy());
    }

    #[test]
    fn a_write_that_cannot_reach_the_disk_marks_the_journal_unhealthy() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::read_only_for_test(&dir.path().join("journal.jsonl"));
        assert!(journal.record_escrow(Uuid::new_v4(), &held(500)).is_err());
        assert!(!journal.is_healthy());
    }

    #[test]
    fn a_healthy_append_clears_a_prior_unhealthy_mark() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(&dir.path().join("journal.jsonl")).unwrap();
        journal.healthy.store(false, Ordering::Relaxed);
        journal.record_escrow(Uuid::new_v4(), &held(700)).unwrap();
        assert!(journal.is_healthy());
    }

    #[test]
    fn size_bytes_grows_as_records_are_appended() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(&dir.path().join("journal.jsonl")).unwrap();
        assert_eq!(journal.size_bytes(), Some(0));
        journal.record_escrow(Uuid::new_v4(), &held(500)).unwrap();
        let one = journal.size_bytes().unwrap();
        assert!(one > 0);
        journal.record_escrow(Uuid::new_v4(), &held(700)).unwrap();
        assert!(journal.size_bytes().unwrap() > one);
    }
}
