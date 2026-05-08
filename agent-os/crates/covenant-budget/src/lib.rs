//! Per-agent token-bucket ledger backing `Settlement.budget_credits_per_hour`
//! (manifest §5).
//!
//! Closes scaffolding for the last `00_spec.md` §11 pin: *when an agent
//! hits `budget_credits_per_hour`, the runtime pauses, persists partial
//! state, settles consumed credits, and queues a resume*. Sprint 58a
//! ships types + storage backends. Sprint 58b wires `dispatch_intent`
//! to call [`BudgetLedger::try_debit`] before spawning. Sprint 58c
//! tackles the actual mid-task pause/resume.
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
//!
//! ## Compaction trade-off (Sprint 58a)
//!
//! [`BudgetLedger::compact_older_than`] drops [`BudgetEvent::Debit`]
//! events older than the cutoff. Currently this is **destructive of
//! replay accuracy**: after compaction, reopening the ledger sees only
//! the surviving events and so reconstructs a bucket that never
//! consumed the dropped credits. The intent for production
//! (Sprint 58c) is to write a synthetic snapshot event before the
//! drop so the replay state stays correct. For Sprint 58a, operators
//! should compact only when the cutoff is older than any window the
//! refill rate cares about (i.e., older than ~1 hour past the slowest
//! refill rate the deployment uses).
//!
//! [`tokens_remaining`]: BudgetLedger::tokens_remaining
//! [`would_exceed`]: BudgetLedger::would_exceed
//! [`try_debit`]: BudgetLedger::try_debit

#![deny(unsafe_code)]

use async_trait::async_trait;
use covenant_types::AgentId;
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
    /// tokens to cover the requested debit (returned for the
    /// pause-and-queue logic in Sprint 58c).
    #[error(
        "budget exhausted: {tokens_remaining} tokens remaining, refill eta {refill_eta_ms} ms"
    )]
    Exhausted {
        tokens_remaining: u64,
        refill_eta_ms: u64,
    },
}

/// One debit event. Persisted to the JSONL log by [`JsonlLedger`] and
/// returned by [`BudgetLedger::recent_debits`] for the operator
/// dashboard / settlement-reconciliation paths.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BudgetDebit {
    pub agent: AgentId,
    pub credits: u64,
    /// The [`uuid::Uuid`] of the [`covenant_types::SettlementReceipt`]
    /// this debit pairs with. Sprint 58b/c will settle this pair on the
    /// runtime's settlement flush, so the budget log and the receipt
    /// log can be joined.
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
    /// the Sprint-58a compaction trade-off.
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
        entry.last_refill_ms = now;
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
        entry.last_refill_ms = now;
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

        let mut dropped: u64 = 0;
        let kept: Vec<&BudgetEvent> = events
            .iter()
            .filter(|ev| match ev {
                BudgetEvent::CapacitySet { .. } => true,
                BudgetEvent::Debit(d) => {
                    if d.at_ms < before_ms {
                        dropped += 1;
                        false
                    } else {
                        true
                    }
                }
            })
            .collect();
        if dropped == 0 {
            return Ok(0);
        }

        let tmp_path = self.path.with_extension("jsonl.tmp");
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)
            .await?;
        for ev in &kept {
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

    fn agent(name: &str) -> AgentId {
        let mut pk = [0u8; 32];
        for (i, b) in name.bytes().take(32).enumerate() {
            pk[i] = b;
        }
        AgentId::new(name, pk)
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
}
