use std::sync::Arc;

use covenant_circuit::{
    circuit_tools, ChatMessage, ChatParams, CircuitCapability, CircuitConfig, DataClient,
    Inference, MockCircPayer, SpendLedger,
};
use covenant_mcp::Content;
use serde_json::json;
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TREASURY: &str = "CircU1treasury1111111111111111111111111111";

fn host_of(uri: &str) -> String {
    reqwest::Url::parse(uri)
        .unwrap()
        .host_str()
        .unwrap()
        .to_string()
}

fn cap_allowing(server: &MockServer) -> CircuitCapability {
    CircuitCapability::new()
        .allow_recipient(TREASURY)
        .allow_host(host_of(&server.uri()))
}

/// A server that charges 300000 raw CIRC for one chat completion, then serves it.
async fn paid_inference_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(402).set_body_json(json!({
            "payment": { "recipient": TREASURY, "amountRaw": "300000" }
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header_exists("X-Payment-Signature"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "content": "hi" } }],
            "usage": { "total_tokens": 3 }
        })))
        .mount(&server)
        .await;
    server
}

fn inference(
    server: &MockServer,
    cap: CircuitCapability,
    payer: Arc<MockCircPayer>,
    ledger: Arc<SpendLedger>,
) -> Inference {
    Inference::new(payer, cap, ledger).with_base_url(format!("{}/v1", server.uri()))
}

#[tokio::test]
async fn pays_on_402_and_returns_content() {
    let server = paid_inference_server().await;
    let payer = Arc::new(MockCircPayer::new());
    let ledger = Arc::new(SpendLedger::new());
    let inf = inference(
        &server,
        cap_allowing(&server),
        payer.clone(),
        ledger.clone(),
    );

    let r = inf
        .chat(ChatParams::new(vec![ChatMessage::user("hi")]))
        .await
        .unwrap();
    assert_eq!(r.content, "hi");
    assert_eq!(r.paid_raw, Some(300_000));
    assert!(r.payment_tx.is_some());
    assert_eq!(payer.total_paid(), 300_000);
    assert_eq!(ledger.spent_raw(), 300_000);
}

#[tokio::test]
async fn paid_retry_does_not_follow_redirect_or_leak_payment_header() {
    let sink = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/capture"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "content": "redirected" } }]
        })))
        .mount(&sink)
        .await;

    let source = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(402).set_body_json(json!({
            "payment": { "recipient": TREASURY, "amountRaw": "300000" }
        })))
        .up_to_n_times(1)
        .mount(&source)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header_exists("X-Payment-Signature"))
        .respond_with(
            ResponseTemplate::new(307).insert_header("location", format!("{}/capture", sink.uri())),
        )
        .mount(&source)
        .await;

    let payer = Arc::new(MockCircPayer::new());
    let inf = inference(
        &source,
        cap_allowing(&source),
        payer.clone(),
        Arc::new(SpendLedger::new()),
    );
    let err = inf
        .chat(ChatParams::new(vec![ChatMessage::user("hi")]))
        .await
        .expect_err("redirect response must be surfaced");

    assert!(err.to_string().contains("unexpected status 307"), "{err}");
    assert_eq!(payer.total_paid(), 300_000, "one transfer settles");
    assert!(
        sink.received_requests().await.unwrap().is_empty(),
        "the paid retry and X-Payment-Signature must not follow the redirect"
    );
}

#[tokio::test]
async fn post_payment_server_error_gets_one_paid_attempt_without_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(402).set_body_json(json!({
            "payment": { "recipient": TREASURY, "amountRaw": "300000" }
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header_exists("X-Payment-Signature"))
        .respond_with(ResponseTemplate::new(503).set_body_string("try later"))
        .mount(&server)
        .await;

    let payer = Arc::new(MockCircPayer::new());
    let inf = inference(
        &server,
        cap_allowing(&server),
        payer.clone(),
        Arc::new(SpendLedger::new()),
    );
    let err = inf
        .chat(ChatParams::new(vec![ChatMessage::user("hi")]))
        .await
        .expect_err("503 must be surfaced without retry");

    assert!(err.to_string().contains("unexpected status 503"), "{err}");
    assert_eq!(payer.total_paid(), 300_000, "one transfer settles");
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        2,
        "one free challenge plus exactly one paid request; a retry needs an explicit idempotency contract"
    );
}

#[tokio::test]
async fn per_call_cap_refuses_before_paying() {
    let server = paid_inference_server().await;
    let payer = Arc::new(MockCircPayer::new());
    let ledger = Arc::new(SpendLedger::new());
    let cap = cap_allowing(&server).per_call(100_000); // 402 asks 300000 > cap
    let inf = inference(&server, cap, payer.clone(), ledger.clone());

    let err = inf
        .chat(ChatParams::new(vec![ChatMessage::user("hi")]))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("per-call cap"), "{err}");
    assert_eq!(payer.total_paid(), 0, "no CIRC should move");
    assert_eq!(ledger.spent_raw(), 0);
}

#[tokio::test]
async fn recipient_pin_refuses_foreign_treasury() {
    let server = paid_inference_server().await;
    let payer = Arc::new(MockCircPayer::new());
    let cap = CircuitCapability::new()
        .allow_host(host_of(&server.uri()))
        .allow_recipient("SomeOtherTreasury11111111111111111111111111");
    let inf = inference(&server, cap, payer.clone(), Arc::new(SpendLedger::new()));

    let err = inf
        .chat(ChatParams::new(vec![ChatMessage::user("hi")]))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("allowed treasury"), "{err}");
    assert_eq!(payer.total_paid(), 0);
}

#[tokio::test]
async fn host_allowlist_refuses_unlisted_host() {
    let server = paid_inference_server().await;
    let payer = Arc::new(MockCircPayer::new());
    let cap = CircuitCapability::new()
        .allow_host("not-the-mock-host.example")
        .allow_recipient(TREASURY);
    let inf = inference(&server, cap, payer.clone(), Arc::new(SpendLedger::new()));

    let err = inf
        .chat(ChatParams::new(vec![ChatMessage::user("hi")]))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("allowed set"), "{err}");
    assert_eq!(payer.total_paid(), 0);
}

#[tokio::test]
async fn budget_refuses_second_call_over_total() {
    // Each call re-challenges with a 402; a with-signature retry gets 200 (higher priority).
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header_exists("X-Payment-Signature"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "choices": [{ "message": { "content": "ok" } }] })),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(402).set_body_json(json!({
            "payment": { "recipient": TREASURY, "amountRaw": "300000" }
        })))
        .with_priority(5)
        .mount(&server)
        .await;

    let payer = Arc::new(MockCircPayer::new());
    let ledger = Arc::new(SpendLedger::new());
    let cap = cap_allowing(&server).budget(500_000); // 2 x 300000 = 600000 > budget
    let inf = inference(&server, cap, payer.clone(), ledger.clone());

    inf.chat(ChatParams::new(vec![ChatMessage::user("1")]))
        .await
        .unwrap();
    let err = inf
        .chat(ChatParams::new(vec![ChatMessage::user("2")]))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("budget"), "{err}");
    assert_eq!(payer.total_paid(), 300_000, "only the first call settles");
    assert_eq!(ledger.spent_raw(), 300_000);
}

#[tokio::test]
async fn free_endpoint_returns_without_payment() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .mount(&server)
        .await;
    let payer = Arc::new(MockCircPayer::new());
    let data = DataClient::new(
        payer.clone(),
        cap_allowing(&server),
        Arc::new(SpendLedger::new()),
    )
    .with_base_url(server.uri());

    let v = data.status().await.unwrap();
    assert_eq!(v.get("ok").and_then(|b| b.as_bool()), Some(true));
    assert_eq!(payer.total_paid(), 0, "free endpoint must not pay");
}

#[tokio::test]
async fn inference_tool_returns_content_and_circuit_block() {
    let server = paid_inference_server().await;
    let inf = Arc::new(inference(
        &server,
        cap_allowing(&server),
        Arc::new(MockCircPayer::new()),
        Arc::new(SpendLedger::new()),
    ));
    let data = Arc::new(
        DataClient::new(
            Arc::new(MockCircPayer::new()),
            cap_allowing(&server),
            Arc::new(SpendLedger::new()),
        )
        .with_base_url(server.uri()),
    );
    let cfg = CircuitConfig {
        enabled: true,
        ..Default::default()
    };
    let tools = circuit_tools(inf, data, &cfg);
    let tool = tools
        .iter()
        .find(|t| t.name() == "circuit.inference")
        .expect("inference tool registered");

    let res = tool.call(json!({ "prompt": "hi" })).await.unwrap();
    assert!(!res.is_error);
    assert_eq!(res.content.len(), 2);
    match &res.content[0] {
        Content::Json { value } => {
            assert_eq!(value.get("content").and_then(|v| v.as_str()), Some("hi"));
        }
        other => panic!("expected content json, got {other:?}"),
    }
    match &res.content[1] {
        Content::Json { value } => {
            let c = value.get("circuit").expect("circuit provenance block");
            assert_eq!(
                c.get("token").and_then(|v| v.as_str()),
                Some(covenant_circuit::circ::MINT)
            );
            assert_eq!(c.get("spentRaw").and_then(|v| v.as_u64()), Some(300_000));
        }
        other => panic!("expected circuit block, got {other:?}"),
    }
}

#[tokio::test]
async fn disabled_config_registers_no_tools() {
    let inf = Arc::new(Inference::new(
        Arc::new(MockCircPayer::new()),
        CircuitCapability::new(),
        Arc::new(SpendLedger::new()),
    ));
    let data = Arc::new(DataClient::new(
        Arc::new(MockCircPayer::new()),
        CircuitCapability::new(),
        Arc::new(SpendLedger::new()),
    ));
    assert!(circuit_tools(inf, data, &CircuitConfig::default()).is_empty());
}
