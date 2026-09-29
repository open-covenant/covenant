//! Coordinator binary. `$COVENANT_COMPUTE_COORDINATOR_HOME` (default
//! `$HOME/.covenant-compute-coordinator`) holds the coordinator's own
//! identity key — stable across restarts, since operators pin this
//! pubkey out-of-band to verify `EscrowHoldAttestation`s
//! (build-notes-phase1-foundation.md §1.7 step 3) — and
//! `journal.jsonl`, the append-only journal the job book and escrow
//! replay at boot so holds and in-flight jobs survive a restart. The
//! operator registry stays in-memory on purpose: nodes re-register on
//! heartbeat rejection.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use covenant_audit::JsonlAuditLog;
use covenant_compute_coordinator::{
    router, spawn_periodic_sweep, AuditReputationSource, CoordinatorConfig, CoordinatorState,
    LeaseMeter, MockPayout, NoopLeaseMeter, Payout, SidecarLeaseMeter, SidecarLeaseMeterConfig,
    SidecarPayout, SidecarPayoutConfig,
};
use covenant_identity::LocalIdentity;
use tracing_subscriber::EnvFilter;

fn coordinator_home() -> anyhow::Result<PathBuf> {
    if let Ok(p) = std::env::var("COVENANT_COMPUTE_COORDINATOR_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".covenant-compute-coordinator"))
}

/// Reads a boot knob that has a default: absent falls back to `default`,
/// present-but-unparseable fails the boot with a clean error naming the
/// variable, the offending value and why. A misconfigured service should
/// refuse to start with one readable line, not crash with a panic
/// backtrace and a source path, nor silently ignore a typo and serve on
/// the wrong port. `hint` reads into "{key} {hint}".
fn env_parse<T>(key: &str, default: T, hint: &str) -> anyhow::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match std::env::var(key) {
        Ok(v) => v
            .trim()
            .parse()
            .map_err(|e| anyhow::anyhow!("{key} {hint} (got {v:?}): {e}")),
        Err(_) => Ok(default),
    }
}

/// [`env_parse`] for a knob with no default: an unset variable is
/// `None`, a set one parses or fails the boot cleanly.
fn env_parse_opt<T>(key: &str, hint: &str) -> anyhow::Result<Option<T>>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match std::env::var(key) {
        Ok(v) => v
            .trim()
            .parse()
            .map(Some)
            .map_err(|e| anyhow::anyhow!("{key} {hint} (got {v:?}): {e}")),
        Err(_) => Ok(None),
    }
}

/// [`env_parse`] for a boolean toggle. Unset takes `default`; a set value
/// parses case-insensitively (`1`/`true`/`yes`/`on` vs `0`/`false`/`no`/
/// `off`), anything else fails the boot. An exact `== Ok("1")` compare
/// reads every other spelling — `true`, `yes`, `TRUE` — as off, so an
/// operator asking to enforce buyer prefunding could silently get the
/// custodial-promise posture instead; refusing the unrecognized value
/// names the mistake rather than guessing at it.
fn env_bool(key: &str, default: bool) -> anyhow::Result<bool> {
    match std::env::var(key) {
        Ok(v) => parse_bool(key, &v),
        Err(_) => Ok(default),
    }
}

fn parse_bool(key: &str, raw: &str) -> anyhow::Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => Err(anyhow::anyhow!(
            "{key} must be a boolean (1/true/yes/on or 0/false/no/off), got {other:?}"
        )),
    }
}

/// Refuses a `Some(0)` for an opt-in ceiling. A cap of 0 is not a small
/// cap, it is a closed door — a registry cap of 0 turns away every
/// operator, an in-flight cap of 0 refuses every submission — and since
/// these knobs are opt-in (unset means uncapped), 0 is only ever a
/// mistake. Refusing it by name beats silently taking the coordinator
/// out of service.
fn require_positive_cap(key: &str, cap: Option<usize>) -> anyhow::Result<()> {
    anyhow::ensure!(
        cap != Some(0),
        "{key} must be a positive integer — 0 turns every request away; unset it to run uncapped"
    );
    Ok(())
}

/// Payout backend selection. Defaults to `MockPayout` — the existing
/// hermetic e2e stays exactly as-is unless an operator explicitly
/// opts in. `COVENANT_COMPUTE_PAYOUT_BACKEND=sidecar` switches to a
/// real signed SPL transfer via `SidecarPayout`; every one of its
/// config values is required with no fallback, since a half-configured
/// real-money path failing loudly at startup is far better than one
/// silently defaulting onto the wrong network or an unbounded cap —
/// see build-notes-phase1-payout.md.
fn build_payout() -> anyhow::Result<Arc<dyn Payout>> {
    if std::env::var("COVENANT_COMPUTE_PAYOUT_BACKEND").as_deref() != Ok("sidecar") {
        tracing::info!("payout backend: mock (records intended transfers, moves no funds)");
        return Ok(Arc::new(MockPayout::new()));
    }

    let require = |key: &str, why: &str| -> anyhow::Result<String> {
        std::env::var(key).map_err(|_| anyhow::anyhow!("{key} must be set when {why}"))
    };
    let sidecar = "COVENANT_COMPUTE_PAYOUT_BACKEND=sidecar";
    let signer_binary = require("COVENANT_COMPUTE_PAYOUT_SIGNER_BINARY", sidecar)?;
    let rpc_url = require(
        "COVENANT_X402_RPC_URL",
        "COVENANT_COMPUTE_PAYOUT_BACKEND=sidecar — a real payout path never assumes a network",
    )?;
    let funding_keypair_path = require("COVENANT_X402_FUNDING_KEYPAIR", sidecar)?;
    let mint = require("COVENANT_COMPUTE_PAYOUT_MINT", sidecar)?;
    let cap_micro_usdc: u64 = require(
        "COVENANT_COMPUTE_PAYOUT_CAP_MICRO_USDC",
        "COVENANT_COMPUTE_PAYOUT_BACKEND=sidecar — sizing the cap is an operator decision, \
         not a safe default",
    )?
    .trim()
    .parse()
    .context("COVENANT_COMPUTE_PAYOUT_CAP_MICRO_USDC must be a u64")?;
    // Optional, default 0 = no per-transfer limit on principal returns
    // (buyer withdrawals, unbond refunds). Those are bounded by the
    // books already; the per-job payout cap deliberately does not apply
    // to a party reclaiming its own money. Set this only to bound a
    // single obligation transfer — requests above it are refused up
    // front, never left for the sweep to spin on.
    let obligation_cap_micro_usdc: u64 = env_parse(
        "COVENANT_COMPUTE_OBLIGATION_CAP_MICRO_USDC",
        0,
        "must be a u64",
    )?;

    tracing::info!(
        signer_binary = %signer_binary,
        mint = %mint,
        cap_micro_usdc,
        obligation_cap_micro_usdc,
        "payout backend: sidecar (real, confirmed SPL transfers)"
    );
    Ok(Arc::new(SidecarPayout::new(SidecarPayoutConfig {
        signer_binary: PathBuf::from(signer_binary),
        rpc_url,
        funding_keypair_path,
        mint,
        cap_micro_usdc,
        obligation_cap_micro_usdc,
    })))
}

/// The operator stake gate (`covenant_compute_coordinator::stake`). Off unless
/// `COVENANT_COMPUTE_MIN_STAKE` names an amount in the stake mint's base
/// units. A stake counts only while it stays locked past a lease's longest
/// window and the dispute window, so it is still there to slash.
fn build_stake_requirement(
    dispute_window_secs: u64,
) -> anyhow::Result<Option<covenant_compute_coordinator::StakeRequirement>> {
    let min_amount: u64 = env_parse(
        "COVENANT_COMPUTE_MIN_STAKE",
        0,
        "must be a u64 (base units)",
    )?;
    if min_amount == 0 {
        tracing::info!(
            "operator stake not required (set COVENANT_COMPUTE_MIN_STAKE to require one)"
        );
        return Ok(None);
    }
    let require = |key: &str| -> anyhow::Result<String> {
        std::env::var(key)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("{key} must be set when COVENANT_COMPUTE_MIN_STAKE is"))
    };
    let default_lock = covenant_compute_protocol::MAX_LEASE_DURATION_SECS + dispute_window_secs;
    Ok(Some(covenant_compute_coordinator::StakeRequirement {
        program_id: require("COVENANT_COMPUTE_STAKE_PROGRAM_ID")?,
        rpc_url: require("COVENANT_COMPUTE_STAKE_RPC_URL")?,
        min_amount,
        min_lock_remaining_secs: env_parse(
            "COVENANT_COMPUTE_MIN_STAKE_LOCK_SECS",
            default_lock,
            "must be a u64 (seconds)",
        )?,
    }))
}

/// On-chain lease meter selection (`covenant_compute_coordinator::onchain_meter`).
/// Off by default: unset — or `off` — leaves lease settlement exactly
/// as it is today, the coordinator's clock metering and the custodial
/// escrow settling, with no chain touched anywhere.
///
/// `noop` installs the recording meter, which exercises the seam
/// without a signer (a drill, and what the hermetic tests use).
/// `sidecar` is the real one, and like the payout backend every value
/// it needs is required with no fallback — a half-configured money path
/// should refuse to start rather than default onto the wrong network.
fn build_lease_meter() -> anyhow::Result<Option<Arc<dyn LeaseMeter>>> {
    let selection = std::env::var("COVENANT_COMPUTE_LEASE_METER").unwrap_or_default();
    match selection.trim() {
        "" | "off" => Ok(None),
        "noop" => Ok(Some(Arc::new(NoopLeaseMeter::new()))),
        "sidecar" => {
            let require = |key: &str| -> anyhow::Result<String> {
                std::env::var(key).map_err(|_| {
                    anyhow::anyhow!("{key} must be set when COVENANT_COMPUTE_LEASE_METER=sidecar")
                })
            };
            let renter_keypair_path = require("COVENANT_COMPUTE_LEASE_KEYPAIR")?;
            let coordinator_keypair_path = require("COVENANT_COMPUTE_LEASE_COORDINATOR_KEYPAIR")?;
            // The program will not let the renter meter their own lease.
            // One file for both keys hands that back, so refuse it here
            // rather than discover it in a settlement.
            anyhow::ensure!(
                renter_keypair_path != coordinator_keypair_path,
                "COVENANT_COMPUTE_LEASE_KEYPAIR and \
                 COVENANT_COMPUTE_LEASE_COORDINATOR_KEYPAIR must be different keys: the renter \
                 escrows and the coordinator meters, and one key doing both is a meter its own \
                 payer can under-report"
            );
            Ok(Some(Arc::new(SidecarLeaseMeter::new(
                SidecarLeaseMeterConfig {
                    signer_binary: PathBuf::from(require("COVENANT_COMPUTE_LEASE_SIGNER_BINARY")?),
                    program_id: require("COVENANT_COMPUTE_LEASE_PROGRAM_ID")?,
                    mint: require("COVENANT_COMPUTE_LEASE_MINT")?,
                    rpc_url: require("COVENANT_COMPUTE_LEASE_RPC_URL")?,
                    er_rpc_url: require("COVENANT_COMPUTE_LEASE_ER_RPC_URL")?,
                    er_validator: require("COVENANT_COMPUTE_LEASE_ER_VALIDATOR")?,
                    renter_keypair_path,
                    coordinator_keypair_path,
                },
            ))))
        }
        other => anyhow::bail!(
            "COVENANT_COMPUTE_LEASE_METER must be off, noop or sidecar (got {other:?})"
        ),
    }
}

/// Funding-source selection (C6). `organic` (the default) needs no
/// policy. `bootstrap` requires BOTH subsidy knobs, loudly: a
/// subsidized deployment with an unstated budget is the faucet
/// anti-pattern, so there are no defaults here — and with no policy
/// configured the escrow refuses every bootstrap hold anyway (the
/// kill-switch's off position).
fn build_funding() -> anyhow::Result<(
    covenant_compute_protocol::FundingSource,
    Option<covenant_compute_coordinator::SubsidyPolicy>,
)> {
    use covenant_compute_protocol::FundingSource;
    let source =
        std::env::var("COVENANT_COMPUTE_FUNDING_SOURCE").unwrap_or_else(|_| "organic".into());
    match source.as_str() {
        // An organic deployment can still open the subsidy switch a
        // crack: bootstrap-tagged spend exists there too (canary
        // probes), and it is refused outright unless BOTH subsidy
        // vars are set.
        "organic" => {
            let policy = match (
                env_parse_opt::<u32>("COVENANT_COMPUTE_SUBSIDY_MAX_RATIO_BPS", "must be a u32")?,
                env_parse_opt::<u64>("COVENANT_COMPUTE_SUBSIDY_FLOOR_MICRO_USDC", "must be a u64")?,
            ) {
                (Some(ratio_bps), Some(floor)) => {
                    let policy = covenant_compute_coordinator::SubsidyPolicy::new(ratio_bps, floor)
                        .map_err(|e| anyhow::anyhow!("subsidy policy rejected: {e}"))?;
                    tracing::info!(
                        max_ratio_bps = policy.max_ratio_bps(),
                        floor_micro_usdc = policy.floor_micro_usdc(),
                        "funding source: organic, with a subsidy policy for bootstrap-tagged spend"
                    );
                    Some(policy)
                }
                (None, None) => None,
                _ => anyhow::bail!(
                    "COVENANT_COMPUTE_SUBSIDY_MAX_RATIO_BPS and \
                     COVENANT_COMPUTE_SUBSIDY_FLOOR_MICRO_USDC must be set together"
                ),
            };
            Ok((FundingSource::Organic, policy))
        }
        "bootstrap" => {
            let ratio_bps: u32 =
                env_parse_opt("COVENANT_COMPUTE_SUBSIDY_MAX_RATIO_BPS", "must be a u32")?.context(
                    "COVENANT_COMPUTE_SUBSIDY_MAX_RATIO_BPS must be set when \
                     COVENANT_COMPUTE_FUNDING_SOURCE=bootstrap — the subsidy:organic ratio \
                     cap (<= 10000) is an operator decision, not a default",
                )?;
            let floor: u64 =
                env_parse_opt("COVENANT_COMPUTE_SUBSIDY_FLOOR_MICRO_USDC", "must be a u64")?
                    .context(
                        "COVENANT_COMPUTE_SUBSIDY_FLOOR_MICRO_USDC must be set when \
                 COVENANT_COMPUTE_FUNDING_SOURCE=bootstrap — the cold-start subsidy budget \
                 is an operator decision, not a default",
                    )?;
            let policy = covenant_compute_coordinator::SubsidyPolicy::new(ratio_bps, floor)
                .map_err(|e| anyhow::anyhow!("subsidy policy rejected: {e}"))?;
            tracing::info!(
                max_ratio_bps = ratio_bps,
                floor_micro_usdc = floor,
                "funding source: bootstrap (subsidy-capped, funding_source-tagged)"
            );
            Ok((FundingSource::Bootstrap, Some(policy)))
        }
        other => anyhow::bail!(
            "COVENANT_COMPUTE_FUNDING_SOURCE must be \"organic\" or \"bootstrap\", got {other:?}"
        ),
    }
}

const COORDINATOR_USAGE: &str = "\
covenant-compute-coordinator — matching, escrow and settlement service
for the Covenant compute network

Usage:
  covenant-compute-coordinator             boot and serve
  covenant-compute-coordinator --version   print the version

The service is configured entirely through the environment. Core knobs
(the full table is in the crate README):
COVENANT_COMPUTE_COORDINATOR_PORT, COVENANT_COMPUTE_COORDINATOR_HOME,
COVENANT_COMPUTE_FEE_BPS, COVENANT_COMPUTE_FUNDING_SOURCE,
COVENANT_COMPUTE_REQUIRE_PREFUNDED, COVENANT_COMPUTE_PAYOUT_BACKEND.
";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Help and version answer before any boot work — asking a question
    // must not mint a coordinator identity or open a journal; any other
    // argument refuses rather than silently booting a misconfigured
    // service.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--help" | "-h" | "help") => {
            print!("{COORDINATOR_USAGE}");
            return Ok(());
        }
        Some("--version" | "-V" | "version") => {
            println!("{} {}", env!("CARGO_BIN_NAME"), env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some(other) => anyhow::bail!(
            "unexpected argument {other:?} — this service is configured entirely through \
             the environment; run `--help` for the summary"
        ),
        None => {}
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "covenant_compute_coordinator=info".into()),
        )
        .init();

    let home = coordinator_home()?;
    std::fs::create_dir_all(&home).with_context(|| format!("create {}", home.display()))?;
    let identity =
        LocalIdentity::load_or_create(&home.join("identity.json"), "coordinator@compute")
            .context("load or create coordinator identity")?;
    tracing::info!(
        pubkey = %bs58::encode(identity.pubkey_bytes()).into_string(),
        "coordinator identity ready — pin this pubkey on every operator node"
    );

    // File-backed and hash-chained: reputation is derived from this
    // log, and a coordinator restart must not amnesty every operator's
    // fault history (or lose the release/fee trail buyers and
    // operators check their books against).
    let audit: Arc<dyn covenant_audit::AuditLog> = Arc::new(
        JsonlAuditLog::open(home.join("audit.jsonl"))
            .await
            .context("open coordinator audit log")?,
    );
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = build_payout()?;
    let (default_funding_source, subsidy_policy) = build_funding()?;
    // Inbound rail (A3), selected the way the payout backend is: all
    // three RAIL_ variables or none. With a rail, a buyer funds the
    // deposit account once (SPL transfer to the owner's ATA in the
    // configured mint, memo `compute-buyer:<buyer_pubkey>`) and
    // claims the signature at /federation/buyers/deposit. Prefunding
    // can still be turned on without a rail (deposits become
    // impossible — every organic job 402s), which is only useful for
    // drills, so warn.
    let rail: Option<Arc<dyn covenant_compute_coordinator::InboundRail>> = {
        let rpc_url = std::env::var("COVENANT_COMPUTE_RAIL_RPC_URL").ok();
        let owner = std::env::var("COVENANT_COMPUTE_RAIL_DEPOSIT_OWNER").ok();
        let mint = std::env::var("COVENANT_COMPUTE_RAIL_MINT").ok();
        match (rpc_url, owner, mint) {
            (Some(rpc_url), Some(owner), Some(mint)) => {
                let rail = covenant_compute_coordinator::SolanaRpcRail::new(rpc_url, owner, mint);
                tracing::info!(rail = %covenant_compute_coordinator::InboundRail::describe(&rail), "inbound rail configured");
                Some(Arc::new(rail))
            }
            (None, None, None) => None,
            _ => anyhow::bail!(
                "COVENANT_COMPUTE_RAIL_RPC_URL, COVENANT_COMPUTE_RAIL_DEPOSIT_OWNER and \
                 COVENANT_COMPUTE_RAIL_MINT must be set together (or not at all) — a \
                 half-configured rail would verify deposits against the wrong account"
            ),
        }
    };
    let require_prefunded_buyers = env_bool("COVENANT_COMPUTE_REQUIRE_PREFUNDED", false)?;
    match (require_prefunded_buyers, &rail) {
        (true, None) => tracing::warn!(
            "buyer prefunding ENFORCED but no inbound rail is configured — \
             deposits are impossible, every organic job will be refused with 402"
        ),
        (true, Some(_)) => tracing::info!("buyer prefunding enforced"),
        (false, _) => tracing::info!(
            "buyer prefunding NOT enforced — escrow holds are custodial promises \
             (set COVENANT_COMPUTE_REQUIRE_PREFUNDED=1 to enforce)"
        ),
    }
    // Marketplace fee (C7). Defaults to zero — taking a cut is a
    // deployment decision. Whatever is set here is disclosed in every
    // RegisterResponse, so operators price their asks knowing it.
    let fee_bps: u32 = env_parse(
        "COVENANT_COMPUTE_FEE_BPS",
        0,
        "must be a u32 (basis points)",
    )?;
    let fee = covenant_compute_protocol::MarketplaceFee::new(fee_bps)
        .map_err(|e| anyhow::anyhow!("COVENANT_COMPUTE_FEE_BPS rejected: {e}"))?;
    if fee.bps() > 0 {
        tracing::info!(
            fee_bps = fee.bps(),
            "marketplace fee ENABLED — withheld from each payout, disclosed at registration"
        );
    }
    // Referral partners (C8): `code=payout_address:share_bps`,
    // comma-separated. The share is carved out of the marketplace fee,
    // so partners without a fee configured earn nothing. Malformed
    // entries fail the boot — a half-parsed rev-share table silently
    // shorting a partner is worse than being down.
    let partners: std::collections::HashMap<String, covenant_compute_coordinator::PartnerConfig> =
        match std::env::var("COVENANT_COMPUTE_PARTNERS") {
            Ok(raw) => raw
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(|entry| {
                    let malformed =
                        || format!("COVENANT_COMPUTE_PARTNERS entry {entry:?} must be code=address:share_bps");
                    let (code, terms) = entry.split_once('=').with_context(malformed)?;
                    let (address, bps) = terms.rsplit_once(':').with_context(malformed)?;
                    let share_bps: u32 = bps.trim().parse().with_context(|| {
                        format!("COVENANT_COMPUTE_PARTNERS share_bps must be a u32 (got {bps:?})")
                    })?;
                    let partner = covenant_compute_coordinator::PartnerConfig::new(
                        address.to_string(),
                        share_bps,
                    )
                    .map_err(|e| {
                        anyhow::anyhow!("COVENANT_COMPUTE_PARTNERS entry {code:?} rejected: {e}")
                    })?;
                    tracing::info!(code, share_bps, "referral partner configured");
                    Ok((code.to_string(), partner))
                })
                .collect::<anyhow::Result<_>>()?,
            Err(_) => std::collections::HashMap::new(),
        };
    if !partners.is_empty() && fee.bps() == 0 {
        tracing::warn!(
            "referral partners configured but no marketplace fee — every share accrues zero"
        );
    }
    // Trust floor (C5). Zero (default) matches on price alone with
    // reputation as tie-break; above zero, proven-bad operators stop
    // winning organic work until canary passes rebuild their score.
    let min_operator_score_bps: u32 = env_parse(
        "COVENANT_COMPUTE_MIN_OPERATOR_SCORE_BPS",
        0,
        "must be a u32 (basis points)",
    )?;
    if min_operator_score_bps > 0 {
        tracing::info!(
            min_operator_score_bps,
            "matcher trust floor ENABLED — operators scoring below it win no organic work"
        );
    }
    // Stake floor (C5 phase 2), the score floor's sibling with money
    // behind it. Zero (default) matches without requiring a bond;
    // above zero, only operators with that much committed stake win
    // organic work.
    let min_bond_micro_usdc: u64 = env_parse(
        "COVENANT_COMPUTE_MIN_BOND_MICRO_USDC",
        0,
        "must be a u64 (micro-USDC)",
    )?;
    if min_bond_micro_usdc > 0 {
        tracing::info!(
            min_bond_micro_usdc,
            "matcher stake floor ENABLED — operators without that much committed bond \
             win no organic work"
        );
    }
    // Rate scaling for the stake floor on GPU-lease supply: a lease
    // operator pricing by the GPU-hour must have committed this many
    // hours of its advertised rate, so premium supply posts more stake
    // than cheap supply. Zero (default) keeps the flat floor for all.
    let min_bond_lease_hours: u64 = env_parse(
        "COVENANT_COMPUTE_MIN_BOND_LEASE_HOURS",
        0,
        "must be a u64 (GPU-hours of the advertised rate)",
    )?;
    if min_bond_lease_hours > 0 {
        tracing::info!(
            min_bond_lease_hours,
            "lease stake floor SCALES with the advertised GPU-hour rate — a lease \
             operator must bond this many hours of its own rate to win organic work"
        );
    }
    // How long an unbond matures before its refund pushes; the stake
    // stays slashable for the whole window.
    let unbond_secs: u64 = env_parse(
        "COVENANT_COMPUTE_UNBOND_SECS",
        24 * 60 * 60,
        "must be a u64 (seconds)",
    )?;
    // Dispute window (C4): how long after a job concludes its buyer
    // can still land a signed dispute. Zero refuses every dispute.
    let dispute_window_secs: u64 = env_parse(
        "COVENANT_COMPUTE_DISPUTE_WINDOW_SECS",
        24 * 60 * 60,
        "must be a u64 (seconds)",
    )?;
    if dispute_window_secs == 0 {
        tracing::warn!(
            "dispute window is 0 — every buyer dispute will be refused \
             (set COVENANT_COMPUTE_DISPUTE_WINDOW_SECS to enable)"
        );
    }
    // Volumetric backstops (C9), for a coordinator fronting traffic
    // directly. Unset = unlimited: behind a reverse proxy the per-IP
    // limits live there instead.
    let max_operators: Option<usize> = env_parse_opt(
        "COVENANT_COMPUTE_MAX_OPERATORS",
        "must be a positive integer",
    )?;
    let max_inflight_per_buyer: Option<usize> = env_parse_opt(
        "COVENANT_COMPUTE_MAX_INFLIGHT_PER_BUYER",
        "must be a positive integer",
    )?;
    require_positive_cap("COVENANT_COMPUTE_MAX_OPERATORS", max_operators)?;
    require_positive_cap(
        "COVENANT_COMPUTE_MAX_INFLIGHT_PER_BUYER",
        max_inflight_per_buyer,
    )?;
    if let Some(cap) = max_operators {
        tracing::info!(
            cap,
            "operator registry capped — new registrations past it get 503"
        );
    }
    if let Some(cap) = max_inflight_per_buyer {
        tracing::info!(
            cap,
            "per-buyer in-flight ceiling ENABLED — submissions past it get 429"
        );
    }
    // Wire-version floor (the deploy-skew defense): /federation/*
    // requests declaring an older protocol get 426 with both numbers
    // named. Zero (default) admits every client, versionless included;
    // raised only alongside a breaking wire change.
    let min_protocol: u32 = env_parse(
        "COVENANT_COMPUTE_MIN_PROTOCOL",
        0,
        "must be a u32 (wire version)",
    )?;
    if min_protocol > 0 {
        tracing::info!(
            min_protocol,
            speaks = covenant_compute_protocol::PROTOCOL_VERSION,
            "wire-version floor ENABLED — older clients get 426 on /federation/*"
        );
    }
    // Stale-offer re-match window: how long a delivered offer may sit
    // unaccepted before the sweep re-points the job at whoever the
    // matcher would pick now. Zero disables the heal.
    let reoffer_secs: u64 = env_parse(
        "COVENANT_COMPUTE_REOFFER_SECS",
        60,
        "must be a u64 (seconds)",
    )?;
    if reoffer_secs == 0 {
        tracing::warn!(
            "stale-offer re-matching is disabled — a job whose assignee never accepts \
             waits for its deadline refund (set COVENANT_COMPUTE_REOFFER_SECS to enable)"
        );
    }
    // Admin surface (today: recording out-of-band partner payouts).
    // Fail closed: without a token every admin call is 401.
    let admin_token = std::env::var("COVENANT_COMPUTE_ADMIN_TOKEN")
        .ok()
        .filter(|t| !t.trim().is_empty());
    match &admin_token {
        Some(_) => tracing::info!("admin surface ENABLED (bearer-token gated)"),
        None => {
            tracing::info!("admin surface disabled (set COVENANT_COMPUTE_ADMIN_TOKEN to enable)")
        }
    }
    // Operator work polls hold this long before answering empty. Any
    // fronting proxy's idle timeout must exceed it, or every quiet
    // poll dies as a proxy 5xx instead of a clean no-work answer —
    // size it just under the proxy's limit. Floored at 1s: a 0 turns the
    // long poll into an immediate empty answer, so every waiting operator
    // busy-polls this endpoint in a tight loop.
    let long_poll_secs: u64 = env_parse(
        "COVENANT_COMPUTE_LONG_POLL_SECS",
        30,
        "must be a u64 (seconds)",
    )?;
    // On-chain lease meter. Off unless a deployment names one; a
    // chain-settling meter pays the operator the whole metered charge
    // straight out of the vault, and `settle_lease` has no fee
    // recipient in its account set, so a marketplace fee would be
    // booked and never collected. Refuse the combination rather than
    // publish a revenue number nobody received.
    let lease_meter = build_lease_meter()?;
    let stake = build_stake_requirement(dispute_window_secs)?;
    match &lease_meter {
        Some(meter) => {
            anyhow::ensure!(
                fee.bps() == 0,
                "COVENANT_COMPUTE_LEASE_METER cannot run with COVENANT_COMPUTE_FEE_BPS={} — \
                 an on-chain lease settle pays the operator the whole metered charge and \
                 returns the rest to the buyer, so the fee would be booked but never collected",
                fee.bps()
            );
            tracing::info!(meter = %meter.describe(), "on-chain lease meter ENABLED");
        }
        None => tracing::info!(
            "on-chain lease meter disabled (set COVENANT_COMPUTE_LEASE_METER=noop|sidecar to \
             enable)"
        ),
    }
    // Public settlement proof feed. Off by default: every receipt read
    // stays buyer-gated until a deployment opts in. On, and paying out on
    // a chain, it serves each settled job's operator-signed receipt and
    // the payout that honored it, so anyone can verify the network paid
    // for the work — no buyer identity or job contents, only hashes.
    let public_proof_feed = env_bool("COVENANT_COMPUTE_PUBLIC_PROOF_FEED", false)?;
    if public_proof_feed {
        tracing::info!("public settlement proof feed ENABLED at /proof/receipts");
    }

    // Client-sealed secret vault. Off by default: on, the coordinator
    // stores per-owner ciphertext it cannot read and serves it back over
    // a signed request, durable beside the money journal.
    let vault_enabled = env_bool("COVENANT_COMPUTE_VAULT", false)?;
    if vault_enabled {
        tracing::info!("client-sealed secret vault ENABLED at /vault");
    }
    let vault_max_owners: Option<usize> = env_parse_opt(
        "COVENANT_COMPUTE_VAULT_MAX_OWNERS",
        "must be a positive integer",
    )?;
    require_positive_cap("COVENANT_COMPUTE_VAULT_MAX_OWNERS", vault_max_owners)?;

    let config = CoordinatorConfig {
        long_poll_timeout: Duration::from_secs(long_poll_secs.max(1)),
        lease_meter,
        stake,
        require_prefunded_buyers,
        default_funding_source,
        subsidy_policy,
        fee,
        partners,
        min_operator_score_bps,
        min_bond_micro_usdc,
        min_bond_lease_hours,
        unbond_window: Duration::from_secs(unbond_secs),
        admin_token,
        dispute_window: Duration::from_secs(dispute_window_secs),
        max_operators,
        max_inflight_per_buyer,
        reoffer_after: Duration::from_secs(reoffer_secs),
        min_protocol,
        public_proof_feed,
        vault_enabled,
        vault_max_owners,
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::with_journal(
        identity,
        config,
        reputation,
        payout,
        audit,
        &home.join("journal.jsonl"),
        rail,
    )
    .await
    .context("replay coordinator journal")?;
    // The meter's ledger is in memory; hand it back every lease the
    // replayed job book still shows running.
    let stake_refresh_secs: u64 = env_parse(
        "COVENANT_COMPUTE_STAKE_REFRESH_SECS",
        120,
        "must be a u64 (seconds)",
    )?;
    let _stake_refresh_handle = state.config().stake.as_ref().map(|requirement| {
        let interval = stake_refresh_secs.max(10);
        tracing::info!(
            min_amount = requirement.min_amount,
            min_lock_remaining_secs = requirement.min_lock_remaining_secs,
            program = %requirement.program_id,
            refresh_secs = interval,
            "operator stake REQUIRED: operators win work only while it is staked and locked"
        );
        covenant_compute_coordinator::spawn_periodic_stake_refresh(
            state.clone(),
            Duration::from_secs(interval),
        )
    });
    let adopted = covenant_compute_coordinator::adopt_live_leases(&state);
    if adopted > 0 {
        tracing::info!(adopted, "on-chain lease meter resumed leases still running");
    }
    let _sweep_handle = spawn_periodic_sweep(state.clone(), Duration::from_secs(10));

    // Payout-retry sweep: re-pushes released-but-unpaid jobs (a push
    // that failed, or a crash between release and push) until the
    // money actually moves. On by default — self-healing books are the
    // point — 0 disables for hermetic drills.
    let payout_retry_secs: u64 = env_parse(
        "COVENANT_COMPUTE_PAYOUT_RETRY_SECS",
        60,
        "must be a u64 (seconds)",
    )?;
    let _payout_retry_handle = if payout_retry_secs > 0 {
        Some(covenant_compute_coordinator::spawn_periodic_payout_retry(
            state.clone(),
            Duration::from_secs(payout_retry_secs),
        ))
    } else {
        tracing::warn!(
            "payout retry disabled (COVENANT_COMPUTE_PAYOUT_RETRY_SECS=0) — a failed \
             payout push stays owed until an operator intervenes"
        );
        None
    };

    // On-chain lease meter ticks. Off unless a cadence is set, and
    // pointless without a meter — a per-second meter wants a faster
    // cadence than either existing sweep, so it runs on its own.
    let lease_tick_secs: u64 = env_parse(
        "COVENANT_COMPUTE_LEASE_TICK_SECS",
        0,
        "must be a u64 (seconds)",
    )?;
    let _lease_meter_handle = if lease_tick_secs > 0 {
        if state.lease_meter().is_none() {
            tracing::warn!(
                "COVENANT_COMPUTE_LEASE_TICK_SECS is set but no lease meter is configured — \
                 the tick pass will find nothing to meter"
            );
        }
        tracing::info!(interval_secs = lease_tick_secs, "lease meter ticks ENABLED");
        Some(covenant_compute_coordinator::spawn_periodic_lease_meter(
            state.clone(),
            Duration::from_secs(lease_tick_secs),
        ))
    } else {
        tracing::info!(
            "lease meter ticks disabled (set COVENANT_COMPUTE_LEASE_TICK_SECS to enable)"
        );
        None
    };

    // Journal compaction: the journal writes a whole-state line per
    // phase change, so a long-lived deployment re-shrinks it on a slow
    // cadence (boot already compacted once inside the state build).
    let compact_secs: u64 = env_parse(
        "COVENANT_COMPUTE_JOURNAL_COMPACT_SECS",
        3_600,
        "must be a u64 (seconds)",
    )?;
    let _compaction_handle = match state.journal() {
        Some(journal) if compact_secs > 0 => {
            Some(covenant_compute_coordinator::spawn_periodic_compaction(
                journal,
                Duration::from_secs(compact_secs),
            ))
        }
        _ => {
            if compact_secs == 0 {
                tracing::warn!(
                    "journal compaction disabled (COVENANT_COMPUTE_JOURNAL_COMPACT_SECS=0) — \
                     the journal grows with every phase change until the next boot"
                );
            }
            None
        }
    };

    // Canary probes (C5). Off unless an interval is set — probe spend
    // is real money, and it is bootstrap-tagged, so a deployment also
    // needs the subsidy policy open or every probe hold is refused.
    let canary_interval_secs: u64 = env_parse(
        "COVENANT_COMPUTE_CANARY_INTERVAL_SECS",
        0,
        "must be a u64 (seconds)",
    )?;
    let _canary_handle = if canary_interval_secs > 0 {
        let canary_config = covenant_compute_coordinator::CanaryConfig {
            max_price_micro_usdc: env_parse(
                "COVENANT_COMPUTE_CANARY_MAX_PRICE_MICRO_USDC",
                10_000,
                "must be a u64 (micro-USDC)",
            )?,
            deadline_ms: env_parse(
                "COVENANT_COMPUTE_CANARY_DEADLINE_MS",
                120_000,
                "must be a u64 (ms)",
            )?,
        };
        anyhow::ensure!(
            canary_config.deadline_ms >= 1_000,
            "COVENANT_COMPUTE_CANARY_DEADLINE_MS must be at least 1000 (one second) — a probe \
             whose deadline is shorter than the time to deliver and serve it expires on arrival, \
             and a deadline-expired canary faults every honest operator it probes"
        );
        // No persistent canary key: probe buyer identities rotate per
        // probe (fingerprint hardening), and restart-judgeability rides
        // the audit chain's ComputeCanaryDispatched markers instead. A
        // canary_identity.json from an older build is simply unused.
        if state.config().subsidy_policy.is_none() {
            tracing::warn!(
                "canary probes enabled but no subsidy policy is set — every probe hold \
                 will be refused (probes are bootstrap-tagged by design)"
            );
        }
        tracing::info!(
            interval_secs = canary_interval_secs,
            max_price_micro_usdc = canary_config.max_price_micro_usdc,
            "canary probes ENABLED — bootstrap-tagged, subsidy-capped"
        );
        let prober = Arc::new(covenant_compute_coordinator::CanaryProber::new(
            state.clone(),
            canary_config,
        ));
        Some(covenant_compute_coordinator::spawn_periodic_canary(
            prober,
            Duration::from_secs(canary_interval_secs),
        ))
    } else {
        tracing::info!(
            "canary probes disabled (set COVENANT_COMPUTE_CANARY_INTERVAL_SECS to enable)"
        );
        None
    };

    // Redundancy sampling (C5). Off unless an interval is set, and
    // meaningful ONLY for deterministic batch workloads — enabling it
    // is this deployment's assertion that same input means same bytes
    // out for its batch population. Mirror spend is bootstrap-tagged
    // like the canary's, so it needs the subsidy policy open too.
    let redundancy_interval_secs: u64 = env_parse(
        "COVENANT_COMPUTE_REDUNDANCY_INTERVAL_SECS",
        0,
        "must be a u64 (seconds)",
    )?;
    let _redundancy_handle = if redundancy_interval_secs > 0 {
        let redundancy_config = covenant_compute_coordinator::RedundancyConfig {
            mirrors: env_parse("COVENANT_COMPUTE_REDUNDANCY_MIRRORS", 2, "must be a usize")?,
            max_price_micro_usdc: env_parse(
                "COVENANT_COMPUTE_REDUNDANCY_MAX_PRICE_MICRO_USDC",
                10_000,
                "must be a u64 (micro-USDC)",
            )?,
            sample_inference: env_bool("COVENANT_COMPUTE_REDUNDANCY_INFERENCE", false)?,
        };
        anyhow::ensure!(
            redundancy_config.mirrors >= 2,
            "COVENANT_COMPUTE_REDUNDANCY_MIRRORS must be at least 2 when redundancy sampling \
             is enabled — a strict-majority verdict needs the source plus two mirrors (three \
             receipts), so fewer can never fault a divergent operator yet still spend bootstrap \
             subsidy on every mirror dispatched"
        );
        if state.config().subsidy_policy.is_none() {
            tracing::warn!(
                "redundancy sampling enabled but no subsidy policy is set — every mirror \
                 hold will be refused (mirrors are bootstrap-tagged by design)"
            );
        }
        tracing::info!(
            interval_secs = redundancy_interval_secs,
            mirrors = redundancy_config.mirrors,
            max_price_micro_usdc = redundancy_config.max_price_micro_usdc,
            sample_inference = redundancy_config.sample_inference,
            "redundancy sampling ENABLED — receipt hashes cross-checked by strict \
             majority; batch always, and deterministic (temperature-0, seeded) \
             inference when COVENANT_COMPUTE_REDUNDANCY_INFERENCE=1; assumes the \
             sampled population reproduces byte-for-byte"
        );
        let sampler = Arc::new(covenant_compute_coordinator::RedundancySampler::new(
            state.clone(),
            redundancy_config,
        ));
        Some(covenant_compute_coordinator::spawn_periodic_redundancy(
            sampler,
            Duration::from_secs(redundancy_interval_secs),
        ))
    } else {
        tracing::info!(
            "redundancy sampling disabled (set COVENANT_COMPUTE_REDUNDANCY_INTERVAL_SECS to enable)"
        );
        None
    };

    let bind: IpAddr = env_parse(
        "COVENANT_COMPUTE_COORDINATOR_BIND_ADDR",
        IpAddr::from([127, 0, 0, 1]),
        "must be an IP address",
    )?;
    let port: u16 = env_parse(
        "COVENANT_COMPUTE_COORDINATOR_PORT",
        8720,
        "must be a port (u16)",
    )?;
    let addr = SocketAddr::new(bind, port);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    tracing::info!(%addr, version = env!("CARGO_PKG_VERSION"), "coordinator listening");

    // SIGTERM is how a supervisor (docker stop, systemd, a PaaS deploy)
    // asks nicely; without a handler tokio's runtime dies mid-write.
    // The journal makes a hard kill safe already — this just lets
    // in-flight responses finish and the exit code read as clean.
    let shutdown = async {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("install SIGTERM handler");
            tokio::select! {
                _ = ctrl_c => {},
                _ = term.recv() => {},
            }
        }
        #[cfg(not(unix))]
        {
            let _ = ctrl_c.await;
        }
        tracing::info!("shutdown signal received; draining in-flight requests");
    };

    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await
        .context("coordinator server exited")
}

#[cfg(test)]
mod tests {
    use super::{parse_bool, require_positive_cap};

    #[test]
    fn a_zero_cap_is_refused_by_name_but_unset_and_positive_pass() {
        // Unset (no cap) and any positive cap are fine.
        require_positive_cap("K", None).expect("uncapped is allowed");
        require_positive_cap("K", Some(1)).expect("a real cap is allowed");
        // 0 would turn every request away — refuse it, naming the knob.
        let err = require_positive_cap("COVENANT_COMPUTE_MAX_OPERATORS", Some(0)).unwrap_err();
        assert!(
            err.to_string().contains("COVENANT_COMPUTE_MAX_OPERATORS"),
            "names the variable: {err}"
        );
    }

    #[test]
    fn a_boolean_toggle_reads_every_common_spelling_and_refuses_the_rest() {
        for on in ["1", "true", "TRUE", "yes", " on "] {
            assert!(parse_bool("K", on).unwrap(), "{on:?} is on");
        }
        for off in ["0", "false", "No", "off"] {
            assert!(!parse_bool("K", off).unwrap(), "{off:?} is off");
        }
        // The bug this guards: an unrecognized value must fail the boot,
        // naming the variable, not silently pick the off branch.
        let err = parse_bool("COVENANT_COMPUTE_REQUIRE_PREFUNDED", "enabled").unwrap_err();
        assert!(
            err.to_string()
                .contains("COVENANT_COMPUTE_REQUIRE_PREFUNDED"),
            "names the variable: {err}"
        );
    }
}
