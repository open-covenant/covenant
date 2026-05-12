//! Per-agent token-bucket ledger backing `Settlement.budget_credits_per_hour`
//! (manifest §5).
//!
//! Backs the `00_spec.md` §11 pin: *when an agent hits
//! `budget_credits_per_hour`, the runtime pauses, persists partial state,
//! settles consumed credits, and queues a resume*. This crate ships the
//! types and storage backends; `dispatch_intent` calls
//! [`BudgetLedger::try_debit`] before spawning, and the checkpoint store
//! preserves pause state before full runtime suspension wiring lands.
//!
//! ## Token-bucket model
//!
//! Each agent has a `capacity = credits_per_hour` and a current
//! `tokens_remaining`. Refill rate is `capacity / 3_600_000` tokens
//! per millisecond. Refill is **lazy**: every read
//! ([`tokens_remaining`], [`would_exceed`], [`try_debit`]) computes the
//! refill from `epoch_ms() - last_refill_ms` before answering, advancing
//! `last_refill_ms` only by the time that produced an integer-token
//! refill so sub-token elapsed time accumulates instead of being lost.
//! When the bucket caps at capacity the clock resets to `now` so an
//! idle agent can't bank arbitrary refills past capacity and then drain
//! them all instantly (would violate the rate limit).
//!
//! ## Storage backends
//!
//! [`InMemoryLedger`] for tests; [`JsonlLedger`] for production
//! (event-log replay on `open`, tempfile+rename for compaction).
//! Both serialize concurrent mutations through a `Mutex` so two
//! debits can't race past the same `tokens_remaining` snapshot.
//! [`JsonlPauseCheckpointStore`] stores pause/resume handoff records
//! separately from spend events so replay cannot charge the same work
//! twice.
//!
//! ## Compaction
//!
//! [`BudgetLedger::compact_older_than`] drops [`BudgetEvent::Debit`]
//! events older than the cutoff and emits one per-agent
//! [`BudgetEvent::Snapshot`] capturing the bucket's `(capacity,
//! tokens_remaining, last_refill_ms)` at the cutoff. Replay treats
//! Snapshot as authoritative for that agent, so reopening a compacted
//! ledger reconstructs the same bucket state as before the rewrite —
//! compaction is non-destructive of bucket state.
//!
//! [`tokens_remaining`]: BudgetLedger::tokens_remaining
//! [`would_exceed`]: BudgetLedger::would_exceed
//! [`try_debit`]: BudgetLedger::try_debit

#![deny(unsafe_code)]

use async_trait::async_trait;
use covenant_types::{AgentId, BudgetPauseCheckpoint};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use uuid::Uuid;

const MS_PER_HOUR: u128 = 3_600_000;

#[derive(Debug, thiserror::Error)]
pub enum BudgetError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    /// Agent has no capacity registered. Call [`BudgetLedger::set_capacity`]
    /// before debiting.
    #[error("no capacity for {0}")]
    NoCapacity(String),
    /// Debit would drop `tokens_remaining` negative. `tokens_remaining`
    /// is what was available when the call was made; `refill_eta_ms`
    /// is the absolute `epoch_ms` at which the bucket will hold enough
    /// tokens to cover the requested debit — the wait floor for the
    /// pause-and-queue resume logic.
    #[error(
        "budget exhausted: {tokens_remaining} tokens remaining, refill eta {refill_eta_ms} ms"
    )]
    Exhausted {
        tokens_remaining: u64,
        refill_eta_ms: u64,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum BudgetCheckpointError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("invalid pause checkpoint: {0}")]
    InvalidCheckpoint(String),
    #[error("pause checkpoint already active for intent {0}")]
    AlreadyPaused(Uuid),
    #[error("pause checkpoint already resumed for intent {0}")]
    AlreadyResumed(Uuid),
    #[error("no pause checkpoint for intent {0}")]
    NotFound(Uuid),
}

/// One debit event. Persisted to the JSONL log by [`JsonlLedger`] and
/// returned by [`BudgetLedger::recent_debits`] for the operator
/// dashboard / settlement-reconciliation paths.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BudgetDebit {
    pub agent: AgentId,
    pub credits: u64,
    /// The [`uuid::Uuid`] of the [`covenant_types::SettlementReceipt`]
    /// this debit pairs with. The runtime's settlement flush settles
    /// the pair, so the budget log and the receipt log can be joined.
    pub paired_receipt: Uuid,
    pub at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BudgetEvent {
    CapacitySet {
        agent: AgentId,
        credits_per_hour: u64,
        at_ms: u64,
    },
    Debit(BudgetDebit),
    /// Synthetic checkpoint emitted by [`BudgetLedger::compact_older_than`]
    /// before pre-cutoff [`BudgetEvent::Debit`] events are dropped.
    /// Replay overwrites the bucket's state with these fields, so the
    /// dropped debits' net effect is preserved. One per agent that had
    /// any debit dropped.
    Snapshot {
        agent: AgentId,
        capacity: u64,
        tokens_remaining: u64,
        last_refill_ms: u64,
        at_ms: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BudgetCheckpointEvent {
    PauseSaved {
        checkpoint: BudgetPauseCheckpoint,
    },
    ResumeClaimed {
        intent_id: Uuid,
        agent: AgentId,
        resumed_at_ms: u64,
    },
}

#[derive(Debug, Clone, PartialEq)]
struct CheckpointState {
    checkpoint: BudgetPauseCheckpoint,
    resumed_at_ms: Option<u64>,
}

#[async_trait]
pub trait BudgetLedger: Send + Sync {
    /// Provision `agent` with a budget of `credits_per_hour`. Initial
    /// `tokens_remaining` is `credits_per_hour` (full bucket); calling
    /// again on an existing agent first refills under the prior
    /// capacity, then re-bases capacity (clamping `tokens_remaining`
    /// down if the new capacity is smaller).
    async fn set_capacity(&self, agent: &AgentId, credits_per_hour: u64)
        -> Result<(), BudgetError>;

    /// Atomic predicate-then-debit. Returns `Ok(())` on a successful
    /// debit; returns [`BudgetError::Exhausted`] when the debit would
    /// drive `tokens_remaining` negative; returns
    /// [`BudgetError::NoCapacity`] when the agent has no bucket.
    /// Persists a [`BudgetDebit`] to the underlying log.
    async fn try_debit(
        &self,
        agent: &AgentId,
        credits: u64,
        paired_receipt: Uuid,
    ) -> Result<(), BudgetError>;

    /// Read-only predicate. Lazy-refills before answering.
    async fn would_exceed(&self, agent: &AgentId, credits: u64) -> Result<bool, BudgetError>;

    /// Lazy-refills before returning the live count.
    async fn tokens_remaining(&self, agent: &AgentId) -> Result<u64, BudgetError>;

    /// Oldest-first up to `limit`. Operator-facing.
    async fn recent_debits(
        &self,
        agent: &AgentId,
        limit: usize,
    ) -> Result<Vec<BudgetDebit>, BudgetError>;

    /// Drop debit events with `at_ms < before_ms`. See module docs for
    /// the snapshot-based non-destructive compaction model.
    async fn compact_older_than(&self, before_ms: u64) -> Result<u64, BudgetError>;
}

#[derive(Debug, Clone)]
struct Bucket {
    display: String,
    capacity: u64,
    tokens_remaining: u64,
    last_refill_ms: u64,
}

/// Lazy refill: bring `bucket.tokens_remaining` forward to `now`.
fn refill(bucket: &mut Bucket, now: u64) {
    if bucket.capacity == 0 || now <= bucket.last_refill_ms {
        return;
    }
    let elapsed = (now - bucket.last_refill_ms) as u128;
    let add_u128 = elapsed * (bucket.capacity as u128) / MS_PER_HOUR;
    if add_u128 == 0 {
        // Sub-token elapsed: leave `last_refill_ms` so the fractional
        // milliseconds roll into the next call.
        return;
    }
    let add = add_u128.min(bucket.capacity as u128) as u64;
    bucket.tokens_remaining = bucket
        .tokens_remaining
        .saturating_add(add)
        .min(bucket.capacity);
    let consumed_ms = (add as u128) * MS_PER_HOUR / (bucket.capacity as u128);
    bucket.last_refill_ms = bucket.last_refill_ms.saturating_add(consumed_ms as u64);
    if bucket.tokens_remaining == bucket.capacity {
        // Bucket is full; drop unconsumed accumulator so an idle agent
        // can't bank arbitrary refills past capacity.
        bucket.last_refill_ms = now;
    }
}

/// `epoch_ms` at which the bucket will hold at least `credits` tokens
/// given the current refill rate. Returns `now` if already enough,
/// `u64::MAX` if `capacity == 0` (will never refill).
fn refill_eta_ms(bucket: &Bucket, credits: u64, now: u64) -> u64 {
    if credits <= bucket.tokens_remaining {
        return now;
    }
    if bucket.capacity == 0 {
        return u64::MAX;
    }
    let needed = (credits - bucket.tokens_remaining) as u128;
    let ms = (needed * MS_PER_HOUR).div_ceil(bucket.capacity as u128);
    now.saturating_add(ms.min(u64::MAX as u128) as u64)
}

/// In-process ledger suitable for tests.
pub struct InMemoryLedger {
    buckets: Mutex<HashMap<[u8; 32], Bucket>>,
    debits: Mutex<Vec<BudgetDebit>>,
}

impl Default for InMemoryLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryLedger {
    pub fn new() -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            debits: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl BudgetLedger for InMemoryLedger {
    async fn set_capacity(
        &self,
        agent: &AgentId,
        credits_per_hour: u64,
    ) -> Result<(), BudgetError> {
        let now = epoch_ms();
        let mut buckets = self.buckets.lock().await;
        // Idempotent on capacity match: a re-stamp of last_refill_ms = now
        // every boot would prevent slow-rate buckets from ever refilling
        // on restart-heavy deployments. Display name is updated in place
        // (cheap, no rate-limit semantic).
        if let Some(existing) = buckets.get_mut(&agent.pubkey) {
            if existing.capacity == credits_per_hour {
                existing.display = agent.display.clone();
                return Ok(());
            }
        }
        let entry = buckets.entry(agent.pubkey).or_insert_with(|| Bucket {
            display: agent.display.clone(),
            capacity: credits_per_hour,
            tokens_remaining: credits_per_hour,
            last_refill_ms: now,
        });
        refill(entry, now);
        entry.capacity = credits_per_hour;
        entry.display = agent.display.clone();
        if entry.tokens_remaining > credits_per_hour {
            entry.tokens_remaining = credits_per_hour;
        }
        // Strictly monotonic — refuse to walk the clock backward on NTP
        // skew or a re-stamp from a stale `now` argument.
        entry.last_refill_ms = now.max(entry.last_refill_ms);
        Ok(())
    }

    async fn try_debit(
        &self,
        agent: &AgentId,
        credits: u64,
        paired_receipt: Uuid,
    ) -> Result<(), BudgetError> {
        let now = epoch_ms();
        let mut buckets = self.buckets.lock().await;
        let bucket = buckets
            .get_mut(&agent.pubkey)
            .ok_or_else(|| BudgetError::NoCapacity(agent.display.clone()))?;
        refill(bucket, now);
        if bucket.tokens_remaining < credits {
            return Err(BudgetError::Exhausted {
                tokens_remaining: bucket.tokens_remaining,
                refill_eta_ms: refill_eta_ms(bucket, credits, now),
            });
        }
        bucket.tokens_remaining -= credits;
        drop(buckets);
        self.debits.lock().await.push(BudgetDebit {
            agent: agent.clone(),
            credits,
            paired_receipt,
            at_ms: now,
        });
        Ok(())
    }

    async fn would_exceed(&self, agent: &AgentId, credits: u64) -> Result<bool, BudgetError> {
        let now = epoch_ms();
        let mut buckets = self.buckets.lock().await;
        let bucket = buckets
            .get_mut(&agent.pubkey)
            .ok_or_else(|| BudgetError::NoCapacity(agent.display.clone()))?;
        refill(bucket, now);
        Ok(bucket.tokens_remaining < credits)
    }

    async fn tokens_remaining(&self, agent: &AgentId) -> Result<u64, BudgetError> {
        let now = epoch_ms();
        let mut buckets = self.buckets.lock().await;
        let bucket = buckets
            .get_mut(&agent.pubkey)
            .ok_or_else(|| BudgetError::NoCapacity(agent.display.clone()))?;
        refill(bucket, now);
        Ok(bucket.tokens_remaining)
    }

    async fn recent_debits(
        &self,
        agent: &AgentId,
        limit: usize,
    ) -> Result<Vec<BudgetDebit>, BudgetError> {
        let debits = self.debits.lock().await;
        Ok(debits
            .iter()
            .filter(|d| d.agent.pubkey == agent.pubkey)
            .take(limit)
            .cloned()
            .collect())
    }

    async fn compact_older_than(&self, before_ms: u64) -> Result<u64, BudgetError> {
        let mut debits = self.debits.lock().await;
        let before = debits.len();
        debits.retain(|d| d.at_ms >= before_ms);
        Ok((before - debits.len()) as u64)
    }
}

/// JSONL-backed [`BudgetLedger`]. Append-only event log; `open()`
/// replays the log to rebuild in-memory bucket state and the debit
/// history. Mirrors the shape of [`covenant_peer_auth::JsonlPeerRegistry`]
/// and [`covenant_a2a::JsonlMailbox`].
///
/// Concurrency: every mutation holds `file_lock` across the persist
/// (append) and the in-memory mutation, so two debits can't both
/// observe `tokens_remaining = N` and then both spend it.
///
/// [`covenant_peer_auth::JsonlPeerRegistry`]: https://docs.rs/covenant-peer-auth
/// [`covenant_a2a::JsonlMailbox`]: https://docs.rs/covenant-a2a
pub struct JsonlLedger {
    path: PathBuf,
    buckets: Mutex<HashMap<[u8; 32], Bucket>>,
    debits: Mutex<Vec<BudgetDebit>>,
    file_lock: Arc<Mutex<()>>,
}

/// JSONL-backed checkpoint store for pausing in-flight work without
/// charging the budget ledger twice on resume.
pub struct JsonlPauseCheckpointStore {
    path: PathBuf,
    checkpoints: Mutex<HashMap<(Uuid, [u8; 32]), CheckpointState>>,
    file_lock: Arc<Mutex<()>>,
}

impl JsonlPauseCheckpointStore {
    /// `path` should typically be `$COVENANT_HOME/budget/checkpoints.jsonl`.
    /// Creates the file and parent directories if missing.
    pub async fn open(path: PathBuf) -> Result<Self, BudgetCheckpointError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;

        let mut checkpoints: HashMap<(Uuid, [u8; 32]), CheckpointState> = HashMap::new();
        let f = fs::File::open(&path).await?;
        let mut reader = BufReader::new(f);
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break;
            }
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<BudgetCheckpointEvent>(trimmed)? {
                BudgetCheckpointEvent::PauseSaved { checkpoint } => {
                    validate_pause_checkpoint(&checkpoint)?;
                    let key = checkpoint_key(checkpoint.intent_id, &checkpoint.agent);
                    if matches!(
                        checkpoints.get(&key),
                        Some(state) if state.resumed_at_ms.is_none()
                    ) {
                        return Err(BudgetCheckpointError::AlreadyPaused(checkpoint.intent_id));
                    }
                    checkpoints.insert(
                        key,
                        CheckpointState {
                            checkpoint,
                            resumed_at_ms: None,
                        },
                    );
                }
                BudgetCheckpointEvent::ResumeClaimed {
                    intent_id,
                    agent,
                    resumed_at_ms,
                } => {
                    let state = checkpoints
                        .get_mut(&checkpoint_key(intent_id, &agent))
                        .ok_or(BudgetCheckpointError::NotFound(intent_id))?;
                    if state.resumed_at_ms.is_some() {
                        return Err(BudgetCheckpointError::AlreadyResumed(intent_id));
                    }
                    state.resumed_at_ms = Some(resumed_at_ms);
                }
            }
        }

        Ok(Self {
            path,
            checkpoints: Mutex::new(checkpoints),
            file_lock: Arc::new(Mutex::new(())),
        })
    }

    pub async fn save_pause(
        &self,
        checkpoint: BudgetPauseCheckpoint,
    ) -> Result<(), BudgetCheckpointError> {
        validate_pause_checkpoint(&checkpoint)?;
        let _g = self.file_lock.lock().await;
        let key = checkpoint_key(checkpoint.intent_id, &checkpoint.agent);
        {
            let checkpoints = self.checkpoints.lock().await;
            if let Some(existing) = checkpoints.get(&key) {
                if existing.resumed_at_ms.is_none() {
                    return Err(BudgetCheckpointError::AlreadyPaused(checkpoint.intent_id));
                }
            }
        }
        self.append(&BudgetCheckpointEvent::PauseSaved {
            checkpoint: checkpoint.clone(),
        })
        .await?;
        self.checkpoints.lock().await.insert(
            key,
            CheckpointState {
                checkpoint,
                resumed_at_ms: None,
            },
        );
        Ok(())
    }

    pub async fn active_pause(
        &self,
        intent_id: Uuid,
        agent: &AgentId,
    ) -> Option<BudgetPauseCheckpoint> {
        let checkpoints = self.checkpoints.lock().await;
        checkpoints
            .get(&checkpoint_key(intent_id, agent))
            .filter(|state| state.resumed_at_ms.is_none())
            .map(|state| state.checkpoint.clone())
    }

    pub async fn claim_resume(
        &self,
        intent_id: Uuid,
        agent: &AgentId,
        resumed_at_ms: u64,
    ) -> Result<BudgetPauseCheckpoint, BudgetCheckpointError> {
        let _g = self.file_lock.lock().await;
        let key = checkpoint_key(intent_id, agent);
        let checkpoint = {
            let checkpoints = self.checkpoints.lock().await;
            let state = checkpoints
                .get(&key)
                .ok_or(BudgetCheckpointError::NotFound(intent_id))?;
            if state.resumed_at_ms.is_some() {
                return Err(BudgetCheckpointError::AlreadyResumed(intent_id));
            }
            state.checkpoint.clone()
        };
        self.append(&BudgetCheckpointEvent::ResumeClaimed {
            intent_id,
            agent: agent.clone(),
            resumed_at_ms,
        })
        .await?;
        let mut checkpoints = self.checkpoints.lock().await;
        if let Some(state) = checkpoints.get_mut(&key) {
            state.resumed_at_ms = Some(resumed_at_ms);
        }
        Ok(checkpoint)
    }

    async fn append(&self, ev: &BudgetCheckpointEvent) -> Result<(), BudgetCheckpointError> {
        let line = serde_json::to_string(ev)?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        f.write_all(line.as_bytes()).await?;
        f.write_all(b"\n").await?;
        f.flush().await?;
        Ok(())
    }
}

fn checkpoint_key(intent_id: Uuid, agent: &AgentId) -> (Uuid, [u8; 32]) {
    (intent_id, agent.pubkey)
}

fn validate_pause_checkpoint(
    checkpoint: &BudgetPauseCheckpoint,
) -> Result<(), BudgetCheckpointError> {
    if checkpoint.version != BudgetPauseCheckpoint::VERSION {
        return Err(BudgetCheckpointError::InvalidCheckpoint(
            "unsupported checkpoint version".into(),
        ));
    }
    if checkpoint.requested_credits == 0 {
        return Err(BudgetCheckpointError::InvalidCheckpoint(
            "requested_credits must be non-zero".into(),
        ));
    }
    validate_resume_state_map(&checkpoint.resume_state)
}

fn validate_resume_state_map(
    map: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), BudgetCheckpointError> {
    for value in map.values() {
        validate_resume_state_value(value)?;
    }
    Ok(())
}

fn validate_resume_state_value(value: &serde_json::Value) -> Result<(), BudgetCheckpointError> {
    match value {
        serde_json::Value::String(s) if looks_machine_local_path(s) => {
            Err(BudgetCheckpointError::InvalidCheckpoint(
                "resume_state contains a machine-local path".into(),
            ))
        }
        serde_json::Value::Array(values) => {
            for value in values {
                validate_resume_state_value(value)?;
            }
            Ok(())
        }
        serde_json::Value::Object(map) => validate_resume_state_map(map),
        _ => Ok(()),
    }
}

fn looks_machine_local_path(s: &str) -> bool {
    let bytes = s.as_bytes();
    s.starts_with('/')
        || s.starts_with("~/")
        || s.starts_with("$HOME/")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/'))
}

impl JsonlLedger {
    /// `path` should typically be `$COVENANT_HOME/budget/ledger.jsonl`.
    /// Creates the file (and parent dirs) if missing.
    pub async fn open(path: PathBuf) -> Result<Self, BudgetError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;

        let mut buckets: HashMap<[u8; 32], Bucket> = HashMap::new();
        let mut debits: Vec<BudgetDebit> = Vec::new();
        let f = fs::File::open(&path).await?;
        let mut reader = BufReader::new(f);
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break;
            }
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<BudgetEvent>(trimmed)? {
                BudgetEvent::CapacitySet {
                    agent,
                    credits_per_hour,
                    at_ms,
                } => {
                    let entry = buckets.entry(agent.pubkey).or_insert_with(|| Bucket {
                        display: agent.display.clone(),
                        capacity: credits_per_hour,
                        tokens_remaining: credits_per_hour,
                        last_refill_ms: at_ms,
                    });
                    refill(entry, at_ms);
                    entry.capacity = credits_per_hour;
                    entry.display = agent.display.clone();
                    if entry.tokens_remaining > credits_per_hour {
                        entry.tokens_remaining = credits_per_hour;
                    }
                    entry.last_refill_ms = at_ms;
                }
                BudgetEvent::Debit(debit) => {
                    if let Some(bucket) = buckets.get_mut(&debit.agent.pubkey) {
                        refill(bucket, debit.at_ms);
                        bucket.tokens_remaining =
                            bucket.tokens_remaining.saturating_sub(debit.credits);
                    }
                    debits.push(debit);
                }
                BudgetEvent::Snapshot {
                    agent,
                    capacity,
                    tokens_remaining,
                    last_refill_ms,
                    at_ms: _,
                } => {
                    buckets.insert(
                        agent.pubkey,
                        Bucket {
                            display: agent.display,
                            capacity,
                            tokens_remaining,
                            last_refill_ms,
                        },
                    );
                }
            }
        }

        Ok(Self {
            path,
            buckets: Mutex::new(buckets),
            debits: Mutex::new(debits),
            file_lock: Arc::new(Mutex::new(())),
        })
    }

    async fn append(&self, ev: &BudgetEvent) -> Result<(), BudgetError> {
        let line = serde_json::to_string(ev)?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        f.write_all(line.as_bytes()).await?;
        f.write_all(b"\n").await?;
        f.flush().await?;
        Ok(())
    }
}

#[async_trait]
impl BudgetLedger for JsonlLedger {
    async fn set_capacity(
        &self,
        agent: &AgentId,
        credits_per_hour: u64,
    ) -> Result<(), BudgetError> {
        let _g = self.file_lock.lock().await;
        // Idempotent: when the bucket already has the requested capacity
        // (prior CapacitySet replayed at open) skip the persist + the
        // last_refill_ms re-stamp. Avoids the slow-rate-bucket
        // starvation on restart-heavy deployments — `register_agent_budgets`
        // calls this on every boot.
        {
            let mut buckets = self.buckets.lock().await;
            if let Some(existing) = buckets.get_mut(&agent.pubkey) {
                if existing.capacity == credits_per_hour {
                    existing.display = agent.display.clone();
                    return Ok(());
                }
            }
        }
        let now = epoch_ms();
        self.append(&BudgetEvent::CapacitySet {
            agent: agent.clone(),
            credits_per_hour,
            at_ms: now,
        })
        .await?;
        let mut buckets = self.buckets.lock().await;
        let entry = buckets.entry(agent.pubkey).or_insert_with(|| Bucket {
            display: agent.display.clone(),
            capacity: credits_per_hour,
            tokens_remaining: credits_per_hour,
            last_refill_ms: now,
        });
        refill(entry, now);
        entry.capacity = credits_per_hour;
        entry.display = agent.display.clone();
        if entry.tokens_remaining > credits_per_hour {
            entry.tokens_remaining = credits_per_hour;
        }
        // Strictly monotonic — refuse to walk the clock backward on NTP
        // skew or a re-stamp from a stale `now` argument.
        entry.last_refill_ms = now.max(entry.last_refill_ms);
        Ok(())
    }

    async fn try_debit(
        &self,
        agent: &AgentId,
        credits: u64,
        paired_receipt: Uuid,
    ) -> Result<(), BudgetError> {
        let _g = self.file_lock.lock().await;
        let now = epoch_ms();
        let (would_pass, snapshot) = {
            let mut buckets = self.buckets.lock().await;
            let bucket = buckets
                .get_mut(&agent.pubkey)
                .ok_or_else(|| BudgetError::NoCapacity(agent.display.clone()))?;
            refill(bucket, now);
            if bucket.tokens_remaining < credits {
                return Err(BudgetError::Exhausted {
                    tokens_remaining: bucket.tokens_remaining,
                    refill_eta_ms: refill_eta_ms(bucket, credits, now),
                });
            }
            (true, bucket.clone())
        };
        debug_assert!(would_pass);
        let _ = snapshot;
        let debit = BudgetDebit {
            agent: agent.clone(),
            credits,
            paired_receipt,
            at_ms: now,
        };
        // Persist before mutating in-memory: a crash between the two
        // leaves the in-memory state ahead of disk; the next `open()`
        // would replay the persisted debit and arrive at the same
        // state. Reversing the order would let an in-memory debit live
        // forever without a corresponding event in the log.
        self.append(&BudgetEvent::Debit(debit.clone())).await?;
        {
            let mut buckets = self.buckets.lock().await;
            if let Some(bucket) = buckets.get_mut(&agent.pubkey) {
                bucket.tokens_remaining = bucket.tokens_remaining.saturating_sub(credits);
            }
        }
        self.debits.lock().await.push(debit);
        Ok(())
    }

    async fn would_exceed(&self, agent: &AgentId, credits: u64) -> Result<bool, BudgetError> {
        let now = epoch_ms();
        let mut buckets = self.buckets.lock().await;
        let bucket = buckets
            .get_mut(&agent.pubkey)
            .ok_or_else(|| BudgetError::NoCapacity(agent.display.clone()))?;
        refill(bucket, now);
        Ok(bucket.tokens_remaining < credits)
    }

    async fn tokens_remaining(&self, agent: &AgentId) -> Result<u64, BudgetError> {
        let now = epoch_ms();
        let mut buckets = self.buckets.lock().await;
        let bucket = buckets
            .get_mut(&agent.pubkey)
            .ok_or_else(|| BudgetError::NoCapacity(agent.display.clone()))?;
        refill(bucket, now);
        Ok(bucket.tokens_remaining)
    }

    async fn recent_debits(
        &self,
        agent: &AgentId,
        limit: usize,
    ) -> Result<Vec<BudgetDebit>, BudgetError> {
        let debits = self.debits.lock().await;
        Ok(debits
            .iter()
            .filter(|d| d.agent.pubkey == agent.pubkey)
            .take(limit)
            .cloned()
            .collect())
    }

    async fn compact_older_than(&self, before_ms: u64) -> Result<u64, BudgetError> {
        let _g = self.file_lock.lock().await;
        let raw = match fs::read_to_string(&self.path).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        let events: Vec<BudgetEvent> = raw
            .lines()
            .filter(|l| !l.is_empty())
            .map(serde_json::from_str)
            .collect::<Result<Vec<_>, _>>()?;

        // Replay all pre-cutoff events into a synthetic bucket map and
        // track which agents had at least one pre-cutoff Debit (those
        // are the agents whose Debit rows we'll drop and replace with a
        // Snapshot). Agents with only pre-cutoff CapacitySet rows — no
        // dropped Debits — keep their CapacitySet rows in the rewritten
        // stream, no Snapshot needed.
        let mut state: HashMap<[u8; 32], Bucket> = HashMap::new();
        let mut affected: std::collections::HashSet<[u8; 32]> = std::collections::HashSet::new();
        let mut dropped: u64 = 0;
        for ev in &events {
            match ev {
                BudgetEvent::CapacitySet {
                    agent,
                    credits_per_hour,
                    at_ms,
                } if *at_ms < before_ms => {
                    let entry = state.entry(agent.pubkey).or_insert_with(|| Bucket {
                        display: agent.display.clone(),
                        capacity: *credits_per_hour,
                        tokens_remaining: *credits_per_hour,
                        last_refill_ms: *at_ms,
                    });
                    refill(entry, *at_ms);
                    entry.capacity = *credits_per_hour;
                    entry.display = agent.display.clone();
                    if entry.tokens_remaining > *credits_per_hour {
                        entry.tokens_remaining = *credits_per_hour;
                    }
                    entry.last_refill_ms = *at_ms;
                }
                BudgetEvent::Debit(d) if d.at_ms < before_ms => {
                    if let Some(bucket) = state.get_mut(&d.agent.pubkey) {
                        refill(bucket, d.at_ms);
                        bucket.tokens_remaining = bucket.tokens_remaining.saturating_sub(d.credits);
                    }
                    affected.insert(d.agent.pubkey);
                    dropped += 1;
                }
                BudgetEvent::Snapshot {
                    agent,
                    capacity,
                    tokens_remaining,
                    last_refill_ms,
                    at_ms,
                } if *at_ms < before_ms => {
                    state.insert(
                        agent.pubkey,
                        Bucket {
                            display: agent.display.clone(),
                            capacity: *capacity,
                            tokens_remaining: *tokens_remaining,
                            last_refill_ms: *last_refill_ms,
                        },
                    );
                }
                _ => {}
            }
        }
        if dropped == 0 {
            return Ok(0);
        }

        // Rewrite the event stream:
        //   1. Pre-cutoff CapacitySet and Snapshot rows are kept (replay
        //      will set the bucket up; the new Snapshot for affected
        //      agents comes after and overwrites them).
        //   2. Pre-cutoff Debit rows are dropped.
        //   3. One Snapshot per affected agent at at_ms = before_ms - 1.
        //   4. All post-cutoff events kept verbatim.
        // Post-cutoff Snapshot insertion order is determined by sort
        // over pubkeys for cross-run determinism.
        let snapshot_at = before_ms.saturating_sub(1);
        let mut rewritten: Vec<BudgetEvent> = Vec::new();
        for ev in &events {
            match ev {
                BudgetEvent::CapacitySet { at_ms, .. } if *at_ms < before_ms => {
                    rewritten.push(ev.clone());
                }
                BudgetEvent::Snapshot { at_ms, .. } if *at_ms < before_ms => {
                    rewritten.push(ev.clone());
                }
                _ => {}
            }
        }
        let mut affected_sorted: Vec<[u8; 32]> = affected.iter().copied().collect();
        affected_sorted.sort();
        for pk in &affected_sorted {
            if let Some(b) = state.get(pk) {
                rewritten.push(BudgetEvent::Snapshot {
                    agent: AgentId::new(b.display.clone(), *pk),
                    capacity: b.capacity,
                    tokens_remaining: b.tokens_remaining,
                    last_refill_ms: b.last_refill_ms,
                    at_ms: snapshot_at,
                });
            }
        }
        for ev in &events {
            let at = match ev {
                BudgetEvent::CapacitySet { at_ms, .. } => *at_ms,
                BudgetEvent::Debit(d) => d.at_ms,
                BudgetEvent::Snapshot { at_ms, .. } => *at_ms,
            };
            if at >= before_ms {
                rewritten.push(ev.clone());
            }
        }

        let tmp_path = self.path.with_extension("jsonl.tmp");
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)
            .await?;
        for ev in &rewritten {
            let line = serde_json::to_string(ev)?;
            f.write_all(line.as_bytes()).await?;
            f.write_all(b"\n").await?;
        }
        f.flush().await?;
        drop(f);
        fs::rename(&tmp_path, &self.path).await?;

        let mut debits = self.debits.lock().await;
        debits.retain(|d| d.at_ms >= before_ms);
        Ok(dropped)
    }
}

fn epoch_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_types::BudgetPauseReason;
    use serde_json::json;

    fn agent(name: &str) -> AgentId {
        let mut pk = [0u8; 32];
        for (i, b) in name.bytes().take(32).enumerate() {
            pk[i] = b;
        }
        AgentId::new(name, pk)
    }

    #[test]
    fn budget_debit_serde_pins_four_required_fields() {
        // BudgetDebit is persisted to the JSONL budget ledger and
        // returned by BudgetLedger::recent_debits — the operator
        // dashboard surface (CLI `covenant budget recent`, HTTP
        // `/debits`, IPC RecentDebits → Response::Debits). The four
        // wire keys document the budget-log ↔ settlement-receipt
        // join: `paired_receipt` is the load-bearing UUID the
        // runtime's settlement flush settles against the receipt
        // log. None of the fields carry `#[serde(default)]` or
        // `#[serde(skip_serializing_if)]`, so a refactor that
        // added a default on paired_receipt would silently let a
        // malformed budget event decode with Uuid::nil() and the
        // reconciliation path would silently dereference a missing
        // receipt.

        let mut pubkey = [0u8; 32];
        for (i, b) in b"alice@host".iter().take(32).enumerate() {
            pubkey[i] = *b;
        }
        let debit = BudgetDebit {
            agent: AgentId::new("alice@host", pubkey),
            credits: 1500,
            paired_receipt: Uuid::from_u128(0x1234),
            at_ms: 12345,
        };

        let wire = serde_json::to_value(&debit).unwrap();
        let obj = wire
            .as_object()
            .expect("BudgetDebit serialises as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["agent", "at_ms", "credits", "paired_receipt"],
            "BudgetDebit wire object must contain exactly the four documented \
             fields; an addition or rename of any field shifts every operator \
             dashboard consumer and breaks JSONL replay of existing budget logs"
        );

        let agent_obj = obj
            .get("agent")
            .and_then(serde_json::Value::as_object)
            .expect("agent must serialise as a nested JSON object");
        let mut agent_keys: Vec<&str> = agent_obj.keys().map(String::as_str).collect();
        agent_keys.sort();
        assert_eq!(
            agent_keys,
            vec!["display", "pubkey"],
            "BudgetDebit::agent must surface the AgentId display+pubkey shape; \
             a refactor that flattened or restructured AgentId would break the \
             budget-log JSONL replay and the operator dashboard's grouping"
        );
        assert_eq!(
            agent_obj.get("display").and_then(serde_json::Value::as_str),
            Some("alice@host")
        );
        let pubkey_b58 = agent_obj
            .get("pubkey")
            .and_then(serde_json::Value::as_str)
            .expect("pubkey must serialise as a string");
        assert_eq!(
            pubkey_b58,
            debit.agent.pubkey_base58().as_str(),
            "BudgetDebit::agent.pubkey wire form must equal the AgentId's \
             base58-encoded pubkey — the cross-type contract every JSONL \
             replay and dashboard consumer leans on"
        );

        assert_eq!(
            obj.get("credits").and_then(serde_json::Value::as_u64),
            Some(1500),
            "credits must surface as a u64 on the wire"
        );
        assert_eq!(
            obj.get("at_ms").and_then(serde_json::Value::as_u64),
            Some(12345),
            "at_ms must surface as a u64 on the wire"
        );
        assert_eq!(
            obj.get("paired_receipt").and_then(serde_json::Value::as_str),
            Some(Uuid::from_u128(0x1234).to_string().as_str()),
            "paired_receipt must surface as the canonical UUID string on the \
             wire — this is the load-bearing key the settlement-receipt join \
             leans on"
        );

        let decoded: BudgetDebit = serde_json::from_value(wire).unwrap();
        assert_eq!(
            decoded, debit,
            "BudgetDebit must round-trip through serde_json verbatim — the \
             Eq derive is the contract the JSONL replay path leans on"
        );

        let full_obj = serde_json::to_value(&debit).unwrap();
        let full_map = full_obj.as_object().unwrap().clone();
        for required in ["agent", "credits", "paired_receipt", "at_ms"] {
            let mut payload = full_map.clone();
            payload.remove(required);
            assert!(
                serde_json::from_value::<BudgetDebit>(serde_json::Value::Object(payload)).is_err(),
                "BudgetDebit must reject a wire payload that omits {required}; \
                 a stray #[serde(default)] introduction — particularly on \
                 paired_receipt where the nil UUID default would silently mask \
                 receipt-join failures — must fail the test loud"
            );
        }
    }

    #[test]
    fn budget_event_capacity_set_serde_pins_three_field_variant() {
        // BudgetEvent::CapacitySet is the load-bearing JSONL budget-ledger
        // row appended by BudgetLedger::set_capacity. The crate-internal
        // enum is persisted to disk (one JSONL line per event under the
        // budget data dir) so its wire shape is durable across releases —
        // replay reads each line through serde_json::from_str. Three
        // required fields plus the snake_case 'capacity_set' tag:
        // agent (nested AgentId with display + pubkey), credits_per_hour
        // (u64), at_ms (u64). budget_debit_serde_pins_four_required_fields
        // covers the BudgetDebit payload but not the BudgetEvent
        // enum-level discriminator + variant shape; this test closes the
        // capacity-provisioning replay-contract gap so a refactor that
        // defaulted credits_per_hour would not silently disable an
        // agent's bucket after restart.
        let event = BudgetEvent::CapacitySet {
            agent: agent("alice@host"),
            credits_per_hour: 1500,
            at_ms: 12345,
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("BudgetEvent serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["agent", "at_ms", "credits_per_hour", "type"],
            "BudgetEvent::CapacitySet wire form must be exactly four keys: the three variant fields plus the 'type' discriminator",
        );
        assert_eq!(
            obj.get("type"),
            Some(&json!("capacity_set")),
            "BudgetEvent discriminator slug must be snake_case 'capacity_set'; a titlecase or kebab-case regression silently strands every prior JSONL capacity-provisioning row at replay time and leaves runtime buckets uninitialized after restart",
        );

        let agent_obj = obj
            .get("agent")
            .and_then(serde_json::Value::as_object)
            .expect("agent must serialize as a nested JSON object");
        let mut agent_keys: Vec<&str> = agent_obj.keys().map(String::as_str).collect();
        agent_keys.sort();
        assert_eq!(
            agent_keys,
            vec!["display", "pubkey"],
            "BudgetEvent::CapacitySet::agent must surface the AgentId display+pubkey shape; a refactor that flattened or restructured AgentId would break the budget-log JSONL replay's per-agent bucket keying",
        );

        let back: BudgetEvent = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "BudgetEvent::CapacitySet must round-trip through serde_json verbatim — the Eq derive is the contract the JSONL replay path leans on",
        );

        for required in ["agent", "credits_per_hour", "at_ms"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<BudgetEvent>(serde_json::Value::Object(missing)).is_err(),
                "BudgetEvent::CapacitySet wire form must reject a payload missing {required:?}; a stray #[serde(default)] on credits_per_hour would let a malformed row decode with credits_per_hour=0 and replay quietly disables the agent's bucket — every subsequent dispatch hits BudgetExhausted with tokens_remaining=0",
            );
        }
    }

    #[test]
    fn budget_event_snapshot_serde_pins_five_field_variant() {
        // BudgetEvent::Snapshot is the synthetic checkpoint event
        // BudgetLedger::compact_older_than appends before pre-cutoff
        // Debit events are dropped. Replay overwrites the bucket's
        // state with these fields, so the dropped debits' net effect
        // is preserved across compaction. Five required fields plus
        // the snake_case 'snapshot' discriminator: agent (nested
        // AgentId), capacity (u64), tokens_remaining (u64),
        // last_refill_ms (u64), at_ms (u64). A refactor that defaulted
        // tokens_remaining or last_refill_ms would silently corrupt
        // the post-compaction bucket state at replay; a default on
        // capacity would zero out the agent's bucket and every
        // subsequent dispatch would hit BudgetExhausted.
        let event = BudgetEvent::Snapshot {
            agent: agent("alice@host"),
            capacity: 1500,
            tokens_remaining: 500,
            last_refill_ms: 12000,
            at_ms: 12345,
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("BudgetEvent serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "agent",
                "at_ms",
                "capacity",
                "last_refill_ms",
                "tokens_remaining",
                "type",
            ],
            "BudgetEvent::Snapshot wire form must be exactly six keys: the five variant fields plus the 'type' discriminator",
        );
        assert_eq!(
            obj.get("type"),
            Some(&json!("snapshot")),
            "BudgetEvent discriminator slug must be snake_case 'snapshot'; a titlecase or kebab-case regression silently strands every prior JSONL synthetic-checkpoint row at replay time and post-compaction bucket state diverges from the durable history",
        );

        let agent_obj = obj
            .get("agent")
            .and_then(serde_json::Value::as_object)
            .expect("agent must serialize as a nested JSON object");
        let mut agent_keys: Vec<&str> = agent_obj.keys().map(String::as_str).collect();
        agent_keys.sort();
        assert_eq!(
            agent_keys,
            vec!["display", "pubkey"],
            "BudgetEvent::Snapshot::agent must surface the AgentId display+pubkey shape; a refactor that flattened or restructured AgentId would break replay's per-agent checkpoint-to-bucket keying",
        );

        let back: BudgetEvent = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "BudgetEvent::Snapshot must round-trip through serde_json verbatim — the Eq derive is the contract the post-compaction JSONL replay path leans on",
        );

        for required in [
            "agent",
            "capacity",
            "tokens_remaining",
            "last_refill_ms",
            "at_ms",
        ] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<BudgetEvent>(serde_json::Value::Object(missing)).is_err(),
                "BudgetEvent::Snapshot wire form must reject a payload missing {required:?}; a stray #[serde(default)] on tokens_remaining or capacity would zero the bucket at replay and every subsequent dispatch would hit BudgetExhausted, masking replay corruption as policy rejection",
            );
        }
    }

    #[test]
    fn validate_pause_checkpoint_pins_version_credits_and_resume_state() {
        let a = agent("alice@local");
        let intent = Uuid::from_u128(1);

        let ok = checkpoint(&a, intent);
        validate_pause_checkpoint(&ok).expect("valid checkpoint must pass");

        let mut future_version = checkpoint(&a, intent);
        future_version.version = BudgetPauseCheckpoint::VERSION + 1;
        let err = validate_pause_checkpoint(&future_version).unwrap_err();
        match &err {
            BudgetCheckpointError::InvalidCheckpoint(msg) => assert!(
                msg.contains("unsupported checkpoint version"),
                "version-mismatch message must name the guard: {msg}",
            ),
            other => panic!("expected InvalidCheckpoint, got {other:?}"),
        }

        let mut zero_credits = checkpoint(&a, intent);
        zero_credits.requested_credits = 0;
        let err = validate_pause_checkpoint(&zero_credits).unwrap_err();
        match &err {
            BudgetCheckpointError::InvalidCheckpoint(msg) => assert!(
                msg.contains("requested_credits must be non-zero"),
                "zero-credits message must name the guard: {msg}",
            ),
            other => panic!("expected InvalidCheckpoint, got {other:?}"),
        }

        let mut nested_array = checkpoint(&a, intent);
        nested_array.resume_state.insert(
            "trace".to_string(),
            json!(["ok", "/tmp/covenant-state", "ok"]),
        );
        let err = validate_pause_checkpoint(&nested_array).unwrap_err();
        match &err {
            BudgetCheckpointError::InvalidCheckpoint(msg) => assert!(
                msg.contains("machine-local path"),
                "array-nested machine-local path must be rejected: {msg}",
            ),
            other => panic!("expected InvalidCheckpoint, got {other:?}"),
        }

        let mut nested_object = checkpoint(&a, intent);
        nested_object.resume_state.insert(
            "scratch".to_string(),
            json!({"deep": {"local": "~/.config/covenant"}}),
        );
        let err = validate_pause_checkpoint(&nested_object).unwrap_err();
        match &err {
            BudgetCheckpointError::InvalidCheckpoint(msg) => assert!(
                msg.contains("machine-local path"),
                "object-nested machine-local path must be rejected: {msg}",
            ),
            other => panic!("expected InvalidCheckpoint, got {other:?}"),
        }
    }

    #[test]
    fn looks_machine_local_path_pins_unix_tilde_homevar_and_windows_prefixes_and_rejects_non_paths()
    {
        for s in [
            "/tmp/covenant-state",
            "~/.config/covenant",
            "$HOME/cov",
            "C:\\Users\\cov",
            "D:/data/cov",
        ] {
            assert!(
                looks_machine_local_path(s),
                "{s:?} is a documented machine-local prefix and must be rejected by validate_resume_state_value before it can leak into shared pause checkpoints",
            );
        }

        for s in [
            "",
            "foo",
            "foo/bar",
            "./foo",
            "https://example.com/x",
            "C",
            "C:",
        ] {
            assert!(
                !looks_machine_local_path(s),
                "{s:?} must NOT match the machine-local predicate; falsely flagging non-paths would block legitimate resume_state values without explaining why",
            );
        }
    }

    fn checkpoint(agent: &AgentId, intent_id: Uuid) -> BudgetPauseCheckpoint {
        let mut resume_state = serde_json::Map::new();
        resume_state.insert("cursor".to_string(), json!({"step": 2, "unit": "compile"}));
        BudgetPauseCheckpoint {
            version: BudgetPauseCheckpoint::VERSION,
            intent_id,
            agent: agent.clone(),
            reason: BudgetPauseReason::BudgetExhausted,
            requested_credits: 4,
            tokens_remaining: 1,
            refill_eta_ms: 2_000,
            saved_at_ms: 1_000,
            resume_state,
        }
    }

    #[test]
    fn refill_zero_capacity_is_noop() {
        let mut b = Bucket {
            display: "x@y".into(),
            capacity: 0,
            tokens_remaining: 0,
            last_refill_ms: 0,
        };
        refill(&mut b, 1_000_000);
        assert_eq!(b.tokens_remaining, 0);
        assert_eq!(b.last_refill_ms, 0);
    }

    #[test]
    fn refill_partial_elapsed_accumulates_in_clock() {
        // capacity 10/hr → 1 token per 360_000 ms. After 100 ms, refill
        // computes 0 tokens; the clock must NOT advance, so a later
        // refill at 360_000 ms total elapsed grants the full 1 token.
        let mut b = Bucket {
            display: "x@y".into(),
            capacity: 10,
            tokens_remaining: 0,
            last_refill_ms: 0,
        };
        refill(&mut b, 100);
        assert_eq!(b.tokens_remaining, 0);
        assert_eq!(b.last_refill_ms, 0);
        refill(&mut b, 360_000);
        assert_eq!(b.tokens_remaining, 1);
        assert_eq!(b.last_refill_ms, 360_000);
    }

    #[test]
    fn refill_full_bucket_resets_clock_to_now() {
        // 10-hour-idle bucket of capacity 10 should fill to 10 (not 100)
        // and then advance the clock to `now` so a subsequent burn must
        // wait the full refill interval before refilling again.
        let mut b = Bucket {
            display: "x@y".into(),
            capacity: 10,
            tokens_remaining: 0,
            last_refill_ms: 0,
        };
        refill(&mut b, 36_000_000);
        assert_eq!(b.tokens_remaining, 10);
        assert_eq!(b.last_refill_ms, 36_000_000);

        // Burn the bucket and step a small amount of time. Without the
        // clock-reset, the leftover accumulator would refill the bucket
        // immediately; with the reset, it must not.
        b.tokens_remaining = 0;
        refill(&mut b, 36_000_001);
        assert_eq!(b.tokens_remaining, 0);
    }

    #[test]
    fn refill_eta_zero_when_already_have_enough() {
        let b = Bucket {
            display: "x@y".into(),
            capacity: 10,
            tokens_remaining: 5,
            last_refill_ms: 100,
        };
        assert_eq!(refill_eta_ms(&b, 5, 200), 200);
        assert_eq!(refill_eta_ms(&b, 3, 200), 200);
    }

    #[test]
    fn refill_eta_grows_with_shortfall_at_capacity_rate() {
        // capacity 10/hr; need 1 token; bucket empty → ~360_000 ms.
        let b = Bucket {
            display: "x@y".into(),
            capacity: 10,
            tokens_remaining: 0,
            last_refill_ms: 100,
        };
        assert_eq!(refill_eta_ms(&b, 1, 200), 200 + 360_000);
        assert_eq!(refill_eta_ms(&b, 2, 200), 200 + 720_000);
    }

    #[test]
    fn refill_eta_zero_capacity_returns_max() {
        let b = Bucket {
            display: "x@y".into(),
            capacity: 0,
            tokens_remaining: 0,
            last_refill_ms: 0,
        };
        assert_eq!(refill_eta_ms(&b, 1, 100), u64::MAX);
    }

    #[tokio::test]
    async fn in_memory_set_capacity_seeds_full_bucket() {
        let l = InMemoryLedger::new();
        let a = agent("a@local");
        l.set_capacity(&a, 10).await.unwrap();
        assert_eq!(l.tokens_remaining(&a).await.unwrap(), 10);
    }

    #[tokio::test]
    async fn in_memory_try_debit_subtracts_and_logs() {
        let l = InMemoryLedger::new();
        let a = agent("a@local");
        l.set_capacity(&a, 10).await.unwrap();
        l.try_debit(&a, 3, Uuid::new_v4()).await.unwrap();
        assert_eq!(l.tokens_remaining(&a).await.unwrap(), 7);
        let debits = l.recent_debits(&a, 10).await.unwrap();
        assert_eq!(debits.len(), 1);
        assert_eq!(debits[0].credits, 3);
    }

    #[tokio::test]
    async fn in_memory_try_debit_returns_exhausted_when_short() {
        let l = InMemoryLedger::new();
        let a = agent("a@local");
        l.set_capacity(&a, 5).await.unwrap();
        let err = l.try_debit(&a, 6, Uuid::new_v4()).await.unwrap_err();
        match err {
            BudgetError::Exhausted {
                tokens_remaining,
                refill_eta_ms,
            } => {
                assert_eq!(tokens_remaining, 5);
                assert!(refill_eta_ms > 0);
            }
            other => panic!("expected Exhausted, got {other:?}"),
        }
        // No debit should have landed.
        assert!(l.recent_debits(&a, 10).await.unwrap().is_empty());
        assert_eq!(l.tokens_remaining(&a).await.unwrap(), 5);
    }

    #[tokio::test]
    async fn in_memory_try_debit_returns_no_capacity_for_unset_agent() {
        let l = InMemoryLedger::new();
        let stranger = agent("stranger@local");
        let err = l.try_debit(&stranger, 1, Uuid::new_v4()).await.unwrap_err();
        assert!(matches!(err, BudgetError::NoCapacity(_)));
    }

    #[tokio::test]
    async fn in_memory_would_exceed_does_not_consume() {
        let l = InMemoryLedger::new();
        let a = agent("a@local");
        l.set_capacity(&a, 10).await.unwrap();
        assert!(!l.would_exceed(&a, 5).await.unwrap());
        assert!(l.would_exceed(&a, 11).await.unwrap());
        assert_eq!(l.tokens_remaining(&a).await.unwrap(), 10);
    }

    #[tokio::test]
    async fn in_memory_recent_debits_filters_by_agent() {
        let l = InMemoryLedger::new();
        let a = agent("a@local");
        let b = agent("b@local");
        l.set_capacity(&a, 10).await.unwrap();
        l.set_capacity(&b, 10).await.unwrap();
        l.try_debit(&a, 1, Uuid::new_v4()).await.unwrap();
        l.try_debit(&b, 2, Uuid::new_v4()).await.unwrap();
        l.try_debit(&a, 3, Uuid::new_v4()).await.unwrap();
        let a_debits = l.recent_debits(&a, 10).await.unwrap();
        assert_eq!(a_debits.len(), 2);
        assert!(a_debits.iter().all(|d| d.agent.pubkey == a.pubkey));
        let b_debits = l.recent_debits(&b, 10).await.unwrap();
        assert_eq!(b_debits.len(), 1);
        assert_eq!(b_debits[0].credits, 2);
    }

    #[tokio::test]
    async fn in_memory_compact_drops_old_debits_only() {
        let l = InMemoryLedger::new();
        let a = agent("a@local");
        l.set_capacity(&a, 10).await.unwrap();
        l.try_debit(&a, 1, Uuid::new_v4()).await.unwrap();
        // Force one debit's at_ms into the past.
        {
            let mut debits = l.debits.lock().await;
            debits[0].at_ms = 50;
        }
        l.try_debit(&a, 2, Uuid::new_v4()).await.unwrap();
        assert_eq!(l.compact_older_than(100).await.unwrap(), 1);
        let surviving = l.recent_debits(&a, 10).await.unwrap();
        assert_eq!(surviving.len(), 1);
        assert_eq!(surviving[0].credits, 2);
    }

    #[tokio::test]
    async fn in_memory_set_capacity_clamps_tokens_when_shrinking() {
        let l = InMemoryLedger::new();
        let a = agent("a@local");
        l.set_capacity(&a, 10).await.unwrap();
        // Burn 4, leaving 6.
        l.try_debit(&a, 4, Uuid::new_v4()).await.unwrap();
        assert_eq!(l.tokens_remaining(&a).await.unwrap(), 6);
        // Shrink to 5; bucket clamps to 5.
        l.set_capacity(&a, 5).await.unwrap();
        assert_eq!(l.tokens_remaining(&a).await.unwrap(), 5);
    }

    #[tokio::test]
    async fn jsonl_open_on_missing_file_yields_empty_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does").join("not").join("exist.jsonl");
        let l = JsonlLedger::open(path).await.unwrap();
        let stranger = agent("nope@local");
        // No capacity → NoCapacity, not a panic.
        assert!(matches!(
            l.tokens_remaining(&stranger).await.unwrap_err(),
            BudgetError::NoCapacity(_)
        ));
    }

    #[tokio::test]
    async fn jsonl_replays_capacity_and_debits_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget").join("ledger.jsonl");
        let a = agent("alice@local");
        let r1 = Uuid::new_v4();
        let r2 = Uuid::new_v4();
        {
            let l = JsonlLedger::open(path.clone()).await.unwrap();
            l.set_capacity(&a, 10).await.unwrap();
            l.try_debit(&a, 3, r1).await.unwrap();
            l.try_debit(&a, 2, r2).await.unwrap();
            assert_eq!(l.tokens_remaining(&a).await.unwrap(), 5);
        }
        // Reopen — bucket and debits both replay.
        let l2 = JsonlLedger::open(path).await.unwrap();
        // tokens_remaining will be ≥5 (refill may have ticked up some
        // tokens between the two opens; assert it's at least 5).
        let after = l2.tokens_remaining(&a).await.unwrap();
        assert!(
            (5..=10).contains(&after),
            "expected 5..=10, got {after} (refill drift between open and reopen)"
        );
        let debits = l2.recent_debits(&a, 10).await.unwrap();
        assert_eq!(debits.len(), 2);
        assert_eq!(debits[0].paired_receipt, r1);
        assert_eq!(debits[1].paired_receipt, r2);
    }

    #[tokio::test]
    async fn jsonl_compact_atomically_rewrites_log_and_keeps_no_tempfile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        let a = agent("a@local");
        let l = JsonlLedger::open(path.clone()).await.unwrap();
        l.set_capacity(&a, 10).await.unwrap();
        l.try_debit(&a, 1, Uuid::new_v4()).await.unwrap();
        // Force the on-disk debit into the past so compact picks it up.
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = raw.lines().map(String::from).collect();
        for line in lines.iter_mut() {
            if line.contains("\"debit\"") {
                let ev: BudgetEvent = serde_json::from_str(line).unwrap();
                if let BudgetEvent::Debit(mut d) = ev {
                    d.at_ms = 50;
                    *line = serde_json::to_string(&BudgetEvent::Debit(d)).unwrap();
                }
            }
        }
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        // Reopen so in-memory matches the rewritten file, then compact.
        let l2 = JsonlLedger::open(path.clone()).await.unwrap();
        l2.try_debit(&a, 2, Uuid::new_v4()).await.unwrap();
        let purged = l2.compact_older_than(100).await.unwrap();
        assert_eq!(purged, 1);
        assert!(!path.with_extension("jsonl.tmp").exists());

        // Reopen — only the surviving Debit remains, plus the
        // CapacitySet event. The bucket replays from CapacitySet (full)
        // minus 2 (the surviving debit) = 8.
        let l3 = JsonlLedger::open(path).await.unwrap();
        let debits = l3.recent_debits(&a, 10).await.unwrap();
        assert_eq!(debits.len(), 1);
        assert_eq!(debits[0].credits, 2);
        let after = l3.tokens_remaining(&a).await.unwrap();
        assert!(
            after >= 8,
            "expected ≥8 after compact-then-replay, got {after}"
        );
    }

    #[tokio::test]
    async fn jsonl_compact_no_op_when_nothing_old() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        let a = agent("a@local");
        let l = JsonlLedger::open(path.clone()).await.unwrap();
        l.set_capacity(&a, 10).await.unwrap();
        l.try_debit(&a, 1, Uuid::new_v4()).await.unwrap();
        assert_eq!(l.compact_older_than(100).await.unwrap(), 0);
        assert!(!path.with_extension("jsonl.tmp").exists());
        // Surviving debit still on disk.
        assert_eq!(l.recent_debits(&a, 10).await.unwrap().len(), 1);
    }

    /// Non-destructive compact: pre-cutoff Debits are dropped but a
    /// per-agent Snapshot captures the state at cutoff, so reopening
    /// reconstructs the same `tokens_remaining` as before the rewrite.
    #[tokio::test]
    async fn jsonl_compact_replay_yields_same_state_as_pre_compact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        let a = agent("a@local");
        let l = JsonlLedger::open(path.clone()).await.unwrap();
        l.set_capacity(&a, 10).await.unwrap();
        l.try_debit(&a, 1, Uuid::new_v4()).await.unwrap();
        l.try_debit(&a, 2, Uuid::new_v4()).await.unwrap();
        // Rewrite all event timestamps into the past so compact sees
        // them as droppable. CapacitySet AND Debits both pre-cutoff.
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut shifted: Vec<String> = Vec::new();
        let mut t: u64 = 0;
        for line in raw.lines() {
            let mut ev: BudgetEvent = serde_json::from_str(line).unwrap();
            match &mut ev {
                BudgetEvent::CapacitySet { at_ms, .. } => *at_ms = t,
                BudgetEvent::Debit(d) => d.at_ms = t,
                BudgetEvent::Snapshot { at_ms, .. } => *at_ms = t,
            }
            t += 10;
            shifted.push(serde_json::to_string(&ev).unwrap());
        }
        std::fs::write(&path, shifted.join("\n") + "\n").unwrap();

        // Reopen so in-memory state matches the rewritten timestamps,
        // then snapshot tokens_remaining BEFORE compact.
        let l2 = JsonlLedger::open(path.clone()).await.unwrap();
        let pre = l2.tokens_remaining(&a).await.unwrap();
        let purged = l2.compact_older_than(100).await.unwrap();
        assert!(
            purged >= 2,
            "expected at least 2 Debits dropped, got {purged}"
        );

        // Reopen the compacted file: tokens_remaining must equal pre
        // (the bucket state at cutoff is the Snapshot's checkpoint).
        let l3 = JsonlLedger::open(path).await.unwrap();
        let post = l3.tokens_remaining(&a).await.unwrap();
        assert_eq!(
            post, pre,
            "compact then reopen must preserve tokens_remaining; pre={pre} post={post}"
        );
        // The dropped Debit history is gone but the surviving in-memory
        // log is empty (compact cleared it of pre-cutoff entries).
        assert!(l3.recent_debits(&a, 10).await.unwrap().is_empty());
    }

    /// `set_capacity` is idempotent on capacity match. A re-stamp of
    /// `last_refill_ms = now` every boot would prevent slow-rate buckets
    /// from refilling on restart-heavy deployments.
    #[tokio::test]
    async fn in_memory_set_capacity_idempotent_when_unchanged() {
        let l = InMemoryLedger::new();
        let a = agent("a@local");
        l.set_capacity(&a, 10).await.unwrap();
        // Burn 1, leaving 9.
        l.try_debit(&a, 1, Uuid::new_v4()).await.unwrap();
        // Stash last_refill_ms so we can verify the no-op below leaves
        // it untouched.
        let before = {
            let buckets = l.buckets.lock().await;
            buckets.get(&a.pubkey).unwrap().last_refill_ms
        };
        // Re-set with the same capacity — must be a no-op on the clock.
        l.set_capacity(&a, 10).await.unwrap();
        let after = {
            let buckets = l.buckets.lock().await;
            buckets.get(&a.pubkey).unwrap().last_refill_ms
        };
        assert_eq!(before, after, "last_refill_ms must not be re-stamped");
        assert_eq!(l.tokens_remaining(&a).await.unwrap(), 9);
    }

    #[tokio::test]
    async fn in_memory_set_capacity_re_stamps_when_capacity_changes() {
        let l = InMemoryLedger::new();
        let a = agent("a@local");
        l.set_capacity(&a, 10).await.unwrap();
        l.try_debit(&a, 4, Uuid::new_v4()).await.unwrap();
        // Capacity change still re-stamps and clamps tokens (pre-existing
        // semantic from `set_capacity_clamps_tokens_when_shrinking`).
        l.set_capacity(&a, 5).await.unwrap();
        assert_eq!(l.tokens_remaining(&a).await.unwrap(), 5);
    }

    #[tokio::test]
    async fn jsonl_set_capacity_idempotent_does_not_append_second_row() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        let a = agent("a@local");
        let l = JsonlLedger::open(path.clone()).await.unwrap();
        l.set_capacity(&a, 10).await.unwrap();
        let after_first = std::fs::read_to_string(&path).unwrap();
        // Idempotent re-call must not append.
        l.set_capacity(&a, 10).await.unwrap();
        let after_second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            after_first, after_second,
            "second set_capacity with unchanged capacity must not write"
        );
    }

    #[tokio::test]
    async fn jsonl_concurrent_debits_do_not_race_past_capacity() {
        // Hardening test for the file_lock guarantee. Without the lock,
        // two simultaneous debits could both observe tokens_remaining=N
        // and both spend it, over-debiting by `credits`.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        let a = agent("a@local");
        let l = std::sync::Arc::new(JsonlLedger::open(path).await.unwrap());
        l.set_capacity(&a, 10).await.unwrap();

        let mut handles = Vec::new();
        for _ in 0..20 {
            let l = l.clone();
            let a = a.clone();
            handles.push(tokio::spawn(async move {
                l.try_debit(&a, 1, Uuid::new_v4()).await
            }));
        }
        let mut ok = 0;
        let mut exhausted = 0;
        for h in handles {
            match h.await.unwrap() {
                Ok(()) => ok += 1,
                Err(BudgetError::Exhausted { .. }) => exhausted += 1,
                Err(other) => panic!("unexpected error: {other:?}"),
            }
        }
        assert_eq!(
            ok + exhausted,
            20,
            "all 20 attempts must resolve to Ok or Exhausted"
        );
        // capacity 10, refill rate is 10/hr (negligible over the test's
        // microsecond runtime). At most 10 should pass; the rest should
        // see Exhausted.
        assert!(ok <= 10, "no more than 10 debits should pass, got {ok}");
        assert_eq!(
            l.tokens_remaining(&a).await.unwrap() + ok as u64,
            10,
            "tokens spent + tokens remaining must equal capacity"
        );
    }

    #[test]
    fn pause_checkpoint_event_round_trips_with_stable_fields() {
        let a = agent("a@local");
        let intent_id = Uuid::from_u128(1);
        let event = BudgetCheckpointEvent::PauseSaved {
            checkpoint: checkpoint(&a, intent_id),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "pause_saved");
        assert_eq!(
            value["checkpoint"]["version"],
            BudgetPauseCheckpoint::VERSION
        );
        assert_eq!(value["checkpoint"]["intent_id"], intent_id.to_string());
        assert_eq!(value["checkpoint"]["agent"]["display"], "a@local");
        assert_eq!(value["checkpoint"]["reason"], "budget_exhausted");
        assert_eq!(value["checkpoint"]["requested_credits"], 4);
        assert_eq!(value["checkpoint"]["tokens_remaining"], 1);
        assert_eq!(value["checkpoint"]["resume_state"]["cursor"]["step"], 2);

        let back: BudgetCheckpointEvent = serde_json::from_value(value).unwrap();
        assert_eq!(back, event);
    }

    #[tokio::test]
    async fn pause_checkpoint_claim_is_single_use_and_preserves_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.jsonl");
        let checkpoint_path = dir.path().join("checkpoints.jsonl");
        let a = agent("a@local");
        let intent_id = Uuid::from_u128(7);
        let ledger = JsonlLedger::open(ledger_path).await.unwrap();
        let store = JsonlPauseCheckpointStore::open(checkpoint_path)
            .await
            .unwrap();

        ledger.set_capacity(&a, 10).await.unwrap();
        ledger.try_debit(&a, 4, Uuid::from_u128(11)).await.unwrap();
        let tokens_after_debit = ledger.tokens_remaining(&a).await.unwrap();

        let mut saved = checkpoint(&a, intent_id);
        saved.tokens_remaining = tokens_after_debit;
        store.save_pause(saved.clone()).await.unwrap();
        assert_eq!(
            store
                .active_pause(intent_id, &a)
                .await
                .expect("checkpoint should be active"),
            saved
        );

        let claimed = store.claim_resume(intent_id, &a, 3_000).await.unwrap();
        assert_eq!(claimed, saved);
        let err = store.claim_resume(intent_id, &a, 4_000).await.unwrap_err();
        assert!(matches!(err, BudgetCheckpointError::AlreadyResumed(_)));
        assert!(store.active_pause(intent_id, &a).await.is_none());

        let debits = ledger.recent_debits(&a, 10).await.unwrap();
        assert_eq!(debits.len(), 1);
        assert_eq!(debits[0].credits, 4);
        assert!(ledger.tokens_remaining(&a).await.unwrap() >= tokens_after_debit);
    }

    #[tokio::test]
    async fn pause_checkpoint_replays_resume_state_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("checkpoints.jsonl");
        let a = agent("a@local");
        let intent_id = Uuid::from_u128(8);
        let saved = checkpoint(&a, intent_id);
        {
            let store = JsonlPauseCheckpointStore::open(path.clone()).await.unwrap();
            store.save_pause(saved.clone()).await.unwrap();
        }
        {
            let store = JsonlPauseCheckpointStore::open(path.clone()).await.unwrap();
            assert_eq!(store.active_pause(intent_id, &a).await, Some(saved.clone()));
            assert_eq!(
                store.claim_resume(intent_id, &a, 5_000).await.unwrap(),
                saved
            );
        }
        let store = JsonlPauseCheckpointStore::open(path).await.unwrap();
        assert!(store.active_pause(intent_id, &a).await.is_none());
        let err = store.claim_resume(intent_id, &a, 6_000).await.unwrap_err();
        assert!(matches!(err, BudgetCheckpointError::AlreadyResumed(_)));
    }

    #[tokio::test]
    async fn pause_checkpoint_replay_rejects_duplicate_claims() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("checkpoints.jsonl");
        let a = agent("a@local");
        let intent_id = Uuid::from_u128(9);
        let saved = BudgetCheckpointEvent::PauseSaved {
            checkpoint: checkpoint(&a, intent_id),
        };
        let claimed = BudgetCheckpointEvent::ResumeClaimed {
            intent_id,
            agent: a,
            resumed_at_ms: 5_000,
        };
        let lines = [
            serde_json::to_string(&saved).unwrap(),
            serde_json::to_string(&claimed).unwrap(),
            serde_json::to_string(&claimed).unwrap(),
        ]
        .join("\n");
        tokio::fs::write(&path, format!("{lines}\n")).await.unwrap();

        let result = JsonlPauseCheckpointStore::open(path).await;
        assert!(matches!(
            result,
            Err(BudgetCheckpointError::AlreadyResumed(_))
        ));
    }

    #[tokio::test]
    async fn pause_checkpoint_rejects_machine_local_resume_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("checkpoints.jsonl");
        let store = JsonlPauseCheckpointStore::open(path).await.unwrap();
        let a = agent("a@local");
        let mut saved = checkpoint(&a, Uuid::from_u128(10));
        saved.resume_state.insert(
            "scratch".to_string(),
            json!({"local": "/tmp/covenant-state"}),
        );

        let err = store.save_pause(saved).await.unwrap_err();
        let message = err.to_string();
        assert!(matches!(err, BudgetCheckpointError::InvalidCheckpoint(_)));
        assert!(message.contains("machine-local path"));
        assert!(
            !message.contains("/tmp"),
            "error messages must not echo machine-local paths"
        );
    }
}
