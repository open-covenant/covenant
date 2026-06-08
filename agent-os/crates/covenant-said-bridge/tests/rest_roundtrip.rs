//! End-to-end coverage of the REST surface (off-chain `lookup` and the
//! cross-chain inbox/free-tier/send calls) against a one-shot in-process
//! HTTP server. Exercises the status-code branches in `rest::decode`
//! (success, 402, other errors, undecodable body) and confirms each call
//! reaches the right method/path and maps the JSON envelope onto its typed
//! result — without a mock framework or touching the network.

use covenant_said_bridge::config::Cluster;
use covenant_said_bridge::xchain::SendRequest;
use covenant_said_bridge::{BridgeError, Config, SaidBridge};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

struct Request {
    method: String,
    path: String,
    body: String,
}

/// One-shot HTTP server: binds an ephemeral loopback port, accepts a single
/// connection, replies with `status` (a full status line like "404 Not
/// Found") and `body`, then hands back the parsed request so the test can
/// assert what the bridge actually sent. Returns the base URL to point the
/// bridge at and the join handle carrying the captured request.
async fn serve_once(status: &'static str, body: &'static str) -> (String, JoinHandle<Request>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let request = read_request(&mut stream).await;
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.expect("write");
        stream.flush().await.expect("flush");
        request
    });
    (format!("http://{addr}"), handle)
}

async fn read_request(stream: &mut TcpStream) -> Request {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    let header_end = loop {
        let n = stream.read(&mut chunk).await.expect("read headers");
        if n == 0 {
            break buf.len();
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut request_line = head.lines().next().unwrap_or_default().split_whitespace();
    let method = request_line.next().unwrap_or_default().to_owned();
    let path = request_line.next().unwrap_or_default().to_owned();
    let content_length = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
        })
        .unwrap_or(0);
    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk).await.expect("read body");
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Request {
        method,
        path,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

fn enabled_bridge(api_base_url: String) -> SaidBridge {
    let mut config = Config::disabled(Cluster::Devnet);
    config.enabled = true;
    config.api_base_url = api_base_url;
    SaidBridge::new(config).expect("bridge")
}

fn sample_send() -> SendRequest {
    SendRequest {
        source_chain: "solana".into(),
        source_address: "Addr1".into(),
        target_chain: "base".into(),
        target_address: "0xabc".into(),
        payload: serde_json::json!({ "ping": 1 }),
    }
}

#[tokio::test]
async fn lookup_maps_success_envelope() {
    let (base, server) = serve_once(
        "200 OK",
        r#"{"wallet":"Wallet111","owner":"Owner222","name":"scout","isVerified":true,"reputationScore":4.5,"feedbackCount":12,"activityCount":99,"metadataUri":"https://example.test/a.json","registeredAt":"2026-01-01T00:00:00Z"}"#,
    )
    .await;
    let agent = enabled_bridge(base)
        .lookup("Wallet111")
        .await
        .expect("lookup");
    let req = server.await.expect("server");

    assert_eq!(req.method, "GET");
    assert_eq!(req.path, "/api/agents/Wallet111");
    assert_eq!(agent.wallet, "Wallet111");
    assert_eq!(agent.owner.as_deref(), Some("Owner222"));
    assert!(agent.is_verified);
    assert!((agent.reputation_score - 4.5).abs() < 1e-9);
    assert_eq!(agent.feedback_count, 12);
    assert_eq!(agent.activity_count, 99);
    assert_eq!(
        agent.metadata_uri.as_deref(),
        Some("https://example.test/a.json")
    );
}

#[tokio::test]
async fn lookup_surfaces_http_error_with_body() {
    let (base, server) = serve_once("404 Not Found", r#"{"error":"agent not found"}"#).await;
    let err = enabled_bridge(base)
        .lookup("Missing")
        .await
        .expect_err("should 404");
    server.await.expect("server");

    match err {
        BridgeError::Http { status, body } => {
            assert_eq!(status, 404);
            assert!(body.contains("agent not found"), "body: {body}");
        }
        other => panic!("expected Http, got {other:?}"),
    }
}

#[tokio::test]
async fn lookup_invalid_json_surfaces_decode() {
    let (base, server) = serve_once("200 OK", "not json").await;
    let err = enabled_bridge(base)
        .lookup("Wallet111")
        .await
        .expect_err("should fail to decode");
    server.await.expect("server");

    assert!(
        matches!(err, BridgeError::Decode(_)),
        "expected Decode, got {err:?}"
    );
}

#[tokio::test]
async fn xchain_inbox_maps_messages() {
    let (base, server) = serve_once(
        "200 OK",
        r#"{"chain":"solana","address":"Addr1","messages":[{"id":"m1","sourceChain":"base","sourceAddress":"0xabc","targetChain":"solana","targetAddress":"Addr1","payload":{"hello":"world"},"createdAt":1700000000}]}"#,
    )
    .await;
    let inbox = enabled_bridge(base)
        .xchain_inbox("solana", "Addr1")
        .await
        .expect("inbox");
    let req = server.await.expect("server");

    assert_eq!(req.method, "GET");
    assert_eq!(req.path, "/xchain/inbox/solana/Addr1");
    assert_eq!(inbox.messages.len(), 1);
    let msg = &inbox.messages[0];
    assert_eq!(msg.id, "m1");
    assert_eq!(msg.source_chain, "base");
    assert_eq!(msg.target_address, "Addr1");
    assert_eq!(msg.created_at, Some(1700000000));
}

#[tokio::test]
async fn xchain_free_tier_maps_status() {
    let (base, server) = serve_once(
        "200 OK",
        r#"{"address":"Addr1","used":3,"remaining":7,"limit":10,"paidPrice":"0.01","paymentChains":[{"name":"base","network":"mainnet"}]}"#,
    )
    .await;
    let status = enabled_bridge(base)
        .xchain_free_tier("Addr1")
        .await
        .expect("free tier");
    let req = server.await.expect("server");

    assert_eq!(req.method, "GET");
    assert_eq!(req.path, "/xchain/free-tier/Addr1");
    assert_eq!(status.used, 3);
    assert_eq!(status.remaining, 7);
    assert_eq!(status.limit, 10);
    assert_eq!(status.paid_price.as_deref(), Some("0.01"));
    assert_eq!(status.payment_chains.len(), 1);
    assert_eq!(status.payment_chains[0].name, "base");
}

#[tokio::test]
async fn xchain_send_posts_camel_case_and_maps_receipt() {
    let (base, server) = serve_once(
        "200 OK",
        r#"{"messageId":"msg-1","freeTierRemaining":9,"deliveredAt":1700000001}"#,
    )
    .await;
    let receipt = enabled_bridge(base)
        .xchain_send(&sample_send())
        .await
        .expect("send");
    let req = server.await.expect("server");

    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/xchain/message");
    assert!(
        req.body.contains(r#""sourceChain":"solana""#),
        "body: {}",
        req.body
    );
    assert!(
        req.body.contains(r#""targetAddress":"0xabc""#),
        "body: {}",
        req.body
    );
    assert_eq!(receipt.message_id, "msg-1");
    assert_eq!(receipt.free_tier_remaining, Some(9));
    assert_eq!(receipt.delivered_at, Some(1700000001));
}

#[tokio::test]
async fn xchain_send_payment_required_surfaces_402() {
    // Free tier is 10/day; past that SAID answers the POST with 402 so the
    // caller can settle via x402 instead of silently dropping the message.
    let (base, server) =
        serve_once("402 Payment Required", r#"{"error":"free tier exhausted"}"#).await;
    let err = enabled_bridge(base)
        .xchain_send(&sample_send())
        .await
        .expect_err("should be 402");
    server.await.expect("server");

    match err {
        BridgeError::Http { status, body } => {
            assert_eq!(status, 402);
            assert!(body.contains("free tier exhausted"), "body: {body}");
        }
        other => panic!("expected Http 402, got {other:?}"),
    }
}

#[tokio::test]
async fn rest_calls_require_enabled_before_network() {
    // The bridge ships disabled; a REST call on a disabled bridge must fail
    // closed with Disabled before any socket is opened.
    let bridge = SaidBridge::new(Config::disabled(Cluster::Devnet)).expect("bridge");

    let err = bridge.lookup("Wallet111").await.expect_err("disabled");
    assert!(
        matches!(err, BridgeError::Disabled),
        "expected Disabled, got {err:?}"
    );

    let err = bridge
        .xchain_send(&sample_send())
        .await
        .expect_err("disabled");
    assert!(
        matches!(err, BridgeError::Disabled),
        "expected Disabled, got {err:?}"
    );
}
