// State-machine restart/authorization/reserve tests, with real signed envelopes.
use super::*;
use covenant_compute_buyer::{sign_envelope, BuyerConfig, JobRequest};
use covenant_compute_protocol::JobKind;
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use std::sync::atomic::{AtomicUsize, Ordering};
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
    let GatewayReply::PaymentRequired(terms) = gateway
        .handle(request.clone(), None, None, 1000)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(matches!(
        gateway
            .handle(request.clone(), Some(terms.quote_id), Some("tx1"), 1001)
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
            1002
        )
        .await
        .is_err());
    assert!(gateway
        .handle(request.clone(), Some(terms.quote_id), None, 1003)
        .await
        .is_err());
    assert!(gateway
        .handle(request.clone(), Some(terms.quote_id), Some("tx2"), 1004)
        .await
        .is_err());
    let GatewayReply::PaymentRequired(other) = gateway
        .handle(request.clone(), None, None, 1005)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(gateway
        .handle(request.clone(), Some(other.quote_id), Some("tx3"), 1006)
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
            .handle(request.clone(), Some(terms.quote_id), Some("tx1"), 1007)
            .await
            .unwrap(),
        GatewayReply::Complete { .. }
    ));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        gateway
            .handle(request.clone(), Some(terms.quote_id), Some("tx1"), 301008)
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
    let GatewayReply::PaymentRequired(quote) = gateway
        .handle(request.clone(), None, None, 100)
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
                101
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
    gateway.recover_pending(102).await.unwrap();
    assert!(matches!(
        gateway
            .handle(
                request.clone(),
                Some(quote.quote_id),
                Some("authorization"),
                103
            )
            .await
            .unwrap(),
        GatewayReply::Failed(_)
    ));
    let GatewayReply::PaymentRequired(next) = gateway
        .handle(request.clone(), None, None, 104)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(gateway
        .handle(
            request,
            Some(next.quote_id),
            Some("another-authorization"),
            105
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
    let GatewayReply::PaymentRequired(terms) = gateway
        .handle(request.clone(), None, None, 100)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(matches!(
        gateway
            .handle(request.clone(), Some(terms.quote_id), Some("tx1"), 101)
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
            .handle(request.clone(), Some(terms.quote_id), Some("tx1"), 102)
            .await
            .unwrap(),
        GatewayReply::Complete { .. }
    ));
    let seen = executor.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0], seen[1]);
    drop(seen);
    // Payment revenue never manufactures additional prefunded compute reserve.
    let GatewayReply::PaymentRequired(other) = gateway
        .handle(request.clone(), None, None, 103)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(gateway
        .handle(request, Some(other.quote_id), Some("tx2"), 104)
        .await
        .is_err());
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
    let GatewayReply::PaymentRequired(terms) = gateway
        .handle(request.clone(), None, None, 100)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(matches!(
        gateway
            .handle(request.clone(), Some(terms.quote_id), Some("tx1"), 101)
            .await
            .unwrap(),
        GatewayReply::Failed(_)
    ));
    assert_eq!(rail.settles.load(Ordering::SeqCst), 0);
    let path = directory.path().join("gateway.json");
    std::fs::rename(&path, directory.path().join("saved.json")).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(gateway
        .handle(request.clone(), None, None, 102)
        .await
        .is_err());
    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(directory.path().join("saved.json"), &path).unwrap();
    assert!(gateway.handle(request, None, None, 103).await.is_err());
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
        let GatewayReply::PaymentRequired(terms) = gateway
            .handle(request.clone(), Some(id), None, 1000)
            .await
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(terms.quote_id, id);
    }
    assert_eq!(gateway.disk.lock().await.journal.entries.len(), 1);
    assert!(gateway
        .handle(json!({"prompt":"changed"}), Some(id), None, 1000)
        .await
        .is_err());
    let (a, b) = tokio::join!(
        gateway.handle(request.clone(), Some(id), Some("tx"), 1001),
        gateway.handle(request.clone(), Some(id), Some("tx"), 1001)
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
    gateway.recover_pending(1002).await.unwrap();
    assert!(matches!(
        gateway
            .handle(request, Some(id), Some("tx"), 1003)
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
        .handle(request.clone(), Some(id), None, 1000)
        .await
        .unwrap();
    gateway
        .handle(request.clone(), Some(id), Some("tx"), 1001)
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
            .handle(request, Some(id), Some("tx"), 1002)
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
    assert!(required["accepts"][0]["extra"]["memo"]
        .as_str()
        .unwrap()
        .starts_with("covenant.x402.v2:"));
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
        .handle(request.clone(), Some(id), None, 1000)
        .await
        .unwrap();
    gateway
        .handle(request.clone(), Some(id), Some("tx"), 1001)
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
        .handle(request.clone(), None, None, 1002)
        .await
        .is_err());
    gateway.recover_pending(1003).await.unwrap();
    assert!(matches!(
        gateway
            .handle(request, Some(id), Some("tx"), 1004)
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
            .handle(request.clone(), Some(id), None, 1000)
            .await
            .unwrap();
    }
    gateway
        .handle(request.clone(), Some(paid), Some("paid"), 1001)
        .await
        .unwrap();
    std::fs::write(
        directory.path().join("admissions.disabled"),
        "provider failed",
    )
    .unwrap();
    assert!(!gateway.admissions_open());
    assert!(gateway
        .handle(request.clone(), None, None, 1002)
        .await
        .is_err());
    assert!(gateway
        .handle(request.clone(), Some(unpaid), Some("unpaid"), 1003)
        .await
        .is_err());
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    drop(gateway);
    let gateway = open();
    assert!(!gateway.admissions_open());
    rail.finalized.store(true, Ordering::SeqCst);
    gateway.recover_pending(1004).await.unwrap();
    assert!(matches!(
        gateway
            .handle(request.clone(), Some(paid), Some("paid"), 1005)
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
        .handle(request.clone(), None, None, 1006)
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
        .handle(request.clone(), Some(id), None, 1000)
        .await
        .unwrap();
    assert!(gateway
        .handle(request, Some(id), Some("tx"), 1001)
        .await
        .is_err());
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
    assert_eq!(gateway.disk.lock().await.used_reserve(), 0);
}
