//! Coordinator service for the Covenant compute network (codename
//! compute): the axum server implementing the operator- and
//! buyer-facing endpoints `covenant-compute-protocol` and
//! `covenant-compute-node` are built against — see
//! `docs/internal/compute-network/build-notes-phase1-foundation.md` §1
//! for the exact contract this crate implements, and
//! `build-notes-phase1-coordinator.md` for the as-built record.
//!
//! Module map: [`registry`] is the operator registry plus the
//! per-operator long-poll delivery queue; [`matcher`] is the v1
//! matching algorithm (capability filter, price-ascending sort,
//! reputation tie-break); [`reputation`] is the matchmaker's tie-break
//! input, behind a trait; [`escrow`] is the coordinator-custodial
//! `FederationEscrow` v1 implementation; [`payout`] is the payout-push
//! seam (`MockPayout` for hermetic tests, `SidecarPayout` for a real,
//! sidecar-isolated SPL transfer — see build-notes-phase1-payout.md);
//! [`jobs`] is the coordinator's own per-job lifecycle ledger;
//! [`journal`] is the append-only JSONL journal that makes the job
//! book, escrow and buyer deposits survive a restart; [`recover`] is
//! the boot pass that settles crash-window disagreements between the
//! replayed escrow ledger and job book; [`accounts`] is
//! the buyer deposit ledger and [`deposit`] the inbound-rail seam that
//! verifies claimed deposits before they credit; [`canary`] is the C5
//! prober that buys known-answer work from operators through the
//! ordinary pipeline and judges the output; [`redundancy`] is its C5
//! sibling that re-buys released batch work from other operators and
//! compares receipt hashes; [`http`] wires all of
//! the above into the axum router; [`state`] is the shared application
//! state and config.

#![deny(unsafe_code)]

pub mod accounts;
pub mod attempts;
pub mod bond;
pub mod canary;
pub mod deposit;
pub mod escrow;
pub mod http;
pub mod jobs;
pub mod journal;
pub mod matcher;
pub mod onchain_meter;
pub mod payout;
pub mod recover;
pub mod redundancy;
pub mod registry;
pub mod reputation;
pub mod stake;
pub mod state;
pub mod stream;
pub mod sweep;
pub mod vault;

pub use accounts::{BuyerAccounts, DepositOutcome, PartnerPayoutOutcome, PartnerPayouts};
pub use bond::{
    BondPostOutcome, BondRefundPush, BondStatus, OperatorBonds, SlashOutcome, SlashRecord,
    UnbondOutcome, UnbondState,
};
pub use canary::{spawn_periodic_canary, CanaryConfig, CanaryProber, TickReport, INFER_TEMPLATES};
pub use deposit::{
    BondClaim, DepositClaim, InboundRail, MockRail, RailError, SolanaRpcRail, VerifiedBond,
    VerifiedDeposit, BOND_MEMO_PREFIX, DEPOSIT_MEMO_PREFIX,
};
pub use escrow::{CustodialEscrow, EscrowMoneyTotals, SubsidyPolicy, SubsidyStatus};
pub use http::router;
pub use jobs::{JobBook, JobPhase, JobRecord, JobStats, PayoutOutcome, ReleaseCharges};
pub use journal::{
    spawn_periodic_compaction, CompactStats, EscrowHoldState, Journal, JournalError, RestoredState,
};
pub use matcher::{select_operator, BondFloor};
pub use onchain_meter::{
    adopt_live_leases, conclude_lease_onchain, hold_payout_for_chain, observe_lease,
    observed_elapsed_ms, open_lease_onchain, spawn_periodic_lease_meter, tick_live_leases,
    void_lease_onchain, LeaseConclusion, LeaseMeter, LeaseMeterError, LeaseObservation, LeaseOpen,
    LeaseSettlement, NoopLeaseMeter, SidecarLeaseMeter, SidecarLeaseMeterConfig, LEASE_TICK_DOMAIN,
};
pub use payout::{
    MockPayout, Payout, PayoutError, PayoutRecord, SidecarPayout, SidecarPayoutConfig,
    TransferRecord,
};
pub use recover::{reconcile_books, ReconcileReport, RECOVERED_REFUND_REASON};
pub use redundancy::{
    spawn_periodic_redundancy, RedundancyConfig, RedundancySampler, SampleReport, SampleVerdict,
};
pub use registry::{OperatorRecord, OperatorRegistry, RegistryError};
pub use reputation::{
    smoothed_score_bps, AuditReputationSource, NoReputation, ReputationSource, ReputationStats,
};
pub use stake::{
    resume_stake_slashes, spawn_periodic_stake_refresh, StakeRequirement, StakeSlashing,
};
pub use state::{BuyerFunds, CoordinatorConfig, CoordinatorState, PartnerConfig};
pub use stream::{StreamBook, StreamError, StreamReadout, STREAM_LINGER_MS};
pub use sweep::{
    reoffer_offline, spawn_periodic_payout_retry, spawn_periodic_sweep, sweep_expired,
    sweep_stale_offers, sweep_unpaid,
};

pub(crate) fn epoch_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
