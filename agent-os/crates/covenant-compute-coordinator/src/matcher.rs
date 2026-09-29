//! v1 matching algorithm (design-02-federation.md §2.3): filter
//! registered operators by capability match, sort by price ascending,
//! tie-break by reputation descending, skip anyone not `Online`.
//! Deliberately unsophisticated — no auction, no sealed bids — mirroring
//! `covenant-router`'s own simple-is-fine posture for v1 routing.
//!
//! Uses [`covenant_compute_protocol::CapabilityProfile::satisfies`]
//! directly — the same function the operator node calls at admission —
//! so both sides of the network agree on what "matches" means without
//! duplicating the logic.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use covenant_compute_protocol::{
    canonical_model, CapabilityProfile, CapabilityRequirement, CapacityEntry, CapacityView,
    HardwareClass, JobKind, OperatorStatus, PriceUnit,
};

use crate::bond::OperatorBonds;
use crate::registry::{OperatorRecord, OperatorRegistry};
use crate::reputation::ReputationSource;

/// The standing half of matchability — Online, and seen within the
/// liveness window. Shared by [`select_operator`], [`capacity_view`],
/// and the reputation view's `matchable` verdict, so what "live" means
/// can never drift between the matcher and what the reads report.
pub(crate) fn in_standing(record: &OperatorRecord, now_ms: u64, cutoff_ms: u64) -> bool {
    record.status == OperatorStatus::Online
        && now_ms.saturating_sub(record.last_seen_ms) <= cutoff_ms
}

/// The operator's floor for this job, in whole-job micro-USDC — the
/// figure that has to fit inside the buyer's offer.
///
/// Every kind but a lease reads the ask as a per-job floor: its
/// `micro_usdc` is the whole-job price whatever unit it declares, so it
/// compares to the offer directly. A lease settles by meter, so a lease
/// operator may price by the GPU-hour instead; a `PerLeaseHour` ask is
/// scaled to the window the buyer signed for — rate x duration, rounded
/// up so the floor is never understated into a pay cut the operator
/// didn't agree to — and that scaled figure is what the offer must
/// cover. A lease priced any other way (a flat `PerJob` ask) still
/// floors on the whole window, unchanged.
fn operator_job_floor(profile: &CapabilityProfile, requirement: &CapabilityRequirement) -> u64 {
    profile
        .price
        .job_floor_micro_usdc(requirement.kind, requirement.max_duration_secs)
}

/// The committed stake an operator must hold to win organic work.
///
/// `flat` is the deployment's base floor, applied to every operator
/// whatever it serves. `lease_hours` scales that floor with the rate a
/// GPU-lease operator advertises: an operator pricing by the GPU-hour
/// must additionally cover `lease_hours` hours of its own advertised
/// rate, so the stake it posts grows with the money a renter trusts it
/// with. A cheap card and a premium one no longer post the same bond —
/// advertising expensive supply you cannot back is what gets priced out.
///
/// The effective requirement is the higher of the two, so the rate
/// scaling never lowers the base floor, and it touches only
/// `PerLeaseHour` asks — a per-job or per-token operator floors on
/// `flat` alone. `lease_hours` of `0` (the default) is the pre-existing
/// flat-only behavior exactly.
#[derive(Debug, Clone, Copy)]
pub struct BondFloor {
    flat_micro_usdc: u64,
    lease_hours: u64,
}

impl BondFloor {
    /// No floor at all — every operator is admitted whatever its stake.
    pub const OPEN: Self = Self {
        flat_micro_usdc: 0,
        lease_hours: 0,
    };

    pub fn new(flat_micro_usdc: u64, lease_hours: u64) -> Self {
        Self {
            flat_micro_usdc,
            lease_hours,
        }
    }

    /// A flat floor with no rate scaling — the whole gate before phase 2's
    /// lease scaling, and what every non-lease deployment still wants.
    pub fn flat(flat_micro_usdc: u64) -> Self {
        Self {
            flat_micro_usdc,
            lease_hours: 0,
        }
    }

    /// The committed stake this operator must hold to be matchable.
    /// Saturates rather than overflows: an absurdly-priced lease ask
    /// whose scaled floor exceeds `u64` simply cannot be met, which
    /// fails it closed.
    pub fn required_micro_usdc(&self, profile: &CapabilityProfile) -> u64 {
        let scaled = if profile.price.unit == PriceUnit::PerLeaseHour {
            profile.price.micro_usdc.saturating_mul(self.lease_hours)
        } else {
            0
        };
        self.flat_micro_usdc.max(scaled)
    }

    /// Whether `committed` stake clears this operator's effective floor.
    fn admits(&self, profile: &CapabilityProfile, committed: u64) -> bool {
        committed >= self.required_micro_usdc(profile)
    }
}

/// Returns the winning operator's pubkey_b58, or `None` if no live
/// `Online` operator's declared
/// [`covenant_compute_protocol::CapabilityProfile`] satisfies
/// `requirement` at a price within `offered_micro_usdc` with a
/// reputation score of at least `min_score_bps`.
///
/// Live means seen (register/heartbeat/long-poll) within
/// `liveness_cutoff` of `now_ms`: a node that crashed without an
/// Offline heartbeat would otherwise stay matchable until it happened
/// to re-register, eating jobs into a dead queue.
///
/// The ask is read as the operator's per-job floor: settlement pays the
/// envelope's price per job on success (the receipt prices the whole
/// job, whatever `PriceUnit` the ask declares), so matching an operator
/// asking above the offer would be a pay cut it never agreed to. The one
/// unit the matcher does convert is a lease priced by the GPU-hour:
/// because a lease settles by meter, a `PerLeaseHour` ask is scaled to
/// the buyer's window before it floors the offer (see
/// `operator_job_floor`). Other metered units (a per-token inference
/// ask) still floor the whole job — comparing them needs settlement to
/// price by that meter, which only the lease path does today.
///
/// The score floor (C5's trust gate) turns reputation from a tie-break
/// into an exclusion: a proven-bad operator stops winning organic work
/// on price alone. `0` disables it. Exclusion is not a death sentence —
/// canary probes pin their target instead of matching, so a floored
/// operator keeps getting probed, and passed probes put release rows
/// back on its record until it clears the floor again.
///
/// A buyer can raise the floor for their own job through
/// `requirement.min_reputation_bps` (signed into the envelope, so the
/// coordinator can't lower it): the effective floor is the higher of it
/// and `min_score_bps`, and a job that clears no operator at that floor
/// no-matches into the refund path rather than settling on a lesser one.
///
/// The bond floor (C5 phase 2) is the score floor's sibling with money
/// behind it: below its *committed* stake requirement — posted minus
/// slashed, refunded, and pending unbonds — an operator wins no organic
/// work, so a fresh sybil can't out-price its way into jobs and an
/// operator heading for the exit stops being matchable before its money
/// leaves. [`BondFloor`] carries the requirement: a flat base every
/// operator clears, plus a lease-rate scaling that makes a GPU-hour
/// operator's floor grow with the rate it advertises. [`BondFloor::OPEN`]
/// disables it; probes still pin floored operators.
///
/// `exclude_operator` drops one operator from consideration whatever
/// its standing — the stale-offer sweep's lever for an assignee that
/// looks alive to every gate here (registered, recent, cheap) while
/// demonstrably not picking up the very job being re-matched. `None`
/// for a fresh match.
#[allow(clippy::too_many_arguments)]
pub async fn select_operator(
    registry: &OperatorRegistry,
    reputation: &dyn ReputationSource,
    bonds: &OperatorBonds,
    requirement: &CapabilityRequirement,
    offered_micro_usdc: u64,
    now_ms: u64,
    liveness_cutoff: Duration,
    min_score_bps: u32,
    bond: BondFloor,
    exclude_operator: Option<&str>,
) -> Option<String> {
    let cutoff_ms = liveness_cutoff.as_millis() as u64;
    let candidates: Vec<_> = registry
        .snapshot()
        .into_iter()
        .filter(|(key, _)| exclude_operator != Some(key.as_str()))
        .filter(|(_, record)| {
            in_standing(record, now_ms, cutoff_ms)
                && operator_job_floor(&record.profile, requirement) <= offered_micro_usdc
                && record.profile.satisfies(requirement)
        })
        .filter(|(key, record)| {
            record.staked && bond.admits(&record.profile, bonds.status(key).committed_micro_usdc)
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }

    // One history read for the whole surviving pool, not one per
    // candidate — the score of an audit-derived source is otherwise a
    // full-log scan, and the tie-break needs every candidate's.
    let keys: Vec<&str> = candidates.iter().map(|(key, _)| key.as_str()).collect();
    let scores = reputation.scores(&keys).await;
    // The buyer's own floor rides in the signed requirement; the pool
    // must clear the higher of it and the coordinator's standing floor.
    // Reading it here costs nothing — the scores are already fetched for
    // the tie-break whatever the floors.
    let score_floor = min_score_bps.max(requirement.min_reputation_bps.unwrap_or(0));
    let mut scored = Vec::with_capacity(candidates.len());
    for (key, record) in candidates {
        let score = scores.get(key.as_str()).copied().unwrap_or(0);
        if score < score_floor {
            continue;
        }
        scored.push((key, operator_job_floor(&record.profile, requirement), score));
    }
    // Cheapest first, then best reputation, then — the tie-break that
    // actually decides the common case, since every operator ties at
    // zero under `NoReputation` — the operator pubkey. Without this last
    // key the winner falls out of `snapshot()`'s HashMap-iteration
    // order, which is randomized per process and reshuffles on rehash:
    // the same pool would pick different winners across restarts. A
    // stable key makes a match reproducible and auditable.
    scored.sort_by(|a, b| a.1.cmp(&b.1).then(b.2.cmp(&a.2)).then(a.0.cmp(&b.0)));
    scored.into_iter().next().map(|(key, _, _)| key)
}

/// Why [`select_operator`] found nothing, in the one dimension the buyer
/// can fix on the spot: price. Returns `Some(floor)` when at least one
/// operator that is `Online`, live, and past both the score and bond
/// floors serves this exact `requirement` but was left out only because
/// its per-job ask exceeds `offered_micro_usdc`; `floor` is the cheapest
/// such ask, the smallest offer that would have matched.
///
/// Call it only after [`select_operator`] returned `None`. That result
/// guarantees no capable, affordable, in-standing operator exists, so a
/// capable one that clears the floors necessarily asks above the offer,
/// and `Some(floor)` reads cleanly as "raise the offer to `floor`".
/// `None` means the block was capability or trust, not price: nothing a
/// higher offer resolves.
#[allow(clippy::too_many_arguments)]
pub async fn cheapest_capable_ask_above_offer(
    registry: &OperatorRegistry,
    reputation: &dyn ReputationSource,
    bonds: &OperatorBonds,
    requirement: &CapabilityRequirement,
    offered_micro_usdc: u64,
    now_ms: u64,
    liveness_cutoff: Duration,
    min_score_bps: u32,
    bond: BondFloor,
) -> Option<u64> {
    let cutoff_ms = liveness_cutoff.as_millis() as u64;
    // Capable, in-standing, past the bond floor, and asking above the
    // offer — the only operators a higher offer could reach. An ask
    // within the offer would have matched on price; its absence is a
    // trust or bond floor, which no higher offer fixes.
    let mut above: Vec<(String, u64)> = Vec::new();
    for (key, record) in registry.snapshot() {
        if !in_standing(&record, now_ms, cutoff_ms) || !record.profile.satisfies(requirement) {
            continue;
        }
        let ask = operator_job_floor(&record.profile, requirement);
        if ask <= offered_micro_usdc {
            continue;
        }
        if !record.staked || !bond.admits(&record.profile, bonds.status(&key).committed_micro_usdc)
        {
            continue;
        }
        above.push((key, ask));
    }
    // The effective floor is the higher of the coordinator's and the
    // buyer's own (signed into the requirement); the price diagnostic
    // must not name an ask from an operator the buyer's floor would
    // exclude anyway.
    let score_floor = min_score_bps.max(requirement.min_reputation_bps.unwrap_or(0));
    // The score floor reads the history once for the pool, not once per
    // operator — and only when a floor is actually in force.
    let scores = if score_floor > 0 {
        let keys: Vec<&str> = above.iter().map(|(key, _)| key.as_str()).collect();
        reputation.scores(&keys).await
    } else {
        HashMap::new()
    };
    let mut floor: Option<u64> = None;
    for (key, ask) in above {
        if score_floor > 0 && scores.get(key.as_str()).copied().unwrap_or(0) < score_floor {
            continue;
        }
        floor = Some(floor.map_or(ask, |f| f.min(ask)));
    }
    floor
}

/// The buyer-facing answer to "what can I purchase right now" — the
/// discovery read the submit path never needed but a stranger buyer
/// does: without it, knowing a model id and a workable offer price is
/// out-of-band homework. Aggregates the declared profiles of every
/// operator this matcher would actually consider (same standing gates,
/// same floors) into (kind, model) rows, so a row's `min_ask` is a
/// price at which [`select_operator`] genuinely starts returning
/// `Some` for that row — never advertised supply the matcher would
/// refuse.
///
/// Per-operator requirement gates (price vs. a specific offer,
/// `satisfies` vs. a specific job) are exactly what the buyer decides
/// with this view, so they don't filter it. Identities never appear:
/// counts and asks only, the anonymous-aggregate posture `/metrics`
/// set. Models are canonicalized ([`canonical_model`]) and deduped per
/// operator, so `m` and `m:latest` advertise as the one row they match
/// as.
pub async fn capacity_view(
    registry: &OperatorRegistry,
    reputation: &dyn ReputationSource,
    bonds: &OperatorBonds,
    now_ms: u64,
    liveness_cutoff: Duration,
    min_score_bps: u32,
    bond: BondFloor,
) -> CapacityView {
    struct Row {
        operators: usize,
        min_ask_micro_usdc: u64,
        min_ask_unit: PriceUnit,
        max_ask_micro_usdc: u64,
        max_vram_gb: u32,
        gpu_classes: Vec<String>,
        tee_capable: bool,
    }

    let cutoff_ms = liveness_cutoff.as_millis() as u64;
    let mut snapshot = registry.snapshot();
    let registered_operators = snapshot.len();
    // The fold visits operators in pubkey order so an equal-ask tie
    // resolves the min-ask unit the same way across restarts —
    // `snapshot()` iterates a HashMap, whose order is neither.
    snapshot.sort_by(|a, b| a.0.cmp(&b.0));

    // One history read for every operator that clears the standing and
    // bond gates, rather than a full-log scan per operator inside the
    // aggregation below — and only when a score floor is set.
    let scores = if min_score_bps > 0 {
        let keys: Vec<&str> = snapshot
            .iter()
            .filter(|(key, record)| {
                in_standing(record, now_ms, cutoff_ms)
                    && record.staked
                    && bond.admits(&record.profile, bonds.status(key).committed_micro_usdc)
            })
            .map(|(key, _)| key.as_str())
            .collect();
        reputation.scores(&keys).await
    } else {
        HashMap::new()
    };

    let mut matchable_operators = 0usize;
    let mut rows: BTreeMap<(JobKind, String), Row> = BTreeMap::new();
    for (key, record) in snapshot {
        if !in_standing(&record, now_ms, cutoff_ms) || !record.staked {
            continue;
        }
        if !bond.admits(&record.profile, bonds.status(&key).committed_micro_usdc) {
            continue;
        }
        if min_score_bps > 0 && scores.get(key.as_str()).copied().unwrap_or(0) < min_score_bps {
            continue;
        }
        matchable_operators += 1;

        let profile = &record.profile;
        let gpu_class = match &profile.hardware {
            HardwareClass::ConsumerGpu { model } | HardwareClass::DatacenterGpu { model } => {
                model.clone()
            }
            // The value `CapabilityProfile::satisfies` accepts as this
            // hardware's requestable `gpu_class`.
            HardwareClass::CpuOnly => "cpu".into(),
        };
        let mut models: Vec<&str> = profile
            .models_served
            .iter()
            .map(|m| canonical_model(m))
            .collect();
        models.sort_unstable();
        models.dedup();
        // Dedup the kinds too — a profile that names a kind twice must
        // not count its one operator into the same (kind, model) row
        // twice and advertise false redundancy for that pairing.
        let mut kinds = profile.job_kinds.clone();
        kinds.sort_unstable();
        kinds.dedup();

        for kind in &kinds {
            for model in &models {
                let row = rows
                    .entry((*kind, (*model).to_string()))
                    .or_insert_with(|| Row {
                        operators: 0,
                        min_ask_micro_usdc: profile.price.micro_usdc,
                        min_ask_unit: profile.price.unit,
                        max_ask_micro_usdc: profile.price.micro_usdc,
                        max_vram_gb: profile.vram_gb,
                        gpu_classes: Vec::new(),
                        tee_capable: false,
                    });
                row.operators += 1;
                if profile.price.micro_usdc < row.min_ask_micro_usdc {
                    row.min_ask_micro_usdc = profile.price.micro_usdc;
                    row.min_ask_unit = profile.price.unit;
                }
                row.max_ask_micro_usdc = row.max_ask_micro_usdc.max(profile.price.micro_usdc);
                row.max_vram_gb = row.max_vram_gb.max(profile.vram_gb);
                if !row.gpu_classes.contains(&gpu_class) {
                    row.gpu_classes.push(gpu_class.clone());
                }
                row.tee_capable |= profile.tee_capable;
            }
        }
    }

    let entries = rows
        .into_iter()
        .map(|((kind, model), mut row)| {
            row.gpu_classes.sort_unstable();
            CapacityEntry {
                kind,
                model,
                operators: row.operators,
                min_ask_micro_usdc: row.min_ask_micro_usdc,
                min_ask_unit: row.min_ask_unit,
                max_ask_micro_usdc: row.max_ask_micro_usdc,
                max_vram_gb: row.max_vram_gb,
                gpu_classes: row.gpu_classes,
                tee_capable: row.tee_capable,
            }
        })
        .collect();

    CapacityView {
        registered_operators,
        matchable_operators,
        liveness_window_ms: cutoff_ms,
        min_score_bps,
        min_bond_micro_usdc: bond.flat_micro_usdc,
        entries,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reputation::NoReputation;
    use async_trait::async_trait;
    use covenant_compute_protocol::{
        CapabilityProfile, HardwareClass, JobKind, PriceAsk, PriceUnit, RegisterRequest,
    };
    use covenant_identity::LocalIdentity;

    fn requirement() -> CapabilityRequirement {
        CapabilityRequirement {
            gpu_class: None,
            min_vram_gb: None,
            model_id: None,
            kind: JobKind::BatchJob,
            max_duration_secs: 30,
            min_reputation_bps: None,
        }
    }

    /// [`select_operator`] with no bond floor — every pre-phase-2
    /// matching behavior is asserted through this, so those tests state
    /// exactly what they always did.
    #[allow(clippy::too_many_arguments)]
    async fn select(
        registry: &OperatorRegistry,
        reputation: &dyn ReputationSource,
        requirement: &CapabilityRequirement,
        offered_micro_usdc: u64,
        now_ms: u64,
        liveness_cutoff: Duration,
        min_score_bps: u32,
    ) -> Option<String> {
        select_operator(
            registry,
            reputation,
            &OperatorBonds::new(),
            requirement,
            offered_micro_usdc,
            now_ms,
            liveness_cutoff,
            min_score_bps,
            BondFloor::OPEN,
            None,
        )
        .await
    }

    fn payout_for(label: &str) -> String {
        let mut key = [0u8; 32];
        for (i, b) in label.bytes().take(32).enumerate() {
            key[i] = b;
        }
        bs58::encode(key).into_string()
    }

    fn register(registry: &OperatorRegistry, display: &str, micro_usdc: u64) -> String {
        let identity = LocalIdentity::generate(display);
        let profile = CapabilityProfile {
            operator: identity.agent_id(),
            hardware: HardwareClass::CpuOnly,
            vram_gb: 0,
            models_served: vec!["any".into()],
            job_kinds: vec![JobKind::BatchJob],
            price: PriceAsk {
                unit: PriceUnit::PerJob,
                micro_usdc,
            },
            tee_capable: false,
        };
        let req = RegisterRequest::sign(profile, payout_for(display), &identity).unwrap();
        registry.register(&req, 0, None, false).unwrap();
        identity.agent_id().pubkey_base58()
    }

    #[tokio::test]
    async fn an_operator_without_the_stake_wins_nothing_even_when_cheapest() {
        let registry = OperatorRegistry::new();
        let cheap = register(&registry, "cheap-unstaked@local", 100);
        let staked = register(&registry, "staked@local", 500);
        registry.set_staked(&cheap, false);

        let winner = select(
            &registry,
            &NoReputation,
            &requirement(),
            1_000,
            0,
            Duration::from_secs(45),
            0,
        )
        .await
        .unwrap();
        assert_eq!(winner, staked, "price never buys past the stake gate");

        registry.set_staked(&staked, false);
        assert!(select(
            &registry,
            &NoReputation,
            &requirement(),
            1_000,
            0,
            Duration::from_secs(45),
            0,
        )
        .await
        .is_none());
    }

    #[tokio::test]
    async fn picks_the_cheapest_satisfying_operator() {
        let registry = OperatorRegistry::new();
        let cheap = register(&registry, "cheap@local", 100);
        let _expensive = register(&registry, "expensive@local", 500);

        let winner = select(
            &registry,
            &NoReputation,
            &requirement(),
            1_000,
            0,
            Duration::from_secs(45),
            0,
        )
        .await
        .unwrap();
        assert_eq!(winner, cheap);
    }

    #[tokio::test]
    async fn no_satisfying_operator_returns_none() {
        let registry = OperatorRegistry::new();
        register(&registry, "cpu-only@local", 100);

        let mut req = requirement();
        req.min_vram_gb = Some(80); // no registered operator has this much VRAM
        assert!(select(
            &registry,
            &NoReputation,
            &req,
            1_000,
            0,
            Duration::from_secs(45),
            0,
        )
        .await
        .is_none());
    }

    #[tokio::test]
    async fn an_excluded_operator_never_wins_even_as_the_cheapest() {
        let registry = OperatorRegistry::new();
        let cheap = register(&registry, "cheap@local", 100);
        let pricier = register(&registry, "pricier@local", 500);

        let winner = select_operator(
            &registry,
            &NoReputation,
            &OperatorBonds::new(),
            &requirement(),
            1_000,
            0,
            Duration::from_secs(45),
            0,
            BondFloor::OPEN,
            Some(cheap.as_str()),
        )
        .await
        .unwrap();
        assert_eq!(winner, pricier);

        // Excluding the only candidate is a no-match, not a fallback
        // onto the excluded one.
        assert!(select_operator(
            &registry,
            &NoReputation,
            &OperatorBonds::new(),
            &requirement(),
            400,
            0,
            Duration::from_secs(45),
            0,
            BondFloor::OPEN,
            Some(cheap.as_str()),
        )
        .await
        .is_none());
    }

    struct FixedReputation(std::collections::HashMap<String, u32>);

    #[async_trait]
    impl ReputationSource for FixedReputation {
        async fn score(&self, operator_pubkey_b58: &str) -> u32 {
            self.0.get(operator_pubkey_b58).copied().unwrap_or(0)
        }
    }

    #[tokio::test]
    async fn ties_on_price_break_by_reputation_descending() {
        let registry = OperatorRegistry::new();
        let low_rep = register(&registry, "low-rep@local", 100);
        let high_rep = register(&registry, "high-rep@local", 100);

        let mut scores = std::collections::HashMap::new();
        scores.insert(low_rep, 10);
        scores.insert(high_rep.clone(), 9_000);
        let reputation = FixedReputation(scores);

        let winner = select(
            &registry,
            &reputation,
            &requirement(),
            1_000,
            0,
            Duration::from_secs(45),
            0,
        )
        .await
        .unwrap();
        assert_eq!(winner, high_rep);
    }

    #[tokio::test]
    async fn a_full_tie_resolves_the_same_regardless_of_registration_order() {
        // Two operators identical on every ranked axis — same price,
        // both zero under NoReputation. The winner must come from the
        // deterministic pubkey tie-break, never the registry's
        // HashMap-iteration order, so registering them in either order
        // picks the same one.
        let a = LocalIdentity::generate("tie-a@local");
        let b = LocalIdentity::generate("tie-b@local");
        let expected = std::cmp::min(a.agent_id().pubkey_base58(), b.agent_id().pubkey_base58());

        let register_into = |order: [&LocalIdentity; 2]| {
            let registry = OperatorRegistry::new();
            for id in order {
                let profile = CapabilityProfile {
                    operator: id.agent_id(),
                    hardware: HardwareClass::CpuOnly,
                    vram_gb: 0,
                    models_served: vec!["any".into()],
                    job_kinds: vec![JobKind::BatchJob],
                    price: PriceAsk {
                        unit: PriceUnit::PerJob,
                        micro_usdc: 100,
                    },
                    tee_capable: false,
                };
                let req = RegisterRequest::sign(profile, payout_for("payout"), id).unwrap();
                registry.register(&req, 0, None, false).unwrap();
            }
            registry
        };

        for order in [[&a, &b], [&b, &a]] {
            let registry = register_into(order);
            let winner = select(
                &registry,
                &NoReputation,
                &requirement(),
                1_000,
                0,
                Duration::from_secs(45),
                0,
            )
            .await
            .unwrap();
            assert_eq!(
                winner, expected,
                "the pubkey tie-break must ignore registration order"
            );
        }
    }

    #[tokio::test]
    async fn an_operator_asking_above_the_offer_is_never_matched() {
        let registry = OperatorRegistry::new();
        let _expensive = register(&registry, "expensive@local", 5_000);

        assert!(
            select(
                &registry,
                &NoReputation,
                &requirement(),
                1_000,
                0,
                Duration::from_secs(45),
                0,
            )
            .await
            .is_none(),
            "matching above the ask would be a pay cut the operator never agreed to"
        );

        let at_the_offer = register(&registry, "at-the-offer@local", 1_000);
        let winner = select(
            &registry,
            &NoReputation,
            &requirement(),
            1_000,
            0,
            Duration::from_secs(45),
            0,
        )
        .await
        .unwrap();
        assert_eq!(winner, at_the_offer, "ask == offer is a valid match");
    }

    #[tokio::test]
    async fn an_operator_silent_past_the_liveness_cutoff_is_skipped() {
        let registry = OperatorRegistry::new();
        register(&registry, "crashed@local", 100); // last_seen_ms = 0

        let cutoff = Duration::from_secs(45);
        assert!(
            select(
                &registry,
                &NoReputation,
                &requirement(),
                1_000,
                45_001,
                cutoff,
                0,
            )
            .await
            .is_none(),
            "an operator silent past the cutoff is presumed crashed, not matchable"
        );
        assert!(
            select(
                &registry,
                &NoReputation,
                &requirement(),
                1_000,
                45_000,
                cutoff,
                0,
            )
            .await
            .is_some(),
            "exactly at the cutoff still counts as live"
        );
    }

    #[tokio::test]
    async fn offline_operators_are_skipped() {
        use covenant_compute_protocol::HeartbeatRequest;

        let registry = OperatorRegistry::new();
        let identity = LocalIdentity::generate("offline@local");
        let profile = CapabilityProfile {
            operator: identity.agent_id(),
            hardware: HardwareClass::CpuOnly,
            vram_gb: 0,
            models_served: vec!["any".into()],
            job_kinds: vec![JobKind::BatchJob],
            price: PriceAsk {
                unit: PriceUnit::PerJob,
                micro_usdc: 1,
            },
            tee_capable: false,
        };
        let req = RegisterRequest::sign(profile, payout_for("payout"), &identity).unwrap();
        registry.register(&req, 0, None, false).unwrap();
        let hb = HeartbeatRequest::sign(
            identity.agent_id(),
            OperatorStatus::Offline,
            0,
            1,
            &identity,
        )
        .unwrap();
        registry.heartbeat(&hb, 1).unwrap();

        assert!(select(
            &registry,
            &NoReputation,
            &requirement(),
            1_000,
            0,
            Duration::from_secs(45),
            0,
        )
        .await
        .is_none());
    }

    #[tokio::test]
    async fn the_trust_floor_excludes_proven_bad_operators_even_when_cheapest() {
        let registry = OperatorRegistry::new();
        let bad_but_cheap = register(&registry, "bad@local", 100);
        let good_but_pricier = register(&registry, "good@local", 500);

        let mut scores = std::collections::HashMap::new();
        // A record like 0 released / 4 faults: (0+1)/(0+4+2) = 1_666.
        scores.insert(bad_but_cheap.clone(), 1_666);
        scores.insert(good_but_pricier.clone(), 8_000);
        let reputation = FixedReputation(scores);

        let winner = select(
            &registry,
            &reputation,
            &requirement(),
            1_000,
            0,
            Duration::from_secs(45),
            2_000,
        )
        .await
        .unwrap();
        assert_eq!(
            winner, good_but_pricier,
            "below the floor, price no longer wins"
        );

        // Floor 0 is the tie-break-only behavior, unchanged.
        let winner = select(
            &registry,
            &reputation,
            &requirement(),
            1_000,
            0,
            Duration::from_secs(45),
            0,
        )
        .await
        .unwrap();
        assert_eq!(winner, bad_but_cheap);

        // Everyone under the floor: no match at all — the refund path,
        // not a bad operator by default.
        assert!(select(
            &registry,
            &reputation,
            &requirement(),
            1_000,
            0,
            Duration::from_secs(45),
            9_999,
        )
        .await
        .is_none());
    }

    #[tokio::test]
    async fn a_buyer_reputation_floor_excludes_a_below_floor_operator_the_coordinator_would_allow()
    {
        let registry = OperatorRegistry::new();
        let bad_but_cheap = register(&registry, "bad@local", 100);
        let good_but_pricier = register(&registry, "good@local", 500);

        let mut scores = std::collections::HashMap::new();
        scores.insert(bad_but_cheap.clone(), 3_000);
        scores.insert(good_but_pricier.clone(), 8_500);
        let reputation = FixedReputation(scores);

        // The coordinator sets no floor of its own; the buyer's rides in
        // the requirement and still excludes the cheap, worse-rated node.
        let mut req = requirement();
        req.min_reputation_bps = Some(8_000);
        let winner = select(
            &registry,
            &reputation,
            &req,
            1_000,
            0,
            Duration::from_secs(45),
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            winner, good_but_pricier,
            "the buyer's floor excludes the cheaper operator the coordinator would have taken"
        );

        // No floor: price wins again, unchanged.
        let winner = select(
            &registry,
            &reputation,
            &requirement(),
            1_000,
            0,
            Duration::from_secs(45),
            0,
        )
        .await
        .unwrap();
        assert_eq!(winner, bad_but_cheap);
    }

    #[tokio::test]
    async fn a_buyer_floor_above_every_operator_no_matches_into_the_refund_path() {
        let registry = OperatorRegistry::new();
        let only = register(&registry, "decent@local", 100);
        let reputation = FixedReputation(std::collections::HashMap::from([(only, 7_000)]));

        let mut req = requirement();
        req.min_reputation_bps = Some(9_000);
        assert!(
            select(
                &registry,
                &reputation,
                &req,
                1_000,
                0,
                Duration::from_secs(45),
                0,
            )
            .await
            .is_none(),
            "a floor no operator clears is a clean no-match, not a downgrade onto a lesser one"
        );
    }

    #[tokio::test]
    async fn the_effective_floor_is_the_higher_of_the_buyer_and_coordinator_floors() {
        let registry = OperatorRegistry::new();
        let mid = register(&registry, "mid@local", 100);
        let high = register(&registry, "high@local", 500);

        let mut scores = std::collections::HashMap::new();
        scores.insert(mid.clone(), 5_000);
        scores.insert(high.clone(), 9_000);
        let reputation = FixedReputation(scores);

        // Coordinator floor 2_000 would admit the cheap mid-rated node;
        // the buyer's 8_000 raises the bar past it to the high-rated one.
        let mut req = requirement();
        req.min_reputation_bps = Some(8_000);
        let winner = select(
            &registry,
            &reputation,
            &req,
            1_000,
            0,
            Duration::from_secs(45),
            2_000,
        )
        .await
        .unwrap();
        assert_eq!(winner, high);

        // A buyer floor below the coordinator's never lowers it: the
        // coordinator's 6_000 still excludes the 5_000 node.
        req.min_reputation_bps = Some(1_000);
        let winner = select(
            &registry,
            &reputation,
            &req,
            1_000,
            0,
            Duration::from_secs(45),
            6_000,
        )
        .await
        .unwrap();
        assert_eq!(
            winner, high,
            "the buyer's lower floor cannot undercut the coordinator's standing floor"
        );
    }

    #[tokio::test]
    async fn a_buyer_floored_operator_is_not_reported_as_a_price_gap() {
        let registry = OperatorRegistry::new();
        // The only capable operator asks above the offer and sits under
        // the buyer's floor: raising the offer would not reach it, so the
        // gap is trust, not price.
        let only = register(&registry, "capable-but-floored@local", 500);
        let reputation = FixedReputation(std::collections::HashMap::from([(only, 4_000)]));

        let mut req = requirement();
        req.min_reputation_bps = Some(8_000);
        assert_eq!(
            cheapest_capable_ask_above_offer(
                &registry,
                &reputation,
                &OperatorBonds::new(),
                &req,
                50,
                0,
                Duration::from_secs(45),
                0,
                BondFloor::OPEN,
            )
            .await,
            None,
            "an operator the buyer's floor excludes is not an actionable price gap"
        );

        // Lower the buyer floor under the operator's score and the same
        // ask becomes the actionable price gap.
        req.min_reputation_bps = Some(1_000);
        assert_eq!(
            cheapest_capable_ask_above_offer(
                &registry,
                &reputation,
                &OperatorBonds::new(),
                &req,
                50,
                0,
                Duration::from_secs(45),
                0,
                BondFloor::OPEN,
            )
            .await,
            Some(500)
        );
    }

    fn register_profile(
        registry: &OperatorRegistry,
        display: &str,
        now_ms: u64,
        build: impl FnOnce(CapabilityProfile) -> CapabilityProfile,
    ) -> String {
        let identity = LocalIdentity::generate(display);
        let base = CapabilityProfile {
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
        };
        let profile = build(base);
        let req = RegisterRequest::sign(profile, payout_for(display), &identity).unwrap();
        registry.register(&req, now_ms, None, false).unwrap();
        identity.agent_id().pubkey_base58()
    }

    #[tokio::test]
    async fn capacity_view_aggregates_only_matchable_operators() {
        use covenant_compute_protocol::HeartbeatRequest;

        let registry = OperatorRegistry::new();
        let now_ms = 50_000;
        let cutoff = Duration::from_secs(45);

        register_profile(&registry, "gpu@local", now_ms, |mut p| {
            p.hardware = HardwareClass::ConsumerGpu {
                model: "rtx-4090".into(),
            };
            p.vram_gb = 24;
            p.models_served = vec!["qwen:7b".into()];
            p.job_kinds = vec![JobKind::InferenceCall];
            p.price = PriceAsk {
                unit: PriceUnit::PerMillionTokens,
                micro_usdc: 400,
            };
            p
        });
        register_profile(&registry, "generalist@local", now_ms, |mut p| {
            p.models_served = vec!["qwen:7b".into(), "any".into()];
            p.job_kinds = vec![JobKind::InferenceCall, JobKind::BatchJob];
            p.price = PriceAsk {
                unit: PriceUnit::PerJob,
                micro_usdc: 900,
            };
            p.tee_capable = true;
            p
        });
        // Registered long ago, silent since: standing-gated out.
        register_profile(&registry, "stale@local", 0, |p| p);
        // Fresh but self-declared Offline: standing-gated out.
        let offline = LocalIdentity::generate("offline@local");
        let profile = CapabilityProfile {
            operator: offline.agent_id(),
            hardware: HardwareClass::CpuOnly,
            vram_gb: 0,
            models_served: vec!["any".into()],
            job_kinds: vec![JobKind::BatchJob],
            price: PriceAsk {
                unit: PriceUnit::PerJob,
                micro_usdc: 1,
            },
            tee_capable: false,
        };
        let req = RegisterRequest::sign(profile, payout_for("payout-offline"), &offline).unwrap();
        registry.register(&req, now_ms, None, false).unwrap();
        let hb = HeartbeatRequest::sign(
            offline.agent_id(),
            OperatorStatus::Offline,
            0,
            now_ms,
            &offline,
        )
        .unwrap();
        registry.heartbeat(&hb, now_ms).unwrap();

        let view = capacity_view(
            &registry,
            &NoReputation,
            &OperatorBonds::new(),
            now_ms,
            cutoff,
            0,
            BondFloor::OPEN,
        )
        .await;

        assert_eq!(view.registered_operators, 4);
        assert_eq!(view.matchable_operators, 2);
        assert_eq!(view.liveness_window_ms, 45_000);

        let keys: Vec<(JobKind, &str)> = view
            .entries
            .iter()
            .map(|e| (e.kind, e.model.as_str()))
            .collect();
        assert_eq!(
            keys,
            vec![
                (JobKind::InferenceCall, "any"),
                (JobKind::InferenceCall, "qwen:7b"),
                (JobKind::BatchJob, "any"),
                (JobKind::BatchJob, "qwen:7b"),
            ],
            "rows are sorted by kind then model, and neither excluded operator contributes"
        );

        let shared = &view.entries[1];
        assert_eq!(shared.operators, 2);
        assert_eq!(
            (shared.min_ask_micro_usdc, shared.min_ask_unit),
            (400, PriceUnit::PerMillionTokens),
            "the min ask carries the cheapest operator's declared unit"
        );
        assert_eq!(shared.max_ask_micro_usdc, 900);
        assert_eq!(shared.max_vram_gb, 24);
        assert_eq!(shared.gpu_classes, vec!["cpu", "rtx-4090"]);
        assert!(shared.tee_capable, "one TEE declaration marks the row");

        let generalist_only = &view.entries[0];
        assert_eq!(generalist_only.operators, 1);
        assert_eq!(generalist_only.gpu_classes, vec!["cpu"]);
        assert_eq!(generalist_only.max_vram_gb, 0);
    }

    #[tokio::test]
    async fn capacity_view_applies_the_same_floors_as_the_matcher() {
        let registry = OperatorRegistry::new();
        let low_rep = register_profile(&registry, "low-rep@local", 0, |p| p);
        let bonded = register_profile(&registry, "bonded@local", 0, |p| p);

        let mut scores = std::collections::HashMap::new();
        scores.insert(low_rep.clone(), 1_000);
        scores.insert(bonded.clone(), 9_000);
        let reputation = FixedReputation(scores);

        let bonds = OperatorBonds::new();
        bonds.credit_post("sig-1", &bonded, 50_000).unwrap();

        // Floors of 0 exclude nobody — even an unbonded, unscored pool
        // advertises, exactly as the matcher would consider it.
        let open = capacity_view(
            &registry,
            &NoReputation,
            &bonds,
            0,
            Duration::from_secs(45),
            0,
            BondFloor::OPEN,
        )
        .await;
        assert_eq!(open.matchable_operators, 2);

        let floored = capacity_view(
            &registry,
            &reputation,
            &bonds,
            0,
            Duration::from_secs(45),
            2_000,
            BondFloor::flat(50_000),
        )
        .await;
        assert_eq!(
            floored.matchable_operators, 1,
            "the score floor drops one, the bond floor would drop the other"
        );
        assert_eq!(floored.entries.len(), 1);
        assert_eq!(floored.entries[0].operators, 1);
        assert_eq!(floored.min_score_bps, 2_000);
        assert_eq!(floored.min_bond_micro_usdc, 50_000);

        // Both floors against both operators: an empty directory, not
        // an advertised pool the matcher would refuse.
        let empty = capacity_view(
            &registry,
            &FixedReputation(std::collections::HashMap::new()),
            &OperatorBonds::new(),
            0,
            Duration::from_secs(45),
            2_000,
            BondFloor::flat(50_000),
        )
        .await;
        assert_eq!(empty.matchable_operators, 0);
        assert!(empty.entries.is_empty());
        assert_eq!(empty.registered_operators, 2);
    }

    #[tokio::test]
    async fn capacity_view_canonicalizes_and_dedupes_served_models() {
        let registry = OperatorRegistry::new();
        register_profile(&registry, "tagged@local", 0, |mut p| {
            p.models_served = vec!["qwen2.5-coder".into(), "qwen2.5-coder:latest".into()];
            p
        });

        let view = capacity_view(
            &registry,
            &NoReputation,
            &OperatorBonds::new(),
            0,
            Duration::from_secs(45),
            0,
            BondFloor::OPEN,
        )
        .await;

        assert_eq!(
            view.entries.len(),
            1,
            "`m` and `m:latest` are the same purchasable row"
        );
        assert_eq!(view.entries[0].model, "qwen2.5-coder");
        assert_eq!(
            view.entries[0].operators, 1,
            "one operator advertising both spellings counts once"
        );
    }

    #[tokio::test]
    async fn capacity_view_counts_an_operator_once_per_kind_despite_a_duplicate() {
        let registry = OperatorRegistry::new();
        register_profile(&registry, "dup@local", 0, |mut p| {
            p.models_served = vec!["qwen:7b".into()];
            p.job_kinds = vec![JobKind::InferenceCall, JobKind::InferenceCall];
            p
        });

        let view = capacity_view(
            &registry,
            &NoReputation,
            &OperatorBonds::new(),
            0,
            Duration::from_secs(45),
            0,
            BondFloor::OPEN,
        )
        .await;

        assert_eq!(view.entries.len(), 1, "a repeated kind is still one row");
        assert_eq!(
            view.entries[0].operators, 1,
            "one operator naming a kind twice is not two operators of supply"
        );
    }

    #[tokio::test]
    async fn a_below_market_offer_reports_the_cheapest_reachable_ask() {
        let registry = OperatorRegistry::new();
        register(&registry, "cheap@local", 100);
        register(&registry, "pricier@local", 500);

        // An offer under both asks: the diagnostic points at the cheaper
        // one — the smallest offer that would have matched.
        assert_eq!(
            cheapest_capable_ask_above_offer(
                &registry,
                &NoReputation,
                &OperatorBonds::new(),
                &requirement(),
                50,
                0,
                Duration::from_secs(45),
                0,
                BondFloor::OPEN,
            )
            .await,
            Some(100),
            "the floor is the cheapest ask a higher offer would reach"
        );

        // No operator serves the shape at all: not a price problem, so no
        // floor to name.
        let mut demanding = requirement();
        demanding.min_vram_gb = Some(80);
        assert_eq!(
            cheapest_capable_ask_above_offer(
                &registry,
                &NoReputation,
                &OperatorBonds::new(),
                &demanding,
                1_000_000,
                0,
                Duration::from_secs(45),
                0,
                BondFloor::OPEN,
            )
            .await,
            None,
            "a capability mismatch is not something a higher offer fixes"
        );
    }

    #[tokio::test]
    async fn a_floored_operator_is_not_reported_as_a_price_gap() {
        let registry = OperatorRegistry::new();
        let only = register(&registry, "capable-but-floored@local", 500);

        let mut scores = std::collections::HashMap::new();
        scores.insert(only, 1_000);
        let reputation = FixedReputation(scores);

        // The only capable operator asks above the offer AND sits under
        // the score floor. Raising the offer would not match it, so the
        // gap is trust, not price: no floor is reported.
        assert_eq!(
            cheapest_capable_ask_above_offer(
                &registry,
                &reputation,
                &OperatorBonds::new(),
                &requirement(),
                50,
                0,
                Duration::from_secs(45),
                2_000,
                BondFloor::OPEN,
            )
            .await,
            None
        );

        // With the floor lifted, the same operator's ask is now the
        // actionable price gap.
        assert_eq!(
            cheapest_capable_ask_above_offer(
                &registry,
                &reputation,
                &OperatorBonds::new(),
                &requirement(),
                50,
                0,
                Duration::from_secs(45),
                0,
                BondFloor::OPEN,
            )
            .await,
            Some(500)
        );
    }

    #[tokio::test]
    async fn the_bond_floor_gates_on_committed_stake() {
        let registry = OperatorRegistry::new();
        let unbonded_cheap = register(&registry, "unbonded@local", 100);
        let bonded_pricier = register(&registry, "bonded@local", 500);

        let bonds = OperatorBonds::new();
        bonds.credit_post("sig-1", &bonded_pricier, 50_000).unwrap();

        let winner = select_operator(
            &registry,
            &NoReputation,
            &bonds,
            &requirement(),
            1_000,
            0,
            Duration::from_secs(45),
            0,
            BondFloor::flat(50_000),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            winner, bonded_pricier,
            "below the bond floor, price no longer wins"
        );

        // Floor 0 ignores stake entirely.
        let winner = select_operator(
            &registry,
            &NoReputation,
            &bonds,
            &requirement(),
            1_000,
            0,
            Duration::from_secs(45),
            0,
            BondFloor::OPEN,
            None,
        )
        .await
        .unwrap();
        assert_eq!(winner, unbonded_cheap);

        // A pending unbond drops the operator below the floor before
        // any money leaves — committed stake is what gates, not posted.
        bonds
            .request_unbond(crate::bond::UnbondState {
                unbond_id: uuid::Uuid::new_v4(),
                operator_pubkey_b58: bonded_pricier.clone(),
                recipient_address_b58: "exit-wallet".into(),
                amount_micro_usdc: 1,
                requested_at_ms: 1,
                matures_at_ms: u64::MAX,
                pushed: None,
            })
            .unwrap();
        assert!(select_operator(
            &registry,
            &NoReputation,
            &bonds,
            &requirement(),
            1_000,
            0,
            Duration::from_secs(45),
            0,
            BondFloor::flat(50_000),
            None,
        )
        .await
        .is_none());
    }

    fn lease_requirement(max_duration_secs: u32) -> CapabilityRequirement {
        CapabilityRequirement {
            gpu_class: None,
            min_vram_gb: None,
            model_id: None,
            kind: JobKind::LeaseSession,
            max_duration_secs,
            min_reputation_bps: None,
        }
    }

    /// Registers a lease-serving operator whose floor is priced by the
    /// GPU-hour; returns its pubkey.
    fn register_lease_hourly(
        registry: &OperatorRegistry,
        display: &str,
        micro_usdc_per_hour: u64,
    ) -> String {
        register_profile(registry, display, 0, |mut p| {
            p.job_kinds = vec![JobKind::LeaseSession];
            p.price = PriceAsk {
                unit: PriceUnit::PerLeaseHour,
                micro_usdc: micro_usdc_per_hour,
            };
            p
        })
    }

    #[tokio::test]
    async fn a_per_lease_hour_ask_is_scaled_to_the_buyers_window() {
        // 3_600_000 micro-USDC/GPU-hour is 1_000/sec. Over a 600s window
        // the operator's floor is 600_000 — the same whole-window figure
        // the buyer escrows for a 1_000/sec, 600s lease.
        let registry = OperatorRegistry::new();
        register_lease_hourly(&registry, "hourly@local", 3_600_000);

        let req = lease_requirement(600);
        assert!(
            select(
                &registry,
                &NoReputation,
                &req,
                600_000,
                0,
                Duration::from_secs(45),
                0
            )
            .await
            .is_some(),
            "an offer that covers the per-hour ask scaled to the window matches"
        );
        assert!(
            select(
                &registry,
                &NoReputation,
                &req,
                599_999,
                0,
                Duration::from_secs(45),
                0
            )
            .await
            .is_none(),
            "a micro under the scaled floor is a pay cut the operator never agreed to"
        );
    }

    #[tokio::test]
    async fn a_per_lease_hour_ask_scales_with_the_requested_duration() {
        // One hourly ask, priced correctly across two different windows —
        // the whole point of pricing a lease by the hour rather than by a
        // fixed whole-job figure that only fits one duration.
        let registry = OperatorRegistry::new();
        register_lease_hourly(&registry, "hourly@local", 3_600_000); // 1_000/sec

        let short = lease_requirement(300);
        assert!(select(
            &registry,
            &NoReputation,
            &short,
            300_000,
            0,
            Duration::from_secs(45),
            0
        )
        .await
        .is_some());
        assert!(select(
            &registry,
            &NoReputation,
            &short,
            299_999,
            0,
            Duration::from_secs(45),
            0
        )
        .await
        .is_none());

        let hour = lease_requirement(3_600);
        assert!(select(
            &registry,
            &NoReputation,
            &hour,
            3_600_000,
            0,
            Duration::from_secs(45),
            0
        )
        .await
        .is_some());
        assert!(select(
            &registry,
            &NoReputation,
            &hour,
            3_599_999,
            0,
            Duration::from_secs(45),
            0
        )
        .await
        .is_none());
    }

    #[tokio::test]
    async fn the_cheaper_per_hour_lease_operator_wins() {
        let registry = OperatorRegistry::new();
        let cheap = register_lease_hourly(&registry, "cheap@local", 1_800_000); // 500/sec
        let _pricey = register_lease_hourly(&registry, "pricey@local", 3_600_000); // 1_000/sec

        // A 600s window offering the pricier operator's full rate: both
        // fit, so the cheaper per-hour ask wins on the scaled floor.
        let req = lease_requirement(600);
        let winner = select(
            &registry,
            &NoReputation,
            &req,
            600_000,
            0,
            Duration::from_secs(45),
            0,
        )
        .await
        .unwrap();
        assert_eq!(winner, cheap, "the cheaper GPU-hour rate wins the lease");
    }

    #[tokio::test]
    async fn a_flat_per_job_lease_ask_is_refused_at_registration() {
        // A flat per-job ask floors the whole-window offer but not the
        // per-second rate a lease meters, so an early close would pay the
        // operator a fraction of what it priced. The registry refuses it at
        // registration, the same wire gate that rejects a per-second or
        // per-token lease unit — only a per-hour ask scales to the window.
        let registry = OperatorRegistry::new();
        let identity = LocalIdentity::generate("flat@local");
        let profile = CapabilityProfile {
            operator: identity.agent_id(),
            hardware: HardwareClass::CpuOnly,
            vram_gb: 0,
            models_served: vec!["any".into()],
            job_kinds: vec![JobKind::LeaseSession],
            price: PriceAsk {
                unit: PriceUnit::PerJob,
                micro_usdc: 60_000,
            },
            tee_capable: false,
        };
        let req = RegisterRequest::sign(profile, payout_for("flat@local"), &identity).unwrap();
        let err = registry
            .register(&req, 0, None, false)
            .expect_err("a per-job lease ask is refused at registration");
        assert!(
            err.to_string()
                .contains("lease-serving operator must price"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn per_hour_scaling_rounds_up_so_a_lease_never_underpays_the_operator() {
        // 5_000 micro/hour over 1s is 1.388.. micro — rounded up to 2, so
        // the operator's floor is never understated below its rate.
        let registry = OperatorRegistry::new();
        register_lease_hourly(&registry, "hourly@local", 5_000);

        let req = lease_requirement(1);
        assert!(select(
            &registry,
            &NoReputation,
            &req,
            2,
            0,
            Duration::from_secs(45),
            0
        )
        .await
        .is_some());
        assert!(
            select(
                &registry,
                &NoReputation,
                &req,
                1,
                0,
                Duration::from_secs(45),
                0
            )
            .await
            .is_none(),
            "the fractional micro rounds up, so an offer of 1 does not reach the floor"
        );
    }

    fn priced_profile(unit: PriceUnit, micro_usdc: u64) -> CapabilityProfile {
        CapabilityProfile {
            operator: LocalIdentity::generate("priced@local").agent_id(),
            hardware: HardwareClass::CpuOnly,
            vram_gb: 0,
            models_served: vec!["any".into()],
            job_kinds: vec![JobKind::LeaseSession],
            price: PriceAsk { unit, micro_usdc },
            tee_capable: false,
        }
    }

    #[test]
    fn a_bond_floor_scales_only_a_lease_hourly_ask() {
        let floor = BondFloor::new(1_000_000, 5);
        // A GPU-hour lease ask scales: 2_000_000/hr x 5h = 10_000_000,
        // above the flat base, so the requirement rises to it.
        let hourly = priced_profile(PriceUnit::PerLeaseHour, 2_000_000);
        assert_eq!(floor.required_micro_usdc(&hourly), 10_000_000);
        // A cheap hourly ask scales below the base — the base still holds.
        let cheap = priced_profile(PriceUnit::PerLeaseHour, 100_000);
        assert_eq!(floor.required_micro_usdc(&cheap), 1_000_000);
        // A per-job ask never scales, however dear.
        let per_job = priced_profile(PriceUnit::PerJob, 9_000_000);
        assert_eq!(floor.required_micro_usdc(&per_job), 1_000_000);
        // OPEN requires nothing; a flat floor never scales.
        assert_eq!(BondFloor::OPEN.required_micro_usdc(&hourly), 0);
        assert_eq!(
            BondFloor::flat(1_000_000).required_micro_usdc(&hourly),
            1_000_000
        );
        // An absurd rate saturates rather than overflowing, which fails it
        // closed: no committed stake can meet a u64::MAX requirement.
        let absurd = priced_profile(PriceUnit::PerLeaseHour, u64::MAX);
        assert_eq!(floor.required_micro_usdc(&absurd), u64::MAX);
    }

    #[tokio::test]
    async fn a_lease_operators_required_bond_scales_with_its_advertised_rate() {
        // One operator asking 3_600_000/GPU-hour (1_000/sec) with 10 USDC
        // of committed stake. A 600s window offering its full rate fits on
        // price; whether it wins turns entirely on the bond scaling.
        let registry = OperatorRegistry::new();
        let op = register_lease_hourly(&registry, "hourly@local", 3_600_000);
        let bonds = OperatorBonds::new();
        bonds.credit_post("sig-1", &op, 10_000_000).unwrap();
        let req = lease_requirement(600);

        let pick = |bond| {
            select_operator(
                &registry,
                &NoReputation,
                &bonds,
                &req,
                600_000,
                0,
                Duration::from_secs(45),
                0,
                bond,
                None,
            )
        };
        // 10 GPU-hours of cover is 36_000_000, above the 10 USDC posted, so
        // the operator wins nothing however affordable its ask.
        assert!(pick(BondFloor::new(0, 10)).await.is_none());
        // 2 hours of cover is 7_200_000, which its stake clears.
        assert_eq!(pick(BondFloor::new(0, 2)).await.unwrap(), op);
        // A flat floor at that same number never scales, so it clears too.
        assert_eq!(pick(BondFloor::flat(10_000_000)).await.unwrap(), op);
    }

    #[tokio::test]
    async fn capacity_view_hides_a_lease_operator_underbonded_for_its_rate() {
        // Two lease operators with identical stake. Once the floor scales,
        // the pricier one cannot back its own advertised rate, so the
        // directory stops advertising it — never supply the matcher refuses.
        let registry = OperatorRegistry::new();
        let cheap = register_lease_hourly(&registry, "cheap@local", 360_000); // 100/sec
        let pricey = register_lease_hourly(&registry, "pricey@local", 3_600_000); // 1_000/sec
        let bonds = OperatorBonds::new();
        bonds.credit_post("sig-cheap", &cheap, 5_000_000).unwrap();
        bonds.credit_post("sig-pricey", &pricey, 5_000_000).unwrap();

        // 10 hours of cover: cheap needs 3_600_000 (clears), pricey needs
        // 36_000_000 (does not), so only the cheap one advertises.
        let scaled = capacity_view(
            &registry,
            &NoReputation,
            &bonds,
            0,
            Duration::from_secs(45),
            0,
            BondFloor::new(0, 10),
        )
        .await;
        assert_eq!(scaled.matchable_operators, 1);
        // With no scaling both back the flat floor and both advertise.
        let flat = capacity_view(
            &registry,
            &NoReputation,
            &bonds,
            0,
            Duration::from_secs(45),
            0,
            BondFloor::flat(5_000_000),
        )
        .await;
        assert_eq!(flat.matchable_operators, 2);
    }
}
