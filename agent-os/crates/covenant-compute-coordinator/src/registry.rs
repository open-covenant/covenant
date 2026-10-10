//! Operator registry: signed registration + heartbeat bookkeeping, and
//! the per-operator long-poll delivery queue `GET
//! /federation/operators/:id/next-job` reads from. In-memory for v1
//! (design-02-federation.md §2.1 picks a coordinator-run registry over
//! on-chain/gossip discovery for live, second-by-second state);
//! persistence across restarts is an open seam, see
//! `build-notes-phase1-coordinator.md`.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use covenant_compute_protocol::{
    validate_address_b58, CapabilityProfile, HeartbeatRequest, JobOffer, OperatorStatus,
    RegisterRequest, HEARTBEAT_MAX_SKEW_MS,
};
use parking_lot::Mutex;
use subtle::ConstantTimeEq;
use tokio::sync::Notify;
use uuid::Uuid;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegistryError {
    #[error("signature: {0}")]
    BadSignature(String),
    #[error("operator {0} is not registered")]
    NotRegistered(String),
    #[error("missing or invalid session bearer token for this operator")]
    BadSession,
    #[error(
        "heartbeat ts_ms is outside the {0}ms freshness window; a stale beat is refused so a \
         captured one can't be replayed to keep a dead operator matchable"
    )]
    StaleHeartbeat(u64),
    #[error("operator registry is at its capacity of {0}; not accepting new registrations")]
    RegistryFull(usize),
    #[error(
        "{0}; the coordinator would accept work it can never pay out, so registration is refused"
    )]
    UnpayablePayout(String),
    #[error("invalid capability profile: {0}")]
    InvalidProfile(String),
}

/// What the matcher (and any operator-facing status view) reads.
#[derive(Debug, Clone)]
pub struct OperatorRecord {
    pub profile: CapabilityProfile,
    pub payout_address: String,
    /// The signed partner attribution this operator registered with,
    /// if any — copied onto each job it wins so rev-share accrual
    /// survives the in-memory registry.
    pub referral_code: Option<String>,
    pub status: OperatorStatus,
    pub queue_depth: u32,
    pub last_seen_ms: u64,
    pub session_token: String,
    /// Whether the operator holds the CVNT stake this deployment requires
    /// for its node identity. Always true where no stake is required;
    /// where one is, false until the chain has been read and says so.
    pub staked: bool,
    /// The wallets behind the stake that counts, base58. Two operators
    /// sharing one are one party, so a check never pairs them with each
    /// other. Empty where no stake is required.
    pub stake_owners: Vec<String>,
}

struct OperatorSlot {
    record: OperatorRecord,
    pending: VecDeque<JobOffer>,
    notify: Arc<Notify>,
}

/// Every operator that has ever registered, keyed by
/// `AgentId::pubkey_base58()`. A restart-surviving re-register keeps
/// any offer still sitting in `pending` — a redelivered operator
/// process shouldn't lose an in-flight job because it had to restart.
#[derive(Default)]
pub struct OperatorRegistry {
    operators: Mutex<HashMap<String, OperatorSlot>>,
}

fn mint_session_token() -> String {
    format!("sess_{}", Uuid::new_v4().simple())
}

impl OperatorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Verifies `req`'s signature, then inserts or refreshes the
    /// operator's record and mints a fresh opaque session token
    /// (`RegisterResponse::operator_session`) — the bearer the
    /// operator must present on `next-job`/`accept` afterward.
    ///
    /// `max_operators` is the volumetric backstop for a coordinator
    /// fronting traffic directly (C9): registration is open by design,
    /// and every accepted one grows this in-memory map, so a cap bounds
    /// what a registration flood can allocate. Checked under the same
    /// lock as the insert, so concurrent registrations can't overshoot.
    /// A KNOWN operator re-registering always succeeds — the cap
    /// refuses growth, never a restart-surviving node coming back.
    pub fn register(
        &self,
        req: &RegisterRequest,
        now_ms: u64,
        max_operators: Option<usize>,
        stake_required: bool,
    ) -> Result<String, RegistryError> {
        req.verify()
            .map_err(|e| RegistryError::BadSignature(e.to_string()))?;
        // The coordinator is the money-mover: an operator registered
        // with an unpayable address would win jobs whose payout push
        // fails forever, retried by a sweep that can't fix a typo.
        // Refused on every register — re-registration updates the
        // stored address, so the gate holds the invariant that the
        // registry never carries an address a transfer can't land on.
        // The node refuses the same defect at boot; this is the wire
        // gate for operators that don't run our node binary.
        validate_address_b58("payout address", &req.payout_address)
            .map_err(|e| RegistryError::UnpayablePayout(e.to_string()))?;
        // The declared model and hardware labels ride this registration
        // into the public capacity directory every buyer reads. Anyone
        // holding a keypair can register, so refuse a label carrying a
        // control character (an ANSI escape a client would render) or an
        // unreasonable length here, the same wire gate the payout check
        // is — a client must not have to sanitize what the coordinator
        // vouches for.
        req.profile
            .validate_labels()
            .map_err(|e| RegistryError::InvalidProfile(e.to_string()))?;
        // A lease-serving operator whose price unit the lease floor can't
        // scale to the window would be matched and metered below its rate.
        // The node refuses the same defect at boot; this is the wire gate
        // for operators that don't run our node binary.
        req.profile
            .validate_lease_pricing()
            .map_err(|e| RegistryError::InvalidProfile(e.to_string()))?;
        let key = req.profile.operator.pubkey_base58();
        let session_token = mint_session_token();
        let mut guard = self.operators.lock();
        if let Some(cap) = max_operators {
            if !guard.contains_key(&key) && guard.len() >= cap {
                return Err(RegistryError::RegistryFull(cap));
            }
        }
        let slot = guard.entry(key).or_insert_with(|| OperatorSlot {
            record: OperatorRecord {
                profile: req.profile.clone(),
                payout_address: req.payout_address.clone(),
                referral_code: req.referral_code.clone(),
                status: OperatorStatus::Online,
                queue_depth: 0,
                last_seen_ms: now_ms,
                session_token: session_token.clone(),
                staked: !stake_required,
                stake_owners: Vec::new(),
            },
            pending: VecDeque::new(),
            notify: Arc::new(Notify::new()),
        });
        slot.record.profile = req.profile.clone();
        slot.record.payout_address = req.payout_address.clone();
        slot.record.referral_code = req.referral_code.clone();
        slot.record.status = OperatorStatus::Online;
        slot.record.last_seen_ms = now_ms;
        slot.record.session_token = session_token.clone();
        // A re-register declares a fresh start (a node reboots and
        // re-registers), so drop any stale pre-crash queue depth — the
        // next heartbeat carries the real one. Matches the new-slot init;
        // without it the matcher/capacity view briefly reads the node
        // busier than it is.
        slot.record.queue_depth = 0;
        Ok(session_token)
    }

    /// Records what the chain says about an operator's stake. A re-register
    /// keeps the last reading, so a restarting node is not dropped from
    /// matching while its stake is re-read.
    pub fn set_staked(&self, operator_pubkey_b58: &str, staked: bool) {
        if let Some(slot) = self.operators.lock().get_mut(operator_pubkey_b58) {
            slot.record.staked = staked;
        }
    }

    pub fn set_stake_owners(&self, operator_pubkey_b58: &str, owners: Vec<String>) {
        if let Some(slot) = self.operators.lock().get_mut(operator_pubkey_b58) {
            slot.record.stake_owners = owners;
        }
    }

    /// Returns the status the operator held before this beat, so the
    /// caller can react to transitions (the offline heal fires on
    /// `!= Offline` -> `Offline`, nothing else).
    pub fn heartbeat(
        &self,
        req: &HeartbeatRequest,
        now_ms: u64,
    ) -> Result<OperatorStatus, RegistryError> {
        req.verify()
            .map_err(|e| RegistryError::BadSignature(e.to_string()))?;
        // A verified heartbeat still replays: the same signed bytes
        // refresh liveness every time. Bound it to the signed `ts_ms`'s
        // distance from now, exactly as the withdrawal/unbond/dispute
        // paths bound their signed requests — otherwise one captured
        // `Online` beat keeps a crashed node matchable indefinitely.
        if now_ms.abs_diff(req.ts_ms) > HEARTBEAT_MAX_SKEW_MS {
            return Err(RegistryError::StaleHeartbeat(HEARTBEAT_MAX_SKEW_MS));
        }
        let key = req.operator.pubkey_base58();
        let mut guard = self.operators.lock();
        let slot = guard
            .get_mut(&key)
            .ok_or_else(|| RegistryError::NotRegistered(key.clone()))?;
        let previous = slot.record.status;
        slot.record.status = req.status;
        slot.record.queue_depth = req.queue_depth;
        slot.record.last_seen_ms = now_ms;
        Ok(previous)
    }

    /// Session-bearer check for the two operator endpoints whose wire
    /// messages carry no signature of their own (`next-job`, `accept` —
    /// see build-notes-phase1-coordinator.md's auth-boundary note).
    /// `result` needs no separate check: `SignedWorkReceipt` is
    /// self-authenticating against the job's assigned operator pubkey.
    pub fn check_session(
        &self,
        operator_pubkey_b58: &str,
        bearer: Option<&str>,
    ) -> Result<(), RegistryError> {
        let guard = self.operators.lock();
        let slot = guard
            .get(operator_pubkey_b58)
            .ok_or_else(|| RegistryError::NotRegistered(operator_pubkey_b58.to_string()))?;
        // Constant-time compare: the token is a 122-bit random value, so
        // a timing oracle is not a practical recovery vector, but the
        // sibling auth crate (`covenant-peer-auth`) already holds every
        // bearer/secret comparison to this bar, and an auth boundary in
        // front of real money should not be the one place that leaks a
        // byte-compare timing signal.
        match bearer {
            Some(token)
                if bool::from(token.as_bytes().ct_eq(slot.record.session_token.as_bytes())) =>
            {
                Ok(())
            }
            _ => Err(RegistryError::BadSession),
        }
    }

    pub fn record(&self, operator_pubkey_b58: &str) -> Option<OperatorRecord> {
        self.operators
            .lock()
            .get(operator_pubkey_b58)
            .map(|s| s.record.clone())
    }

    /// Snapshot of every registered operator, keyed by pubkey_b58 —
    /// what [`crate::matcher::select_operator`] filters and sorts over.
    pub fn snapshot(&self) -> Vec<(String, OperatorRecord)> {
        self.operators
            .lock()
            .iter()
            .map(|(k, v)| (k.clone(), v.record.clone()))
            .collect()
    }

    /// Pushes an offer into an operator's pending queue and wakes any
    /// in-flight long-poll waiter. Returns `false` if the operator
    /// isn't registered — the caller should treat that as a match
    /// failure (the winner came from a stale snapshot).
    ///
    /// Idempotent per job: an offer whose `job_id` is already queued
    /// for this operator is not queued twice, so the stale-offer
    /// sweep's redelivery can't make a node that was merely slow see —
    /// and try to serve — the same job as two offers.
    pub fn deliver(&self, operator_pubkey_b58: &str, offer: JobOffer) -> bool {
        let mut guard = self.operators.lock();
        let Some(slot) = guard.get_mut(operator_pubkey_b58) else {
            return false;
        };
        let job_id = offer.envelope.payload.job_id;
        if slot
            .pending
            .iter()
            .any(|o| o.envelope.payload.job_id == job_id)
        {
            return true;
        }
        slot.pending.push_back(offer);
        slot.notify.notify_one();
        true
    }

    /// Whether an offer for `job_id` still sits undelivered in this
    /// operator's pending queue. The stale-offer sweep's tell: a queue
    /// still holding the offer after the whole re-offer window means
    /// the operator isn't polling — re-match around it. A queue
    /// without it means the offer was polled out (the accept guard's
    /// domain) or died with a coordinator restart (redeliver).
    pub fn queue_holds(&self, operator_pubkey_b58: &str, job_id: Uuid) -> bool {
        self.operators
            .lock()
            .get(operator_pubkey_b58)
            .is_some_and(|slot| {
                slot.pending
                    .iter()
                    .any(|o| o.envelope.payload.job_id == job_id)
            })
    }

    /// Removes a queued offer for `job_id` from an operator's pending
    /// queue, so a job re-offered elsewhere isn't also polled by the
    /// assignee it moved away from. Best-effort: an offer the operator
    /// already polled out is beyond reach here, which is what the
    /// accept-side assignee guard exists for. Returns whether a queued
    /// offer was actually removed.
    pub fn revoke(&self, operator_pubkey_b58: &str, job_id: Uuid) -> bool {
        let mut guard = self.operators.lock();
        let Some(slot) = guard.get_mut(operator_pubkey_b58) else {
            return false;
        };
        let before = slot.pending.len();
        slot.pending.retain(|o| o.envelope.payload.job_id != job_id);
        slot.pending.len() < before
    }

    /// Long-polls for the next offer, waiting up to `wait` before
    /// returning `Ok(None)`. Never holds the registry lock across the
    /// wait: check-pending, clone the per-operator `Notify`, release
    /// the lock, then wait — `Notify`'s stored-permit semantics close
    /// the race where `deliver` fires between the check and the wait.
    ///
    /// A poll is proof of life, so it refreshes `last_seen_ms` (to
    /// `now_ms`, the poll's arrival time) the same way a heartbeat
    /// does — the matcher's staleness cutoff must not drop an operator
    /// that is actively asking for work.
    pub async fn poll_next_job(
        &self,
        operator_pubkey_b58: &str,
        wait: Duration,
        now_ms: u64,
    ) -> Result<Option<JobOffer>, RegistryError> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let notify = {
                let mut guard = self.operators.lock();
                let slot = guard
                    .get_mut(operator_pubkey_b58)
                    .ok_or_else(|| RegistryError::NotRegistered(operator_pubkey_b58.to_string()))?;
                slot.record.last_seen_ms = slot.record.last_seen_ms.max(now_ms);
                if let Some(offer) = slot.pending.pop_front() {
                    return Ok(Some(offer));
                }
                slot.notify.clone()
            };
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            let _ = tokio::time::timeout(remaining, notify.notified()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
    use covenant_compute_protocol::{
        CapabilityRequirement, EscrowHoldAttestation, FundingSource, HardwareClass,
        JobEnvelopePayload, JobKind, PriceAsk, PriceUnit, SignedJobEnvelope,
    };
    use covenant_identity::LocalIdentity;
    use covenant_mcp::Content;

    #[test]
    fn a_newcomer_is_unstaked_until_read_and_a_reregister_keeps_the_verdict() {
        let registry = OperatorRegistry::new();
        let operator = LocalIdentity::generate("operator@stake");
        let key = operator.agent_id().pubkey_base58();
        let req = RegisterRequest::sign(profile(&operator), payout(1), &operator).unwrap();
        registry.register(&req, 1_000, None, true).unwrap();
        assert!(!registry.record(&key).unwrap().staked);
        registry.set_staked(&key, true);
        registry.register(&req, 2_000, None, true).unwrap();
        assert!(
            registry.record(&key).unwrap().staked,
            "a restarting node keeps its standing while the stake is re-read"
        );
        let open = OperatorRegistry::new();
        open.register(&req, 1_000, None, false).unwrap();
        assert!(
            open.record(&key).unwrap().staked,
            "no requirement, nothing to hold"
        );
    }

    fn profile(identity: &LocalIdentity) -> CapabilityProfile {
        CapabilityProfile {
            operator: identity.agent_id(),
            hardware: HardwareClass::CpuOnly,
            vram_gb: 0,
            models_served: vec!["any".into()],
            job_kinds: vec![JobKind::BatchJob],
            price: PriceAsk {
                unit: PriceUnit::PerJob,
                micro_usdc: 100,
            },
            tee_capable: false,
            kind_prices: Vec::new(),
            kind_models: Vec::new(),
        }
    }

    fn payout(seed: u8) -> String {
        bs58::encode([seed; 32]).into_string()
    }

    fn offer(coordinator: &LocalIdentity, buyer: &LocalIdentity, job_id: Uuid) -> JobOffer {
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
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "registry-test"),
            issued_at_ms: 0,
            referral_code: None,
            stream: false,
        };
        let envelope = SignedJobEnvelope::sign(payload, buyer).unwrap();
        let escrow_hold =
            EscrowHoldAttestation::sign(job_id, 100, FundingSource::Organic, 0, coordinator)
                .unwrap();
        JobOffer {
            envelope,
            escrow_hold,
            rework: None,
            reproduction: None,
        }
    }

    #[test]
    fn register_then_heartbeat_updates_status() {
        let registry = OperatorRegistry::new();
        let operator = LocalIdentity::generate("operator@local");
        let req = RegisterRequest::sign(profile(&operator), payout(1), &operator).unwrap();
        registry.register(&req, 1_000, None, false).unwrap();

        let hb = HeartbeatRequest::sign(
            operator.agent_id(),
            OperatorStatus::Busy,
            3,
            2_000,
            &operator,
        )
        .unwrap();
        assert_eq!(
            registry.heartbeat(&hb, 2_000).unwrap(),
            OperatorStatus::Online,
            "the beat reports the status it replaced"
        );

        let record = registry
            .record(&operator.agent_id().pubkey_base58())
            .unwrap();
        assert_eq!(record.status, OperatorStatus::Busy);
        assert_eq!(record.queue_depth, 3);
        assert_eq!(record.last_seen_ms, 2_000);

        let hb = HeartbeatRequest::sign(
            operator.agent_id(),
            OperatorStatus::Offline,
            0,
            3_000,
            &operator,
        )
        .unwrap();
        assert_eq!(
            registry.heartbeat(&hb, 3_000).unwrap(),
            OperatorStatus::Busy
        );
    }

    #[test]
    fn re_register_clears_a_stale_queue_depth() {
        let registry = OperatorRegistry::new();
        let operator = LocalIdentity::generate("operator@local");
        let key = operator.agent_id().pubkey_base58();
        let req = RegisterRequest::sign(profile(&operator), payout(1), &operator).unwrap();
        registry.register(&req, 1_000, None, false).unwrap();

        let hb = HeartbeatRequest::sign(
            operator.agent_id(),
            OperatorStatus::Busy,
            5,
            2_000,
            &operator,
        )
        .unwrap();
        registry.heartbeat(&hb, 2_000).unwrap();
        assert_eq!(registry.record(&key).unwrap().queue_depth, 5);

        // A re-register (the node rebooted) must not keep the pre-crash
        // depth — the next heartbeat carries the real one.
        let req = RegisterRequest::sign(profile(&operator), payout(1), &operator).unwrap();
        registry.register(&req, 3_000, None, false).unwrap();
        assert_eq!(
            registry.record(&key).unwrap().queue_depth,
            0,
            "a fresh registration reports an empty queue until the next beat"
        );
    }

    #[test]
    fn register_rejects_bad_signature() {
        let registry = OperatorRegistry::new();
        let operator = LocalIdentity::generate("operator@local");
        let mut req = RegisterRequest::sign(profile(&operator), payout(1), &operator).unwrap();
        // "tampered" is also unpayable — expecting BadSignature pins
        // that verification runs before the payout gate, so a forged
        // request never reaches address validation.
        req.payout_address = "tampered".into();
        assert!(matches!(
            registry.register(&req, 1_000, None, false),
            Err(RegistryError::BadSignature(_))
        ));
    }

    #[test]
    fn register_refuses_an_unpayable_payout_address() {
        let registry = OperatorRegistry::new();
        let operator = LocalIdentity::generate("operator@local");
        // Honestly signed — the address itself is the one defect, in
        // both classes: not base58 at all, and too short to be a key.
        for bad in ["not-an-address", &bs58::encode([7u8; 8]).into_string()] {
            let req =
                RegisterRequest::sign(profile(&operator), bad.to_string(), &operator).unwrap();
            match registry.register(&req, 1_000, None, false) {
                Err(RegistryError::UnpayablePayout(m)) => {
                    assert!(m.contains("payout address"), "names the defect: {m}")
                }
                other => panic!("expected UnpayablePayout, got {other:?}"),
            }
        }

        // Nothing was admitted: the operator's heartbeat finds no record.
        let hb = HeartbeatRequest::sign(
            operator.agent_id(),
            OperatorStatus::Online,
            0,
            1_500,
            &operator,
        )
        .unwrap();
        assert!(matches!(
            registry.heartbeat(&hb, 1_500),
            Err(RegistryError::NotRegistered(_))
        ));
    }

    #[test]
    fn register_refuses_a_control_char_in_a_declared_model() {
        let registry = OperatorRegistry::new();
        let operator = LocalIdentity::generate("operator@local");
        // Honestly signed, with a payable address — the declared model
        // name is the one defect: an ANSI escape that would otherwise
        // ride into every buyer's capacity view and rewrite their
        // terminal. The payout being valid proves the label check runs
        // even once the payout gate passes.
        let mut prof = profile(&operator);
        prof.models_served = vec!["gpt-4\x1b[2Kfree".into()];
        let payout = bs58::encode([7u8; 32]).into_string();
        let req = RegisterRequest::sign(prof, payout, &operator).unwrap();
        match registry.register(&req, 1_000, None, false) {
            Err(RegistryError::InvalidProfile(m)) => {
                assert!(m.contains("control character"), "names the defect: {m}")
            }
            other => panic!("expected InvalidProfile, got {other:?}"),
        }

        // Nothing was admitted: the operator's heartbeat finds no record.
        let hb = HeartbeatRequest::sign(
            operator.agent_id(),
            OperatorStatus::Online,
            0,
            1_500,
            &operator,
        )
        .unwrap();
        assert!(matches!(
            registry.heartbeat(&hb, 1_500),
            Err(RegistryError::NotRegistered(_))
        ));
    }

    #[test]
    fn register_refuses_a_lease_priced_by_a_window_blind_unit() {
        let registry = OperatorRegistry::new();
        let operator = LocalIdentity::generate("operator@local");
        // Honestly signed, payable address — the one defect is a lease
        // operator pricing per GPU-second, a unit the lease floor never
        // scales to the window, so it would be matched and metered below
        // its advertised rate.
        let mut prof = profile(&operator);
        prof.job_kinds = vec![JobKind::LeaseSession];
        prof.price = PriceAsk {
            unit: PriceUnit::PerGpuSecond,
            micro_usdc: 1_000,
        };
        let payout = bs58::encode([7u8; 32]).into_string();
        let req = RegisterRequest::sign(prof, payout, &operator).unwrap();
        match registry.register(&req, 1_000, None, false) {
            Err(RegistryError::InvalidProfile(m)) => {
                assert!(m.contains("lease-serving operator must price"), "{m}")
            }
            other => panic!("expected InvalidProfile, got {other:?}"),
        }

        // A PerLeaseHour lease from the same operator is admitted.
        let mut ok = profile(&operator);
        ok.job_kinds = vec![JobKind::LeaseSession];
        ok.price = PriceAsk {
            unit: PriceUnit::PerLeaseHour,
            micro_usdc: 3_600_000,
        };
        let payout = bs58::encode([7u8; 32]).into_string();
        let req = RegisterRequest::sign(ok, payout, &operator).unwrap();
        assert!(registry.register(&req, 1_000, None, false).is_ok());
    }

    #[test]
    fn a_full_registry_refuses_strangers_but_never_a_returning_operator() {
        let registry = OperatorRegistry::new();
        let resident = LocalIdentity::generate("resident@local");
        let resident_req = RegisterRequest::sign(profile(&resident), payout(1), &resident).unwrap();
        registry
            .register(&resident_req, 1_000, Some(1), false)
            .unwrap();

        // The registry is at its cap: a NEW operator bounces...
        let newcomer = LocalIdentity::generate("newcomer@local");
        let newcomer_req = RegisterRequest::sign(profile(&newcomer), payout(2), &newcomer).unwrap();
        assert!(matches!(
            registry.register(&newcomer_req, 2_000, Some(1), false),
            Err(RegistryError::RegistryFull(1))
        ));

        // ...but the resident re-registering (a node restart) is growth
        // of nothing and always lands.
        registry
            .register(&resident_req, 3_000, Some(1), false)
            .expect("re-register at cap");

        // And no cap means no ceiling.
        registry
            .register(&newcomer_req, 4_000, None, false)
            .expect("uncapped registration");
    }

    #[test]
    fn a_stale_heartbeat_is_refused_and_leaves_liveness_untouched() {
        let registry = OperatorRegistry::new();
        let operator = LocalIdentity::generate("operator@local");
        let req = RegisterRequest::sign(profile(&operator), payout(1), &operator).unwrap();
        registry.register(&req, 1_000, None, false).unwrap();
        let key = operator.agent_id().pubkey_base58();

        // A heartbeat the operator signed once, captured and replayed
        // long after: its ts_ms is now far in the past.
        let captured = HeartbeatRequest::sign(
            operator.agent_id(),
            OperatorStatus::Online,
            0,
            1_000,
            &operator,
        )
        .unwrap();
        let replay_at = 1_000 + HEARTBEAT_MAX_SKEW_MS + 1;
        assert!(matches!(
            registry.heartbeat(&captured, replay_at),
            Err(RegistryError::StaleHeartbeat(_))
        ));
        assert_eq!(
            registry.record(&key).unwrap().last_seen_ms,
            1_000,
            "a refused replay must not refresh liveness"
        );

        // A beat close to the coordinator's clock still lands.
        let fresh = HeartbeatRequest::sign(
            operator.agent_id(),
            OperatorStatus::Online,
            0,
            replay_at,
            &operator,
        )
        .unwrap();
        registry.heartbeat(&fresh, replay_at).unwrap();
        assert_eq!(registry.record(&key).unwrap().last_seen_ms, replay_at);
    }

    #[test]
    fn heartbeat_before_register_is_rejected() {
        let registry = OperatorRegistry::new();
        let operator = LocalIdentity::generate("operator@local");
        let hb =
            HeartbeatRequest::sign(operator.agent_id(), OperatorStatus::Online, 0, 1, &operator)
                .unwrap();
        assert!(matches!(
            registry.heartbeat(&hb, 1),
            Err(RegistryError::NotRegistered(_))
        ));
    }

    #[test]
    fn check_session_requires_the_minted_token() {
        let registry = OperatorRegistry::new();
        let operator = LocalIdentity::generate("operator@local");
        let req = RegisterRequest::sign(profile(&operator), payout(1), &operator).unwrap();
        let session = registry.register(&req, 1_000, None, false).unwrap();
        let key = operator.agent_id().pubkey_base58();

        assert!(registry.check_session(&key, Some(&session)).is_ok());
        assert!(matches!(
            registry.check_session(&key, Some("wrong")),
            Err(RegistryError::BadSession)
        ));
        assert!(matches!(
            registry.check_session(&key, None),
            Err(RegistryError::BadSession)
        ));
    }

    #[tokio::test]
    async fn poll_next_job_returns_a_pending_offer_immediately() {
        let registry = OperatorRegistry::new();
        let operator = LocalIdentity::generate("operator@local");
        let buyer = LocalIdentity::generate("buyer@local");
        let coordinator = LocalIdentity::generate("coordinator@local");
        let req = RegisterRequest::sign(profile(&operator), payout(1), &operator).unwrap();
        registry.register(&req, 0, None, false).unwrap();

        let key = operator.agent_id().pubkey_base58();
        let job_id = Uuid::new_v4();
        assert!(registry.deliver(&key, offer(&coordinator, &buyer, job_id)));

        let got = registry
            .poll_next_job(&key, Duration::from_millis(50), 1)
            .await
            .unwrap()
            .expect("offer should be immediately available");
        assert_eq!(got.envelope.payload.job_id, job_id);
    }

    #[tokio::test]
    async fn deliver_queues_an_already_queued_job_only_once() {
        let registry = OperatorRegistry::new();
        let operator = LocalIdentity::generate("operator@local");
        let buyer = LocalIdentity::generate("buyer@local");
        let coordinator = LocalIdentity::generate("coordinator@local");
        let req = RegisterRequest::sign(profile(&operator), payout(1), &operator).unwrap();
        registry.register(&req, 0, None, false).unwrap();
        let key = operator.agent_id().pubkey_base58();

        let job_id = Uuid::new_v4();
        assert!(registry.deliver(&key, offer(&coordinator, &buyer, job_id)));
        assert!(
            registry.deliver(&key, offer(&coordinator, &buyer, job_id)),
            "a redelivery is acknowledged, not an error"
        );

        let first = registry
            .poll_next_job(&key, Duration::from_millis(20), 1)
            .await
            .unwrap()
            .expect("the offer is queued once");
        assert_eq!(first.envelope.payload.job_id, job_id);
        assert!(
            registry
                .poll_next_job(&key, Duration::from_millis(20), 2)
                .await
                .unwrap()
                .is_none(),
            "no second copy of the same job"
        );
    }

    #[tokio::test]
    async fn revoke_removes_only_the_named_job_from_the_queue() {
        let registry = OperatorRegistry::new();
        let operator = LocalIdentity::generate("operator@local");
        let buyer = LocalIdentity::generate("buyer@local");
        let coordinator = LocalIdentity::generate("coordinator@local");
        let req = RegisterRequest::sign(profile(&operator), payout(1), &operator).unwrap();
        registry.register(&req, 0, None, false).unwrap();
        let key = operator.agent_id().pubkey_base58();

        let moved = Uuid::new_v4();
        let kept = Uuid::new_v4();
        registry.deliver(&key, offer(&coordinator, &buyer, moved));
        registry.deliver(&key, offer(&coordinator, &buyer, kept));

        assert!(registry.revoke(&key, moved));
        assert!(!registry.revoke(&key, moved), "already gone");
        assert!(!registry.revoke("never-registered", moved));

        let next = registry
            .poll_next_job(&key, Duration::from_millis(20), 1)
            .await
            .unwrap()
            .expect("the untouched offer survives");
        assert_eq!(next.envelope.payload.job_id, kept);
        assert!(registry
            .poll_next_job(&key, Duration::from_millis(20), 2)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn poll_next_job_wakes_on_a_concurrent_deliver() {
        let registry = Arc::new(OperatorRegistry::new());
        let operator = LocalIdentity::generate("operator@local");
        let buyer = LocalIdentity::generate("buyer@local");
        let coordinator = LocalIdentity::generate("coordinator@local");
        let req = RegisterRequest::sign(profile(&operator), payout(1), &operator).unwrap();
        registry.register(&req, 0, None, false).unwrap();
        let key = operator.agent_id().pubkey_base58();

        let poller = {
            let registry = registry.clone();
            let key = key.clone();
            tokio::spawn(async move {
                registry
                    .poll_next_job(&key, Duration::from_secs(5), 1)
                    .await
                    .unwrap()
            })
        };

        tokio::time::sleep(Duration::from_millis(30)).await;
        let job_id = Uuid::new_v4();
        assert!(registry.deliver(&key, offer(&coordinator, &buyer, job_id)));

        let got = poller.await.unwrap().expect("delivered while waiting");
        assert_eq!(got.envelope.payload.job_id, job_id);
    }

    #[tokio::test]
    async fn poll_next_job_refreshes_last_seen() {
        let registry = OperatorRegistry::new();
        let operator = LocalIdentity::generate("operator@local");
        let req = RegisterRequest::sign(profile(&operator), payout(1), &operator).unwrap();
        registry.register(&req, 0, None, false).unwrap();
        let key = operator.agent_id().pubkey_base58();

        registry
            .poll_next_job(&key, Duration::from_millis(10), 7_000)
            .await
            .unwrap();
        assert_eq!(
            registry.record(&key).unwrap().last_seen_ms,
            7_000,
            "a long-poll is proof of life"
        );

        // A stale now_ms (e.g. a slow request racing a fresher
        // heartbeat) must never move liveness backwards.
        registry
            .poll_next_job(&key, Duration::from_millis(10), 6_000)
            .await
            .unwrap();
        assert_eq!(registry.record(&key).unwrap().last_seen_ms, 7_000);
    }

    #[tokio::test]
    async fn poll_next_job_times_out_to_none_when_empty() {
        let registry = OperatorRegistry::new();
        let operator = LocalIdentity::generate("operator@local");
        let req = RegisterRequest::sign(profile(&operator), payout(1), &operator).unwrap();
        registry.register(&req, 0, None, false).unwrap();
        let key = operator.agent_id().pubkey_base58();

        let got = registry
            .poll_next_job(&key, Duration::from_millis(30), 1)
            .await
            .unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn poll_next_job_unknown_operator_errors() {
        let registry = OperatorRegistry::new();
        assert!(matches!(
            registry
                .poll_next_job("never-registered", Duration::from_millis(10), 1)
                .await,
            Err(RegistryError::NotRegistered(_))
        ));
    }
}
