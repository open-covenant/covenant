use super::*;
use crate::batch::BatchExecutor;
use covenant_compute_buyer::{sign_envelope, BuyerConfig, JobRequest};
use covenant_compute_protocol::{inference_batch::InferenceBatchResult, JobKind};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use std::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Mutex as StdMutex,
};

struct Rail {
    verifies: AtomicUsize,
    settles: AtomicUsize,
    delay_ms: u64,
}
#[async_trait]
impl PaymentRail for Rail {
    fn network(&self) -> &str {
        "test:batch"
    }
    fn asset(&self) -> &str {
        "USDC"
    }
    fn pay_to(&self) -> &str {
        "merchant"
    }
    fn resource_url(&self) -> &str {
        "https://compute.example/v1/chat/completions"
    }
    fn authorization_id(&self, header: &str) -> Result<String> {
        Ok(header.into())
    }
    async fn verify(&self, header: &str, _: &PaymentTerms) -> Result<VerifiedPayment> {
        self.verifies.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
        Ok(VerifiedPayment {
            payer: "payer".into(),
            authorization_id: header.into(),
            transaction_id: header.into(),
            encoded_transaction: header.into(),
        })
    }
    async fn settle(
        &self,
        payment: &VerifiedPayment,
        _: &PaymentTerms,
    ) -> Result<SettlementStatus> {
        self.settles.fetch_add(1, Ordering::SeqCst);
        Ok(SettlementStatus::Finalized {
            transaction_id: payment.transaction_id.clone(),
        })
    }
}
struct Execute {
    identity: LocalIdentity,
    calls: StdMutex<Vec<Uuid>>,
    unknown_call: AtomicUsize,
    unpaid_call: AtomicUsize,
    oversized_call: AtomicUsize,
    retail: AtomicU64,
    quote_ttl: u64,
    budget: u64,
}
impl Execute {
    fn new() -> Self {
        Self {
            identity: LocalIdentity::generate("batch@local"),
            calls: StdMutex::new(vec![]),
            unknown_call: AtomicUsize::new(0),
            unpaid_call: AtomicUsize::new(0),
            oversized_call: AtomicUsize::new(0),
            retail: AtomicU64::new(20),
            quote_ttl: 90_000,
            budget: 1000,
        }
    }
}
#[async_trait]
impl Executor for Execute {
    fn prepare(&self, request: &Value) -> Result<PreparedExecution> {
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
                input: vec![Content::text(
                    request["messages"][0]["content"].as_str().unwrap(),
                )],
                model: Some("model".into()),
                gpu_class: None,
                min_vram_gb: None,
                min_reputation_bps: None,
                price_micro_usdc: 10,
                deadline_ms: 180_000,
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
    fn quote(&self, request: &Value, _: u64, now: u64) -> Result<ExecutionQuote> {
        Ok(ExecutionQuote {
            batch_pricing: None,
            prepared: self.prepare(request)?,
            amount_atomic: self.retail.load(Ordering::SeqCst),
            expires_at_ms: now + self.quote_ttl,
        })
    }
    fn validate_batch_budget(&self, amount: u64, _: u64) -> Result<()> {
        if amount > self.budget {
            return Err(GatewayError::Unavailable("batch budget exceeded".into()));
        }
        Ok(())
    }
    async fn discovery_offers(&self, _: u64, _: u64) -> Result<Vec<discovery::DiscoveryOffer>> {
        Ok(vec![discovery::DiscoveryOffer {
            model: "model".into(),
            max_output_tokens: 8,
            max_input_bytes: 1024,
            pricing: discovery::DiscoveryPricing::Legacy {
                retail_micro_usdc: 20,
                native_micro_usdc: 10,
            },
        }])
    }
    async fn execute(
        &self,
        prepared: &PreparedExecution,
    ) -> std::result::Result<Value, ExecutionFailure> {
        assert_eq!(prepared.operation, None);
        let id = prepared.job_envelopes[0].payload.job_id;
        let count = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(id);
            calls.len()
        };
        if self.unknown_call.load(Ordering::SeqCst) == count {
            return Err(ExecutionFailure {
                message: "unknown transport".into(),
                definitely_unpaid: false,
                terminal_seller_loss: false,
            });
        }
        if self.unpaid_call.load(Ordering::SeqCst) == count {
            return Err(ExecutionFailure {
                message: "verified unpaid refusal".into(),
                definitely_unpaid: true,
                terminal_seller_loss: false,
            });
        }
        if self.oversized_call.load(Ordering::SeqCst) == count {
            return Ok(json!({"text":"x".repeat(256*1024)}));
        }
        Ok(json!({"covenant":{"job_id":id},"text":"fixture"}))
    }
}
fn request() -> Value {
    json!({"version":1,"requests":[{"model":"model","max_tokens":1,"messages":[{"role":"user","content":"first"}]},{"model":"model","max_tokens":2,"messages":[{"role":"user","content":"second"}]}]})
}
fn wrapper(root: &Path, inner: Arc<dyn Executor>, enabled: bool) -> Arc<BatchExecutor> {
    Arc::new(BatchExecutor {
        inner,
        directory: root.join("batches"),
        enabled,
    })
}
async fn now(gateway: &Gateway) -> u64 {
    gateway
        .disk
        .lock()
        .await
        .journal
        .watermark_ms
        .max(crate::now_ms())
        + 1
}
async fn quote(gateway: &Gateway) -> PaymentTerms {
    let GatewayReply::PaymentRequired(terms) = gateway
        .handle(request(), None, None, now(gateway).await)
        .await
        .unwrap()
    else {
        panic!("expected quote")
    };
    terms.terms
}

#[tokio::test]
async fn unknown_child_restart_skips_completed_preserves_original_ids_and_disabled_recovery() {
    let root = tempfile::tempdir().unwrap();
    let inner = Arc::new(Execute::new());
    inner.unknown_call.store(2, Ordering::SeqCst);
    let rail = Arc::new(Rail {
        verifies: AtomicUsize::new(0),
        settles: AtomicUsize::new(0),
        delay_ms: 0,
    });
    let gateway = Gateway::open(
        root.path(),
        "batch",
        1000,
        20,
        rail.clone(),
        wrapper(root.path(), inner.clone(), true),
    )
    .unwrap();
    let terms = quote(&gateway).await;
    assert_eq!(terms.amount_atomic, 40);
    assert_eq!(terms.memo_binding, MemoBinding::V3);
    assert_eq!(terms.memo().len(), 32);
    assert_eq!(terms.request_sha256, request_digest(&request()).unwrap());
    assert_eq!(
        terms.memo(),
        short_memo(terms.quote_id, &terms.request_sha256)
    );
    let mut reordered = request();
    reordered["requests"].as_array_mut().unwrap().reverse();
    assert_ne!(
        terms.memo(),
        short_memo(terms.quote_id, &request_digest(&reordered).unwrap())
    );

    assert_eq!(
        terms.resource_url,
        "https://compute.example/v1/inference/batches"
    );
    let ids: Vec<_> = terms
        .batch_pricing
        .as_ref()
        .unwrap()
        .children
        .iter()
        .map(|child| child.job_id)
        .collect();
    assert!(matches!(
        gateway
            .handle(
                request(),
                Some(terms.quote_id),
                Some("payment"),
                now(&gateway).await
            )
            .await
            .unwrap(),
        GatewayReply::Pending { .. }
    ));
    assert_eq!(*inner.calls.lock().unwrap(), ids[..2]);
    assert_eq!(rail.settles.load(Ordering::SeqCst), 0);
    drop(gateway);
    inner.retail.store(999, Ordering::SeqCst);
    let gateway = Gateway::open(
        root.path(),
        "batch",
        1000,
        20,
        rail.clone(),
        wrapper(root.path(), inner.clone(), false),
    )
    .unwrap();
    assert!(gateway
        .handle(request(), None, None, now(&gateway).await)
        .await
        .is_err());
    gateway.recover_pending(now(&gateway).await).await.unwrap();
    let GatewayReply::Complete { output, .. } = gateway
        .handle(
            request(),
            Some(terms.quote_id),
            Some("payment"),
            now(&gateway).await,
        )
        .await
        .unwrap()
    else {
        panic!("expected original batch")
    };
    let result: InferenceBatchResult = serde_json::from_value(output).unwrap();
    result.verify().unwrap();
    assert_eq!(
        result.batch_id,
        terms.batch_pricing.as_ref().unwrap().batch_id
    );
    assert_eq!(
        result
            .items
            .iter()
            .map(|item| item.leaf.job_id)
            .collect::<Vec<_>>(),
        ids
    );
    assert_eq!(*inner.calls.lock().unwrap(), vec![ids[0], ids[1], ids[1]]);
    assert_eq!(rail.settles.load(Ordering::SeqCst), 1);
    assert!(!root
        .path()
        .join("batches")
        .join(result.batch_id.to_string())
        .exists());
    gateway
        .handle(
            request(),
            Some(terms.quote_id),
            Some("payment"),
            now(&gateway).await,
        )
        .await
        .unwrap();
    assert_eq!(rail.settles.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn terminal_partial_failure_retains_full_seller_reserve_without_customer_settlement() {
    for failure_at in [1, 2] {
        let root = tempfile::tempdir().unwrap();
        let inner = Arc::new(Execute::new());
        inner.unpaid_call.store(failure_at, Ordering::SeqCst);
        let rail = Arc::new(Rail {
            verifies: AtomicUsize::new(0),
            settles: AtomicUsize::new(0),
            delay_ms: 0,
        });
        let gateway = Gateway::open(
            root.path(),
            "batch",
            1000,
            20,
            rail.clone(),
            wrapper(root.path(), inner.clone(), true),
        )
        .unwrap();
        let terms = quote(&gateway).await;
        assert!(matches!(
            gateway
                .handle(
                    request(),
                    Some(terms.quote_id),
                    Some("payment"),
                    now(&gateway).await
                )
                .await
                .unwrap(),
            GatewayReply::Failed(_)
        ));
        assert_eq!(
            gateway.disk.lock().await.used_reserve(),
            if failure_at == 1 { 0 } else { 20 }
        );
        assert_eq!(rail.settles.load(Ordering::SeqCst), 0);
        assert_eq!(inner.calls.lock().unwrap().len(), failure_at);
        drop(gateway);
        let gateway = Gateway::open(
            root.path(),
            "batch",
            1000,
            20,
            rail.clone(),
            wrapper(root.path(), inner.clone(), false),
        )
        .unwrap();
        assert!(matches!(
            gateway
                .handle(
                    request(),
                    Some(terms.quote_id),
                    Some("payment"),
                    now(&gateway).await
                )
                .await
                .unwrap(),
            GatewayReply::Failed(_)
        ));
        assert_eq!(inner.calls.lock().unwrap().len(), failure_at);
    }
}
#[tokio::test]
async fn frozen_unpaid_batch_disables_new_payment_and_expiry_prevents_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let mut raw = Execute::new();
    raw.quote_ttl = 2000;
    let inner = Arc::new(raw);
    let rail = Arc::new(Rail {
        verifies: AtomicUsize::new(0),
        settles: AtomicUsize::new(0),
        delay_ms: 2100,
    });
    let gateway = Gateway::open(
        root.path(),
        "batch",
        1000,
        20,
        rail.clone(),
        wrapper(root.path(), inner.clone(), true),
    )
    .unwrap();
    let terms = quote(&gateway).await;
    assert!(gateway
        .handle(
            request(),
            Some(terms.quote_id),
            Some("payment"),
            now(&gateway).await
        )
        .await
        .is_err());
    assert!(inner.calls.lock().unwrap().is_empty());
    assert_eq!(rail.settles.load(Ordering::SeqCst), 0);
    drop(gateway);
    let gateway = Gateway::open(
        root.path(),
        "batch",
        1000,
        20,
        rail,
        wrapper(root.path(), inner.clone(), false),
    )
    .unwrap();
    assert!(gateway
        .handle(
            request(),
            Some(terms.quote_id),
            Some("payment"),
            now(&gateway).await
        )
        .await
        .is_err());
}
#[test]
fn quote_rejects_aggregate_daily_budget_and_pins_unique_children() {
    let root = tempfile::tempdir().unwrap();
    let mut inner = Execute::new();
    inner.budget = 15;
    let batch = wrapper(root.path(), Arc::new(inner), true);
    assert!(batch
        .quote(&request(), 20, crate::now_ms())
        .err()
        .unwrap()
        .to_string()
        .contains("budget"));
    let batch = wrapper(root.path(), Arc::new(Execute::new()), true);
    let quote = batch.quote(&request(), 20, crate::now_ms()).unwrap();
    let mut manifest = quote.batch_pricing.unwrap();
    assert_eq!(manifest.total().unwrap(), 40);
    manifest.children[1].job_id = manifest.children[0].job_id;
    assert!(manifest.total().is_err());
    assert!(batch.prepare_legacy(&request()).is_err());
}
#[tokio::test]
async fn http_routes_reject_crossed_single_and_batch_bodies_before_payment_quote() {
    let root = tempfile::tempdir().unwrap();
    let inner = Arc::new(Execute::new());
    let rail = Arc::new(Rail {
        verifies: AtomicUsize::new(0),
        settles: AtomicUsize::new(0),
        delay_ms: 0,
    });
    let gateway = Arc::new(
        Gateway::open(
            root.path(),
            "batch",
            1000,
            20,
            rail,
            wrapper(root.path(), inner.clone(), true),
        )
        .unwrap(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, router(gateway)).await.unwrap();
    });
    let http = reqwest::Client::new();
    for (path, body) in [
        ("/v1/chat/completions", request()),
        ("/v1/inference/batches", request()["requests"][0].clone()),
    ] {
        let response = http
            .post(format!("http://{address}{path}"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(!response.headers().contains_key("payment-required"));
    }
    assert!(inner.calls.lock().unwrap().is_empty());
    task.abort();
}
#[test]
fn real_tariff_batch_sums_each_rounded_child_and_freezes_native_quotes() {
    use covenant_compute_buyer::durable_budget::DurableBudget;
    use covenant_compute_protocol::{parse_tariff_input, InferenceTariff};
    let root = tempfile::tempdir().unwrap();
    let identity = Arc::new(LocalIdentity::generate("batch-pricing@local"));
    let now = crate::now_ms();
    let config = BuyerConfig {
        coordinator_url: "https://compute.example".into(),
        poll_interval: std::time::Duration::from_millis(1),
        referral_code: None,
        rpc_url: None,
    };
    let native = InferenceTariff {
        schema_version: 1,
        id: "native".into(),
        revision: 1,
        model: "model".into(),
        base_micro_usdc: 100,
        output_cap_micro_usdc_per_million: 1_500_001,
        max_output_tokens: 8,
        valid_from_ms: now - 1,
        valid_until_ms: now + 3_600_000,
    };
    let retail = InferenceTariff {
        id: "retail".into(),
        base_micro_usdc: 120,
        output_cap_micro_usdc_per_million: 2_500_001,
        ..native.clone()
    };
    let expected_native = (1..=2).map(|cap| native.price(cap).unwrap()).sum::<u64>();
    let expected_retail = (1..=2).map(|cap| retail.price(cap).unwrap()).sum::<u64>();
    let executor = buyer::BuyerExecutor {
        budget: DurableBudget::open(
            &root.path().join("buyer"),
            &identity.agent_id().pubkey_base58(),
            &config.coordinator_url,
            1000,
        )
        .unwrap(),
        config,
        identity,
        http: reqwest::Client::new(),
        model: "model".into(),
        offer_micro_usdc: 150,
        max_tokens: 8,
        max_input_bytes: 4096,
        approved_operators: vec![],
        tariffs: Some(Arc::new(
            pricing::TariffBook::new(vec![pricing::ModelTariff {
                native: native.clone(),
                retail,
            }])
            .unwrap(),
        )),
    };
    let batch = wrapper(root.path(), Arc::new(executor), true);
    let mut request = request();
    for child in request["requests"].as_array_mut().unwrap() {
        child["pricing"] = json!("model_tariff_v1");
    }
    let quote = batch.quote(&request, 200, now).unwrap();
    assert_eq!(quote.amount_atomic, expected_retail);
    assert_eq!(quote.prepared.maximum_cost_micro_usdc, expected_native);
    assert!(expected_native < expected_retail);
    let manifest = quote.batch_pricing.as_ref().unwrap();
    assert_eq!(manifest.total().unwrap(), expected_retail);
    for (index, envelope) in quote.prepared.job_envelopes.iter().enumerate() {
        let tariff = parse_tariff_input(&envelope.payload.input)
            .unwrap()
            .unwrap();
        assert_eq!(tariff.tariff, native);
        assert_eq!(
            envelope.payload.price_micro_usdc,
            native.price(index as u32 + 1).unwrap()
        );
        assert_eq!(manifest.children[index].job_id, envelope.payload.job_id);
    }
    batch
        .validate_admission(&quote.prepared, now + 29_000)
        .unwrap();
    assert!(batch
        .validate_admission(&quote.prepared, now + 31_000)
        .is_err());
}
#[tokio::test]
async fn crash_after_terminal_save_cleans_orphaned_child_plaintext_on_recovery() {
    let root = tempfile::tempdir().unwrap();
    let inner = Arc::new(Execute::new());
    let rail = Arc::new(Rail {
        verifies: AtomicUsize::new(0),
        settles: AtomicUsize::new(0),
        delay_ms: 0,
    });
    let gateway = Gateway::open(
        root.path(),
        "batch",
        1000,
        20,
        rail,
        wrapper(root.path(), inner, true),
    )
    .unwrap();
    let terms = quote(&gateway).await;
    gateway
        .handle(
            request(),
            Some(terms.quote_id),
            Some("payment"),
            now(&gateway).await,
        )
        .await
        .unwrap();
    let progress = root
        .path()
        .join("batches")
        .join(terms.batch_pricing.unwrap().batch_id.to_string());
    std::fs::create_dir_all(&progress).unwrap();
    std::fs::write(progress.join("progress.json"), b"cached child plaintext").unwrap();
    gateway.recover_pending(now(&gateway).await).await.unwrap();
    assert!(!progress.exists());
}
#[tokio::test]
async fn batch_freshness_expiry_rejects_before_payment_verification_or_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let inner = Arc::new(Execute::new());
    let rail = Arc::new(Rail {
        verifies: AtomicUsize::new(0),
        settles: AtomicUsize::new(0),
        delay_ms: 0,
    });
    let gateway = Gateway::open(
        root.path(),
        "batch",
        1000,
        20,
        rail.clone(),
        wrapper(root.path(), inner.clone(), true),
    )
    .unwrap();
    let terms = quote(&gateway).await;
    let prepared = gateway.disk.lock().await.journal.entries[&terms.quote_id]
        .quoted_execution
        .clone()
        .unwrap();
    assert!(terms.expires_at_ms <= prepared.job_envelopes[1].payload.issued_at_ms + 30_000);
    assert!(gateway
        .handle(
            request(),
            Some(terms.quote_id),
            Some("payment"),
            terms.expires_at_ms
        )
        .await
        .is_err());
    assert_eq!(rail.verifies.load(Ordering::SeqCst), 0);
    assert_eq!(rail.settles.load(Ordering::SeqCst), 0);
    assert!(inner.calls.lock().unwrap().is_empty());
}
#[test]
fn initial_execution_limit_is_smaller_than_proof_protocol_and_checked_pre_quote() {
    let root = tempfile::tempdir().unwrap();
    let batch = wrapper(root.path(), Arc::new(Execute::new()), true);
    let mut body = request();
    let child = body["requests"][0].clone();
    body["requests"].as_array_mut().unwrap().push(child);
    assert!(batch::parse(&body).is_ok());
    assert!(batch.validate_request(&body).is_err());
    assert!(batch.quote(&body, 20, crate::now_ms()).is_err());
}
#[tokio::test]
async fn oversized_verified_child_is_terminal_seller_loss_without_payment_or_retry() {
    let root = tempfile::tempdir().unwrap();
    let inner = Arc::new(Execute::new());
    inner.oversized_call.store(1, Ordering::SeqCst);
    let rail = Arc::new(Rail {
        verifies: AtomicUsize::new(0),
        settles: AtomicUsize::new(0),
        delay_ms: 0,
    });
    let gateway = Gateway::open(
        root.path(),
        "batch",
        1000,
        20,
        rail.clone(),
        wrapper(root.path(), inner.clone(), true),
    )
    .unwrap();
    let terms = quote(&gateway).await;
    assert!(matches!(
        gateway
            .handle(
                request(),
                Some(terms.quote_id),
                Some("payment"),
                now(&gateway).await
            )
            .await
            .unwrap(),
        GatewayReply::Failed(_)
    ));
    assert_eq!(gateway.disk.lock().await.used_reserve(), 20);
    assert_eq!(rail.settles.load(Ordering::SeqCst), 0);
    assert_eq!(inner.calls.lock().unwrap().len(), 1);
}
#[test]
fn journal_size_is_checked_before_reading_or_parsing_sparse_oversized_file() {
    let root = tempfile::tempdir().unwrap();
    let file = File::create(root.path().join("gateway.json")).unwrap();
    file.set_len(MAX_GATEWAY_JOURNAL_BYTES as u64 + 1).unwrap();
    let rail = Arc::new(Rail {
        verifies: AtomicUsize::new(0),
        settles: AtomicUsize::new(0),
        delay_ms: 0,
    });
    let error = Gateway::open(
        root.path(),
        "batch",
        1000,
        20,
        rail,
        wrapper(root.path(), Arc::new(Execute::new()), true),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("size limit"));
}

#[tokio::test]
async fn mixed_v5_base_payment_and_batch_migrate_to_v6_without_cross_rail_or_duplicate_execution() {
    let root = tempfile::tempdir().unwrap();
    let inner = Arc::new(Execute::new());
    let rail = Arc::new(Rail {
        verifies: AtomicUsize::new(0),
        settles: AtomicUsize::new(0),
        delay_ms: 0,
    });
    let base = super::tests::base_rail(true, super::tests::BASE_MERCHANT);
    base.finalized.store(false, Ordering::SeqCst);
    let gateway = Gateway::open(
        root.path(),
        "mixed-v5",
        1000,
        20,
        rail.clone(),
        wrapper(root.path(), inner.clone(), true),
    )
    .unwrap()
    .with_alternate_rail(base.clone())
    .unwrap();
    let single = request()["requests"][0].clone();
    let GatewayReply::PaymentRequired(single_quote) = gateway
        .handle(single.clone(), None, None, now(&gateway).await)
        .await
        .unwrap()
    else {
        panic!("expected single quote")
    };
    assert_eq!(
        single_quote.alternates.len(),
        1,
        "Base remains available on single requests"
    );
    let base_terms = single_quote.alternates[0].clone();
    let base_header = base64::engine::general_purpose::STANDARD
        .encode(json!({"x402Version":2,"accepted":base_terms.accepted()}).to_string());
    assert!(matches!(
        gateway
            .handle(
                single.clone(),
                Some(base_terms.quote_id),
                Some(&base_header),
                now(&gateway).await
            )
            .await
            .unwrap(),
        GatewayReply::Pending { .. }
    ));
    assert_eq!(base.verifications.load(Ordering::SeqCst), 1);
    assert_eq!(rail.verifies.load(Ordering::SeqCst), 0);
    let GatewayReply::PaymentRequired(batch_quote) = gateway
        .handle(request(), None, None, now(&gateway).await)
        .await
        .unwrap()
    else {
        panic!("expected batch quote")
    };
    assert!(
        batch_quote.alternates.is_empty(),
        "batch admission must not offer a Base fallback"
    );
    assert_eq!(
        batch_quote.requirements()["accepts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(batch_quote
        .challenge(Some(json!({"info":{}})))
        .get("extensions")
        .is_none());
    let terms = batch_quote.terms;
    let child_ids: Vec<_> = terms
        .batch_pricing
        .as_ref()
        .unwrap()
        .children
        .iter()
        .map(|c| c.job_id)
        .collect();
    inner.unknown_call.store(3, Ordering::SeqCst);
    assert!(matches!(
        gateway
            .handle(
                request(),
                Some(terms.quote_id),
                Some("original-batch-payment"),
                now(&gateway).await
            )
            .await
            .unwrap(),
        GatewayReply::Pending { .. }
    ));
    assert_eq!(rail.verifies.load(Ordering::SeqCst), 1);
    assert_eq!(rail.settles.load(Ordering::SeqCst), 0);
    drop(gateway);
    let journal_path = root.path().join("gateway.json");
    let mut legacy: Value = serde_json::from_slice(&std::fs::read(&journal_path).unwrap()).unwrap();
    legacy["version"] = json!(5);
    assert!(
        legacy["entries"][base_terms.quote_id.to_string()]["terms"]["token_domain"].is_object()
    );
    assert!(legacy["entries"][terms.quote_id.to_string()]["terms"]["batch_pricing"].is_object());
    std::fs::write(&journal_path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    // A Base payment cannot be silently redirected to the primary Solana rail.
    let missing_base = Gateway::open(
        root.path(),
        "mixed-v5",
        1000,
        20,
        rail.clone(),
        wrapper(root.path(), inner.clone(), false),
    )
    .unwrap();
    assert!(missing_base
        .check_open_payments(now(&missing_base).await)
        .await
        .is_err());
    assert_eq!(inner.calls.lock().unwrap().len(), 3);
    drop(missing_base);
    let gateway = Gateway::open(
        root.path(),
        "mixed-v5",
        1000,
        20,
        rail.clone(),
        wrapper(root.path(), inner.clone(), false),
    )
    .unwrap()
    .with_alternate_rail(base.clone())
    .unwrap()
    .with_admissions_enabled(false);
    let migrated: Value = serde_json::from_slice(&std::fs::read(&journal_path).unwrap()).unwrap();
    assert_eq!(migrated["version"], 6);
    assert_eq!(
        migrated["entries"], legacy["entries"],
        "neither v5 feature may lose frozen financial fields"
    );
    gateway
        .check_open_payments(now(&gateway).await)
        .await
        .unwrap();
    base.finalized.store(true, Ordering::SeqCst);
    inner.unknown_call.store(0, Ordering::SeqCst);
    gateway.recover_pending(now(&gateway).await).await.unwrap();
    let calls = inner.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 4);
    assert_eq!(
        &calls[1..],
        &[child_ids[0], child_ids[1], child_ids[1]],
        "completed child is skipped; unknown outcome reuses only its original ID"
    );
    assert_eq!(base.verifications.load(Ordering::SeqCst), 1);
    assert_eq!(rail.verifies.load(Ordering::SeqCst), 1);
    assert_eq!(
        base.settles.load(Ordering::SeqCst),
        2,
        "one pending Base settlement plus original-authorization recovery"
    );
    assert_eq!(rail.settles.load(Ordering::SeqCst), 1);
    assert!(
        matches!(gateway.handle(single, Some(base_terms.quote_id), Some(&base_header), now(&gateway).await).await.unwrap(), GatewayReply::Complete { network, .. } if network == base_terms.network)
    );
    assert!(
        matches!(gateway.handle(request(), Some(terms.quote_id), Some("original-batch-payment"), now(&gateway).await).await.unwrap(), GatewayReply::Complete { network, .. } if network == terms.network)
    );
    gateway.recover_pending(now(&gateway).await).await.unwrap();
    assert_eq!(inner.calls.lock().unwrap().len(), 4);
    assert_eq!(base.settles.load(Ordering::SeqCst), 2);
    assert_eq!(rail.settles.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.disk.lock().await.used_reserve(), 30);
}

#[tokio::test]
async fn base_primary_cannot_admit_batch_or_fall_back_to_another_rail() {
    let root = tempfile::tempdir().unwrap();
    let inner = Arc::new(Execute::new());
    let base = super::tests::base_rail(true, super::tests::BASE_MERCHANT);
    let solana = Arc::new(Rail {
        verifies: AtomicUsize::new(0),
        settles: AtomicUsize::new(0),
        delay_ms: 0,
    });
    let gateway = Gateway::open(
        root.path(),
        "no-base-batch",
        1000,
        20,
        base.clone(),
        wrapper(root.path(), inner.clone(), true),
    )
    .unwrap()
    .with_alternate_rail(solana.clone())
    .unwrap();
    assert!(gateway
        .handle(request(), None, None, now(&gateway).await)
        .await
        .is_err());
    assert_eq!(base.verifications.load(Ordering::SeqCst), 0);
    assert_eq!(solana.verifies.load(Ordering::SeqCst), 0);
    assert!(inner.calls.lock().unwrap().is_empty());
    assert!(gateway.disk.lock().await.journal.entries.is_empty());
}
