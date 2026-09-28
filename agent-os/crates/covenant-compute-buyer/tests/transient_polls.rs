//! Hermetic proof that a dispatch rides out the coordinator's deploy
//! window: stream and receipt polls that fail with transport-class
//! errors (5xx) keep polling to the buyer's deadline instead of
//! failing a job that is journal-durable server-side, while a real
//! refusal (bad signature, unknown job) still fails fast.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency, A2ATaskStatus};
use covenant_compute_buyer::{stream_and_verify, BuyerConfig, BuyerError};
use covenant_compute_protocol::{
    output_hash_hex, CapabilityRequirement, JobEnvelopePayload, JobKind, JobMeter,
    SignedJobEnvelope, SignedWorkReceipt, WorkReceiptPayload,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use sha2::{Digest, Sha256};
use uuid::Uuid;

fn envelope_and_receipt(
    output: &[Content],
) -> (LocalIdentity, SignedJobEnvelope, SignedWorkReceipt) {
    envelope_and_receipt_status(output, A2ATaskStatus::Ok)
}

fn envelope_and_receipt_status(
    output: &[Content],
    status: A2ATaskStatus,
) -> (LocalIdentity, SignedJobEnvelope, SignedWorkReceipt) {
    let buyer = LocalIdentity::generate("buyer@transient-test");
    let operator = LocalIdentity::generate("operator@transient-test");
    let job_id = Uuid::new_v4();
    let payload = JobEnvelopePayload {
        job_id,
        buyer: buyer.agent_id(),
        kind: JobKind::InferenceCall,
        capability_requirement: CapabilityRequirement {
            gpu_class: None,
            min_vram_gb: None,
            model_id: None,
            kind: JobKind::InferenceCall,
            max_duration_secs: 2,
            min_reputation_bps: None,
        },
        input: vec![Content::text("in")],
        price_micro_usdc: 1_000,
        deadline_ms: 1_500,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "transient-poll-test"),
        issued_at_ms: 1,
        referral_code: None,
        stream: true,
    };
    let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
    let job_hash_hex: String = {
        let digest = Sha256::digest(envelope.payload_json.as_bytes());
        digest.iter().map(|b| format!("{b:02x}")).collect()
    };
    let receipt = SignedWorkReceipt::sign(
        WorkReceiptPayload {
            job_id,
            operator: operator.agent_id(),
            job_hash_hex,
            result_hash_hex: output_hash_hex(output),
            meter: JobMeter {
                wall_ms: 5,
                tokens_in: None,
                tokens_out: None,
                gpu_seconds: None,
                finish_reason: None,
            },
            price_micro_usdc: 1_000,
            status,
            executed_at_ms: 2,
            node_audit_root_hex: "cc".repeat(32),
        },
        &operator,
    )
    .unwrap();
    (buyer, envelope, receipt)
}

/// A coordinator whose stream and receipt reads each refuse their
/// first `fail_first` hits with a 502 — the shape a reverse proxy
/// serves while the process behind it redeploys — then answer
/// normally.
async fn serve_flaky_coordinator(
    stream_fail_first: usize,
    receipt_fail_first: usize,
    stream_view: serde_json::Value,
    receipt_view: serde_json::Value,
) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let stream_hits = Arc::new(AtomicUsize::new(0));
    let receipt_hits = Arc::new(AtomicUsize::new(0));
    let router = Router::new()
        .route(
            "/federation/jobs/:id/stream",
            get(move || {
                let hits = stream_hits.clone();
                let view = stream_view.clone();
                async move {
                    if hits.fetch_add(1, Ordering::SeqCst) < stream_fail_first {
                        return (StatusCode::BAD_GATEWAY, "deploying").into_response();
                    }
                    Json(view).into_response()
                }
            }),
        )
        .route(
            "/federation/jobs/:id/receipt",
            get(move || {
                let hits = receipt_hits.clone();
                let view = receipt_view.clone();
                async move {
                    if hits.fetch_add(1, Ordering::SeqCst) < receipt_fail_first {
                        return (StatusCode::BAD_GATEWAY, "deploying").into_response();
                    }
                    Json(view).into_response()
                }
            }),
        );
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

fn config(coordinator_url: String) -> BuyerConfig {
    BuyerConfig {
        coordinator_url,
        poll_interval: Duration::from_millis(50),
        referral_code: None,
        rpc_url: None,
    }
}

fn stream_view(job_id: Uuid, chunks: &[(u64, &str)], status: &str) -> serde_json::Value {
    let chunk_rows: Vec<serde_json::Value> = chunks
        .iter()
        .map(|(seq, text)| serde_json::json!({ "seq": seq, "text": text }))
        .collect();
    serde_json::json!({
        "job_id": job_id,
        "status": status,
        "chunks": chunk_rows,
        "next_seq": chunks.len() as u64,
        "done": true,
        "truncated": false,
    })
}

fn receipt_view(receipt: &SignedWorkReceipt, output: &[Content]) -> serde_json::Value {
    serde_json::json!({
        "status": "completed",
        "receipt": receipt,
        "output": output,
        "payout": null,
    })
}

#[tokio::test]
async fn a_deploy_window_during_the_receipt_poll_does_not_fail_the_dispatch() {
    let output = vec![Content::text("helloworld")];
    let (buyer, envelope, receipt) = envelope_and_receipt(&output);
    let base_url = serve_flaky_coordinator(
        0,
        3,
        stream_view(envelope.payload.job_id, &[], "completed"),
        receipt_view(&receipt, &output),
    )
    .await;

    let outcome = stream_and_verify(
        &reqwest::Client::new(),
        &config(base_url),
        &buyer,
        envelope,
        |_| {},
    )
    .await
    .expect("a 502 mid-deploy is not a verdict on the job");
    assert_eq!(outcome.outcome.receipt.receipt.status, A2ATaskStatus::Ok);
    assert!(
        !outcome.stream_matched_output,
        "no chunk ever arrived; the flag grades the preview only"
    );
}

#[tokio::test]
async fn a_completed_job_carrying_a_non_ok_receipt_is_surfaced_as_failed_not_paid() {
    // A misbehaving coordinator reports a job completed but hands back a
    // receipt the operator signed as Error. Every signature and hash check
    // passes, so only the receipt's own status distinguishes it — the buyer
    // must refuse to book it as a paid success and surface the failure.
    let output = vec![Content::text("execution failed: the model crashed")];
    let (buyer, envelope, receipt) = envelope_and_receipt_status(&output, A2ATaskStatus::Error);
    let base_url = serve_flaky_coordinator(
        0,
        0,
        stream_view(envelope.payload.job_id, &[], "completed"),
        receipt_view(&receipt, &output),
    )
    .await;

    let err = stream_and_verify(
        &reqwest::Client::new(),
        &config(base_url),
        &buyer,
        envelope,
        |_| {},
    )
    .await
    .expect_err("a non-Ok receipt reported as completed must not settle as paid");
    match err {
        BuyerError::NotServed { status, detail, .. } => {
            assert_eq!(status, "failed");
            assert!(
                detail.is_some_and(|d| d.contains("model crashed")),
                "the operator's signed failure cause should reach the caller"
            );
        }
        other => panic!("expected NotServed, got: {other:?}"),
    }
}

#[tokio::test]
async fn a_deploy_window_during_the_stream_poll_degrades_the_feed_not_the_outcome() {
    let output = vec![Content::text("helloworld")];
    let (buyer, envelope, receipt) = envelope_and_receipt(&output);
    let base_url = serve_flaky_coordinator(
        3,
        0,
        stream_view(
            envelope.payload.job_id,
            &[(0, "hello"), (1, "world")],
            "completed",
        ),
        receipt_view(&receipt, &output),
    )
    .await;

    let mut fed = String::new();
    let outcome = stream_and_verify(
        &reqwest::Client::new(),
        &config(base_url),
        &buyer,
        envelope,
        |chunk| fed.push_str(chunk),
    )
    .await
    .expect("stream polls that fail must leave the receipt path intact");
    assert_eq!(outcome.outcome.receipt.receipt.status, A2ATaskStatus::Ok);
    assert_eq!(fed, "helloworld", "the feed resumed once the poll landed");
    assert!(outcome.stream_matched_output);
}

#[tokio::test]
async fn a_submit_retries_the_same_envelope_through_a_deploy_blip() {
    use axum::routing::post;

    // The upstream accepts the job; the front door the buyer dials
    // kills its first two connections mid-request — the deploy window.
    let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = upstream_listener.local_addr().unwrap();
    let router = Router::new().route("/federation/jobs", post(|| async { StatusCode::ACCEPTED }));
    tokio::spawn(async move { axum::serve(upstream_listener, router).await.unwrap() });

    let front = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let front_addr = front.local_addr().unwrap();
    tokio::spawn(async move {
        let mut dropped = 0usize;
        loop {
            let Ok((mut inbound, _)) = front.accept().await else {
                break;
            };
            if dropped < 2 {
                dropped += 1;
                continue; // dropping the socket resets the connection
            }
            tokio::spawn(async move {
                let Ok(mut outbound) = tokio::net::TcpStream::connect(upstream).await else {
                    return;
                };
                let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
            });
        }
    });

    let buyer = LocalIdentity::generate("buyer@submit-retry");
    let envelope = covenant_compute_buyer::submit_streaming(
        &reqwest::Client::new(),
        &config(format!("http://{front_addr}")),
        &buyer,
        Uuid::new_v4(),
        covenant_compute_buyer::JobRequest {
            kind: JobKind::InferenceCall,
            model: None,
            gpu_class: None,
            min_vram_gb: None,
            min_reputation_bps: None,
            input: vec![Content::text("in")],
            price_micro_usdc: 1_000,
            deadline_ms: 1_500,
        },
    )
    .await
    .expect("two reset connections are a deploy blip, not a refusal");
    assert!(envelope.payload.stream);
}

#[tokio::test]
async fn a_submit_refusal_is_a_verdict_and_is_never_retried() {
    use axum::routing::post;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let served = hits.clone();
    let router = Router::new().route(
        "/federation/jobs",
        post(move || {
            let hits = served.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (StatusCode::PAYMENT_REQUIRED, "deposit required").into_response()
            }
        }),
    );
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let buyer = LocalIdentity::generate("buyer@submit-refused");
    let err = covenant_compute_buyer::submit_streaming(
        &reqwest::Client::new(),
        &config(format!("http://{addr}")),
        &buyer,
        Uuid::new_v4(),
        covenant_compute_buyer::JobRequest {
            kind: JobKind::InferenceCall,
            model: None,
            gpu_class: None,
            min_vram_gb: None,
            min_reputation_bps: None,
            input: vec![Content::text("in")],
            price_micro_usdc: 1_000,
            deadline_ms: 1_500,
        },
    )
    .await
    .expect_err("402 is a verdict");
    // Structured, not stringly: a keyed caller reads the status to
    // tell a healable 402 from a permanent 400.
    match &err {
        BuyerError::SubmitRefused { status, body } => {
            assert_eq!(*status, 402);
            assert!(body.contains("deposit required"), "got: {body}");
        }
        other => panic!("expected SubmitRefused, got: {other}"),
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "a refused envelope must not be re-submitted"
    );
}

#[tokio::test]
async fn a_real_refusal_still_fails_fast() {
    let output = vec![Content::text("helloworld")];
    let (buyer, envelope, _) = envelope_and_receipt(&output);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = Router::new()
        .route(
            "/federation/jobs/:id/stream",
            get(move || async move {
                (StatusCode::UNAUTHORIZED, "read signature required").into_response()
            }),
        )
        .route(
            "/federation/jobs/:id/receipt",
            get(move || async move {
                (StatusCode::UNAUTHORIZED, "read signature required").into_response()
            }),
        );
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let started = std::time::Instant::now();
    let err = stream_and_verify(
        &reqwest::Client::new(),
        &config(format!("http://{addr}")),
        &buyer,
        envelope,
        |_| {},
    )
    .await
    .expect_err("a 401 is a real answer, not a deploy window");
    assert!(
        matches!(err, BuyerError::Coordinator(ref msg) if msg.contains("401")),
        "got: {err}"
    );
    // The first stream poll's 401 hands over to the receipt poll (a
    // refusal ends the feed; only the receipt loop's answer is
    // authoritative), whose own 401 is fatal — no deadline burn.
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_coordinator_refusal_names_the_action_not_the_endpoint_url() {
    use axum::routing::post;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = Router::new().route(
        "/federation/jobs/:id/cancel",
        post(|| async { (StatusCode::NOT_FOUND, "no such job").into_response() }),
    );
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let buyer = LocalIdentity::generate("buyer@refusal-render");
    let err = covenant_compute_buyer::cancel_job(
        &reqwest::Client::new(),
        &config(format!("http://{addr}")),
        &buyer,
        Uuid::new_v4(),
    )
    .await
    .expect_err("a 404 is a refusal, not a transport blip");
    let BuyerError::Coordinator(msg) = &err else {
        panic!("expected Coordinator, got: {err}");
    };
    // The refusal reads as the action plus the coordinator's own reason,
    // never the internal endpoint path or the coordinator's base URL.
    assert!(msg.starts_with("cancel returned"), "got: {msg}");
    assert!(msg.contains("no such job"), "got: {msg}");
    assert!(
        !msg.contains("/federation/") && !msg.contains("http://"),
        "error leaked the coordinator URL: {msg}"
    );
}
