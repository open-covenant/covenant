use super::*;
use crate::{
    ExecutionFailure, ExecutionQuote, Executor, PaymentRail, PaymentTerms, PreparedExecution,
    SettlementStatus, VerifiedPayment,
};
use async_trait::async_trait;
use covenant_compute_buyer::{sign_envelope, BuyerConfig, JobRequest};
use covenant_compute_protocol::JobKind;
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use std::future::IntoFuture;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn offers(now: u64) -> Vec<DiscoveryOffer> {
    let native = InferenceTariff {
        schema_version: 1,
        id: "native".into(),
        revision: 1,
        model: "example".into(),
        base_micro_usdc: 10_000,
        output_cap_micro_usdc_per_million: 7_500_000,
        max_output_tokens: 2048,
        valid_from_ms: now.saturating_sub(1000),
        valid_until_ms: now + 600_000,
    };
    let mut retail = native.clone();
    retail.id = "retail".into();
    retail.base_micro_usdc = 12_000;
    retail.output_cap_micro_usdc_per_million = 9_000_000;
    vec![
        DiscoveryOffer {
            model: "example".into(),
            max_output_tokens: 2048,
            max_input_bytes: 4096,
            pricing: DiscoveryPricing::Legacy {
                retail_micro_usdc: 50_000,
                native_micro_usdc: 30_000,
            },
        },
        DiscoveryOffer {
            model: "example".into(),
            max_output_tokens: 2048,
            max_input_bytes: 4096,
            pricing: DiscoveryPricing::ModelTariff { retail, native },
        },
    ]
}
struct Catalog {
    offers: Vec<DiscoveryOffer>,
    available: AtomicBool,
    calls: AtomicUsize,
    prepares: AtomicUsize,
    identity: LocalIdentity,
}
#[async_trait]
impl Executor for Catalog {
    async fn discovery_offers(&self, _now: u64, _legacy: u64) -> Result<Vec<DiscoveryOffer>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(if self.available.load(Ordering::SeqCst) {
            self.offers.clone()
        } else {
            vec![]
        })
    }
    async fn check_available(
        &self,
        request: &Value,
        now: u64,
        legacy: u64,
        remaining: u64,
    ) -> Result<()> {
        if self
            .discovery_offers(now, legacy)
            .await?
            .into_iter()
            .any(|mut offer| offer.constrain_to_budget(remaining) && offer.matches_request(request))
        {
            Ok(())
        } else {
            Err(GatewayError::Unavailable("no payable supply".into()))
        }
    }
    fn prepare(&self, request: &Value) -> Result<PreparedExecution> {
        self.prepares.fetch_add(1, Ordering::SeqCst);
        let offer = self
            .offers
            .iter()
            .find(|o| o.matches_request(request))
            .unwrap();
        let cost = offer.native_cost(request["max_tokens"].as_u64().unwrap() as u32)?;
        let config = BuyerConfig {
            coordinator_url: "http://127.0.0.1:1".into(),
            poll_interval: Duration::from_millis(1),
            referral_code: None,
            rpc_url: None,
        };
        let envelope = sign_envelope(
            &config,
            &self.identity,
            JobRequest {
                kind: JobKind::InferenceCall,
                input: vec![Content::text("fixture")],
                model: Some("example".into()),
                gpu_class: None,
                min_vram_gb: None,
                min_reputation_bps: None,
                price_micro_usdc: cost,
                deadline_ms: 180_000,
            },
        )
        .unwrap();
        Ok(PreparedExecution {
            operation: None,
            buyer: self.identity.agent_id().pubkey_base58(),
            coordinator: config.coordinator_url,
            job_envelopes: vec![envelope],
            maximum_cost_micro_usdc: cost,
        })
    }
    fn quote(&self, request: &Value, legacy: u64, now: u64) -> Result<ExecutionQuote> {
        let offer = self
            .offers
            .iter()
            .find(|o| o.matches_request(request))
            .unwrap();
        let amount = match &offer.pricing {
            DiscoveryPricing::Legacy { .. } => legacy,
            DiscoveryPricing::ModelTariff { retail, .. } => retail
                .price(request["max_tokens"].as_u64().unwrap() as u32)
                .unwrap(),
        };
        Ok(ExecutionQuote {
            batch_pricing: None,
            prepared: self.prepare(request)?,
            amount_atomic: amount,
            expires_at_ms: now + 90_000,
        })
    }
    async fn execute(&self, _: &PreparedExecution) -> std::result::Result<Value, ExecutionFailure> {
        panic!("discovery must never execute")
    }
}
struct Rail {
    ready: AtomicBool,
}
#[async_trait]
impl PaymentRail for Rail {
    fn network(&self) -> &str {
        "solana:test"
    }
    fn asset(&self) -> &str {
        "USDC"
    }
    fn pay_to(&self) -> &str {
        "merchant"
    }
    fn fee_payer(&self) -> &str {
        "sponsor"
    }
    fn resource_url(&self) -> &str {
        "https://example.test/v1/chat/completions"
    }
    async fn check_offers(&self, _: &[DiscoveryOffer], now: u64) -> Result<()> {
        self.check_price(1, now).await
    }
    async fn check_price(&self, _: u64, _: u64) -> Result<()> {
        if self.ready.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(GatewayError::Unavailable(
                "private bridge secret must not escape".into(),
            ))
        }
    }
    fn authorization_id(&self, _: &str) -> Result<String> {
        panic!("discovery must not authorize")
    }
    async fn verify(&self, _: &str, _: &PaymentTerms) -> Result<VerifiedPayment> {
        panic!("discovery must not verify")
    }
    async fn settle(&self, _: &VerifiedPayment, _: &PaymentTerms) -> Result<SettlementStatus> {
        panic!("discovery must not settle")
    }
}
fn fixture(directory: &std::path::Path, reserve: u64) -> (Arc<Gateway>, Arc<Catalog>, Arc<Rail>) {
    let executor = Arc::new(Catalog {
        offers: offers(crate::now_ms()),
        available: AtomicBool::new(true),
        calls: AtomicUsize::new(0),
        prepares: AtomicUsize::new(0),
        identity: LocalIdentity::generate("discovery@local"),
    });
    let rail = Arc::new(Rail {
        ready: AtomicBool::new(true),
    });
    (
        Arc::new(
            Gateway::open(
                directory,
                "fixture",
                reserve,
                50_000,
                rail.clone(),
                executor.clone(),
            )
            .unwrap(),
        ),
        executor,
        rail,
    )
}

#[tokio::test]
async fn real_http_discovery_is_read_only_and_matches_both_quote_modes() {
    let dir = tempfile::tempdir().unwrap();
    let (gateway, executor, rail) = fixture(dir.path(), 100_000);
    let before = std::fs::read(dir.path().join("gateway.json")).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(axum::serve(listener, crate::router(gateway.clone())).into_future());
    let client = reqwest::Client::new();
    let base = format!("http://{address}");
    let response = client
        .get(format!("{base}/.well-known/x402.json"))
        .header("host", "attacker.invalid")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let manifest: Value = response.json().await.unwrap();
    assert_eq!(manifest["x402Version"], 2);
    assert_eq!(manifest["available"], true);
    assert_eq!(manifest["endpoints"].as_array().unwrap().len(), 3);
    let models: Value = client
        .get(format!("{base}/v1/models"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(models["data"], manifest["models"]);
    let response = client
        .get(format!("{base}/openapi.json"))
        .header("host", "attacker.invalid")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(!response.headers().contains_key("payment-required"));
    let openapi: Value = response.json().await.unwrap();
    assert_eq!(openapi["servers"][0]["url"], "https://example.test");
    assert!(openapi["paths"]["/v1/chat/completions"]["post"].is_object());
    for endpoint in manifest["endpoints"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["method"] == "POST")
    {
        assert_eq!(endpoint["url"], rail.resource_url());
        assert_eq!(endpoint["payment"]["payTo"], rail.pay_to());
        assert_eq!(
            endpoint["inputSchema"]["properties"]["messages"]["maxItems"],
            32
        );
        assert_eq!(endpoint["inputSchema"]["additionalProperties"], false);
    }
    for path in ["/.well-known/x402.json", "/v1/models", "/openapi.json"] {
        let response = client.head(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(response.bytes().await.unwrap().is_empty());
        assert_eq!(
            before,
            std::fs::read(dir.path().join("gateway.json")).unwrap(),
            "HEAD must not mutate the journal"
        );
    }
    assert_eq!(
        before,
        std::fs::read(dir.path().join("gateway.json")).unwrap()
    );
    assert_eq!(executor.prepares.load(Ordering::SeqCst), 0);
    for (cap, tariff, price) in [
        (256, false, 50_000),
        (1, true, 12_009),
        (256, true, 14_304),
        (2048, true, 30_432),
    ] {
        let mut request = json!({"model":"example","messages":[{"role":"user","content":"hello"}],"max_tokens":cap});
        if tariff {
            request["pricing"] = json!("model_tariff_v1");
        }
        let response = client
            .post(format!("{base}/v1/chat/completions"))
            .json(&request)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
        let terms: Value = response.json().await.unwrap();
        assert_eq!(terms["x402Version"], 2);
        assert_eq!(terms["accepts"][0]["amount"], price.to_string());
        assert_eq!(terms["accepts"][0]["network"], rail.network());
        assert_eq!(terms["accepts"][0]["payTo"], rail.pay_to());
    }
    executor.available.store(false, Ordering::SeqCst);
    assert_eq!(gateway.discovery().await.unwrap()["available"], false);
    assert_eq!(
        gateway.discovery_openapi().await.unwrap()["paths"],
        json!({})
    );
    let prepares = executor.prepares.load(Ordering::SeqCst);
    assert!(gateway
        .handle(
            json!({"model":"example","max_tokens":256}),
            None,
            None,
            crate::now_ms()
        )
        .await
        .is_err());
    assert_eq!(executor.prepares.load(Ordering::SeqCst), prepares);
    executor.available.store(true, Ordering::SeqCst);
    rail.ready.store(false, Ordering::SeqCst);
    let response = client
        .get(format!("{base}/openapi.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"error":"live discovery unavailable"})
    );
    let response = client
        .get(format!("{base}/.well-known/x402.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"error":"live discovery unavailable"})
    );
    std::fs::write(dir.path().join("admissions.disabled"), b"closed").unwrap();
    let calls = executor.calls.load(Ordering::SeqCst);
    assert_eq!(
        gateway.discovery_openapi().await.unwrap()["paths"],
        json!({})
    );
    assert_eq!(
        gateway.discovery().await.unwrap()["endpoints"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(executor.calls.load(Ordering::SeqCst), calls);
    task.abort();
}

#[tokio::test]
async fn reserve_limits_advertised_cap_without_changing_tariff_identity() {
    let dir = tempfile::tempdir().unwrap();
    let (gateway, _, _) = fixture(dir.path(), 11_920);
    let manifest = gateway.discovery().await.unwrap();
    let endpoints = manifest["endpoints"].as_array().unwrap();
    assert_eq!(endpoints.len(), 2);
    assert_eq!(
        endpoints[1]["inputSchema"]["properties"]["max_tokens"]["maximum"],
        256
    );
    assert_eq!(endpoints[1]["price"]["tariff"]["max_output_tokens"], 2048);
    assert_eq!(endpoints[1]["price"]["maximum_price_micro_usdc"], 14_304);
    let before = std::fs::read(dir.path().join("gateway.json")).unwrap();
    gateway.disk.lock().await.journal.reserve_micro_usdc = 10_000;
    assert_eq!(gateway.discovery().await.unwrap()["available"], false);
    assert_eq!(
        before,
        std::fs::read(dir.path().join("gateway.json")).unwrap()
    );
}

#[test]
fn tariff_expiry_margin_and_request_modes_are_exact() {
    let mut rows = offers(1000);
    let tariff = &mut rows[1];
    assert!(tariff.current(555_999));
    assert!(!tariff.current(556_000));
    assert!(tariff
        .matches_request(&json!({"model":"example","max_tokens":1,"pricing":"model_tariff_v1"})));
    assert!(!tariff.matches_request(&json!({"model":"example","max_tokens":1})));
    assert!(!rows[0].matches_request(&json!({"model":"example","max_tokens":1,"pricing":null})));
}

#[test]
fn mixed_price_modes_keep_their_own_affordable_caps() {
    let mut rows = offers(1000);
    rows[0].pricing = DiscoveryPricing::Legacy {
        retail_micro_usdc: 50_000,
        native_micro_usdc: 1,
    };
    for offer in &mut rows {
        assert!(offer.constrain_to_budget(11_920));
    }
    let models = model_records(&rows);
    assert_eq!(models[0]["max_output_tokens"], 256);
    assert_eq!(models[0]["pricing"]["max_output_tokens"], 256);
    assert_eq!(models[0]["pricing"]["tariff"]["max_output_tokens"], 2048);
    assert_eq!(models[0]["legacy_pricing"]["max_output_tokens"], 2048);
}

#[test]
fn x402scan_contract_fixture_uses_the_real_quote_serializer() {
    let document = super::openapi::document(
        "https://example.test/v1/chat/completions",
        123,
        &offers(123),
        false,
    )
    .unwrap();
    let request = &document["paths"]["/v1/chat/completions"]["post"]["requestBody"]["content"]
        ["application/json"]["example"];
    assert!(offers(123)[1].matches_request(request));
    let terms = PaymentTerms {
        batch_pricing: None,
        quote_id: uuid::Uuid::nil(),
        request_sha256: crate::request_digest(request).unwrap(),
        network: "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp".into(),
        asset: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".into(),
        pay_to: "11111111111111111111111111111111".into(),
        fee_payer: "11111111111111111111111111111111".into(),
        amount_atomic: 14_304,
        asset_decimals: 6,
        expires_at_ms: 75_123,
        resource_url: "https://example.test/v1/chat/completions".into(),
        memo_binding: crate::MemoBinding::V3,
        token_domain: None,
    };
    // Public, unpayable fixtures can be extracted with --nocapture for an
    // isolated run of the pinned indexer's parser. No wallet or network is used.
    println!("OPENAPI_FIXTURE_JSON={document}");
    let quote = crate::Quote {
        terms,
        alternates: vec![],
    };
    println!(
        "PAYMENT_FIXTURE_JSON={}",
        quote.challenge(crate::discovery::bazaar(request))
    );
}

#[tokio::test]
async fn merchant_branding_is_bounded_and_read_only_with_admissions_closed() {
    let dir = tempfile::tempdir().unwrap();
    let (gateway, executor, rail) = fixture(dir.path(), 100_000);
    let before = std::fs::read(dir.path().join("gateway.json")).unwrap();
    std::fs::write(dir.path().join("admissions.disabled"), b"closed").unwrap();
    rail.ready.store(false, Ordering::SeqCst);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(axum::serve(listener, crate::router(gateway)).into_future());
    let client = reqwest::Client::new();
    let base = format!("http://{address}");
    for (path, content_type, max_bytes, immutable) in [
        ("/", "text/html; charset=utf-8", 8192, false),
        ("/favicon.ico", "image/x-icon", 3000, false),
        (
            "/brand/covenant-icon-02026997.png",
            "image/png",
            56_000,
            true,
        ),
        (
            "/brand/covenant-banner-4b58cf6c.png",
            "image/png",
            162_000,
            true,
        ),
        ("/brand/style.css", "text/css; charset=utf-8", 4096, false),
    ] {
        let response = client
            .get(format!("{base}{path}"))
            .header("host", "attacker.invalid")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], content_type);
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(response.headers()["referrer-policy"], "no-referrer");
        assert_eq!(response.headers()["content-security-policy"], "default-src 'none'; img-src 'self'; style-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'");
        assert_eq!(
            response.headers()["cache-control"],
            if immutable {
                "public, max-age=31536000, immutable"
            } else {
                "public, max-age=300"
            }
        );
        assert!(!response.headers().contains_key("payment-required"));
        let body = response.bytes().await.unwrap();
        assert!(!body.is_empty() && body.len() <= max_bytes);
        if path == "/" {
            let html = std::str::from_utf8(&body).unwrap();
            assert!(html.contains("<title>Covenant Compute</title>"));
            assert!(html
                .contains("content=\"https://example.test/brand/covenant-banner-4b58cf6c.png\""));
            assert!(html.contains("href=\"/brand/covenant-icon-02026997.png\""));
            assert!(!html.contains("attacker.invalid"));
            assert!(!html.contains("private bridge secret"));
        } else if content_type == "image/png" {
            assert!(body.starts_with(b"\x89PNG\r\n\x1a\n"));
        }
        let response = client.head(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], content_type);
        assert!(!response.headers().contains_key("payment-required"));
        assert!(response.bytes().await.unwrap().is_empty());
        assert_eq!(
            client
                .post(format!("{base}{path}"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }
    assert_eq!(
        client
            .get(format!("{base}/brand/not-published.png"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
    assert_eq!(executor.prepares.load(Ordering::SeqCst), 0);
    assert_eq!(
        before,
        std::fs::read(dir.path().join("gateway.json")).unwrap()
    );
    task.abort();
}

struct BaseRail {
    ready: AtomicBool,
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
        "0x5fA1d0C0bfFE257a20027C523093F941834f5D66"
    }
    fn token_domain(&self) -> Option<crate::TokenDomain> {
        Some(crate::TokenDomain {
            name: "USDC".into(),
            version: "2".into(),
        })
    }
    async fn check_offers(&self, _: &[DiscoveryOffer], _: u64) -> Result<()> {
        if self.ready.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(GatewayError::Unavailable(
                "Base verifier unavailable".into(),
            ))
        }
    }
    fn authorization_id(&self, _: &str) -> Result<String> {
        panic!("discovery must not authorize")
    }
    async fn verify(&self, _: &str, _: &PaymentTerms) -> Result<VerifiedPayment> {
        panic!("discovery must not verify")
    }
    async fn settle(&self, _: &VerifiedPayment, _: &PaymentTerms) -> Result<SettlementStatus> {
        panic!("discovery must not settle")
    }
}

#[tokio::test]
async fn discovery_lists_base_only_while_its_verifier_confirms_the_prices() {
    let dir = tempfile::tempdir().unwrap();
    let executor = Arc::new(Catalog {
        offers: offers(crate::now_ms()),
        available: AtomicBool::new(true),
        calls: AtomicUsize::new(0),
        prepares: AtomicUsize::new(0),
        identity: LocalIdentity::generate("discovery-base@local"),
    });
    let base = Arc::new(BaseRail {
        ready: AtomicBool::new(true),
    });
    let gateway = Gateway::open(
        dir.path(),
        "fixture-base",
        50_000,
        50_000,
        Arc::new(Rail {
            ready: AtomicBool::new(true),
        }),
        executor,
    )
    .unwrap()
    .with_alternate_rail(base.clone())
    .unwrap();
    let manifest = gateway.discovery().await.unwrap();
    assert_eq!(
        manifest["description"],
        "Available text inference and its current exact USDC payment terms on Solana and Base."
    );
    let endpoint = &manifest["endpoints"][1];
    assert_eq!(endpoint["payment"]["network"], "solana:test");
    assert_eq!(
        endpoint["payments"],
        json!([endpoint["payment"], {"scheme":"exact","network":"eip155:84532",
            "asset":"0x036CbD53842c5426634e7929541eC2318f3dCF7e","assetDecimals":6,
            "payTo":"0x5fA1d0C0bfFE257a20027C523093F941834f5D66","name":"USDC","version":"2"}])
    );
    let openapi = gateway.discovery_openapi().await.unwrap();
    assert!(openapi["info"]["description"]
        .as_str()
        .unwrap()
        .starts_with("Text inference paid with exact USDC on Solana or Base through x402 v2."));
    base.ready.store(false, Ordering::SeqCst);
    let openapi = gateway.discovery_openapi().await.unwrap();
    assert!(openapi["info"]["description"]
        .as_str()
        .unwrap()
        .starts_with("Text inference paid with exact Solana USDC through x402 v2."));
    let manifest = gateway.discovery().await.unwrap();
    assert!(manifest["endpoints"][1].get("payments").is_none());
    assert_eq!(
        manifest["description"],
        "Available text inference and its current exact Solana USDC payment terms."
    );
}
