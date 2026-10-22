//! A real gateway process, private temporary state, and a loopback payment rail.
//! No provider, blockchain, funded account, or public endpoint is used.
use axum::{routing::post, Json, Router};
use covenant_compute_protocol::{JobEnvelopePayload, SignedJobEnvelope};
use covenant_identity::LocalIdentity;
use serde_json::{json, Value};
use std::{
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn invalid_tariff_configuration_keeps_existing_settlement_recovery_alive() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("gateway");
    std::fs::create_dir(&home).unwrap();
    let identity =
        LocalIdentity::load_or_create(&home.join("identity.key"), "gateway@local").unwrap();
    let buyer = identity.agent_id().pubkey_base58();
    let public_key = bs58::encode([1u8; 32]).into_string();
    let coordinator = "http://127.0.0.1:1";
    let network = "solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1";
    let asset = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU";
    let resource = "https://fixture.invalid/v1/chat/completions";
    let quote_id = "00000000-0000-4000-8000-000000000001";
    let transaction = "synthetic-retained-transaction";
    let authorization = "synthetic-retained-authorization";
    let namespace = format!(
        "{buyer}\0{coordinator}\0fixture-model\0{max_tokens}\0{offer}\0{max_input}\0{public_key}\0{network}\0{asset}\0{public_key}\0{public_key}\0{resource}\0{price}",
        max_tokens = 100, offer = 10, max_input = 4096, price = 20,
    );
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../sdk/compute-x402-client/test/fixtures/native-response.json"
    ))
    .unwrap();
    let mut payload: JobEnvelopePayload =
        serde_json::from_value(fixture["response"]["covenant"]["envelope"]["payload"].clone())
            .unwrap();
    payload.buyer = identity.agent_id();
    let envelope = SignedJobEnvelope::sign(payload, &identity).unwrap();
    let terms = json!({
        "quote_id":quote_id, "request_sha256":"ab".repeat(32), "network":network,
        "asset":asset, "pay_to":public_key, "amount_atomic":20,
        "asset_decimals":6, "expires_at_ms":1, "fee_payer":public_key,
        "resource_url":resource,
    });
    let payment = json!({
        "payer":public_key, "authorization_id":authorization,
        "transaction_id":transaction, "encoded_transaction":"synthetic-retained-signed-bytes",
    });
    let journal_path = home.join("gateway.json");
    std::fs::write(
        &journal_path,
        serde_json::to_vec(&json!({
            "version":2, "namespace":namespace, "watermark_ms":0, "reserve_micro_usdc":100,
            "entries":{quote_id:{
                "terms":terms,
                "phase":{"phase":"settling", "payment":payment,
                    "prepared":{"buyer":buyer,"coordinator":coordinator,
                        "job_envelopes":[envelope],"maximum_cost_micro_usdc":10},
                    "output":{"retained_result":"synthetic already-verified output"}}
            }}
        }))
        .unwrap(),
    )
    .unwrap();
    let tariff_path = root.path().join("broken-tariffs.json");
    std::fs::write(&tariff_path, b"not json").unwrap();

    let received = Arc::new(Mutex::new(Vec::<Value>::new()));
    let capture = received.clone();
    let bridge_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bridge_address = bridge_listener.local_addr().unwrap();
    let bridge = tokio::spawn(async move {
        axum::serve(
            bridge_listener,
            Router::new().route(
                "/settle",
                post(move |Json(body): Json<Value>| {
                    capture.lock().unwrap().push(body);
                    async move { Json(json!({"status":"finalized","transaction_id":transaction})) }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let mut process = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_covenant-compute-gateway"))
            .env_clear()
            .env("COVENANT_GATEWAY_ENABLED", "true")
            .env("COVENANT_GATEWAY_HOME", &home)
            .env("COVENANT_GATEWAY_LISTEN", address.to_string())
            .env("COVENANT_COMPUTE_COORDINATOR_URL", coordinator)
            .env("COVENANT_COMPUTE_BUDGET_HOME", root.path().join("budget"))
            .env("COVENANT_COMPUTE_DAILY_BUDGET_MICRO_USDC", "100")
            .env("COVENANT_GATEWAY_RESERVE_MICRO_USDC", "100")
            .env("COVENANT_GATEWAY_PRICE_MICRO_USDC", "20")
            .env("COVENANT_GATEWAY_OFFER_MICRO_USDC", "10")
            .env("COVENANT_GATEWAY_MAX_INPUT_BYTES", "4096")
            .env("COVENANT_GATEWAY_APPROVED_OPERATOR_PUBKEYS", &public_key)
            .env(
                "COVENANT_GATEWAY_BRIDGE_URL",
                format!("http://{bridge_address}"),
            )
            .env(
                "COVENANT_GATEWAY_BRIDGE_SECRET",
                "fixture-private-bridge-secret-32-characters",
            )
            .env("COVENANT_GATEWAY_NETWORK", network)
            .env("COVENANT_GATEWAY_ASSET", asset)
            .env("COVENANT_GATEWAY_PAY_TO", &public_key)
            .env("COVENANT_GATEWAY_FEE_PAYER", &public_key)
            .env("COVENANT_GATEWAY_RESOURCE_URL", resource)
            .env("COVENANT_GATEWAY_MODEL", "fixture-model")
            .env("COVENANT_GATEWAY_MAX_TOKENS", "100")
            .env("COVENANT_GATEWAY_ADMISSIONS_ENABLED", "true")
            .env("COVENANT_GATEWAY_TARIFFS_FILE", &tariff_path)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let http = reqwest::Client::new();
    let mut completed = None;
    for _ in 0..100 {
        assert!(process.0.try_wait().unwrap().is_none(), "gateway exited");
        if let Ok(response) = http.get(format!("http://{address}/health")).send().await {
            assert!(response.status().is_success());
            let health: Value = response.json().await.unwrap();
            assert_eq!(health["admissions_enabled"], false);
            let journal: Value =
                serde_json::from_slice(&std::fs::read(&journal_path).unwrap()).unwrap();
            if journal["entries"][quote_id]["phase"]["phase"] == "complete" {
                completed = Some(journal);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let journal = completed.expect("the saved settlement did not recover");
    let saved = &journal["entries"][quote_id]["phase"];
    assert_eq!(journal["version"], 6);
    assert_eq!(journal["namespace"], namespace);
    assert_eq!(journal["reserve_micro_usdc"], 100);
    assert_eq!(saved["transaction_id"], transaction);
    assert_eq!(saved["authorization_id"], authorization);
    assert_eq!(saved["reserve_cost_micro_usdc"], 10);
    assert_eq!(received.lock().unwrap().len(), 1);
    assert_eq!(received.lock().unwrap()[0]["payment"], payment);
    assert!(home.join("admissions.disabled").is_file());
    let refused = http
        .post(format!("http://{address}/v1/chat/completions"))
        .json(&fixture["request"])
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    bridge.abort();
}
