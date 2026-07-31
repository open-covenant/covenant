//! The Circuit x Covenant integration shape, end to end, offline.
//!
//! One agent uses BOTH Circuit inference and Circuit data under a single capability and
//! spend ledger, against a local mock of the Circuit endpoints, settled by `MockCircPayer`
//! (no network, no CIRC). It also shows a per-call cap refusing an over-priced call before
//! any payment. This is the "see the shape" demo — run it with zero setup:
//!
//!   cargo run -p covenant-circuit --example demo_shape

use std::sync::Arc;

use covenant_circuit::{
    ChatMessage, ChatParams, CircuitCapability, DataClient, Inference, MockCircPayer, SpendLedger,
};
use serde_json::json;
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TREASURY: &str = "CircU1treasury1111111111111111111111111111";
const CIRC: &str = "8fQgfsRnRkKSeNUhevT7wp8mhNvMSJdLn1fJi4oVpump";

#[tokio::main]
async fn main() {
    let server = MockServer::start().await;

    // Inference: 402 with a CIRC price, then 200 once the payment signature is present.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(402).set_body_json(json!({
            "payment": { "recipient": TREASURY, "amountRaw": "300000", "amountDisplay": "0.30 CIRC" }
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header_exists("X-Payment-Signature"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "role": "assistant", "content": "Solana is a high-throughput L1 with sub-second finality." } }],
            "usage": { "prompt_tokens": 12, "completion_tokens": 10, "total_tokens": 22 }
        })))
        .mount(&server)
        .await;

    // Data token-price: 402 then 200.
    Mock::given(method("GET"))
        .and(path("/api/token-price"))
        .respond_with(ResponseTemplate::new(402).set_body_json(json!({
            "payment": { "recipient": TREASURY, "amountRaw": "50000" }
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/token-price"))
        .and(header_exists("X-Payment-Signature"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "mint": CIRC, "priceUsd": 0.0001 })),
        )
        .mount(&server)
        .await;

    let host = reqwest::Url::parse(&server.uri())
        .unwrap()
        .host_str()
        .unwrap()
        .to_string();

    // One capability + one ledger bound every call the agent makes.
    let cap = CircuitCapability::new()
        .per_call(1_000_000) // <= 1.00 CIRC per call
        .budget(5_000_000) // <= 5.00 CIRC total
        .allow_recipient(TREASURY) // only Circuit's treasury may be paid
        .allow_host(host);
    let ledger = Arc::new(SpendLedger::new());
    let payer = Arc::new(MockCircPayer::new());
    let inference = Inference::new(payer.clone(), cap.clone(), ledger.clone())
        .with_base_url(format!("{}/v1", server.uri()));
    let data =
        DataClient::new(payer.clone(), cap.clone(), ledger.clone()).with_base_url(server.uri());

    println!("== Circuit x Covenant — integration shape (offline, mock settlement) ==\n");

    // 1. The agent thinks: paid inference.
    let chat = inference
        .chat(ChatParams::new(vec![ChatMessage::user(
            "What is Solana in one line?",
        )]))
        .await
        .expect("inference");
    println!("inference -> {}", chat.content);
    println!(
        "  paid {} raw CIRC | tx {}\n",
        chat.paid_raw.unwrap_or(0),
        chat.payment_tx.as_deref().unwrap_or("-")
    );

    // 2. The agent senses: paid data.
    let price = data.token_price(CIRC).await.expect("token price");
    println!("data.token_price -> {price}");
    println!(
        "  ledger: {} raw CIRC over {} settled calls\n",
        ledger.spent_raw(),
        payer.payments().len()
    );

    // 3. Capability enforcement: a call priced over the per-call cap is refused before any
    //    CIRC moves.
    Mock::given(method("GET"))
        .and(path("/api/scan"))
        .respond_with(ResponseTemplate::new(402).set_body_json(json!({
            "payment": { "recipient": TREASURY, "amountRaw": "9000000" } // 9 CIRC, over the 1.00 cap
        })))
        .mount(&server)
        .await;
    match data.scan(CIRC).await {
        Err(e) => println!("capability refused an over-cap call: {e}"),
        Ok(_) => println!("(unexpected: over-cap call was allowed)"),
    }

    println!(
        "\ntotal settled this run: {} raw CIRC (mock payer — no chain, no money)",
        payer.total_paid()
    );
}
