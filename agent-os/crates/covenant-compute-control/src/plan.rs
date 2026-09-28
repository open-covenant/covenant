//! What a launch plan has to clear before any money moves. These are the
//! deterministic checks: the plan names a real app, matches the released
//! catalog, and pairs it with an offer that meets the app's floors at the
//! price the client was quoted. The one check that is not here — that the
//! offer is still live in the market — needs the current offer set and so
//! belongs to the service that holds it.

use crate::catalog::AppCatalog;
use crate::wire::{
    quote_maximum, AppAvailability, ComputeOffer, LaunchPlan, LaunchRequest, MIN_DURATION_SECS,
};
use thiserror::Error;

/// Why a launch plan was refused. Every arm names the field the caller
/// has to change; collapsing them would lose the only clue a first-time
/// caller gets. [`PlanRejection::code`] is the stable machine string a
/// client keys on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PlanRejection {
    #[error("launch plan names an app that is not in the catalog")]
    UnknownApp,
    #[error("launch plan does not match the released catalog")]
    CatalogMismatch,
    #[error("app is not released for launch")]
    AppUnavailable,
    #[error("duration_secs must be between {minimum_secs} and {maximum_secs}")]
    Duration {
        minimum_secs: u64,
        maximum_secs: u64,
    },
    #[error("the selected offer is offline")]
    OfferOffline,
    #[error("the selected offer has less GPU memory than the app requires")]
    GpuMemory,
    #[error("the selected offer is below the app's minimum trust class")]
    TrustClass,
    #[error("the offer rate cannot be priced for this duration")]
    OfferRate,
    #[error("maximum_usdc_micros must be {expected_usdc_micros} for this offer and duration")]
    Maximum { expected_usdc_micros: u64 },
}

impl PlanRejection {
    /// The machine-readable code a client sees, stable across message
    /// wording.
    pub fn code(self) -> &'static str {
        match self {
            Self::UnknownApp => "unknown_app",
            Self::CatalogMismatch => "invalid_launch_plan",
            Self::AppUnavailable => "app_unavailable",
            Self::Duration { .. } => "invalid_duration",
            Self::OfferOffline => "offer_offline",
            Self::GpuMemory => "insufficient_gpu_memory",
            Self::TrustClass => "insufficient_trust",
            Self::OfferRate => "invalid_offer_rate",
            Self::Maximum { .. } => "invalid_maximum_usdc_micros",
        }
    }
}

/// Checks a plan against the catalog and its own offer: the app is real
/// and unchanged, released with an image, the window is in range, and the
/// offer clears the app's floors at exactly the quoted ceiling. Leaves
/// the market-liveness check to the caller.
pub fn validate_plan(catalog: &AppCatalog, plan: &LaunchPlan) -> Result<(), PlanRejection> {
    let catalog_app = catalog
        .app(&plan.app.id)
        .map_err(|_| PlanRejection::UnknownApp)?;
    if catalog_app != &plan.app {
        return Err(PlanRejection::CatalogMismatch);
    }
    if plan.app.availability != AppAvailability::Available || plan.app.image.is_none() {
        return Err(PlanRejection::AppUnavailable);
    }
    if plan.duration_secs < MIN_DURATION_SECS || plan.duration_secs > plan.app.max_duration_secs {
        return Err(PlanRejection::Duration {
            minimum_secs: MIN_DURATION_SECS,
            maximum_secs: plan.app.max_duration_secs,
        });
    }
    if !plan.offer.online {
        return Err(PlanRejection::OfferOffline);
    }
    if plan.offer.gpu.vram_mib < plan.app.min_vram_mib {
        return Err(PlanRejection::GpuMemory);
    }
    if plan.offer.trust_class < plan.app.min_trust {
        return Err(PlanRejection::TrustClass);
    }
    let expected = quote_maximum(plan.offer.rate_usdc_micros_per_hour, plan.duration_secs)
        .ok_or(PlanRejection::OfferRate)?;
    if plan.maximum_usdc_micros != expected {
        return Err(PlanRejection::Maximum {
            expected_usdc_micros: expected,
        });
    }
    Ok(())
}

/// Turns a [`LaunchRequest`] into a committed [`LaunchPlan`] by screening
/// the live market and pricing the cheapest offer that clears every floor.
/// The app's own floors (GPU memory, trust class) and the caller's budget
/// and optional trust request all apply; the caller can only raise the
/// trust floor above the app's, never below it. The survivor is quoted at
/// the same rounded rate a launch is checked against, so the plan this
/// returns clears [`validate_plan`] by construction — the caller reviews
/// the concrete offer and ceiling, then commits it.
///
/// `offers` is the market as [`crate::service`] has already screened it for
/// malformed entries; a plan is refused only when nothing on it meets the
/// requirements, and an over-budget market names the price the cheapest
/// match would need so the caller knows what to raise.
pub fn resolve_plan(
    catalog: &AppCatalog,
    offers: &[ComputeOffer],
    request: &LaunchRequest,
) -> Result<LaunchPlan, ResolveRejection> {
    let app = catalog
        .app(&request.app_id)
        .map_err(|_| ResolveRejection::UnknownApp)?;
    if app.availability != AppAvailability::Available || app.image.is_none() {
        return Err(ResolveRejection::AppUnavailable);
    }
    if request.duration_secs < MIN_DURATION_SECS || request.duration_secs > app.max_duration_secs {
        return Err(ResolveRejection::Duration {
            minimum_secs: MIN_DURATION_SECS,
            maximum_secs: app.max_duration_secs,
        });
    }
    let trust_floor = app
        .min_trust
        .max(request.min_trust.unwrap_or(app.min_trust));

    let mut eligible: Vec<(u64, &ComputeOffer)> = offers
        .iter()
        .filter(|offer| {
            offer.online
                && offer.gpu.vram_mib >= app.min_vram_mib
                && offer.trust_class >= trust_floor
        })
        .filter_map(|offer| {
            quote_maximum(offer.rate_usdc_micros_per_hour, request.duration_secs)
                .map(|price| (price, offer))
        })
        .collect();
    if eligible.is_empty() {
        return Err(ResolveRejection::NoOfferMeetsRequirements);
    }
    eligible.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id.cmp(&b.1.id)));
    let (price, offer) = eligible[0];
    if price > request.max_usdc_micros {
        return Err(ResolveRejection::NoOfferWithinBudget {
            cheapest_usdc_micros: price,
        });
    }
    Ok(LaunchPlan {
        app: app.clone(),
        offer: offer.clone(),
        duration_secs: request.duration_secs,
        maximum_usdc_micros: price,
    })
}

/// Why a launch request could not be resolved into a plan. The first three
/// are faults in the request itself and share [`PlanRejection`]'s codes;
/// the last two describe the market — nothing meets the requirements, or
/// the cheapest match costs more than the caller allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ResolveRejection {
    #[error("launch request names an app that is not in the catalog")]
    UnknownApp,
    #[error("app is not released for launch")]
    AppUnavailable,
    #[error("duration_secs must be between {minimum_secs} and {maximum_secs}")]
    Duration {
        minimum_secs: u64,
        maximum_secs: u64,
    },
    #[error("no online offer meets the app's GPU memory and the requested trust class")]
    NoOfferMeetsRequirements,
    #[error(
        "the cheapest matching offer needs at least {cheapest_usdc_micros} micro-USDC for this window"
    )]
    NoOfferWithinBudget { cheapest_usdc_micros: u64 },
}

impl ResolveRejection {
    /// The machine-readable code a client sees, stable across message
    /// wording.
    pub fn code(self) -> &'static str {
        match self {
            Self::UnknownApp => "unknown_app",
            Self::AppUnavailable => "app_unavailable",
            Self::Duration { .. } => "invalid_duration",
            Self::NoOfferMeetsRequirements => "no_matching_offer",
            Self::NoOfferWithinBudget { .. } => "over_budget",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{ComputeOffer, GpuSpec, LaunchPlan, TrustClass};

    fn catalog() -> AppCatalog {
        AppCatalog::builtin()
    }

    fn workspace_offer() -> ComputeOffer {
        ComputeOffer {
            id: "offer-1".into(),
            gpu: GpuSpec {
                model: "rtx-4090".into(),
                vram_mib: 24_576,
                cuda_major: 12,
            },
            rate_usdc_micros_per_hour: 1_000_000,
            trust_class: TrustClass::Open,
            online: true,
        }
    }

    fn good_plan() -> LaunchPlan {
        let app = catalog().app("gpu-workspace").unwrap().clone();
        let duration_secs = 3_600;
        let offer = workspace_offer();
        let maximum_usdc_micros =
            quote_maximum(offer.rate_usdc_micros_per_hour, duration_secs).unwrap();
        LaunchPlan {
            app,
            offer,
            duration_secs,
            maximum_usdc_micros,
        }
    }

    #[test]
    fn a_coherent_plan_clears() {
        validate_plan(&catalog(), &good_plan()).unwrap();
    }

    #[test]
    fn every_floor_is_enforced_with_its_own_code() {
        let cat = catalog();

        let mut unknown = good_plan();
        unknown.app.id = "ghost".into();
        assert_eq!(
            validate_plan(&cat, &unknown).unwrap_err().code(),
            "unknown_app"
        );

        let mut drifted = good_plan();
        drifted.app.summary = "tampered".into();
        assert_eq!(
            validate_plan(&cat, &drifted).unwrap_err().code(),
            "invalid_launch_plan"
        );

        let mut short = good_plan();
        short.duration_secs = MIN_DURATION_SECS - 1;
        assert_eq!(
            validate_plan(&cat, &short).unwrap_err().code(),
            "invalid_duration"
        );

        let mut offline = good_plan();
        offline.offer.online = false;
        // Recompute the price so only online-ness is under test.
        offline.maximum_usdc_micros = quote_maximum(
            offline.offer.rate_usdc_micros_per_hour,
            offline.duration_secs,
        )
        .unwrap();
        assert_eq!(
            validate_plan(&cat, &offline).unwrap_err().code(),
            "offer_offline"
        );

        let mut starved = good_plan();
        starved.offer.gpu.vram_mib = 1_024;
        assert_eq!(
            validate_plan(&cat, &starved).unwrap_err().code(),
            "insufficient_gpu_memory"
        );

        let mut mispriced = good_plan();
        mispriced.maximum_usdc_micros += 1;
        assert_eq!(
            validate_plan(&cat, &mispriced).unwrap_err().code(),
            "invalid_maximum_usdc_micros"
        );

        // A hostile offer rate that overflows the window quote is refused
        // as unpriceable before the ceiling is even compared, so the guard
        // never hands a saturated price to the escrow.
        let mut unpriceable = good_plan();
        unpriceable.offer.rate_usdc_micros_per_hour = u64::MAX;
        assert_eq!(
            validate_plan(&cat, &unpriceable).unwrap_err().code(),
            "invalid_offer_rate"
        );
    }

    #[test]
    fn an_offer_below_the_apps_trust_floor_is_refused() {
        // Every built-in app floors at Open, the weakest class, so the trust
        // gate never fires against the built-in catalog. An app that demands
        // a stronger class refuses an Open offer with its own code — the one
        // floor the shared suite above cannot reach.
        let mut app = catalog().app("gpu-workspace").unwrap().clone();
        app.id = "confidential-workspace".into();
        app.min_trust = TrustClass::Attested;
        let cat = AppCatalog::new(vec![app.clone()]).unwrap();

        let offer = workspace_offer(); // trust_class: Open
        let duration_secs = 3_600;
        let plan = LaunchPlan {
            maximum_usdc_micros: quote_maximum(offer.rate_usdc_micros_per_hour, duration_secs)
                .unwrap(),
            app,
            offer,
            duration_secs,
        };
        assert_eq!(
            validate_plan(&cat, &plan).unwrap_err().code(),
            "insufficient_trust"
        );
    }

    fn offer(id: &str, rate: u64, vram_mib: u64, trust: TrustClass, online: bool) -> ComputeOffer {
        ComputeOffer {
            id: id.into(),
            gpu: GpuSpec {
                model: "rtx-4090".into(),
                vram_mib,
                cuda_major: 12,
            },
            rate_usdc_micros_per_hour: rate,
            trust_class: trust,
            online,
        }
    }

    fn request(
        duration_secs: u64,
        max_usdc_micros: u64,
        min_trust: Option<TrustClass>,
    ) -> LaunchRequest {
        LaunchRequest {
            app_id: "gpu-workspace".into(),
            duration_secs,
            max_usdc_micros,
            min_trust,
        }
    }

    #[test]
    fn resolve_picks_the_cheapest_eligible_offer_and_prices_it() {
        let offers = vec![
            offer("premium", 4_000_000, 81_920, TrustClass::Isolated, true),
            offer("value", 1_000_000, 24_576, TrustClass::Open, true),
            offer("mid", 2_000_000, 40_960, TrustClass::Open, true),
        ];
        let plan = resolve_plan(&catalog(), &offers, &request(3_600, 2_000_000, None)).unwrap();
        assert_eq!(plan.offer.id, "value");
        assert_eq!(plan.maximum_usdc_micros, 1_000_000);
        assert_eq!(plan.duration_secs, 3_600);
        // The plan a resolve produces always clears the launch validation.
        validate_plan(&catalog(), &plan).unwrap();
    }

    #[test]
    fn resolve_breaks_a_price_tie_by_offer_id() {
        // Two offers at the same rate: the choice must not depend on the
        // market's ordering, so it falls to the smaller id.
        let ordered = vec![
            offer("bbb", 1_000_000, 24_576, TrustClass::Open, true),
            offer("aaa", 1_000_000, 24_576, TrustClass::Open, true),
        ];
        let reversed: Vec<ComputeOffer> = ordered.iter().rev().cloned().collect();
        for market in [ordered, reversed] {
            let plan = resolve_plan(&catalog(), &market, &request(3_600, 2_000_000, None)).unwrap();
            assert_eq!(plan.offer.id, "aaa");
        }
    }

    #[test]
    fn resolve_honours_a_requested_trust_floor_above_the_apps() {
        // The app floors at Open, but the caller demands Isolated, so the
        // cheaper Open offer is filtered out and the Isolated one is chosen.
        let offers = vec![
            offer("open-cheap", 1_000_000, 24_576, TrustClass::Open, true),
            offer("isolated", 2_000_000, 24_576, TrustClass::Isolated, true),
        ];
        let plan = resolve_plan(
            &catalog(),
            &offers,
            &request(3_600, 3_000_000, Some(TrustClass::Isolated)),
        )
        .unwrap();
        assert_eq!(plan.offer.id, "isolated");
        assert_eq!(plan.offer.trust_class, TrustClass::Isolated);
    }

    #[test]
    fn resolve_screens_out_offline_starved_and_weak_offers() {
        let offers = vec![
            offer("offline", 500_000, 24_576, TrustClass::Open, false),
            offer("starved", 500_000, 8_192, TrustClass::Open, true),
            offer("eligible", 1_000_000, 24_576, TrustClass::Open, true),
        ];
        let plan = resolve_plan(&catalog(), &offers, &request(3_600, 2_000_000, None)).unwrap();
        assert_eq!(plan.offer.id, "eligible");
    }

    #[test]
    fn resolve_drops_an_unpriceable_offer_without_letting_it_win_or_starve_the_market() {
        // An offer priced so high the window quote overflows u64 is
        // unpriceable. It must be dropped from the eligible set — never
        // selected and handed a saturated ceiling to escrow, and never left to
        // deny a valid cheaper offer sitting beside it.
        let offers = vec![
            offer("poison", u64::MAX, 24_576, TrustClass::Open, true),
            offer("value", 1_000_000, 24_576, TrustClass::Open, true),
        ];
        let plan = resolve_plan(&catalog(), &offers, &request(3_600, 2_000_000, None)).unwrap();
        assert_eq!(plan.offer.id, "value");
        assert_eq!(plan.maximum_usdc_micros, 1_000_000);

        // A market whose only matching supply is unpriceable resolves to a
        // clean no-match, even against a ceiling wide enough to admit a
        // saturated price — proof the overflow is dropped, not quoted.
        let err = resolve_plan(
            &catalog(),
            &[offer("poison", u64::MAX, 24_576, TrustClass::Open, true)],
            &request(3_600, u64::MAX, None),
        )
        .unwrap_err();
        assert_eq!(err, ResolveRejection::NoOfferMeetsRequirements);
    }

    #[test]
    fn resolve_reports_an_empty_market_when_nothing_clears_the_floors() {
        // Every offer is below the requested trust floor.
        let offers = vec![
            offer("a", 1_000_000, 24_576, TrustClass::Open, true),
            offer("b", 1_000_000, 24_576, TrustClass::Isolated, true),
        ];
        let err = resolve_plan(
            &catalog(),
            &offers,
            &request(3_600, 5_000_000, Some(TrustClass::Attested)),
        )
        .unwrap_err();
        assert_eq!(err, ResolveRejection::NoOfferMeetsRequirements);
        assert_eq!(err.code(), "no_matching_offer");

        // An empty market resolves the same way.
        assert_eq!(
            resolve_plan(&catalog(), &[], &request(3_600, 5_000_000, None)).unwrap_err(),
            ResolveRejection::NoOfferMeetsRequirements
        );
    }

    #[test]
    fn resolve_names_the_price_the_cheapest_match_needs_when_over_budget() {
        let offers = vec![
            offer("a", 3_000_000, 24_576, TrustClass::Open, true),
            offer("b", 2_000_000, 24_576, TrustClass::Open, true),
        ];
        // One hour of the cheapest is 2_000_000; a 1_000_000 budget cannot
        // reach it, and the rejection says exactly what it would take.
        let err = resolve_plan(&catalog(), &offers, &request(3_600, 1_000_000, None)).unwrap_err();
        assert_eq!(
            err,
            ResolveRejection::NoOfferWithinBudget {
                cheapest_usdc_micros: 2_000_000
            }
        );
        assert_eq!(err.code(), "over_budget");
    }

    #[test]
    fn resolve_rejects_a_bad_request_before_it_reaches_the_market() {
        let offers = vec![offer("a", 1_000_000, 24_576, TrustClass::Open, true)];

        let mut unknown = request(3_600, 2_000_000, None);
        unknown.app_id = "ghost".into();
        assert_eq!(
            resolve_plan(&catalog(), &offers, &unknown)
                .unwrap_err()
                .code(),
            "unknown_app"
        );

        let mut previewed = request(3_600, 2_000_000, None);
        previewed.app_id = "comfyui".into();
        assert_eq!(
            resolve_plan(&catalog(), &offers, &previewed)
                .unwrap_err()
                .code(),
            "app_unavailable"
        );

        let short = request(MIN_DURATION_SECS - 1, 2_000_000, None);
        assert_eq!(
            resolve_plan(&catalog(), &offers, &short)
                .unwrap_err()
                .code(),
            "invalid_duration"
        );
    }

    #[test]
    fn a_preview_app_cannot_be_launched() {
        // Naming a released app but pointing at a previewed one is a
        // catalog mismatch; a plan that faithfully carries a previewed
        // app is refused as unavailable.
        let cat = catalog();
        let previewed = cat.app("comfyui").unwrap().clone();
        let offer = workspace_offer();
        let duration_secs = previewed.default_duration_secs;
        let plan = LaunchPlan {
            maximum_usdc_micros: quote_maximum(offer.rate_usdc_micros_per_hour, duration_secs)
                .unwrap(),
            app: previewed,
            offer,
            duration_secs,
        };
        assert_eq!(
            validate_plan(&cat, &plan).unwrap_err().code(),
            "app_unavailable"
        );
    }
}
