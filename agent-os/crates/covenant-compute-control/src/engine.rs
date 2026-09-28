//! The engine-backed provider: the control plane as a buyer client of the
//! coordinator's lease engine. Each launch opens a lease, each read pulls
//! the meter back, and the coordinator's capacity directory is projected
//! into the offers a customer browses. A customer never sees a lease; it
//! sees a [`ComputeJob`], and this module is where a lease becomes one.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use async_trait::async_trait;
use covenant_compute_buyer::{
    cancel_job, capacity, close_lease, http_client_with_timeout, lease_view, submit_streaming,
    BuyerConfig, CapacityView, JobRequest,
};
use covenant_compute_protocol::{
    lease_input, CancelView, JobKind, LeaseTerms, LeaseView, PriceUnit, LEASE_DEADLINE_SLACK_MS,
};
use covenant_identity::LocalIdentity;
use uuid::Uuid;

use crate::provider::{ProviderBackend, ProviderError, ProviderJob};
use crate::wire::{ComputeOffer, ComputeReceipt, GpuSpec, JobStatus, LaunchPlan, TrustClass};

/// A lease settles per second, so the control plane offers only rates that
/// divide evenly into a whole micro-USDC per second — an operator's
/// per-hour ask is rounded up to the next such rate. This keeps the
/// per-hour price a customer is quoted exactly equal to the per-second
/// escrow the lease opens with: `quote_maximum(rate, secs)` and
/// `(rate / 3600) * secs` agree when `rate` is a multiple of 3600.
const SETTLEMENT_GRANULARITY_PER_HOUR: u64 = 3_600;

/// `launch` opens a lease then reads it back — two sequential coordinator
/// calls that run inside the web layer's request timeout. Budgeting each
/// call at a third of that timeout keeps the pair, plus a whole call's
/// slack, inside it: a stalled coordinator then surfaces as an error and
/// `ControlPlane::submit` releases the reservation it took, rather than the
/// request being dropped mid-call with the reservation left holding the
/// owner's spend cap until a restart.
const LAUNCH_CALL_TIMEOUT: Duration = Duration::from_secs(crate::web::REQUEST_TIMEOUT_SECS / 3);

/// The engine-backed [`ProviderBackend`]. Holds the control plane's own
/// buyer identity, which funds every lease; its balance on the coordinator
/// is the ceiling on what all its beta owners can spend at once.
pub struct EngineProvider {
    http: reqwest::Client,
    config: BuyerConfig,
    identity: LocalIdentity,
    max_price_micro_usdc: u64,
    min_reputation_bps: Option<u32>,
}

impl EngineProvider {
    pub fn new(
        config: BuyerConfig,
        identity: LocalIdentity,
        max_price_micro_usdc: u64,
        min_reputation_bps: Option<u32>,
    ) -> Self {
        Self {
            http: http_client_with_timeout(LAUNCH_CALL_TIMEOUT),
            config,
            identity,
            max_price_micro_usdc,
            min_reputation_bps,
        }
    }
}

#[async_trait]
impl ProviderBackend for EngineProvider {
    async fn offers(&self) -> Result<Vec<ComputeOffer>, ProviderError> {
        let view = capacity(&self.http, &self.config)
            .await
            .map_err(|error| unavailable("read the market", error))?;
        Ok(offers_from_capacity(&view))
    }

    async fn launch(&self, job_id: Uuid, plan: &LaunchPlan) -> Result<ProviderJob, ProviderError> {
        let terms = lease_terms(plan);
        let price = terms
            .validate()
            .and_then(|()| terms.max_price_micro_usdc())
            .map_err(|_| ProviderError::Rejected)?;
        if price > self.max_price_micro_usdc {
            return Err(ProviderError::Rejected);
        }
        let request = JobRequest {
            kind: JobKind::LeaseSession,
            input: vec![lease_input(terms).map_err(|_| ProviderError::Rejected)?],
            model: None,
            gpu_class: Some(plan.offer.id.clone()),
            min_vram_gb: Some(vram_gb(plan.app.min_vram_mib)),
            min_reputation_bps: self.min_reputation_bps,
            price_micro_usdc: price,
            deadline_ms: plan
                .duration_secs
                .saturating_mul(1_000)
                .saturating_add(LEASE_DEADLINE_SLACK_MS),
        };
        submit_streaming(&self.http, &self.config, &self.identity, job_id, request)
            .await
            .map_err(|error| unavailable("open the lease", error))?;
        // The lease is open now. A failure to read its opening state must
        // not unwind into a launch error: the service releases a failed
        // launch's reservation, and a retry would then open a second lease
        // for the one customer request. Report it provisioning instead and
        // let the client poll the live state in.
        Ok(project_opened_lease(
            plan,
            lease_view(&self.http, &self.config, &self.identity, job_id).await,
        ))
    }

    async fn poll(&self, job_id: Uuid, plan: &LaunchPlan) -> Result<ProviderJob, ProviderError> {
        let view = lease_view(&self.http, &self.config, &self.identity, job_id)
            .await
            .map_err(|error| unavailable("read the lease", error))?;
        Ok(project_lease(plan, &view))
    }

    async fn cancel(&self, job_id: Uuid, plan: &LaunchPlan) -> Result<ProviderJob, ProviderError> {
        // A lease no operator has accepted yet has no session to meter: it
        // is cancelled with a whole refund. Once accepted, cancellation is
        // a metered close instead. Read the phase to pick the right one, so
        // a buyer can abort a job still waiting on supply and get the
        // reservation back at once rather than waiting out the deadline.
        let offered = matches!(
            lease_view(&self.http, &self.config, &self.identity, job_id).await,
            Ok(view) if view.status == "offered" && !view.close_requested
        );
        if offered {
            match cancel_job(&self.http, &self.config, &self.identity, job_id).await {
                Ok(view) => return Ok(project_cancel(plan, &view)),
                // The lease accepted between the read and the cancel, or the
                // refund's answer was lost. Either way the close below
                // reconciles the real state: it records a close if the lease
                // is now accepted, and reports the settled outcome if the
                // refund did land.
                Err(error) => tracing::warn!(%error, "lease cancel fell back to close"),
            }
        }
        let view = close_lease(&self.http, &self.config, &self.identity, job_id)
            .await
            .map_err(|error| unavailable("close the lease", error))?;
        Ok(project_lease(plan, &view))
    }
}

fn unavailable(action: &str, error: impl std::fmt::Display) -> ProviderError {
    tracing::warn!(action, %error, "coordinator call failed");
    ProviderError::Unavailable
}

/// Rounds an operator's per-hour ask up to a whole micro-USDC per second,
/// expressed per hour (a multiple of 3600).
fn offer_rate_per_hour(min_ask_micro_usdc: u64) -> u64 {
    min_ask_micro_usdc
        .div_ceil(SETTLEMENT_GRANULARITY_PER_HOUR)
        .saturating_mul(SETTLEMENT_GRANULARITY_PER_HOUR)
}

/// Projects the coordinator's capacity directory into bookable offers: one
/// per GPU class across the hourly lease entries, priced at the pool's
/// cheapest rate for it. A class whose only operators ask more will not
/// match a launch quoted at this rate, so the lease refunds rather than
/// overcharges.
fn offers_from_capacity(view: &CapacityView) -> Vec<ComputeOffer> {
    let lease_entries = || {
        view.entries.iter().filter(|e| {
            e.kind == JobKind::LeaseSession && e.min_ask_unit == PriceUnit::PerLeaseHour
        })
    };
    // A gpu class can appear in more than one lease entry: operators serving
    // the same hardware under different model labels each land in their own
    // (kind, model) row. Pool the class across every entry it appears in so
    // its offer quotes the cheapest rate on offer for it — otherwise a class
    // would be quoted at whichever row happened to be projected first, above
    // supply that could serve the launch for less.
    let mut pooled: HashMap<&str, (u64, u32, bool)> = HashMap::new();
    for entry in lease_entries() {
        let rate = offer_rate_per_hour(entry.min_ask_micro_usdc);
        let online = entry.operators > 0;
        for class in &entry.gpu_classes {
            pooled
                .entry(class.as_str())
                .and_modify(|(cheapest, vram_gb, up)| {
                    *cheapest = (*cheapest).min(rate);
                    *vram_gb = (*vram_gb).max(entry.max_vram_gb);
                    *up |= online;
                })
                .or_insert((rate, entry.max_vram_gb, online));
        }
    }
    let mut offers = Vec::new();
    let mut emitted = HashSet::new();
    for entry in lease_entries() {
        for class in &entry.gpu_classes {
            if class == "cpu" || !emitted.insert(class.as_str()) {
                continue;
            }
            let (rate, vram_gb, online) = pooled[class.as_str()];
            offers.push(ComputeOffer {
                id: class.clone(),
                gpu: GpuSpec {
                    model: class.clone(),
                    vram_mib: u64::from(vram_gb).saturating_mul(1024),
                    cuda_major: 0,
                },
                rate_usdc_micros_per_hour: rate,
                trust_class: TrustClass::Open,
                online,
            });
        }
    }
    offers
}

/// Builds the lease terms for a checked plan. The offer rate is always a
/// multiple of 3600, so the per-second rate divides evenly and the escrow
/// matches the quoted maximum.
fn lease_terms(plan: &LaunchPlan) -> LeaseTerms {
    LeaseTerms {
        max_duration_secs: plan.duration_secs,
        rate_micro_usdc_per_sec: plan.offer.rate_usdc_micros_per_hour
            / SETTLEMENT_GRANULARITY_PER_HOUR,
        client_public_key: None,
    }
}

fn vram_gb(min_vram_mib: u64) -> u32 {
    u32::try_from(min_vram_mib.div_ceil(1024)).unwrap_or(u32::MAX)
}

/// Projects a just-opened lease onto the customer job. The lease is open
/// whether or not its opening view could be read, so a read failure reports
/// provisioning rather than failing the launch: a failed launch releases
/// its reservation, and a retry would open a second lease for the one
/// request. The client polls the live state in from there.
fn project_opened_lease<E: std::fmt::Display>(
    plan: &LaunchPlan,
    view: Result<LeaseView, E>,
) -> ProviderJob {
    match view {
        Ok(view) => project_lease(plan, &view),
        Err(error) => {
            tracing::warn!(action = "read the opened lease", %error, "coordinator call failed");
            ProviderJob {
                status: JobStatus::Provisioning,
                access_url: None,
                error: None,
                receipt: None,
            }
        }
    }
}

/// Projects a lease view onto the customer job shape. The access
/// credential is surfaced only while the session is running: a stopping or
/// provisioning job is not offered for use, and a terminal one never
/// carries it.
fn project_lease(plan: &LaunchPlan, view: &LeaseView) -> ProviderJob {
    let status = lease_status(&view.status, view.access.is_some(), view.close_requested);
    let access_url = match status {
        JobStatus::Running => view.access.as_ref().map(|access| access.endpoint.clone()),
        _ => None,
    };
    let error = match status {
        JobStatus::Failed => Some(format!("the lease {}", view.status)),
        _ => None,
    };
    // Every terminal lease carries its receipt, including a failed one: the
    // workload broke and the escrow refunded whole, so the receipt shows a
    // zero charge. A live or stopping lease has not settled and carries
    // none.
    let receipt = match status {
        JobStatus::Completed | JobStatus::Cancelled | JobStatus::Failed => {
            Some(lease_receipt(plan, view))
        }
        _ => None,
    };
    ProviderJob {
        status,
        access_url,
        error,
        receipt,
    }
}

/// Projects a pre-acceptance cancellation onto the customer job shape. A
/// cancelled lease never ran, so it settles as `cancelled` with a receipt
/// that refunds the whole escrowed ceiling and charges nothing. The refund
/// is clamped to that ceiling, so charge and refund always reconcile to it.
fn project_cancel(plan: &LaunchPlan, view: &CancelView) -> ProviderJob {
    let maximum = plan.maximum_usdc_micros;
    let refunded = view.refunded_micro_usdc.min(maximum);
    ProviderJob {
        status: JobStatus::Cancelled,
        access_url: None,
        error: None,
        receipt: Some(ComputeReceipt {
            id: format!("lease-{}", view.job_id),
            job_id: view.job_id.to_string(),
            app_id: plan.app.id.clone(),
            provider: "covenant".to_owned(),
            runtime_secs: 0,
            provisioning_secs: 0,
            provisioning_usdc_micros: 0,
            charged_usdc_micros: maximum.saturating_sub(refunded),
            refunded_usdc_micros: refunded,
            commitment: view.job_id.to_string(),
            transaction: None,
        }),
    }
}

/// Maps the coordinator's lease status onto the customer job lifecycle. An
/// offered lease is provisioning until an operator accepts it and the
/// machine is reachable; a lease whose close the buyer has requested but
/// which has not yet settled is stopping; a whole refund reads as
/// cancelled, whether the engine refunded it or an operator declined the
/// offer; anything the engine failed reads as failed.
fn lease_status(status: &str, reachable: bool, close_requested: bool) -> JobStatus {
    match status {
        "completed" => JobStatus::Completed,
        // A refund and an operator's rejection both hand the buyer's money
        // back whole with nothing billed, so both read as a cancellation
        // carrying the whole-refund receipt, not a failure. A rejection is
        // the specific admission refund where an operator was offered the
        // job and declined it.
        "refunded" | "rejected" => JobStatus::Cancelled,
        // A close is recorded and then carried out asynchronously: the
        // serving node sees it, tears the session down and submits the
        // receipt that settles the meter. Until that lands the lease is
        // still accepted, so report the wind-down rather than a session the
        // buyer has already asked to end.
        "offered" | "accepted" if close_requested => JobStatus::Stopping,
        "offered" => JobStatus::Provisioning,
        "accepted" if reachable => JobStatus::Running,
        "accepted" => JobStatus::Provisioning,
        _ => JobStatus::Failed,
    }
}

/// Builds the settled receipt from the meter. The charge is clamped to the
/// ceiling the customer reserved — the plan maximum their job already
/// carries — and the refund is the remainder, so `charged + refunded`
/// always equals that reserved ceiling however the coordinator's echoed
/// meter reads, and the charge can never exceed what the customer agreed
/// to. Runtime is capped at the window. This anchors to the plan for the
/// same reason [`project_cancel`] does: the receipt reconciles against the
/// figure the customer sees, not one re-derived from the returned view.
fn lease_receipt(plan: &LaunchPlan, view: &LeaseView) -> ComputeReceipt {
    let maximum = plan.maximum_usdc_micros;
    let charged = view.charged_micro_usdc.min(maximum);
    ComputeReceipt {
        id: format!("lease-{}", view.job_id),
        job_id: view.job_id.to_string(),
        app_id: plan.app.id.clone(),
        provider: "covenant".to_owned(),
        runtime_secs: view.elapsed_ms.div_ceil(1_000).min(plan.duration_secs),
        provisioning_secs: 0,
        provisioning_usdc_micros: 0,
        charged_usdc_micros: charged,
        refunded_usdc_micros: maximum.saturating_sub(charged),
        commitment: view.job_id.to_string(),
        transaction: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{quote_maximum, AppAvailability, AppKind, ComputeApp};
    use covenant_compute_buyer::CapacityEntry;
    use covenant_compute_protocol::LeaseAccess;

    fn lease_entry(
        classes: &[&str],
        min_ask: u64,
        max_vram_gb: u32,
        operators: usize,
    ) -> CapacityEntry {
        CapacityEntry {
            kind: JobKind::LeaseSession,
            model: "any".into(),
            operators,
            min_ask_micro_usdc: min_ask,
            min_ask_unit: PriceUnit::PerLeaseHour,
            max_ask_micro_usdc: min_ask,
            max_vram_gb,
            gpu_classes: classes.iter().map(|c| (*c).to_owned()).collect(),
            tee_capable: false,
        }
    }

    fn view(entries: Vec<CapacityEntry>) -> CapacityView {
        CapacityView {
            registered_operators: entries.iter().map(|e| e.operators).sum(),
            matchable_operators: entries.iter().map(|e| e.operators).sum(),
            liveness_window_ms: 30_000,
            min_score_bps: 0,
            min_bond_micro_usdc: 0,
            entries,
        }
    }

    fn plan(rate_per_hour: u64, duration_secs: u64) -> LaunchPlan {
        let offer = crate::wire::ComputeOffer {
            id: "rtx-4090".into(),
            gpu: GpuSpec {
                model: "rtx-4090".into(),
                vram_mib: 24_576,
                cuda_major: 0,
            },
            rate_usdc_micros_per_hour: rate_per_hour,
            trust_class: TrustClass::Open,
            online: true,
        };
        let app = ComputeApp {
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
        };
        LaunchPlan {
            maximum_usdc_micros: quote_maximum(rate_per_hour, duration_secs).unwrap(),
            app,
            offer,
            duration_secs,
        }
    }

    fn settled_view(
        rate_per_sec: u64,
        max_duration_secs: u64,
        elapsed_ms: u64,
        charged: u64,
    ) -> LeaseView {
        LeaseView {
            job_id: Uuid::nil(),
            status: "completed".into(),
            access: None,
            rate_micro_usdc_per_sec: rate_per_sec,
            max_duration_secs,
            elapsed_ms,
            charged_micro_usdc: charged,
            close_requested: true,
        }
    }

    #[test]
    fn a_rate_rounds_up_to_a_whole_micro_per_second() {
        assert_eq!(offer_rate_per_hour(3_600_000), 3_600_000);
        assert_eq!(offer_rate_per_hour(1_000_000), 1_000_800);
        assert_eq!(offer_rate_per_hour(1), 3_600);
        assert_eq!(offer_rate_per_hour(0), 0);
    }

    #[test]
    fn the_quoted_rate_and_the_lease_escrow_agree() {
        // Every offered rate is a multiple of 3600, so the per-hour quote a
        // customer sees equals the per-second escrow the lease opens with.
        for min_ask in [1, 999_999, 1_000_000, 2_500_000] {
            let rate = offer_rate_per_hour(min_ask);
            for duration_secs in [300, 1_800, 3_600, 21_600] {
                let terms = lease_terms(&plan(rate, duration_secs));
                let escrow = terms.max_price_micro_usdc().unwrap();
                assert_eq!(escrow, quote_maximum(rate, duration_secs).unwrap());
            }
        }
    }

    #[test]
    fn only_hourly_lease_entries_become_offers() {
        let offers = offers_from_capacity(&view(vec![
            lease_entry(&["rtx-4090", "a100-80g", "cpu"], 1_000_000, 80, 2),
            CapacityEntry {
                kind: JobKind::InferenceCall,
                ..lease_entry(&["rtx-4090"], 500_000, 24, 3)
            },
            CapacityEntry {
                min_ask_unit: PriceUnit::PerJob,
                ..lease_entry(&["h100"], 700_000, 80, 1)
            },
        ]));
        let ids: Vec<&str> = offers.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, ["rtx-4090", "a100-80g"]);
        assert!(offers
            .iter()
            .all(|o| o.rate_usdc_micros_per_hour == 1_000_800));
        assert!(offers.iter().all(|o| o.gpu.vram_mib == 80 * 1024));
        assert!(offers.iter().all(|o| o.online));
    }

    #[test]
    fn an_offer_from_an_empty_pool_reads_offline() {
        let offers =
            offers_from_capacity(&view(vec![lease_entry(&["rtx-4090"], 1_000_000, 24, 0)]));
        assert_eq!(offers.len(), 1);
        assert!(!offers[0].online);
    }

    #[test]
    fn a_class_across_entries_is_quoted_at_the_pools_cheapest() {
        // The same hardware advertised under two model labels at different
        // rates lands in two rows sharing one gpu class. The class is offered
        // once, at the cheaper rate, so a launch is never quoted above the
        // supply that can serve it.
        let offers = offers_from_capacity(&view(vec![
            CapacityEntry {
                model: "label-a".into(),
                ..lease_entry(&["rtx-4090"], 2_000_000, 24, 1)
            },
            CapacityEntry {
                model: "label-b".into(),
                ..lease_entry(&["rtx-4090"], 1_000_000, 24, 1)
            },
        ]));
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].id, "rtx-4090");
        assert_eq!(
            offers[0].rate_usdc_micros_per_hour,
            offer_rate_per_hour(1_000_000)
        );
        assert!(offers[0].online);
    }

    #[test]
    fn lease_status_maps_the_lifecycle() {
        assert_eq!(
            lease_status("offered", false, false),
            JobStatus::Provisioning
        );
        assert_eq!(
            lease_status("accepted", false, false),
            JobStatus::Provisioning
        );
        assert_eq!(lease_status("accepted", true, false), JobStatus::Running);
        assert_eq!(
            lease_status("completed", false, false),
            JobStatus::Completed
        );
        assert_eq!(lease_status("refunded", false, false), JobStatus::Cancelled);
        assert_eq!(lease_status("failed", false, false), JobStatus::Failed);
        // An operator declining the offer refunds the buyer whole, so it
        // reads as a cancellation, the same as any other whole refund.
        assert_eq!(lease_status("rejected", false, false), JobStatus::Cancelled);
        // A requested-but-unsettled close reads as stopping whether or not
        // the machine is still reachable; once the close has settled the
        // lease reports its terminal outcome, not stopping.
        assert_eq!(lease_status("accepted", true, true), JobStatus::Stopping);
        assert_eq!(lease_status("accepted", false, true), JobStatus::Stopping);
        assert_eq!(lease_status("completed", false, true), JobStatus::Completed);
        assert_eq!(lease_status("refunded", false, true), JobStatus::Cancelled);
    }

    #[test]
    fn a_running_lease_carries_its_endpoint() {
        let mut view = settled_view(1_000, 1_800, 30_000, 0);
        view.status = "accepted".into();
        view.close_requested = false;
        view.access = Some(LeaseAccess {
            job_id: Uuid::nil(),
            endpoint: "ssh renter@203.0.113.7 -p 2222".into(),
            ready_at_ms: 0,
            note: None,
        });
        let job = project_lease(&plan(3_600_000, 1_800), &view);
        assert_eq!(job.status, JobStatus::Running);
        assert_eq!(
            job.access_url.as_deref(),
            Some("ssh renter@203.0.113.7 -p 2222")
        );
        assert!(job.receipt.is_none());
    }

    #[test]
    fn a_close_requested_lease_reads_as_stopping_and_drops_access() {
        // The buyer has asked to close; the coordinator has recorded it but
        // the node has not yet settled, so the lease is still accepted and
        // reachable. The customer sees the session winding down, not running,
        // and the credential it just asked to release is no longer served.
        let mut view = settled_view(1_000, 1_800, 30_000, 0);
        view.status = "accepted".into();
        view.close_requested = true;
        view.access = Some(LeaseAccess {
            job_id: Uuid::nil(),
            endpoint: "ssh renter@203.0.113.7 -p 2222".into(),
            ready_at_ms: 0,
            note: None,
        });
        let job = project_lease(&plan(3_600_000, 1_800), &view);
        assert_eq!(job.status, JobStatus::Stopping);
        assert!(
            job.access_url.is_none(),
            "a stopping session drops its credential"
        );
        assert!(job.receipt.is_none());
        assert!(job.error.is_none());
    }

    #[test]
    fn an_open_lease_whose_view_fails_reports_provisioning_not_failure() {
        // submit_streaming already opened the lease; the immediate view read
        // then failed. The launch must report provisioning, not a failure
        // the service would release the reservation on and retry into a
        // second lease.
        let job = project_opened_lease(
            &plan(3_600_000, 1_800),
            Err::<LeaseView, _>("coordinator timed out"),
        );
        assert_eq!(job.status, JobStatus::Provisioning);
        assert!(job.access_url.is_none());
        assert!(job.error.is_none());
        assert!(job.receipt.is_none());
    }

    #[test]
    fn a_settled_lease_reconciles_against_its_ceiling() {
        // 1000 micro/s over a 1800s window escrows 1_800_000; billed 500_000.
        let job = project_lease(
            &plan(3_600_000, 1_800),
            &settled_view(1_000, 1_800, 500_000, 500_000),
        );
        assert_eq!(job.status, JobStatus::Completed);
        let receipt = job.receipt.unwrap();
        assert_eq!(receipt.charged_usdc_micros, 500_000);
        assert_eq!(receipt.refunded_usdc_micros, 1_800_000 - 500_000);
        assert_eq!(
            receipt.charged_usdc_micros + receipt.refunded_usdc_micros,
            1_800_000
        );
        assert_eq!(receipt.runtime_secs, 500);
        assert_eq!(receipt.provider, "covenant");
        // The meter runs from operator acceptance, so the billed window
        // includes the bring-up before the workspace is reachable: no
        // provisioning is carved out of the charge, and the receipt says
        // so rather than claiming a free-bring-up the customer never got.
        assert_eq!(receipt.provisioning_secs, 0);
        assert_eq!(receipt.provisioning_usdc_micros, 0);
    }

    #[test]
    fn a_settled_receipt_reconciles_to_the_reserved_ceiling_not_the_returned_view() {
        // The receipt reconciles to what the customer reserved and sees on
        // their job (the plan's ceiling and window), never to figures
        // re-derived from the coordinator's echoed view. Under an honest
        // coordinator the two agree, but should a returned view ever carry a
        // rate or window above the signed terms, anchoring to it would report
        // a charge above what the customer agreed to, a total that doesn't add
        // back up to it, and a runtime past the window. Here a drifted view
        // claims twice the window (3_600s), a 2_000_000 charge, and a 3_600s
        // run against a lease that reserved 1_800_000 over 1_800s.
        let plan = plan(3_600_000, 1_800); // reserves 1_800_000 over 1_800s
        let receipt = project_lease(&plan, &settled_view(2_000, 3_600, 3_600_000, 2_000_000))
            .receipt
            .unwrap();
        assert_eq!(
            receipt.charged_usdc_micros, 1_800_000,
            "the charge is clamped to the reserved ceiling, never above it"
        );
        assert_eq!(
            receipt.charged_usdc_micros + receipt.refunded_usdc_micros,
            1_800_000,
            "charge and refund reconcile to the ceiling the customer reserved"
        );
        assert_eq!(
            receipt.runtime_secs, 1_800,
            "runtime is capped at the window the customer agreed to, not the view's"
        );
    }

    #[test]
    fn a_cancelled_lease_refunds_the_whole_ceiling() {
        // A pre-acceptance cancel never ran, so it refunds the escrowed
        // ceiling and charges nothing; the receipt reconciles to the ceiling.
        let plan = plan(3_600_000, 1_800); // escrows 1_800_000
        let view = CancelView {
            job_id: Uuid::nil(),
            status: "refunded".into(),
            refunded_micro_usdc: 1_800_000,
        };
        let job = project_cancel(&plan, &view);
        assert_eq!(job.status, JobStatus::Cancelled);
        assert!(job.access_url.is_none());
        assert!(job.error.is_none());
        let receipt = job.receipt.unwrap();
        assert_eq!(receipt.charged_usdc_micros, 0);
        assert_eq!(receipt.refunded_usdc_micros, 1_800_000);
        assert_eq!(
            receipt.charged_usdc_micros + receipt.refunded_usdc_micros,
            1_800_000
        );
        assert_eq!(receipt.runtime_secs, 0);
    }

    #[test]
    fn a_rejected_lease_reads_as_a_whole_refund_not_a_failure() {
        // An operator declined the offer, so the buyer never got a machine
        // and was refunded whole. It settles as cancelled with a receipt
        // that charges nothing, the same shape a pre-acceptance cancel
        // produces, rather than a failure that leaves no money record.
        let mut view = settled_view(1_000, 1_800, 0, 0); // escrows 1_800_000
        view.status = "rejected".into();
        view.close_requested = false;
        let job = project_lease(&plan(3_600_000, 1_800), &view);
        assert_eq!(job.status, JobStatus::Cancelled);
        assert!(job.access_url.is_none());
        assert!(job.error.is_none());
        let receipt = job.receipt.unwrap();
        assert_eq!(receipt.charged_usdc_micros, 0);
        assert_eq!(receipt.refunded_usdc_micros, 1_800_000);
        assert_eq!(receipt.runtime_secs, 0);
    }

    #[test]
    fn a_failed_lease_carries_its_whole_refund_receipt() {
        // A lease whose execution failed is refunded whole. It still reads
        // as failed — the workload broke — and names why, but it carries
        // the receipt showing the buyer was charged nothing, the way the
        // live product surfaces a receipt on every terminal job.
        let mut view = settled_view(1_000, 1_800, 0, 0); // escrows 1_800_000
        view.status = "failed".into();
        view.close_requested = false;
        let job = project_lease(&plan(3_600_000, 1_800), &view);
        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(job.error.as_deref(), Some("the lease failed"));
        assert!(job.access_url.is_none());
        let receipt = job.receipt.unwrap();
        assert_eq!(receipt.charged_usdc_micros, 0);
        assert_eq!(receipt.refunded_usdc_micros, 1_800_000);
        assert_eq!(receipt.runtime_secs, 0);
    }
}
