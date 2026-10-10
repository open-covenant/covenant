//! Shared application state: everything a request handler needs, wired
//! once at startup and cloned (cheaply, via the inner `Arc`) into every
//! axum handler through `State<CoordinatorState>`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use covenant_audit::{AuditEvent, AuditKind, AuditLog};
use covenant_compute_protocol::FundingSource;
use covenant_identity::LocalIdentity;
use uuid::Uuid;

use crate::accounts::{BuyerAccounts, BuyerWithdrawals, PartnerPayouts};
use crate::bond::OperatorBonds;
use crate::deposit::InboundRail;
use crate::escrow::CustodialEscrow;
use crate::jobs::JobBook;
use crate::journal::{Journal, JournalError, StakeSlashState, StakeSlashStatus};
use crate::payout::Payout;
use crate::registry::OperatorRegistry;
use crate::reputation::ReputationSource;
use crate::stream::StreamBook;

/// One referral partner's terms (C8). The share is carved out of the
/// coordinator's own captured marketplace fee — never out of operator
/// pay, never out of the buyer's charge, never new money — so it can't
/// exceed the fee by construction, and a coordinator with no fee
/// accrues nothing however many referrals a partner has.
#[derive(Debug, Clone)]
pub struct PartnerConfig {
    payout_address: String,
    share_bps: u32,
}

impl PartnerConfig {
    /// Refuses a share above 10_000 bps: a partner can earn the whole
    /// fee, not more than exists.
    pub fn new(payout_address: String, share_bps: u32) -> Result<Self, String> {
        if share_bps > 10_000 {
            return Err(format!(
                "partner share {share_bps} bps exceeds 10_000 (the whole fee)"
            ));
        }
        Ok(Self {
            payout_address,
            share_bps,
        })
    }

    pub fn payout_address(&self) -> &str {
        &self.payout_address
    }

    pub fn share_bps(&self) -> u32 {
        self.share_bps
    }

    /// Floor of the partner's cut of a captured fee — the rounding
    /// dust stays with the coordinator, whose fee it was.
    pub fn share_of(&self, fee_micro_usdc: u64) -> u64 {
        let share = u128::from(fee_micro_usdc) * u128::from(self.share_bps) / 10_000;
        u64::try_from(share).unwrap_or(u64::MAX)
    }
}

#[derive(Clone)]
pub struct CoordinatorConfig {
    /// How long `GET /federation/operators/:id/next-job` hangs before
    /// answering `null` (design-02-federation.md §5, §6: long-poll for
    /// v1, ~30s). Kept configurable so tests don't have to wait 30s to
    /// exercise the empty-timeout path.
    pub long_poll_timeout: Duration,
    /// Funding tag `CustodialEscrow::hold` stamps on every hold it
    /// mints, since `FederationEscrow::hold`'s signature (fixed by
    /// `covenant-compute-protocol`) carries no funding_source
    /// parameter — see `build-notes-phase1-coordinator.md`'s design-choice
    /// note. `Organic` is the default: a real signed buyer envelope is
    /// real demand, even though no inbound payment collection is wired
    /// in this slice.
    pub default_funding_source: FundingSource,
    /// How long since an operator's last register/heartbeat/long-poll
    /// before the matcher treats it as gone. A node that crashes
    /// without an Offline heartbeat must stop being matchable on its
    /// own; 45s is 3x the node binary's default 15s heartbeat.
    pub operator_liveness_timeout: Duration,
    /// When true, an `Organic` job is only admitted if the buyer's
    /// verified deposits cover its price (402 otherwise). False is the
    /// pre-A3 open mode — holds are custodial promises with no inbound
    /// payment behind them — kept as the default until a deployment
    /// wires a real inbound rail; the mainnet cutover flips it.
    pub require_prefunded_buyers: bool,
    /// The anti-faucet gate for `Bootstrap` holds. `None` (default) is
    /// the kill-switch's off position: every bootstrap hold is refused,
    /// so a deployment whose `default_funding_source` is `Bootstrap`
    /// admits nothing until an explicit policy is set.
    pub subsidy_policy: Option<crate::escrow::SubsidyPolicy>,
    /// The marketplace take (C7), withheld from each release's payout
    /// push and disclosed in every `RegisterResponse` so an operator
    /// prices its ask knowing it. Default is zero: fee capture is a
    /// deployment decision, never a silent default. The escrow and the
    /// buyer are untouched by it — the buyer pays the envelope price,
    /// the hold releases gross, the split happens at payout.
    pub fee: covenant_compute_protocol::MarketplaceFee,
    /// Referral partners by code (C8). A registration carrying a code
    /// with no entry here is accepted and recorded, but accrues
    /// nothing — attribution is a claim, terms are a deployment
    /// decision.
    pub partners: std::collections::HashMap<String, PartnerConfig>,
    /// The matcher's reputation floor (C5): operators scoring below
    /// this win no organic work, however cheap their ask. `0` (the
    /// default) disables it — enforcing a trust tier is a deployment
    /// decision, like the fee. Floored operators still get canary
    /// probes (those pin their target), so passing probes is the road
    /// back above the floor.
    pub min_operator_score_bps: u32,
    /// The matcher's stake floor (C5 phase 2), a sibling to the score
    /// floor: operators whose committed bond sits below this win no
    /// organic work. `0` (the default) disables it — requiring stake is
    /// a deployment decision. Committed means posted minus slashed
    /// minus refunded minus pending unbonds, so an operator heading for
    /// the exit stops being matchable before its money leaves.
    pub min_bond_micro_usdc: u64,
    /// Rate scaling for the stake floor on GPU-lease supply: a lease
    /// operator pricing by the GPU-hour must have committed at least this
    /// many hours of its own advertised rate, on top of the flat floor.
    /// A premium card is trusted with more of a renter's money per hour,
    /// so it posts proportionally more stake; a cheap one posts less.
    /// `0` (the default) is a flat floor for everyone, unchanged.
    pub min_bond_lease_hours: u64,
    /// How long an unbond request matures before its refund pushes.
    /// The window is what makes a bond mean anything — the stake stays
    /// slashable while it runs, so a fault can't be outrun by
    /// unbonding. Zero pays matured refunds on the next sweep tick.
    pub unbond_window: Duration,
    /// Bearer token for the operator-only admin surface (today: the
    /// partner mark-paid endpoint). `None` (default) fails closed —
    /// every admin call is 401 until a deployment sets a token.
    pub admin_token: Option<String>,
    /// How long after a job concludes its buyer may still dispute it
    /// (C4). A dispute is a durable reputation fault, so it can't stay
    /// open forever — an operator's score has to be able to settle.
    /// Zero refuses every dispute.
    pub dispute_window: Duration,
    /// Volumetric backstops (C9) for a coordinator fronting traffic
    /// directly: registration and (in open mode) submission are free,
    /// so a flood grows the in-memory registry and the escrow/journal
    /// without bound. `None` (the default) is unlimited — the deploy
    /// posture behind a reverse proxy with its own limits. Known
    /// operators re-register past the cap; the ceiling counts only
    /// Offered/Accepted jobs, so concluded work never blocks a buyer.
    pub max_operators: Option<usize>,
    pub max_inflight_per_buyer: Option<usize>,
    /// How long a delivered offer may sit unaccepted before the sweep
    /// re-matches the job to whoever the matcher would pick now — the
    /// heal for an assignee that died holding an offer, and for the
    /// in-memory delivery queues a coordinator restart drops. A live
    /// node accepts within moments of its long poll, so anything past
    /// one poll cycle means the offer is going nowhere; the default
    /// gives it two. Zero disables re-offering entirely — the deadline
    /// sweep stays the only backstop.
    pub reoffer_after: Duration,
    /// The wire-version floor for `/federation/*` (the deploy-skew
    /// defense): a request declaring a `PROTOCOL_VERSION` below this
    /// is refused 426 with both numbers named, so an operator whose
    /// node predates a breaking wire change reads "upgrade this node"
    /// instead of a deserialization error. A request with no version
    /// header counts as 0, and `0` (the default) admits everyone —
    /// bare curl included — until a deployment raises the floor.
    /// `/health` and `/metrics` are never floored: deploy probes and
    /// scrapers don't speak the wire.
    pub min_protocol: u32,
    /// Whether to serve the public settlement proof feed (`/proof/*`):
    /// concluded jobs anyone can verify were paid on-chain for the work
    /// their operator signed. `false` (the default) keeps every receipt
    /// read buyer-gated — publishing per-job settlement facts, even with
    /// no buyer identity or job contents in them, is a deployment
    /// decision, so it is opt-in like the fee and the reputation floor.
    pub public_proof_feed: bool,
    /// Whether to serve the client-sealed secret vault (`/vault/*`): a
    /// buyer stores ciphertext under a key the coordinator never receives,
    /// and reads it back over a signed request. `false` (the default)
    /// serves no vault routes. It is opt-in because it has the coordinator
    /// keep durable per-owner state on the buyer's behalf — a real
    /// resource the deployment chooses to offer, even though what it holds
    /// is opaque.
    pub vault_enabled: bool,
    /// A ceiling on how many distinct owners the vault will admit, or
    /// `None` for no ceiling (the default). Keypairs are free to mint, so
    /// a vault exposed to the open internet wants a volumetric backstop
    /// the way `max_operators` does; a fronting proxy or this ceiling.
    pub vault_max_owners: Option<usize>,
    /// The on-chain lease meter (`crate::onchain_meter`). `None` (the
    /// default) leaves lease settlement exactly as it is: the
    /// coordinator's own clock meters the session and the custodial
    /// escrow settles it. With a meter configured, the same elapsed is
    /// mirrored onto the `compute-lease` program — opened and delegated
    /// at accept, ticked while the session runs, undelegated and
    /// settled when it ends. It lives on the config rather than in
    /// `new`'s signature so a deployment that wants no chain touches
    /// nothing.
    pub lease_meter: Option<Arc<dyn crate::onchain_meter::LeaseMeter>>,
    /// The CVNT stake an operator must hold for its node identity to win
    /// work (`crate::stake`). `None` (the default) requires none.
    pub stake: Option<crate::stake::StakeRequirement>,
    /// Agent work (`crate::agent`). `None` (the default) refuses every
    /// agent task.
    pub agent: Option<crate::agent::AgentPolicy>,
    /// Check-vote rounds (`crate::rounds`). `None` (the default) settles
    /// agent tasks on the coordinator's own count, as before rounds existed.
    pub vote_rounds: Option<Arc<dyn crate::rounds::VoteRounds>>,
    /// Where repositories too large to travel inside a job are stored for
    /// the operators to fetch (`crate::bundles`). `None` takes no uploads.
    pub bundles: Option<Arc<crate::bundles::BundleStore>>,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            long_poll_timeout: Duration::from_secs(30),
            default_funding_source: FundingSource::Organic,
            operator_liveness_timeout: Duration::from_secs(45),
            require_prefunded_buyers: false,
            subsidy_policy: None,
            fee: covenant_compute_protocol::MarketplaceFee::zero(),
            partners: std::collections::HashMap::new(),
            min_operator_score_bps: 0,
            min_bond_micro_usdc: 0,
            min_bond_lease_hours: 0,
            unbond_window: Duration::from_secs(24 * 60 * 60),
            admin_token: None,
            dispute_window: Duration::from_secs(24 * 60 * 60),
            max_operators: None,
            max_inflight_per_buyer: None,
            reoffer_after: Duration::from_secs(60),
            min_protocol: 0,
            public_proof_feed: false,
            vault_enabled: false,
            vault_max_owners: None,
            lease_meter: None,
            stake: None,
            agent: None,
            vote_rounds: None,
            bundles: None,
        }
    }
}

impl CoordinatorConfig {
    /// The matcher's committed-stake requirement: the flat floor plus its
    /// lease-rate scaling, as the one policy object every match and
    /// capacity read shares.
    pub fn bond_floor(&self) -> crate::matcher::BondFloor {
        crate::matcher::BondFloor::new(self.min_bond_micro_usdc, self.min_bond_lease_hours)
    }
}

/// One buyer's funds, derived on read: deposits are the accounts
/// ledger's monotonic total, charged is the escrow's non-refunded
/// organic holds, withdrawn is the withdrawal book's monotonic total.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct BuyerFunds {
    pub deposited_micro_usdc: u64,
    pub charged_micro_usdc: u64,
    pub withdrawn_micro_usdc: u64,
    pub available_micro_usdc: u64,
}

struct Inner {
    config: CoordinatorConfig,
    registry: OperatorRegistry,
    jobs: JobBook,
    escrow: CustodialEscrow,
    accounts: Arc<BuyerAccounts>,
    withdrawals: Arc<BuyerWithdrawals>,
    partner_payouts: PartnerPayouts,
    bonds: Arc<OperatorBonds>,
    attempts: crate::attempts::TransferAttempts,
    rail: Option<Arc<dyn InboundRail>>,
    reputation: Arc<dyn ReputationSource>,
    payout: Arc<dyn Payout>,
    audit: Arc<dyn AuditLog>,
    journal: Option<Arc<Journal>>,
    /// Live chunk relay for streaming jobs. In-memory on purpose —
    /// see `stream.rs`'s module doc for why this never journals.
    streams: StreamBook,
    /// The client-sealed secret vault, present only when the deployment
    /// opts in (`config.vault_enabled`). Durable when the coordinator
    /// runs with a journal, in-memory otherwise.
    vault: Option<Arc<crate::vault::VaultStore>>,
    /// Latest state per on-chain stake slash, keyed by `slash_id`.
    stake_slashes: parking_lot::Mutex<HashMap<String, StakeSlashState>>,
}

#[derive(Clone)]
pub struct CoordinatorState(Arc<Inner>);

impl CoordinatorState {
    pub fn new(
        identity: LocalIdentity,
        config: CoordinatorConfig,
        reputation: Arc<dyn ReputationSource>,
        payout: Arc<dyn Payout>,
        audit: Arc<dyn AuditLog>,
    ) -> Self {
        let accounts = Arc::new(BuyerAccounts::new());
        let withdrawals = Arc::new(BuyerWithdrawals::new());
        let mut escrow = CustodialEscrow::new(identity, config.default_funding_source)
            .with_withdrawals(withdrawals.clone());
        if config.require_prefunded_buyers {
            escrow = escrow.with_accounts(accounts.clone());
        }
        if let Some(policy) = config.subsidy_policy {
            escrow = escrow.with_subsidy_policy(policy);
        }
        let vault = config.vault_enabled.then(|| {
            Arc::new(crate::vault::VaultStore::in_memory().with_max_owners(config.vault_max_owners))
        });
        Self(Arc::new(Inner {
            config,
            registry: OperatorRegistry::new(),
            jobs: JobBook::new(),
            escrow,
            accounts,
            withdrawals,
            partner_payouts: PartnerPayouts::new(),
            bonds: Arc::new(OperatorBonds::new()),
            attempts: crate::attempts::TransferAttempts::new(),
            rail: None,
            reputation,
            payout,
            audit,
            journal: None,
            stake_slashes: Default::default(),
            streams: StreamBook::new(),
            vault,
        }))
    }

    /// Like [`CoordinatorState::new`], but the job book, escrow and
    /// buyer accounts replay `journal_path` at boot and journal every
    /// mutation from here on — the durable configuration a real
    /// deployment runs (escrow holds, in-flight jobs and buyer deposits
    /// survive a restart; the operator registry deliberately does not,
    /// nodes re-register). `rail` is the inbound deposit verifier;
    /// without one the deposit endpoint refuses claims. Before
    /// returning, [`crate::recover::reconcile_books`] settles any
    /// crash-window disagreement between the replayed escrow ledger and
    /// job book — async, and unconditionally here, so no restored
    /// deployment can forget it.
    pub async fn with_journal(
        identity: LocalIdentity,
        config: CoordinatorConfig,
        reputation: Arc<dyn ReputationSource>,
        payout: Arc<dyn Payout>,
        audit: Arc<dyn AuditLog>,
        journal_path: &Path,
        rail: Option<Arc<dyn InboundRail>>,
    ) -> Result<Self, JournalError> {
        let restored = Journal::load(journal_path)?;
        let journal = Arc::new(Journal::open(journal_path)?);
        tracing::info!(
            jobs = restored.jobs.len(),
            holds = restored.holds.len(),
            buyers = restored.deposit_totals.len(),
            path = %journal_path.display(),
            "coordinator state restored from journal"
        );
        // Boot-time compaction: replay just proved which facts
        // survive, so rewrite the file down to them. Best-effort — the
        // uncompacted journal is correct, only bigger, and a coordinator
        // that can serve shouldn't be held down by a shrink that can
        // wait for the periodic pass.
        match journal.compact() {
            Ok(stats) if stats.shrank() => tracing::info!(
                entries_before = stats.entries_before,
                entries_after = stats.entries_after,
                bytes_before = stats.bytes_before,
                bytes_after = stats.bytes_after,
                "journal compacted at boot"
            ),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "boot journal compaction failed; continuing"),
        }
        let accounts = Arc::new(BuyerAccounts::restore(
            restored.deposit_totals,
            restored.deposit_ids,
            journal.clone(),
        ));
        let partner_payouts = PartnerPayouts::restore(
            restored.partner_paid_totals,
            restored.partner_payout_ids,
            journal.clone(),
        );
        let withdrawals = Arc::new(BuyerWithdrawals::restore(
            restored.withdrawals,
            journal.clone(),
        ));
        let bonds = Arc::new(OperatorBonds::restore(
            restored.bond_totals,
            restored.bond_ids,
            restored.bond_slashes,
            restored.unbonds,
            journal.clone(),
        ));
        let mut escrow = CustodialEscrow::restore(
            identity,
            config.default_funding_source,
            restored.holds,
            journal.clone(),
        )
        .with_withdrawals(withdrawals.clone());
        if config.require_prefunded_buyers {
            escrow = escrow.with_accounts(accounts.clone());
        }
        if let Some(policy) = config.subsidy_policy {
            escrow = escrow.with_subsidy_policy(policy);
        }
        if let Some(closed_at_ms) = restored.subsidy_closed_at_ms {
            // The journaled runtime close outranks whatever policy the
            // boot environment supplied — otherwise a restart would
            // silently re-arm a kill-switch an admin deliberately shut.
            escrow = escrow.with_subsidy_closed(true);
            if config.subsidy_policy.is_some() {
                tracing::warn!(
                    closed_at_ms,
                    "subsidy policy configured but the journal records a runtime close; \
                     the subsidy stays closed"
                );
            }
        }
        let attempts =
            crate::attempts::TransferAttempts::restore(restored.transfer_attempts, journal.clone());
        // The vault keeps its own append-only log beside the money
        // journal rather than sharing it: what it holds is opaque
        // ciphertext with no bearing on settlement, and a bad vault file
        // should never wedge the escrow replay above.
        let vault = if config.vault_enabled {
            let vault_path = journal_path.with_file_name("vault.jsonl");
            let store = crate::vault::VaultStore::open(&vault_path)
                .map_err(|e| JournalError::Io(std::io::Error::other(e.to_string())))?
                .with_max_owners(config.vault_max_owners);
            Some(Arc::new(store))
        } else {
            None
        };
        let state = Self(Arc::new(Inner {
            config,
            registry: OperatorRegistry::new(),
            jobs: JobBook::restore(restored.jobs, journal.clone()),
            escrow,
            accounts,
            withdrawals,
            partner_payouts,
            bonds,
            attempts,
            rail,
            reputation,
            payout,
            audit,
            journal: Some(journal),
            stake_slashes: parking_lot::Mutex::new(restored.stake_slashes),
            streams: StreamBook::new(),
            vault,
        }));
        state.reconcile_transfer_attempts();
        let report = crate::recover::reconcile_books(&state).await;
        if !report.is_clean() {
            tracing::warn!(
                orphan_holds_refunded = report.orphan_holds_refunded,
                stale_holds_settled = report.stale_holds_settled,
                records_concluded = report.records_concluded,
                "boot reconcile settled crash-window disagreements between escrow and the job book"
            );
        }
        Ok(state)
    }

    pub fn config(&self) -> &CoordinatorConfig {
        &self.0.config
    }

    pub fn registry(&self) -> &OperatorRegistry {
        &self.0.registry
    }

    /// The client-sealed secret vault, or `None` when the deployment did
    /// not opt in. The routes 404 on `None`, so a coordinator that serves
    /// no vault is indistinguishable from one that never had the feature.
    pub fn vault(&self) -> Option<&Arc<crate::vault::VaultStore>> {
        self.0.vault.as_ref()
    }

    pub fn jobs(&self) -> &JobBook {
        &self.0.jobs
    }

    pub fn escrow(&self) -> &CustodialEscrow {
        &self.0.escrow
    }

    pub fn accounts(&self) -> &BuyerAccounts {
        &self.0.accounts
    }

    pub fn withdrawals(&self) -> &BuyerWithdrawals {
        &self.0.withdrawals
    }

    pub fn partner_payouts(&self) -> &PartnerPayouts {
        &self.0.partner_payouts
    }

    pub fn bonds(&self) -> &OperatorBonds {
        &self.0.bonds
    }

    pub fn attempts(&self) -> &crate::attempts::TransferAttempts {
        &self.0.attempts
    }

    /// The signed drift in the release-to-payout conservation identity a
    /// deployment's scraper alerts on: escrow released to settlements
    /// equals what was pushed to operators, plus the net still owed
    /// them, plus the fees withheld. Zero when the books balance; any
    /// nonzero figure is an accounting bug, not a traffic pattern.
    ///
    /// The owed side is summed from the escrow hold, never the job
    /// record's `released_gross`. The two agree for a clean settlement,
    /// but a receipt-less crash-recovered lease released only its
    /// metered seconds while leaving no metered stamp on its record: the
    /// record would then read back the whole window ceiling and drift
    /// the identity by `ceiling - used` against escrow that only ever
    /// moved `used`. The hold is what actually left escrow, the same
    /// authority [`crate::sweep::sweep_unpaid`] sizes a retry from, so
    /// it is the honest figure here too.
    pub fn reconciliation_drift_micro_usdc(&self) -> i128 {
        let released: u128 = [FundingSource::Organic, FundingSource::Bootstrap]
            .into_iter()
            .map(|source| u128::from(self.escrow().money_totals(source).released_micro_usdc))
            .sum();
        let (pushed, _) = self.jobs().payout_totals();
        let owed: u128 = self
            .jobs()
            .completed_unpaid()
            .into_iter()
            .map(|(job_id, record)| {
                let released_gross = self
                    .escrow()
                    .hold_info(job_id)
                    .map(|(amount, _)| amount)
                    .unwrap_or_else(|| record.released_gross_micro_usdc());
                u128::from(released_gross.saturating_sub(record.fee_micro_usdc))
            })
            .sum();
        let (fees, _) = self.jobs().fees_captured();
        let accounted = u128::from(pushed) + owed + u128::from(fees);
        released as i128 - accounted as i128
    }

    /// Boot half of the transfer bracket: for every attempt a crash
    /// left open, either close it from the completion marker that did
    /// land (the common, benign window) or leave it suspended — and for
    /// a suspended unbond refund, re-fence the in-doubt stake so a
    /// post-restart slash can't double-draw it.
    fn reconcile_transfer_attempts(&self) {
        use crate::journal::TransferAttemptKind;
        for attempt in self.0.attempts.open_attempts() {
            let completed = match attempt.kind {
                TransferAttemptKind::JobPayout => self
                    .jobs()
                    .get(attempt.attempt_id)
                    .is_some_and(|r| r.payout.is_some()),
                TransferAttemptKind::Withdrawal => self
                    .withdrawals()
                    .get(attempt.attempt_id)
                    .is_some_and(|w| w.pushed.is_some()),
                TransferAttemptKind::UnbondRefund => self
                    .bonds()
                    .get_unbond(attempt.attempt_id)
                    .is_some_and(|u| u.pushed.is_some()),
            };
            if completed {
                if let Err(e) = self
                    .0
                    .attempts
                    .resolve(attempt.attempt_id, attempt.tx_signature.as_deref())
                {
                    tracing::error!(
                        attempt_id = %attempt.attempt_id,
                        error = %e,
                        "could not resolve a completed transfer attempt at boot"
                    );
                }
                continue;
            }
            if attempt.kind == TransferAttemptKind::UnbondRefund {
                self.bonds()
                    .reinstate_reservation(attempt.attempt_id, attempt.amount_micro_usdc);
            }
            tracing::error!(
                attempt_id = %attempt.attempt_id,
                kind = attempt.kind.as_str(),
                amount_micro_usdc = attempt.amount_micro_usdc,
                memo = %attempt.memo,
                tx_signature = attempt.tx_signature.as_deref().unwrap_or(""),
                "restart found a transfer whose outcome is unknown; obligation suspended \
                 until reconciled (POST /admin/transfers/{{id}}/resolve)"
            );
        }
    }

    pub fn streams(&self) -> &StreamBook {
        &self.0.streams
    }

    pub fn rail(&self) -> Option<&dyn InboundRail> {
        self.0.rail.as_deref()
    }

    /// Check-vote rounds, when a deployment runs them.
    pub fn vote_rounds(&self) -> Option<&dyn crate::rounds::VoteRounds> {
        self.0.config.vote_rounds.as_deref()
    }

    /// The on-chain lease meter, when a deployment configured one.
    /// `None` is the default and every call site short-circuits on it.
    pub fn lease_meter(&self) -> Option<&dyn crate::onchain_meter::LeaseMeter> {
        self.0.config.lease_meter.as_deref()
    }

    /// The durable journal, when this state was built with one — what
    /// `main.rs` hands the periodic compaction task.
    pub fn journal(&self) -> Option<Arc<Journal>> {
        self.0.journal.clone()
    }

    /// The latest state of one on-chain stake slash.
    pub fn stake_slash(&self, slash_id: &str) -> Option<StakeSlashState> {
        self.0.stake_slashes.lock().get(slash_id).cloned()
    }

    /// Slashes whose outcome never became durable, for boot to drive
    /// again.
    pub fn attempted_stake_slashes(&self) -> Vec<StakeSlashState> {
        self.0
            .stake_slashes
            .lock()
            .values()
            .filter(|s| s.status == StakeSlashStatus::Attempted)
            .cloned()
            .collect()
    }

    /// Journal-then-commit, like every other money transition.
    pub fn record_stake_slash(&self, state: StakeSlashState) -> Result<(), JournalError> {
        if let Some(journal) = &self.0.journal {
            journal.record_stake_slash(&state)?;
        }
        self.0
            .stake_slashes
            .lock()
            .insert(state.slash_id.clone(), state);
        Ok(())
    }

    pub fn buyer_funds(&self, buyer_pubkey_b58: &str) -> BuyerFunds {
        let deposited = self.0.accounts.deposited(buyer_pubkey_b58);
        let charged = self.0.escrow.organic_charged(buyer_pubkey_b58);
        let withdrawn = self.0.withdrawals.withdrawn(buyer_pubkey_b58);
        BuyerFunds {
            deposited_micro_usdc: deposited,
            charged_micro_usdc: charged,
            withdrawn_micro_usdc: withdrawn,
            available_micro_usdc: deposited.saturating_sub(charged).saturating_sub(withdrawn),
        }
    }

    pub fn reputation(&self) -> &dyn ReputationSource {
        self.0.reputation.as_ref()
    }

    pub fn payout(&self) -> &dyn Payout {
        self.0.payout.as_ref()
    }

    pub fn audit(&self) -> &Arc<dyn AuditLog> {
        &self.0.audit
    }

    pub fn coordinator_pubkey_b58(&self) -> String {
        self.0.escrow.coordinator_pubkey_b58()
    }

    pub fn coordinator_agent_id(&self) -> covenant_types::AgentId {
        self.0.escrow.agent_id()
    }

    /// Pins an accepted payout push onto the journaled job record and
    /// writes its audit row — the one way a push becomes a durable
    /// fact, shared by the first-chance push in `submit_result` and
    /// the retry sweep so the two can never record differently.
    pub async fn record_payout_pushed(
        &self,
        job_id: Uuid,
        operator_pubkey_b58: &str,
        paid: &crate::payout::PayoutRecord,
    ) {
        if let Err(e) = self.jobs().set_payout(
            job_id,
            crate::jobs::PayoutOutcome {
                amount_micro_usdc: paid.amount_micro_usdc,
                tx_signature: paid.tx_signature.clone(),
                recorded_at_ms: paid.recorded_at_ms,
            },
        ) {
            tracing::error!(%job_id, error = %e, "payout pushed but recording it failed");
        }
        self.record_audit(AuditKind::ComputePayoutPushed {
            job_id,
            operator_pubkey_b58: operator_pubkey_b58.to_string(),
            amount_micro_usdc: paid.amount_micro_usdc,
            tx_signature: paid.tx_signature.clone(),
        })
        .await;
    }

    /// Pins an accepted withdrawal transfer onto the journaled
    /// withdrawal record and writes its audit row — shared by the
    /// first-chance push in the withdraw handler and the retry sweep,
    /// mirroring [`CoordinatorState::record_payout_pushed`].
    pub async fn record_withdrawal_pushed(
        &self,
        withdrawal: &crate::accounts::WithdrawalState,
        transfer: &crate::payout::TransferRecord,
    ) {
        if let Err(e) = self.escrow().record_withdrawal_pushed(
            withdrawal.withdrawal_id,
            crate::accounts::WithdrawalPush {
                tx_signature: transfer.tx_signature.clone(),
                recorded_at_ms: transfer.recorded_at_ms,
            },
        ) {
            tracing::error!(
                withdrawal_id = %withdrawal.withdrawal_id,
                error = %e,
                "withdrawal pushed but recording it failed"
            );
        }
        self.record_audit(AuditKind::ComputeWithdrawalPushed {
            withdrawal_id: withdrawal.withdrawal_id,
            buyer_pubkey_b58: withdrawal.buyer_pubkey_b58.clone(),
            amount_micro_usdc: transfer.amount_micro_usdc,
            tx_signature: transfer.tx_signature.clone(),
        })
        .await;
    }

    /// Takes stake for one coordinator-proven fault — the shared landing
    /// for the only two sites allowed to slash: a canary probe judged
    /// wrong and a redundancy strict-majority minority. The take is the
    /// faulted job's price clamped by the stake standing; the
    /// deterministic slash id makes a replayed verdict (audit-seeded
    /// books after a restart) move nothing. A buyer dispute must never
    /// reach this — an unadjudicated accusation moving money would make
    /// instant-dispute a griefing strategy.
    pub async fn slash_for_fault(
        &self,
        fault_kind: &str,
        job_id: Uuid,
        operator_pubkey_b58: &str,
        job_price_micro_usdc: u64,
        reason: &str,
    ) {
        let slash_id = format!("{fault_kind}:{job_id}:{operator_pubkey_b58}");
        match self.bonds().slash(
            &slash_id,
            operator_pubkey_b58,
            job_price_micro_usdc,
            job_id,
            reason,
            crate::epoch_ms(),
        ) {
            Ok(crate::bond::SlashOutcome::Slashed {
                amount_micro_usdc,
                at_stake_micro_usdc,
            }) => {
                self.record_audit(AuditKind::ComputeBondSlashed {
                    operator_pubkey_b58: operator_pubkey_b58.to_string(),
                    amount_micro_usdc,
                    job_id,
                    reason: reason.to_string(),
                })
                .await;
                tracing::warn!(
                    %job_id,
                    operator = %operator_pubkey_b58,
                    amount_micro_usdc,
                    at_stake_micro_usdc,
                    reason,
                    "operator bond slashed for a proven fault"
                );
            }
            // No stake standing: normal on a deployment without a bond
            // floor — the fault's own audit row is still the record.
            Ok(crate::bond::SlashOutcome::NoStake) => {}
            Ok(crate::bond::SlashOutcome::Duplicate) => {}
            Err(e) => {
                tracing::error!(
                    %job_id,
                    operator = %operator_pubkey_b58,
                    error = %e,
                    "bond slash could not be made durable"
                );
            }
        }
        crate::stake::slash_for_fault(self, &slash_id, job_id, operator_pubkey_b58, reason);
    }

    /// Pins an accepted bond-refund transfer onto the journaled unbond
    /// record and writes its audit row — the sweep's landing half,
    /// mirroring [`CoordinatorState::record_withdrawal_pushed`].
    /// `paid_micro_usdc` rides separately from the request's amount: a
    /// slash during maturation shrinks what actually left.
    pub async fn record_bond_refunded(
        &self,
        unbond: &crate::bond::UnbondState,
        paid_micro_usdc: u64,
        tx_signature: Option<String>,
        recorded_at_ms: u64,
    ) {
        if let Err(e) = self.bonds().record_refunded(
            unbond.unbond_id,
            crate::bond::BondRefundPush {
                tx_signature: tx_signature.clone(),
                recorded_at_ms,
                paid_micro_usdc,
            },
        ) {
            tracing::error!(
                unbond_id = %unbond.unbond_id,
                error = %e,
                "bond refund pushed but recording it failed"
            );
        }
        self.record_audit(AuditKind::ComputeBondRefunded {
            operator_pubkey_b58: unbond.operator_pubkey_b58.clone(),
            unbond_id: unbond.unbond_id,
            requested_micro_usdc: unbond.amount_micro_usdc,
            paid_micro_usdc,
            tx_signature,
        })
        .await;
    }

    /// The one path a job payout may take to the backend: brackets the
    /// push in a durable transfer attempt (see `crate::attempts`),
    /// records a landed push before resolving the bracket, and pins an
    /// unknown outcome open so nothing retries it. Both the
    /// first-chance push and the retry sweep come through here — a push
    /// outside the bracket would reintroduce the cross-restart
    /// double-spend this exists to close.
    pub async fn push_job_payout(
        &self,
        job_id: Uuid,
        operator_pubkey_b58: &str,
        payout_address: &str,
        amount_micro_usdc: u64,
        receipt: &covenant_compute_protocol::SignedWorkReceipt,
    ) -> Result<crate::payout::PayoutRecord, crate::payout::PayoutError> {
        use crate::attempts::BeginOutcome;
        use crate::journal::TransferAttemptKind;
        use crate::payout::PayoutError;

        let memo = receipt.payout_memo();
        match self.0.attempts.begin(
            job_id,
            TransferAttemptKind::JobPayout,
            amount_micro_usdc,
            payout_address,
            &memo,
            crate::epoch_ms(),
        ) {
            Ok(BeginOutcome::Proceed) => {}
            Ok(BeginOutcome::Open(state)) => {
                return Err(PayoutError::Backend(format!(
                    "job {job_id}: a transfer attempt is already open ({}); \
                     reconcile it before anything can move",
                    if state.detail.is_empty() {
                        "in flight"
                    } else {
                        state.detail.as_str()
                    }
                )));
            }
            Err(e) => {
                return Err(PayoutError::Backend(format!(
                    "job {job_id}: could not journal the transfer attempt: {e}"
                )));
            }
        }

        let outcome = self
            .payout()
            .pay(
                job_id,
                operator_pubkey_b58,
                payout_address,
                amount_micro_usdc,
                receipt,
            )
            .await;
        match &outcome {
            Ok(paid) => {
                self.record_payout_pushed(job_id, operator_pubkey_b58, paid)
                    .await;
                if let Err(e) = self
                    .0
                    .attempts
                    .resolve(job_id, paid.tx_signature.as_deref())
                {
                    tracing::error!(%job_id, error = %e, "payout landed but its attempt bracket could not resolve");
                }
            }
            Err(PayoutError::Backend(detail)) => {
                if let Err(e) = self.0.attempts.clear(job_id, detail) {
                    tracing::error!(%job_id, error = %e, "could not clear a refused transfer attempt");
                }
            }
            Err(PayoutError::Unresolved {
                message,
                tx_signature,
            }) => {
                if let Err(e) = self
                    .0
                    .attempts
                    .suspend(job_id, message, tx_signature.as_deref())
                {
                    tracing::error!(%job_id, error = %e, "could not journal a suspended transfer attempt");
                }
            }
        }
        outcome
    }

    /// [`CoordinatorState::push_job_payout`] for a withdrawal debit —
    /// the only path a withdrawal transfer may take to the backend.
    pub async fn push_withdrawal(
        &self,
        withdrawal: &crate::accounts::WithdrawalState,
    ) -> Result<crate::payout::TransferRecord, crate::payout::PayoutError> {
        use crate::attempts::BeginOutcome;
        use crate::journal::TransferAttemptKind;
        use crate::payout::PayoutError;

        let id = withdrawal.withdrawal_id;
        let memo = covenant_compute_protocol::withdrawal_memo_for(&withdrawal.buyer_pubkey_b58, id);
        match self.0.attempts.begin(
            id,
            TransferAttemptKind::Withdrawal,
            withdrawal.amount_micro_usdc,
            &withdrawal.recipient_address_b58,
            &memo,
            crate::epoch_ms(),
        ) {
            Ok(BeginOutcome::Proceed) => {}
            Ok(BeginOutcome::Open(state)) => {
                return Err(PayoutError::Backend(format!(
                    "withdrawal {id}: a transfer attempt is already open ({}); \
                     reconcile it before anything can move",
                    if state.detail.is_empty() {
                        "in flight"
                    } else {
                        state.detail.as_str()
                    }
                )));
            }
            Err(e) => {
                return Err(PayoutError::Backend(format!(
                    "withdrawal {id}: could not journal the transfer attempt: {e}"
                )));
            }
        }

        let outcome = self
            .payout()
            .transfer(
                id,
                &withdrawal.recipient_address_b58,
                withdrawal.amount_micro_usdc,
                &memo,
            )
            .await;
        match &outcome {
            Ok(transfer) => {
                self.record_withdrawal_pushed(withdrawal, transfer).await;
                if let Err(e) = self
                    .0
                    .attempts
                    .resolve(id, transfer.tx_signature.as_deref())
                {
                    tracing::error!(withdrawal_id = %id, error = %e, "withdrawal landed but its attempt bracket could not resolve");
                }
            }
            Err(PayoutError::Backend(detail)) => {
                if let Err(e) = self.0.attempts.clear(id, detail) {
                    tracing::error!(withdrawal_id = %id, error = %e, "could not clear a refused transfer attempt");
                }
            }
            Err(PayoutError::Unresolved {
                message,
                tx_signature,
            }) => {
                if let Err(e) = self
                    .0
                    .attempts
                    .suspend(id, message, tx_signature.as_deref())
                {
                    tracing::error!(withdrawal_id = %id, error = %e, "could not journal a suspended transfer attempt");
                }
            }
        }
        outcome
    }

    /// The only path a matured unbond refund may take to the backend.
    /// On top of the attempt bracket, the payable is reserved in the
    /// bond book for the transfer's whole flight, so a slash landing
    /// mid-air can't draw the same stake (`Ok(None)` = nothing payable
    /// or already reserved — the caller moves on). A zero payable is
    /// booked as a closed, transfer-less refund exactly as before.
    pub async fn push_unbond_refund(
        &self,
        unbond: &crate::bond::UnbondState,
    ) -> Result<Option<crate::payout::TransferRecord>, crate::payout::PayoutError> {
        use crate::attempts::BeginOutcome;
        use crate::journal::TransferAttemptKind;
        use crate::payout::PayoutError;

        let id = unbond.unbond_id;
        let now_ms = crate::epoch_ms();
        if self.0.attempts.is_open(id) {
            return Ok(None);
        }
        let Some(payable) = self.bonds().reserve_refund(id, now_ms) else {
            return Ok(None);
        };
        if payable == 0 {
            self.record_bond_refunded(unbond, 0, None, now_ms).await;
            tracing::info!(
                unbond_id = %id,
                operator = %unbond.operator_pubkey_b58,
                requested_micro_usdc = unbond.amount_micro_usdc,
                "unbond closed with nothing to refund: the stake was slashed while maturing"
            );
            return Ok(None);
        }
        let memo = covenant_compute_protocol::bond_refund_memo_for(&unbond.operator_pubkey_b58, id);
        match self.0.attempts.begin(
            id,
            TransferAttemptKind::UnbondRefund,
            payable,
            &unbond.recipient_address_b58,
            &memo,
            now_ms,
        ) {
            Ok(BeginOutcome::Proceed) => {}
            Ok(BeginOutcome::Open(_)) => {
                self.bonds().release_reservation(id);
                return Ok(None);
            }
            Err(e) => {
                self.bonds().release_reservation(id);
                return Err(PayoutError::Backend(format!(
                    "unbond {id}: could not journal the transfer attempt: {e}"
                )));
            }
        }

        let outcome = self
            .payout()
            .transfer(id, &unbond.recipient_address_b58, payable, &memo)
            .await;
        match &outcome {
            Ok(transfer) => {
                // record_bond_refunded books the spend and releases the
                // reservation under the same ledger lock.
                self.record_bond_refunded(
                    unbond,
                    transfer.amount_micro_usdc,
                    transfer.tx_signature.clone(),
                    transfer.recorded_at_ms,
                )
                .await;
                if let Err(e) = self
                    .0
                    .attempts
                    .resolve(id, transfer.tx_signature.as_deref())
                {
                    tracing::error!(unbond_id = %id, error = %e, "unbond refund landed but its attempt bracket could not resolve");
                }
            }
            Err(PayoutError::Backend(detail)) => {
                self.bonds().release_reservation(id);
                if let Err(e) = self.0.attempts.clear(id, detail) {
                    tracing::error!(unbond_id = %id, error = %e, "could not clear a refused transfer attempt");
                }
            }
            Err(PayoutError::Unresolved {
                message,
                tx_signature,
            }) => {
                // The reservation stays: the money may be mid-air, and
                // releasing it would hand the same stake back to the
                // slash clamp.
                if let Err(e) = self
                    .0
                    .attempts
                    .suspend(id, message, tx_signature.as_deref())
                {
                    tracing::error!(unbond_id = %id, error = %e, "could not journal a suspended transfer attempt");
                }
            }
        }
        outcome.map(Some)
    }

    /// Records one row into this coordinator's own hash-chained audit
    /// log, issued as the coordinator's own identity. Best-effort: a
    /// write failure is logged, never propagated — an audit outage
    /// shouldn't block a mechanical escrow release or refund that
    /// already committed.
    pub async fn record_audit(&self, kind: AuditKind) {
        let event = AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: crate::epoch_ms(),
            issuer: self.coordinator_agent_id(),
            kind,
        };
        if let Err(e) = self.0.audit.record(event).await {
            tracing::warn!(error = %e, "coordinator audit write failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::WithdrawalState;
    use crate::bond::{BondRefundPush, SlashOutcome, UnbondState};
    use crate::journal::TransferAttemptKind;
    use crate::payout::{MockPayout, PayoutError};
    use crate::reputation::NoReputation;
    use covenant_a2a::A2ATaskStatus;
    use covenant_audit::InMemoryAuditLog;
    use covenant_compute_protocol::{JobMeter, SignedWorkReceipt, WorkReceiptPayload};
    use covenant_identity::LocalIdentity;
    use std::sync::Arc;

    fn test_state() -> CoordinatorState {
        CoordinatorState::new(
            LocalIdentity::generate("coordinator@test"),
            CoordinatorConfig::default(),
            Arc::new(NoReputation),
            Arc::new(MockPayout::new()),
            Arc::new(InMemoryAuditLog::new()),
        )
    }

    fn matured_unbond(op: &str, amount: u64) -> UnbondState {
        UnbondState {
            unbond_id: Uuid::new_v4(),
            operator_pubkey_b58: op.to_string(),
            recipient_address_b58: op.to_string(),
            amount_micro_usdc: amount,
            requested_at_ms: 1,
            matures_at_ms: 100,
            pushed: None,
        }
    }

    /// A restart finds a refund transfer whose outcome is unknown: its
    /// bracket is open and no completion marker landed. Boot must
    /// re-fence the in-doubt principal so a slash arriving after the
    /// restart cannot draw the stake the refund may already have paid.
    #[test]
    fn boot_re_fences_a_suspended_unbond_refunds_stake() {
        let op = bs58::encode([7u8; 32]).into_string();
        let state = test_state();
        state.bonds().credit_post("bond-1", &op, 1_000).unwrap();
        let unbond = matured_unbond(&op, 1_000);
        let unbond_id = unbond.unbond_id;
        state.bonds().request_unbond(unbond).unwrap();
        state
            .attempts()
            .begin(
                unbond_id,
                TransferAttemptKind::UnbondRefund,
                1_000,
                &op,
                "memo",
                5,
            )
            .unwrap();

        state.reconcile_transfer_attempts();

        assert!(
            state.attempts().is_open(unbond_id),
            "an unresolved refund stays suspended"
        );
        assert_eq!(
            state
                .bonds()
                .slash("canary:j1", &op, 1_000, Uuid::new_v4(), "wrong-answer", 110)
                .unwrap(),
            SlashOutcome::NoStake,
            "the reinstated reservation blocks the slash from double-drawing"
        );
    }

    /// Same crash window, but the push did land — its marker is on the
    /// record. Boot closes the bracket instead of suspending it.
    #[test]
    fn boot_resolves_a_suspended_unbond_refund_whose_push_landed() {
        let op = bs58::encode([9u8; 32]).into_string();
        let state = test_state();
        state.bonds().credit_post("bond-1", &op, 1_000).unwrap();
        let unbond = matured_unbond(&op, 1_000);
        let unbond_id = unbond.unbond_id;
        state.bonds().request_unbond(unbond).unwrap();
        state
            .attempts()
            .begin(
                unbond_id,
                TransferAttemptKind::UnbondRefund,
                1_000,
                &op,
                "memo",
                5,
            )
            .unwrap();
        state
            .bonds()
            .record_refunded(
                unbond_id,
                BondRefundPush {
                    tx_signature: Some("sig".into()),
                    recorded_at_ms: 6,
                    paid_micro_usdc: 1_000,
                },
            )
            .unwrap();

        state.reconcile_transfer_attempts();

        assert!(
            !state.attempts().is_open(unbond_id),
            "a landed refund closes its bracket at boot"
        );
    }

    fn state_with_payout(payout: Arc<MockPayout>) -> CoordinatorState {
        CoordinatorState::new(
            LocalIdentity::generate("coordinator@test"),
            CoordinatorConfig::default(),
            Arc::new(NoReputation),
            payout,
            Arc::new(InMemoryAuditLog::new()),
        )
    }

    fn signed_receipt(
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

    // The transfer-attempt bracket closes the cross-restart double-spend
    // only if a push refuses to move whenever a bracket for that obligation
    // is already open — a transfer may be live right now, or a prior one's
    // outcome is still unresolved. `attempts` proves it blocks a second
    // `begin`; the three tests below pin the wrappers that turn that block
    // into a refusal the signer never sees, one per money-out path.

    #[tokio::test]
    async fn an_open_bracket_refuses_a_job_payout_and_pushes_nothing() {
        let payout = Arc::new(MockPayout::new());
        let state = state_with_payout(payout.clone());
        let operator = LocalIdentity::generate("operator@test");
        let job_id = Uuid::new_v4();
        let receipt = signed_receipt(job_id, &operator, 5_000);

        state
            .attempts()
            .begin(
                job_id,
                TransferAttemptKind::JobPayout,
                5_000,
                "payout-addr",
                &receipt.payout_memo(),
                1,
            )
            .unwrap();

        let err = state
            .push_job_payout(job_id, "op-pubkey", "payout-addr", 5_000, &receipt)
            .await
            .unwrap_err();

        assert!(
            matches!(&err, PayoutError::Backend(m) if m.contains("already open")),
            "a job payout onto an open bracket must be refused, got {err:?}"
        );
        assert!(
            payout.records().is_empty(),
            "the refused payout must never reach the signer"
        );
    }

    #[tokio::test]
    async fn an_open_bracket_refuses_a_withdrawal_and_pushes_nothing() {
        let payout = Arc::new(MockPayout::new());
        let state = state_with_payout(payout.clone());
        let withdrawal = WithdrawalState {
            withdrawal_id: Uuid::new_v4(),
            buyer_pubkey_b58: "buyer".into(),
            recipient_address_b58: "recipient-addr".into(),
            amount_micro_usdc: 4_000,
            requested_at_ms: 1,
            pushed: None,
        };
        let memo = covenant_compute_protocol::withdrawal_memo_for(
            &withdrawal.buyer_pubkey_b58,
            withdrawal.withdrawal_id,
        );
        state
            .attempts()
            .begin(
                withdrawal.withdrawal_id,
                TransferAttemptKind::Withdrawal,
                withdrawal.amount_micro_usdc,
                &withdrawal.recipient_address_b58,
                &memo,
                1,
            )
            .unwrap();

        let err = state.push_withdrawal(&withdrawal).await.unwrap_err();

        assert!(
            matches!(&err, PayoutError::Backend(m) if m.contains("already open")),
            "a withdrawal onto an open bracket must be refused, got {err:?}"
        );
        assert!(
            payout.transfers().is_empty(),
            "the refused withdrawal must never reach the signer"
        );
    }

    #[tokio::test]
    async fn an_open_bracket_stops_an_unbond_refund_from_transferring() {
        let payout = Arc::new(MockPayout::new());
        let state = state_with_payout(payout.clone());

        // Baseline: a matured refund with a clear bracket transfers once, so
        // the refusal below can't pass for the wrong reason (an empty
        // payable) — it has to be the bracket that gates it.
        let op_clear = bs58::encode([4u8; 32]).into_string();
        state
            .bonds()
            .credit_post("bond-clear", &op_clear, 1_000)
            .unwrap();
        let clear = matured_unbond(&op_clear, 1_000);
        state.bonds().request_unbond(clear.clone()).unwrap();
        assert!(
            state.push_unbond_refund(&clear).await.unwrap().is_some(),
            "a matured refund transfers when its bracket is clear"
        );
        assert_eq!(payout.transfers().len(), 1);

        // A second operator's refund, just as payable, but a transfer is
        // already bracketed for it — a push may be live. It must move
        // nothing, whether the guard that stops it is the is_open
        // short-circuit or the begin arm behind it.
        let op_open = bs58::encode([5u8; 32]).into_string();
        state
            .bonds()
            .credit_post("bond-open", &op_open, 1_000)
            .unwrap();
        let open = matured_unbond(&op_open, 1_000);
        state.bonds().request_unbond(open.clone()).unwrap();
        state
            .attempts()
            .begin(
                open.unbond_id,
                TransferAttemptKind::UnbondRefund,
                1_000,
                &op_open,
                "memo",
                2,
            )
            .unwrap();

        assert!(
            state.push_unbond_refund(&open).await.unwrap().is_none(),
            "an open bracket yields no refund"
        );
        assert_eq!(
            payout.transfers().len(),
            1,
            "the bracketed refund added no second transfer"
        );
    }
}
