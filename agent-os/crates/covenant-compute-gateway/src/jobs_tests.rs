// Paid-job lifecycle tests against a scripted payment rail and coordinator.
use super::*;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex as Script;

const ORIGIN: &str = "https://jobs.example";
const OPERATOR: &str = "approved-operator";
const OBJECTIVE: &str = "Compare how three agent wallets cap what an agent can spend.";

fn transaction(header: &str) -> String {
    crate::payment_payload(header).unwrap()["payload"]["transaction"]
        .as_str()
        .unwrap()
        .into()
}

#[derive(Default)]
struct Rail {
    settles: Script<VecDeque<SettlementStatus>>,
    refund_statuses: Script<VecDeque<RefundStatus>>,
    verified: Script<Vec<Value>>,
    refunds: AtomicUsize,
}
#[async_trait]
impl JobsRail for Rail {
    fn network(&self) -> &str {
        "solana:test"
    }
    fn asset(&self) -> &str {
        "USDC"
    }
    fn pay_to(&self) -> &str {
        "jobs-merchant"
    }
    fn fee_payer(&self) -> &str {
        "fee-payer"
    }
    async fn check_price(&self, resource_url: &str, amount: u64, _: u64) -> Result<()> {
        assert_eq!(resource_url, "https://jobs.example/v1/jobs/research");
        assert_eq!(amount, 5_000_000);
        Ok(())
    }
    // Like the real rail, the id covers the signed transaction only, not the
    // resource and extensions a payment echoes.
    fn authorization_id(&self, header: &str) -> Result<String> {
        Ok(format!("auth-{}", transaction(header)))
    }
    async fn verify(&self, header: &str, terms: &PaymentTerms) -> Result<VerifiedPayment> {
        let payload = crate::payment_payload(header)?;
        if payload["accepted"] != terms.accepted() {
            return Err(GatewayError::Payment("terms changed".into()));
        }
        self.verified.lock().unwrap().push(payload);
        let tx = transaction(header);
        Ok(VerifiedPayment {
            payer: "payer".into(),
            authorization_id: format!("auth-{tx}"),
            transaction_id: format!("sig-{tx}"),
            encoded_transaction: "sealed".into(),
        })
    }
    async fn settle(
        &self,
        payment: &VerifiedPayment,
        _: &PaymentTerms,
    ) -> Result<SettlementStatus> {
        Ok(self
            .settles
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(SettlementStatus::Finalized {
                transaction_id: payment.transaction_id.clone(),
            }))
    }
    async fn refund(&self, _: &VerifiedPayment, _: &PaymentTerms) -> Result<(String, String)> {
        let n = self.refunds.fetch_add(1, Ordering::SeqCst) + 1;
        Ok((format!("refund-{n}"), format!("sealed-refund-{n}")))
    }
    async fn refund_status(
        &self,
        _: &VerifiedPayment,
        _: &PaymentTerms,
        refund: &str,
    ) -> Result<RefundStatus> {
        Ok(self
            .refund_statuses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(RefundStatus::Finalized {
                transaction_id: refund.trim_start_matches("sealed-").into(),
            }))
    }
}

#[derive(Clone, Copy)]
enum Outcome {
    Report,
    Refund,
    Retry,
    Unapproved,
}
struct Runner {
    identity: LocalIdentity,
    signs: AtomicUsize,
    runs: Script<Vec<Uuid>>,
    outcomes: Script<VecDeque<Outcome>>,
}
impl Runner {
    fn new(outcomes: &[Outcome]) -> Self {
        Self {
            identity: LocalIdentity::generate("jobs-test@local"),
            signs: AtomicUsize::new(0),
            runs: Script::new(Vec::new()),
            outcomes: Script::new(outcomes.iter().copied().collect()),
        }
    }
}
#[async_trait]
impl JobRunner for Runner {
    fn sign(&self, offer: &Offer, request: &Value) -> Result<SignedJobEnvelope> {
        self.signs.fetch_add(1, Ordering::SeqCst);
        let config = BuyerConfig {
            coordinator_url: "https://coordinator.example".into(),
            poll_interval: Duration::from_millis(1),
            referral_code: None,
            rpc_url: None,
        };
        covenant_compute_buyer::sign_envelope(
            &config,
            &self.identity,
            JobRequest {
                kind: JobKind::BatchJob,
                input: swarm_input(&research_order(request)),
                model: Some(SWARM_MODEL.into()),
                gpu_class: None,
                min_vram_gb: None,
                min_reputation_bps: None,
                price_micro_usdc: offer.offer_micro_usdc,
                deadline_ms: offer.deadline_ms,
            },
        )
        .map_err(|e| GatewayError::Invalid(e.to_string()))
    }
    async fn run(
        &self,
        envelope: &SignedJobEnvelope,
    ) -> std::result::Result<RunOutput, RunFailure> {
        self.runs.lock().unwrap().push(envelope.payload.job_id);
        let outcome = self
            .outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Outcome::Report);
        let output = |operator: &str| RunOutput {
            job_id: envelope.payload.job_id,
            operator: operator.into(),
            result: json!({"receipt":{"signer_pubkey_b58":operator},"output":[
                {"type":"json","value":{"swarm":{"jobId":"imd-1","explorer":"https://explorer.imd.fun/jobs/imd-1"},
                    "outcome":{"state":"completed","files":[{"name":"report","path":"artifacts/report.md",
                        "mediaType":"text/markdown","bytes":9,"hash":"ab","url":"https://api.imd.fun/artifacts/ab",
                        "content":"# Report\n"}]}}},
                {"type":"text","text":"IMD job imd-1 completed"}]}),
        };
        match outcome {
            Outcome::Report => Ok(output(OPERATOR)),
            Outcome::Unapproved => Ok(output("someone-else")),
            Outcome::Refund => Err(RunFailure::Refund("job 1 was not served: refunded".into())),
            Outcome::Retry => Err(RunFailure::Retry("coordinator unreachable".into())),
        }
    }
}

fn offer() -> Offer {
    Offer {
        kind: Kind::Research,
        price_atomic: 5_000_000,
        offer_micro_usdc: 4_000_000,
        deadline_ms: 7_200_000,
        approved_operators: vec![OPERATOR.into()],
        max_running: 2,
    }
}
fn open(directory: &Path, rail: Arc<Rail>, runner: Arc<Runner>) -> Arc<Jobs> {
    Arc::new(Jobs::open(directory, ORIGIN, rail, runner, vec![offer()]).unwrap())
}
fn request() -> Value {
    json!({"objective":OBJECTIVE})
}
fn header(challenge: &Value, tx: &str) -> String {
    let payload = json!({"x402Version":2,"resource":challenge["resource"],"accepted":challenge["accepts"][0],
        "payload":{"transaction":tx}});
    base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&payload).unwrap())
}
async fn quote(jobs: &Arc<Jobs>) -> Value {
    match jobs
        .handle(Kind::Research, request(), None, crate::now_ms())
        .await
        .unwrap()
    {
        JobReply::PaymentRequired(challenge) => challenge,
        other => panic!("expected a quote, got {other:?}"),
    }
}
async fn pay(jobs: &Arc<Jobs>, challenge: &Value, tx: &str) -> Result<JobReply> {
    jobs.handle(
        Kind::Research,
        request(),
        Some(&header(challenge, tx)),
        crate::now_ms(),
    )
    .await
}
fn job_id(challenge: &Value) -> Uuid {
    Uuid::parse_str(
        challenge["accepts"][0]["extra"]["quoteId"]
            .as_str()
            .unwrap(),
    )
    .unwrap()
}
/// The job's status once it stops changing, ticking the worker meanwhile.
async fn settled(jobs: &Arc<Jobs>, id: Uuid, until: &str) -> Value {
    for _ in 0..200 {
        let view = jobs.status(id).await.unwrap();
        if view["status"] == until {
            return view;
        }
        jobs.tick(crate::now_ms()).await;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!(
        "job never reached {until}: {}",
        jobs.status(id).await.unwrap()
    );
}

#[tokio::test]
async fn a_quote_names_the_job_price_and_its_listing() {
    let dir = tempfile::tempdir().unwrap();
    let jobs = open(dir.path(), Arc::default(), Arc::new(Runner::new(&[])));
    let challenge = quote(&jobs).await;
    let accepted = &challenge["accepts"][0];
    assert_eq!(accepted["amount"], "5000000");
    assert_eq!(accepted["payTo"], "jobs-merchant");
    assert_eq!(accepted["extra"]["feePayer"], "fee-payer");
    assert_eq!(accepted["extra"]["memo"].as_str().unwrap().len(), 32);
    assert_eq!(
        challenge["resource"]["url"],
        "https://jobs.example/v1/jobs/research"
    );
    assert_eq!(challenge["resource"]["serviceName"], "Covenant Compute");
    let bazaar = &challenge["extensions"]["bazaar"];
    assert_eq!(bazaar["info"]["input"]["method"], "POST");
    // The listing's example is fixed; a customer's question never reaches it.
    assert!(!bazaar.to_string().contains(OBJECTIVE));
}

#[tokio::test]
async fn requests_that_would_fail_after_payment_are_refused_before_a_quote() {
    let dir = tempfile::tempdir().unwrap();
    let jobs = open(dir.path(), Arc::default(), Arc::new(Runner::new(&[])));
    for body in [
        json!({"objective":"too short"}),
        json!({"objective":OBJECTIVE,"extra":true}),
        json!({"question":OBJECTIVE}),
        json!([OBJECTIVE]),
    ] {
        let refused = jobs
            .handle(Kind::Research, body, None, crate::now_ms())
            .await;
        assert!(
            matches!(refused, Err(GatewayError::Invalid(_))),
            "{refused:?}"
        );
    }
}

#[tokio::test]
async fn a_paid_job_settles_then_runs_and_its_status_returns_the_report() {
    let dir = tempfile::tempdir().unwrap();
    let rail = Arc::new(Rail::default());
    let runner = Arc::new(Runner::new(&[Outcome::Report]));
    let jobs = open(dir.path(), rail.clone(), runner.clone());
    let challenge = quote(&jobs).await;
    let id = job_id(&challenge);
    let JobReply::Status { view, settlement } = pay(&jobs, &challenge, "tx1").await.unwrap() else {
        panic!("expected a status");
    };
    assert_eq!(
        view["status_url"],
        format!("https://jobs.example/v1/jobs/{id}")
    );
    assert_eq!(settlement.unwrap()["transaction"], "sig-tx1");
    let done = settled(&jobs, id, "completed").await;
    assert_eq!(done["result"]["report"], "# Report\n");
    assert_eq!(done["result"]["imd"]["job_id"], "imd-1");
    assert_eq!(done["result"]["covenant"]["operator"], OPERATOR);
    assert_eq!(done["payment"]["transaction"], "sig-tx1");
    assert_eq!(runner.signs.load(Ordering::SeqCst), 1);
    assert_eq!(runner.runs.lock().unwrap().len(), 1);
    // The verifier saw the listing this gateway issued.
    assert_eq!(
        rail.verified.lock().unwrap()[0]["resource"],
        challenge["resource"]
    );
    // Repeating the paid request reports on the same job; another payment
    // for the same quote is refused.
    let JobReply::Status { view, .. } = pay(&jobs, &challenge, "tx1").await.unwrap() else {
        panic!("expected a status");
    };
    assert_eq!(view["status"], "completed");
    assert!(matches!(
        pay(&jobs, &challenge, "tx2").await,
        Err(GatewayError::Payment(_))
    ));
}

#[tokio::test]
async fn the_issued_listing_replaces_whatever_the_payment_echoes() {
    let dir = tempfile::tempdir().unwrap();
    let rail = Arc::new(Rail::default());
    let jobs = open(dir.path(), rail.clone(), Arc::new(Runner::new(&[])));
    let challenge = quote(&jobs).await;
    let mut forged = challenge.clone();
    forged["resource"]["description"] = json!("Free money");
    forged["extensions"] = json!({"bazaar":{"info":"forged"}});
    pay(&jobs, &forged, "tx1").await.unwrap();
    let verified = rail.verified.lock().unwrap()[0].clone();
    assert_eq!(verified["resource"], challenge["resource"]);
    assert_eq!(verified["extensions"], challenge["extensions"]);
}

#[tokio::test]
async fn a_payment_that_never_lands_runs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let rail = Arc::new(Rail::default());
    rail.settles
        .lock()
        .unwrap()
        .push_back(SettlementStatus::Failed);
    let runner = Arc::new(Runner::new(&[]));
    let jobs = open(dir.path(), rail, runner.clone());
    let challenge = quote(&jobs).await;
    assert!(matches!(
        pay(&jobs, &challenge, "tx1").await,
        Err(GatewayError::Payment(_))
    ));
    assert_eq!(
        jobs.status(job_id(&challenge)).await.unwrap()["status"],
        "unpaid"
    );
    assert_eq!(runner.signs.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_slow_payment_answers_with_a_status_and_starts_the_job_once_final() {
    let dir = tempfile::tempdir().unwrap();
    let rail = Arc::new(Rail::default());
    rail.settles
        .lock()
        .unwrap()
        .extend([SettlementStatus::Pending, SettlementStatus::Pending]);
    let runner = Arc::new(Runner::new(&[Outcome::Report]));
    let jobs = open(dir.path(), rail, runner.clone());
    let challenge = quote(&jobs).await;
    let JobReply::Status { view, settlement } = pay(&jobs, &challenge, "tx1").await.unwrap() else {
        panic!("expected a status");
    };
    assert_eq!(view["status"], "paying");
    assert!(settlement.is_none());
    assert_eq!(runner.signs.load(Ordering::SeqCst), 0);
    settled(&jobs, job_id(&challenge), "completed").await;
}

#[tokio::test]
async fn a_job_the_coordinator_refunds_is_refunded_to_the_payer_once() {
    let dir = tempfile::tempdir().unwrap();
    let rail = Arc::new(Rail::default());
    let jobs = open(
        dir.path(),
        rail.clone(),
        Arc::new(Runner::new(&[Outcome::Refund])),
    );
    let challenge = quote(&jobs).await;
    pay(&jobs, &challenge, "tx1").await.unwrap();
    let refunded = settled(&jobs, job_id(&challenge), "refunded").await;
    assert_eq!(refunded["refund"]["transaction"], "refund-1");
    assert_eq!(refunded["refund"]["amount"], "5000000");
    assert!(refunded["message"].as_str().unwrap().contains("returned"));
    for _ in 0..5 {
        jobs.tick(crate::now_ms()).await;
    }
    assert_eq!(rail.refunds.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_refund_that_expires_is_rebuilt_and_only_a_finalized_one_ends_it() {
    let dir = tempfile::tempdir().unwrap();
    let rail = Arc::new(Rail::default());
    rail.refund_statuses
        .lock()
        .unwrap()
        .extend([RefundStatus::Pending, RefundStatus::Void]);
    let jobs = open(
        dir.path(),
        rail.clone(),
        Arc::new(Runner::new(&[Outcome::Refund])),
    );
    let challenge = quote(&jobs).await;
    pay(&jobs, &challenge, "tx1").await.unwrap();
    let refunded = settled(&jobs, job_id(&challenge), "refunded").await;
    assert_eq!(refunded["refund"]["transaction"], "refund-2");
    assert_eq!(rail.refunds.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_result_from_an_unapproved_operator_is_refunded_not_delivered() {
    let dir = tempfile::tempdir().unwrap();
    let jobs = open(
        dir.path(),
        Arc::default(),
        Arc::new(Runner::new(&[Outcome::Unapproved])),
    );
    let challenge = quote(&jobs).await;
    pay(&jobs, &challenge, "tx1").await.unwrap();
    let refunded = settled(&jobs, job_id(&challenge), "refunded").await;
    assert!(refunded.get("result").is_none());
}

#[tokio::test]
async fn an_inconclusive_run_retries_the_same_signed_job_after_a_delay() {
    let dir = tempfile::tempdir().unwrap();
    let runner = Arc::new(Runner::new(&[Outcome::Retry, Outcome::Report]));
    let jobs = open(dir.path(), Arc::default(), runner.clone());
    let challenge = quote(&jobs).await;
    let id = job_id(&challenge);
    pay(&jobs, &challenge, "tx1").await.unwrap();
    for _ in 0..50 {
        if !runner.runs.lock().unwrap().is_empty() && jobs.active.lock().await.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    jobs.tick(crate::now_ms()).await;
    assert_eq!(
        runner.runs.lock().unwrap().len(),
        1,
        "retried before its delay"
    );
    assert_eq!(jobs.status(id).await.unwrap()["status"], "running");
    jobs.retry_at.lock().await.clear();
    settled(&jobs, id, "completed").await;
    let runs = runner.runs.lock().unwrap().clone();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0], runs[1]);
    assert_eq!(runner.signs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_restart_resumes_a_running_job_with_its_stored_signed_job() {
    let dir = tempfile::tempdir().unwrap();
    let first = Arc::new(Runner::new(&[Outcome::Retry]));
    let jobs = open(dir.path(), Arc::default(), first.clone());
    let challenge = quote(&jobs).await;
    let id = job_id(&challenge);
    pay(&jobs, &challenge, "tx1").await.unwrap();
    for _ in 0..50 {
        if !first.runs.lock().unwrap().is_empty() && jobs.active.lock().await.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let signed = first.runs.lock().unwrap()[0];
    drop(jobs);
    let second = Arc::new(Runner::new(&[Outcome::Report]));
    let jobs = open(dir.path(), Arc::default(), second.clone());
    settled(&jobs, id, "completed").await;
    assert_eq!(second.signs.load(Ordering::SeqCst), 0);
    assert_eq!(second.runs.lock().unwrap()[0], signed);
}

#[tokio::test]
async fn quotes_stop_while_the_kind_is_at_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let rail = Arc::new(Rail::default());
    rail.settles
        .lock()
        .unwrap()
        .extend([SettlementStatus::Pending, SettlementStatus::Pending]);
    let jobs = open(dir.path(), rail, Arc::new(Runner::new(&[])));
    for tx in ["tx1", "tx2"] {
        let challenge = quote(&jobs).await;
        pay(&jobs, &challenge, tx).await.unwrap();
    }
    assert!(matches!(
        jobs.handle(Kind::Research, request(), None, crate::now_ms())
            .await,
        Err(GatewayError::Unavailable(_))
    ));
}

#[tokio::test]
async fn a_payment_cannot_start_two_jobs_or_pay_for_another_request() {
    let dir = tempfile::tempdir().unwrap();
    let jobs = open(dir.path(), Arc::default(), Arc::new(Runner::new(&[])));
    let first = quote(&jobs).await;
    pay(&jobs, &first, "tx1").await.unwrap();
    let second = quote(&jobs).await;
    assert!(matches!(
        pay(&jobs, &second, "tx1").await,
        Err(GatewayError::Payment(_))
    ));
    let other = json!({"objective":"A different question that is long enough to quote."});
    assert!(matches!(
        jobs.handle(
            Kind::Research,
            other,
            Some(&header(&second, "tx3")),
            crate::now_ms()
        )
        .await,
        Err(GatewayError::Invalid(_))
    ));
}

#[tokio::test]
async fn a_result_is_dropped_after_its_retention_but_the_job_is_remembered() {
    let dir = tempfile::tempdir().unwrap();
    let jobs = open(
        dir.path(),
        Arc::default(),
        Arc::new(Runner::new(&[Outcome::Report])),
    );
    let challenge = quote(&jobs).await;
    let id = job_id(&challenge);
    pay(&jobs, &challenge, "tx1").await.unwrap();
    settled(&jobs, id, "completed").await;
    jobs.tick(crate::now_ms() + RESULT_RETENTION_MS + 1).await;
    let view = jobs.status(id).await.unwrap();
    assert_eq!(view["status"], "completed");
    assert!(view.get("result").is_none());
    let record = &jobs.store.lock().await.records[&id];
    assert!(record.request.is_none());
}

#[tokio::test]
async fn discovery_lists_each_job_with_its_price_merchant_and_free_status_read() {
    let dir = tempfile::tempdir().unwrap();
    let jobs = open(dir.path(), Arc::default(), Arc::new(Runner::new(&[])));
    let endpoints = jobs.manifest_endpoints(crate::now_ms()).await;
    assert_eq!(endpoints.len(), 1);
    let endpoint = &endpoints[0];
    let mut fields: Vec<_> = endpoint.as_object().unwrap().keys().cloned().collect();
    fields.sort();
    // The deploy acceptance check expects exactly these fields.
    assert_eq!(
        fields,
        [
            "description",
            "inputSchema",
            "method",
            "path",
            "payment",
            "paymentRequiredHeader",
            "paymentSignatureHeader",
            "price",
            "statusMethod",
            "statusPath",
            "url"
        ]
    );
    assert_eq!(endpoint["url"], "https://jobs.example/v1/jobs/research");
    assert_eq!(endpoint["payment"]["payTo"], "jobs-merchant");
    assert_eq!(endpoint["price"]["price_micro_usdc"], 5_000_000);
    let paths = jobs.openapi_paths(crate::now_ms()).await;
    let names: Vec<_> = paths.iter().map(|(path, _)| path.as_str()).collect();
    assert_eq!(names, ["/v1/jobs/research", "/v1/jobs/{id}"]);
    assert_eq!(
        paths[0].1["post"]["x-payment-info"]["price"]["amount"],
        "5.000000"
    );
}
