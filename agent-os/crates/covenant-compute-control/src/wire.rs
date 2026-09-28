//! The control plane's wire contract: the apps a caller can launch, the
//! GPU offers that run them, the launch plan that commits one, and the
//! job a running session is read back as. These shapes are the customer
//! API; a workspace client is written against them, so their field names
//! and JSON forms are load-bearing and change only with the client.

use std::cmp::Ordering;

use covenant_compute_protocol::MAX_LEASE_DURATION_SECS;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Shortest bookable window. Allocating a GPU and bringing the workspace
/// up takes minutes; a session that expired inside that window would
/// bill for a machine the customer never reached.
pub const MIN_DURATION_SECS: u64 = 300;

/// What an app does with the GPU it is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppKind {
    Workspace,
    Image,
    Chat,
    Agent,
}

/// Whether an app can be launched now or is only previewed in the
/// catalog. A launch is refused for anything not `Available`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppAvailability {
    Available,
    Preview,
}

/// The isolation an offer runs under, ordered weakest to strongest. An
/// app names the least it will accept; an offer clears the app when its
/// class is at or above that floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustClass {
    Open,
    Isolated,
    Attested,
    Confidential,
}

impl TrustClass {
    fn rank(self) -> u8 {
        match self {
            Self::Open => 0,
            Self::Isolated => 1,
            Self::Attested => 2,
            Self::Confidential => 3,
        }
    }
}

impl PartialOrd for TrustClass {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TrustClass {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank().cmp(&other.rank())
    }
}

/// One catalog entry: a runnable image and the resources it needs. The
/// duration and budget fields are defaults a client can present before a
/// customer overrides them; the floors (`min_vram_mib`, `min_trust`) are
/// hard and screen the offers a launch may pair the app with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeApp {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub kind: AppKind,
    pub availability: AppAvailability,
    pub image: Option<String>,
    pub min_vram_mib: u64,
    pub min_trust: TrustClass,
    pub default_duration_secs: u64,
    pub max_duration_secs: u64,
    pub default_max_usdc_micros: u64,
}

impl ComputeApp {
    /// Checks the entry is internally coherent before it can be served or
    /// launched: a routable id, a duration contract that is launchable as a
    /// lease and leaves room for the default, a non-zero budget, and — once
    /// released — a digest-pinned image so a launch runs the exact bytes the
    /// catalog named.
    pub fn validate(&self) -> Result<(), ComputeError> {
        if self.id.is_empty()
            || !self
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(ComputeError::InvalidAppId(self.id.clone()));
        }
        // The window must be one a lease can actually run: below the
        // bookable floor no launch clears plan validation, and above the
        // lease cap every launch is rejected at the protocol. Bounding it
        // here fails a misconfigured catalog at load with a clear reason,
        // not at launch with an opaque provider rejection.
        if self.default_duration_secs < MIN_DURATION_SECS
            || self.max_duration_secs > MAX_LEASE_DURATION_SECS
            || self.default_duration_secs > self.max_duration_secs
        {
            return Err(ComputeError::InvalidAppDuration(self.id.clone()));
        }
        if self.default_max_usdc_micros == 0 {
            return Err(ComputeError::InvalidAppBudget(self.id.clone()));
        }
        if let Some(image) = &self.image {
            validate_digest_pinned_image(image)?;
        }
        if self.availability == AppAvailability::Available && self.image.is_none() {
            return Err(ComputeError::MissingAppImage(self.id.clone()));
        }
        Ok(())
    }
}

/// The GPU an offer puts on the market.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuSpec {
    pub model: String,
    pub vram_mib: u64,
    pub cuda_major: u16,
}

/// A quotable unit of GPU supply: a specific card at an hourly rate under
/// a trust class, and whether it can be booked right now. The `id` is how
/// a launch plan names the exact offer it priced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeOffer {
    pub id: String,
    pub gpu: GpuSpec,
    pub rate_usdc_micros_per_hour: u64,
    pub trust_class: TrustClass,
    pub online: bool,
}

/// A client's request to plan a launch: an app, how long, and the most
/// it may spend. Resolved into a concrete [`LaunchPlan`] against live
/// offers before anything is committed.
///
/// Rejects an unknown field rather than dropping it: a misspelled
/// `min_trust` would otherwise fall to the app's own floor and resolve a
/// weaker-isolation offer than the caller asked for, instead of the honest
/// refusal the correct spelling earns — the same fail-loud posture the paid
/// [`BetaCredential`](crate::auth::BetaCredential) and speech inputs take.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchRequest {
    pub app_id: String,
    pub duration_secs: u64,
    pub max_usdc_micros: u64,
    #[serde(default)]
    pub min_trust: Option<TrustClass>,
}

/// The committed launch: the exact app and offer the customer agreed to,
/// the window, and the ceiling. The app and offer are carried whole so
/// the plan the customer saw is the plan the control plane checks —
/// both are compared against the live catalog and market, and a launch
/// is refused if either drifted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchPlan {
    pub app: ComputeApp,
    pub offer: ComputeOffer,
    pub duration_secs: u64,
    pub maximum_usdc_micros: u64,
}

/// Where a session is in its life. A launch answers `provisioning` and
/// moves through `running` and `stopping` to a terminal
/// `completed`/`cancelled`/`failed`; `funding` means the spend is
/// reserved but the machine has not been reached yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Funding,
    Provisioning,
    Running,
    Stopping,
    Completed,
    Cancelled,
    Failed,
}

impl JobStatus {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }
}

/// The settled account of a finished session: what ran, for how long,
/// what it cost, and what came back. `runtime_secs` is the whole billed
/// window, counted from the moment a provider takes the session through
/// to close, so it includes the bring-up before the workspace first
/// answers. The customer pays for that window at the session rate.
/// `provisioning_secs` and `provisioning_usdc_micros` report bring-up a
/// provider absorbs rather than bills; no provider absorbs it today, so
/// both read zero and the whole window settles at the session rate.
/// `commitment` and `transaction` tie the charge to its settlement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeReceipt {
    pub id: String,
    pub job_id: String,
    pub app_id: String,
    pub provider: String,
    pub runtime_secs: u64,
    #[serde(default)]
    pub provisioning_secs: u64,
    #[serde(default)]
    pub provisioning_usdc_micros: u64,
    pub charged_usdc_micros: u64,
    pub refunded_usdc_micros: u64,
    pub commitment: String,
    pub transaction: Option<String>,
}

/// A launched session as a client reads it. `access_url` carries the
/// workspace credential and is present only on the response that just
/// obtained it — never in a list and never once the job is terminal.
/// `receipt` fills in when the session settles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeJob {
    pub id: String,
    pub app_id: String,
    pub offer_id: String,
    pub status: JobStatus,
    pub maximum_usdc_micros: u64,
    pub access_url: Option<String>,
    pub error: Option<String>,
    pub receipt: Option<ComputeReceipt>,
}

/// The ceiling a launch escrows: the hourly rate over the window, rounded
/// up to the whole micro-USDC, so a partial final hour is never
/// under-quoted. `None` when the rate and window overflow `u64` — an
/// unpriceable offer, not a saturation.
pub fn quote_maximum(rate_usdc_micros_per_hour: u64, duration_secs: u64) -> Option<u64> {
    rate_usdc_micros_per_hour
        .checked_mul(duration_secs)
        .and_then(|value| value.checked_add(3_599))
        .and_then(|value| value.checked_div(3_600))
}

/// A released image must name a repository and a full 32-byte sha256
/// digest, so a launch runs the exact bytes the catalog pinned rather
/// than whatever a mutable tag resolves to at boot.
pub(crate) fn validate_digest_pinned_image(image: &str) -> Result<(), ComputeError> {
    let Some((repository, digest)) = image.rsplit_once("@sha256:") else {
        return Err(ComputeError::ImageNotPinned);
    };
    if repository.is_empty() || digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(ComputeError::ImageNotPinned);
    }
    Ok(())
}

/// Why a catalog entry was rejected. Each arm names the app at fault so a
/// misconfigured catalog fails to load with the reason, not a blanket
/// "invalid".
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ComputeError {
    #[error("invalid compute app id: {0}")]
    InvalidAppId(String),
    #[error("compute app has an invalid duration contract: {0}")]
    InvalidAppDuration(String),
    #[error("compute app has an invalid budget contract: {0}")]
    InvalidAppBudget(String),
    #[error("compute app is missing a release image: {0}")]
    MissingAppImage(String),
    #[error("duplicate compute app: {0}")]
    DuplicateApp(String),
    #[error("unknown compute app: {0}")]
    UnknownApp(String),
    #[error("container image must be pinned to a sha256 digest")]
    ImageNotPinned,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn released_app() -> ComputeApp {
        ComputeApp {
            id: "gpu-workspace".into(),
            name: "GPU Workspace".into(),
            summary: "A bounded CUDA workspace.".into(),
            kind: AppKind::Workspace,
            availability: AppAvailability::Available,
            image: Some(format!("docker.io/nvidia/cuda@sha256:{}", "a".repeat(64))),
            min_vram_mib: 16_384,
            min_trust: TrustClass::Open,
            default_duration_secs: 1_800,
            max_duration_secs: 21_600,
            default_max_usdc_micros: 500_000,
        }
    }

    #[test]
    fn an_app_window_must_be_launchable_as_a_lease() {
        assert!(released_app().validate().is_ok());

        // Above the lease cap: a resolved plan would clear plan validation
        // and then be rejected at the protocol, so it is refused at load.
        let mut too_long = released_app();
        too_long.max_duration_secs = MAX_LEASE_DURATION_SECS + 1;
        assert!(matches!(
            too_long.validate(),
            Err(ComputeError::InvalidAppDuration(_))
        ));

        // Below the bookable floor: no window would ever launch.
        let mut too_short = released_app();
        too_short.default_duration_secs = MIN_DURATION_SECS - 1;
        assert!(matches!(
            too_short.validate(),
            Err(ComputeError::InvalidAppDuration(_))
        ));

        // A ceiling under the floor leaves an empty valid range.
        let mut unlaunchable = released_app();
        unlaunchable.default_duration_secs = MIN_DURATION_SECS;
        unlaunchable.max_duration_secs = MIN_DURATION_SECS - 1;
        assert!(matches!(
            unlaunchable.validate(),
            Err(ComputeError::InvalidAppDuration(_))
        ));
    }

    #[test]
    fn trust_classes_order_weakest_to_strongest() {
        assert!(TrustClass::Open < TrustClass::Isolated);
        assert!(TrustClass::Isolated < TrustClass::Attested);
        assert!(TrustClass::Attested < TrustClass::Confidential);
    }

    #[test]
    fn job_status_terminality_matches_the_contract() {
        for terminal in [
            JobStatus::Completed,
            JobStatus::Cancelled,
            JobStatus::Failed,
        ] {
            assert!(terminal.terminal());
        }
        for live in [
            JobStatus::Funding,
            JobStatus::Provisioning,
            JobStatus::Running,
            JobStatus::Stopping,
        ] {
            assert!(!live.terminal());
        }
    }

    #[test]
    fn status_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(JobStatus::Provisioning).unwrap(),
            serde_json::json!("provisioning")
        );
        assert_eq!(
            serde_json::to_value(TrustClass::Confidential).unwrap(),
            serde_json::json!("confidential")
        );
        assert_eq!(
            serde_json::to_value(AppKind::Workspace).unwrap(),
            serde_json::json!("workspace")
        );
    }

    #[test]
    fn a_job_round_trips_through_json() {
        let job = ComputeJob {
            id: "11111111-1111-4111-8111-111111111111".into(),
            app_id: "gpu-workspace".into(),
            offer_id: "offer-7".into(),
            status: JobStatus::Running,
            maximum_usdc_micros: 250_000,
            access_url: Some("ssh renter@203.0.113.7 -p 2222".into()),
            error: None,
            receipt: None,
        };
        let round =
            serde_json::from_value::<ComputeJob>(serde_json::to_value(&job).unwrap()).unwrap();
        assert_eq!(round, job);
    }

    // The round trip above proves the type is symmetric with itself, but a
    // renamed or dropped key survives it — both ends move together. The keys
    // are the customer contract: a deployed workspace client reads exactly
    // these names, so pin the serialized shape whole. A change here is a
    // change a client must ship for, and this is what makes that deliberate.
    #[test]
    fn a_settled_job_serializes_to_the_pinned_customer_shape() {
        let job = ComputeJob {
            id: "11111111-1111-4111-8111-111111111111".into(),
            app_id: "gpu-workspace".into(),
            offer_id: "offer-7".into(),
            status: JobStatus::Completed,
            maximum_usdc_micros: 250_000,
            access_url: None,
            error: None,
            receipt: Some(ComputeReceipt {
                id: "lease-7".into(),
                job_id: "11111111-1111-4111-8111-111111111111".into(),
                app_id: "gpu-workspace".into(),
                provider: "covenant".into(),
                runtime_secs: 1_800,
                provisioning_secs: 0,
                provisioning_usdc_micros: 0,
                charged_usdc_micros: 125_000,
                refunded_usdc_micros: 125_000,
                commitment: "commit-7".into(),
                transaction: None,
            }),
        };
        assert_eq!(
            serde_json::to_value(&job).unwrap(),
            serde_json::json!({
                "id": "11111111-1111-4111-8111-111111111111",
                "app_id": "gpu-workspace",
                "offer_id": "offer-7",
                "status": "completed",
                "maximum_usdc_micros": 250_000,
                "access_url": null,
                "error": null,
                "receipt": {
                    "id": "lease-7",
                    "job_id": "11111111-1111-4111-8111-111111111111",
                    "app_id": "gpu-workspace",
                    "provider": "covenant",
                    "runtime_secs": 1_800,
                    "provisioning_secs": 0,
                    "provisioning_usdc_micros": 0,
                    "charged_usdc_micros": 125_000,
                    "refunded_usdc_micros": 125_000,
                    "commitment": "commit-7",
                    "transaction": null,
                }
            })
        );
    }

    #[test]
    fn an_offer_serializes_to_the_pinned_customer_shape() {
        let offer = ComputeOffer {
            id: "offer-7".into(),
            gpu: GpuSpec {
                model: "rtx-4090".into(),
                vram_mib: 24_576,
                cuda_major: 0,
            },
            rate_usdc_micros_per_hour: 500_000,
            trust_class: TrustClass::Open,
            online: true,
        };
        assert_eq!(
            serde_json::to_value(&offer).unwrap(),
            serde_json::json!({
                "id": "offer-7",
                "gpu": {
                    "model": "rtx-4090",
                    "vram_mib": 24_576,
                    "cuda_major": 0,
                },
                "rate_usdc_micros_per_hour": 500_000,
                "trust_class": "open",
                "online": true,
            })
        );
    }

    #[test]
    fn an_app_serializes_to_the_pinned_customer_shape() {
        assert_eq!(
            serde_json::to_value(released_app()).unwrap(),
            serde_json::json!({
                "id": "gpu-workspace",
                "name": "GPU Workspace",
                "summary": "A bounded CUDA workspace.",
                "kind": "workspace",
                "availability": "available",
                "image": format!("docker.io/nvidia/cuda@sha256:{}", "a".repeat(64)),
                "min_vram_mib": 16_384,
                "min_trust": "open",
                "default_duration_secs": 1_800,
                "max_duration_secs": 21_600,
                "default_max_usdc_micros": 500_000,
            })
        );
    }

    #[test]
    fn quote_rounds_a_partial_hour_up() {
        // One hour at 1_000_000/hr is exact.
        assert_eq!(quote_maximum(1_000_000, 3_600), Some(1_000_000));
        // Half an hour rounds the half-micro up to the whole.
        assert_eq!(quote_maximum(1_000_000, 1_800), Some(500_000));
        // A single second still costs its rounded-up share, never zero.
        assert_eq!(quote_maximum(3_600_000, 1), Some(1_000));
        assert_eq!(quote_maximum(1, 1), Some(1));
        // An overflowing product is unpriceable, not saturated.
        assert_eq!(quote_maximum(u64::MAX, 2), None);
    }

    #[test]
    fn a_released_app_validates_and_its_floors_are_enforced() {
        released_app().validate().unwrap();

        let mut bad_id = released_app();
        bad_id.id = "Not Valid".into();
        assert!(matches!(
            bad_id.validate(),
            Err(ComputeError::InvalidAppId(_))
        ));

        let mut unpinned = released_app();
        unpinned.image = Some("docker.io/nvidia/cuda:latest".into());
        assert!(matches!(
            unpinned.validate(),
            Err(ComputeError::ImageNotPinned)
        ));

        let mut released_without_image = released_app();
        released_without_image.image = None;
        assert!(matches!(
            released_without_image.validate(),
            Err(ComputeError::MissingAppImage(_))
        ));

        let mut bad_duration = released_app();
        bad_duration.default_duration_secs = bad_duration.max_duration_secs + 1;
        assert!(matches!(
            bad_duration.validate(),
            Err(ComputeError::InvalidAppDuration(_))
        ));

        // A zero default duration is as malformed as one past the ceiling:
        // an app that defaults to no window at all cannot be launched.
        let mut zero_duration = released_app();
        zero_duration.default_duration_secs = 0;
        assert!(matches!(
            zero_duration.validate(),
            Err(ComputeError::InvalidAppDuration(_))
        ));

        // A zero default budget cannot price a launch, so the catalog
        // refuses to load it.
        let mut zero_budget = released_app();
        zero_budget.default_max_usdc_micros = 0;
        assert!(matches!(
            zero_budget.validate(),
            Err(ComputeError::InvalidAppBudget(_))
        ));
    }
}
