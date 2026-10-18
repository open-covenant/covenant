//! Cross-process rolling budget, separate from the existing per-session caps.
//! Every pending/uncertain reservation counts indefinitely until reconciled.
//! A stable lock inode guards atomic, fsynced state replacement on local disk.
use covenant_compute_protocol::SignedJobEnvelope;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;

const DAY_MS: u64 = 86_400_000;
#[derive(Debug, thiserror::Error)]
pub enum BudgetError {
    #[error("budget storage: {0}")]
    Io(#[from] std::io::Error),
    #[error("budget state is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("budget refused: {0}")]
    Refused(String),
}
type Result<T> = std::result::Result<T, BudgetError>;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ReservationState {
    Pending,
    /// An explicit submit refusal is retriable, but must reserve again.
    Unsubmitted,
    Final {
        charged_micro_usdc: u64,
        settled_at_ms: u64,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reservation {
    pub job_id: Uuid,
    pub request_sha256: String,
    pub ceiling_micro_usdc: u64,
    pub reserved_at_ms: u64,
    pub state: ReservationState,
    pub envelope: SignedJobEnvelope,
}
#[derive(Serialize, Deserialize)]
struct State {
    version: u8,
    buyer: String,
    endpoint: String,
    watermark_ms: u64,
    limit_micro_usdc: u64,
    entries: BTreeMap<Uuid, Reservation>,
}
#[derive(Debug, Clone)]
pub struct DurableBudget {
    directory: PathBuf,
    buyer: String,
    endpoint: String,
    limit: u64,
}
#[derive(Debug, Serialize)]
pub struct BudgetStatus {
    pub limit_micro_usdc: u64,
    pub settled_24h_micro_usdc: u64,
    pub pending_micro_usdc: u64,
    pub remaining_micro_usdc: u64,
    pub pending_jobs: Vec<Uuid>,
}

pub fn canonical_endpoint(endpoint: &str) -> Result<String> {
    let mut url = reqwest::Url::parse(endpoint)
        .map_err(|_| BudgetError::Refused("invalid coordinator URL".into()))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(BudgetError::Refused(
            "coordinator URL must have no credentials, query or fragment".into(),
        ));
    }
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&path);
    Ok(url.as_str().trim_end_matches('/').to_owned())
}
impl DurableBudget {
    pub fn open(root: &Path, buyer: &str, endpoint: &str, limit: u64) -> Result<Self> {
        if limit == 0 || buyer.is_empty() {
            return Err(BudgetError::Refused(
                "budget and buyer must be nonzero".into(),
            ));
        }
        let endpoint = canonical_endpoint(endpoint)?;
        let key =
            Sha256::digest(format!("covenant.compute.budget.v1\0{buyer}\0{endpoint}").as_bytes());
        let directory = root.join(key.iter().map(|b| format!("{b:02x}")).collect::<String>());
        std::fs::create_dir_all(&directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            directory,
            buyer: buyer.into(),
            endpoint,
            limit,
        })
    }
    pub fn from_env(buyer: &str, endpoint: &str) -> Result<Option<Self>> {
        let limit = match std::env::var("COVENANT_COMPUTE_DAILY_BUDGET_MICRO_USDC") {
            Err(std::env::VarError::NotPresent) => return Ok(None),
            Ok(v) => v.parse().map_err(|_| {
                BudgetError::Refused(
                    "COVENANT_COMPUTE_DAILY_BUDGET_MICRO_USDC must be a positive integer".into(),
                )
            })?,
            Err(_) => {
                return Err(BudgetError::Refused(
                    "invalid daily budget configuration".into(),
                ))
            }
        };
        Self::open_with_env_root(buyer, endpoint, limit).map(Some)
    }
    /// Resolve the shared namespace without changing the process environment.
    /// Explicit limit changes use the caller's expected persisted limit here.
    pub fn open_with_env_root(buyer: &str, endpoint: &str, limit: u64) -> Result<Self> {
        // Common to CLI/MCP/gateway homes: identities sharing an endpoint must
        // share a budget namespace even if they have distinct session homes.
        let root = match std::env::var("COVENANT_COMPUTE_BUDGET_HOME") {
            Ok(value) if !value.is_empty() => PathBuf::from(value),
            Err(std::env::VarError::NotPresent) => {
                PathBuf::from(std::env::var("HOME").map_err(|_| {
                    BudgetError::Refused("HOME is required for durable budgets".into())
                })?)
                .join(".covenant-compute-budgets")
            }
            _ => return Err(BudgetError::Refused("invalid budget home".into())),
        };
        if !root.is_absolute() {
            return Err(BudgetError::Refused("budget home must be absolute".into()));
        }
        Self::open(&root, buyer, endpoint, limit)
    }
    fn transact<T>(&self, now: u64, change: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
        self.transact_inner(now, false, change)
    }
    fn transact_inner<T>(
        &self,
        now: u64,
        limit_update: bool,
        change: impl FnOnce(&mut State) -> Result<T>,
    ) -> Result<T> {
        let lock_path = self.directory.join("lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(lock_path)?;
        lock.lock()?;
        let path = self.directory.join("budget.json");
        let mut state = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<State>(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !limit_update => State {
                version: 1,
                buyer: self.buyer.clone(),
                endpoint: self.endpoint.clone(),
                watermark_ms: 0,
                limit_micro_usdc: self.limit,
                entries: BTreeMap::new(),
            },
            Err(e) => return Err(e.into()),
        };
        if state.version != 1 || state.buyer != self.buyer || state.endpoint != self.endpoint {
            return Err(BudgetError::Refused(
                "budget namespace/version mismatch".into(),
            ));
        }
        if state.limit_micro_usdc != self.limit {
            if limit_update {
                return Err(BudgetError::Refused(format!(
                    "expected limit {}, but persisted limit is {}; re-read the budget before changing it",
                    self.limit, state.limit_micro_usdc
                )));
            }
            return Err(BudgetError::Refused(
                "processes sharing a buyer/endpoint must use the same persisted daily ceiling"
                    .into(),
            ));
        }
        if now < state.watermark_ms {
            return Err(BudgetError::Refused(
                "clock moved backwards; retain reservations and correct the clock".into(),
            ));
        }
        let value = change(&mut state)?;
        if !limit_update {
            state.watermark_ms = now;
        }
        let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
        serde_json::to_writer(file.as_file_mut(), &state)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(&path).map_err(|e| e.error)?;
        File::open(&self.directory)?.sync_all()?;
        // File's drop releases the lock after the rename and directory fsync.
        Ok(value)
    }
    fn totals(state: &State, now: u64) -> Result<(u64, u64)> {
        let mut pending = 0u64;
        let mut spent = 0u64;
        for row in state.entries.values() {
            match row.state {
                ReservationState::Pending => {
                    pending = pending
                        .checked_add(row.ceiling_micro_usdc)
                        .ok_or_else(|| BudgetError::Refused("pending total overflow".into()))?
                }
                ReservationState::Final {
                    charged_micro_usdc,
                    settled_at_ms,
                } if now.saturating_sub(settled_at_ms) < DAY_MS => {
                    spent = spent
                        .checked_add(charged_micro_usdc)
                        .ok_or_else(|| BudgetError::Refused("settled total overflow".into()))?
                }
                _ => {}
            }
        }
        Ok((pending, spent))
    }
    pub fn reserve(&self, envelope: &SignedJobEnvelope, now: u64) -> Result<()> {
        self.reserve_inner(envelope, now, false)
    }
    /// Reserve a job exactly once, including across processes and restarts.
    /// Even an explicitly refused or settled ID remains spent as an identifier:
    /// callers reconcile it by reading, never authorize another submission.
    pub fn reserve_new(&self, envelope: &SignedJobEnvelope, now: u64) -> Result<()> {
        self.reserve_inner(envelope, now, true)
    }
    fn reserve_inner(
        &self,
        envelope: &SignedJobEnvelope,
        now: u64,
        require_new: bool,
    ) -> Result<()> {
        envelope
            .verify()
            .map_err(|e| BudgetError::Refused(format!("invalid envelope: {e}")))?;
        if envelope.payload.buyer.pubkey_base58() != self.buyer {
            return Err(BudgetError::Refused("wrong buyer identity".into()));
        }
        let job_id = envelope.payload.job_id;
        let digest = crate::sha256_hex(envelope.payload_json.as_bytes());
        let ceiling = envelope.payload.price_micro_usdc;
        self.transact(now,|state| {
            if let Some(row)=state.entries.get(&job_id) {
                if require_new {return Err(BudgetError::Refused("job id was already reserved; use read-only reconciliation, never submit it again".into()));}
                if row.request_sha256!=digest || row.ceiling_micro_usdc!=ceiling {return Err(BudgetError::Refused("job id is bound to another request".into()));}
                if row.state!=ReservationState::Unsubmitted {return Ok(());}
            }
            let (pending,spent)=Self::totals(state,now)?;
            let total=pending.checked_add(spent).and_then(|v|v.checked_add(ceiling));
            if total.is_none_or(|n|n>self.limit) {return Err(BudgetError::Refused(format!("rolling 24h ceiling: {spent} settled + {pending} pending + {ceiling} requested > {}",self.limit)));}
            state.entries.insert(job_id,Reservation {job_id,request_sha256:digest,ceiling_micro_usdc:ceiling,reserved_at_ms:now,state:ReservationState::Pending,envelope:envelope.clone()});
            Ok(())
        })
    }
    pub fn submit_refused(&self, id: Uuid, now: u64) -> Result<()> {
        self.transact(now, |state| {
            let row = state
                .entries
                .get_mut(&id)
                .ok_or_else(|| BudgetError::Refused("missing reservation".into()))?;
            if row.state == ReservationState::Pending {
                row.state = ReservationState::Unsubmitted;
            }
            Ok(())
        })
    }
    pub fn settle(&self, id: Uuid, charged: u64, now: u64) -> Result<()> {
        self.transact(now, |state| {
            let row = state
                .entries
                .get_mut(&id)
                .ok_or_else(|| BudgetError::Refused("missing reservation".into()))?;
            if let ReservationState::Final {
                charged_micro_usdc, ..
            } = row.state
            {
                return if charged_micro_usdc == charged {
                    Ok(())
                } else {
                    Err(BudgetError::Refused("conflicting terminal charge".into()))
                };
            }
            if charged > row.ceiling_micro_usdc {
                return Err(BudgetError::Refused(
                    "settlement exceeds reserved ceiling".into(),
                ));
            }
            row.state = ReservationState::Final {
                charged_micro_usdc: charged,
                settled_at_ms: now,
            };
            Ok(())
        })
    }
    pub fn pending(&self, now: u64) -> Result<Vec<Reservation>> {
        self.transact(now, |state| {
            Ok(state
                .entries
                .values()
                .filter(|r| r.state == ReservationState::Pending)
                .cloned()
                .collect())
        })
    }
    pub fn status(&self, now: u64) -> Result<BudgetStatus> {
        self.transact(now, |state| Self::status_of(state, now))
    }
    fn status_of(state: &State, now: u64) -> Result<BudgetStatus> {
        let (pending, spent) = Self::totals(state, now)?;
        Ok(BudgetStatus {
            limit_micro_usdc: state.limit_micro_usdc,
            settled_24h_micro_usdc: spent,
            pending_micro_usdc: pending,
            remaining_micro_usdc: state
                .limit_micro_usdc
                .saturating_sub(pending.saturating_add(spent)),
            pending_jobs: state
                .entries
                .values()
                .filter(|r| r.state == ReservationState::Pending)
                .map(|r| r.job_id)
                .collect(),
        })
    }
    /// Compare-and-set the persisted limit using this handle's current limit
    /// as the expectation. Every reservation and the clock watermark survive.
    /// Old handles refuse further operations after a successful change.
    pub fn set_limit(&self, new_limit: u64, now: u64) -> Result<BudgetStatus> {
        if new_limit == 0 {
            return Err(BudgetError::Refused(
                "budget must be a positive integer".into(),
            ));
        }
        self.transact_inner(now, true, |state| {
            let (pending, spent) = Self::totals(state, now)?;
            let committed = pending
                .checked_add(spent)
                .ok_or_else(|| BudgetError::Refused("committed total overflow".into()))?;
            if new_limit < committed {
                return Err(BudgetError::Refused(format!(
                    "new limit {new_limit} is below {pending} pending + {spent} settled"
                )));
            }
            state.limit_micro_usdc = new_limit;
            Self::status_of(state, now)
        })
    }
}

fn buyer_error(error: BudgetError) -> crate::BuyerError {
    crate::BuyerError::Protocol(error.to_string())
}
/// Reconcile only authoritative terminal states; absence, timeout or bad proof
/// retains the full reservation. No external request is dispatched here.
pub async fn reconcile(
    budget: &DurableBudget,
    http: &reqwest::Client,
    config: &crate::BuyerConfig,
    identity: &covenant_identity::LocalIdentity,
) -> std::result::Result<(), crate::BuyerError> {
    for row in budget.pending(crate::epoch_ms()).map_err(buyer_error)? {
        let Ok(value) = crate::read_job_status(http, config, identity, row.job_id).await else {
            continue;
        };
        let Ok(job) = serde_json::from_value::<crate::JobStatusResponse>(value) else {
            continue;
        };
        let charge = match job.status.as_str() {
            "completed" => {
                let Some(receipt) = &job.receipt else {
                    continue;
                };
                let Some(output) = &job.output else {
                    continue;
                };
                if crate::verify_receipt(&row.envelope, receipt, output).is_err() {
                    continue;
                }
                if receipt.receipt.status != covenant_a2a::A2ATaskStatus::Ok {
                    continue;
                }
                // The coordinator's metered-charge metadata is not signed.
                // Count the complete authorized ceiling until a signed charge
                // statement exists; never reclaim headroom from that field.
                row.ceiling_micro_usdc
            }
            "failed" => {
                let Some(receipt) = &job.receipt else {
                    continue;
                };
                let Some(output) = &job.output else {
                    continue;
                };
                if crate::verify_receipt(&row.envelope, receipt, output).is_err()
                    || receipt.receipt.status == covenant_a2a::A2ATaskStatus::Ok
                    || job.charged_micro_usdc != Some(0)
                {
                    continue;
                }
                0
            }
            // An unsigned status alone cannot release a reservation.
            _ => continue,
        };
        budget
            .settle(row.job_id, charge, crate::epoch_ms())
            .map_err(buyer_error)?;
    }
    Ok(())
}

pub async fn before_submit(
    http: &reqwest::Client,
    config: &crate::BuyerConfig,
    identity: &covenant_identity::LocalIdentity,
    envelope: &SignedJobEnvelope,
) -> std::result::Result<Option<DurableBudget>, crate::BuyerError> {
    let budget = DurableBudget::from_env(
        &identity.agent_id().pubkey_base58(),
        &config.coordinator_url,
    )
    .map_err(buyer_error)?;
    if let Some(budget) = &budget {
        reconcile(budget, http, config, identity).await?;
        budget
            .reserve(envelope, crate::epoch_ms())
            .map_err(buyer_error)?;
    }
    Ok(budget)
}

pub fn tool_spec() -> covenant_mcp::ToolSpec {
    covenant_mcp::ToolSpec {name:"compute.budget".into(),description:"Read this buyer and coordinator's persistent rolling-24-hour spend ceiling. Optional reconcile reads existing jobs only; uncertain reservations remain held and it never dispatches work.".into(),input_schema:serde_json::json!({"type":"object","properties":{"reconcile":{"type":"boolean","default":false}},"additionalProperties":false})}
}
pub async fn describe(
    http: &reqwest::Client,
    config: &crate::BuyerConfig,
    identity: &covenant_identity::LocalIdentity,
    refresh: bool,
) -> std::result::Result<serde_json::Value, crate::BuyerError> {
    let Some(budget) = DurableBudget::from_env(
        &identity.agent_id().pubkey_base58(),
        &config.coordinator_url,
    )
    .map_err(buyer_error)?
    else {
        return Ok(serde_json::json!({"enabled":false}));
    };
    if refresh {
        reconcile(&budget, http, config, identity).await?;
    }
    let status = budget.status(crate::epoch_ms()).map_err(buyer_error)?;
    Ok(serde_json::json!({"enabled":true,"status":status}))
}

pub const BUDGET_TOOL: &str = "compute.budget";

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_identity::LocalIdentity;
    fn envelope(identity: &LocalIdentity, price: u64) -> SignedJobEnvelope {
        crate::sign_envelope(
            &crate::BuyerConfig {
                coordinator_url: "https://compute.example".into(),
                poll_interval: std::time::Duration::from_millis(1),
                referral_code: None,
                rpc_url: None,
            },
            identity,
            crate::JobRequest {
                kind: covenant_compute_protocol::JobKind::InferenceCall,
                input: vec![],
                model: Some("test".into()),
                gpu_class: None,
                min_vram_gb: None,
                min_reputation_bps: None,
                price_micro_usdc: price,
                deadline_ms: 60_000,
            },
        )
        .unwrap()
    }
    #[test]
    fn restart_retains_uncertain_reservations_beyond_24h() {
        let dir = tempfile::tempdir().unwrap();
        let identity = LocalIdentity::generate("budget@local");
        let buyer = identity.agent_id().pubkey_base58();
        let envelope = envelope(&identity, 60);
        DurableBudget::open(dir.path(), &buyer, "https://compute.example/", 100)
            .unwrap()
            .reserve(&envelope, 1)
            .unwrap();
        let book = DurableBudget::open(dir.path(), &buyer, "https://compute.example", 100).unwrap();
        assert_eq!(book.status(DAY_MS * 3).unwrap().pending_micro_usdc, 60);
        assert!(book
            .reserve(&self::envelope(&identity, 60), DAY_MS * 3)
            .is_err());
        book.settle(envelope.payload.job_id, 20, DAY_MS * 3)
            .unwrap();
        book.settle(envelope.payload.job_id, 20, DAY_MS * 3)
            .unwrap();
        assert!(book
            .settle(envelope.payload.job_id, 21, DAY_MS * 3)
            .is_err());
        assert_eq!(book.status(DAY_MS * 4).unwrap().remaining_micro_usdc, 100);
    }
    #[test]
    fn changed_request_overflow_clock_rollback_and_corrupt_state_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let identity = LocalIdentity::generate("budget@local");
        let book = DurableBudget::open(
            dir.path(),
            &identity.agent_id().pubkey_base58(),
            "https://compute.example",
            100,
        )
        .unwrap();
        let env = envelope(&identity, 60);
        book.reserve(&env, 100).unwrap();
        let mut changed = env.payload.clone();
        changed.price_micro_usdc = 20;
        let changed = SignedJobEnvelope::sign(changed, &identity).unwrap();
        assert!(book.reserve(&changed, 100).is_err());
        assert!(book.status(99).is_err());
        assert!(book.settle(env.payload.job_id, 61, 101).is_err());
        std::fs::write(book.directory.join("budget.json"), b"{torn").unwrap();
        assert!(book.reserve(&env, 200).is_err());
    }
    #[test]
    fn a_single_attempt_id_is_never_reserved_again_even_after_refusal_or_settlement() {
        let dir = tempfile::tempdir().unwrap();
        let identity = LocalIdentity::generate("single-attempt@local");
        let buyer = identity.agent_id().pubkey_base58();
        let book = DurableBudget::open(dir.path(), &buyer, "https://compute.example", 100).unwrap();
        let first = envelope(&identity, 40);
        book.reserve_new(&first, 100).unwrap();
        assert!(book.reserve_new(&first, 101).is_err());
        book.submit_refused(first.payload.job_id, 102).unwrap();
        assert!(book.reserve_new(&first, 103).is_err());
        let second = envelope(&identity, 40);
        book.reserve_new(&second, 104).unwrap();
        book.settle(second.payload.job_id, 20, 105).unwrap();
        let reopened =
            DurableBudget::open(dir.path(), &buyer, "https://compute.example", 100).unwrap();
        assert!(reopened.reserve_new(&second, DAY_MS * 2).is_err());
        assert_eq!(
            reopened.status(DAY_MS * 2).unwrap().remaining_micro_usdc,
            100
        );
    }

    #[test]
    fn explicit_submit_refusal_must_reserve_again_but_terminal_replay_never_recharges() {
        let dir = tempfile::tempdir().unwrap();
        let identity = LocalIdentity::generate("budget@local");
        let book = DurableBudget::open(
            dir.path(),
            &identity.agent_id().pubkey_base58(),
            "https://compute.example",
            100,
        )
        .unwrap();
        let env = envelope(&identity, 60);
        book.reserve(&env, 100).unwrap();
        book.submit_refused(env.payload.job_id, 101).unwrap();
        assert_eq!(book.status(102).unwrap().remaining_micro_usdc, 100);
        book.reserve(&env, 103).unwrap();
        assert_eq!(book.status(104).unwrap().pending_micro_usdc, 60);
        book.settle(env.payload.job_id, 20, 105).unwrap();
        book.reserve(&env, 106).unwrap();
        assert_eq!(book.status(107).unwrap().settled_24h_micro_usdc, 20);
        assert_eq!(book.status(107).unwrap().pending_micro_usdc, 0);
    }
    #[test]
    fn set_limit_preserves_every_entry_and_watermark_and_invalidates_old_handles() {
        let dir = tempfile::tempdir().unwrap();
        let identity = LocalIdentity::generate("budget@local");
        let buyer = identity.agent_id().pubkey_base58();
        let book = DurableBudget::open(dir.path(), &buyer, "https://compute.example", 100).unwrap();
        let expired = envelope(&identity, 30);
        book.reserve(&expired, 1).unwrap();
        book.settle(expired.payload.job_id, 30, 2).unwrap();
        let pending = envelope(&identity, 40);
        book.reserve(&pending, 3).unwrap();
        let settled = envelope(&identity, 20);
        book.reserve(&settled, DAY_MS + 3).unwrap();
        book.settle(settled.payload.job_id, 20, DAY_MS + 4).unwrap();
        let unsubmitted = envelope(&identity, 10);
        book.reserve(&unsubmitted, DAY_MS + 5).unwrap();
        book.submit_refused(unsubmitted.payload.job_id, DAY_MS + 6)
            .unwrap();
        let path = book.directory.join("budget.json");
        let before: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let status = book.set_limit(200, DAY_MS + 10).unwrap();
        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let mut expected = before;
        expected["limit_micro_usdc"] = 200.into();
        assert_eq!(after, expected, "only the persisted ceiling may change");
        assert_eq!(status.pending_micro_usdc, 40);
        assert_eq!(status.settled_24h_micro_usdc, 20);
        assert_eq!(status.remaining_micro_usdc, 140);
        assert_eq!(status.pending_jobs, vec![pending.payload.job_id]);
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            book.set_limit(300, DAY_MS + 11).is_err(),
            "stale expectation"
        );
        assert!(
            book.status(DAY_MS + 11).is_err(),
            "old process configuration"
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let updated =
            DurableBudget::open(dir.path(), &buyer, "https://compute.example", 200).unwrap();
        assert!(
            updated.set_limit(300, DAY_MS + 5).is_err(),
            "clock rollback"
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(updated.status(DAY_MS + 11).unwrap().pending_micro_usdc, 40);
    }
    #[test]
    fn set_limit_refuses_zero_missing_state_and_reductions_below_commitments() {
        let dir = tempfile::tempdir().unwrap();
        let identity = LocalIdentity::generate("budget@local");
        let book = DurableBudget::open(
            dir.path(),
            &identity.agent_id().pubkey_base58(),
            "https://compute.example",
            100,
        )
        .unwrap();
        let path = book.directory.join("budget.json");
        assert!(book.set_limit(200, 1).is_err());
        assert!(!path.exists(), "an expected limit must already exist");
        let pending = envelope(&identity, 40);
        let settled = envelope(&identity, 30);
        book.reserve(&pending, 1).unwrap();
        book.reserve(&settled, 2).unwrap();
        book.settle(settled.payload.job_id, 20, 3).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        for limit in [0, 39, 59] {
            assert!(book.set_limit(limit, 4).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        let status = book.set_limit(60, 4).unwrap();
        assert_eq!(status.limit_micro_usdc, 60);
        assert_eq!(status.remaining_micro_usdc, 0);
        assert_eq!(status.pending_micro_usdc, 40);
        assert_eq!(status.settled_24h_micro_usdc, 20);
    }
    #[test]
    fn subprocess_reservation() {
        let Ok(path) = std::env::var("COVENANT_TEST_BUDGET_ENVELOPE") else {
            return;
        };
        let env: SignedJobEnvelope = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let root = std::env::var("COVENANT_TEST_BUDGET_ROOT").unwrap();
        let book = DurableBudget::open(
            Path::new(&root),
            &env.payload.buyer.pubkey_base58(),
            "https://compute.example",
            100,
        )
        .unwrap();
        if book.reserve(&env, 100).is_err() {
            std::process::exit(3);
        }
    }
    #[test]
    fn independent_processes_cannot_both_cross_the_shared_ceiling() {
        let dir = tempfile::tempdir().unwrap();
        let identity = LocalIdentity::generate("budget@local");
        let mut children = vec![];
        for n in 0..6 {
            let path = dir.path().join(format!("envelope-{n}.json"));
            std::fs::write(&path, serde_json::to_vec(&envelope(&identity, 60)).unwrap()).unwrap();
            children.push(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "durable_budget::tests::subprocess_reservation"])
                    .env("COVENANT_TEST_BUDGET_ENVELOPE", path)
                    .env("COVENANT_TEST_BUDGET_ROOT", dir.path())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap(),
            );
        }
        let successes = children
            .into_iter()
            .filter_map(|mut child| child.wait().ok())
            .filter(|status| status.success())
            .count();
        assert_eq!(successes, 1);
    }
}
