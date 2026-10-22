// State-machine restart/authorization/reserve tests, with real signed envelopes.
use super::*;
use covenant_compute_buyer::{sign_envelope, BuyerConfig, JobRequest};
use covenant_compute_protocol::JobKind;
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

// Quote preparation includes real signing and disk time. Sequential fixture
// calls must start after the persisted clock, even on a slow CI worker.
async fn fixture_now(gateway: &Gateway, minimum: u64) -> u64 {
    minimum.max(gateway.disk.lock().await.journal.watermark_ms + 1)
}
struct Rail {
    settles: AtomicUsize,
    finalized: std::sync::atomic::AtomicBool,
}
#[async_trait]
impl PaymentRail for Rail {
    fn network(&self) -> &str {
        "test:1"
    }
    fn asset(&self) -> &str {
        "USDC"
    }
    fn pay_to(&self) -> &str {
        "merchant"
    }
    fn authorization_id(&self, header: &str) -> Result<String> {
        Ok(header.into())
    }
    async fn verify(&self, header: &str, _: &PaymentTerms) -> Result<VerifiedPayment> {
        Ok(VerifiedPayment {
            payer: "payer".into(),
            authorization_id: header.into(),
            transaction_id: header.into(),
            encoded_transaction: header.into(),
        })
    }
    async fn settle(&self, p: &VerifiedPayment, _: &PaymentTerms) -> Result<SettlementStatus> {
        self.settles.fetch_add(1, Ordering::SeqCst);
        Ok(if self.finalized.load(Ordering::SeqCst) {
            SettlementStatus::Finalized {
                transaction_id: p.transaction_id.clone(),
            }
        } else {
            SettlementStatus::Pending
        })
    }
}
struct Execute {
    identity: LocalIdentity,
    calls: AtomicUsize,
}
#[async_trait]
impl Executor for Execute {
    fn prepare(&self, _: &Value) -> Result<PreparedExecution> {
        let config = BuyerConfig {
            coordinator_url: "http://localhost:1234".into(),
            poll_interval: std::time::Duration::from_millis(1),
            referral_code: None,
            rpc_url: None,
        };
        let envelope = sign_envelope(
            &config,
            &self.identity,
            JobRequest {
                kind: JobKind::InferenceCall,
                input: vec![Content::text("hi")],
                model: Some("model".into()),
                gpu_class: None,
                min_vram_gb: None,
                min_reputation_bps: None,
                price_micro_usdc: 10,
                deadline_ms: 30_000,
            },
        )
        .unwrap();
        Ok(PreparedExecution {
            operation: None,
            buyer: self.identity.agent_id().pubkey_base58(),
            coordinator: config.coordinator_url,
            job_envelopes: vec![envelope],
            maximum_cost_micro_usdc: 10,
        })
    }
    async fn execute(&self, p: &PreparedExecution) -> std::result::Result<Value, ExecutionFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"job":p.job_envelopes[0].payload.job_id}))
    }
}
#[tokio::test]
async fn restart_reuses_result_reconciles_and_expires_without_repurchase() {
    let directory = tempfile::tempdir().unwrap();
    let rail = Arc::new(Rail {
        settles: AtomicUsize::new(0),
        finalized: std::sync::atomic::AtomicBool::new(false),
    });
    let executor = Arc::new(Execute {
        identity: LocalIdentity::generate("test@local"),
        calls: AtomicUsize::new(0),
    });
    let gateway = Gateway::open(
        directory.path(),
        "buyer/coord",
        10,
        12,
        rail.clone(),
        executor.clone(),
    )
    .unwrap();
    let request = json!({"prompt":"hi"});
    let GatewayReply::PaymentRequired(Quote { terms, .. }) = gateway
        .handle(
            request.clone(),
            None,
            None,
            fixture_now(&gateway, 1000).await,
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(matches!(
        gateway
            .handle(
                request.clone(),
                Some(terms.quote_id),
                Some("tx1"),
                fixture_now(&gateway, 1001).await
            )
            .await
            .unwrap(),
        GatewayReply::Pending { .. }
    ));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    assert!(gateway
        .handle(
            json!({"prompt":"different"}),
            Some(terms.quote_id),
            Some("tx1"),
            fixture_now(&gateway, 1002).await
        )
        .await
        .is_err());
    assert!(gateway
        .handle(
            request.clone(),
            Some(terms.quote_id),
            None,
            fixture_now(&gateway, 1003).await
        )
        .await
        .is_err());
    assert!(gateway
        .handle(
            request.clone(),
            Some(terms.quote_id),
            Some("tx2"),
            fixture_now(&gateway, 1004).await
        )
        .await
        .is_err());
    // Exhausted reserve now refuses even a new unpaid quote.
    assert!(gateway
        .handle(
            request.clone(),
            None,
            None,
            fixture_now(&gateway, 1005).await
        )
        .await
        .is_err());
    assert!(gateway
        .handle(
            request.clone(),
            None,
            Some("tx3"),
            fixture_now(&gateway, 1006).await
        )
        .await
        .is_err());
    assert!(Gateway::open(
        directory.path(),
        "buyer/coord",
        10,
        12,
        rail.clone(),
        executor.clone()
    )
    .is_err());
    drop(gateway);
    rail.finalized.store(true, Ordering::SeqCst);
    let gateway = Gateway::open(
        directory.path(),
        "buyer/coord",
        10,
        12,
        rail.clone(),
        executor.clone(),
    )
    .unwrap();
    assert!(matches!(
        gateway
            .handle(
                request.clone(),
                Some(terms.quote_id),
                Some("tx1"),
                fixture_now(&gateway, 1007).await
            )
            .await
            .unwrap(),
        GatewayReply::Complete { .. }
    ));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        gateway
            .handle(
                request.clone(),
                Some(terms.quote_id),
                Some("tx1"),
                fixture_now(&gateway, 0).await + 300_000
            )
            .await
            .unwrap(),
        GatewayReply::Gone
    ));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    assert_eq!(rail.settles.load(Ordering::SeqCst), 2);
    assert!(gateway
        .handle(request, Some(terms.quote_id), Some("tx1"), 1000)
        .await
        .is_err());
}

struct UncertainExecute {
    inner: Execute,
    seen: std::sync::Mutex<Vec<Uuid>>,
    fail_unpaid: bool,
}

struct SellerLossExecute(Execute);
#[async_trait]
impl Executor for SellerLossExecute {
    fn prepare(&self, request: &Value) -> Result<PreparedExecution> {
        self.0.prepare(request)
    }
    async fn execute(&self, _: &PreparedExecution) -> std::result::Result<Value, ExecutionFailure> {
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        Err(ExecutionFailure {
            message: "verified native result used an unapproved operator".into(),
            definitely_unpaid: false,
            terminal_seller_loss: true,
        })
    }
}

#[tokio::test]
async fn unapproved_operator_result_is_a_terminal_seller_loss_without_customer_settlement() {
    let directory = tempfile::tempdir().unwrap();
    let rail = Arc::new(Rail {
        settles: AtomicUsize::new(0),
        finalized: std::sync::atomic::AtomicBool::new(true),
    });
    let executor = Arc::new(SellerLossExecute(Execute {
        identity: LocalIdentity::generate("pin-test@local"),
        calls: AtomicUsize::new(0),
    }));
    let gateway = Gateway::open(
        directory.path(),
        "pinned-operator",
        10,
        12,
        rail.clone(),
        executor.clone(),
    )
    .unwrap();
    let request = json!({"prompt":"same"});
    let GatewayReply::PaymentRequired(Quote { terms: quote, .. }) = gateway
        .handle(
            request.clone(),
            None,
            None,
            fixture_now(&gateway, 100).await,
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(matches!(
        gateway
            .handle(
                request.clone(),
                Some(quote.quote_id),
                Some("authorization"),
                fixture_now(&gateway, 101).await
            )
            .await
            .unwrap(),
        GatewayReply::Failed(_)
    ));
    assert_eq!(gateway.disk.lock().await.used_reserve(), 10);
    drop(gateway);
    let gateway = Gateway::open(
        directory.path(),
        "pinned-operator",
        10,
        12,
        rail.clone(),
        executor.clone(),
    )
    .unwrap();
    gateway
        .recover_pending(fixture_now(&gateway, 102).await)
        .await
        .unwrap();
    assert!(matches!(
        gateway
            .handle(
                request.clone(),
                Some(quote.quote_id),
                Some("authorization"),
                fixture_now(&gateway, 103).await
            )
            .await
            .unwrap(),
        GatewayReply::Failed(_)
    ));
    assert!(gateway
        .handle(
            request.clone(),
            None,
            None,
            fixture_now(&gateway, 104).await
        )
        .await
        .is_err());
    assert!(gateway
        .handle(
            request,
            None,
            Some("another-authorization"),
            fixture_now(&gateway, 105).await
        )
        .await
        .is_err());
    assert_eq!(
        executor.0.calls.load(Ordering::SeqCst),
        1,
        "no replacement native purchase"
    );
    assert_eq!(
        rail.settles.load(Ordering::SeqCst),
        0,
        "customer is never charged for an unapproved operator"
    );
    assert_eq!(
        gateway.disk.lock().await.used_reserve(),
        10,
        "seller loss survives restart"
    );
}
#[async_trait]
impl Executor for UncertainExecute {
    fn prepare(&self, r: &Value) -> Result<PreparedExecution> {
        self.inner.prepare(r)
    }
    async fn execute(&self, p: &PreparedExecution) -> std::result::Result<Value, ExecutionFailure> {
        let mut seen = self.seen.lock().unwrap();
        seen.push(p.job_envelopes[0].payload.job_id);
        if seen.len() == 1 || self.fail_unpaid {
            return Err(ExecutionFailure {
                terminal_seller_loss: false,
                message: "fixture execution failed".into(),
                definitely_unpaid: self.fail_unpaid,
            });
        }
        Ok(json!({"job":p.job_envelopes[0].payload.job_id}))
    }
}
#[tokio::test]
async fn uncertain_execution_retries_the_exact_saved_job_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let rail = Arc::new(Rail {
        settles: AtomicUsize::new(0),
        finalized: std::sync::atomic::AtomicBool::new(true),
    });
    let executor = Arc::new(UncertainExecute {
        inner: Execute {
            identity: LocalIdentity::generate("retry@local"),
            calls: AtomicUsize::new(0),
        },
        seen: std::sync::Mutex::new(vec![]),
        fail_unpaid: false,
    });
    let gateway = Gateway::open(
        directory.path(),
        "buyer/coord",
        10,
        12,
        rail.clone(),
        executor.clone(),
    )
    .unwrap();
    let request = json!({"prompt":"hi"});
    let GatewayReply::PaymentRequired(Quote { terms, .. }) = gateway
        .handle(
            request.clone(),
            None,
            None,
            fixture_now(&gateway, 100).await,
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(matches!(
        gateway
            .handle(
                request.clone(),
                Some(terms.quote_id),
                Some("tx1"),
                fixture_now(&gateway, 101).await
            )
            .await
            .unwrap(),
        GatewayReply::Pending { .. }
    ));
    assert_eq!(rail.settles.load(Ordering::SeqCst), 0);
    drop(gateway);
    let gateway = Gateway::open(
        directory.path(),
        "buyer/coord",
        10,
        12,
        rail.clone(),
        executor.clone(),
    )
    .unwrap();
    assert!(matches!(
        gateway
            .handle(
                request.clone(),
                Some(terms.quote_id),
                Some("tx1"),
                fixture_now(&gateway, 102).await
            )
            .await
            .unwrap(),
        GatewayReply::Complete { .. }
    ));
    {
        let seen = executor.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], seen[1]);
    }
    // Payment revenue never manufactures additional prefunded compute reserve.
    assert!(gateway
        .handle(
            request.clone(),
            None,
            None,
            fixture_now(&gateway, 103).await
        )
        .await
        .is_err());
    assert!(gateway
        .handle(request, None, Some("tx2"), fixture_now(&gateway, 104).await)
        .await
        .is_err());
}

struct RecoveryExecute {
    inner: Execute,
    available: std::sync::atomic::AtomicBool,
    admission_checks: AtomicUsize,
    prepares: AtomicUsize,
    uncertain_first_execution: bool,
    executions: std::sync::Mutex<Vec<Vec<u8>>>,
}
#[async_trait]
impl Executor for RecoveryExecute {
    async fn discovery_offers(&self, _: u64, _: u64) -> Result<Vec<discovery::DiscoveryOffer>> {
        self.admission_checks.fetch_add(1, Ordering::SeqCst);
        Err(GatewayError::Unavailable("capacity unavailable".into()))
    }
    async fn check_available(&self, _: &Value, _: u64, _: u64, _: u64) -> Result<()> {
        self.admission_checks.fetch_add(1, Ordering::SeqCst);
        if self.available.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(GatewayError::Unavailable("capacity unavailable".into()))
        }
    }
    fn prepare(&self, request: &Value) -> Result<PreparedExecution> {
        self.prepares.fetch_add(1, Ordering::SeqCst);
        self.inner.prepare(request)
    }
    async fn execute(
        &self,
        prepared: &PreparedExecution,
    ) -> std::result::Result<Value, ExecutionFailure> {
        let first = {
            let mut executions = self.executions.lock().unwrap();
            executions.push(serde_json::to_vec(prepared).unwrap());
            executions.len() == 1
        };
        if first && self.uncertain_first_execution {
            return Err(ExecutionFailure {
                message: "original job still pending".into(),
                definitely_unpaid: false,
                terminal_seller_loss: false,
            });
        }
        self.inner.execute(prepared).await
    }
}

struct RecoveryRail {
    inner: Rail,
    ready: std::sync::atomic::AtomicBool,
    readiness_checks: AtomicUsize,
    verifications: AtomicUsize,
    settlements: std::sync::Mutex<Vec<Vec<u8>>>,
}
#[async_trait]
impl PaymentRail for RecoveryRail {
    fn network(&self) -> &str {
        self.inner.network()
    }
    fn asset(&self) -> &str {
        self.inner.asset()
    }
    fn pay_to(&self) -> &str {
        self.inner.pay_to()
    }
    async fn check_offers(&self, _: &[discovery::DiscoveryOffer], now: u64) -> Result<()> {
        self.check_price(12, now).await
    }
    async fn check_price(&self, _: u64, _: u64) -> Result<()> {
        self.readiness_checks.fetch_add(1, Ordering::SeqCst);
        if self.ready.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(GatewayError::Unavailable("bridge unavailable".into()))
        }
    }
    fn authorization_id(&self, header: &str) -> Result<String> {
        self.inner.authorization_id(header)
    }
    async fn verify(&self, header: &str, terms: &PaymentTerms) -> Result<VerifiedPayment> {
        self.verifications.fetch_add(1, Ordering::SeqCst);
        self.inner.verify(header, terms).await
    }
    async fn settle(
        &self,
        payment: &VerifiedPayment,
        terms: &PaymentTerms,
    ) -> Result<SettlementStatus> {
        self.settlements
            .lock()
            .unwrap()
            .push(serde_json::to_vec(&(payment, terms)).unwrap());
        self.inner.settle(payment, terms).await
    }
}

#[tokio::test]
async fn pending_work_recovers_original_jobs_and_payments_when_discovery_is_unavailable() {
    for pending_execution in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let executor = Arc::new(RecoveryExecute {
            inner: Execute {
                identity: LocalIdentity::generate("recovery-discovery@local"),
                calls: AtomicUsize::new(0),
            },
            available: std::sync::atomic::AtomicBool::new(true),
            admission_checks: AtomicUsize::new(0),
            prepares: AtomicUsize::new(0),
            uncertain_first_execution: pending_execution,
            executions: std::sync::Mutex::new(vec![]),
        });
        let rail = Arc::new(RecoveryRail {
            inner: Rail {
                settles: AtomicUsize::new(0),
                finalized: std::sync::atomic::AtomicBool::new(false),
            },
            ready: std::sync::atomic::AtomicBool::new(true),
            readiness_checks: AtomicUsize::new(0),
            verifications: AtomicUsize::new(0),
            settlements: std::sync::Mutex::new(vec![]),
        });
        let gateway = Gateway::open(
            directory.path(),
            "discovery-recovery",
            30,
            12,
            rail.clone(),
            executor.clone(),
        )
        .unwrap();
        let request = json!({"prompt":"original"});
        let GatewayReply::PaymentRequired(Quote { terms, .. }) = gateway
            .handle(
                request.clone(),
                None,
                None,
                fixture_now(&gateway, 1000).await,
            )
            .await
            .unwrap()
        else {
            panic!("expected original quote")
        };
        let payment_at = gateway.disk.lock().await.journal.watermark_ms + 1;
        assert!(matches!(
            gateway
                .handle(
                    request.clone(),
                    Some(terms.quote_id),
                    Some("original-tx"),
                    payment_at
                )
                .await
                .unwrap(),
            GatewayReply::Pending { .. }
        ));
        let (original_execution, original_payment, original_job) = {
            let disk = gateway.disk.lock().await;
            let entry = &disk.journal.entries[&terms.quote_id];
            assert_eq!(
                matches!(entry.phase, Phase::Executing { .. }),
                pending_execution
            );
            let (payment, prepared) = match &entry.phase {
                Phase::Executing { payment, prepared }
                | Phase::Settling {
                    payment, prepared, ..
                } => (payment, prepared),
                _ => panic!("expected persisted pending work"),
            };
            (
                serde_json::to_vec(prepared).unwrap(),
                serde_json::to_vec(&(payment, &entry.terms)).unwrap(),
                prepared.job_envelopes[0].payload.job_id,
            )
        };
        drop(gateway);
        executor.available.store(false, Ordering::SeqCst);
        rail.ready.store(false, Ordering::SeqCst);
        rail.inner.finalized.store(true, Ordering::SeqCst);
        let gateway = Gateway::open(
            directory.path(),
            "discovery-recovery",
            30,
            12,
            rail.clone(),
            executor.clone(),
        )
        .unwrap();

        // Prove the fresh-admission hooks now refuse before exercising recovery.
        let refusal_at = gateway.disk.lock().await.journal.watermark_ms + 1;
        assert!(gateway
            .handle(request.clone(), None, None, refusal_at)
            .await
            .is_err());
        assert!(rail
            .check_price(terms.amount_atomic, refusal_at)
            .await
            .is_err());
        assert!(rail.check_offers(&[], refusal_at).await.is_err());
        let admission_checks = executor.admission_checks.load(Ordering::SeqCst);
        let readiness_checks = rail.readiness_checks.load(Ordering::SeqCst);
        let recovery_at = gateway.disk.lock().await.journal.watermark_ms + 1;
        gateway.recover_pending(recovery_at).await.unwrap();
        let replay_at = gateway.disk.lock().await.journal.watermark_ms + 1;
        let GatewayReply::Complete {
            output,
            transaction_id,
            ..
        } = gateway
            .handle(
                request,
                Some(terms.quote_id),
                Some("original-tx"),
                replay_at,
            )
            .await
            .unwrap()
        else {
            panic!("original work must recover despite unavailable discovery")
        };
        assert_eq!(output["job"], json!(original_job));
        assert_eq!(transaction_id, "original-tx");
        assert_eq!(
            executor.admission_checks.load(Ordering::SeqCst),
            admission_checks
        );
        assert_eq!(
            rail.readiness_checks.load(Ordering::SeqCst),
            readiness_checks
        );
        assert_eq!(
            executor.prepares.load(Ordering::SeqCst),
            1,
            "never sign a replacement job"
        );
        assert_eq!(
            rail.verifications.load(Ordering::SeqCst),
            1,
            "never reauthorize a recovery"
        );
        {
            let executions = executor.executions.lock().unwrap();
            assert_eq!(executions.len(), if pending_execution { 2 } else { 1 });
            assert!(executions.iter().all(|bytes| bytes == &original_execution));
            let settlements = rail.settlements.lock().unwrap();
            assert_eq!(settlements.len(), if pending_execution { 1 } else { 2 });
            assert!(settlements.iter().all(|bytes| bytes == &original_payment));
        }
        let disk = gateway.disk.lock().await;
        assert_eq!(disk.journal.entries.len(), 1);
        assert_eq!(
            disk.used_reserve(),
            10,
            "original reserve remains accounted"
        );
    }
}

struct PausedAvailability {
    inner: Execute,
    entered: AtomicUsize,
    release: tokio::sync::Semaphore,
    prepared: std::sync::Mutex<Vec<Vec<u8>>>,
}
impl PausedAvailability {
    fn new() -> Self {
        Self {
            inner: Execute {
                identity: LocalIdentity::generate("paused-availability@local"),
                calls: AtomicUsize::new(0),
            },
            entered: AtomicUsize::new(0),
            release: tokio::sync::Semaphore::new(0),
            prepared: std::sync::Mutex::new(vec![]),
        }
    }
}
#[async_trait]
impl Executor for PausedAvailability {
    async fn check_available(&self, request: &Value, _: u64, _: u64, _: u64) -> Result<()> {
        if request["pause"] == true {
            self.entered.fetch_add(1, Ordering::SeqCst);
            self.release.acquire().await.unwrap().forget();
        }
        Ok(())
    }
    fn prepare(&self, request: &Value) -> Result<PreparedExecution> {
        let prepared = self.inner.prepare(request)?;
        self.prepared
            .lock()
            .unwrap()
            .push(serde_json::to_vec(&prepared).unwrap());
        Ok(prepared)
    }
    async fn execute(
        &self,
        prepared: &PreparedExecution,
    ) -> std::result::Result<Value, ExecutionFailure> {
        self.inner.execute(prepared).await
    }
}

#[tokio::test]
async fn waiting_fresh_availability_does_not_block_original_settlement_recovery() {
    use std::{future::Future, task::Poll};
    let directory = tempfile::tempdir().unwrap();
    let executor = Arc::new(PausedAvailability::new());
    let rail = Arc::new(Rail {
        settles: AtomicUsize::new(0),
        finalized: std::sync::atomic::AtomicBool::new(false),
    });
    let gateway = Gateway::open(
        directory.path(),
        "unblocked-recovery",
        30,
        12,
        rail.clone(),
        executor.clone(),
    )
    .unwrap();
    let request = json!({"prompt":"original"});
    let GatewayReply::PaymentRequired(Quote {
        terms: original, ..
    }) = gateway
        .handle(
            request.clone(),
            None,
            None,
            fixture_now(&gateway, 1000).await,
        )
        .await
        .unwrap()
    else {
        panic!("expected original quote")
    };
    let payment_at = gateway.disk.lock().await.journal.watermark_ms + 1;
    assert!(matches!(
        gateway
            .handle(
                request,
                Some(original.quote_id),
                Some("original-tx"),
                payment_at
            )
            .await
            .unwrap(),
        GatewayReply::Pending { .. }
    ));
    let fresh_at = gateway.disk.lock().await.journal.watermark_ms + 1;
    let mut fresh =
        Box::pin(gateway.handle(json!({"prompt":"new", "pause":true}), None, None, fresh_at));
    assert!(std::future::poll_fn(|cx| Poll::Ready(fresh.as_mut().poll(cx).is_pending())).await);
    assert_eq!(executor.entered.load(Ordering::SeqCst), 1);
    rail.finalized.store(true, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(1), gateway.recover_pending(fresh_at))
        .await
        .expect("fresh capacity I/O must not hold the settlement mutex")
        .unwrap();
    {
        let disk = gateway.disk.lock().await;
        assert!(matches!(&disk.journal.entries[&original.quote_id].phase,
            Phase::Complete { transaction_id, .. } if transaction_id == "original-tx"));
        assert_eq!(disk.journal.entries.len(), 1);
    }
    assert_eq!(executor.inner.calls.load(Ordering::SeqCst), 1);
    assert_eq!(rail.settles.load(Ordering::SeqCst), 2);
    assert_eq!(
        executor.prepared.lock().unwrap().len(),
        1,
        "waiting request has not signed a job"
    );
    executor.release.add_permits(1);
    assert!(matches!(
        fresh.await.unwrap(),
        GatewayReply::PaymentRequired(_)
    ));
    assert_eq!(
        executor.inner.calls.load(Ordering::SeqCst),
        1,
        "fresh quote never dispatches"
    );
}

#[tokio::test]
async fn concurrent_fresh_same_id_quotes_preserve_first_terms_and_signed_job() {
    use std::{future::Future, task::Poll};
    let directory = tempfile::tempdir().unwrap();
    let executor = Arc::new(PausedAvailability::new());
    let rail = Arc::new(Rail {
        settles: AtomicUsize::new(0),
        finalized: std::sync::atomic::AtomicBool::new(true),
    });
    let gateway = Gateway::open(
        directory.path(),
        "same-id-preflight",
        30,
        12,
        rail.clone(),
        executor.clone(),
    )
    .unwrap();
    let request = json!({"prompt":"same", "pause":true});
    let id = Uuid::new_v4();
    let mut first = Box::pin(gateway.handle(
        request.clone(),
        Some(id),
        None,
        fixture_now(&gateway, 1000).await,
    ));
    let mut second =
        Box::pin(gateway.handle(request, Some(id), None, fixture_now(&gateway, 1000).await));
    assert!(std::future::poll_fn(|cx| Poll::Ready(first.as_mut().poll(cx).is_pending())).await);
    assert!(std::future::poll_fn(|cx| Poll::Ready(second.as_mut().poll(cx).is_pending())).await);
    assert_eq!(
        executor.entered.load(Ordering::SeqCst),
        2,
        "both fresh lookups run before either quote is persisted"
    );
    executor.release.add_permits(1);
    let GatewayReply::PaymentRequired(Quote {
        terms: first_terms, ..
    }) = first.await.unwrap()
    else {
        panic!("expected first quote")
    };
    let original = serde_json::to_vec(&gateway.disk.lock().await.journal.entries[&id]).unwrap();
    // The second lookup finishes later and must return the first expiry and job.
    tokio::time::sleep(Duration::from_millis(5)).await;
    executor.release.add_permits(1);
    let GatewayReply::PaymentRequired(Quote {
        terms: second_terms,
        ..
    }) = second.await.unwrap()
    else {
        panic!("expected original quote from concurrent lookup")
    };
    assert_eq!(
        serde_json::to_vec(&first_terms).unwrap(),
        serde_json::to_vec(&second_terms).unwrap()
    );
    let disk = gateway.disk.lock().await;
    assert_eq!(disk.journal.entries.len(), 1);
    assert_eq!(
        serde_json::to_vec(&disk.journal.entries[&id]).unwrap(),
        original
    );
    let prepared = executor.prepared.lock().unwrap();
    assert_eq!(
        prepared.len(),
        2,
        "each lookup prepared a candidate before deduplication"
    );
    assert_ne!(prepared[0], prepared[1]);
    assert_eq!(
        serde_json::to_vec(disk.journal.entries[&id].quoted_execution.as_ref().unwrap()).unwrap(),
        prepared[0]
    );
    assert_eq!(executor.inner.calls.load(Ordering::SeqCst), 0);
    assert_eq!(rail.settles.load(Ordering::SeqCst), 0);
    assert_eq!(disk.used_reserve(), 0);
}

#[tokio::test]
async fn fresh_quote_rechecks_reserve_consumed_while_availability_waits() {
    use std::{future::Future, task::Poll};
    let directory = tempfile::tempdir().unwrap();
    let executor = Arc::new(PausedAvailability::new());
    let rail = Arc::new(Rail {
        settles: AtomicUsize::new(0),
        finalized: std::sync::atomic::AtomicBool::new(true),
    });
    let gateway = Gateway::open(
        directory.path(),
        "reserve-during-preflight",
        10,
        12,
        rail.clone(),
        executor.clone(),
    )
    .unwrap();
    let request = json!({"prompt":"original"});
    let GatewayReply::PaymentRequired(Quote {
        terms: original, ..
    }) = gateway
        .handle(
            request.clone(),
            None,
            None,
            fixture_now(&gateway, 1000).await,
        )
        .await
        .unwrap()
    else {
        panic!("expected original quote")
    };
    let fresh_at = gateway.disk.lock().await.journal.watermark_ms + 1;
    let mut fresh =
        Box::pin(gateway.handle(json!({"prompt":"new", "pause":true}), None, None, fresh_at));
    assert!(std::future::poll_fn(|cx| Poll::Ready(fresh.as_mut().poll(cx).is_pending())).await);
    assert_eq!(executor.entered.load(Ordering::SeqCst), 1);
    let payment = tokio::time::timeout(
        Duration::from_secs(1),
        gateway.handle(
            request,
            Some(original.quote_id),
            Some("original-tx"),
            fresh_at,
        ),
    )
    .await
    .expect("fresh lookup must not block an existing payment")
    .unwrap();
    assert!(matches!(payment, GatewayReply::Complete { .. }));
    tokio::time::sleep(Duration::from_millis(5)).await;
    executor.release.add_permits(1);
    let error = fresh.await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("reserve changed during quote admission"),
        "unexpected refusal: {error}"
    );
    let disk = gateway.disk.lock().await;
    assert_eq!(
        disk.journal.entries.len(),
        1,
        "stale reserve must not yield a payable quote"
    );
    assert_eq!(disk.used_reserve(), 10);
    assert_eq!(executor.inner.calls.load(Ordering::SeqCst), 1);
    assert_eq!(rail.settles.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn verified_unpaid_failure_never_settles_and_journal_write_failure_stops_spending() {
    let directory = tempfile::tempdir().unwrap();
    let rail = Arc::new(Rail {
        settles: AtomicUsize::new(0),
        finalized: std::sync::atomic::AtomicBool::new(true),
    });
    let executor = Arc::new(UncertainExecute {
        inner: Execute {
            identity: LocalIdentity::generate("failure@local"),
            calls: AtomicUsize::new(0),
        },
        seen: std::sync::Mutex::new(vec![]),
        fail_unpaid: true,
    });
    let gateway = Gateway::open(
        directory.path(),
        "buyer/coord",
        10,
        12,
        rail.clone(),
        executor.clone(),
    )
    .unwrap();
    let request = json!({"prompt":"hi"});
    let GatewayReply::PaymentRequired(Quote { terms, .. }) = gateway
        .handle(
            request.clone(),
            None,
            None,
            fixture_now(&gateway, 100).await,
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(matches!(
        gateway
            .handle(
                request.clone(),
                Some(terms.quote_id),
                Some("tx1"),
                fixture_now(&gateway, 101).await
            )
            .await
            .unwrap(),
        GatewayReply::Failed(_)
    ));
    assert_eq!(rail.settles.load(Ordering::SeqCst), 0);
    let path = directory.path().join("gateway.json");
    std::fs::rename(&path, directory.path().join("saved.json")).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(gateway
        .handle(
            request.clone(),
            None,
            None,
            fixture_now(&gateway, 102).await
        )
        .await
        .is_err());
    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(directory.path().join("saved.json"), &path).unwrap();
    assert!(gateway
        .handle(request, None, None, fixture_now(&gateway, 103).await)
        .await
        .is_err());
    assert_eq!(executor.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn idempotency_concurrency_and_background_recovery_preserve_one_purchase() {
    let directory = tempfile::tempdir().unwrap();
    let rail = Arc::new(Rail {
        settles: AtomicUsize::new(0),
        finalized: std::sync::atomic::AtomicBool::new(false),
    });
    let executor = Arc::new(Execute {
        identity: LocalIdentity::generate("concurrent@local"),
        calls: AtomicUsize::new(0),
    });
    let gateway = Arc::new(
        Gateway::open(
            directory.path(),
            "buyer/coord",
            10,
            12,
            rail.clone(),
            executor.clone(),
        )
        .unwrap(),
    );
    let id = Uuid::new_v4();
    let request = json!({"prompt":"same"});
    for _ in 0..2 {
        let GatewayReply::PaymentRequired(Quote { terms, .. }) = gateway
            .handle(
                request.clone(),
                Some(id),
                None,
                fixture_now(&gateway, 1000).await,
            )
            .await
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(terms.quote_id, id);
    }
    assert_eq!(gateway.disk.lock().await.journal.entries.len(), 1);
    assert!(gateway
        .handle(
            json!({"prompt":"changed"}),
            Some(id),
            None,
            fixture_now(&gateway, 1000).await
        )
        .await
        .is_err());
    let (a, b) = tokio::join!(
        gateway.handle(
            request.clone(),
            Some(id),
            Some("tx"),
            fixture_now(&gateway, 1001).await
        ),
        gateway.handle(
            request.clone(),
            Some(id),
            Some("tx"),
            fixture_now(&gateway, 1001).await
        )
    );
    assert!(matches!(a.unwrap(), GatewayReply::Pending { .. }));
    assert!(matches!(b.unwrap(), GatewayReply::Pending { .. }));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    drop(gateway);
    rail.finalized.store(true, Ordering::SeqCst);
    let gateway = Gateway::open(
        directory.path(),
        "buyer/coord",
        10,
        12,
        rail.clone(),
        executor.clone(),
    )
    .unwrap();
    gateway
        .recover_pending(fixture_now(&gateway, 1002).await)
        .await
        .unwrap();
    assert!(matches!(
        gateway
            .handle(
                request,
                Some(id),
                Some("tx"),
                fixture_now(&gateway, 1003).await
            )
            .await
            .unwrap(),
        GatewayReply::Complete { .. }
    ));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn funded_ceiling_increase_preserves_replay_and_exclusive_lock() {
    let directory = tempfile::tempdir().unwrap();
    let rail = Arc::new(Rail {
        settles: AtomicUsize::new(0),
        finalized: std::sync::atomic::AtomicBool::new(true),
    });
    let executor = Arc::new(Execute {
        identity: LocalIdentity::generate("funding@local"),
        calls: AtomicUsize::new(0),
    });
    let gateway = Gateway::open(
        directory.path(),
        "buyer/coord",
        10,
        12,
        rail.clone(),
        executor.clone(),
    )
    .unwrap();
    assert!(Gateway::open(
        directory.path(),
        "buyer/coord",
        10,
        12,
        rail.clone(),
        executor.clone()
    )
    .is_err());
    let id = Uuid::new_v4();
    let request = json!({"prompt":"same"});
    gateway
        .handle(
            request.clone(),
            Some(id),
            None,
            fixture_now(&gateway, 1000).await,
        )
        .await
        .unwrap();
    gateway
        .handle(
            request.clone(),
            Some(id),
            Some("tx"),
            fixture_now(&gateway, 1001).await,
        )
        .await
        .unwrap();
    drop(gateway);
    assert!(Gateway::open(
        directory.path(),
        "buyer/coord",
        20,
        12,
        rail.clone(),
        executor.clone()
    )
    .is_err());
    assert!(Gateway::open_with_reserve_increase(
        directory.path(),
        "buyer/coord",
        9,
        12,
        rail.clone(),
        executor.clone(),
        true
    )
    .is_err());
    let gateway = Gateway::open_with_reserve_increase(
        directory.path(),
        "buyer/coord",
        20,
        12,
        rail,
        executor.clone(),
        true,
    )
    .unwrap();
    assert_eq!(gateway.disk.lock().await.used_reserve(), 10);
    assert!(matches!(
        gateway
            .handle(
                request,
                Some(id),
                Some("tx"),
                fixture_now(&gateway, 1002).await
            )
            .await
            .unwrap(),
        GatewayReply::Complete { .. }
    ));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
}

struct RejectRail;
#[async_trait]
impl PaymentRail for RejectRail {
    fn network(&self) -> &str {
        "solana:test"
    }
    fn asset(&self) -> &str {
        "USDC"
    }
    fn pay_to(&self) -> &str {
        "merchant"
    }
    fn authorization_id(&self, _: &str) -> Result<String> {
        Err(GatewayError::Payment("rejected".into()))
    }
    async fn verify(&self, _: &str, _: &PaymentTerms) -> Result<VerifiedPayment> {
        Err(GatewayError::Payment(
            "invalid signature or insufficient funds".into(),
        ))
    }
    async fn settle(&self, _: &VerifiedPayment, _: &PaymentTerms) -> Result<SettlementStatus> {
        panic!("rejected payment must never settle")
    }
}
#[tokio::test]
async fn rejected_payment_and_http_body_mismatch_cause_no_dispatch() {
    let directory = tempfile::tempdir().unwrap();
    let executor = Arc::new(Execute {
        identity: LocalIdentity::generate("http@local"),
        calls: AtomicUsize::new(0),
    });
    let gateway = Arc::new(
        Gateway::open(
            directory.path(),
            "buyer/coord",
            10,
            12,
            Arc::new(RejectRail),
            executor.clone(),
        )
        .unwrap(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, router(gateway)).await.unwrap();
    });
    let http = reqwest::Client::new();
    let id = Uuid::new_v4();
    let quote = http
        .post(&endpoint)
        .header("idempotency-key", id.to_string())
        .json(&json!({"prompt":"hi"}))
        .send()
        .await
        .unwrap();
    assert_eq!(quote.status(), StatusCode::PAYMENT_REQUIRED);
    let required = payment_payload(quote.headers()["payment-required"].to_str().unwrap()).unwrap();
    assert_eq!(required["x402Version"], 2);
    assert_eq!(required["accepts"][0]["extra"]["quoteId"], id.to_string());
    let memo = required["accepts"][0]["extra"]["memo"].as_str().unwrap();
    assert_eq!(
        memo,
        short_memo(id, &request_digest(&json!({"prompt":"hi"})).unwrap())
    );
    assert_eq!(memo.len(), 32);
    assert!(required.get("extensions").is_none());
    let changed = http
        .post(&endpoint)
        .header("idempotency-key", id.to_string())
        .json(&json!({"prompt":"changed"}))
        .send()
        .await
        .unwrap();
    assert_eq!(changed.status(), StatusCode::BAD_REQUEST);
    let payment = base64::engine::general_purpose::STANDARD.encode(
        serde_json::to_vec(
            &json!({"accepted":required["accepts"][0],"payload":{"transaction":"invalid"}}),
        )
        .unwrap(),
    );
    let rejected = http
        .post(&endpoint)
        .header("payment-signature", payment)
        .json(&json!({"prompt":"hi"}))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::PAYMENT_REQUIRED);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn quote_declares_bazaar_discovery_without_the_buyers_prompt() {
    let directory = tempfile::tempdir().unwrap();
    let executor = Arc::new(Execute {
        identity: LocalIdentity::generate("bazaar@local"),
        calls: AtomicUsize::new(0),
    });
    let gateway = Arc::new(
        Gateway::open(
            directory.path(),
            "buyer/coord",
            10,
            12,
            Arc::new(RejectRail),
            executor,
        )
        .unwrap(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, router(gateway)).await.unwrap();
    });
    let request = json!({"model":"m","messages":[{"role":"user","content":"a private question"}],
        "max_tokens":512,"pricing":"model_tariff_v1"});
    let quote = reqwest::Client::new()
        .post(&endpoint)
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(quote.status(), StatusCode::PAYMENT_REQUIRED);
    let required = payment_payload(quote.headers()["payment-required"].to_str().unwrap()).unwrap();
    assert_eq!(quote.json::<Value>().await.unwrap(), required);
    assert!(!required.to_string().contains("a private question"));
    let bazaar = &required["extensions"]["bazaar"];
    assert_eq!(
        bazaar["info"]["input"],
        json!({"type":"http","method":"POST","bodyType":"json","body":{"model":"m",
            "messages":[{"role":"user","content":"Reply with a short greeting."}],
            "max_tokens":256,"pricing":"model_tariff_v1"}})
    );
    assert_eq!(
        bazaar["schema"]["properties"]["input"]["required"],
        json!(["type", "method", "bodyType", "body"])
    );
    let resource = &required["resource"];
    assert_eq!(resource["serviceName"], "Covenant Compute");
    assert_eq!(
        resource["iconUrl"],
        "https://localhost/brand/covenant-icon-02026997.png"
    );
    server.abort();
}

#[tokio::test]
async fn native_signed_response_fixture_and_request_validation() {
    use covenant_compute_buyer::{durable_budget::DurableBudget, verify_receipt, DispatchOutcome};
    use covenant_compute_protocol::{
        output_hash_hex, JobMeter, SignedWorkReceipt, WorkReceiptPayload,
    };
    let directory = tempfile::tempdir().unwrap();
    let identity = Arc::new(LocalIdentity::generate("fixture-buyer@local"));
    let operator = LocalIdentity::generate("fixture-operator@local");
    let config = BuyerConfig {
        coordinator_url: "https://compute.example".into(),
        poll_interval: std::time::Duration::from_millis(500),
        referral_code: None,
        rpc_url: None,
    };
    let executor = Arc::new(buyer::BuyerExecutor {
        budget: DurableBudget::open(
            directory.path(),
            &identity.agent_id().pubkey_base58(),
            &config.coordinator_url,
            1000,
        )
        .unwrap(),
        identity,
        config,
        http: reqwest::Client::new(),
        model: "fixture-model".into(),
        offer_micro_usdc: 10,
        max_tokens: 100,
        max_input_bytes: 4096,
        approved_operators: vec![operator.agent_id().pubkey_base58()],
        tariffs: None,
    });
    let rail = Arc::new(Rail {
        settles: AtomicUsize::new(0),
        finalized: std::sync::atomic::AtomicBool::new(true),
    });
    let gateway = Gateway::open(
        &directory.path().join("gateway"),
        "input-limits",
        1000,
        12,
        rail.clone(),
        executor.clone(),
    )
    .unwrap();
    let request = json!({"model":"fixture-model","messages":[{"role":"system","content":"Be precise."},{"role":"user","content":"Explain a rainbow."}],"max_tokens":64,"temperature":0.25});
    for bad in [
        json!({"model":"fixture-model","messages":[{"role":"user","content":"hi","tools":[]}],"max_tokens":64}),
        json!({"model":"another-model","messages":[{"role":"user","content":"hi"}],"max_tokens":64}),
        json!({"model":"fixture-model","messages":[{"role":"user","content":"hi"}],"max_tokens":101}),
        json!({"model":"fixture-model","messages":[{"role":"user","content":"hi"}],"max_tokens":64,"temperature":3}),
        json!({"model":"fixture-model","messages":vec![json!({"role":"user","content":"hi"});33],"max_tokens":64}),
        json!({"model":"fixture-model","messages":[{"role":"user","content":"x".repeat(4096)}],"max_tokens":64}),
        json!({"model":"fixture-model","messages":[{"role":"user","content":"界".repeat(1400)}],"max_tokens":64}),
    ] {
        assert!(executor.prepare(&bad).is_err());
        assert!(gateway.handle(bad, None, None, now_ms()).await.is_err());
    }
    assert!(
        gateway.disk.lock().await.journal.entries.is_empty(),
        "invalid input must not obtain a payable quote"
    );
    assert_eq!(rail.settles.load(Ordering::SeqCst), 0);
    assert!(executor.budget.pending(now_ms()).unwrap().is_empty());
    assert!(executor
        .require_approved_operator(&operator.agent_id().pubkey_base58())
        .is_ok());
    let pin_failure = executor
        .require_approved_operator(
            &LocalIdentity::generate("unapproved@local")
                .agent_id()
                .pubkey_base58(),
        )
        .unwrap_err();
    assert!(pin_failure.terminal_seller_loss);
    assert!(!pin_failure.definitely_unpaid);
    let overhead = serde_json::to_vec(&json!([{"role":"user","content":""}]))
        .unwrap()
        .len();
    let exact_limit = json!({"model":"fixture-model","messages":[{"role":"user","content":"x".repeat(4096-overhead)}],"max_tokens":64});
    assert!(
        executor.prepare(&exact_limit).is_ok(),
        "exact byte boundary is supported"
    );
    let envelope = executor.prepare(&request).unwrap().job_envelopes.remove(0);
    let output = vec![
        Content::text("Light separates into colors."),
        Content::json(json!({"confidence":0.125,"count":1})),
        Content::text("Verified fixture."),
    ];
    let receipt = SignedWorkReceipt::sign(
        WorkReceiptPayload {
            job_id: envelope.payload.job_id,
            operator: operator.agent_id(),
            job_hash_hex: Sha256::digest(envelope.payload_json.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            result_hash_hex: output_hash_hex(&output),
            meter: JobMeter {
                wall_ms: 5,
                tokens_in: Some(12),
                tokens_out: Some(9),
                gpu_seconds: None,
                finish_reason: None,
            },
            price_micro_usdc: 10,
            status: serde_json::from_str("\"ok\"").unwrap(),
            executed_at_ms: now_ms(),
            node_audit_root_hex: "cc".repeat(32),
        },
        &operator,
    )
    .unwrap();
    verify_receipt(&envelope, &receipt, &output).unwrap();
    let mut response = buyer::completed_response(
        DispatchOutcome {
            envelope,
            receipt,
            output,
            payout: None,
        },
        "fixture-model",
    )
    .unwrap();
    response["covenant"]["request_sha256"] = json!(request_digest(&request).unwrap());
    response["covenant"]["quote_id"] = json!("00000000-0000-4000-8000-000000000001");
    assert_eq!(
        response["choices"][0]["message"]["content"],
        "Light separates into colors.\nVerified fixture."
    );
    if let Ok(path) = std::env::var("COVENANT_X402_FIXTURE_OUTPUT") {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&json!({"request":request,"response":response})).unwrap(),
        )
        .unwrap();
    }
}

#[tokio::test]
async fn rollback_disables_new_spending_but_settlement_recovery_continues() {
    let directory = tempfile::tempdir().unwrap();
    let rail = Arc::new(Rail {
        settles: AtomicUsize::new(0),
        finalized: std::sync::atomic::AtomicBool::new(false),
    });
    let executor = Arc::new(Execute {
        identity: LocalIdentity::generate("rollback@local"),
        calls: AtomicUsize::new(0),
    });
    let gateway = Gateway::open(
        directory.path(),
        "buyer/coord",
        10,
        12,
        rail.clone(),
        executor.clone(),
    )
    .unwrap();
    let id = Uuid::new_v4();
    let request = json!({"prompt":"same"});
    gateway
        .handle(
            request.clone(),
            Some(id),
            None,
            fixture_now(&gateway, 1000).await,
        )
        .await
        .unwrap();
    gateway
        .handle(
            request.clone(),
            Some(id),
            Some("tx"),
            fixture_now(&gateway, 1001).await,
        )
        .await
        .unwrap();
    drop(gateway);
    rail.finalized.store(true, Ordering::SeqCst);
    let gateway = Gateway::open(
        directory.path(),
        "buyer/coord",
        10,
        12,
        rail,
        executor.clone(),
    )
    .unwrap()
    .with_admissions_enabled(false);
    assert!(gateway
        .handle(
            request.clone(),
            None,
            None,
            fixture_now(&gateway, 1002).await
        )
        .await
        .is_err());
    gateway
        .recover_pending(fixture_now(&gateway, 1003).await)
        .await
        .unwrap();
    assert!(matches!(
        gateway
            .handle(
                request,
                Some(id),
                Some("tx"),
                fixture_now(&gateway, 1004).await
            )
            .await
            .unwrap(),
        GatewayReply::Complete { .. }
    ));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn durable_latch_blocks_new_quotes_and_payments_across_restart_but_recovers_existing() {
    let directory = tempfile::tempdir().unwrap();
    let rail = Arc::new(Rail {
        settles: AtomicUsize::new(0),
        finalized: std::sync::atomic::AtomicBool::new(false),
    });
    let executor = Arc::new(Execute {
        identity: LocalIdentity::generate("latch@local"),
        calls: AtomicUsize::new(0),
    });
    let open = || {
        Gateway::open(
            directory.path(),
            "latch",
            30,
            12,
            rail.clone(),
            executor.clone(),
        )
        .unwrap()
    };
    let gateway = open();
    let request = json!({"prompt":"same"});
    let paid = Uuid::new_v4();
    let unpaid = Uuid::new_v4();
    for id in [paid, unpaid] {
        gateway
            .handle(
                request.clone(),
                Some(id),
                None,
                fixture_now(&gateway, 1000).await,
            )
            .await
            .unwrap();
    }
    gateway
        .handle(
            request.clone(),
            Some(paid),
            Some("paid"),
            fixture_now(&gateway, 1001).await,
        )
        .await
        .unwrap();
    std::fs::write(
        directory.path().join("admissions.disabled"),
        "provider failed",
    )
    .unwrap();
    assert!(!gateway.admissions_open());
    assert!(gateway
        .handle(
            request.clone(),
            None,
            None,
            fixture_now(&gateway, 1002).await
        )
        .await
        .is_err());
    assert!(gateway
        .handle(
            request.clone(),
            Some(unpaid),
            Some("unpaid"),
            fixture_now(&gateway, 1003).await
        )
        .await
        .is_err());
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    drop(gateway);
    let gateway = open();
    assert!(!gateway.admissions_open());
    rail.finalized.store(true, Ordering::SeqCst);
    gateway
        .recover_pending(fixture_now(&gateway, 1004).await)
        .await
        .unwrap();
    assert!(matches!(
        gateway
            .handle(
                request.clone(),
                Some(paid),
                Some("paid"),
                fixture_now(&gateway, 1005).await
            )
            .await
            .unwrap(),
        GatewayReply::Complete { .. }
    ));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    // Only explicit removal permits admissions again; startup markers also deny.
    std::fs::remove_file(directory.path().join("admissions.disabled")).unwrap();
    std::fs::create_dir(directory.path().join("admissions.starting")).unwrap();
    assert!(!gateway.admissions_open());
    assert!(gateway
        .handle(
            request.clone(),
            None,
            None,
            fixture_now(&gateway, 1006).await
        )
        .await
        .is_err());
    std::fs::remove_dir(directory.path().join("admissions.starting")).unwrap();
    assert!(gateway.admissions_open());
}

struct LatchingRail {
    latch: PathBuf,
}
#[async_trait]
impl PaymentRail for LatchingRail {
    fn network(&self) -> &str {
        "test:1"
    }
    fn asset(&self) -> &str {
        "USDC"
    }
    fn pay_to(&self) -> &str {
        "merchant"
    }
    fn authorization_id(&self, header: &str) -> Result<String> {
        Ok(header.into())
    }
    async fn verify(&self, _: &str, _: &PaymentTerms) -> Result<VerifiedPayment> {
        // Optional worker dies while external payment verification is in flight.
        std::fs::write(&self.latch, "provider failed")?;
        Ok(VerifiedPayment {
            payer: "payer".into(),
            authorization_id: "tx".into(),
            transaction_id: "tx".into(),
            encoded_transaction: "tx".into(),
        })
    }
    async fn settle(&self, _: &VerifiedPayment, _: &PaymentTerms) -> Result<SettlementStatus> {
        panic!("disabled admission must never reach settlement")
    }
}
#[tokio::test]
async fn latch_raised_during_payment_verification_prevents_paid_dispatch() {
    let directory = tempfile::tempdir().unwrap();
    let executor = Arc::new(Execute {
        identity: LocalIdentity::generate("race@local"),
        calls: AtomicUsize::new(0),
    });
    let gateway = Gateway::open(
        directory.path(),
        "latch-race",
        10,
        12,
        Arc::new(LatchingRail {
            latch: directory.path().join("admissions.disabled"),
        }),
        executor.clone(),
    )
    .unwrap();
    let request = json!({"prompt":"same"});
    let id = Uuid::new_v4();
    gateway
        .handle(
            request.clone(),
            Some(id),
            None,
            fixture_now(&gateway, 1000).await,
        )
        .await
        .unwrap();
    assert!(gateway
        .handle(
            request,
            Some(id),
            Some("tx"),
            fixture_now(&gateway, 1001).await
        )
        .await
        .is_err());
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
    assert_eq!(gateway.disk.lock().await.used_reserve(), 0);
}

struct VariableQuoteExecutor {
    base: Execute,
    retail: std::sync::atomic::AtomicU64,
    prepares: AtomicUsize,
}
#[async_trait]
impl Executor for VariableQuoteExecutor {
    fn prepare(&self, request: &Value) -> Result<PreparedExecution> {
        self.prepares.fetch_add(1, Ordering::SeqCst);
        self.base.prepare(request)
    }
    fn quote(&self, request: &Value, _: u64, now: u64) -> Result<ExecutionQuote> {
        Ok(ExecutionQuote {
            batch_pricing: None,
            prepared: self.prepare(request)?,
            amount_atomic: self.retail.load(Ordering::SeqCst),
            expires_at_ms: now + 90_000,
        })
    }
    async fn execute(
        &self,
        prepared: &PreparedExecution,
    ) -> std::result::Result<Value, ExecutionFailure> {
        self.base.execute(prepared).await
    }
}

#[tokio::test]
async fn variable_quote_freezes_native_job_and_sale_price_across_restart_and_repricing() {
    let dir = tempfile::tempdir().unwrap();
    let rail = Arc::new(Rail {
        settles: AtomicUsize::new(0),
        finalized: std::sync::atomic::AtomicBool::new(true),
    });
    let executor = Arc::new(VariableQuoteExecutor {
        base: Execute {
            identity: LocalIdentity::generate("quote@local"),
            calls: AtomicUsize::new(0),
        },
        retail: std::sync::atomic::AtomicU64::new(12),
        prepares: AtomicUsize::new(0),
    });
    let gateway = Gateway::open(
        dir.path(),
        "unchanged-legacy-namespace",
        1000,
        50,
        rail.clone(),
        executor.clone(),
    )
    .unwrap();
    let request = json!({"prompt":"frozen"});
    let GatewayReply::PaymentRequired(Quote { terms: first, .. }) = gateway
        .handle(
            request.clone(),
            None,
            None,
            fixture_now(&gateway, 1000).await,
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    let job = gateway.disk.lock().await.journal.entries[&first.quote_id]
        .quoted_execution
        .as_ref()
        .unwrap()
        .job_envelopes[0]
        .payload
        .job_id;
    assert_eq!(first.amount_atomic, 12);
    assert_eq!(executor.prepares.load(Ordering::SeqCst), 1);
    drop(gateway);
    executor.retail.store(25, Ordering::SeqCst);
    let gateway = Gateway::open(
        dir.path(),
        "unchanged-legacy-namespace",
        1000,
        50,
        rail,
        executor.clone(),
    )
    .unwrap();
    let GatewayReply::PaymentRequired(Quote {
        terms: replayed, ..
    }) = gateway
        .handle(
            request.clone(),
            Some(first.quote_id),
            None,
            fixture_now(&gateway, 1001).await,
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(replayed, first);
    let GatewayReply::Complete { output, .. } = gateway
        .handle(
            request.clone(),
            Some(first.quote_id),
            Some("auth-one"),
            fixture_now(&gateway, 1002).await,
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(output["job"], job.to_string());
    assert_eq!(executor.prepares.load(Ordering::SeqCst), 1);
    let GatewayReply::PaymentRequired(Quote { terms: second, .. }) = gateway
        .handle(
            request.clone(),
            None,
            None,
            fixture_now(&gateway, 1003).await,
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(second.amount_atomic, 25);
    assert!(matches!(
        gateway
            .handle(
                request,
                Some(first.quote_id),
                Some("auth-one"),
                fixture_now(&gateway, 1004).await
            )
            .await
            .unwrap(),
        GatewayReply::Complete { .. }
    ));
    assert_eq!(executor.base.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn old_journal_quoted_entry_remains_payable_without_repricing_or_reset() {
    let dir = tempfile::tempdir().unwrap();
    let rail = Arc::new(Rail {
        settles: AtomicUsize::new(0),
        finalized: std::sync::atomic::AtomicBool::new(true),
    });
    let executor = Arc::new(VariableQuoteExecutor {
        base: Execute {
            identity: LocalIdentity::generate("legacy@local"),
            calls: AtomicUsize::new(0),
        },
        retail: std::sync::atomic::AtomicU64::new(12),
        prepares: AtomicUsize::new(0),
    });
    let gateway = Gateway::open(
        dir.path(),
        "legacy",
        1000,
        50,
        rail.clone(),
        executor.clone(),
    )
    .unwrap();
    let request = json!({"prompt":"legacy"});
    let GatewayReply::PaymentRequired(Quote { terms: first, .. }) = gateway
        .handle(
            request.clone(),
            None,
            None,
            fixture_now(&gateway, 1000).await,
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    drop(gateway);
    let file = dir.path().join("gateway.json");
    let mut old: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    old["version"] = json!(2);
    old["entries"][first.quote_id.to_string()]
        .as_object_mut()
        .unwrap()
        .remove("quoted_execution");
    old["entries"][first.quote_id.to_string()]["terms"]
        .as_object_mut()
        .unwrap()
        .remove("memo_binding");
    std::fs::write(&file, serde_json::to_vec(&old).unwrap()).unwrap();
    executor.retail.store(80, Ordering::SeqCst);
    let gateway = Gateway::open(dir.path(), "legacy", 1000, 50, rail, executor.clone()).unwrap();
    assert!(matches!(
        gateway
            .handle(
                request,
                Some(first.quote_id),
                Some("old-authorization"),
                fixture_now(&gateway, 1001).await
            )
            .await
            .unwrap(),
        GatewayReply::Complete { .. }
    ));
    let disk = gateway.disk.lock().await;
    assert_eq!(
        disk.journal.entries[&first.quote_id].terms.amount_atomic,
        12
    );
    assert_eq!(disk.used_reserve(), 10);
    assert_eq!(disk.journal.reserve_micro_usdc, 1000);
    assert_eq!(disk.journal.version, 6);
    // A quote saved before v3 memos keeps the memo its payment was signed over.
    let terms = &disk.journal.entries[&first.quote_id].terms;
    assert_eq!(terms.memo_binding, MemoBinding::V2);
    assert_eq!(
        terms.memo(),
        format!(
            "covenant.x402.v2:{}:{}",
            first.quote_id, first.request_sha256
        )
    );
}

#[tokio::test]
async fn refusing_retail_below_native_precedes_payment_offer() {
    let dir = tempfile::tempdir().unwrap();
    let rail = Arc::new(Rail {
        settles: AtomicUsize::new(0),
        finalized: std::sync::atomic::AtomicBool::new(true),
    });
    let executor = Arc::new(Execute {
        identity: LocalIdentity::generate("cost@local"),
        calls: AtomicUsize::new(0),
    });
    let gateway = Gateway::open(dir.path(), "cost", 1000, 9, rail, executor.clone()).unwrap();
    assert!(gateway
        .handle(
            json!({"prompt":"hi"}),
            None,
            None,
            fixture_now(&gateway, 1000).await
        )
        .await
        .is_err());
    assert_eq!(gateway.disk.lock().await.journal.entries.len(), 0);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn real_buyer_tariff_quotes_are_opt_in_and_preserve_legacy_payloads() {
    use covenant_compute_buyer::durable_budget::DurableBudget;
    use covenant_compute_protocol::{parse_tariff_input, InferenceTariff};
    let dir = tempfile::tempdir().unwrap();
    let identity = Arc::new(LocalIdentity::generate("pricing-buyer@local"));
    let config = BuyerConfig {
        coordinator_url: "https://compute.example".into(),
        poll_interval: std::time::Duration::from_millis(1),
        referral_code: None,
        rpc_url: None,
    };
    let now = now_ms();
    let native = InferenceTariff {
        schema_version: 1,
        id: "haiku-native".into(),
        revision: 1,
        model: "claude-haiku-4.5".into(),
        base_micro_usdc: 10000,
        output_cap_micro_usdc_per_million: 7_500_000,
        max_output_tokens: 2048,
        valid_from_ms: now - 1000,
        valid_until_ms: now + 3_600_000,
    };
    let mut retail = native.clone();
    retail.id = "haiku-retail".into();
    retail.base_micro_usdc = 12000;
    retail.output_cap_micro_usdc_per_million = 9_000_000;
    let executor = buyer::BuyerExecutor {
        budget: DurableBudget::open(
            dir.path(),
            &identity.agent_id().pubkey_base58(),
            &config.coordinator_url,
            1_000_000,
        )
        .unwrap(),
        config,
        identity,
        http: reqwest::Client::new(),
        model: "claude-haiku-4.5".into(),
        offer_micro_usdc: 30000,
        max_tokens: 2048,
        max_input_bytes: 4096,
        approved_operators: vec![],
        tariffs: Some(Arc::new(
            pricing::TariffBook::new(vec![pricing::ModelTariff { native, retail }]).unwrap(),
        )),
    };
    let mut request = json!({"model":"claude-haiku-4.5","messages":[{"role":"user","content":"Hello"}],"max_tokens":256});
    let legacy = executor.quote(&request, 50000, now).unwrap();
    assert_eq!(legacy.amount_atomic, 50000);
    assert_eq!(legacy.prepared.maximum_cost_micro_usdc, 30000);
    assert_eq!(legacy.prepared.job_envelopes[0].payload.input.len(), 2);
    assert!(legacy.prepared.job_envelopes[0]
        .payload
        .capability_requirement
        .inference_tariff
        .is_none());
    request["pricing"] = json!("model_tariff_v1");
    let quoted = executor.quote(&request, 50000, now).unwrap();
    assert_eq!(quoted.amount_atomic, 14304);
    assert_eq!(quoted.prepared.maximum_cost_micro_usdc, 11920);
    let envelope = &quoted.prepared.job_envelopes[0];
    assert_eq!(envelope.payload.input.len(), 3);
    let native_quote = parse_tariff_input(&envelope.payload.input)
        .unwrap()
        .unwrap();
    assert_eq!(
        native_quote.price_micro_usdc,
        envelope.payload.price_micro_usdc
    );
    assert_eq!(
        envelope.payload.capability_requirement.inference_tariff,
        Some(native_quote)
    );
    executor
        .validate_admission(&quoted.prepared, envelope.payload.issued_at_ms + 74_000)
        .unwrap();
    assert!(executor
        .validate_admission(&quoted.prepared, envelope.payload.issued_at_ms + 75_000)
        .is_err());
    assert_eq!(quoted.expires_at_ms, envelope.payload.issued_at_ms + 75_000);
    // A payer arriving just before expiry retains the full 45-second delivery
    // margin before the node rejects the frozen envelope as stale.
    assert!(
        quoted.expires_at_ms - 1 + 45_000
            < envelope.payload.issued_at_ms + covenant_compute_protocol::JOB_FRESHNESS_WINDOW_MS
    );
    request["max_tokens"] = json!(2048);
    assert_eq!(
        executor.quote(&request, 50000, now).unwrap().amount_atomic,
        30432
    );
    request["pricing"] = json!("unknown");
    assert!(executor.quote(&request, 50000, now).is_err());
    request["pricing"] = Value::Null;
    assert!(executor.quote(&request, 50000, now).is_err());
    assert!(executor.budget.pending(now).unwrap().is_empty());
}

#[tokio::test]
async fn queued_retention_uses_time_after_the_journal_lock() {
    use std::{future::Future, task::Poll, time::Duration};
    for recover in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let gateway = Gateway::open(
            directory.path(),
            "buyer/coord",
            100,
            12,
            Arc::new(Rail {
                settles: AtomicUsize::new(0),
                finalized: false.into(),
            }),
            Arc::new(Execute {
                identity: LocalIdentity::generate("clock@local"),
                calls: AtomicUsize::new(0),
            }),
        )
        .unwrap();
        let mut held = gateway.disk.lock().await;
        let mut retention = std::pin::pin!(async {
            if recover {
                gateway.recover_pending(1000).await
            } else {
                gateway.purge_expired(1000).await
            }
        });
        // Poll until queued behind an operation holding the financial journal.
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(retention.as_mut().poll(cx).is_pending())).await
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
        held.journal.watermark_ms = 1010;
        drop(held);
        retention.await.unwrap();
        assert!(gateway.disk.lock().await.journal.watermark_ms >= 1030);
    }
}

#[test]
fn short_memo_matches_the_published_vector() {
    // The same vector is checked by the facilitator bridge and both SDKs.
    let quote = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
    assert_eq!(
        short_memo(quote, &"a".repeat(64)),
        "8bfb7fde3cb99c6b0f16b7a18d83a0e1"
    );
}

#[tokio::test]
async fn paid_request_waits_for_finality_and_returns_the_result() {
    let directory = tempfile::tempdir().unwrap();
    let rail = Arc::new(Rail {
        settles: AtomicUsize::new(0),
        finalized: std::sync::atomic::AtomicBool::new(false),
    });
    let executor = Arc::new(Execute {
        identity: LocalIdentity::generate("settlement-wait@local"),
        calls: AtomicUsize::new(0),
    });
    let gateway = Gateway::open(
        directory.path(),
        "settlement-wait",
        30,
        12,
        rail.clone(),
        executor,
    )
    .unwrap()
    .with_settlement_wait(Duration::from_secs(10));
    let request = json!({"prompt":"wait"});
    let GatewayReply::PaymentRequired(Quote { terms, .. }) = gateway
        .handle(
            request.clone(),
            None,
            None,
            fixture_now(&gateway, 1000).await,
        )
        .await
        .unwrap()
    else {
        panic!("expected a quote")
    };
    let finalizer = {
        let rail = rail.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            rail.finalized.store(true, Ordering::SeqCst);
        })
    };
    let paid_at = fixture_now(&gateway, 1001).await;
    let reply = gateway
        .handle(request, Some(terms.quote_id), Some("waited-tx"), paid_at)
        .await
        .unwrap();
    finalizer.await.unwrap();
    assert!(
        matches!(reply, GatewayReply::Complete { transaction_id, .. } if transaction_id == "waited-tx")
    );
    assert!(rail.settles.load(Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn background_recovery_never_waits_on_a_pending_settlement() {
    let directory = tempfile::tempdir().unwrap();
    let rail = Arc::new(Rail {
        settles: AtomicUsize::new(0),
        finalized: std::sync::atomic::AtomicBool::new(false),
    });
    let executor = Arc::new(Execute {
        identity: LocalIdentity::generate("recovery-no-wait@local"),
        calls: AtomicUsize::new(0),
    });
    let gateway = Gateway::open(
        directory.path(),
        "recovery-no-wait",
        30,
        12,
        rail.clone(),
        executor,
    )
    .unwrap();
    let request = json!({"prompt":"pending"});
    let GatewayReply::PaymentRequired(Quote { terms, .. }) = gateway
        .handle(
            request.clone(),
            None,
            None,
            fixture_now(&gateway, 1000).await,
        )
        .await
        .unwrap()
    else {
        panic!("expected a quote")
    };
    let paid_at = fixture_now(&gateway, 1001).await;
    assert!(matches!(
        gateway
            .handle(request, Some(terms.quote_id), Some("pending-tx"), paid_at)
            .await
            .unwrap(),
        GatewayReply::Pending { .. }
    ));
    let gateway = gateway.with_settlement_wait(Duration::from_secs(60));
    let recover_at = fixture_now(&gateway, paid_at + 1).await;
    tokio::time::timeout(Duration::from_secs(2), gateway.recover_pending(recover_at))
        .await
        .expect("recovery must not hold the journal for the settlement wait")
        .unwrap();
}

#[tokio::test]
async fn unfinished_wait_answers_pending_without_holding_the_journal_or_rewaiting_retries() {
    let directory = tempfile::tempdir().unwrap();
    let rail = Arc::new(Rail {
        settles: AtomicUsize::new(0),
        finalized: std::sync::atomic::AtomicBool::new(false),
    });
    let executor = Arc::new(Execute {
        identity: LocalIdentity::generate("unfinished-wait@local"),
        calls: AtomicUsize::new(0),
    });
    let gateway = Arc::new(
        Gateway::open(
            directory.path(),
            "unfinished-wait",
            30,
            12,
            rail.clone(),
            executor,
        )
        .unwrap()
        .with_settlement_wait(Duration::from_secs(3)),
    );
    let request = json!({"prompt":"slow finality"});
    let GatewayReply::PaymentRequired(Quote { terms, .. }) = gateway
        .handle(
            request.clone(),
            None,
            None,
            fixture_now(&gateway, 1000).await,
        )
        .await
        .unwrap()
    else {
        panic!("expected a quote")
    };
    let quote_id = terms.quote_id;
    let paid_at = fixture_now(&gateway, 1001).await;
    let waiting = {
        let gateway = gateway.clone();
        let request = request.clone();
        tokio::spawn(async move {
            gateway
                .handle(request, Some(quote_id), Some("slow-tx"), paid_at)
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    // The journal is free while the paid request waits for finality.
    let quote_at = fixture_now(&gateway, paid_at + 1).await;
    let started = std::time::Instant::now();
    assert!(matches!(
        tokio::time::timeout(
            Duration::from_secs(1),
            gateway.handle(json!({"prompt":"meanwhile"}), None, None, quote_at)
        )
        .await
        .expect("a fresh quote must not wait behind a settlement")
        .unwrap(),
        GatewayReply::PaymentRequired(_)
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(matches!(
        waiting.await.unwrap().unwrap(),
        GatewayReply::Pending { .. }
    ));
    let polls = rail.settles.load(Ordering::SeqCst);
    assert!(polls >= 2);
    // A retry of the stuck payment answers at once instead of waiting again.
    let retry_at = fixture_now(&gateway, quote_at + 1).await;
    let started = std::time::Instant::now();
    assert!(matches!(
        gateway
            .handle(request, Some(quote_id), Some("slow-tx"), retry_at)
            .await
            .unwrap(),
        GatewayReply::Pending { .. }
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(rail.settles.load(Ordering::SeqCst), polls + 1);
}

#[tokio::test]
async fn v4_wallet_journal_migrates_to_v6_without_changing_frozen_memos_or_jobs() {
    for binding in [MemoBinding::V2, MemoBinding::V3] {
        for already_executed in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let rail = Arc::new(Rail {
                settles: AtomicUsize::new(0),
                finalized: std::sync::atomic::AtomicBool::new(false),
            });
            let executor = Arc::new(Execute {
                identity: LocalIdentity::generate("v4-migration@local"),
                calls: AtomicUsize::new(0),
            });
            let gateway = Gateway::open(
                directory.path(),
                "wallet-v4",
                100,
                12,
                rail.clone(),
                executor.clone(),
            )
            .unwrap();
            let request = json!({"prompt":"preserve this authorization"});
            let GatewayReply::PaymentRequired(terms) = gateway
                .handle(
                    request.clone(),
                    None,
                    None,
                    fixture_now(&gateway, 1000).await,
                )
                .await
                .unwrap()
            else {
                panic!("expected quote")
            };
            let mut terms = terms.terms;
            terms.memo_binding = binding;
            {
                let mut disk = gateway.disk.lock().await;
                disk.journal.entries.get_mut(&terms.quote_id).unwrap().terms = terms.clone();
                disk.save().unwrap();
            }
            if already_executed {
                assert!(matches!(
                    gateway
                        .handle(
                            request.clone(),
                            Some(terms.quote_id),
                            Some("original-signed-payment"),
                            fixture_now(&gateway, 1001).await,
                        )
                        .await
                        .unwrap(),
                    GatewayReply::Pending { .. }
                ));
            }
            drop(gateway);
            let file = directory.path().join("gateway.json");
            let mut prior: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
            prior["version"] = json!(4);
            let frozen_entry = prior["entries"][terms.quote_id.to_string()].clone();
            let frozen_memo = terms.memo();
            let frozen_job = if already_executed {
                frozen_entry["phase"]["prepared"]["job_envelopes"][0]["payload"]["job_id"].clone()
            } else {
                frozen_entry["quoted_execution"]["job_envelopes"][0]["payload"]["job_id"].clone()
            };
            assert!(frozen_job.is_string());
            assert!(frozen_entry["terms"].get("batch_pricing").is_none());
            std::fs::write(&file, serde_json::to_vec(&prior).unwrap()).unwrap();
            let gateway = Gateway::open(
                directory.path(),
                "wallet-v4",
                100,
                12,
                rail.clone(),
                executor.clone(),
            )
            .unwrap()
            .with_admissions_enabled(!already_executed);
            let migrated: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
            assert_eq!(migrated["version"], 6);
            assert_eq!(
                migrated["entries"][terms.quote_id.to_string()],
                frozen_entry
            );
            assert_eq!(
                gateway.disk.lock().await.journal.entries[&terms.quote_id]
                    .terms
                    .memo(),
                frozen_memo
            );
            rail.finalized.store(true, Ordering::SeqCst);
            if already_executed {
                gateway
                    .recover_pending(fixture_now(&gateway, 1002).await)
                    .await
                    .unwrap();
            }
            let GatewayReply::Complete { output, .. } = gateway
                .handle(
                    request.clone(),
                    Some(terms.quote_id),
                    Some("original-signed-payment"),
                    fixture_now(&gateway, 1003).await,
                )
                .await
                .unwrap()
            else {
                panic!("original payment should complete")
            };
            assert_eq!(output["job"], frozen_job);
            assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
            let settles = rail.settles.load(Ordering::SeqCst);
            assert!(matches!(
                gateway
                    .handle(
                        request,
                        Some(terms.quote_id),
                        Some("original-signed-payment"),
                        fixture_now(&gateway, 1004).await,
                    )
                    .await
                    .unwrap(),
                GatewayReply::Complete { .. }
            ));
            assert_eq!(rail.settles.load(Ordering::SeqCst), settles);
            assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
            let disk = gateway.disk.lock().await;
            assert_eq!(disk.journal.entries[&terms.quote_id].terms, terms);
            assert_eq!(disk.used_reserve(), 10);
        }
    }
}

pub(super) const BASE_MERCHANT: &str = "0x5fA1d0C0bfFE257a20027C523093F941834f5D66";
pub(super) struct BaseRail {
    ready: std::sync::atomic::AtomicBool,
    merchant: &'static str,
    pub(super) finalized: std::sync::atomic::AtomicBool,
    pub(super) verifications: AtomicUsize,
    pub(super) settles: AtomicUsize,
}
pub(super) fn base_rail(ready: bool, merchant: &'static str) -> Arc<BaseRail> {
    Arc::new(BaseRail {
        ready: std::sync::atomic::AtomicBool::new(ready),
        merchant,
        finalized: std::sync::atomic::AtomicBool::new(true),
        verifications: AtomicUsize::new(0),
        settles: AtomicUsize::new(0),
    })
}
#[async_trait]
impl PaymentRail for BaseRail {
    fn network(&self) -> &str {
        "eip155:84532"
    }
    fn asset(&self) -> &str {
        "0x036CbD53842c5426634e7929541eC2318f3dCF7e"
    }
    fn pay_to(&self) -> &str {
        self.merchant
    }
    fn token_domain(&self) -> Option<TokenDomain> {
        Some(TokenDomain {
            name: "USDC".into(),
            version: "2".into(),
        })
    }
    async fn check_price(&self, _: u64, _: u64) -> Result<()> {
        if self.ready.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(GatewayError::Unavailable(
                "Base verifier unavailable".into(),
            ))
        }
    }
    fn authorization_id(&self, header: &str) -> Result<String> {
        request_digest(&json!(header))
    }
    async fn verify(&self, header: &str, terms: &PaymentTerms) -> Result<VerifiedPayment> {
        assert_eq!(terms.network, self.network());
        self.verifications.fetch_add(1, Ordering::SeqCst);
        Ok(VerifiedPayment {
            payer: "0xAbCdEf0000000000000000000000000000000001".into(),
            authorization_id: self.authorization_id(header)?,
            transaction_id: "reference".into(),
            encoded_transaction: header.into(),
        })
    }
    async fn settle(&self, _: &VerifiedPayment, terms: &PaymentTerms) -> Result<SettlementStatus> {
        assert_eq!(terms.network, self.network());
        self.settles.fetch_add(1, Ordering::SeqCst);
        if !self.finalized.load(Ordering::SeqCst) {
            return Ok(SettlementStatus::Pending);
        }
        Ok(SettlementStatus::Finalized {
            transaction_id: format!("0x{}", "ab".repeat(32)),
        })
    }
}
fn payment_header(accepted: Value) -> String {
    base64::engine::general_purpose::STANDARD
        .encode(json!({"x402Version":2,"accepted":accepted}).to_string())
}
fn two_rail_gateway(directory: &Path, base_ready: bool) -> (Gateway, Arc<Rail>, Arc<BaseRail>) {
    let rail = Arc::new(Rail {
        settles: AtomicUsize::new(0),
        finalized: std::sync::atomic::AtomicBool::new(true),
    });
    let base = base_rail(base_ready, BASE_MERCHANT);
    let executor = Arc::new(Execute {
        identity: LocalIdentity::generate("two-rails@local"),
        calls: AtomicUsize::new(0),
    });
    let gateway = Gateway::open(directory, "two-rails", 30, 12, rail.clone(), executor)
        .unwrap()
        .with_alternate_rail(base.clone())
        .unwrap();
    (gateway, rail, base)
}

#[tokio::test]
async fn quote_offers_base_beside_solana_and_settles_on_the_paid_rail() {
    let directory = tempfile::tempdir().unwrap();
    let (gateway, rail, base) = two_rail_gateway(directory.path(), true);
    let request = json!({"prompt":"base"});
    let GatewayReply::PaymentRequired(quote) = gateway
        .handle(
            request.clone(),
            None,
            None,
            fixture_now(&gateway, 1000).await,
        )
        .await
        .unwrap()
    else {
        panic!("expected a quote")
    };
    let required = quote.requirements();
    let accepts = required["accepts"].as_array().unwrap();
    assert_eq!(accepts.len(), 2);
    assert_eq!(accepts[0]["network"], "test:1");
    assert_eq!(
        accepts[1],
        json!({"scheme":"exact","network":"eip155:84532","asset":"0x036CbD53842c5426634e7929541eC2318f3dCF7e",
            "amount":"12","payTo":"0x5fA1d0C0bfFE257a20027C523093F941834f5D66","maxTimeoutSeconds":300,
            "extra":{"name":"USDC","version":"2","quoteId":quote.terms.quote_id,
            "requestSha256":quote.terms.request_sha256,"expiresAtMs":quote.terms.expires_at_ms}})
    );
    let header = payment_header(accepts[1].clone());
    let paid_at = fixture_now(&gateway, 1001).await;
    let reply = gateway
        .handle(
            request.clone(),
            Some(quote.terms.quote_id),
            Some(&header),
            paid_at,
        )
        .await
        .unwrap();
    let hash = format!("0x{}", "ab".repeat(32));
    assert!(
        matches!(&reply, GatewayReply::Complete { transaction_id, network, .. }
        if transaction_id == &hash && network == "eip155:84532")
    );
    assert_eq!(base.verifications.load(Ordering::SeqCst), 1);
    assert_eq!(base.settles.load(Ordering::SeqCst), 1);
    assert_eq!(rail.settles.load(Ordering::SeqCst), 0);
    {
        let disk = gateway.disk.lock().await;
        let entry = &disk.journal.entries[&quote.terms.quote_id];
        assert_eq!(entry.terms.network, "eip155:84532");
        assert!(entry.alternates.is_empty());
    }
    // A retry with the same authorization returns the saved result.
    let retry_at = fixture_now(&gateway, paid_at + 1).await;
    assert!(matches!(
        gateway
            .handle(request, Some(quote.terms.quote_id), Some(&header), retry_at)
            .await
            .unwrap(),
        GatewayReply::Complete { .. }
    ));
    assert_eq!(base.settles.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn primary_rail_still_pays_a_quote_that_also_offered_base() {
    let directory = tempfile::tempdir().unwrap();
    let (gateway, rail, base) = two_rail_gateway(directory.path(), true);
    let request = json!({"prompt":"solana"});
    let GatewayReply::PaymentRequired(quote) = gateway
        .handle(
            request.clone(),
            None,
            None,
            fixture_now(&gateway, 1000).await,
        )
        .await
        .unwrap()
    else {
        panic!("expected a quote")
    };
    let header = payment_header(quote.terms.accepted());
    let paid_at = fixture_now(&gateway, 1001).await;
    assert!(matches!(
        gateway
            .handle(request, Some(quote.terms.quote_id), Some(&header), paid_at)
            .await
            .unwrap(),
        GatewayReply::Complete { network, .. } if network == "test:1"
    ));
    assert_eq!(rail.settles.load(Ordering::SeqCst), 1);
    assert_eq!(base.verifications.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unready_alternate_is_left_out_and_unoffered_network_refused() {
    let directory = tempfile::tempdir().unwrap();
    let (gateway, _, base) = two_rail_gateway(directory.path(), false);
    let GatewayReply::PaymentRequired(quote) = gateway
        .handle(
            json!({"prompt":"solana only"}),
            None,
            None,
            fixture_now(&gateway, 1000).await,
        )
        .await
        .unwrap()
    else {
        panic!("expected a quote")
    };
    assert!(quote.alternates.is_empty());
    assert_eq!(quote.requirements()["accepts"].as_array().unwrap().len(), 1);

    base.ready.store(true, Ordering::SeqCst);
    let request = json!({"prompt":"both"});
    let GatewayReply::PaymentRequired(quote) = gateway
        .handle(
            request.clone(),
            None,
            None,
            fixture_now(&gateway, 1001).await,
        )
        .await
        .unwrap()
    else {
        panic!("expected a quote")
    };
    let mut elsewhere = quote.alternates[0].accepted();
    elsewhere["network"] = json!("eip155:8453");
    let paid_at = fixture_now(&gateway, 1002).await;
    assert!(matches!(
        gateway
            .handle(
                request,
                Some(quote.terms.quote_id),
                Some(&payment_header(elsewhere)),
                paid_at
            )
            .await,
        Err(GatewayError::Payment(_))
    ));
    assert_eq!(base.verifications.load(Ordering::SeqCst), 0);
}

/// The two-rail gateway's journal reopened, optionally with a Base rail.
fn reopen(directory: &Path, base: Option<Arc<BaseRail>>) -> Gateway {
    let rail = Arc::new(Rail {
        settles: AtomicUsize::new(0),
        finalized: std::sync::atomic::AtomicBool::new(true),
    });
    let executor = Arc::new(Execute {
        identity: LocalIdentity::generate("two-rails@local"),
        calls: AtomicUsize::new(0),
    });
    let gateway = Gateway::open(directory, "two-rails", 30, 12, rail, executor).unwrap();
    match base {
        Some(base) => gateway.with_alternate_rail(base).unwrap(),
        None => gateway,
    }
}

#[tokio::test]
async fn base_must_stay_configured_while_a_base_payment_or_its_result_is_open() {
    let directory = tempfile::tempdir().unwrap();
    let request = json!({"prompt":"base"});
    let base = base_rail(true, BASE_MERCHANT);
    base.finalized.store(false, Ordering::SeqCst);
    let (quote_id, header, paid_at) = {
        let gateway = reopen(directory.path(), Some(base.clone()));
        let GatewayReply::PaymentRequired(quote) = gateway
            .handle(
                request.clone(),
                None,
                None,
                fixture_now(&gateway, 1000).await,
            )
            .await
            .unwrap()
        else {
            panic!("expected a quote")
        };
        let header = payment_header(quote.alternates[0].accepted());
        let paid_at = fixture_now(&gateway, 1001).await;
        assert!(matches!(
            gateway
                .handle(
                    request.clone(),
                    Some(quote.terms.quote_id),
                    Some(&header),
                    paid_at
                )
                .await
                .unwrap(),
            GatewayReply::Pending { .. }
        ));
        (quote.terms.quote_id, header, paid_at)
    };
    // While the Base payment settles, the gateway refuses to start without Base
    // or with another Base merchant.
    for other in [
        None,
        Some(base_rail(
            true,
            "0x0000000000000000000000000000000000000009",
        )),
    ] {
        let gateway = reopen(directory.path(), other);
        assert!(matches!(
            gateway.check_open_payments(paid_at + 1).await,
            Err(GatewayError::Unavailable(_))
        ));
    }
    base.finalized.store(true, Ordering::SeqCst);
    let gateway = reopen(directory.path(), Some(base.clone()));
    gateway.check_open_payments(paid_at + 1).await.unwrap();
    let retry_at = fixture_now(&gateway, paid_at + 2).await;
    assert!(matches!(
        gateway
            .handle(request, Some(quote_id), Some(&header), retry_at)
            .await
            .unwrap(),
        GatewayReply::Complete { network, .. } if network == "eip155:84532"
    ));
    drop(gateway);
    // The buyer can fetch the result for five minutes; after that Base may go.
    let gateway = reopen(directory.path(), None);
    assert!(gateway.check_open_payments(retry_at + 1).await.is_err());
    gateway
        .check_open_payments(retry_at + OUTPUT_RETENTION_MS + 1)
        .await
        .unwrap();
}

#[test]
fn second_rail_on_a_configured_network_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let (gateway, _, _) = two_rail_gateway(directory.path(), true);
    assert!(gateway
        .with_alternate_rail(base_rail(true, BASE_MERCHANT))
        .is_err());
    let primary = Arc::new(Rail {
        settles: AtomicUsize::new(0),
        finalized: std::sync::atomic::AtomicBool::new(true),
    });
    assert!(reopen(directory.path(), None)
        .with_alternate_rail(primary)
        .is_err());
}
