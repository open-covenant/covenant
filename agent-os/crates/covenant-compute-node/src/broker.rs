//! A broker node: an operator that owns no hardware and rents it per
//! session from a cloud GPU market.
//!
//! This is what puts a real machine behind a lease. When a session
//! opens, the broker works down the cheapest admissible offers,
//! launches an instance carrying the buyer's own public key, waits for
//! it to answer, and hands back the address. When the session closes it
//! destroys the instance.
//!
//! Rentals race. An offer is a quote, not a reservation, and the head of
//! the book is what everyone else is also trying to take, so a survey
//! and a take are two different views of a market that moved in
//! between. Losing that race is routine and is treated as such: the
//! broker tries the next machine, remembers the ones that already lost,
//! and asks for a new book once the one in hand is used up. What it will
//! not do is retry blindly — the walk is bounded, and any instance an
//! attempt managed to create is destroyed before the next one starts.
//!
//! The machine's cloud bill is the operator's cost, never the buyer's:
//! destruction is best-effort-until-confirmed and runs on every exit
//! path, and a leaked instance bills the broker until someone notices,
//! so the cleanup here retries rather than logging and moving on.
//!
//! The lease meter is a separate ledger, and today it runs on the
//! coordinator's clock from accept to result submission — which brackets
//! more than the session the buyer used. `open` rents a machine and waits
//! for it to answer after the meter's t0: a boot that never answers within
//! `READY_TIMEOUT` fails the job and refunds the buyer whole, but one that
//! does answer has its boot seconds billed to the buyer. And `close`
//! destroys the instance before the result is submitted, so close
//! detection and teardown fall inside the metered window too. Billing only
//! the seconds between readiness and the buyer's close means moving the
//! meter's t0 to readiness and its end to the recorded close instant — a
//! coordinator-side change this module cannot make on its own.
//!
//! The buyer's key is what makes the session theirs: it is signed into
//! the lease terms, attached to the instance at launch, and never
//! shared with anyone else. A lease with no key is refused rather than
//! opened with a broker-held credential — a session whose operator can
//! also log in is not a rental.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use covenant_compute_protocol::{JobEnvelopePayload, LeaseAccess};
use covenant_compute_vast::{Launch, LaunchRequest, OfferSurvey, VastClient, VastError};
use parking_lot::Mutex;
use uuid::Uuid;

use crate::executor::ExecutorError;
use crate::lease::SessionBackend;

/// How long a session may spend coming up before the broker gives up
/// and refuses the lease. Past this the buyer has been billed for a
/// machine that never answered, so the honest move is to fail the job —
/// which refunds them — rather than keep burning their window.
pub const READY_TIMEOUT: Duration = Duration::from_secs(240);
/// How often the broker asks whether a launching instance is up.
pub const READY_POLL_INTERVAL: Duration = Duration::from_secs(5);
/// How many times teardown retries before giving up and shouting. Each
/// failure leaves an instance billing the broker, so this is generous.
pub const DESTROY_ATTEMPTS: u32 = 6;
/// How many offers one survey ranks. Deep enough that losing the head of
/// the book still leaves somewhere to go.
pub const SURVEY_DEPTH: usize = 8;
/// How many machines the broker will try to take before it gives up on
/// the lease. Each loss is a round trip, not a rental, so this is cheap;
/// the bound exists so a market that is refusing everyone cannot hold a
/// buyer's window open indefinitely.
pub const RENTAL_ATTEMPTS: u32 = 4;
/// How many times the market is surveyed. The second survey is what
/// makes the retry useful when the whole book turned over: the offers
/// held from the first one are, by then, exactly the stale ones.
pub const SURVEY_ROUNDS: u32 = 2;

/// Whether a failed take means someone else got the machine first.
///
/// Four different answers say the same thing. `OfferChanged` is the
/// offer gone from the book or repriced under us, `NoCapacity` is it
/// coming back outside the cap, `Refused` on creation is the provider
/// rejecting the take, and `SshKeyAttachment` is a host that accepted
/// the take but would not hold the buyer's key. All four are the
/// market's answer about one machine, so the next machine is worth
/// asking. Anything else is the broker's own fault and retrying it just
/// repeats the mistake against a fresh host.
fn lost_the_race(error: &VastError) -> bool {
    matches!(
        error,
        VastError::OfferChanged
            | VastError::NoCapacity
            | VastError::Refused {
                operation: "instance creation"
            }
            | VastError::SshKeyAttachment(_)
    )
}

/// What the broker rents and how it bounds the spend.
#[derive(Debug, Clone)]
pub struct BrokerConfig {
    /// Digest-pinned image every session runs. Pinned by digest so the
    /// machine a buyer gets is the machine that was measured.
    pub image: String,
    /// The most the broker will pay per hour for one session. Bounds
    /// the loss on any single lease independently of what the buyer
    /// paid — the two are different sides of the trade.
    pub max_hourly_micros: u64,
    pub ready_timeout: Duration,
    pub ready_poll_interval: Duration,
}

/// One live rented instance, kept so close can find it.
struct Rented {
    instance_id: u64,
}

/// Rents a machine per session from a cloud GPU market.
pub struct BrokerSessionBackend {
    client: Arc<VastClient>,
    config: BrokerConfig,
    live: Mutex<std::collections::HashMap<Uuid, Rented>>,
}

impl BrokerSessionBackend {
    pub fn new(client: Arc<VastClient>, config: BrokerConfig) -> Self {
        Self {
            client,
            config,
            live: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Destroys an instance, retrying transient failures. Returns
    /// whether the machine is confirmed gone — a `false` here is money
    /// leaving the broker's account for nothing, so the caller escalates
    /// rather than swallows it.
    async fn release(&self, instance_id: u64) -> bool {
        for attempt in 1..=DESTROY_ATTEMPTS {
            match self.client.destroy(instance_id).await {
                Ok(()) => return true,
                Err(e) => {
                    tracing::warn!(
                        instance_id,
                        attempt,
                        error = %e,
                        "could not destroy a rented instance; retrying"
                    );
                    tokio::time::sleep(Duration::from_secs(2u64.pow(attempt.min(5)))).await;
                }
            }
        }
        false
    }

    /// Waits for a rented instance to answer and turns it into the
    /// buyer's access. The instance is registered before the wait so a
    /// concurrent close can find it, and destroyed on every exit that is
    /// not a live session: the broker is billed from creation.
    async fn hand_over(&self, job_id: Uuid, launch: &Launch) -> Result<LeaseAccess, ExecutorError> {
        self.live.lock().insert(
            job_id,
            Rented {
                instance_id: launch.instance_id,
            },
        );
        let deadline = tokio::time::Instant::now() + self.config.ready_timeout;
        loop {
            match self.client.instance(launch.instance_id).await {
                Ok(facts) if facts.ready => {
                    let Some(ssh) = facts.ssh else {
                        self.abandon(job_id, launch.instance_id).await;
                        return Err(ExecutorError::Failed(
                            "the rented machine came up without an address to reach it on".into(),
                        ));
                    };
                    return Ok(LeaseAccess {
                        job_id,
                        endpoint: format!("ssh -p {} root@{}", ssh.port, ssh.host),
                        ready_at_ms: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0),
                        note: Some(format!(
                            "{} · {} MiB VRAM · session ends when you close the lease",
                            facts.gpu_model, facts.gpu_memory_mib
                        )),
                    });
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    instance_id = launch.instance_id,
                    error = %e,
                    "could not read a launching instance; retrying"
                ),
            }
            if tokio::time::Instant::now() >= deadline {
                self.abandon(job_id, launch.instance_id).await;
                return Err(ExecutorError::Failed(format!(
                    "the rented machine did not become reachable within {:?}",
                    self.config.ready_timeout
                )));
            }
            tokio::time::sleep(self.config.ready_poll_interval).await;
        }
    }

    /// Gives a machine back that never became a session.
    async fn abandon(&self, job_id: Uuid, instance_id: u64) {
        self.live.lock().remove(&job_id);
        if !self.release(instance_id).await {
            tracing::error!(
                %job_id,
                instance_id,
                "an instance the broker gave up on could not be destroyed and is still \
                 billing; destroy it by hand"
            );
        }
    }
}

#[async_trait]
impl SessionBackend for BrokerSessionBackend {
    async fn open(&self, job: &JobEnvelopePayload) -> Result<LeaseAccess, ExecutorError> {
        let terms = covenant_compute_protocol::parse_lease_terms(&job.input)
            .map_err(|e| ExecutorError::Failed(format!("lease terms unreadable: {e}")))?
            .ok_or_else(|| ExecutorError::Failed("lease carries no terms".into()))?;
        // Without the buyer's key there is no way to give them — and
        // only them — the machine. Refusing costs the broker a job;
        // opening anyway would hand a renter a box their operator can
        // also log into.
        let ssh_public_key = terms.client_public_key.clone().ok_or_else(|| {
            ExecutorError::Failed(
                "this lease carries no client public key, so its session could not be \
                 given to the buyer alone"
                    .into(),
            )
        })?;

        // The cheapest offer is also the most contended one, and the
        // market re-lets it between the survey and the take often enough
        // that betting the whole lease on it loses about half the time.
        // Walk down the book instead, and when the book is used up, ask
        // for a new one: by then the offers held from the first survey
        // are precisely the stale ones. A slightly dearer machine is the
        // operator's cost, and far cheaper than failing the job.
        let mut rejected: Vec<u64> = Vec::new();
        let mut attempts = 0u32;
        let mut opening_survey: Option<OfferSurvey> = None;
        let mut lost_to: Option<VastError> = None;

        'surveys: for round in 1..=SURVEY_ROUNDS {
            let ranked = self
                .client
                .ranked_offers(SURVEY_DEPTH, &rejected, self.config.max_hourly_micros)
                .await
                .map_err(|e| {
                    ExecutorError::Failed(format!("no capacity could be surveyed: {e}"))
                })?;
            opening_survey.get_or_insert(ranked.survey);
            if ranked.offers.is_empty() {
                break;
            }
            for offer in &ranked.offers {
                if attempts >= RENTAL_ATTEMPTS {
                    break 'surveys;
                }
                attempts += 1;
                tracing::info!(
                    job_id = %job.job_id,
                    round,
                    attempt = attempts,
                    offer_id = offer.id,
                    machine_id = offer.machine_id,
                    hourly_micros = offer.hourly_micros,
                    "taking a machine off the market"
                );
                let launch = match self
                    .client
                    .launch(LaunchRequest {
                        workload_id: job.job_id.to_string(),
                        image: self.config.image.clone(),
                        max_hourly_micros: self.config.max_hourly_micros,
                        ssh_public_key: ssh_public_key.clone(),
                        // Everything already lost this open, so the
                        // market cannot sell us the same dead host under
                        // a second offer ID.
                        rejected_machine_ids: rejected.clone(),
                        required_offer: offer.quote(),
                    })
                    .await
                {
                    Ok(launch) => launch,
                    Err(e) if lost_the_race(&e) => {
                        tracing::info!(
                            job_id = %job.job_id,
                            attempt = attempts,
                            offer_id = offer.id,
                            machine_id = offer.machine_id,
                            error = %e,
                            "this machine went before we could take it; trying the next offer"
                        );
                        if !rejected.contains(&offer.machine_id) {
                            rejected.push(offer.machine_id);
                        }
                        lost_to = Some(e);
                        continue;
                    }
                    Err(e) => {
                        match &e {
                            VastError::AttachAndCleanupFailed { instance_id, .. } => {
                                tracing::error!(
                                    job_id = %job.job_id,
                                    instance_id,
                                    machine_id = offer.machine_id,
                                    error = %e,
                                    "an instance was created and could not be destroyed; it is \
                                     still billing, destroy it by hand"
                                )
                            }
                            // The create response was lost, so a machine may be
                            // billing under this label with its id unknown to us.
                            // Retrying would rent a second one while the first
                            // leaks, so the lease fails here; the label is the
                            // only trace an operator has to reconcile it.
                            VastError::CreateUncertain { label } => tracing::error!(
                                job_id = %job.job_id,
                                machine_id = offer.machine_id,
                                %label,
                                error = %e,
                                "a create response was lost; an instance may be billing under \
                                 this label — list instances by it and destroy any orphan"
                            ),
                            _ => {}
                        }
                        return Err(ExecutorError::Failed(format!(
                            "could not rent a machine: {e}"
                        )));
                    }
                };
                tracing::info!(
                    job_id = %job.job_id,
                    attempt = attempts,
                    instance_id = launch.instance_id,
                    machine_id = launch.offer.machine_id,
                    hourly_micros = launch.offer.hourly_micros,
                    "rented; waiting for it to answer"
                );
                return self.hand_over(job.job_id, &launch).await;
            }
        }

        let survey = opening_survey.expect("the first survey either returns or fails the lease");
        Err(ExecutorError::Failed(match lost_to {
            Some(last) => format!(
                "could not rent a machine: {attempts} machines went while we were taking \
                 them (last: {last})"
            ),
            None => format!(
                "no admissible GPU offer right now (surveyed {}, {} priced out, \
                 {} wrong class)",
                survey.returned, survey.price_ceiling, survey.gpu_class
            ),
        }))
    }

    async fn close(&self, job_id: Uuid) {
        let Some(rented) = self.live.lock().remove(&job_id) else {
            return;
        };
        if !self.release(rented.instance_id).await {
            // Nothing else in the system knows this instance exists, so
            // this line is the only trace an operator has to chase.
            tracing::error!(
                %job_id,
                instance_id = rented.instance_id,
                "a rented instance could not be destroyed and is still billing; \
                 destroy it by hand"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_compute_protocol::{lease_input, CapabilityRequirement, JobKind, LeaseTerms};
    use covenant_mcp::Content;

    pub(super) fn lease_job(client_public_key: Option<&str>) -> JobEnvelopePayload {
        let terms = LeaseTerms {
            max_duration_secs: 600,
            rate_micro_usdc_per_sec: 100,
            client_public_key: client_public_key.map(str::to_string),
        };
        let price = terms.max_price_micro_usdc().unwrap();
        JobEnvelopePayload {
            job_id: Uuid::new_v4(),
            buyer: covenant_identity::LocalIdentity::generate("buyer@broker").agent_id(),
            kind: JobKind::LeaseSession,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: None,
                kind: JobKind::LeaseSession,
                max_duration_secs: 600,
                min_reputation_bps: None,
            },
            input: vec![lease_input(terms).unwrap(), Content::text("rent me a gpu")],
            price_micro_usdc: price,
            deadline_ms: 660_000,
            idempotency: covenant_a2a::A2AIdempotency::new(
                covenant_a2a::A2ADuplicateSafety::Idempotent,
                "broker-test",
            ),
            issued_at_ms: 1,
            referral_code: None,
            stream: true,
        }
    }

    fn client() -> Arc<VastClient> {
        // A client pointed at nowhere: these tests never reach the
        // network, they pin the refusals that happen before it.
        let config = covenant_compute_vast::VastConfig::default();
        Arc::new(
            VastClient::new(config, covenant_compute_vast::ApiToken::new("t").unwrap()).unwrap(),
        )
    }

    fn config() -> BrokerConfig {
        BrokerConfig {
            image: "vastai/base-image@sha256:\
                    0000000000000000000000000000000000000000000000000000000000000000"
                .into(),
            max_hourly_micros: 600_000,
            ready_timeout: Duration::from_millis(50),
            ready_poll_interval: Duration::from_millis(10),
        }
    }

    #[tokio::test]
    async fn a_lease_with_no_client_key_is_refused_before_anything_is_rented() {
        let broker = BrokerSessionBackend::new(client(), config());
        let err = broker
            .open(&lease_job(None))
            .await
            .expect_err("a keyless lease must not open a session");
        assert!(
            matches!(&err, ExecutorError::Failed(msg) if msg.contains("no client public key")),
            "got: {err}"
        );
        assert!(
            broker.live.lock().is_empty(),
            "nothing may be rented for a lease that cannot be handed over"
        );
    }

    #[tokio::test]
    async fn closing_a_session_that_was_never_opened_is_a_no_op() {
        let broker = BrokerSessionBackend::new(client(), config());
        // Never panics, never reaches the network: the close path runs
        // on every exit, including ones where open failed early.
        broker.close(Uuid::new_v4()).await;
    }
}

#[cfg(test)]
mod market_tests {
    use super::tests::*;
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const IMAGE: &str = "docker.io/nvidia/cuda@sha256:\
                         cff3a0d82d2c2b47bab252d67fa9b34a20ef4c50781d98501b5c7367ea9afd10";

    fn offer_row(id: u64, machine_id: u64, price: f64) -> serde_json::Value {
        json!({
            "id": id,
            "machine_id": machine_id,
            "gpu_name": "L40S",
            "gpu_ram": 46068,
            "dph_total": price,
            "inet_down_cost": 0.0,
            "inet_up_cost": 0.0,
            "verification": "verified",
            "reliability": 0.999,
            "rentable": true,
            "rented": false,
            "direct_port_count": 2,
            "cuda_max_good": 12.4,
            "num_gpus": 1,
            "gpu_arch": "nvidia",
            "cpu_arch": "amd64"
        })
    }

    /// What the market answers each time it is asked for offers: the nth
    /// search gets the nth book, and the last book repeats. The client
    /// searches again to confirm every offer it takes, so a book that
    /// has dropped an offer *is* the market re-letting it mid-rental —
    /// which is the failure this whole path exists for.
    async fn book_turns_over(server: &MockServer, books: &[Vec<serde_json::Value>]) {
        for (index, book) in books.iter().enumerate() {
            let mock = Mock::given(method("POST"))
                .and(path("/api/v0/bundles/"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"offers": book})))
                .with_priority(index as u8 + 1);
            let mock = if index + 1 == books.len() {
                mock
            } else {
                mock.up_to_n_times(1)
            };
            mock.mount(server).await;
        }
    }

    /// A host that lets its machine be taken, comes up (or does not),
    /// and records every destroy it is asked for.
    async fn host(server: &MockServer, offer_id: u64, machine_id: u64, instance: u64, ready: bool) {
        takeable(server, offer_id, instance).await;
        Mock::given(method("POST"))
            .and(path(format!("/api/v0/instances/{instance}/ssh/")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": true})))
            .mount(server)
            .await;
        reports(server, offer_id, machine_id, instance, ready).await;
    }

    /// A host that hands over the machine and then will not hold the
    /// buyer's key. The take has already created a billing instance, so
    /// this is the case where a retry must not leak one.
    async fn host_that_drops_the_key(
        server: &MockServer,
        offer_id: u64,
        machine_id: u64,
        instance: u64,
    ) {
        takeable(server, offer_id, instance).await;
        Mock::given(method("POST"))
            .and(path(format!("/api/v0/instances/{instance}/ssh/")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": false})))
            .mount(server)
            .await;
        reports(server, offer_id, machine_id, instance, true).await;
    }

    /// A host that takes the machine, refuses the buyer's key, and then
    /// cannot be destroyed either. The take has created a billing instance
    /// whose id we hold but can no longer return, which is what separates
    /// this from a lost race: walking on would leave it leaking.
    async fn host_whose_cleanup_fails(server: &MockServer, offer_id: u64, instance: u64) {
        takeable(server, offer_id, instance).await;
        Mock::given(method("POST"))
            .and(path(format!("/api/v0/instances/{instance}/ssh/")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": false})))
            .mount(server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(format!("/api/v0/instances/{instance}/")))
            .respond_with(ResponseTemplate::new(500))
            .mount(server)
            .await;
    }

    /// A host whose take answers 200 but names no instance id: the create
    /// reply is lost, so a machine may be billing under the label with its id
    /// unknown to us. Retrying would rent a second one while the first leaks.
    async fn host_with_a_lost_create_response(server: &MockServer, offer_id: u64) {
        Mock::given(method("PUT"))
            .and(path(format!("/api/v0/asks/{offer_id}/")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": true})))
            .mount(server)
            .await;
    }

    /// A host that lists an offer it will not actually let: the take is
    /// refused because someone else got there first.
    async fn host_already_taken(server: &MockServer, offer_id: u64) {
        Mock::given(method("PUT"))
            .and(path(format!("/api/v0/asks/{offer_id}/")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": false})))
            .mount(server)
            .await;
    }

    async fn takeable(server: &MockServer, offer_id: u64, instance: u64) {
        Mock::given(method("PUT"))
            .and(path(format!("/api/v0/asks/{offer_id}/")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"success": true, "new_contract": instance})),
            )
            .mount(server)
            .await;
    }

    async fn reports(
        server: &MockServer,
        offer_id: u64,
        machine_id: u64,
        instance: u64,
        ready: bool,
    ) {
        Mock::given(method("GET"))
            .and(path(format!("/api/v0/instances/{instance}/")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "instances": {
                    "id": instance,
                    "label": "covenant-workload-x",
                    "actual_status": if ready { "running" } else { "loading" },
                    "image_uuid": IMAGE,
                    "image_runtype": "ssh_direct",
                    "gpu_name": "L40S",
                    "gpu_ram": 46068,
                    "verification": "verified",
                    "dph_total": 0.50,
                    "machine_id": machine_id,
                    "bundle_id": offer_id,
                    "public_ipaddr": "203.0.113.7",
                    "ssh_host": "ssh5.vast.ai",
                    "ssh_port": 40001,
                    "direct_port_start": 40000,
                    "direct_port_end": 40002,
                    "ports": {"22/tcp": [{"HostIp": "0.0.0.0", "HostPort": "40001"}]}
                }
            })))
            .mount(server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(format!("/api/v0/instances/{instance}/")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": true})))
            .mount(server)
            .await;
    }

    /// A market that lists one machine, rents it, reports it running,
    /// and records every destroy it is asked for.
    async fn market(ready: bool) -> MockServer {
        let server = MockServer::start().await;
        book_turns_over(&server, &[vec![offer_row(7, 70, 0.50)]]).await;
        host(&server, 7, 70, 99, ready).await;
        server
    }

    fn market_client(server: &MockServer) -> Arc<VastClient> {
        Arc::new(
            VastClient::new(
                covenant_compute_vast::VastConfig {
                    api_url: url::Url::parse(&format!("{}/api/v0/", server.uri())).unwrap(),
                    max_hourly_micros: 900_000,
                    ..covenant_compute_vast::VastConfig::default()
                },
                covenant_compute_vast::ApiToken::new("test-token-not-a-secret").unwrap(),
            )
            .unwrap(),
        )
    }

    fn market_config() -> BrokerConfig {
        BrokerConfig {
            image: IMAGE.into(),
            max_hourly_micros: 900_000,
            ready_timeout: Duration::from_secs(2),
            ready_poll_interval: Duration::from_millis(20),
        }
    }

    async fn destroys(server: &MockServer) -> usize {
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.method == wiremock::http::Method::DELETE)
            .count()
    }

    async fn calls(server: &MockServer, method: wiremock::http::Method, path: &str) -> usize {
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.method == method && r.url.path() == path)
            .count()
    }

    async fn takes(server: &MockServer, offer_id: u64) -> usize {
        calls(
            server,
            wiremock::http::Method::PUT,
            &format!("/api/v0/asks/{offer_id}/"),
        )
        .await
    }

    async fn searches(server: &MockServer) -> usize {
        calls(server, wiremock::http::Method::POST, "/api/v0/bundles/").await
    }

    fn buyer_key() -> &'static str {
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMockedPublicKeyMaterial buyer@test"
    }

    fn rented_instance(broker: &BrokerSessionBackend, job_id: Uuid) -> Option<u64> {
        broker.live.lock().get(&job_id).map(|r| r.instance_id)
    }

    #[tokio::test]
    async fn a_session_rents_a_real_machine_and_hands_back_its_address() {
        let server = market(true).await;
        let broker = BrokerSessionBackend::new(market_client(&server), market_config());
        let job = lease_job(Some(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMockedPublicKeyMaterial buyer@test",
        ));

        let access = broker.open(&job).await.expect("a machine is rented");
        assert_eq!(access.job_id, job.job_id);
        assert_eq!(access.endpoint, "ssh -p 40001 root@ssh5.vast.ai");
        assert!(
            access.note.as_deref().unwrap_or_default().contains("L40S"),
            "the renter is told what they got: {access:?}"
        );
        assert_eq!(
            destroys(&server).await,
            0,
            "a live session is not destroyed"
        );

        // Closing the session returns the machine.
        broker.close(job.job_id).await;
        assert_eq!(destroys(&server).await, 1);
        assert!(broker.live.lock().is_empty());

        // Closing twice does not double-destroy.
        broker.close(job.job_id).await;
        assert_eq!(destroys(&server).await, 1);
    }

    #[tokio::test]
    async fn a_machine_that_never_comes_up_is_destroyed_not_leaked() {
        // The failure that actually costs money: the box is created and
        // billing, then never answers. It must be released on the way
        // out, and the lease must fail so the buyer is refunded rather
        // than charged for a machine they never reached.
        let server = market(false).await;
        let broker = BrokerSessionBackend::new(market_client(&server), market_config());
        let job = lease_job(Some(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMockedPublicKeyMaterial buyer@test",
        ));

        let err = broker
            .open(&job)
            .await
            .expect_err("a machine that never answers is not a session");
        assert!(
            matches!(&err, ExecutorError::Failed(msg) if msg.contains("did not become reachable")),
            "got: {err}"
        );
        assert_eq!(
            destroys(&server).await,
            1,
            "the instance the broker is being billed for must be destroyed"
        );
        assert!(
            broker.live.lock().is_empty(),
            "a failed open leaves no session behind"
        );
    }

    #[tokio::test]
    async fn an_offer_that_goes_between_survey_and_take_costs_an_attempt_not_the_lease() {
        // The failure that killed two of three canary runs: the cheapest
        // offer is surveyed, then re-let before the take confirms it.
        let server = MockServer::start().await;
        book_turns_over(
            &server,
            &[
                vec![offer_row(7, 70, 0.50), offer_row(8, 80, 0.60)],
                vec![offer_row(8, 80, 0.60)],
            ],
        )
        .await;
        host(&server, 7, 70, 99, true).await;
        host(&server, 8, 80, 98, true).await;

        let broker = BrokerSessionBackend::new(market_client(&server), market_config());
        let job = lease_job(Some(buyer_key()));
        let access = broker
            .open(&job)
            .await
            .expect("the second-best offer is still a machine");

        assert_eq!(access.job_id, job.job_id);
        assert_eq!(rented_instance(&broker, job.job_id), Some(98));
        assert_eq!(takes(&server, 7).await, 0, "the gone offer is never taken");
        assert_eq!(takes(&server, 8).await, 1);
        assert_eq!(destroys(&server).await, 0, "nothing was rented to leak");
    }

    #[tokio::test]
    async fn a_machine_that_lost_one_offer_is_not_tried_again_under_another() {
        // Hosts list the same machine under several offers. Without the
        // rejection carried between attempts the broker walks straight
        // back onto the box that just refused it.
        let server = MockServer::start().await;
        book_turns_over(
            &server,
            &[
                vec![
                    offer_row(7, 70, 0.50),
                    offer_row(9, 70, 0.51),
                    offer_row(8, 80, 0.60),
                ],
                vec![offer_row(9, 70, 0.51), offer_row(8, 80, 0.60)],
            ],
        )
        .await;
        host(&server, 7, 70, 99, true).await;
        host(&server, 9, 70, 97, true).await;
        host(&server, 8, 80, 98, true).await;

        let broker = BrokerSessionBackend::new(market_client(&server), market_config());
        let job = lease_job(Some(buyer_key()));
        broker.open(&job).await.expect("a machine is rented");

        assert_eq!(rented_instance(&broker, job.job_id), Some(98));
        assert_eq!(
            takes(&server, 9).await,
            0,
            "machine 70 already lost this lease; its other offer must not be taken"
        );
        assert_eq!(takes(&server, 8).await, 1);
    }

    #[tokio::test]
    async fn a_host_that_will_not_hold_the_buyers_key_is_destroyed_before_the_next_try() {
        // This attempt creates a real instance before it fails. It bills
        // from creation, so it has to be gone before the broker moves on.
        let server = MockServer::start().await;
        book_turns_over(
            &server,
            &[vec![offer_row(7, 70, 0.50), offer_row(8, 80, 0.60)]],
        )
        .await;
        host_that_drops_the_key(&server, 7, 70, 99).await;
        host(&server, 8, 80, 98, true).await;

        let broker = BrokerSessionBackend::new(market_client(&server), market_config());
        let job = lease_job(Some(buyer_key()));
        broker
            .open(&job)
            .await
            .expect("the next host takes the key");

        assert_eq!(rented_instance(&broker, job.job_id), Some(98));
        assert_eq!(
            calls(
                &server,
                wiremock::http::Method::DELETE,
                "/api/v0/instances/99/"
            )
            .await,
            1,
            "the instance from the failed attempt must not be left billing"
        );
        assert_eq!(
            calls(
                &server,
                wiremock::http::Method::DELETE,
                "/api/v0/instances/98/"
            )
            .await,
            0,
            "the live session is not destroyed"
        );
    }

    #[tokio::test]
    async fn a_lost_create_response_fails_the_lease_without_renting_a_second_machine() {
        // The take's reply is lost, so offer 7 may already be a billing
        // instance we cannot name. Unlike a refused take, this is not a lost
        // race: walking on to offer 8 would rent a second machine while the
        // first leaks. The lease fails instead, and the label is logged for
        // an operator to reconcile by.
        let server = MockServer::start().await;
        book_turns_over(
            &server,
            &[vec![offer_row(7, 70, 0.50), offer_row(8, 80, 0.60)]],
        )
        .await;
        host_with_a_lost_create_response(&server, 7).await;
        host(&server, 8, 80, 98, true).await;

        let broker = BrokerSessionBackend::new(market_client(&server), market_config());
        let job = lease_job(Some(buyer_key()));
        let err = broker
            .open(&job)
            .await
            .expect_err("an ambiguous create is not a session");

        assert!(
            matches!(&err, ExecutorError::Failed(msg) if msg.contains("could not rent a machine")),
            "got: {err}"
        );
        assert_eq!(
            takes(&server, 7).await,
            1,
            "the ambiguous offer is taken once"
        );
        assert_eq!(
            takes(&server, 8).await,
            0,
            "a lost create must not lead the broker to rent a second machine"
        );
        assert!(
            broker.live.lock().is_empty(),
            "a failed open leaves no session behind"
        );
    }

    #[tokio::test]
    async fn the_market_is_surveyed_again_when_every_held_offer_has_gone() {
        // A book that turned over completely is the case a deeper walk
        // cannot fix: every offer in hand is stale, so ask for new ones.
        let server = MockServer::start().await;
        book_turns_over(
            &server,
            &[
                vec![offer_row(7, 70, 0.50)],
                vec![],
                vec![offer_row(8, 80, 0.60)],
            ],
        )
        .await;
        host(&server, 7, 70, 99, true).await;
        host(&server, 8, 80, 98, true).await;

        let broker = BrokerSessionBackend::new(market_client(&server), market_config());
        let job = lease_job(Some(buyer_key()));
        broker
            .open(&job)
            .await
            .expect("the second survey finds a machine");

        assert_eq!(rented_instance(&broker, job.job_id), Some(98));
        assert_eq!(
            searches(&server).await,
            4,
            "two surveys, each with a confirming search before its take"
        );
    }

    #[tokio::test]
    async fn a_market_that_refuses_everyone_is_given_up_on_after_a_bounded_walk() {
        // Every offer confirms and every take is refused. The broker must
        // stop rather than work down a book of sixty-four while the
        // buyer's window burns.
        let server = MockServer::start().await;
        let book: Vec<_> = (0..6)
            .map(|n| offer_row(7 + n, 70 + n, 0.50 + f64::from(n as u32) / 100.0))
            .collect();
        book_turns_over(&server, &[book]).await;
        for n in 0..6 {
            host_already_taken(&server, 7 + n).await;
        }

        let broker = BrokerSessionBackend::new(market_client(&server), market_config());
        let job = lease_job(Some(buyer_key()));
        let err = broker
            .open(&job)
            .await
            .expect_err("no machine was ever handed over");

        assert!(
            matches!(&err, ExecutorError::Failed(msg) if msg.contains("went while we were taking")),
            "the operator is told the market took them, not that the broker broke: {err}"
        );
        let mut attempted = 0;
        for n in 0..6 {
            attempted += takes(&server, 7 + n).await;
        }
        assert_eq!(attempted, RENTAL_ATTEMPTS as usize);
        assert_eq!(destroys(&server).await, 0, "no instance was ever created");
        assert!(broker.live.lock().is_empty());
    }

    #[tokio::test]
    async fn a_market_priced_out_of_reach_is_reported_without_renting_anything() {
        // The book clears the GPU bar but not the price ceiling. There is
        // nothing admissible to take, so the broker takes nothing and tells
        // the buyer why — priced out, not the wrong class — instead of
        // reporting a race it never ran.
        let server = MockServer::start().await;
        book_turns_over(&server, &[vec![offer_row(7, 70, 1.50)]]).await;

        let broker = BrokerSessionBackend::new(market_client(&server), market_config());
        let job = lease_job(Some(buyer_key()));
        let err = broker
            .open(&job)
            .await
            .expect_err("nothing in the book was affordable");

        let ExecutorError::Failed(msg) = &err else {
            panic!("a priced-out book is a failure, not a timeout: {err}");
        };
        assert!(
            msg.contains("no admissible GPU offer")
                && msg.contains("1 priced out")
                && msg.contains("0 wrong class"),
            "the buyer is told the offer was too dear, not the wrong hardware: {err}"
        );
        assert_eq!(
            takes(&server, 7).await,
            0,
            "an offer over the ceiling is never taken"
        );
        assert_eq!(destroys(&server).await, 0, "nothing was rented to leak");
        assert_eq!(
            searches(&server).await,
            1,
            "an empty admissible book is not surveyed a second time"
        );
        assert!(broker.live.lock().is_empty());
    }

    #[tokio::test]
    async fn an_attach_failure_that_cannot_be_cleaned_up_does_not_rent_a_second_machine() {
        // The take creates a billing instance, the buyer's key will not
        // attach, and the destroy that would return the machine also fails.
        // The instance is leaking under an id we hold, so — like a lost
        // create — walking on to offer 8 would rent a second machine on top
        // of the first. The lease fails instead and the leak is surfaced.
        let server = MockServer::start().await;
        book_turns_over(
            &server,
            &[vec![offer_row(7, 70, 0.50), offer_row(8, 80, 0.60)]],
        )
        .await;
        host_whose_cleanup_fails(&server, 7, 99).await;
        host(&server, 8, 80, 98, true).await;

        let broker = BrokerSessionBackend::new(market_client(&server), market_config());
        let job = lease_job(Some(buyer_key()));
        let err = broker
            .open(&job)
            .await
            .expect_err("a leaked instance is not a session");

        assert!(
            matches!(&err, ExecutorError::Failed(msg) if msg.contains("could not rent a machine")),
            "got: {err}"
        );
        assert_eq!(
            takes(&server, 7).await,
            1,
            "the machine that leaked is taken once"
        );
        assert_eq!(
            takes(&server, 8).await,
            0,
            "a leaked instance must not lead the broker to rent a second machine"
        );
        assert_eq!(
            calls(
                &server,
                wiremock::http::Method::DELETE,
                "/api/v0/instances/99/"
            )
            .await,
            1,
            "cleanup was attempted once and failed; that is what makes this a leak, not a race"
        );
        assert!(
            broker.live.lock().is_empty(),
            "a failed open leaves no session behind"
        );
    }

    #[tokio::test]
    async fn the_machine_is_opened_to_the_buyers_own_key() {
        // The key signed into the lease terms is the one the rented machine
        // is opened to — not a broker-held key, not an empty one. A box the
        // operator can also log into is not a rental, so pin that the buyer's
        // own key is what reaches the host.
        let server = market(true).await;
        let broker = BrokerSessionBackend::new(market_client(&server), market_config());
        let job = lease_job(Some(buyer_key()));
        broker.open(&job).await.expect("a machine is rented");

        let attach = server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .find(|r| {
                r.method == wiremock::http::Method::POST
                    && r.url.path() == "/api/v0/instances/99/ssh/"
            })
            .expect("the buyer's key is attached to the rented machine");
        let body: serde_json::Value =
            serde_json::from_slice(&attach.body).expect("the attach carries a json body");
        assert_eq!(
            body["ssh_key"],
            json!(buyer_key()),
            "the machine is opened to the buyer's own key and no other"
        );
    }

    #[tokio::test]
    async fn a_refused_teardown_is_retried_until_the_machine_is_confirmed_gone() {
        // A leaked instance bills the broker until someone notices, so
        // teardown does not log the first refusal and walk away: it retries
        // until the machine is confirmed gone. The first destroy is refused
        // and the release only stops once a later one is accepted.
        let server = market(true).await;
        Mock::given(method("DELETE"))
            .and(path("/api/v0/instances/99/"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;

        let broker = BrokerSessionBackend::new(market_client(&server), market_config());
        let job = lease_job(Some(buyer_key()));
        broker.open(&job).await.expect("a machine is rented");
        broker.close(job.job_id).await;

        assert_eq!(
            calls(
                &server,
                wiremock::http::Method::DELETE,
                "/api/v0/instances/99/"
            )
            .await,
            2,
            "the refused teardown is retried and stops once the machine is confirmed gone"
        );
        assert!(
            broker.live.lock().is_empty(),
            "the closed session is no longer tracked as live"
        );
    }
}
