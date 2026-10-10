//! The axum server: the operator-side long-poll endpoints and the
//! buyer-side submit/receipt endpoints from
//! build-notes-phase1-foundation.md §1.5.
//!
//! Auth boundary, spelled out because the wire types themselves are
//! uneven: `RegisterRequest`/`HeartbeatRequest` are self-authenticating
//! (each carries its own ed25519 signature, checked in
//! `crate::registry::OperatorRegistry`); `JobResultMessage` is
//! self-authenticating too (the embedded `SignedWorkReceipt` is
//! checked against the job's assigned operator pubkey before release).
//! `JobAccept` and the `next-job` long-poll carry no signature at all
//! in `covenant-compute-protocol` — so this layer requires the opaque
//! `operator_session` bearer token minted at registration on those two
//! calls. Buyer-submitted `SignedJobEnvelope`s are self-authenticating
//! (buyer signature) but the submit endpoint itself is open — any
//! party can propose a job, same trust posture x402 payers have today.

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{
    header::{AUTHORIZATION, CONTENT_TYPE},
    HeaderMap, StatusCode,
};
use axum::response::{IntoResponse, Response as AxumResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use covenant_a2a::A2ATaskStatus;
use covenant_audit::AuditKind;
use covenant_compute_protocol::{
    bond_refund_memo_for, output_hash_hex, withdrawal_memo_for, BatchInclusionProof, CancelRequest,
    CancelView, CapacityView, DisputeRequest, EscrowError, EscrowStatus, FederationEscrow,
    FundingSource, HeartbeatRequest, HeartbeatResponse, JobAccept, JobKind, JobOffer, JobResultAck,
    JobResultMessage, LeaseCloseRequest, LeaseView, OperatorStatus, RefundReason, RegisterRequest,
    RegisterResponse, ResultSettlement, SettlementBatch, SettlementProof, SignedJobEnvelope,
    StreamChunk, StreamPush, UnbondRequest, WithdrawalRequest, BOND_MEMO_PREFIX,
    CANCEL_MAX_SKEW_MS, DISPUTE_MAX_SKEW_MS, LEASE_CLOSE_MAX_SKEW_MS, PROTOCOL_VERSION,
    PROTOCOL_VERSION_HEADER, UNBOND_MAX_SKEW_MS, WITHDRAWAL_MAX_SKEW_MS,
};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::accounts::DepositOutcome;
use crate::bond::{BondPostOutcome, SlashRecord, UnbondOutcome, UnbondState};
use crate::deposit::{BondClaim, DepositClaim, RailError};
use crate::escrow::WithdrawOutcome;
use crate::jobs::{JobError, JobPhase, JobRecord, ReleaseCharges};
use crate::matcher::{cheapest_capable_ask_above_offer, select_operator};
use crate::registry::RegistryError;
use crate::state::{BuyerFunds, CoordinatorState};

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Unauthorized(String),
    #[error("{0}")]
    PaymentRequired(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    UpgradeRequired(String),
    #[error("{0}")]
    TooManyRequests(String),
    #[error("{0}")]
    ServiceUnavailable(String),
    #[error("{0}")]
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> AxumResponse {
        let (status, message) = match &self {
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
            ApiError::Unauthorized(m) => (StatusCode::UNAUTHORIZED, m.clone()),
            ApiError::PaymentRequired(m) => (StatusCode::PAYMENT_REQUIRED, m.clone()),
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, m.clone()),
            ApiError::Conflict(m) => (StatusCode::CONFLICT, m.clone()),
            ApiError::UpgradeRequired(m) => (StatusCode::UPGRADE_REQUIRED, m.clone()),
            ApiError::TooManyRequests(m) => (StatusCode::TOO_MANY_REQUESTS, m.clone()),
            ApiError::ServiceUnavailable(m) => (StatusCode::SERVICE_UNAVAILABLE, m.clone()),
            ApiError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m.clone()),
        };
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}

impl From<RegistryError> for ApiError {
    fn from(e: RegistryError) -> Self {
        let message = e.to_string();
        match e {
            RegistryError::BadSignature(_) => ApiError::BadRequest(message),
            RegistryError::NotRegistered(_) => ApiError::NotFound(message),
            RegistryError::BadSession => ApiError::Unauthorized(message),
            RegistryError::StaleHeartbeat(_) => ApiError::Unauthorized(message),
            RegistryError::RegistryFull(_) => ApiError::ServiceUnavailable(message),
            RegistryError::UnpayablePayout(_) => ApiError::BadRequest(message),
            RegistryError::InvalidProfile(_) => ApiError::BadRequest(message),
        }
    }
}

/// The largest request body the payload-carrying endpoints accept, set to
/// the shared IPC/HTTP frame cap (`covenant-ipc`'s `MAX_FRAME`, 8 MiB)
/// rather than axum's 2 MiB default. A job result carries the operator's
/// output inline — a synthesized speech clip, a transcript, an embedding —
/// and even a minute of audio runs to several megabytes, well past the
/// default. The frame cap is the network's inline ceiling, so `submit_job`
/// and `submit_result` honor it: a real result is not refused with a 413
/// before its handler ever runs. Every other endpoint keeps the small
/// default, since nothing else carries a payload near this size.
const MAX_INLINE_BODY_BYTES: usize = 8 * 1024 * 1024;

pub fn router(state: CoordinatorState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route("/federation/operators/register", post(register))
        .route("/federation/operators/heartbeat", post(heartbeat))
        .route("/federation/operators/:operator/next-job", get(next_job))
        .route(
            "/federation/operators/:operator/reputation",
            get(operator_reputation),
        )
        .route("/federation/operators/:operator/jobs", get(operator_jobs))
        .route(
            "/federation/jobs",
            post(submit_job).layer(DefaultBodyLimit::max(MAX_INLINE_BODY_BYTES)),
        )
        .route("/federation/jobs/:job_id/accept", post(accept_job))
        .route(
            "/federation/jobs/:job_id/result",
            post(submit_result).layer(DefaultBodyLimit::max(MAX_INLINE_BODY_BYTES)),
        )
        .route(
            "/federation/jobs/:job_id/stream",
            post(push_stream).get(job_stream),
        )
        .route("/federation/jobs/:job_id/receipt", get(job_status))
        .route("/federation/jobs/:job_id/dispute", post(dispute_job))
        .route("/federation/jobs/:job_id/cancel", post(cancel_job))
        .route("/federation/jobs/:job_id/close", post(close_lease))
        .route("/federation/jobs/:job_id/lease", get(lease_view))
        .route("/federation/buyers/deposit", post(claim_deposit))
        .route("/federation/buyers/withdraw", post(withdraw_balance))
        .route("/federation/deposit-info", get(deposit_info))
        .route("/federation/operators/bond", post(claim_bond))
        .route("/federation/operators/unbond", post(unbond_stake))
        .route("/federation/operators/:operator/bond", get(operator_bond))
        .route("/federation/bond-info", get(bond_info))
        .route("/federation/buyers/:buyer/balance", get(buyer_balance))
        .route(
            "/federation/buyers/:buyer/withdrawals",
            get(buyer_withdrawals),
        )
        .route("/federation/buyers/:buyer/jobs", get(buyer_jobs))
        .route("/federation/subsidy", get(subsidy))
        .route("/federation/subsidy/close", post(close_subsidy))
        .route("/admin/transfers", get(list_transfer_attempts))
        .route(
            "/admin/transfers/:attempt_id/resolve",
            post(resolve_transfer_attempt),
        )
        .route("/federation/fees", get(fees))
        .route("/federation/capacity", get(capacity))
        .route("/federation/jobs/:job_id/hidden", post(hidden_checks))
        .route("/federation/partners", get(partners))
        .route(
            "/federation/partners/:code/payouts",
            post(mark_partner_paid),
        )
        .route("/proof/receipts", get(receipts_feed))
        .route("/proof/receipts/:job_id", get(receipt_proof))
        .route("/proof/batch", get(settlement_batch))
        .route("/proof/batch/:job_id", get(batch_inclusion))
        .route("/vault/:owner/secrets", get(vault_list))
        .route(
            "/vault/:owner/secret/:label",
            post(vault_put).get(vault_get).delete(vault_delete),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            version_gate,
        ))
        .with_state(state)
}

/// The wire-version gate (the deploy-skew defense). Every response
/// names this build's [`PROTOCOL_VERSION`], so any client can tell how
/// new the coordinator is from any reply; when a deployment raises
/// `min_protocol` past its default 0, a `/federation/*` request
/// declaring an older version is refused 426 with both numbers named —
/// an operator on a stale node reads "upgrade", not a shape error. No
/// header counts as declaring 0 (bare curl, pre-versioning binaries);
/// a header that isn't a u32 is hostile wire and refuses on every
/// path. `/health` and `/metrics` never floor: probes and scrapers
/// don't speak the wire.
async fn version_gate(
    State(state): State<CoordinatorState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> AxumResponse {
    let declared = match req.headers().get(PROTOCOL_VERSION_HEADER) {
        None => Some(0u32),
        Some(value) => value
            .to_str()
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok()),
    };
    let floor = state.config().min_protocol;
    let response = match declared {
        None => ApiError::BadRequest(format!(
            "{PROTOCOL_VERSION_HEADER} must be a u32 wire version"
        ))
        .into_response(),
        Some(v) if v < floor && req.uri().path().starts_with("/federation/") => {
            ApiError::UpgradeRequired(format!(
                "wire protocol {v} is below this coordinator's floor {floor} \
                 (it speaks {PROTOCOL_VERSION}) — upgrade this client's binary"
            ))
            .into_response()
        }
        Some(_) => next.run(req).await,
    };
    stamp_protocol_version(response)
}

fn stamp_protocol_version(mut response: AxumResponse) -> AxumResponse {
    response.headers_mut().insert(
        axum::http::HeaderName::from_static(PROTOCOL_VERSION_HEADER),
        axum::http::HeaderValue::from(PROTOCOL_VERSION),
    );
    response
}

async fn health(State(state): State<CoordinatorState>) -> impl IntoResponse {
    let journal_healthy = state.journal().map(|j| j.is_healthy()).unwrap_or(true);
    health_response(journal_healthy)
}

/// A coordinator that can no longer durably record a fund transition is
/// worse than down (see [`crate::journal`]): it would refuse every real
/// operation while still answering. Reporting that as `503` lets a
/// deployment's health check restart or reroute it instead of leaving it
/// wedged and green. A journal-less (in-memory) coordinator has no
/// durable write to fail and is always ready.
fn health_response(journal_healthy: bool) -> (StatusCode, Json<serde_json::Value>) {
    if journal_healthy {
        (StatusCode::OK, Json(serde_json::json!({ "status": "ok" })))
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "status": "degraded",
                "reason": "journal writes are failing; the coordinator cannot persist fund state",
            })),
        )
    }
}

fn gauge(out: &mut String, name: &str, help: &str, value: impl std::fmt::Display) {
    sample(out, name, help, "gauge", value);
}

fn counter(out: &mut String, name: &str, help: &str, value: impl std::fmt::Display) {
    sample(out, name, help, "counter", value);
}

fn sample(out: &mut String, name: &str, help: &str, kind: &str, value: impl std::fmt::Display) {
    use std::fmt::Write;
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
    let _ = writeln!(out, "{name} {value}");
}

/// Prometheus text exposition of the coordinator's aggregate books —
/// what a deployment's scraper watches. Everything here is either
/// already public through a JSON view (the subsidy and fee endpoints)
/// or an anonymous count; no per-buyer, per-operator or per-partner
/// figure appears, so the open posture of those views carries over.
async fn metrics(State(state): State<CoordinatorState>) -> impl IntoResponse {
    use std::fmt::Write;

    let now_ms = crate::epoch_ms();
    let liveness_ms = state.config().operator_liveness_timeout.as_millis() as u64;
    let operators = state.registry().snapshot();
    let live = operators
        .iter()
        .filter(|(_, r)| now_ms.saturating_sub(r.last_seen_ms) <= liveness_ms)
        .count();
    let stats = state.jobs().stats();
    let (fees_captured, fee_jobs) = state.jobs().fees_captured();
    let subsidy = state.escrow().subsidy_status();

    let mut out = String::with_capacity(2048);
    let _ = writeln!(out, "# HELP compute_build_info Coordinator build metadata.");
    let _ = writeln!(out, "# TYPE compute_build_info gauge");
    let _ = writeln!(
        out,
        "compute_build_info{{version=\"{}\"}} 1",
        env!("CARGO_PKG_VERSION")
    );
    gauge(
        &mut out,
        "compute_protocol_version",
        "Wire protocol version this coordinator speaks.",
        PROTOCOL_VERSION,
    );
    gauge(
        &mut out,
        "compute_protocol_min_supported",
        "Wire-version floor for /federation/* requests; 0 admits versionless clients.",
        state.config().min_protocol,
    );
    gauge(
        &mut out,
        "compute_journal_healthy",
        "1 when the durable journal's last fund-transition write reached disk, 0 when persistence is failing.",
        state.journal().map(|j| u64::from(j.is_healthy())).unwrap_or(1),
    );
    gauge(
        &mut out,
        "compute_journal_bytes",
        "Size of the durable journal on disk; sawtooths with compaction, so steady growth means compaction is falling behind.",
        state.journal().and_then(|j| j.size_bytes()).unwrap_or(0),
    );
    gauge(
        &mut out,
        "compute_operators_registered",
        "Operators currently in the registry.",
        operators.len(),
    );
    gauge(
        &mut out,
        "compute_operators_live",
        "Registered operators seen within the liveness window (matchable).",
        live,
    );

    let _ = writeln!(
        out,
        "# HELP compute_jobs Jobs in the book by lifecycle phase."
    );
    let _ = writeln!(out, "# TYPE compute_jobs gauge");
    for (phase, count) in [
        ("offered", stats.offered),
        ("accepted", stats.accepted),
        ("rejected", stats.rejected),
        ("completed", stats.completed),
        ("failed", stats.failed),
        ("refunded", stats.refunded),
        ("awaiting_check", stats.awaiting_check),
    ] {
        let _ = writeln!(out, "compute_jobs{{phase=\"{phase}\"}} {count}");
    }
    gauge(
        &mut out,
        "compute_jobs_disputed",
        "Jobs carrying a buyer dispute, whatever their phase.",
        stats.disputed,
    );

    gauge(
        &mut out,
        "compute_fee_bps",
        "Marketplace fee rate currently in force, in basis points.",
        state.config().fee.bps(),
    );
    counter(
        &mut out,
        "compute_fees_captured_micro_usdc_total",
        "Marketplace fees withheld across released jobs, in micro-USDC.",
        fees_captured,
    );
    counter(
        &mut out,
        "compute_fee_jobs_charged_total",
        "Released jobs a marketplace fee was withheld from.",
        fee_jobs,
    );

    gauge(
        &mut out,
        "compute_subsidy_enforced",
        "1 when a bootstrap subsidy policy is attached, 0 when the kill-switch is closed.",
        u8::from(subsidy.enforced),
    );
    gauge(
        &mut out,
        "compute_subsidy_closed",
        "1 once the subsidy was closed at runtime — the journaled latch no restart re-arms.",
        u8::from(subsidy.closed),
    );
    gauge(
        &mut out,
        "compute_subsidy_bootstrap_committed_micro_usdc",
        "Non-refunded bootstrap holds counting against the subsidy ceiling, in micro-USDC.",
        subsidy.bootstrap_committed_micro_usdc,
    );
    gauge(
        &mut out,
        "compute_subsidy_organic_released_micro_usdc",
        "Released organic revenue the subsidy ceiling is derived from, in micro-USDC.",
        subsidy.organic_released_micro_usdc,
    );
    gauge(
        &mut out,
        "compute_subsidy_ceiling_micro_usdc",
        "Current subsidy ceiling, in micro-USDC.",
        subsidy.ceiling_micro_usdc,
    );
    gauge(
        &mut out,
        "compute_subsidy_remaining_micro_usdc",
        "Subsidy headroom left under the ceiling, in micro-USDC.",
        subsidy.remaining_micro_usdc,
    );

    gauge(
        &mut out,
        "compute_suspended_transfers",
        "Open transfer-attempt brackets: pushes in flight or with an unknown on-chain \
         outcome awaiting admin reconciliation. Nonzero for more than a sweep interval \
         means money is suspended — alert on it.",
        state.attempts().open_attempts().len() as u64,
    );

    gauge(
        &mut out,
        "compute_prefunding_enforced",
        "1 when organic jobs require a covering verified deposit, 0 in open mode.",
        u8::from(state.config().require_prefunded_buyers),
    );

    // The money books, summed — enough for a scraper to watch the
    // conservation identities themselves: deposited = available +
    // withdrawn + organic held + organic released; released (any
    // source) = payouts pushed + payouts outstanding + fees captured;
    // bonds posted = slashed + refunded + at stake. A drift in any of
    // them is a books bug, not a traffic pattern. The release identity
    // is pre-computed as compute_reconciliation_drift_micro_usdc below,
    // because its owed side must be summed from the escrow holds to stay
    // honest: compute_payouts_outstanding_micro_usdc is the record's own
    // account, which reads a receipt-less lease back at its window
    // ceiling, so the two legitimately differ when a lease lost its
    // metered stamp — the drift gauge is the one that stays zero.
    let deposited = state.accounts().total_deposited();
    let withdrawn = state.withdrawals().total_withdrawn();
    let organic = state.escrow().money_totals(FundingSource::Organic);
    let bootstrap = state.escrow().money_totals(FundingSource::Bootstrap);
    let (payouts_pushed, payouts_outstanding) = state.jobs().payout_totals();
    let partner_accrued: u64 = state
        .jobs()
        .partner_accruals()
        .values()
        .map(|(amount, _)| amount)
        .sum();
    let bonds = state.bonds().totals();
    counter(
        &mut out,
        "compute_deposited_micro_usdc_total",
        "Rail-verified buyer deposits ever credited, in micro-USDC.",
        deposited,
    );
    counter(
        &mut out,
        "compute_withdrawn_micro_usdc_total",
        "Buyer withdrawals ever debited, pushed or not, in micro-USDC.",
        withdrawn,
    );
    gauge(
        &mut out,
        "compute_buyer_available_micro_usdc",
        "Deposits not yet withdrawn, held or spent, in micro-USDC.",
        deposited
            .saturating_sub(withdrawn)
            .saturating_sub(organic.held_micro_usdc)
            .saturating_sub(organic.released_micro_usdc),
    );
    let _ = writeln!(
        out,
        "# HELP compute_escrow_held_micro_usdc In-flight escrow holds by funding source, in micro-USDC."
    );
    let _ = writeln!(out, "# TYPE compute_escrow_held_micro_usdc gauge");
    let _ = writeln!(
        out,
        "# HELP compute_escrow_released_micro_usdc_total Escrow released to settlements by funding source, in micro-USDC."
    );
    let _ = writeln!(
        out,
        "# TYPE compute_escrow_released_micro_usdc_total counter"
    );
    let _ = writeln!(
        out,
        "# HELP compute_escrow_refunded_micro_usdc_total Escrow refunded to buyers by funding source, in micro-USDC."
    );
    let _ = writeln!(
        out,
        "# TYPE compute_escrow_refunded_micro_usdc_total counter"
    );
    for (source, totals) in [("organic", organic), ("bootstrap", bootstrap)] {
        let _ = writeln!(
            out,
            "compute_escrow_held_micro_usdc{{funding_source=\"{source}\"}} {}",
            totals.held_micro_usdc
        );
        let _ = writeln!(
            out,
            "compute_escrow_released_micro_usdc_total{{funding_source=\"{source}\"}} {}",
            totals.released_micro_usdc
        );
        let _ = writeln!(
            out,
            "compute_escrow_refunded_micro_usdc_total{{funding_source=\"{source}\"}} {}",
            totals.refunded_micro_usdc
        );
    }
    counter(
        &mut out,
        "compute_payouts_pushed_micro_usdc_total",
        "Operator payouts the backend accepted, in micro-USDC.",
        payouts_pushed,
    );
    gauge(
        &mut out,
        "compute_payouts_outstanding_micro_usdc",
        "Released jobs' operator net still owed because the push failed, in micro-USDC.",
        payouts_outstanding,
    );
    gauge(
        &mut out,
        "compute_reconciliation_drift_micro_usdc",
        "Signed drift in the release-to-payout books: escrow released, less payouts \
         pushed, the operator net still owed, and fees captured, with the owed side \
         summed from the authoritative escrow holds. Zero when the books balance; a \
         nonzero value is an accounting bug rather than traffic, so alert on it.",
        state.reconciliation_drift_micro_usdc(),
    );
    counter(
        &mut out,
        "compute_partner_accrued_micro_usdc_total",
        "Rev-share accrued to partners out of captured fees, in micro-USDC.",
        partner_accrued,
    );
    counter(
        &mut out,
        "compute_partner_paid_micro_usdc_total",
        "Rev-share recorded as actually paid out to partners, in micro-USDC.",
        state.partner_payouts().total_paid(),
    );
    counter(
        &mut out,
        "compute_bonds_posted_micro_usdc_total",
        "Rail-verified operator bond posts ever credited, in micro-USDC.",
        bonds.posted_micro_usdc,
    );
    counter(
        &mut out,
        "compute_bonds_slashed_micro_usdc_total",
        "Stake taken for coordinator-proven faults, in micro-USDC.",
        bonds.slashed_micro_usdc,
    );
    counter(
        &mut out,
        "compute_bonds_refunded_micro_usdc_total",
        "Matured unbond refunds actually paid back out, in micro-USDC.",
        bonds.refunded_micro_usdc,
    );
    gauge(
        &mut out,
        "compute_bonds_at_stake_micro_usdc",
        "Stake a fault can still take: posted minus slashed minus refunded, in micro-USDC.",
        bonds.at_stake_micro_usdc,
    );
    gauge(
        &mut out,
        "compute_bonds_unbonding_micro_usdc",
        "Unbond requests maturing: still slashable, no longer committable, in micro-USDC.",
        bonds.unbonding_micro_usdc,
    );

    ([(CONTENT_TYPE, "text/plain; version=0.0.4")], out)
}

pub(crate) fn funding_source_str(fs: FundingSource) -> &'static str {
    match fs {
        FundingSource::Bootstrap => "bootstrap",
        FundingSource::Organic => "organic",
    }
}

fn extract_bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
}

fn check_session(
    state: &CoordinatorState,
    operator_pubkey_b58: &str,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    state
        .registry()
        .check_session(operator_pubkey_b58, extract_bearer(headers))
        .map_err(ApiError::from)
}

/// Gate for the operator-only admin surface: a bearer token matching
/// the configured one. Fail closed — no configured token means every
/// admin call is 401.
fn check_admin(state: &CoordinatorState, headers: &HeaderMap) -> Result<(), ApiError> {
    let Some(expected) = state.config().admin_token.as_deref() else {
        return Err(ApiError::Unauthorized(
            "no admin token is configured; the admin surface is disabled".into(),
        ));
    };
    // Constant-time compare so a byte-by-byte timing signal can't walk
    // the admin token out one leading byte at a time — the same standard
    // `registry::check_session` holds for the operator session token,
    // and this secret gates more (the subsidy kill-switch, partner
    // payout addresses, mark-paid). A missing or malformed bearer
    // compares as empty (length differs, rejected) rather than branching
    // early on the secret's presence.
    let presented = extract_bearer(headers).unwrap_or("");
    if !bool::from(presented.as_bytes().ct_eq(expected.as_bytes())) {
        return Err(ApiError::Unauthorized("admin token rejected".into()));
    }
    Ok(())
}

/// Gate for endpoints that return a buyer's own private data: the
/// caller must present a fresh signature over exactly `path` by
/// `expected_pubkey_b58` (see `read_auth` in the protocol crate).
/// Everything short of that is a 401.
fn verify_signed_read(
    headers: &HeaderMap,
    expected_pubkey_b58: &str,
    path: &str,
) -> Result<(), ApiError> {
    let header = |name: &'static str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| ApiError::Unauthorized(format!("missing {name} header")))
    };
    let signed_at_ms: u64 = header(covenant_compute_protocol::READ_SIGNED_AT_HEADER)?
        .parse()
        .map_err(|_| {
            ApiError::Unauthorized(format!(
                "{} must be epoch milliseconds",
                covenant_compute_protocol::READ_SIGNED_AT_HEADER
            ))
        })?;
    let signature = header(covenant_compute_protocol::READ_SIGNATURE_HEADER)?;
    covenant_compute_protocol::verify_read(
        expected_pubkey_b58,
        path,
        signed_at_ms,
        signature,
        crate::epoch_ms(),
    )
    .map_err(|e| ApiError::Unauthorized(format!("read signature rejected: {e}")))
}

async fn register(
    State(state): State<CoordinatorState>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, ApiError> {
    let session = state
        .registry()
        .register(
            &req,
            crate::epoch_ms(),
            state.config().max_operators,
            state.config().stake.is_some(),
        )
        .map_err(ApiError::from)?;
    // Read the new operator's stake now rather than at the next refresh, so a
    // node that staked before registering is matchable within seconds.
    if state.config().stake.is_some() {
        let state = state.clone();
        let operator = req.profile.operator.pubkey_base58();
        tokio::spawn(async move {
            crate::stake::refresh_operator(&state, &crate::stake::stake_client(), &operator).await;
        });
    }
    Ok(Json(RegisterResponse {
        accepted: true,
        operator_session: Some(session),
        reason: None,
        fee_bps: state.config().fee.bps(),
    }))
}

async fn heartbeat(
    State(state): State<CoordinatorState>,
    Json(req): Json<HeartbeatRequest>,
) -> Result<Json<HeartbeatResponse>, ApiError> {
    let now_ms = crate::epoch_ms();
    let previous = state
        .registry()
        .heartbeat(&req, now_ms)
        .map_err(ApiError::from)?;
    // A node declaring itself Offline (dead backend, shutting down)
    // heals its queued offers now — the stale sweep would get there,
    // one whole re-offer window later. Transition-edged: a repeated
    // Offline beat has nothing left to move, and fresh matches never
    // pick an Offline operator in the first place.
    if req.status == OperatorStatus::Offline && previous != OperatorStatus::Offline {
        let operator = req.operator.pubkey_base58();
        let moved = crate::sweep::reoffer_offline(&state, &operator, now_ms).await;
        if !moved.is_empty() {
            tracing::info!(
                %operator,
                count = moved.len(),
                "offline declaration re-matched queued offers"
            );
        }
    }
    Ok(Json(HeartbeatResponse { ack: true }))
}

async fn next_job(
    State(state): State<CoordinatorState>,
    Path(operator): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Option<JobOffer>>, ApiError> {
    state
        .registry()
        .record(&operator)
        .ok_or_else(|| ApiError::NotFound(format!("operator {operator} is not registered")))?;
    check_session(&state, &operator, &headers)?;
    let offer = state
        .registry()
        .poll_next_job(
            &operator,
            state.config().long_poll_timeout,
            crate::epoch_ms(),
        )
        .await
        .map_err(ApiError::from)?;
    Ok(Json(offer))
}

#[derive(Serialize)]
struct ReputationView {
    operator_pubkey_b58: String,
    #[serde(flatten)]
    stats: crate::reputation::ReputationStats,
    /// This deployment's matcher floors, `0` when disabled — the score
    /// and base committed stake an operator must clear to win organic
    /// work.
    min_score_bps: u32,
    min_bond_micro_usdc: u64,
    /// This operator's effective stake requirement: the base floor, or —
    /// for a lease operator pricing by the GPU-hour — the higher figure
    /// its advertised rate scales it to. Equal to `min_bond_micro_usdc`
    /// for everyone else and while the lease scaling is off.
    required_bond_micro_usdc: u64,
    committed_bond_micro_usdc: u64,
    /// The CVNT stake this deployment requires for a node identity, in
    /// base units (`0` when none), and whether the last chain read found
    /// this operator holding it.
    stake_required: u64,
    staked: bool,
    /// Directory standing, the matcher's remaining per-operator gates:
    /// known at all, declared status, and how long since it was seen
    /// (`None` for an operator this coordinator has never met).
    registered: bool,
    status: Option<covenant_compute_protocol::OperatorStatus>,
    seen_ms_ago: Option<u64>,
    /// The matcher's freshness gate as it stands right now: Online and
    /// seen within this deployment's liveness window. `false` with
    /// `registered: true` means the node stopped heartbeating.
    live: bool,
    /// The verdict the other fields explain: would the matcher consider
    /// this operator right now for a job it satisfies? Price and
    /// capability fit stay per-job — `matchable: true` with no wins
    /// means the ask or the profile, not standing.
    matchable: bool,
}

/// The counts behind a match decision, in the open: released successes,
/// attributed faults, and the smoothed score the matcher tie-breaks
/// on — checkable against the per-job audit rows with zero homework.
/// Plus the operator's standing against this deployment's floors and
/// the directory, so "why am I not winning work" is answerable from
/// one read (the node binary's `status` command renders exactly this).
/// Not gated on a session: reputation is public standing, same posture
/// as the buyer-jobs listing.
async fn operator_reputation(
    State(state): State<CoordinatorState>,
    Path(operator): Path<String>,
) -> Json<ReputationView> {
    let stats = state.reputation().stats(&operator).await;
    let config = state.config();
    let record = state.registry().record(&operator);
    let committed = state.bonds().status(&operator).committed_micro_usdc;
    let now_ms = crate::epoch_ms();
    let liveness_ms = config.operator_liveness_timeout.as_millis() as u64;
    // `matcher::select_operator`'s own standing gate, plus both floors
    // (a floor of 0 excludes nothing, exactly as the matcher reads it).
    let live = record
        .as_ref()
        .is_some_and(|r| crate::matcher::in_standing(r, now_ms, liveness_ms));
    // The stake floor an operator must clear is rate-scaled for a lease
    // operator, so read it against this operator's own advertised profile
    // rather than the flat base — otherwise a lease node under-bonded for
    // its rate would read `matchable` here while the matcher refuses it.
    let required_bond = record
        .as_ref()
        .map(|r| config.bond_floor().required_micro_usdc(&r.profile))
        .unwrap_or(config.min_bond_micro_usdc);
    let staked = record.as_ref().is_some_and(|r| r.staked);
    Json(ReputationView {
        matchable: live
            && staked
            && stats.score_bps >= config.min_operator_score_bps
            && committed >= required_bond,
        stake_required: config.stake.as_ref().map_or(0, |s| s.min_amount),
        staked,
        live,
        registered: record.is_some(),
        status: record.as_ref().map(|r| r.status),
        seen_ms_ago: record
            .as_ref()
            .map(|r| now_ms.saturating_sub(r.last_seen_ms)),
        min_score_bps: config.min_operator_score_bps,
        min_bond_micro_usdc: config.min_bond_micro_usdc,
        required_bond_micro_usdc: required_bond,
        committed_bond_micro_usdc: committed,
        stats,
        operator_pubkey_b58: operator,
    })
}

#[derive(Serialize)]
struct SubmitJobResponse {
    job_id: Uuid,
    status: &'static str,
}

/// The phase a duplicate submission of `job_id` is acknowledged with,
/// or `None` when the job is genuinely new. The record is
/// authoritative; with no record, the fund state answers: mid-request
/// the record trails the hold by a moment (`offered`, the phase it is
/// racing into), and a hold whose record died with a crash was
/// refunded at boot (`crate::recover`) — echo that refund, not a
/// phantom offer.
async fn known_job_phase(state: &CoordinatorState, job_id: Uuid) -> Option<&'static str> {
    if let Some(record) = state.jobs().get(job_id) {
        return Some(record.phase.as_str());
    }
    match state.escrow().status(job_id).await {
        Ok(EscrowStatus::Refunded) => Some("refunded"),
        Ok(EscrowStatus::Released) => Some("completed"),
        Ok(_) => Some("offered"),
        Err(_) => None,
    }
}

async fn submit_job(
    State(state): State<CoordinatorState>,
    Json(envelope): Json<SignedJobEnvelope>,
) -> Result<(StatusCode, Json<SubmitJobResponse>), ApiError> {
    envelope
        .verify()
        .map_err(|e| ApiError::BadRequest(format!("envelope does not verify: {e}")))?;

    // Structurally malformed inference input is the buyer's error, and
    // it is refused here — before any escrow hold or match — because
    // downstream every path charges the OPERATOR for it: a node that
    // rejects at admission books a fault, and one that accepts fails
    // the job at the executor and books a fault. Neither may happen on
    // input only the buyer controls.
    envelope
        .payload
        .validate_input()
        .map_err(|e| ApiError::BadRequest(format!("job input malformed: {e}")))?;

    // Agent work is opened to buyers by deployment policy, and a check is
    // only ever ordered by the coordinator itself: a buyer-posted check
    // would be a verdict nobody's settlement waits on.
    match envelope.payload.kind {
        JobKind::AgentTask => {
            let buyer = envelope.payload.buyer.pubkey_base58();
            match &state.config().agent {
                Some(policy) if policy.admits(&buyer) => {
                    let least = policy.least_offer_micro_usdc();
                    if envelope.payload.price_micro_usdc < least {
                        return Err(ApiError::BadRequest(format!(
                            "an agent task pays the build and at least one check, so offer \
                             at least {least} micro-USDC; you are charged what the build \
                             spends, up to your offer, and only if it passes"
                        )));
                    }
                }
                Some(_) => {
                    return Err(ApiError::Unauthorized(format!(
                        "agent tasks are open to approved buyers only; {buyer} is not one"
                    )))
                }
                None => {
                    return Err(ApiError::BadRequest(
                        "this coordinator does not take agent tasks".into(),
                    ))
                }
            }
        }
        JobKind::AgentCheck => {
            return Err(ApiError::BadRequest(
                "agent checks are ordered by the coordinator, not bought".into(),
            ))
        }
        _ => {}
    }

    // A job this coordinator already knows is answered with the truth
    // before any admission gate can refuse it — in particular before
    // the deadline gate below, because a replayed envelope naturally
    // outlives its own deadline: the buyer that crashed mid-purchase
    // and retries after a restart is exactly the buyer that arrives
    // late, and a 400 here would turn "already served and paid" into
    // what reads as a refusal to serve. The gates protect operators
    // from being MATCHED on bad jobs; an echo matches no one.
    if let Some(status) = known_job_phase(&state, envelope.payload.job_id).await {
        return Ok((
            StatusCode::OK,
            Json(SubmitJobResponse {
                job_id: envelope.payload.job_id,
                status,
            }),
        ));
    }

    // An envelope already past its own deadline is refused here for the
    // same reason malformed input is: matched, it charges the OPERATOR.
    // A job whose `issued_at_ms + deadline_ms` has passed is swept to a
    // DeadlineExpired refund the moment it is offered, and that refund
    // faults the assigned operator — a penalty on a deadline only the
    // buyer set — while any compute spent racing the sweep is wasted.
    // The predicate matches the sweep's exactly (`crate::sweep`), so
    // admission never lets through a job the next sweep tick would kill;
    // saturating so a preposterous deadline can't overflow the sum.
    let now_ms = crate::epoch_ms();
    let expires_at_ms = envelope
        .payload
        .issued_at_ms
        .saturating_add(envelope.payload.deadline_ms);
    if expires_at_ms < now_ms {
        return Err(ApiError::BadRequest(format!(
            "job {} is already past its deadline ({expires_at_ms}ms) at the coordinator clock \
             ({now_ms}ms); it would be refunded before any operator could serve it",
            envelope.payload.job_id
        )));
    }

    // Per-buyer in-flight ceiling (C9): in open (pre-prefunding) mode a
    // hold costs the submitter nothing, so unbounded submissions mean
    // unbounded holds and journal growth. Checked before any state is
    // created; advisory under concurrency (the true bound is cap plus
    // in-flight requests), which is what a volumetric backstop needs.
    if let Some(cap) = state.config().max_inflight_per_buyer {
        let buyer_key = envelope.payload.buyer.pubkey_base58();
        let in_flight = state
            .jobs()
            .by_buyer(&buyer_key)
            .iter()
            .filter(|(_, r)| matches!(r.phase, JobPhase::Offered | JobPhase::Accepted))
            .count();
        if in_flight >= cap {
            return Err(ApiError::TooManyRequests(format!(
                "buyer {buyer_key} already has {in_flight} jobs in flight (ceiling {cap}); \
                 wait for one to conclude"
            )));
        }
    }

    let job_id = envelope.payload.job_id;
    let amount = envelope.payload.price_micro_usdc;

    // An offer whose operator net could never clear this coordinator's
    // per-payout cap is refused here, before any hold. Matched and
    // completed it would release the buyer's escrow and then strand at
    // the payout push forever — `Payout::pay` enforces the same cap, and
    // the retry sweep can only spin on a job it can never settle. This is
    // the up-front refusal the obligation cap already earns a withdrawal
    // (payout.rs): no money is held for a push that can't land. The net
    // is the gross price minus the marketplace fee, the exact figure
    // `submit_result` later pushes to the operator.
    if let Some(cap) = state.payout().per_job_cap_micro_usdc() {
        let fee = state.config().fee.take_of(amount);
        let net = amount - fee;
        if net > cap {
            return Err(ApiError::BadRequest(format!(
                "job {job_id}: the operator net on this offer ({net} micro-USDC — price \
                 {amount} minus {fee} marketplace fee) exceeds this coordinator's per-payout \
                 cap ({cap} micro-USDC); it would be matched and completed but never paid \
                 out. Lower the offer price and retry."
            )));
        }
    }

    let fix = crate::agent::admit_fix(&state, &envelope).map_err(ApiError::BadRequest)?;

    let escrow_hold = match state
        .escrow()
        .hold(job_id, &envelope.payload.buyer, amount)
        .await
    {
        Ok(hold) => hold,
        // Idempotent replay, the race backstop: two submissions of one
        // job id in flight at once, the loser landing here after the
        // `known_job_phase` check above saw nothing. Acknowledge
        // against what the winner created instead of re-holding and
        // re-dispatching — the buyer declared the job idempotent on
        // its id, and its authoritative status still comes from the
        // receipt poll.
        Err(EscrowError::AlreadyHeld(_)) => {
            let status = known_job_phase(&state, job_id).await.unwrap_or("offered");
            return Ok((StatusCode::OK, Json(SubmitJobResponse { job_id, status })));
        }
        Err(e @ EscrowError::InsufficientFunds { .. }) => {
            return Err(ApiError::PaymentRequired(e.to_string()));
        }
        Err(e @ EscrowError::SubsidyExhausted { .. }) => {
            return Err(ApiError::Conflict(e.to_string()));
        }
        Err(e) => return Err(ApiError::Internal(e.to_string())),
    };

    // A fix goes to a seat whose stake owners did not build its
    // reproduction: that seat is paid only if the fix passes.
    let winner = match &fix {
        Some(fix) => {
            crate::matcher::select_operator_excluding(
                state.registry(),
                state.reputation(),
                state.bonds(),
                &envelope.payload.capability_requirement,
                envelope.payload.price_micro_usdc,
                now_ms,
                state.config().operator_liveness_timeout,
                state.config().min_operator_score_bps,
                state.config().bond_floor(),
                &fix.exclusions,
            )
            .await
        }
        None => {
            select_operator(
                state.registry(),
                state.reputation(),
                state.bonds(),
                &envelope.payload.capability_requirement,
                envelope.payload.price_micro_usdc,
                now_ms,
                state.config().operator_liveness_timeout,
                state.config().min_operator_score_bps,
                state.config().bond_floor(),
                None,
            )
            .await
        }
    };
    let Some(operator_pubkey_b58) = winner else {
        // Name the refusal in the one term the buyer can act on before
        // the envelope moves into its terminal record. "No operator" is
        // wrong when supply is online but asking more than the offer: a
        // buyer told that gives up on a market that would serve a higher
        // bid. Only reached after `select_operator` returned nothing, so
        // a capable operator here necessarily asks above the offer.
        let offered = envelope.payload.price_micro_usdc;
        let refusal = match cheapest_capable_ask_above_offer(
            state.registry(),
            state.reputation(),
            state.bonds(),
            &envelope.payload.capability_requirement,
            offered,
            now_ms,
            state.config().operator_liveness_timeout,
            state.config().min_operator_score_bps,
            state.config().bond_floor(),
        )
        .await
        {
            Some(floor) => format!(
                "job {job_id}: the offer of {offered} micro-USDC is under the cheapest ask that \
                 serves this request ({floor} micro-USDC); raise the price to at least {floor} \
                 and retry"
            ),
            None => format!(
                "no operator is currently serving job {job_id}'s request; check the capacity \
                 directory for what is purchasable"
            ),
        };
        let _ = state
            .escrow()
            .refund(job_id, RefundReason::AdmissionFailed)
            .await;
        // The refund above already settled the money; this record is
        // terminal bookkeeping for the buyer's receipt poll, so a
        // journal failure here is logged, not surfaced over the refund.
        if let Err(e) = state.jobs().insert(
            job_id,
            JobRecord {
                operator_pubkey_b58: String::new(),
                payout_address: String::new(),
                buyer_referral_code: envelope.payload.referral_code.clone(),
                envelope,
                escrow_hold,
                phase: JobPhase::Refunded,
                receipt: None,
                output: None,
                fee_micro_usdc: 0,
                referral_code: None,
                partner_share_micro_usdc: 0,
                buyer_partner_share_micro_usdc: 0,
                payout: None,
                concluded_at_ms: None,
                refund_reason: Some(RefundReason::AdmissionFailed),
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
                order: None,
            },
        ) {
            tracing::error!(%job_id, error = %e, "failed to record refunded job");
        }
        state
            .record_audit(AuditKind::ComputeJobRefunded {
                job_id,
                reason: RefundReason::AdmissionFailed.as_str().into(),
                operator_pubkey_b58: None,
            })
            .await;
        return Err(ApiError::Conflict(refusal));
    };

    let offer = JobOffer {
        envelope: envelope.clone(),
        escrow_hold: escrow_hold.clone(),
        rework: None,
        reproduction: fix.as_ref().map(|f| f.patch.clone()),
    };
    // Captured into the durable record because the registry restarts
    // empty: the address the operator declared when it won this match
    // is the one its release pays — and the partner whose referral it
    // registered under is the one the accrual credits — whatever
    // happens to the registry.
    let (payout_address, referral_code) = state
        .registry()
        .record(&operator_pubkey_b58)
        .map(|r| (r.payout_address, r.referral_code))
        .unwrap_or_default();
    // The offer must not go out unless the job record that authorizes
    // its eventual release is durable — refund and bail otherwise.
    if let Err(e) = state.jobs().insert(
        job_id,
        JobRecord {
            operator_pubkey_b58: operator_pubkey_b58.clone(),
            payout_address,
            buyer_referral_code: envelope.payload.referral_code.clone(),
            envelope,
            escrow_hold,
            phase: JobPhase::Offered,
            receipt: None,
            output: None,
            fee_micro_usdc: 0,
            referral_code,
            partner_share_micro_usdc: 0,
            buyer_partner_share_micro_usdc: 0,
            payout: None,
            concluded_at_ms: None,
            refund_reason: None,
            dispute: None,
            offered_at_ms: crate::epoch_ms(),
            // A fix may go only to a seat outside its reproduction's.
            pinned: fix.is_some(),
            accepted_at_ms: None,
            metered_elapsed_ms: None,
            close_requested_at_ms: None,
            lease_access: None,
            check_jobs: Vec::new(),
            checks_task: None,
            hidden_checks: None,
            vote_round: None,
            rework: None,
            order: fix.as_ref().map(|f| crate::jobs::TaskOrder::Fix {
                reproduction: f.reproduction,
                reproducer: f.reproducer.clone(),
                patch: f.patch.clone(),
            }),
        },
    ) {
        if let Err(refund_err) = state
            .escrow()
            .refund(job_id, RefundReason::AdmissionFailed)
            .await
        {
            tracing::error!(%job_id, error = %refund_err, "refund after journal failure also failed");
        }
        return Err(ApiError::Internal(e.to_string()));
    }
    // One fix per reproduction: two posted at once both pass admission, and
    // the second to link here is refunded before any seat sees it.
    if let Some(fix) = &fix {
        let linked = state
            .jobs()
            .link_fix(fix.reproduction, job_id)
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        if !linked {
            let _ = state
                .escrow()
                .refund(job_id, RefundReason::AdmissionFailed)
                .await;
            state
                .jobs()
                .conclude_unpaid(job_id, JobPhase::Refunded, RefundReason::AdmissionFailed)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            return Err(ApiError::Conflict(
                "the reproduction already has a fix".into(),
            ));
        }
    }
    // The winner came from a live snapshot taken under the registry
    // lock a moment ago; a concurrent deregistration between the
    // snapshot and this delivery is the only way `deliver` fails here.
    if !state.registry().deliver(&operator_pubkey_b58, offer) {
        let _ = state
            .escrow()
            .refund(job_id, RefundReason::AdmissionFailed)
            .await;
        state
            .jobs()
            .conclude_unpaid(job_id, JobPhase::Refunded, RefundReason::AdmissionFailed)
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        return Err(ApiError::Conflict(format!(
            "matched operator for job {job_id} is no longer registered"
        )));
    }

    state
        .record_audit(AuditKind::ComputeJobOffered {
            job_id,
            operator_pubkey_b58,
            price_micro_usdc: amount,
            funding_source: funding_source_str(state.config().default_funding_source).into(),
        })
        .await;

    Ok((
        StatusCode::ACCEPTED,
        Json(SubmitJobResponse {
            job_id,
            status: "offered",
        }),
    ))
}

async fn accept_job(
    State(state): State<CoordinatorState>,
    Path(job_id): Path<Uuid>,
    headers: HeaderMap,
    Json(decision): Json<JobAccept>,
) -> Result<StatusCode, ApiError> {
    let decision_job_id = match &decision {
        JobAccept::Accept { job_id } => *job_id,
        JobAccept::Reject { job_id, .. } => *job_id,
    };
    if decision_job_id != job_id {
        return Err(ApiError::BadRequest(
            "path job_id does not match the decision's job_id".into(),
        ));
    }

    let record = state
        .jobs()
        .get(job_id)
        .ok_or_else(|| ApiError::NotFound(format!("no such job {job_id}")))?;
    check_session(&state, &record.operator_pubkey_b58, &headers)?;

    // Both legs write through the assignee-and-still-live guard:
    // between this handler's session check (against the record read
    // above) and the phase write, the stale-offer sweep may have
    // re-offered the job to another operator, or the job may have
    // concluded (deadline-swept, buyer-cancelled). A late accept must
    // not overwrite the new assignment or resurrect a settled job —
    // and a late reject must not refund a job someone else is serving.
    // The 409 tells the node to drop the offer without executing.
    let reassigned = |job_id: Uuid| {
        ApiError::Conflict(format!(
            "job {job_id} is no longer this operator's to decide; it was re-offered or has \
             already concluded"
        ))
    };
    match decision {
        JobAccept::Accept { .. } => {
            if !state
                .jobs()
                .set_phase_if_assigned(
                    job_id,
                    &record.operator_pubkey_b58,
                    JobPhase::Accepted,
                    None,
                )
                .map_err(|e| ApiError::Internal(e.to_string()))?
            {
                return Err(reassigned(job_id));
            }
            // That write stamped `accepted_at_ms`, the meter's t0. A
            // lease session mirrors it on-chain from the same stamp, so
            // both meters bill from one timestamp. No-op unless a
            // deployment configured a meter, and never fatal: the
            // accept has landed either way.
            crate::onchain_meter::open_lease_onchain(&state, job_id).await;
        }
        JobAccept::Reject { reason, .. } => {
            if !state
                .jobs()
                .set_phase_if_assigned(
                    job_id,
                    &record.operator_pubkey_b58,
                    JobPhase::Rejected,
                    Some(RefundReason::OperatorRejected),
                )
                .map_err(|e| ApiError::Internal(e.to_string()))?
            {
                return Err(reassigned(job_id));
            }
            // A refund that comes back `AlreadySettled` — a racing deadline
            // sweep, or a sibling branch whose phase write failed after its
            // own refund landed — must fall through to the void, not 409
            // out. The buyer's hold is already back either way, and this is
            // the only in-process path that can return the funded vault too:
            // boot reconcile re-refunds a stranded hold but never voids.
            // Bail before the void and the operator could still settle the
            // vault for the seconds it metered before rejecting.
            match state
                .escrow()
                .refund(job_id, RefundReason::OperatorRejected)
                .await
            {
                Ok(()) => {
                    state
                        .record_audit(AuditKind::ComputeJobRefunded {
                            job_id,
                            reason: RefundReason::OperatorRejected.as_str().into(),
                            operator_pubkey_b58: Some(record.operator_pubkey_b58.clone()),
                        })
                        .await;
                }
                Err(EscrowError::AlreadySettled(_)) => {}
                Err(e) => return Err(ApiError::Conflict(e.to_string())),
            }
            tracing::info!(%job_id, %reason, "operator rejected job");
            // An operator that rejects a lease it had already accepted
            // opened a funded on-chain vault at that accept; the buyer just
            // got the whole window back off-chain, so the vault has to
            // return to them too, or the operator could still settle it for
            // the seconds between its accept and its reject. A no-op for an
            // offer rejected before it was accepted (no vault) or a
            // deployment with no chain meter.
            crate::onchain_meter::void_lease_onchain(&state, &record).await;
        }
    }
    Ok(StatusCode::OK)
}

async fn submit_result(
    State(state): State<CoordinatorState>,
    Path(job_id): Path<Uuid>,
    Json(msg): Json<JobResultMessage>,
) -> Result<Json<JobResultAck>, ApiError> {
    if msg.receipt.receipt.job_id != job_id {
        return Err(ApiError::BadRequest(
            "path job_id does not match the receipt's job_id".into(),
        ));
    }
    msg.receipt
        .verify()
        .map_err(|e| ApiError::BadRequest(format!("receipt does not verify: {e}")))?;

    let record = state
        .jobs()
        .get(job_id)
        .ok_or_else(|| ApiError::NotFound(format!("no such job {job_id}")))?;
    if msg.receipt.receipt.operator.pubkey_base58() != record.operator_pubkey_b58 {
        return Err(ApiError::BadRequest(
            "receipt operator does not match this job's assigned operator".into(),
        ));
    }
    // An idempotent replay of a job that already concluded: its receipt
    // is on file and its fund verdict is final, so echo that verdict
    // rather than the 409 a redelivering node would have to drop. This
    // is how a node crediting after a lost ack — a client-timeout retry,
    // an outbox redelivery, a crash between the release and the local
    // credit — finishes booking earnings for money already released,
    // instead of silently losing the row. Gated on a receipt already
    // recorded, so the receipt-less `Completed` crash-recovery fill-in
    // below still runs; reached only after the assigned-operator check,
    // so a stranger's replay can never read a verdict out of it; and
    // returning before any mutation, so a replay never re-pays or
    // re-audits (the same idempotent-ack posture the buyer envelope
    // replay already takes at `/federation/jobs`).
    if record.receipt.is_some() {
        match record.phase {
            JobPhase::Completed => {
                return Ok(Json(JobResultAck {
                    job_id,
                    settled: ResultSettlement::Released,
                    released_gross_micro_usdc: record.released_gross_micro_usdc(),
                }));
            }
            JobPhase::Failed | JobPhase::Refunded => {
                return Ok(Json(JobResultAck {
                    job_id,
                    settled: ResultSettlement::Refunded,
                    released_gross_micro_usdc: 0,
                }));
            }
            // Already parked: a redelivery must not order a second check.
            JobPhase::AwaitingCheck => {
                return Ok(Json(JobResultAck {
                    job_id,
                    settled: ResultSettlement::AwaitingCheck,
                    released_gross_micro_usdc: 0,
                }));
            }
            // Handed back for a rework: a late redelivery of the build that
            // failed is not the rework's result.
            JobPhase::Offered | JobPhase::Accepted
                if record.rework.is_some()
                    && record.receipt.as_ref().map(|r| &r.receipt.result_hash_hex)
                        == Some(&msg.receipt.receipt.result_hash_hex) =>
            {
                return Ok(Json(JobResultAck {
                    job_id,
                    settled: ResultSettlement::AwaitingCheck,
                    released_gross_micro_usdc: 0,
                }));
            }
            JobPhase::Offered | JobPhase::Accepted | JobPhase::Rejected => {}
        }
    }
    // The receipt signs the output's hash, not the output. Refuse to
    // release escrow for bytes the buyer couldn't verify against the
    // signed receipt — otherwise the coordinator would pay the operator
    // while serving the buyer an unverifiable (or substituted) result.
    if output_hash_hex(&msg.output) != msg.receipt.receipt.result_hash_hex {
        return Err(ApiError::BadRequest(
            "output does not hash to the receipt's result_hash_hex".into(),
        ));
    }

    // The deadline is the buyer's SLA and the refund trigger
    // (`crate::sweep`). A result for a job already past it must not
    // pay, however it arrived before the next sweep tick — otherwise
    // whether the operator is paid or the buyer refunded turns on the
    // sweep's 10s cadence rather than the deadline itself. Settle it the
    // way the sweep would, a DeadlineExpired refund attributed to the
    // operator, so the outcome is deterministic; the late receipt is
    // kept as evidence that work was delivered, only too late to count.
    // `AlreadySettled` means the sweep already reached the same verdict.
    let now_ms = crate::epoch_ms();
    if record
        .envelope
        .payload
        .issued_at_ms
        .saturating_add(record.envelope.payload.deadline_ms)
        < now_ms
    {
        let refund = state
            .escrow()
            .refund(job_id, RefundReason::DeadlineExpired)
            .await;
        // `AlreadySettled` is ambiguous: the deadline sweep already refunded
        // this hold, or an assigned operator's `Ok` result RELEASED it inside
        // the window and the record write died before it recorded — the crash
        // window the settlement refill below heals. A late redelivery of that
        // paid result must recover the payout, not refund a job the buyer was
        // already charged for, so consult the escrow the way `cancel_job`
        // does. A released hold falls through to the settlement path; anything
        // else settles the way the sweep would, a DeadlineExpired refund
        // attributed to the operator.
        let released_in_window = matches!(refund, Err(EscrowError::AlreadySettled(_)))
            && matches!(
                state.escrow().status(job_id).await,
                Ok(EscrowStatus::Released)
            );
        if !released_in_window {
            match refund {
                Ok(()) => {
                    state
                        .jobs()
                        .set_receipt_and_phase(
                            job_id,
                            msg.receipt,
                            msg.output,
                            ReleaseCharges::default(),
                            JobPhase::Refunded,
                            Some(RefundReason::DeadlineExpired),
                            crate::jobs::ReceiptAssignment {
                                operator_pubkey_b58: record.operator_pubkey_b58.clone(),
                                payout_address: record.payout_address.clone(),
                                metered_elapsed_ms: None,
                            },
                        )
                        .map_err(|e| ApiError::Internal(e.to_string()))?;
                    state
                        .record_audit(AuditKind::ComputeJobRefunded {
                            job_id,
                            reason: RefundReason::DeadlineExpired.as_str().into(),
                            operator_pubkey_b58: Some(record.operator_pubkey_b58.clone()),
                        })
                        .await;
                }
                Err(EscrowError::AlreadySettled(_)) => {}
                Err(e) => return Err(ApiError::Conflict(e.to_string())),
            }
            // The buyer got the whole window back off-chain, so the on-chain
            // charge has to become zero too: void the vault or an operator
            // could still settle it for the seconds it metered before the
            // deadline. A no-op unless a chain meter is running.
            crate::onchain_meter::void_lease_onchain(&state, &record).await;
            return Ok(Json(JobResultAck {
                job_id,
                settled: ResultSettlement::Refunded,
                released_gross_micro_usdc: 0,
            }));
        }
    }

    // An agent task's verified result is held for another operator's check
    // rather than released on its own receipt.
    if record.envelope.payload.kind == JobKind::AgentTask
        && msg.receipt.receipt.status == A2ATaskStatus::Ok
    {
        let settled = crate::agent::park_result(&state, job_id, &record, msg.receipt, msg.output)
            .await
            .map_err(ApiError::Conflict)?;
        return Ok(Json(JobResultAck {
            job_id,
            settled,
            released_gross_micro_usdc: 0,
        }));
    }

    // A lease's signed terms drive its metered settlement below. Read
    // once here, from the buyer's own envelope: a lease that somehow
    // carries no terms (impossible past `verify`, but this is the fund
    // path) settles like any other job rather than guessing a rate.
    let lease_terms = match record.envelope.payload.kind {
        JobKind::LeaseSession => {
            covenant_compute_protocol::parse_lease_terms(&record.envelope.payload.input)
                .map_err(|e| ApiError::Internal(format!("lease terms unreadable: {e}")))?
        }
        _ => None,
    };

    // Only an `Ok` receipt pays. A verified non-Ok receipt is the
    // operator's own signed statement that the job ran and failed: the
    // buyer's hold goes back, the receipt is kept as evidence, and the
    // operator earns nothing — anything else makes instant-failure a
    // paid strategy. `Partial` refunds too until metering gives a
    // partial job a price.
    if msg.receipt.receipt.status != A2ATaskStatus::Ok {
        let refund = state
            .escrow()
            .refund(job_id, RefundReason::ExecutionFailed)
            .await;
        // `AlreadySettled` is ambiguous here too: a retry after an earlier
        // attempt already returned the hold, or a prior `Ok` result already
        // RELEASED it (a crash-window redelivery, or a non-conforming
        // operator that paid then failed the same job). Consult the escrow
        // rather than reporting a refund on a hold the operator was paid
        // from: a released hold falls through to the settlement path, where
        // `release` refuses this non-Ok receipt as unpayable — a 409, never
        // a phantom refund. A truly refunded hold falls through to the void
        // rather than erroring out, so a phase-write failure between an
        // earlier refund and its void cannot strand a funded vault the
        // operator could still settle. Otherwise this is the operator's own
        // signed failure: the hold goes back and it earns nothing, or
        // instant failure becomes a paid strategy.
        let released_in_window = matches!(refund, Err(EscrowError::AlreadySettled(_)))
            && matches!(
                state.escrow().status(job_id).await,
                Ok(EscrowStatus::Released)
            );
        if !released_in_window {
            match refund {
                Ok(()) => {
                    state
                        .jobs()
                        .set_receipt_and_phase(
                            job_id,
                            msg.receipt,
                            msg.output,
                            ReleaseCharges::default(),
                            JobPhase::Failed,
                            Some(RefundReason::ExecutionFailed),
                            crate::jobs::ReceiptAssignment {
                                operator_pubkey_b58: record.operator_pubkey_b58.clone(),
                                payout_address: record.payout_address.clone(),
                                metered_elapsed_ms: None,
                            },
                        )
                        .map_err(|e| ApiError::Internal(e.to_string()))?;
                    state
                        .record_audit(AuditKind::ComputeJobRefunded {
                            job_id,
                            reason: RefundReason::ExecutionFailed.as_str().into(),
                            operator_pubkey_b58: Some(record.operator_pubkey_b58.clone()),
                        })
                        .await;
                }
                Err(EscrowError::AlreadySettled(_)) => {}
                Err(e) => return Err(ApiError::Conflict(e.to_string())),
            }
            // A failed receipt earns nothing, so the on-chain vault must go
            // back to the renter whole rather than pay the operator for the
            // seconds it ran before failing. A no-op unless a chain meter is
            // running.
            crate::onchain_meter::void_lease_onchain(&state, &record).await;
            return Ok(Json(JobResultAck {
                job_id,
                settled: ResultSettlement::Refunded,
                released_gross_micro_usdc: 0,
            }));
        }
    }

    // `AlreadySettled` on a Released hold whose record never got its
    // receipt is the crash window's second half: the fund flip was
    // journaled, the record write died — with the process (healed to a
    // receipt-less `Completed` by `crate::recover` at boot) or with a
    // dropped connection mid-handler (record still in-flight). The fund
    // verdict is final, and this is the assigned operator's verified
    // receipt over hash-checked output — the only receipt that can ever
    // exist for the job — so fill the record in and push the payout it
    // names; `sweep_unpaid` reports a receipt-less completed job every
    // tick but can never retry it. A receipt the record already carries
    // stays refused: that is a plain replay, not recovery.
    // A lease is metered, not paid whole: the buyer escrowed the
    // window's ceiling, and what settles is the seconds the session
    // actually ran on the coordinator's own clock (accept → this
    // result). The operator's receipt says the work happened; it never
    // sizes the bill. A lease that concluded without an accept has no
    // meter to read and bills nothing.
    let metered_elapsed_ms = match record.envelope.payload.kind {
        JobKind::LeaseSession => Some(
            record
                .accepted_at_ms
                .map(|started| now_ms.saturating_sub(started))
                .unwrap_or(0),
        ),
        _ => None,
    };
    let metered_used = match (metered_elapsed_ms, lease_terms.as_ref()) {
        (Some(elapsed), Some(terms)) => Some(terms.metered_micro_usdc(elapsed)),
        _ => None,
    };
    // A meter that read zero is a session the coordinator never saw
    // run: nothing is owed, so the whole escrowed window goes back and
    // no payout is pushed. Settling this as a release would pay an
    // operator for an unobserved session out of the buyer's ceiling.
    if metered_used == Some(0) {
        // Nothing ran, so nothing is owed on either book: the on-chain
        // settle returns the whole vault to the renter, exactly what
        // `NoMeteredUsage` means off-chain.
        crate::onchain_meter::conclude_lease_onchain(&state, &record, now_ms, 0, None).await;
        // Conclude the record before refunding the hold, the record-first order
        // `cancel_job` uses for the other no-fault refund. A crash between the
        // two then lands in the boot-reconcile branch that reads the record's
        // own `NoMeteredUsage` reason and leaves the refund unattributed, not
        // the in-flight branch that has no reason to read and would fault the
        // operator for a session it served honestly.
        state
            .jobs()
            .set_receipt_and_phase(
                job_id,
                msg.receipt,
                msg.output,
                ReleaseCharges::default(),
                JobPhase::Refunded,
                Some(RefundReason::NoMeteredUsage),
                crate::jobs::ReceiptAssignment {
                    operator_pubkey_b58: record.operator_pubkey_b58.clone(),
                    payout_address: record.payout_address.clone(),
                    metered_elapsed_ms,
                },
            )
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        // The record is concluded; the refund is what the hold owes back. A
        // transient failure here is completed by the boot reconcile, so warn
        // and let the record stand rather than erroring the operator's
        // delivery (`AlreadySettled` is a redelivery whose first attempt
        // already refunded).
        if let Err(e) = state
            .escrow()
            .refund(job_id, RefundReason::NoMeteredUsage)
            .await
        {
            if !matches!(e, EscrowError::AlreadySettled(_)) {
                tracing::warn!(%job_id, error = %e, "zero-meter lease concluded; hold refund deferred to reconcile");
            }
        }
        // A zero meter is nobody's failure: the coordinator never observed
        // the session run (a lost accept, or an accept and close inside one
        // millisecond), so the whole window refunds and the operator — which
        // did submit a verified receipt — is not faulted. Attributing this
        // refund would make it a reputation fault, since `reputation::classify`
        // faults every operator-attributed refund; `NoMeteredUsage`, unlike the
        // fault refunds it enumerates, is not one. Left unattributed, as
        // `buyer_cancelled` already is.
        state
            .record_audit(AuditKind::ComputeJobRefunded {
                job_id,
                reason: RefundReason::NoMeteredUsage.as_str().into(),
                operator_pubkey_b58: None,
            })
            .await;
        return Ok(Json(JobResultAck {
            job_id,
            settled: ResultSettlement::Refunded,
            released_gross_micro_usdc: 0,
        }));
    }
    let settlement = match metered_used {
        Some(used) => {
            state
                .escrow()
                .release_metered(job_id, &msg.receipt, used)
                .await
        }
        None => state.escrow().release(job_id, &msg.receipt).await,
    };
    if let Err(e) = settlement {
        let refill = matches!(e, EscrowError::AlreadySettled(_))
            && record.receipt.is_none()
            && matches!(
                state.escrow().status(job_id).await,
                Ok(EscrowStatus::Released)
            );
        if !refill {
            return Err(ApiError::Conflict(e.to_string()));
        }
    }

    let (amount, funding_source) = state.escrow().hold_info(job_id).unwrap_or((
        msg.receipt.receipt.price_micro_usdc,
        state.config().default_funding_source,
    ));
    // A crash-recovery redelivery (the refill path above) re-meters against a
    // clock later than the settlement it heals, so this fresh elapsed bills a
    // lease that ran less than the recovery gap for the whole gap. The escrow
    // already settled the true figure; pin the elapsed that reproduces it, so
    // the record's meter matches the charge — keeping the buyer's lease view
    // and the public settlement proof's gross at fee+net — rather than the
    // over-measurement `now_ms` would stamp. The normal path settled `amount`
    // from this very meter, so it agrees and is left as measured.
    let metered_elapsed_ms = match (metered_elapsed_ms, lease_terms.as_ref()) {
        (Some(_), Some(terms)) if metered_used != Some(amount) => {
            Some(terms.elapsed_ms_for(amount))
        }
        _ => metered_elapsed_ms,
    };
    // Commit the meter and settle the vault once the hold is released, so
    // the money decision below knows whether the operator has already been
    // paid on-chain. After the release, not before: a released hold can no
    // longer be refunded by the deadline sweep, so the vault never pays for
    // a job the books refunded. The elapsed is the figure pinned onto the
    // record as the charge's explanation — the two meters must never
    // disagree about what was billed.
    let onchain = match metered_elapsed_ms {
        Some(elapsed) => {
            crate::onchain_meter::conclude_lease_onchain(
                &state,
                &record,
                now_ms,
                elapsed,
                Some(&msg.receipt.payout_memo()),
            )
            .await
        }
        None => crate::onchain_meter::LeaseConclusion::OffChain,
    };
    let ack = finish_release(
        &state,
        job_id,
        &record,
        msg.receipt,
        msg.output,
        amount,
        0,
        funding_source,
        metered_elapsed_ms,
        onchain,
    )
    .await?;
    crate::agent::nudge(&state, &record);
    Ok(ack)
}

/// Concludes a released job: the fee and partner shares, the `Completed`
/// record, its audit rows, and the operator's payout — on-chain when a
/// lease's vault already paid it, pushed otherwise. Shared by a result that
/// releases on its own receipt and an agent task released by its check.
///
/// `retained_micro_usdc` is a part of the release the protocol keeps
/// before the marketplace fee is taken: what an agent task's buyer paid for
/// the checks the protocol had already paid its checkers for. It is booked
/// with the fee, so the operator's net and the books both leave it out,
/// and no partner earns a share of it.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn finish_release(
    state: &CoordinatorState,
    job_id: Uuid,
    record: &JobRecord,
    receipt: covenant_compute_protocol::SignedWorkReceipt,
    output: Vec<covenant_mcp::Content>,
    amount: u64,
    retained_micro_usdc: u64,
    funding_source: FundingSource,
    metered_elapsed_ms: Option<u64>,
    onchain: crate::onchain_meter::LeaseConclusion,
) -> Result<Json<JobResultAck>, ApiError> {
    // The marketplace take (C7): the hold releases gross — the buyer
    // paid the envelope price and the subsidy/revenue books stay in
    // gross terms — and the split happens here, on the payout push.
    // The fee floors (protocol-shared math), so the operator's net is
    // the ceiling; the rate was disclosed to the operator at
    // registration, never sprung at settlement.
    let retained = retained_micro_usdc.min(amount);
    let fee = state.config().fee.take_of(amount - retained);
    let operator_net = amount - retained - fee;
    // Partner rev-share (C8): carved out of the fee just computed —
    // the operator's net and the buyer's charge are untouched. A code
    // with no configured partner accrues nothing. The supply side's
    // share comes off the whole fee; the buyer's partner earns off the
    // remainder, so the two can never sum past the fee.
    let partner = record
        .referral_code
        .as_ref()
        .and_then(|code| state.config().partners.get(code).map(|p| (code, p)))
        .map(|(code, p)| {
            (
                code.clone(),
                p.payout_address().to_string(),
                p.share_of(fee),
            )
        })
        .filter(|(_, _, share)| *share > 0);
    let partner_share = partner.as_ref().map(|(_, _, share)| *share).unwrap_or(0);
    let buyer_partner = record
        .buyer_referral_code
        .as_ref()
        .and_then(|code| state.config().partners.get(code).map(|p| (code, p)))
        .map(|(code, p)| {
            (
                code.clone(),
                p.payout_address().to_string(),
                p.share_of(fee).min(fee - partner_share),
            )
        })
        .filter(|(_, _, share)| *share > 0);
    let buyer_partner_share = buyer_partner
        .as_ref()
        .map(|(_, _, share)| *share)
        .unwrap_or(0);

    state
        .jobs()
        .set_receipt_and_phase(
            job_id,
            receipt.clone(),
            output,
            ReleaseCharges {
                fee_micro_usdc: fee + retained,
                partner_share_micro_usdc: partner_share,
                buyer_partner_share_micro_usdc: buyer_partner_share,
            },
            JobPhase::Completed,
            None,
            // Pin the conclusion to the operator the receipt verified
            // against: a stale-offer reassign racing this handler must
            // not leave another operator's name — and payout address —
            // on a job this one completed (the sweep pays whoever the
            // durable record names).
            crate::jobs::ReceiptAssignment {
                operator_pubkey_b58: record.operator_pubkey_b58.clone(),
                payout_address: record.payout_address.clone(),
                metered_elapsed_ms,
            },
        )
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // A record that was already `Completed` got its release row from
    // the boot reconcile that concluded it — this request only filled
    // the receipt in. One release, one row.
    if record.phase != JobPhase::Completed {
        state
            .record_audit(AuditKind::ComputeJobReleased {
                job_id,
                operator_pubkey_b58: record.operator_pubkey_b58.clone(),
                amount_micro_usdc: amount,
                funding_source: funding_source_str(funding_source).into(),
            })
            .await;
    }
    if fee > 0 {
        state
            .record_audit(AuditKind::ComputeFeeCaptured {
                job_id,
                operator_pubkey_b58: record.operator_pubkey_b58.clone(),
                fee_bps: state.config().fee.bps(),
                fee_micro_usdc: fee,
                operator_net_micro_usdc: operator_net,
            })
            .await;
    }
    if let Some((referral_code, partner_payout_address, share_micro_usdc)) = partner {
        state
            .record_audit(AuditKind::ComputePartnerShareAccrued {
                job_id,
                referral_code,
                partner_payout_address,
                share_micro_usdc,
                fee_micro_usdc: fee,
            })
            .await;
    }
    if let Some((referral_code, partner_payout_address, share_micro_usdc)) = buyer_partner {
        state
            .record_audit(AuditKind::ComputeBuyerPartnerShareAccrued {
                job_id,
                referral_code,
                partner_payout_address,
                share_micro_usdc,
                fee_micro_usdc: fee,
            })
            .await;
    }

    // A lease that settled on-chain has already paid the operator out
    // of its vault. Pushing again would pay twice, so the settle
    // transaction becomes the job's payout of record — which also keeps
    // the job out of `completed_unpaid`, whose retry sweep would
    // otherwise re-push it every tick. A meter that moved no money
    // reports no transaction and leaves the off-chain push in charge.
    // One whose settle may or may not have landed holds the payout
    // instead: the vault could already have paid.
    match onchain {
        crate::onchain_meter::LeaseConclusion::Settled(settlement)
            if settlement.tx_signature.is_some() =>
        {
            if let Err(e) = state.jobs().set_payout(
                job_id,
                crate::jobs::PayoutOutcome {
                    amount_micro_usdc: settlement.charged_micro_usdc,
                    tx_signature: settlement.tx_signature.clone(),
                    recorded_at_ms: crate::epoch_ms(),
                },
            ) {
                tracing::error!(%job_id, error = %e, "lease settled on-chain but recording it failed");
            }
            return Ok(Json(JobResultAck {
                job_id,
                settled: ResultSettlement::Released,
                released_gross_micro_usdc: amount,
            }));
        }
        crate::onchain_meter::LeaseConclusion::Unresolved {
            message,
            tx_signature,
        } => {
            crate::onchain_meter::hold_payout_for_chain(
                state,
                job_id,
                &record.payout_address,
                operator_net,
                &receipt,
                &message,
                tx_signature.as_deref(),
            );
            return Ok(Json(JobResultAck {
                job_id,
                settled: ResultSettlement::Released,
                released_gross_micro_usdc: amount,
            }));
        }
        _ => {}
    }

    // push_job_payout brackets the transfer in a durable attempt and
    // records a landed push itself — the same single path the retry
    // sweep uses, so the two can never record differently.
    if let Err(e) = state
        .push_job_payout(
            job_id,
            &record.operator_pubkey_b58,
            &record.payout_address,
            operator_net,
            &receipt,
        )
        .await
    {
        // Escrow is already released and the receipt is durably
        // recorded; a payout-push failure is an operational incident
        // to alert on, not a reason to unwind a mechanical release.
        // The job record keeps `payout: None` — the operator books
        // show exactly this job as released-but-unpaid, and the
        // payout-retry sweep re-pushes it (unless the outcome is
        // unknown, in which case the attempt stays open and nothing
        // moves until it's reconciled).
        tracing::error!(%job_id, error = %e, "payout push failed after escrow release");
    }

    Ok(Json(JobResultAck {
        job_id,
        settled: ResultSettlement::Released,
        released_gross_micro_usdc: amount,
    }))
}
/// The assigned operator's chunk relay for a streaming job. Session-
/// authed like `accept_job`: chunks predate the receipt, so the bearer
/// session is what ties the push to the operator the job was matched
/// to. Chunks for a job that already concluded are acknowledged and
/// dropped — the final batch can race the result submission, and the
/// record's verified output is complete without them.
async fn push_stream(
    State(state): State<CoordinatorState>,
    Path(job_id): Path<Uuid>,
    headers: HeaderMap,
    Json(push): Json<StreamPush>,
) -> Result<StatusCode, ApiError> {
    if push.job_id != job_id {
        return Err(ApiError::BadRequest(
            "path job_id does not match the push's job_id".into(),
        ));
    }
    let record = state
        .jobs()
        .get(job_id)
        .ok_or_else(|| ApiError::NotFound(format!("no such job {job_id}")))?;
    check_session(&state, &record.operator_pubkey_b58, &headers)?;
    if !record.envelope.payload.stream {
        return Err(ApiError::BadRequest(
            "the buyer did not ask for streaming on this job".into(),
        ));
    }
    match record.phase {
        JobPhase::Accepted => {}
        JobPhase::Completed | JobPhase::Failed => return Ok(StatusCode::OK),
        phase => {
            return Err(ApiError::Conflict(format!(
                "job is {}; chunks are accepted only while it runs",
                phase.as_str()
            )))
        }
    }
    // A lease session's first chunk is its access grant. Pin it to the
    // record as it passes: the stream is a live relay a buyer can miss
    // (a dropped poll, a restarted client), and the address of a
    // machine they are being billed for must not be missable.
    if record.envelope.payload.kind == JobKind::LeaseSession {
        for chunk in &push.chunks {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&chunk.text) else {
                continue;
            };
            match covenant_compute_protocol::parse_lease_access(&[covenant_mcp::Content::json(
                value,
            )]) {
                Ok(Some(access)) if access.job_id == job_id => {
                    if let Err(e) = state.jobs().set_lease_access(job_id, access) {
                        tracing::error!(%job_id, error = %e, "could not pin a lease access grant");
                    }
                    break;
                }
                // A grant naming another job is a relay bug or a
                // hostile node; never let it point a buyer elsewhere.
                Ok(Some(other)) => tracing::warn!(
                    %job_id,
                    named = %other.job_id,
                    "ignoring a lease access grant that names a different job"
                ),
                Ok(None) => {}
                Err(e) => tracing::warn!(%job_id, error = %e, "malformed lease access grant"),
            }
        }
    }
    state
        .streams()
        .append(job_id, &push.chunks, push.done, crate::epoch_ms())
        .map_err(|e| ApiError::Conflict(e.to_string()))?;
    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
struct StreamQuery {
    #[serde(default)]
    since: u64,
}

#[derive(Serialize)]
struct StreamView {
    job_id: Uuid,
    /// The job's phase, so a poller knows when to stop: `done` only
    /// says the executor finished producing text, while settlement —
    /// and the verified final output — arrive with the receipt.
    status: &'static str,
    chunks: Vec<StreamChunk>,
    next_seq: u64,
    done: bool,
    truncated: bool,
}

/// A streaming job's chunks from `?since=` on. Chunk text is job
/// output, so the read is gated exactly like the receipt poll: only
/// the buyer who signed the envelope, by signed read over the
/// canonical path (the cursor selects a suffix of data the same key
/// could read whole, so it stays out of the signature).
async fn job_stream(
    State(state): State<CoordinatorState>,
    Path(job_id): Path<Uuid>,
    Query(query): Query<StreamQuery>,
    headers: HeaderMap,
) -> Result<Json<StreamView>, ApiError> {
    let record = state
        .jobs()
        .get(job_id)
        .ok_or_else(|| ApiError::NotFound(format!("no such job {job_id}")))?;
    verify_signed_read(
        &headers,
        &record.envelope.payload.buyer.pubkey_base58(),
        &format!("/federation/jobs/{job_id}/stream"),
    )?;
    let readout = state.streams().read_from(job_id, query.since);
    Ok(Json(StreamView {
        job_id,
        status: record.phase.as_str(),
        chunks: readout.chunks,
        next_seq: readout.next_seq,
        done: readout.done,
        truncated: readout.truncated,
    }))
}

#[derive(Serialize)]
struct JobStatusView {
    job_id: Uuid,
    status: &'static str,
    disputed: bool,
    /// Why the money went back, for the terminal unpaid statuses —
    /// `deadline_expired` and `buyer_cancelled` are both `refunded`,
    /// and a buyer deciding whether to retry, re-price, or walk away
    /// deserves to know which happened. `None` while the job lives
    /// and on jobs that paid out.
    refund_reason: Option<RefundReason>,
    receipt: Option<covenant_compute_protocol::SignedWorkReceipt>,
    output: Option<Vec<covenant_mcp::Content>>,
    /// The payout push that honored this receipt, once it landed —
    /// the buyer's pointer from "work I paid for" to the chain.
    payout: Option<JobPayoutView>,
    /// What the settled hold actually charged the buyer: the whole price for
    /// a fixed job, the metered draw for a lease, zero once refunded. `None`
    /// while the job is still in flight. Sized from the escrow, the money
    /// authority, so a lease reports the seconds it ran rather than the window
    /// ceiling the receipt's `price_micro_usdc` names.
    charged_micro_usdc: Option<u64>,
    /// An agent task's latest check verdict, once a check has returned one.
    #[serde(skip_serializing_if = "Option::is_none")]
    check: Option<covenant_compute_protocol::AgentCheckVerdict>,
    /// What the chain counted when the task's votes went through a round.
    #[serde(skip_serializing_if = "Option::is_none")]
    round: Option<crate::rounds::RoundRecord>,
    /// How many times a failed check handed an agent task back to its
    /// builder.
    #[serde(skip_serializing_if = "Option::is_none")]
    reworks: Option<u32>,
    /// An agent task's part in a fix order.
    #[serde(skip_serializing_if = "Option::is_none")]
    order: Option<OrderView>,
}

/// A fix order as its buyer follows it: the reproduction names its fix once
/// one is posted, the fix names its reproduction.
#[derive(Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
enum OrderView {
    Reproduced { fix: Option<Uuid> },
    Fix { reproduction: Uuid },
}

#[derive(Serialize)]
struct JobPayoutView {
    amount_micro_usdc: u64,
    /// `None` when the backend records intent without touching a
    /// chain (MockPayout), or until the transfer confirms.
    tx_signature: Option<String>,
    /// The SPL memo stamped on that transaction — recomputable from
    /// the signed receipt alone, echoed here so a reader can check
    /// the chain without knowing the derivation.
    memo: String,
    recorded_at_ms: u64,
}

impl JobPayoutView {
    fn for_record(record: &crate::jobs::JobRecord) -> Option<Self> {
        match (&record.payout, &record.receipt) {
            (Some(paid), Some(receipt)) => Some(Self {
                amount_micro_usdc: paid.amount_micro_usdc,
                tx_signature: paid.tx_signature.clone(),
                memo: receipt.payout_memo(),
                recorded_at_ms: paid.recorded_at_ms,
            }),
            _ => None,
        }
    }
}

/// The receipt poll returns the job's OUTPUT, so holding the job id —
/// unguessable, but shared with every log line that mentions the job —
/// must not be enough to read it. Only the buyer who signed the
/// envelope gets the read; the 404 for an unknown id stays ahead of
/// auth because there is no record to verify against (and the sweep
/// semantics depend on it).
/// A buyer hands over the hidden checks its agent task committed to. The
/// digest is the credential; see [`crate::agent::accept_hidden_checks`].
async fn hidden_checks(
    State(state): State<CoordinatorState>,
    Path(job_id): Path<Uuid>,
    Json(hidden): Json<covenant_compute_protocol::HiddenChecks>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::agent::accept_hidden_checks(&state, job_id, hidden)
        .await
        .map_err(ApiError::BadRequest)?;
    Ok(Json(
        serde_json::json!({ "job_id": job_id, "accepted": true }),
    ))
}

async fn job_status(
    State(state): State<CoordinatorState>,
    Path(job_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<JobStatusView>, ApiError> {
    let record = state
        .jobs()
        .get(job_id)
        .ok_or_else(|| ApiError::NotFound(format!("no such job {job_id}")))?;
    verify_signed_read(
        &headers,
        &record.envelope.payload.buyer.pubkey_base58(),
        &format!("/federation/jobs/{job_id}/receipt"),
    )?;
    let payout = JobPayoutView::for_record(&record);
    let charged_micro_usdc = state.escrow().settled_charge_micro_usdc(job_id);
    // An agent task's patch is the work itself: the buyer receives it once
    // it is paid for, never while it is being checked or after it failed.
    // The check's verdict is shown either way, so a failure says why.
    let agent_task = record.envelope.payload.kind == JobKind::AgentTask;
    let check = agent_task
        .then(|| crate::agent::latest_verdict(&state, &record))
        .flatten();
    let output = if agent_task && record.phase != JobPhase::Completed {
        None
    } else {
        record.output
    };
    Ok(Json(JobStatusView {
        job_id,
        status: record.phase.as_str(),
        disputed: record.dispute.is_some(),
        refund_reason: record.refund_reason,
        receipt: record.receipt,
        output,
        payout,
        charged_micro_usdc,
        check,
        round: record.vote_round,
        reworks: record.rework.map(|r| r.count),
        order: record.order.map(|order| match order {
            crate::jobs::TaskOrder::Reproduced { fix } => OrderView::Reproduced { fix },
            crate::jobs::TaskOrder::Fix { reproduction, .. } => OrderView::Fix { reproduction },
        }),
    }))
}

#[derive(Serialize)]
struct DisputeView {
    job_id: Uuid,
    operator_pubkey_b58: String,
    disputed: bool,
}

/// Records the buyer's signed dispute of a completed job (C4). No
/// money moves: the escrow released against a verified receipt, and a
/// clawback on an unverifiable content judgment would make
/// instant-dispute a free-work strategy. What lands is durable — the
/// signed dispute on the journaled job record, an audit row, and a
/// reputation fault (with its own `disputed` counter, since a dispute
/// is buyer-attested where refund/canary faults are
/// coordinator-attested). One dispute per job, only by the buyer who
/// signed the envelope, only while the window is open.
async fn dispute_job(
    State(state): State<CoordinatorState>,
    Path(job_id): Path<Uuid>,
    Json(req): Json<DisputeRequest>,
) -> Result<Json<DisputeView>, ApiError> {
    if req.job_id != job_id {
        return Err(ApiError::BadRequest(
            "path job_id does not match the dispute's job_id".into(),
        ));
    }
    req.verify()
        .map_err(|e| ApiError::BadRequest(format!("dispute does not verify: {e}")))?;
    let now_ms = crate::epoch_ms();
    if now_ms.abs_diff(req.disputed_at_ms) > DISPUTE_MAX_SKEW_MS {
        return Err(ApiError::Unauthorized(format!(
            "dispute disputed_at {} is outside the {DISPUTE_MAX_SKEW_MS}ms window around {now_ms}",
            req.disputed_at_ms
        )));
    }

    let record = state
        .jobs()
        .get(job_id)
        .ok_or_else(|| ApiError::NotFound(format!("no such job {job_id}")))?;
    if req.buyer.pubkey_base58() != record.envelope.payload.buyer.pubkey_base58() {
        return Err(ApiError::Unauthorized(
            "only the buyer who signed this job's envelope may dispute it".into(),
        ));
    }
    match record.phase {
        JobPhase::Completed => {}
        JobPhase::Offered | JobPhase::Accepted | JobPhase::AwaitingCheck => {
            return Err(ApiError::Conflict(format!(
                "job {job_id} has not concluded yet — dispute what you were charged for"
            )));
        }
        JobPhase::Failed | JobPhase::Refunded | JobPhase::Rejected => {
            return Err(ApiError::Conflict(format!(
                "job {job_id} was already refunded — there is nothing to dispute"
            )));
        }
    }
    // The window anchors on the coordinator's own clock at release; the
    // envelope's buyer-signed issued_at + deadline covers records from
    // before the anchor existed. Never the receipt's executed_at_ms —
    // that is the operator's claim, and a pre-dated receipt must not
    // shrink the window.
    let concluded_at_ms = record.concluded_at_ms.unwrap_or_else(|| {
        record
            .envelope
            .payload
            .issued_at_ms
            .saturating_add(record.envelope.payload.deadline_ms)
    });
    let window_ms = state.config().dispute_window.as_millis() as u64;
    // The window is half-open: a dispute is in time strictly before the
    // deadline. `>=` is what makes a zero-length window mean "disputes
    // are off" — under `>`, a dispute landing in the same millisecond
    // as the conclusion slipped through, so whether a deployment that
    // disabled disputes actually recorded one turned on clock
    // granularity.
    if now_ms >= concluded_at_ms.saturating_add(window_ms) {
        return Err(ApiError::Conflict(format!(
            "the dispute window for job {job_id} has closed ({window_ms}ms after conclusion)"
        )));
    }

    let reason = req.reason.clone();
    state.jobs().set_dispute(job_id, req).map_err(|e| match e {
        JobError::AlreadyDisputed(_) => ApiError::Conflict(e.to_string()),
        JobError::NotFound(_) => ApiError::NotFound(e.to_string()),
        JobError::Journal(_) => ApiError::Internal(e.to_string()),
    })?;
    state
        .record_audit(AuditKind::ComputeJobDisputed {
            job_id,
            operator_pubkey_b58: record.operator_pubkey_b58.clone(),
            buyer_pubkey_b58: record.envelope.payload.buyer.pubkey_base58(),
            reason: reason.clone(),
        })
        .await;
    tracing::info!(%job_id, operator = %record.operator_pubkey_b58, reason, "job disputed by its buyer");

    Ok(Json(DisputeView {
        job_id,
        operator_pubkey_b58: record.operator_pubkey_b58,
        disputed: true,
    }))
}

/// A buyer withdraws a job no operator has accepted yet and takes the
/// refund now instead of waiting out the deadline sweep. Strictly
/// pre-commitment: acceptance makes the work the operator's to finish,
/// so a cancel is never a clawback — and the refund is attributed to
/// no one, because withdrawing an offer is nobody's fault and must not
/// land a reputation mark on the operator who happened to hold it.
async fn cancel_job(
    State(state): State<CoordinatorState>,
    Path(job_id): Path<Uuid>,
    Json(req): Json<CancelRequest>,
) -> Result<Json<CancelView>, ApiError> {
    if req.job_id != job_id {
        return Err(ApiError::BadRequest(
            "path job_id does not match the cancellation's job_id".into(),
        ));
    }
    req.verify()
        .map_err(|e| ApiError::BadRequest(format!("cancellation does not verify: {e}")))?;
    let now_ms = crate::epoch_ms();
    if now_ms.abs_diff(req.cancelled_at_ms) > CANCEL_MAX_SKEW_MS {
        return Err(ApiError::Unauthorized(format!(
            "cancel cancelled_at {} is outside the {CANCEL_MAX_SKEW_MS}ms window around {now_ms}",
            req.cancelled_at_ms
        )));
    }

    let record = state
        .jobs()
        .get(job_id)
        .ok_or_else(|| ApiError::NotFound(format!("no such job {job_id}")))?;
    if req.buyer.pubkey_base58() != record.envelope.payload.buyer.pubkey_base58() {
        return Err(ApiError::Unauthorized(
            "only the buyer who signed this job's envelope may cancel it".into(),
        ));
    }

    let refunded_view = |job_id: Uuid, amount: u64| CancelView {
        job_id,
        status: JobPhase::Refunded.as_str().into(),
        refunded_micro_usdc: amount,
    };
    let amount = record.envelope.payload.price_micro_usdc;

    // The flip is the decision point: check-and-conclude in one atomic
    // step, so a racing accept either committed first (and the cancel
    // answers 409 from the phase that won) or finds the job concluded
    // (and bounces off the accept path's own still-live guard).
    if !state
        .jobs()
        .cancel_if_offered(job_id)
        .map_err(|e| ApiError::Internal(e.to_string()))?
    {
        let phase = state
            .jobs()
            .get(job_id)
            .map(|r| r.phase)
            .ok_or_else(|| ApiError::NotFound(format!("no such job {job_id}")))?;
        return match phase {
            // An honest retry of a cancel whose answer was lost — or
            // the deadline sweep got there first. Either way the money
            // is already back; answer the fact instead of erroring.
            JobPhase::Refunded => Ok(Json(refunded_view(job_id, amount))),
            JobPhase::Accepted => Err(ApiError::Conflict(format!(
                "an operator has already accepted job {job_id} — committed work settles by \
                 result or deadline, not cancellation"
            ))),
            JobPhase::Completed => Err(ApiError::Conflict(format!(
                "job {job_id} already completed and was paid for — dispute what you were \
                 charged for instead"
            ))),
            JobPhase::AwaitingCheck => Err(ApiError::Conflict(format!(
                "the work for job {job_id} is delivered and being checked — it is paid only if \
                 the check passes, and refunded otherwise"
            ))),
            JobPhase::Rejected | JobPhase::Failed => Err(ApiError::Conflict(format!(
                "job {job_id} already concluded ({}) and its hold was already refunded — \
                 there is nothing to cancel",
                phase.as_str()
            ))),
            JobPhase::Offered => Err(ApiError::Internal(format!(
                "job {job_id} reads as offered after a refused cancel"
            ))),
        };
    }

    // The phase was `Offered`, so the hold is normally still held. Two
    // races settle it in this same instant, and `AlreadySettled` cannot tell
    // them apart: the deadline sweep refunding it (its escrow write precedes
    // its phase write) is the money-is-back outcome the cancel asked for; but
    // an assigned operator's result can also RELEASE it here — a lost-accept
    // recovery delivers without re-accepting, so a still-`Offered` job can
    // pay. Consult the escrow, as `known_job_phase` does, and answer an
    // operator-won race with the charged truth rather than a `buyer_cancelled`
    // refund the buyer was never given.
    if let Err(e) = state
        .escrow()
        .refund(job_id, RefundReason::BuyerCancelled)
        .await
    {
        if matches!(e, EscrowError::AlreadySettled(_))
            && matches!(
                state.escrow().status(job_id).await,
                Ok(EscrowStatus::Released)
            )
        {
            // The operator delivered and was paid as the cancel landed; the
            // record self-heals to `Completed` when that result concludes.
            // Report the charge, and record no refund that never happened.
            return Err(ApiError::Conflict(format!(
                "job {job_id} was delivered and paid for as the cancel arrived — \
                 dispute what you were charged for instead"
            )));
        }
        tracing::warn!(%job_id, error = %e, "cancel concluded the job but the refund was already settled");
    }
    // Pull the withdrawn offer out of the assignee's delivery queue so
    // an idle node never polls out work that no longer exists. A node
    // that already holds the offer learns at accept: 409, drop, move on.
    state.registry().revoke(&record.operator_pubkey_b58, job_id);
    state
        .record_audit(AuditKind::ComputeJobRefunded {
            job_id,
            reason: RefundReason::BuyerCancelled.as_str().into(),
            operator_pubkey_b58: None,
        })
        .await;
    tracing::info!(%job_id, "job cancelled by its buyer before acceptance");

    Ok(Json(refunded_view(job_id, amount)))
}

#[derive(Serialize)]
struct DepositView {
    buyer_pubkey_b58: String,
    deposit_id: String,
    /// False when the deposit id was already applied — an honest retry
    /// is acknowledged without a double credit.
    credited: bool,
    amount_micro_usdc: u64,
}

/// The claim itself is unauthenticated; the rail is authoritative for
/// whose deposit this is and how much it was. The claimed buyer only
/// has to match the rail's answer — claiming someone else's payment
/// gets a 400, not their money. The response carries only the deposit
/// facts (already public on-chain), never the buyer's running balance:
/// deposit ids are readable by anyone watching the chain, and a replayed
/// claim must not become a balance oracle (C9) — funds live behind the
/// signed balance read.
async fn claim_deposit(
    State(state): State<CoordinatorState>,
    Json(claim): Json<DepositClaim>,
) -> Result<Json<DepositView>, ApiError> {
    let Some(rail) = state.rail() else {
        return Err(ApiError::Conflict(
            "no inbound rail is configured; deposits cannot be verified".into(),
        ));
    };
    let verified = rail.verify_deposit(&claim).await.map_err(|e| match e {
        RailError::NotFound(_) => ApiError::NotFound(e.to_string()),
        RailError::Pending(_) => ApiError::Conflict(e.to_string()),
        RailError::Rejected(_) => ApiError::BadRequest(e.to_string()),
        RailError::Backend(_) => ApiError::Internal(e.to_string()),
    })?;
    if verified.buyer_pubkey_b58 != claim.buyer_pubkey_b58 {
        return Err(ApiError::BadRequest(format!(
            "deposit {} belongs to a different buyer than claimed",
            claim.deposit_id
        )));
    }

    let outcome = state
        .accounts()
        .credit_deposit(
            &verified.deposit_id,
            &verified.buyer_pubkey_b58,
            verified.amount_micro_usdc,
        )
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let credited = match outcome {
        DepositOutcome::Credited { .. } => {
            state
                .record_audit(AuditKind::ComputeBuyerDeposited {
                    buyer_pubkey_b58: verified.buyer_pubkey_b58.clone(),
                    amount_micro_usdc: verified.amount_micro_usdc,
                    deposit_id: verified.deposit_id.clone(),
                })
                .await;
            true
        }
        DepositOutcome::Duplicate { .. } => false,
    };

    Ok(Json(DepositView {
        buyer_pubkey_b58: verified.buyer_pubkey_b58,
        deposit_id: verified.deposit_id,
        credited,
        amount_micro_usdc: verified.amount_micro_usdc,
    }))
}

#[derive(Serialize)]
struct WithdrawalView {
    withdrawal_id: Uuid,
    buyer_pubkey_b58: String,
    recipient_address_b58: String,
    amount_micro_usdc: u64,
    requested_at_ms: u64,
    /// False while the backend transfer is still owed — the retry
    /// sweep pushes it; re-read the withdrawal list for the outcome.
    pushed: bool,
    tx_signature: Option<String>,
    /// The memo the transfer carries on-chain, re-derivable by anyone
    /// holding (buyer pubkey, withdrawal id).
    memo: String,
}

impl WithdrawalView {
    fn from_state(state: crate::accounts::WithdrawalState) -> Self {
        Self {
            memo: withdrawal_memo_for(&state.buyer_pubkey_b58, state.withdrawal_id),
            withdrawal_id: state.withdrawal_id,
            buyer_pubkey_b58: state.buyer_pubkey_b58,
            recipient_address_b58: state.recipient_address_b58,
            amount_micro_usdc: state.amount_micro_usdc,
            requested_at_ms: state.requested_at_ms,
            pushed: state.pushed.is_some(),
            tx_signature: state.pushed.and_then(|p| p.tx_signature),
        }
    }
}

/// Refuses an obligation request (a buyer withdrawal, an operator
/// unbond) whose amount could never clear the payout backend's
/// per-transfer obligation cap in one transfer — checked before any
/// debit is journaled. Taking the debit anyway would owe money the
/// sweep can only spin on, re-pushing an over-cap amount forever; the
/// party must instead request an amount the backend can actually move.
/// An unset cap (the default) admits any amount the books already
/// bound, so this is a no-op unless an operator opted into a bound.
fn check_obligation_cap(
    state: &CoordinatorState,
    what: &str,
    amount_micro_usdc: u64,
) -> Result<(), ApiError> {
    if let Some(cap) = state.payout().obligation_cap_micro_usdc() {
        if amount_micro_usdc > cap {
            return Err(ApiError::BadRequest(format!(
                "{what} of {amount_micro_usdc} micro-USDC exceeds the {cap} micro-USDC \
                 per-transfer obligation cap — request an amount at or below the cap"
            )));
        }
    }
    Ok(())
}

/// A3's money-out verb: a signed withdrawal debits the buyer's
/// available balance under the escrow's funds lock, then the payout
/// backend honors the debit with a transfer whose memo names it. The
/// request is its own authorization — the same wrap-don't-embed shape
/// as a dispute, so a relay can neither forge one nor re-point the
/// recipient. A duplicate id is acknowledged, not re-debited, and a
/// failed or crashed push stays owed: the retry sweep re-pushes until
/// the transfer lands.
async fn withdraw_balance(
    State(state): State<CoordinatorState>,
    Json(req): Json<WithdrawalRequest>,
) -> Result<Json<WithdrawalView>, ApiError> {
    req.verify()
        .map_err(|e| ApiError::BadRequest(format!("withdrawal does not verify: {e}")))?;
    let now_ms = crate::epoch_ms();
    if now_ms.abs_diff(req.requested_at_ms) > WITHDRAWAL_MAX_SKEW_MS {
        return Err(ApiError::Unauthorized(format!(
            "withdrawal requested_at {} is outside the {WITHDRAWAL_MAX_SKEW_MS}ms window \
             around {now_ms}",
            req.requested_at_ms
        )));
    }
    check_obligation_cap(&state, "withdrawal", req.amount_micro_usdc)?;

    let buyer_b58 = req.buyer.pubkey_base58();
    let outcome = state
        .escrow()
        .withdraw(
            state.accounts(),
            &buyer_b58,
            req.withdrawal_id,
            &req.recipient_address_b58,
            req.amount_micro_usdc,
        )
        .map_err(|e| match e {
            EscrowError::InsufficientFunds {
                needed_micro_usdc,
                available_micro_usdc,
                ..
            } => ApiError::PaymentRequired(format!(
                "withdrawal refused: {needed_micro_usdc} micro-USDC requested, \
                 {available_micro_usdc} available after holds and prior withdrawals"
            )),
            other => ApiError::Internal(other.to_string()),
        })?;

    let withdrawal = match outcome {
        WithdrawOutcome::Requested(withdrawal) => {
            state
                .record_audit(AuditKind::ComputeBuyerWithdrawal {
                    buyer_pubkey_b58: buyer_b58.clone(),
                    amount_micro_usdc: withdrawal.amount_micro_usdc,
                    withdrawal_id: withdrawal.withdrawal_id,
                    recipient_address_b58: withdrawal.recipient_address_b58.clone(),
                })
                .await;
            tracing::info!(
                withdrawal_id = %withdrawal.withdrawal_id,
                buyer = %buyer_b58,
                amount_micro_usdc = withdrawal.amount_micro_usdc,
                "withdrawal debited"
            );
            withdrawal
        }
        // An honest retry: nothing new was debited, but an unpushed
        // debit still gets its push attempt below instead of waiting
        // out the sweep interval.
        WithdrawOutcome::Duplicate(existing) if existing.pushed.is_none() => existing,
        WithdrawOutcome::Duplicate(existing) => {
            return Ok(Json(WithdrawalView::from_state(existing)));
        }
    };

    if let Err(e) = state.push_withdrawal(&withdrawal).await {
        tracing::warn!(
            withdrawal_id = %withdrawal.withdrawal_id,
            error = %e,
            "first-chance withdrawal push failed; the retry sweep re-pushes"
        );
    }
    let current = state
        .withdrawals()
        .get(withdrawal.withdrawal_id)
        .unwrap_or(withdrawal);
    Ok(Json(WithdrawalView::from_state(current)))
}

#[derive(Serialize)]
struct BondView {
    operator_pubkey_b58: String,
    bond_id: String,
    credited: bool,
    amount_micro_usdc: u64,
    posted_total_micro_usdc: u64,
}

/// The stake-side [`claim_deposit`]: an open endpoint carrying nothing
/// but a transaction signature and an asserted operator; the rail
/// answers whose stake the payment posts and how much, and a mismatch
/// refuses — claiming someone else's post credits them, never the
/// claimant.
async fn claim_bond(
    State(state): State<CoordinatorState>,
    Json(claim): Json<BondClaim>,
) -> Result<Json<BondView>, ApiError> {
    let Some(rail) = state.rail() else {
        return Err(ApiError::Conflict(
            "no inbound rail is configured; bond posts cannot be verified".into(),
        ));
    };
    let verified = rail.verify_bond(&claim).await.map_err(|e| match e {
        RailError::NotFound(_) => ApiError::NotFound(e.to_string()),
        RailError::Pending(_) => ApiError::Conflict(e.to_string()),
        RailError::Rejected(_) => ApiError::BadRequest(e.to_string()),
        RailError::Backend(_) => ApiError::Internal(e.to_string()),
    })?;
    if verified.operator_pubkey_b58 != claim.operator_pubkey_b58 {
        return Err(ApiError::BadRequest(format!(
            "bond {} belongs to a different operator than claimed",
            claim.bond_id
        )));
    }

    let outcome = state
        .bonds()
        .credit_post(
            &verified.bond_id,
            &verified.operator_pubkey_b58,
            verified.amount_micro_usdc,
        )
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let (credited, posted_total_micro_usdc) = match outcome {
        BondPostOutcome::Credited {
            posted_total_micro_usdc,
        } => {
            state
                .record_audit(AuditKind::ComputeBondPosted {
                    operator_pubkey_b58: verified.operator_pubkey_b58.clone(),
                    amount_micro_usdc: verified.amount_micro_usdc,
                    bond_id: verified.bond_id.clone(),
                })
                .await;
            (true, posted_total_micro_usdc)
        }
        BondPostOutcome::Duplicate {
            posted_total_micro_usdc,
        } => (false, posted_total_micro_usdc),
    };

    Ok(Json(BondView {
        operator_pubkey_b58: verified.operator_pubkey_b58,
        bond_id: verified.bond_id,
        credited,
        amount_micro_usdc: verified.amount_micro_usdc,
        posted_total_micro_usdc,
    }))
}

#[derive(Serialize)]
struct UnbondView {
    unbond_id: Uuid,
    operator_pubkey_b58: String,
    recipient_address_b58: String,
    amount_micro_usdc: u64,
    requested_at_ms: u64,
    matures_at_ms: u64,
    /// False until the matured refund transfer lands — the retry sweep
    /// pushes it once the window passes; re-read the bond feed for the
    /// outcome.
    pushed: bool,
    /// What the transfer actually moved, once pushed. Less than the
    /// requested amount when a slash landed during maturation.
    paid_micro_usdc: Option<u64>,
    tx_signature: Option<String>,
    /// The memo the refund carries on-chain, re-derivable by anyone
    /// holding (operator pubkey, unbond id).
    memo: String,
}

impl UnbondView {
    fn from_state(state: UnbondState) -> Self {
        Self {
            memo: bond_refund_memo_for(&state.operator_pubkey_b58, state.unbond_id),
            unbond_id: state.unbond_id,
            operator_pubkey_b58: state.operator_pubkey_b58,
            recipient_address_b58: state.recipient_address_b58,
            amount_micro_usdc: state.amount_micro_usdc,
            requested_at_ms: state.requested_at_ms,
            matures_at_ms: state.matures_at_ms,
            pushed: state.pushed.is_some(),
            paid_micro_usdc: state.pushed.as_ref().map(|p| p.paid_micro_usdc),
            tx_signature: state.pushed.and_then(|p| p.tx_signature),
        }
    }
}

/// The stake exit: a signed unbond request registers against the
/// operator's committed bond, then waits out the unbonding window
/// before the sweep pushes its refund. Nothing is reserved — the
/// amount stays slashable until the transfer leaves, so the refund can
/// arrive smaller than requested. A duplicate id is acknowledged, not
/// re-registered.
async fn unbond_stake(
    State(state): State<CoordinatorState>,
    Json(req): Json<UnbondRequest>,
) -> Result<Json<UnbondView>, ApiError> {
    req.verify()
        .map_err(|e| ApiError::BadRequest(format!("unbond does not verify: {e}")))?;
    let now_ms = crate::epoch_ms();
    if now_ms.abs_diff(req.requested_at_ms) > UNBOND_MAX_SKEW_MS {
        return Err(ApiError::Unauthorized(format!(
            "unbond requested_at {} is outside the {UNBOND_MAX_SKEW_MS}ms window around {now_ms}",
            req.requested_at_ms
        )));
    }
    check_obligation_cap(&state, "unbond", req.amount_micro_usdc)?;

    let operator_b58 = req.operator.pubkey_base58();
    let outcome = state
        .bonds()
        .request_unbond(UnbondState {
            unbond_id: req.unbond_id,
            operator_pubkey_b58: operator_b58.clone(),
            recipient_address_b58: req.recipient_address_b58.clone(),
            amount_micro_usdc: req.amount_micro_usdc,
            requested_at_ms: req.requested_at_ms,
            matures_at_ms: now_ms.saturating_add(state.config().unbond_window.as_millis() as u64),
            pushed: None,
        })
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    match outcome {
        UnbondOutcome::Requested(unbond) => {
            tracing::info!(
                unbond_id = %unbond.unbond_id,
                operator = %operator_b58,
                amount_micro_usdc = unbond.amount_micro_usdc,
                matures_at_ms = unbond.matures_at_ms,
                "unbond registered; stake stays slashable until the refund pushes"
            );
            Ok(Json(UnbondView::from_state(unbond)))
        }
        UnbondOutcome::Duplicate(existing) => Ok(Json(UnbondView::from_state(existing))),
        UnbondOutcome::Insufficient {
            committed_micro_usdc,
        } => Err(ApiError::PaymentRequired(format!(
            "unbond refused: {} micro-USDC requested, {committed_micro_usdc} committed \
             after slashes, refunds and pending unbonds",
            req.amount_micro_usdc
        ))),
    }
}

#[derive(Serialize)]
struct OperatorBondFeed {
    status: crate::bond::BondStatus,
    unbonds: Vec<UnbondView>,
    slashes: Vec<SlashRecord>,
}

/// One operator's whole stake picture — posted, slashed, unbonding,
/// refunded — signed read by the key the path names, same posture as
/// the job feed.
async fn operator_bond(
    State(state): State<CoordinatorState>,
    Path(operator): Path<String>,
    headers: HeaderMap,
) -> Result<Json<OperatorBondFeed>, ApiError> {
    verify_signed_read(
        &headers,
        &operator,
        &format!("/federation/operators/{operator}/bond"),
    )?;
    Ok(Json(OperatorBondFeed {
        status: state.bonds().status(&operator),
        unbonds: state
            .bonds()
            .unbonds_for(&operator)
            .into_iter()
            .map(UnbondView::from_state)
            .collect(),
        slashes: state.bonds().slashes_for(&operator),
    }))
}

/// How to post stake on this deployment — the bond-side
/// [`deposit_info`], public on purpose: the receiving account comes
/// from the rail, the memo names the operator the stake backs, and the
/// floor and window tell an operator what the deployment requires
/// before it can be matched.
async fn bond_info(State(state): State<CoordinatorState>) -> Json<serde_json::Value> {
    Json(match state.rail() {
        Some(rail) => serde_json::json!({
            "configured": true,
            "min_bond_micro_usdc": state.config().min_bond_micro_usdc,
            "min_bond_lease_hours": state.config().min_bond_lease_hours,
            "unbond_window_secs": state.config().unbond_window.as_secs(),
            "memo_format": format!("{BOND_MEMO_PREFIX}<operator_pubkey_b58>"),
            "how": "SPL-transfer the mint to the same receiving account deposits use, \
                    with the bond memo naming your operator pubkey, then claim the \
                    transaction signature at POST /federation/operators/bond",
            "rail": rail.deposit_info(),
        }),
        None => serde_json::json!({
            "configured": false,
            "min_bond_micro_usdc": state.config().min_bond_micro_usdc,
            "min_bond_lease_hours": state.config().min_bond_lease_hours,
            "unbond_window_secs": state.config().unbond_window.as_secs(),
            "detail": "no inbound rail is configured; bond posts cannot be verified",
        }),
    })
}

/// A buyer's withdrawal history, newest first — signed read by the key
/// the path names (C9), same posture as the balance.
async fn buyer_withdrawals(
    State(state): State<CoordinatorState>,
    Path(buyer): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<WithdrawalView>>, ApiError> {
    verify_signed_read(
        &headers,
        &buyer,
        &format!("/federation/buyers/{buyer}/withdrawals"),
    )?;
    Ok(Json(
        state
            .withdrawals()
            .for_buyer(&buyer)
            .into_iter()
            .map(WithdrawalView::from_state)
            .collect(),
    ))
}

/// How to fund a balance on this deployment, straight from the
/// configured rail — public on purpose: it names where money should be
/// SENT (the deployment's own receiving account), never anyone's
/// balance or history.
async fn deposit_info(State(state): State<CoordinatorState>) -> Json<serde_json::Value> {
    Json(match state.rail() {
        Some(rail) => serde_json::json!({
            "configured": true,
            "prefunding_enforced": state.config().require_prefunded_buyers,
            "instructions": rail.deposit_info(),
        }),
        None => serde_json::json!({
            "configured": false,
            "prefunding_enforced": state.config().require_prefunded_buyers,
        }),
    })
}

#[derive(Serialize)]
struct BalanceView {
    buyer_pubkey_b58: String,
    prefunding_enforced: bool,
    #[serde(flatten)]
    funds: BuyerFunds,
}

/// A buyer's funds are its spend pattern — deposited, charged,
/// available — and buyer pubkeys are public in receipts and audit
/// rows, so the balance opens only to a signed read by the key the
/// path names (C9), same posture as the job history.
async fn buyer_balance(
    State(state): State<CoordinatorState>,
    Path(buyer): Path<String>,
    headers: HeaderMap,
) -> Result<Json<BalanceView>, ApiError> {
    verify_signed_read(
        &headers,
        &buyer,
        &format!("/federation/buyers/{buyer}/balance"),
    )?;
    Ok(Json(BalanceView {
        funds: state.buyer_funds(&buyer),
        buyer_pubkey_b58: buyer,
        prefunding_enforced: state.config().require_prefunded_buyers,
    }))
}

#[derive(Serialize)]
struct BuyerJobRow {
    job_id: Uuid,
    status: &'static str,
    disputed: bool,
    /// Why a terminal unpaid row's money came back — history readers
    /// get the same answer the per-job receipt poll serves.
    refund_reason: Option<RefundReason>,
    /// The gross price the buyer signed and the escrow held — the whole
    /// price for a fixed job, a lease's window ceiling. It is what the buyer
    /// committed, not necessarily what settled: a lease is charged only for
    /// the seconds it ran and refunded the rest, so read `charged_micro_usdc`
    /// for what was actually paid.
    price_micro_usdc: u64,
    /// What the settled hold actually charged: the whole price for a fixed
    /// job, the metered draw for a lease, zero once refunded. `null` while the
    /// job is still in flight — nothing has settled yet. Sized from the escrow,
    /// the money authority, so a lease reports the seconds it ran rather than
    /// the window ceiling `price_micro_usdc` carries.
    charged_micro_usdc: Option<u64>,
    funding_source: FundingSource,
    issued_at_ms: u64,
    /// The exact envelope the buyer signed and the operator's signed
    /// receipt — everything a client needs to re-verify the pairing
    /// locally instead of trusting this relay. Output stays on the
    /// per-job receipt endpoint; it can be large.
    envelope: covenant_compute_protocol::SignedJobEnvelope,
    receipt: Option<covenant_compute_protocol::SignedWorkReceipt>,
    /// Same payout block the receipt poll serves — history readers
    /// get the on-chain pointer without a second request per job.
    payout: Option<JobPayoutView>,
}

/// A buyer's job history, newest first. The path pubkey is not a
/// secret — everything returned is signed material the buyer produced
/// or verified material the buyer could poll per-job anyway.
async fn buyer_jobs(
    State(state): State<CoordinatorState>,
    Path(buyer): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<BuyerJobRow>>, ApiError> {
    // Envelopes carry the buyer's actual inputs, and buyer pubkeys are
    // public in receipts and audit rows — this listing opens only to a
    // signed read proving possession of the very key the path names.
    verify_signed_read(
        &headers,
        &buyer,
        &format!("/federation/buyers/{buyer}/jobs"),
    )?;

    let rows = state
        .jobs()
        .by_buyer(&buyer)
        .into_iter()
        .map(|(job_id, record)| {
            let payout = JobPayoutView::for_record(&record);
            let charged_micro_usdc = state.escrow().settled_charge_micro_usdc(job_id);
            BuyerJobRow {
                job_id,
                status: record.phase.as_str(),
                disputed: record.dispute.is_some(),
                refund_reason: record.refund_reason,
                price_micro_usdc: record.envelope.payload.price_micro_usdc,
                charged_micro_usdc,
                funding_source: record.escrow_hold.funding_source,
                issued_at_ms: record.envelope.payload.issued_at_ms,
                envelope: record.envelope,
                receipt: record.receipt,
                payout,
            }
        })
        .collect();
    Ok(Json(rows))
}

#[derive(Serialize)]
struct OperatorJobRow {
    job_id: Uuid,
    status: &'static str,
    /// The gross envelope price the escrow held — the whole price for a
    /// fixed job, a lease's window ceiling. A lease settles on the seconds
    /// it ran, so what the operator earns from it is `net_micro_usdc`.
    price_micro_usdc: u64,
    /// The marketplace fee pinned at release; zero before release and
    /// on the failure paths.
    fee_micro_usdc: u64,
    /// What this job owes the operator: gross minus fee once completed,
    /// zero otherwise. `payout` is the push that settled it — a
    /// completed row with `net > 0` and `payout: null` is money the
    /// coordinator still owes.
    net_micro_usdc: u64,
    funding_source: FundingSource,
    issued_at_ms: u64,
    payout: Option<crate::jobs::PayoutOutcome>,
    /// The buyer disputed this job's output after release (C4). The
    /// money is not clawed back; the operator sees the same fault its
    /// reputation already carries.
    disputed: bool,
    /// The buyer's signed complaint, verbatim — the reputation line's
    /// `disputed` count made addressable: an operator can only answer
    /// (or learn from) an accusation it can read. `None` on
    /// undisputed rows.
    dispute_reason: Option<String>,
    /// Why an unpaid row's money went back — the difference between
    /// `deadline_expired` (a fault this operator's reputation carries)
    /// and `buyer_cancelled` (the buyer walked away pre-accept; no
    /// fault, per the cancel's never-attributed rule) is invisible in
    /// `status` alone, and an operator auditing its own standing needs
    /// it.
    refund_reason: Option<RefundReason>,
}

/// An operator's job books, newest first — the payout confirmation
/// feed the node reconciles its own earnings ledger against. Amounts
/// and payout signatures for this operator's work are its own private
/// business, and operator pubkeys are public in receipts and audit
/// rows — so like the buyer listings, this opens only to a signed read
/// proving possession of the key the path names.
async fn operator_jobs(
    State(state): State<CoordinatorState>,
    Path(operator): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<OperatorJobRow>>, ApiError> {
    verify_signed_read(
        &headers,
        &operator,
        &format!("/federation/operators/{operator}/jobs"),
    )?;

    let rows = state
        .jobs()
        .by_operator(&operator)
        .into_iter()
        .map(|(job_id, record)| {
            let price = record.envelope.payload.price_micro_usdc;
            let settled_gross = state.escrow().hold_info(job_id).map(|(amount, _)| amount);
            let net = record.owed_net_micro_usdc(settled_gross);
            let dispute_reason = record.dispute.map(|d| d.reason);
            OperatorJobRow {
                job_id,
                status: record.phase.as_str(),
                price_micro_usdc: price,
                fee_micro_usdc: record.fee_micro_usdc,
                net_micro_usdc: net,
                funding_source: record.escrow_hold.funding_source,
                issued_at_ms: record.envelope.payload.issued_at_ms,
                payout: record.payout,
                disputed: dispute_reason.is_some(),
                dispute_reason,
                refund_reason: record.refund_reason,
            }
        })
        .collect();
    Ok(Json(rows))
}

#[derive(Serialize)]
struct SubsidyView {
    default_funding_source: &'static str,
    #[serde(flatten)]
    status: crate::escrow::SubsidyStatus,
}

/// The subsidy books in the open: how much bootstrap money is
/// committed, how much organic revenue backs it, and where the
/// kill-switch ceiling sits — the "checkable with zero homework" half
/// of the anti-faucet discipline.
async fn subsidy(State(state): State<CoordinatorState>) -> Json<SubsidyView> {
    Json(SubsidyView {
        default_funding_source: funding_source_str(state.config().default_funding_source),
        status: state.escrow().subsidy_status(),
    })
}

/// Closes the bootstrap subsidy for the life of this coordinator's
/// data directory (C6): the close journals before it latches, so a
/// restart whose environment still supplies a policy boots with the
/// switch still shut — the kill-switch would be theater if a deploy
/// could silently re-arm it. One-way on purpose: an admin bearer can
/// stop subsidy spend, never start it; re-opening means a new data
/// directory and the full boot ceremony. Idempotent — closing a
/// closed or never-armed switch re-answers the books — and admin-token
/// gated, fail closed like the rest of the admin surface. Organic
/// money is untouched; in-flight bootstrap holds settle on their own
/// terms (the latch stops new spend, it does not claw back).
async fn close_subsidy(
    State(state): State<CoordinatorState>,
    headers: HeaderMap,
) -> Result<Json<SubsidyView>, ApiError> {
    check_admin(&state, &headers)?;
    let newly_closed = state
        .escrow()
        .close_subsidy()
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if newly_closed {
        let committed = state
            .escrow()
            .subsidy_status()
            .bootstrap_committed_micro_usdc;
        state
            .record_audit(AuditKind::ComputeSubsidyClosed {
                bootstrap_committed_micro_usdc: committed,
            })
            .await;
    }
    Ok(Json(SubsidyView {
        default_funding_source: funding_source_str(state.config().default_funding_source),
        status: state.escrow().subsidy_status(),
    }))
}

#[derive(Serialize)]
struct TransferAttemptView {
    attempt_id: Uuid,
    kind: &'static str,
    amount_micro_usdc: u64,
    recipient_address_b58: String,
    memo: String,
    attempted_at_ms: u64,
    tx_signature: Option<String>,
    detail: String,
}

impl TransferAttemptView {
    fn from_state(state: crate::journal::TransferAttemptState) -> Self {
        Self {
            attempt_id: state.attempt_id,
            kind: state.kind.as_str(),
            amount_micro_usdc: state.amount_micro_usdc,
            recipient_address_b58: state.recipient_address_b58,
            memo: state.memo,
            attempted_at_ms: state.attempted_at_ms,
            tx_signature: state.tx_signature,
            detail: state.detail,
        }
    }
}

/// The admin's reconcile worklist: every open transfer bracket. Each
/// row's `memo` (and `tx_signature`, when the signer got far enough to
/// know one) is what to look up on-chain before resolving.
async fn list_transfer_attempts(
    State(state): State<CoordinatorState>,
    headers: HeaderMap,
) -> Result<Json<Vec<TransferAttemptView>>, ApiError> {
    check_admin(&state, &headers)?;
    Ok(Json(
        state
            .attempts()
            .open_attempts()
            .into_iter()
            .map(TransferAttemptView::from_state)
            .collect(),
    ))
}

#[derive(Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum TransferResolution {
    /// The transfer is on-chain: complete the obligation with the
    /// found signature instead of ever re-pushing it.
    Confirmed { tx_signature: String },
    /// The chain shows nothing for the memo/signature: clear the
    /// bracket so the retry sweep may push again.
    NotLanded,
}

/// Closes a suspended transfer bracket after a human checked the chain
/// — the only way a suspended obligation moves again. `confirmed`
/// books the completion the crash lost (no new transfer is ever
/// spawned here); `not_landed` frees the obligation for the sweep.
/// Wrong answers move real money: confirming a transfer that never
/// landed steals from the recipient, clearing one that did double-pays
/// — hence admin-gated and explicit, never automatic.
async fn resolve_transfer_attempt(
    State(state): State<CoordinatorState>,
    Path(attempt_id): Path<Uuid>,
    headers: HeaderMap,
    Json(resolution): Json<TransferResolution>,
) -> Result<Json<TransferAttemptView>, ApiError> {
    use crate::journal::TransferAttemptKind;
    check_admin(&state, &headers)?;
    let Some(attempt) = state.attempts().get(attempt_id) else {
        return Err(ApiError::NotFound(format!(
            "no open transfer attempt {attempt_id}"
        )));
    };
    match resolution {
        TransferResolution::Confirmed { tx_signature } => {
            let now_ms = crate::epoch_ms();
            match attempt.kind {
                TransferAttemptKind::JobPayout => {
                    let record = state.jobs().get(attempt_id).ok_or_else(|| {
                        ApiError::Conflict(format!(
                            "attempt {attempt_id} names a job the book no longer has"
                        ))
                    })?;
                    state
                        .record_payout_pushed(
                            attempt_id,
                            &record.operator_pubkey_b58,
                            &crate::payout::PayoutRecord {
                                job_id: attempt_id,
                                operator_pubkey_b58: record.operator_pubkey_b58.clone(),
                                payout_address: attempt.recipient_address_b58.clone(),
                                amount_micro_usdc: attempt.amount_micro_usdc,
                                recorded_at_ms: now_ms,
                                tx_signature: Some(tx_signature.clone()),
                            },
                        )
                        .await;
                }
                TransferAttemptKind::Withdrawal => {
                    let withdrawal = state.withdrawals().get(attempt_id).ok_or_else(|| {
                        ApiError::Conflict(format!(
                            "attempt {attempt_id} names a withdrawal the book no longer has"
                        ))
                    })?;
                    state
                        .record_withdrawal_pushed(
                            &withdrawal,
                            &crate::payout::TransferRecord {
                                transfer_id: attempt_id,
                                recipient_address: attempt.recipient_address_b58.clone(),
                                amount_micro_usdc: attempt.amount_micro_usdc,
                                recorded_at_ms: now_ms,
                                tx_signature: Some(tx_signature.clone()),
                            },
                        )
                        .await;
                }
                TransferAttemptKind::UnbondRefund => {
                    let unbond = state.bonds().get_unbond(attempt_id).ok_or_else(|| {
                        ApiError::Conflict(format!(
                            "attempt {attempt_id} names an unbond the book no longer has"
                        ))
                    })?;
                    state
                        .record_bond_refunded(
                            &unbond,
                            attempt.amount_micro_usdc,
                            Some(tx_signature.clone()),
                            now_ms,
                        )
                        .await;
                }
            }
            state
                .attempts()
                .resolve(attempt_id, Some(&tx_signature))
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            tracing::warn!(
                %attempt_id,
                kind = attempt.kind.as_str(),
                %tx_signature,
                "suspended transfer reconciled as landed; completion booked"
            );
        }
        TransferResolution::NotLanded => {
            if attempt.kind == TransferAttemptKind::UnbondRefund {
                state.bonds().release_reservation(attempt_id);
            }
            state
                .attempts()
                .clear(attempt_id, "admin reconciled: not landed")
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            tracing::warn!(
                %attempt_id,
                kind = attempt.kind.as_str(),
                "suspended transfer reconciled as not landed; the retry sweep may re-push"
            );
        }
    }
    Ok(Json(TransferAttemptView::from_state(attempt)))
}

#[derive(Serialize)]
struct FeesView {
    /// The rate currently in force — what the next registration will
    /// be told and the next release will pay.
    fee_bps: u32,
    /// Sum of the per-job fees actually withheld, at whatever rate was
    /// in force when each job released.
    captured_micro_usdc: u64,
    jobs_charged: usize,
}

/// The marketplace's own take, in the open — same zero-homework
/// posture as the subsidy view: any operator can check what the
/// coordinator has kept for itself against the per-job
/// `ComputeFeeCaptured` audit rows.
async fn fees(State(state): State<CoordinatorState>) -> Json<FeesView> {
    let (captured_micro_usdc, jobs_charged) = state.jobs().fees_captured();
    Json(FeesView {
        fee_bps: state.config().fee.bps(),
        captured_micro_usdc,
        jobs_charged,
    })
}

const RECEIPTS_FEED_DEFAULT: usize = 50;
const RECEIPTS_FEED_MAX: usize = 100;

/// Paging for the settlement proof feed. `before_ms` returns only
/// conclusions strictly older than the cursor — pass the last page's
/// oldest `concluded_at_ms` to walk backwards; `limit` caps the page
/// (default 50, hard max 100).
#[derive(Deserialize)]
struct ReceiptsQuery {
    #[serde(default)]
    before_ms: Option<u64>,
    #[serde(default)]
    limit: Option<usize>,
}

/// The mint a public proof cites, or the reason there is no feed: the
/// deployment must have opted in AND settle on a chain. Both refusals are
/// 404 — the feed is simply not a surface this coordinator serves.
fn public_feed_mint(state: &CoordinatorState) -> Result<&str, ApiError> {
    if !state.config().public_proof_feed {
        return Err(ApiError::NotFound(
            "the settlement proof feed is not enabled on this coordinator".into(),
        ));
    }
    state.payout().payout_mint_b58().ok_or_else(|| {
        ApiError::NotFound(
            "this coordinator settles off-chain, so there is no payout to cite".into(),
        )
    })
}

/// The public settlement feed (opt-in): concluded jobs with an on-chain
/// payout, newest first, each a self-verifying [`SettlementProof`]. A
/// reader checks a row without trusting the coordinator — the receipt's
/// signature offline, then the cited payout on-chain. It carries hashes
/// and the operator's own figures; never the buyer, and never the job's
/// input or output.
async fn receipts_feed(
    State(state): State<CoordinatorState>,
    Query(query): Query<ReceiptsQuery>,
) -> Result<Json<Vec<SettlementProof>>, ApiError> {
    let mint = public_feed_mint(&state)?;
    let limit = query
        .limit
        .unwrap_or(RECEIPTS_FEED_DEFAULT)
        .min(RECEIPTS_FEED_MAX);
    Ok(Json(state.jobs().settled_proofs(
        mint,
        query.before_ms,
        limit,
    )))
}

/// One job's settlement proof by id — the same record the feed lists,
/// fetched directly. 404 when the feed is off, the job is unknown, or it
/// has no on-chain payout to prove (still running, refunded, unpaid, or
/// settled off-chain).
async fn receipt_proof(
    State(state): State<CoordinatorState>,
    Path(job_id): Path<Uuid>,
) -> Result<Json<SettlementProof>, ApiError> {
    let mint = public_feed_mint(&state)?;
    state
        .jobs()
        .get(job_id)
        .and_then(|record| record.settlement_proof(job_id, mint))
        .map(Json)
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "no settled on-chain payout to prove for job {job_id}"
            ))
        })
}

/// The batch commitment (opt-in): one Merkle root over every settled job the
/// feed can cite, oldest conclusion first — a binding commitment to exactly
/// that set. A reader fetches the root and, from `/proof/batch/:job_id`, an
/// inclusion proof for any one settlement, and confirms membership without
/// fetching the rest; keeping the pair lets it later show the coordinator
/// committed to that settlement. Same zero-homework, buyer-anonymous posture as
/// the receipts feed.
async fn settlement_batch(
    State(state): State<CoordinatorState>,
) -> Result<Json<SettlementBatch>, ApiError> {
    let mint = public_feed_mint(&state)?;
    Ok(Json(state.jobs().settlement_batch(mint)))
}

/// One job's inclusion proof against the current batch root: the settlement,
/// its leaf position, and the audit path a reader walks to the pinned root.
/// 404 when the feed is off or the job has no on-chain payout to prove.
async fn batch_inclusion(
    State(state): State<CoordinatorState>,
    Path(job_id): Path<Uuid>,
) -> Result<Json<BatchInclusionProof>, ApiError> {
    let mint = public_feed_mint(&state)?;
    state
        .jobs()
        .inclusion_proof(job_id, mint)
        .map(Json)
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "no settled on-chain payout to prove for job {job_id}"
            ))
        })
}

/// The longest label a secret may carry. A label is the buyer's own
/// handle for a secret and rides in the route path, so it is bounded and
/// restricted to a path-safe charset — both so the signed path is
/// unambiguous and so a label cannot smuggle a path separator.
const MAX_VAULT_LABEL_LEN: usize = 128;

/// The vault, or a 404 when the deployment did not opt in. A coordinator
/// that serves no vault is indistinguishable from one built without it.
fn require_vault(state: &CoordinatorState) -> Result<&crate::vault::VaultStore, ApiError> {
    state
        .vault()
        .map(|v| &**v)
        .ok_or_else(|| ApiError::NotFound("this coordinator does not serve a secret vault".into()))
}

fn validate_vault_label(label: &str) -> Result<(), ApiError> {
    if label.is_empty() || label.len() > MAX_VAULT_LABEL_LEN {
        return Err(ApiError::BadRequest(format!(
            "a secret label must be 1..={MAX_VAULT_LABEL_LEN} characters"
        )));
    }
    if !label
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(ApiError::BadRequest(
            "a secret label may hold only letters, digits, '-', '_' and '.'".into(),
        ));
    }
    Ok(())
}

/// Gate for the vault: the caller must present a fresh signature over the
/// method and path — and, for a store, the exact body — by the owner the
/// path names (`vault` in the protocol crate). Everything short of that
/// is a 401.
fn verify_signed_vault(
    headers: &HeaderMap,
    owner_b58: &str,
    signing_path: &str,
    body: &[u8],
) -> Result<(), ApiError> {
    let header = |name: &'static str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| ApiError::Unauthorized(format!("missing {name} header")))
    };
    let signed_at_ms: u64 = header(covenant_compute_protocol::VAULT_SIGNED_AT_HEADER)?
        .parse()
        .map_err(|_| {
            ApiError::Unauthorized(format!(
                "{} must be epoch milliseconds",
                covenant_compute_protocol::VAULT_SIGNED_AT_HEADER
            ))
        })?;
    let signature = header(covenant_compute_protocol::VAULT_SIGNATURE_HEADER)?;
    covenant_compute_protocol::verify_vault(
        owner_b58,
        signing_path,
        body,
        signed_at_ms,
        signature,
        crate::epoch_ms(),
    )
    .map_err(|e| ApiError::Unauthorized(format!("vault signature rejected: {e}")))
}

fn map_vault_store_err(e: crate::vault::VaultStoreError) -> ApiError {
    let message = e.to_string();
    match e {
        crate::vault::VaultStoreError::OwnerFull
        | crate::vault::VaultStoreError::TooManyOwners(_) => ApiError::TooManyRequests(message),
        crate::vault::VaultStoreError::TooLarge(_)
        | crate::vault::VaultStoreError::NonceTooLarge(_) => ApiError::BadRequest(message),
        crate::vault::VaultStoreError::Persist(_) => ApiError::Internal(message),
    }
}

/// Stores (or replaces) a sealed secret. The signature binds the exact
/// ciphertext, so the store keeps only what the owner signed for.
async fn vault_put(
    State(state): State<CoordinatorState>,
    Path((owner, label)): Path<(String, String)>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<StatusCode, ApiError> {
    let store = require_vault(&state)?;
    validate_vault_label(&label)?;
    let signing_path = covenant_compute_protocol::vault_signing_path(
        "POST",
        &covenant_compute_protocol::vault_secret_path(&owner, &label),
    );
    verify_signed_vault(&headers, &owner, &signing_path, &body)?;
    let sealed: covenant_compute_protocol::SealedSecret = serde_json::from_slice(&body)
        .map_err(|e| ApiError::BadRequest(format!("vault body is not a sealed secret: {e}")))?;
    store
        .put(&owner, &label, sealed, crate::epoch_ms())
        .map_err(map_vault_store_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Returns a sealed secret to its owner. The coordinator hands back the
/// same opaque ciphertext it was given; only the owner's key opens it.
async fn vault_get(
    State(state): State<CoordinatorState>,
    Path((owner, label)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<covenant_compute_protocol::SealedSecret>, ApiError> {
    let store = require_vault(&state)?;
    validate_vault_label(&label)?;
    let signing_path = covenant_compute_protocol::vault_signing_path(
        "GET",
        &covenant_compute_protocol::vault_secret_path(&owner, &label),
    );
    verify_signed_vault(&headers, &owner, &signing_path, &[])?;
    store
        .get(&owner, &label)
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("no secret '{label}' for this owner")))
}

/// Removes a secret. Idempotent: deleting one that is already gone still
/// succeeds, so a retried delete is not an error.
async fn vault_delete(
    State(state): State<CoordinatorState>,
    Path((owner, label)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let store = require_vault(&state)?;
    validate_vault_label(&label)?;
    let signing_path = covenant_compute_protocol::vault_signing_path(
        "DELETE",
        &covenant_compute_protocol::vault_secret_path(&owner, &label),
    );
    verify_signed_vault(&headers, &owner, &signing_path, &[])?;
    store
        .delete(&owner, &label, crate::epoch_ms())
        .map_err(map_vault_store_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Lists an owner's secrets by label — metadata only, never a ciphertext.
async fn vault_list(
    State(state): State<CoordinatorState>,
    Path(owner): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<crate::vault::SecretMeta>>, ApiError> {
    let store = require_vault(&state)?;
    let signing_path = covenant_compute_protocol::vault_signing_path(
        "GET",
        &covenant_compute_protocol::vault_list_path(&owner),
    );
    verify_signed_vault(&headers, &owner, &signing_path, &[])?;
    Ok(Json(store.list(&owner)))
}

/// The live-capacity directory (Track A discovery): what a buyer can
/// purchase right now — (kind, model) rows aggregated over exactly the
/// operators the matcher would consider, with each row's ask range.
/// Same zero-homework posture as the fee and subsidy reads, and the
/// same anonymity as `/metrics`: counts and asks, never an operator
/// identity, so the open directory can't be turned into a per-node
/// target list.
async fn capacity(
    State(state): State<CoordinatorState>,
    Query(query): Query<CapacityQuery>,
) -> Json<CapacityView> {
    let config = state.config();
    // A buyer previewing a reputation-floored buy asks the directory to
    // show only the pool that clears their floor, so the ask range they
    // read is the one their job would actually pay. The effective floor
    // is the higher of theirs and the coordinator's standing floor, and
    // the scan `capacity_view` runs only when that floor is above zero —
    // so an anonymous, unfloored read of this public endpoint still costs
    // no reputation history read at all.
    let score_floor = config
        .min_operator_score_bps
        .max(query.min_reputation_bps.unwrap_or(0));
    Json(
        crate::matcher::capacity_view(
            state.registry(),
            state.reputation(),
            state.bonds(),
            crate::epoch_ms(),
            config.operator_liveness_timeout,
            score_floor,
            config.bond_floor(),
        )
        .await,
    )
}

/// Optional reputation floor for the capacity directory: a buyer sizing
/// a floored buy passes `?min_reputation_bps=` so the aggregate reflects
/// only operators their job could match.
#[derive(Deserialize)]
struct CapacityQuery {
    #[serde(default)]
    min_reputation_bps: Option<u32>,
}

#[derive(Serialize)]
struct PartnerView {
    referral_code: String,
    /// Where this partner gets paid — a business relationship, not a
    /// transparency figure, so only the admin bearer sees it (C9).
    /// `Some("")` on an admin view of a code accrued under a partner
    /// no longer (or never) configured — the books outlive the config.
    #[serde(skip_serializing_if = "Option::is_none")]
    payout_address: Option<String>,
    share_bps: u32,
    accrued_micro_usdc: u64,
    jobs_attributed: usize,
    /// Cumulative payouts an operator has recorded against this code
    /// (money moved out-of-band; see the mark-paid endpoint).
    paid_micro_usdc: u64,
    outstanding_micro_usdc: u64,
}

/// The rev-share books (C8): every configured partner plus every code
/// with accruals on the books — supply side (referred operators) and
/// demand side (referred buyers) summed per code, each checkable
/// against its per-job `Compute{,Buyer}PartnerShareAccrued` audit
/// rows. Accrual-only — actually paying the share out is an
/// operator-run money movement, not an autonomous one.
///
/// The amounts stay public — they are the fee-books transparency story
/// — but payout addresses are redacted unless the caller presents the
/// admin bearer. No auth header gets the public view; a WRONG bearer
/// is 401, so a mistyped token can't masquerade as "the address is
/// just hidden".
async fn partners(
    State(state): State<CoordinatorState>,
    headers: HeaderMap,
) -> Result<Json<Vec<PartnerView>>, ApiError> {
    let admin = match extract_bearer(&headers) {
        Some(_) => {
            check_admin(&state, &headers)?;
            true
        }
        None => false,
    };
    let mut accruals = state.jobs().partner_accruals();
    let view = |code: String, payout_address: String, share_bps: u32, accrued: u64, jobs| {
        let paid = state.partner_payouts().paid(&code);
        PartnerView {
            referral_code: code,
            payout_address: admin.then_some(payout_address),
            share_bps,
            accrued_micro_usdc: accrued,
            jobs_attributed: jobs,
            paid_micro_usdc: paid,
            outstanding_micro_usdc: accrued.saturating_sub(paid),
        }
    };
    let mut views: Vec<PartnerView> = state
        .config()
        .partners
        .iter()
        .map(|(code, partner)| {
            let (accrued, jobs) = accruals.remove(code).unwrap_or_default();
            view(
                code.clone(),
                partner.payout_address().to_string(),
                partner.share_bps(),
                accrued,
                jobs,
            )
        })
        .collect();
    // Whatever is left accrued under codes the current config doesn't
    // know — surfaced, not hidden, so a config edit can't quietly
    // orphan owed money.
    views.extend(
        accruals
            .into_iter()
            .map(|(code, (accrued, jobs))| view(code, String::new(), 0, accrued, jobs)),
    );
    views.sort_by(|a, b| a.referral_code.cmp(&b.referral_code));
    Ok(Json(views))
}

#[derive(serde::Deserialize)]
struct MarkPartnerPaid {
    amount_micro_usdc: u64,
    /// The operator's own evidence of the transfer — a transaction
    /// signature or note. Doubles as the idempotency key, so an honest
    /// retry of the same record moves nothing.
    reference: String,
}

#[derive(Serialize)]
struct PartnerPaidView {
    referral_code: String,
    recorded: bool,
    paid_micro_usdc: u64,
    outstanding_micro_usdc: u64,
}

/// Records an out-of-band partner payout against the rev-share books
/// (C8). The coordinator moves no money here — paying a partner is
/// real money movement and stays operator-run; this endpoint is the
/// bookkeeping that keeps `/federation/partners` honest afterwards.
/// Admin-token gated, fail closed: without a configured token every
/// call is 401.
async fn mark_partner_paid(
    State(state): State<CoordinatorState>,
    Path(code): Path<String>,
    headers: HeaderMap,
    Json(req): Json<MarkPartnerPaid>,
) -> Result<Json<PartnerPaidView>, ApiError> {
    check_admin(&state, &headers)?;
    if req.amount_micro_usdc == 0 {
        return Err(ApiError::BadRequest("amount must be positive".into()));
    }
    if req.reference.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "reference is required — record the transaction signature or a note".into(),
        ));
    }

    let (accrued, _) = state
        .jobs()
        .partner_accruals()
        .remove(&code)
        .ok_or_else(|| ApiError::NotFound(format!("no accruals under code {code}")))?;
    let outcome = state
        .partner_payouts()
        .mark_paid(req.reference.trim(), &code, req.amount_micro_usdc, accrued)
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    use crate::accounts::PartnerPayoutOutcome;
    let (recorded, paid) = match outcome {
        PartnerPayoutOutcome::Recorded {
            paid_total_micro_usdc,
        } => {
            state
                .record_audit(AuditKind::ComputePartnerSharePaid {
                    referral_code: code.clone(),
                    amount_micro_usdc: req.amount_micro_usdc,
                    reference: req.reference.trim().into(),
                })
                .await;
            (true, paid_total_micro_usdc)
        }
        PartnerPayoutOutcome::Duplicate {
            paid_total_micro_usdc,
        } => (false, paid_total_micro_usdc),
        PartnerPayoutOutcome::ExceedsAccrued { paid_micro_usdc } => {
            return Err(ApiError::Conflict(format!(
                "recording {} micro-USDC would exceed the books: {} accrued, {} already paid",
                req.amount_micro_usdc, accrued, paid_micro_usdc
            )));
        }
    };
    Ok(Json(PartnerPaidView {
        referral_code: code,
        recorded,
        paid_micro_usdc: paid,
        outstanding_micro_usdc: accrued.saturating_sub(paid),
    }))
}

/// The buyer's live view of one lease: where the machine is, how long
/// it has been running, and what it has cost so far. A running meter
/// the payer cannot watch is the thing that makes metered billing feel
/// like a trap, so this answers the same numbers settlement will use —
/// derived from the same signed terms and the same coordinator clock.
/// [`LeaseView`] is the shared protocol shape the buyer deserializes.
async fn lease_view(
    State(state): State<CoordinatorState>,
    Path(job_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<LeaseView>, ApiError> {
    let record = state
        .jobs()
        .get(job_id)
        .ok_or_else(|| ApiError::NotFound(format!("no such job {job_id}")))?;
    // The access grant names the operator's live machine — an address
    // today, a session credential once revocable access lands — reachable
    // only with the buyer's own key. It rides back only to the buyer who
    // signed the lease, behind the same signed read the receipt view uses,
    // so a bystander who learns the job id cannot read where a running box
    // is. The serving node polls this view unsigned for the buyer's close
    // flag alone, so the meter and status stay open; only the endpoint is
    // held back. A read that carries a signature must present a valid one,
    // rather than quietly dropping to the public view on a bad one.
    let include_access = if headers.contains_key(covenant_compute_protocol::READ_SIGNATURE_HEADER) {
        verify_signed_read(
            &headers,
            &record.envelope.payload.buyer.pubkey_base58(),
            &format!("/federation/jobs/{job_id}/lease"),
        )?;
        true
    } else {
        false
    };
    let settled_gross = state.escrow().hold_info(job_id).map(|(amount, _)| amount);
    lease_view_response(&record, job_id, include_access, settled_gross)
}

/// Projects a lease record into the buyer's view. `include_access` gates
/// the reachable endpoint: the open GET view carries it only on a
/// buyer-signed read, while `close_lease` — which already verified the
/// buyer's signed body — includes it, so ending a session still reports
/// where the machine was.
fn lease_view_response(
    record: &JobRecord,
    job_id: Uuid,
    include_access: bool,
    settled_gross: Option<u64>,
) -> Result<Json<LeaseView>, ApiError> {
    if record.envelope.payload.kind != JobKind::LeaseSession {
        return Err(ApiError::BadRequest(format!("job {job_id} is not a lease")));
    }
    let terms = covenant_compute_protocol::parse_lease_terms(&record.envelope.payload.input)
        .map_err(|e| ApiError::Internal(format!("lease terms unreadable: {e}")))?
        .ok_or_else(|| ApiError::Internal("lease carries no terms".into()))?;
    // A concluded lease reports what it billed; a live one reports the
    // meter as of right now. A refunded conclusion billed nothing, so it
    // reports no elapsed rather than a wall clock that climbs past it. A
    // lease the boot reconcile concluded from a released hold lost its meter
    // stamp, so the charge is read from `settled_gross` (the escrow hold)
    // even while its elapsed can only read zero.
    let now_ms = crate::epoch_ms();
    Ok(Json(LeaseView {
        job_id,
        status: record.phase.as_str().to_string(),
        access: if include_access {
            record.live_lease_access()
        } else {
            None
        },
        rate_micro_usdc_per_sec: terms.rate_micro_usdc_per_sec,
        max_duration_secs: terms.max_duration_secs,
        elapsed_ms: record.lease_elapsed_ms(now_ms),
        charged_micro_usdc: record.lease_charged_micro_usdc(&terms, now_ms, settled_gross),
        close_requested: record.close_requested_at_ms.is_some(),
    }))
}

/// Ends a running lease session on the buyer's signed instruction —
/// the meter's stop button. The close is recorded, not executed here:
/// the serving node sees it on its next poll, shuts the session down
/// and submits its receipt, which is what actually settles the meter.
/// Recording rather than settling on the spot is deliberate — the
/// machine has to be released before the money is, or a buyer could
/// stop paying while still holding the box.
///
/// Strictly post-acceptance, the mirror of [`cancel_job`]: before an
/// operator accepts there is no session to end and a cancel refunds
/// whole; after conclusion the meter has already stopped. Both of those
/// answer the fact rather than erroring, so an honest retry is safe.
async fn close_lease(
    State(state): State<CoordinatorState>,
    Path(job_id): Path<Uuid>,
    Json(req): Json<LeaseCloseRequest>,
) -> Result<Json<LeaseView>, ApiError> {
    if req.job_id != job_id {
        return Err(ApiError::BadRequest(
            "path job_id does not match the close's job_id".into(),
        ));
    }
    req.verify()
        .map_err(|e| ApiError::BadRequest(format!("lease close does not verify: {e}")))?;
    let now_ms = crate::epoch_ms();
    if now_ms.abs_diff(req.closed_at_ms) > LEASE_CLOSE_MAX_SKEW_MS {
        return Err(ApiError::Unauthorized(format!(
            "lease close closed_at {} is outside the {LEASE_CLOSE_MAX_SKEW_MS}ms window \
             around {now_ms}",
            req.closed_at_ms
        )));
    }

    let record = state
        .jobs()
        .get(job_id)
        .ok_or_else(|| ApiError::NotFound(format!("no such job {job_id}")))?;
    if record.envelope.payload.kind != JobKind::LeaseSession {
        return Err(ApiError::BadRequest(format!(
            "job {job_id} is not a lease; only a lease session can be closed"
        )));
    }
    if req.buyer.pubkey_base58() != record.envelope.payload.buyer.pubkey_base58() {
        return Err(ApiError::Unauthorized(
            "only the buyer who signed this lease may close it".into(),
        ));
    }

    if !state
        .jobs()
        .request_lease_close(job_id, now_ms)
        .map_err(|e| ApiError::Internal(e.to_string()))?
    {
        // Not running, or already closing: both are answered as facts.
        // A lease that was never accepted has no session to stop, and
        // its money comes back through the deadline sweep untouched.
        match record.phase {
            JobPhase::Accepted | JobPhase::Completed | JobPhase::Failed | JobPhase::Refunded => {}
            JobPhase::Offered | JobPhase::Rejected | JobPhase::AwaitingCheck => {
                return Err(ApiError::Conflict(format!(
                    "lease {job_id} is {} — no session is running to close",
                    record.phase.as_str()
                )));
            }
        }
    } else {
        state
            .record_audit(AuditKind::ComputeLeaseCloseRequested {
                job_id,
                buyer_pubkey_b58: record.envelope.payload.buyer.pubkey_base58(),
                operator_pubkey_b58: record.operator_pubkey_b58.clone(),
            })
            .await;
        tracing::info!(%job_id, "buyer closed a running lease session");
    }
    // Re-read so the returned view reflects the close just recorded, and
    // include the access grant: the signed close body already proved this
    // is the lease's buyer, so ending the session still reports where the
    // machine was.
    let record = state
        .jobs()
        .get(job_id)
        .ok_or_else(|| ApiError::NotFound(format!("no such job {job_id}")))?;
    let settled_gross = state.escrow().hold_info(job_id).map(|(amount, _)| amount);
    lease_view_response(&record, job_id, true, settled_gross)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_is_ok_when_the_journal_can_persist() {
        let (status, body) = health_response(true);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.0["status"].as_str(), Some("ok"));
    }

    #[test]
    fn health_is_unavailable_when_the_journal_cannot_persist() {
        let (status, body) = health_response(false);
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body.0["status"].as_str(), Some("degraded"));
    }

    #[tokio::test]
    async fn a_failed_lease_result_voids_the_vault_even_when_the_refund_already_settled() {
        use crate::jobs::{JobPhase, JobRecord};
        use crate::onchain_meter::NoopLeaseMeter;
        use crate::payout::MockPayout;
        use crate::reputation::NoReputation;
        use crate::state::{CoordinatorConfig, CoordinatorState};
        use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency, A2ATaskStatus};
        use covenant_audit::InMemoryAuditLog;
        use covenant_compute_protocol::{
            lease_input, CapabilityRequirement, JobEnvelopePayload, JobKind, JobMeter, LeaseTerms,
            SignedJobEnvelope, SignedWorkReceipt, WorkReceiptPayload,
        };
        use covenant_identity::LocalIdentity;
        use covenant_mcp::Content;
        use std::sync::Arc;

        // A failed result arriving after the hold was already refunded —
        // the deadline sweep beat it, or an earlier attempt refunded and
        // then its phase write failed — must still void the funded vault
        // rather than 409 at the refund. Skipping it there leaves a vault
        // the operator can settle for the seconds it ran, after the buyer
        // has its window back.
        let meter = Arc::new(NoopLeaseMeter::new());
        let config = CoordinatorConfig {
            lease_meter: Some(meter.clone()),
            ..Default::default()
        };
        let state = CoordinatorState::new(
            LocalIdentity::generate("coordinator@test"),
            config,
            Arc::new(NoReputation),
            Arc::new(MockPayout::new()),
            Arc::new(InMemoryAuditLog::new()) as Arc<dyn covenant_audit::AuditLog>,
        );

        let buyer = LocalIdentity::generate("buyer@test");
        let operator = LocalIdentity::generate("operator@test");
        let terms = LeaseTerms {
            max_duration_secs: 600,
            rate_micro_usdc_per_sec: 100,
            client_public_key: None,
        };
        let ceiling = terms.max_price_micro_usdc().unwrap();
        let now = crate::epoch_ms();

        let job_id = Uuid::new_v4();
        let payload = JobEnvelopePayload {
            job_id,
            buyer: buyer.agent_id(),
            kind: JobKind::LeaseSession,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: None,
                kind: JobKind::LeaseSession,
                max_duration_secs: 600,
                min_reputation_bps: None,
            },
            input: vec![lease_input(terms).unwrap()],
            price_micro_usdc: ceiling,
            deadline_ms: 3_600_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "fail-void-test"),
            issued_at_ms: now,
            referral_code: None,
            stream: true,
        };
        let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
        let escrow_hold = state
            .escrow()
            .hold(job_id, &buyer.agent_id(), ceiling)
            .await
            .unwrap();
        let record = JobRecord {
            operator_pubkey_b58: operator.agent_id().pubkey_base58(),
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
            offered_at_ms: now,
            pinned: false,
            accepted_at_ms: Some(now),
            metered_elapsed_ms: None,
            close_requested_at_ms: None,
            lease_access: None,
            check_jobs: Vec::new(),
            checks_task: None,
            hidden_checks: None,
            vote_round: None,
            rework: None,
            order: None,
        };
        state.jobs().insert(job_id, record).unwrap();
        crate::onchain_meter::open_lease_onchain(&state, job_id).await;
        assert_eq!(meter.opened().len(), 1, "the lease opened on-chain");

        // The precondition the fix hardens: the hold is already refunded
        // when the failed result lands.
        state
            .escrow()
            .refund(job_id, RefundReason::DeadlineExpired)
            .await
            .unwrap();

        let output = vec![Content::text("it failed")];
        let receipt = SignedWorkReceipt::sign(
            WorkReceiptPayload {
                job_id,
                operator: operator.agent_id(),
                job_hash_hex: "aa".repeat(32),
                result_hash_hex: output_hash_hex(&output),
                meter: JobMeter {
                    wall_ms: 10,
                    tokens_in: None,
                    tokens_out: None,
                    gpu_seconds: None,
                    finish_reason: None,
                },
                price_micro_usdc: ceiling,
                status: A2ATaskStatus::Error,
                executed_at_ms: now,
                node_audit_root_hex: "cc".repeat(32),
            },
            &operator,
        )
        .unwrap();

        let ack = submit_result(
            State(state.clone()),
            Path(job_id),
            Json(JobResultMessage { receipt, output }),
        )
        .await
        .expect("a failed result over an already-refunded hold still acks, not 409");
        assert!(matches!(ack.0.settled, ResultSettlement::Refunded));
        assert_eq!(
            meter.voided(),
            vec![job_id],
            "the funded vault is voided even though the refund had already settled"
        );
    }

    #[tokio::test]
    async fn a_crash_recovered_lease_redelivery_pins_the_settled_meter_not_a_fresh_one() {
        use crate::jobs::{JobPhase, JobRecord};
        use crate::payout::MockPayout;
        use crate::reputation::NoReputation;
        use crate::state::{CoordinatorConfig, CoordinatorState};
        use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency, A2ATaskStatus};
        use covenant_audit::InMemoryAuditLog;
        use covenant_compute_protocol::{
            lease_input, CapabilityRequirement, JobEnvelopePayload, JobKind, JobMeter, LeaseTerms,
            SignedJobEnvelope, SignedWorkReceipt, WorkReceiptPayload,
        };
        use covenant_identity::LocalIdentity;
        use covenant_mcp::Content;
        use std::sync::Arc;

        // The crash-recovery shape: the escrow released the metered charge for
        // the seconds a lease actually ran, then the crash fell before the
        // record write, so the boot reconcile concluded a stampless Completed
        // record. The operator redelivers its receipt to heal it — but arrives
        // long after the settlement. The redelivery must pin the meter the
        // hold already settled, not re-measure accept→now and bill the whole
        // recovery gap. Getting it wrong over-reports the buyer's charge and
        // makes the public settlement proof's gross disagree with fee+net.
        let state = CoordinatorState::new(
            LocalIdentity::generate("coordinator@test"),
            CoordinatorConfig::default(),
            Arc::new(NoReputation),
            Arc::new(MockPayout::new()),
            Arc::new(InMemoryAuditLog::new()) as Arc<dyn covenant_audit::AuditLog>,
        );

        let buyer = LocalIdentity::generate("buyer@test");
        let operator = LocalIdentity::generate("operator@test");
        let terms = LeaseTerms {
            max_duration_secs: 600,
            rate_micro_usdc_per_sec: 100,
            client_public_key: None,
        };
        let ceiling = terms.max_price_micro_usdc().unwrap();
        // The session ran 5s → 500 settled, well under the 60_000 ceiling.
        let settled = terms.metered_micro_usdc(5_000);
        assert_eq!(settled, 500);
        let now = crate::epoch_ms();
        // Accepted a minute ago, so a now-based re-meter would read ~60s → 6_000.
        let accepted_at = now - 60_000;

        let job_id = Uuid::new_v4();
        let payload = JobEnvelopePayload {
            job_id,
            buyer: buyer.agent_id(),
            kind: JobKind::LeaseSession,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: None,
                kind: JobKind::LeaseSession,
                max_duration_secs: 600,
                min_reputation_bps: None,
            },
            input: vec![lease_input(terms.clone()).unwrap()],
            price_micro_usdc: ceiling,
            deadline_ms: 3_600_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "recover-meter-test"),
            issued_at_ms: accepted_at,
            referral_code: None,
            stream: true,
        };
        let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
        let escrow_hold = state
            .escrow()
            .hold(job_id, &buyer.agent_id(), ceiling)
            .await
            .unwrap();

        let output = vec![Content::text(
            "lease session closed by the buyer after 5000ms",
        )];
        let receipt = SignedWorkReceipt::sign(
            WorkReceiptPayload {
                job_id,
                operator: operator.agent_id(),
                job_hash_hex: "aa".repeat(32),
                result_hash_hex: output_hash_hex(&output),
                meter: JobMeter {
                    wall_ms: 5_000,
                    tokens_in: None,
                    tokens_out: None,
                    gpu_seconds: None,
                    finish_reason: None,
                },
                price_micro_usdc: ceiling,
                status: A2ATaskStatus::Ok,
                executed_at_ms: accepted_at,
                node_audit_root_hex: "cc".repeat(32),
            },
            &operator,
        )
        .unwrap();

        // Pre-crash: the meter settled the hold at the 5s figure.
        state
            .escrow()
            .release_metered(job_id, &receipt, settled)
            .await
            .unwrap();
        // Boot reconcile's shape: Completed, no receipt, no stamp.
        let record = JobRecord {
            operator_pubkey_b58: operator.agent_id().pubkey_base58(),
            payout_address: "operator-payout".into(),
            envelope,
            escrow_hold,
            phase: JobPhase::Completed,
            receipt: None,
            output: None,
            fee_micro_usdc: 0,
            referral_code: None,
            partner_share_micro_usdc: 0,
            buyer_referral_code: None,
            buyer_partner_share_micro_usdc: 0,
            payout: None,
            concluded_at_ms: Some(now),
            refund_reason: None,
            dispute: None,
            offered_at_ms: accepted_at,
            pinned: false,
            accepted_at_ms: Some(accepted_at),
            metered_elapsed_ms: None,
            close_requested_at_ms: None,
            lease_access: None,
            check_jobs: Vec::new(),
            checks_task: None,
            hidden_checks: None,
            vote_round: None,
            rework: None,
            order: None,
        };
        state.jobs().insert(job_id, record).unwrap();

        // The operator redelivers the same receipt.
        let ack = submit_result(
            State(state.clone()),
            Path(job_id),
            Json(JobResultMessage {
                receipt,
                output: output.clone(),
            }),
        )
        .await
        .expect("the redelivery heals the record");
        assert!(
            matches!(ack.0.settled, ResultSettlement::Released),
            "the redelivered receipt settles the release it was owed"
        );

        let healed = state.jobs().get(job_id).unwrap();
        let stamp = healed
            .metered_elapsed_ms
            .expect("the receipt landed a stamp");
        assert_eq!(
            terms.metered_micro_usdc(stamp),
            settled,
            "the pinned meter reproduces the settled charge, not the recovery gap"
        );
        assert_eq!(
            healed.released_gross_micro_usdc(),
            settled,
            "released gross stays the settled figure, so the settlement proof's gross == fee + net"
        );
        assert_eq!(
            healed.lease_charged_micro_usdc(&terms, crate::epoch_ms(), Some(settled)),
            settled,
            "the buyer's lease view reports what was charged, not the whole recovery gap"
        );
    }

    #[tokio::test]
    async fn a_zero_metered_lease_refund_faults_no_operator() {
        use crate::jobs::{JobPhase, JobRecord};
        use crate::payout::MockPayout;
        use crate::reputation::{AuditReputationSource, NoReputation, ReputationSource};
        use crate::state::{CoordinatorConfig, CoordinatorState};
        use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency, A2ATaskStatus};
        use covenant_audit::{AuditLog, InMemoryAuditLog};
        use covenant_compute_protocol::{
            lease_input, CapabilityRequirement, JobEnvelopePayload, JobKind, JobMeter, LeaseTerms,
            SignedJobEnvelope, SignedWorkReceipt, WorkReceiptPayload,
        };
        use covenant_identity::LocalIdentity;
        use covenant_mcp::Content;
        use std::sync::Arc;

        // A lease whose accept was lost never stamps `accepted_at_ms`, so its
        // meter reads zero: the coordinator never observed the session run.
        // The operator still submits a verified `Ok` receipt. The whole window
        // refunds (nobody is charged) and — the point of this test — the
        // operator is NOT faulted for it, unlike an execution_failed /
        // operator_rejected / deadline_expired refund. Attributing the refund
        // to the operator would tank the reputation of a node that served
        // honestly on a session the coordinator simply never clocked.
        let audit = Arc::new(InMemoryAuditLog::new());
        let state = CoordinatorState::new(
            LocalIdentity::generate("coordinator@test"),
            CoordinatorConfig::default(),
            Arc::new(NoReputation),
            Arc::new(MockPayout::new()),
            audit.clone() as Arc<dyn AuditLog>,
        );

        let buyer = LocalIdentity::generate("buyer@test");
        let operator = LocalIdentity::generate("operator@test");
        let operator_b58 = operator.agent_id().pubkey_base58();
        let terms = LeaseTerms {
            max_duration_secs: 600,
            rate_micro_usdc_per_sec: 100,
            client_public_key: None,
        };
        let ceiling = terms.max_price_micro_usdc().unwrap();
        let now = crate::epoch_ms();

        let job_id = Uuid::new_v4();
        let payload = JobEnvelopePayload {
            job_id,
            buyer: buyer.agent_id(),
            kind: JobKind::LeaseSession,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: None,
                kind: JobKind::LeaseSession,
                max_duration_secs: 600,
                min_reputation_bps: None,
            },
            input: vec![lease_input(terms.clone()).unwrap()],
            price_micro_usdc: ceiling,
            deadline_ms: 3_600_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "zero-meter-test"),
            issued_at_ms: now,
            referral_code: None,
            stream: true,
        };
        let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
        let escrow_hold = state
            .escrow()
            .hold(job_id, &buyer.agent_id(), ceiling)
            .await
            .unwrap();

        // Still `Offered`, no accept stamp — the lost-accept shape that leaves
        // the meter with nothing to read.
        let record = JobRecord {
            operator_pubkey_b58: operator_b58.clone(),
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
            offered_at_ms: now,
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
            order: None,
        };
        state.jobs().insert(job_id, record).unwrap();

        let output = vec![Content::text("lease closed with no observed runtime")];
        let receipt = SignedWorkReceipt::sign(
            WorkReceiptPayload {
                job_id,
                operator: operator.agent_id(),
                job_hash_hex: "aa".repeat(32),
                result_hash_hex: output_hash_hex(&output),
                meter: JobMeter {
                    wall_ms: 0,
                    tokens_in: None,
                    tokens_out: None,
                    gpu_seconds: None,
                    finish_reason: None,
                },
                price_micro_usdc: ceiling,
                status: A2ATaskStatus::Ok,
                executed_at_ms: now,
                node_audit_root_hex: "cc".repeat(32),
            },
            &operator,
        )
        .unwrap();

        let ack = submit_result(
            State(state.clone()),
            Path(job_id),
            Json(JobResultMessage {
                receipt,
                output: output.clone(),
            }),
        )
        .await
        .expect("a zero-metered lease settles");
        assert!(
            matches!(ack.0.settled, ResultSettlement::Refunded),
            "an unobserved session refunds the whole window"
        );

        // The refund row names no operator, so `reputation::classify` reads no
        // fault from it.
        let refund = audit
            .recent(usize::MAX)
            .await
            .unwrap()
            .into_iter()
            .find_map(|e| match e.kind {
                AuditKind::ComputeJobRefunded {
                    job_id: id,
                    reason,
                    operator_pubkey_b58,
                } if id == job_id => Some((reason, operator_pubkey_b58)),
                _ => None,
            })
            .expect("the settlement wrote a refund row");
        assert_eq!(refund.0, "no_metered_usage");
        assert_eq!(
            refund.1, None,
            "a zero-metered lease refund is attributed to no operator"
        );

        // End to end: the operator carries no reputation fault for it.
        let reputation = AuditReputationSource::new(audit.clone() as Arc<dyn AuditLog>);
        assert_eq!(
            reputation.stats(&operator_b58).await.faults,
            0,
            "an unobserved session is nobody's reputation fault"
        );
    }

    #[tokio::test]
    async fn a_reject_voids_the_vault_even_when_the_refund_already_settled() {
        use crate::jobs::{JobPhase, JobRecord};
        use crate::onchain_meter::NoopLeaseMeter;
        use crate::payout::MockPayout;
        use crate::reputation::NoReputation;
        use crate::state::{CoordinatorConfig, CoordinatorState};
        use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
        use covenant_audit::InMemoryAuditLog;
        use covenant_compute_protocol::{
            lease_input, CapabilityProfile, CapabilityRequirement, HardwareClass,
            JobEnvelopePayload, JobKind, LeaseTerms, PriceAsk, PriceUnit, SignedJobEnvelope,
        };
        use covenant_identity::LocalIdentity;
        use std::sync::Arc;

        // An operator rejecting a lease it had already accepted, arriving
        // after the hold was already refunded — a racing deadline sweep, or
        // a sibling branch whose phase write failed after its own refund
        // landed — must still void the funded vault, not 409 at the refund.
        // Boot reconcile re-refunds a stranded hold but never voids, so the
        // live reject is the vault's only chance to come back; skip it and
        // the operator could still settle the vault for the seconds it
        // metered before rejecting.
        let meter = Arc::new(NoopLeaseMeter::new());
        let config = CoordinatorConfig {
            lease_meter: Some(meter.clone()),
            ..Default::default()
        };
        let state = CoordinatorState::new(
            LocalIdentity::generate("coordinator@test"),
            config,
            Arc::new(NoReputation),
            Arc::new(MockPayout::new()),
            Arc::new(InMemoryAuditLog::new()) as Arc<dyn covenant_audit::AuditLog>,
        );

        let buyer = LocalIdentity::generate("buyer@test");
        let operator = LocalIdentity::generate("operator@test");
        let payout_address = bs58::encode([7u8; 32]).into_string();
        let session = state
            .registry()
            .register(
                &RegisterRequest::sign(
                    CapabilityProfile {
                        operator: operator.agent_id(),
                        hardware: HardwareClass::CpuOnly,
                        vram_gb: 0,
                        models_served: vec!["any".into()],
                        job_kinds: vec![JobKind::LeaseSession],
                        price: PriceAsk {
                            unit: PriceUnit::PerLeaseHour,
                            micro_usdc: 360_000,
                        },
                        tee_capable: false,
                        kind_prices: Vec::new(),
                        kind_models: Vec::new(),
                    },
                    payout_address.clone(),
                    &operator,
                )
                .unwrap(),
                crate::epoch_ms(),
                None,
                false,
            )
            .unwrap();

        let terms = LeaseTerms {
            max_duration_secs: 600,
            rate_micro_usdc_per_sec: 100,
            client_public_key: None,
        };
        let ceiling = terms.max_price_micro_usdc().unwrap();
        let now = crate::epoch_ms();

        let job_id = Uuid::new_v4();
        let payload = JobEnvelopePayload {
            job_id,
            buyer: buyer.agent_id(),
            kind: JobKind::LeaseSession,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: None,
                kind: JobKind::LeaseSession,
                max_duration_secs: 600,
                min_reputation_bps: None,
            },
            input: vec![lease_input(terms).unwrap()],
            price_micro_usdc: ceiling,
            deadline_ms: 3_600_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "reject-void-test"),
            issued_at_ms: now,
            referral_code: None,
            stream: true,
        };
        let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
        let escrow_hold = state
            .escrow()
            .hold(job_id, &buyer.agent_id(), ceiling)
            .await
            .unwrap();
        let record = JobRecord {
            operator_pubkey_b58: operator.agent_id().pubkey_base58(),
            payout_address,
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
            offered_at_ms: now,
            pinned: false,
            accepted_at_ms: Some(now),
            metered_elapsed_ms: None,
            close_requested_at_ms: None,
            lease_access: None,
            check_jobs: Vec::new(),
            checks_task: None,
            hidden_checks: None,
            vote_round: None,
            rework: None,
            order: None,
        };
        state.jobs().insert(job_id, record).unwrap();
        crate::onchain_meter::open_lease_onchain(&state, job_id).await;
        assert_eq!(meter.opened().len(), 1, "the lease opened on-chain");

        // The precondition the fix hardens: the hold is already refunded
        // when the reject lands.
        state
            .escrow()
            .refund(job_id, RefundReason::DeadlineExpired)
            .await
            .unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, format!("Bearer {session}").parse().unwrap());
        let status = accept_job(
            State(state.clone()),
            Path(job_id),
            headers,
            Json(JobAccept::Reject {
                job_id,
                reason: "out of capacity".into(),
            }),
        )
        .await
        .expect("a reject over an already-refunded hold still 200s, not 409");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            meter.voided(),
            vec![job_id],
            "the funded vault is voided even though the refund had already settled"
        );
    }
}
