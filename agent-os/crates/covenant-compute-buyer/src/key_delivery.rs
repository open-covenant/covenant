//! One-shot application-key delivery over the independently attested SSH lease.
//! Only public challenges, vendor evidence and signed ciphertext leave the guest.
use crate::{
    key_broker::{authorize_challenge, challenge_request_for_lease, KeyBrokerClient},
    BuyerError,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use covenant_compute_attestation::{binding::PhalaEphemeralIdentity, vendor_types::TdxEventEntry};
use covenant_compute_key_broker::{
    report_data, verify_signed_response, BrokerChallenge, KeyBinding, ReleaseRequest,
    SignedReleaseResponse, TdxKeyEvidence,
};
use covenant_compute_protocol::{phala, LeaseView, SignedJobEnvelope};
use covenant_identity::LocalIdentity;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

const MAX_FRAME: usize = 256 * 1024;
const MAX_HTTP: usize = 256 * 1024;
pub(crate) const SSH_BRIDGE: &str =
    include_str!("../../../../deploy/compute-key-broker/guest/ssh_bridge.py");

fn invalid(message: impl ToString) -> BuyerError {
    BuyerError::Coordinator(format!("application key delivery: {}", message.to_string()))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KeyDeliveryReceipt {
    pub protocol_version: u32,
    pub status: String,
    pub path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    protocol_version: u32,
    recipient_spki_der_base64: String,
    binding: KeyBinding,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuoteBundle {
    challenge: BrokerChallenge,
    evidence: GuestQuote,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GuestQuote {
    quote_base64: String,
    events: Vec<TdxEventEntry>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChallengeSession {
    session_id: Uuid,
    challenge: BrokerChallenge,
}

pub(crate) struct BrokerGateway {
    http: reqwest::Client,
    origin: String,
}
impl BrokerGateway {
    pub(crate) fn new(origin: &str) -> Result<Self, BuyerError> {
        Self::build(origin, false)
    }
    fn build(origin: &str, allow_test_loopback: bool) -> Result<Self, BuyerError> {
        let url = reqwest::Url::parse(origin).map_err(|_| invalid("invalid broker origin"))?;
        let loopback = matches!(
            url.host_str(),
            Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
        );
        if (url.scheme() != "https" && !(allow_test_loopback && loopback && url.scheme() == "http"))
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid("broker requires a bare HTTPS origin"));
        }
        let http = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(45))
            .build()
            .map_err(|_| invalid("broker HTTP client failed"))?;
        Ok(Self {
            http,
            origin: url.origin().ascii_serialization(),
        })
    }
    async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        value: &impl Serialize,
    ) -> Result<T, BuyerError> {
        let body = serde_json::to_vec(value).map_err(|_| invalid("invalid broker request"))?;
        if body.len() > MAX_HTTP {
            return Err(invalid("broker request exceeds limit"));
        }
        let mut response = self
            .http
            .post(format!("{}{path}", self.origin))
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| invalid("broker outcome unknown; do not automatically retry"))?;
        if !response.status().is_success() {
            return Err(invalid("broker request rejected; do not retry"));
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_HTTP as u64)
        {
            return Err(invalid("broker response exceeds limit"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| invalid("broker response incomplete; do not retry"))?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_HTTP {
                return Err(invalid("broker response exceeds limit"));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| invalid("invalid broker response"))
    }
}

pub(crate) struct DeliveryContext<'a> {
    pub buyer: &'a LocalIdentity,
    pub envelope: &'a SignedJobEnvelope,
    pub view: &'a LeaseView,
    pub identity: &'a PhalaEphemeralIdentity,
    pub coordinator: &'a KeyBrokerClient<'a>,
    pub gateway: &'a BrokerGateway,
    pub collateral_json: &'a str,
}

pub(crate) async fn exchange(
    context: &DeliveryContext<'_>,
    input: &mut (impl AsyncWrite + Unpin),
    output: &mut (impl AsyncBufRead + Unpin),
) -> Result<KeyDeliveryReceipt, BuyerError> {
    exchange_inner(context, input, output, verify_guest).await
}

async fn exchange_inner(
    context: &DeliveryContext<'_>,
    input: &mut (impl AsyncWrite + Unpin),
    output: &mut (impl AsyncBufRead + Unpin),
    verify: fn(
        &TdxKeyEvidence,
        &BrokerChallenge,
        &PhalaEphemeralIdentity,
    ) -> Result<(), BuyerError>,
) -> Result<KeyDeliveryReceipt, BuyerError> {
    let hello: Hello = read_frame(output).await?;
    let expected = challenge_request_for_lease(
        context.envelope,
        context.view,
        context.identity,
        &hello.recipient_spki_der_base64,
        &context.buyer.agent_id().pubkey_base58(),
        crate::epoch_ms(),
    )?;
    if hello.protocol_version != 1 || hello.binding != expected.binding {
        return Err(invalid(
            "recipient hello differs from original signed lease",
        ));
    }
    let spec = phala::parse_phala_ephemeral_lease_spec(&context.envelope.payload.input)
        .map_err(invalid)?
        .ok_or_else(|| invalid("ephemeral lease missing"))?;
    let key_policy = spec
        .application_key_recipient
        .as_ref()
        .ok_or_else(|| invalid("lease does not opt into application keys"))?;
    let session: ChallengeSession = context.gateway.post("/v1/challenge", &expected).await?;
    if session.session_id.is_nil() {
        return Err(invalid("invalid broker session"));
    }
    let challenge = session.challenge;
    let deadline = Duration::from_millis(challenge.ttl_ms.min(60_000));
    tokio::time::timeout(deadline, async {
        // Validate the exact broker/recipient/tenant/job before requesting any
        // quote. Obtain the coordinator's short-lived assertion only after the
        // guest evidence is ready, preserving its lifetime for the KMS calls.
        authorize_challenge(context.buyer, &expected, &challenge, crate::epoch_ms())?;
        write_frame(input, &challenge).await?;
        let quoted: QuoteBundle = read_frame(output).await?;
        if quoted.challenge != challenge {
            return Err(invalid("guest quoted another challenge"));
        }
        let evidence = TdxKeyEvidence {
            quote_base64: quoted.evidence.quote_base64,
            events: quoted.evidence.events,
            collateral_json: context.collateral_json.into(),
        };
        verify(&evidence, &challenge, context.identity)?;
        let (buyer_authorization, lease_assertion) = context
            .coordinator
            .request_lease_assertion(&expected, &challenge)
            .await?;
        let request = ReleaseRequest {
            challenge: challenge.clone(),
            buyer_authorization,
            lease_assertion,
            evidence,
        };
        let signed: SignedReleaseResponse = context
            .gateway
            .post(
                "/v1/release",
                &serde_json::json!({
                    "session_id": session.session_id, "request": request,
                }),
            )
            .await?;
        verify_signed_response(&key_policy.broker_response_pubkey_b58, &challenge, &signed)
            .map_err(invalid)?;
        write_frame(input, &signed).await?;
        let receipt: KeyDeliveryReceipt = read_frame(output).await?;
        if receipt.protocol_version != 1
            || receipt.status != "delivered"
            || receipt.path != "/workspace/.covenant-app-key"
        {
            return Err(invalid("invalid guest delivery receipt"));
        }
        input
            .shutdown()
            .await
            .map_err(|_| invalid("SSH input close failed"))?;
        Ok(receipt)
    })
    .await
    .map_err(|_| invalid("key release deadline exceeded; outcome may be unknown, do not retry"))?
}

fn verify_guest(
    evidence: &TdxKeyEvidence,
    challenge: &BrokerChallenge,
    expected: &PhalaEphemeralIdentity,
) -> Result<(), BuyerError> {
    if evidence.quote_base64.len() > 32768
        || evidence.collateral_json.len() > 128 * 1024
        || evidence.events.len() > 256
    {
        return Err(invalid("guest key evidence exceeds limit"));
    }
    let quote = STANDARD
        .decode(&evidence.quote_base64)
        .map_err(|_| invalid("invalid quote encoding"))?;
    let workload = decode_workload(&challenge.binding.workload_sha256)?;
    let verified =
        covenant_compute_attestation::key_broker::verify_phala_ephemeral_key_attestation(
            &quote,
            &evidence.collateral_json,
            &evidence.events,
            &report_data(challenge).map_err(invalid)?,
            &workload,
            crate::epoch_ms(),
        )
        .map_err(invalid)?;
    if &verified != expected {
        return Err(invalid("key quote belongs to another Phala guest"));
    }
    Ok(())
}

fn decode_workload(value: &str) -> Result<[u8; 32], BuyerError> {
    if value.len() != 64 {
        return Err(invalid("invalid workload digest"));
    }
    let mut bytes = [0u8; 32];
    for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let nibble = |b: u8| match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            _ => None,
        };
        bytes[index] = nibble(pair[0]).ok_or_else(|| invalid("invalid workload digest"))? * 16
            + nibble(pair[1]).ok_or_else(|| invalid("invalid workload digest"))?;
    }
    Ok(bytes)
}

async fn read_frame<T: DeserializeOwned>(
    reader: &mut (impl AsyncBufRead + Unpin),
) -> Result<T, BuyerError> {
    let mut bytes = Vec::new();
    loop {
        let chunk = reader
            .fill_buf()
            .await
            .map_err(|_| invalid("recipient output failed"))?;
        if chunk.is_empty() {
            return Err(invalid("recipient frame truncated"));
        }
        let length = chunk
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|n| n + 1)
            .unwrap_or(chunk.len());
        if bytes.len().saturating_add(length) > MAX_FRAME {
            return Err(invalid("recipient frame exceeds limit"));
        }
        let complete = chunk[length - 1] == b'\n';
        bytes.extend_from_slice(&chunk[..length]);
        reader.consume(length);
        if complete {
            break;
        }
    }
    serde_json::from_slice(&bytes).map_err(|_| invalid("invalid recipient frame"))
}

async fn write_frame(
    writer: &mut (impl AsyncWrite + Unpin),
    value: &impl Serialize,
) -> Result<(), BuyerError> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| invalid("invalid protocol message"))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_FRAME {
        return Err(invalid("protocol frame exceeds limit"));
    }
    writer
        .write_all(&bytes)
        .await
        .map_err(|_| invalid("recipient input failed"))?;
    writer
        .flush()
        .await
        .map_err(|_| invalid("recipient input flush failed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key_broker::tests::{native_fixture, signed_assertion};
    use axum::{routing::post, Json, Router};
    use covenant_compute_key_broker::{
        challenge_digest, release_response_bytes, verify_owner_authorization, ChallengeRequest,
        ReleaseResponse,
    };
    use serde_json::{json, Value};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::io::{AsyncReadExt, BufReader};

    #[tokio::test]
    async fn frames_reject_truncation_duplicates_and_excessive_input() {
        let valid = b"{\"protocol_version\":1,\"status\":\"delivered\",\"path\":\"/workspace/.covenant-app-key\"}\n";
        let receipt: KeyDeliveryReceipt = read_frame(&mut &valid[..]).await.unwrap();
        assert_eq!(receipt.status, "delivered");
        for bad in [
            valid[..valid.len() - 1].to_vec(),
            b"{\"protocol_version\":1,\"protocol_version\":1,\"status\":\"delivered\",\"path\":\"x\"}\n".to_vec(),
            vec![b' '; MAX_FRAME + 1],
        ] {
            assert!(read_frame::<KeyDeliveryReceipt>(&mut &bad[..]).await.is_err());
        }
    }

    #[test]
    fn production_gateway_requires_a_bare_authenticated_origin() {
        assert!(BrokerGateway::new("https://broker.example.com").is_ok());
        for origin in [
            "http://127.0.0.1:8080",
            "https://user@broker.example.com",
            "https://broker.example.com/api",
            "https://broker.example.com?key=x",
            "https://broker.example.com#fragment",
            "file:///tmp/broker",
        ] {
            assert!(BrokerGateway::new(origin).is_err(), "{origin}");
        }
        assert!(SSH_BRIDGE.contains("/key-broker-io/recipient.sock"));
        assert!(!SSH_BRIDGE.contains("dstack.sock"));
    }

    #[tokio::test]
    async fn synthetic_public_wire_round_trip_uses_real_buyer_and_broker_signatures() {
        exercise_public_exchange(false).await;
    }

    #[tokio::test]
    async fn synthetic_public_wire_rejects_host_forged_ciphertext_before_guest_delivery() {
        exercise_public_exchange(true).await;
    }

    // Explicitly synthetic: the TDX verifier and guest decryption are mocked.
    // The actual RSA/OAEP recipient tests live beside guest/recipient.py. This
    // test exercises real HTTP + bounded duplex framing and Ed25519 signatures.
    async fn exercise_public_exchange(tamper: bool) {
        let now = crate::epoch_ms();
        let fixture = native_fixture(now);
        let coordinator_id = LocalIdentity::generate("wire-test@coordinator");
        let assertion = signed_assertion(&fixture, &coordinator_id, now);
        let response = ReleaseResponse {
            protocol_version: 1,
            challenge_id: fixture.challenge.challenge_id,
            challenge_sha256: challenge_digest(&fixture.challenge).unwrap(),
            key_id: fixture.challenge.binding.key_id.clone(),
            key_version: fixture.challenge.binding.key_version,
            ciphertext_base64: STANDARD.encode([5u8; 384]),
            algorithm: "RSA_OAEP_SHA256".into(),
        };
        let mut signed = SignedReleaseResponse {
            signature_b58: bs58::encode(
                fixture
                    .broker
                    .sign(&release_response_bytes(&response).unwrap())
                    .to_bytes(),
            )
            .into_string(),
            broker_pubkey_b58: fixture.broker.agent_id().pubkey_base58(),
            response,
        };
        if tamper {
            signed.response.ciphertext_base64 = STANDARD.encode([6u8; 384]);
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let session_id = Uuid::new_v4();
        let expected = serde_json::to_value(&fixture.expected).unwrap();
        let challenge = fixture.challenge.clone();
        let challenge_calls = calls.clone();
        let assertion_calls = calls.clone();
        let release_calls = calls.clone();
        let release_challenge = challenge.clone();
        let expected_signed = serde_json::to_value(&signed).unwrap();
        let api = Router::new()
            .route(
                "/v1/challenge",
                post(move |Json(request): Json<Value>| {
                    let expected = expected.clone();
                    let challenge = challenge.clone();
                    let calls = challenge_calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(request, expected);
                        Json(json!({"session_id":session_id,"challenge":challenge}))
                    }
                }),
            )
            .route(
                &format!(
                    "/federation/key-broker/{}/lease-assertion",
                    fixture.envelope.payload.job_id
                ),
                post(move |Json(request): Json<Value>| {
                    let assertion = assertion.clone();
                    let calls = assertion_calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        assert!(request["buyer_authorization"]["signature_b58"].is_string());
                        Json(assertion)
                    }
                }),
            )
            .route(
                "/v1/release",
                post(move |Json(value): Json<Value>| {
                    let signed = signed.clone();
                    let challenge = release_challenge.clone();
                    let calls = release_calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(value["session_id"], session_id.to_string());
                        let request: ReleaseRequest =
                            serde_json::from_value(value["request"].clone()).unwrap();
                        assert_eq!(request.challenge, challenge);
                        verify_owner_authorization(&challenge, &request.buyer_authorization)
                            .unwrap();
                        assert_eq!(request.evidence.collateral_json, "MOCK Intel collateral");
                        Json(signed)
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, api).await.unwrap() });
        let coordinator = KeyBrokerClient::new(
            &origin,
            &fixture.buyer,
            &coordinator_id.agent_id().pubkey_base58(),
        )
        .unwrap();
        let gateway = BrokerGateway::build(&origin, true).unwrap();
        let context = DeliveryContext {
            buyer: &fixture.buyer,
            envelope: &fixture.envelope,
            view: &fixture.view,
            identity: &fixture.identity,
            coordinator: &coordinator,
            gateway: &gateway,
            collateral_json: "MOCK Intel collateral",
        };
        let (buyer_stream, guest_stream) = tokio::io::duplex(4096);
        let (buyer_output, mut buyer_input) = tokio::io::split(buyer_stream);
        let mut buyer_output = BufReader::new(buyer_output);
        let (guest_input, mut guest_output) = tokio::io::split(guest_stream);
        let mut guest_input = BufReader::new(guest_input);
        let hello = json!({"protocol_version":1,"binding":fixture.expected.binding,
            "recipient_spki_der_base64":fixture.expected.recipient_spki_der_base64});
        let challenge = fixture.challenge.clone();
        let buyer_exchange = async {
            let outcome = exchange_inner(
                &context,
                &mut buyer_input,
                &mut buyer_output,
                |evidence, _, _| {
                    assert_eq!(evidence.collateral_json, "MOCK Intel collateral");
                    Ok(())
                },
            )
            .await;
            // The real SSH process is killed on failure; emulate its closed pipe.
            buyer_input.shutdown().await.unwrap();
            outcome
        };
        let guest_exchange = async {
            write_frame(&mut guest_output, &hello).await.unwrap();
            let received: BrokerChallenge = read_frame(&mut guest_input).await.unwrap();
            assert_eq!(received, challenge);
            write_frame(
                &mut guest_output,
                &json!({"challenge":received,"evidence":{"quote_base64":"AA==","events":[]}}),
            )
            .await
            .unwrap();
            if tamper {
                let mut byte = [0u8; 1];
                assert_eq!(guest_input.read(&mut byte).await.unwrap(), 0);
            } else {
                let received: Value = read_frame(&mut guest_input).await.unwrap();
                assert_eq!(received, expected_signed);
                write_frame(&mut guest_output, &json!({"protocol_version":1,"status":"delivered","path":"/workspace/.covenant-app-key"})).await.unwrap();
                let mut byte = [0u8; 1];
                assert_eq!(guest_input.read(&mut byte).await.unwrap(), 0);
            }
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(buyer_exchange, guest_exchange)
        })
        .await
        .unwrap();
        server.abort();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        if tamper {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap().status, "delivered");
        }
    }

    #[tokio::test]
    async fn gateway_never_follows_redirects_or_retries_rejections() {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let api = Router::new()
            .route(
                "/v1/challenge",
                post(move || {
                    let count = count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        (
                            axum::http::StatusCode::TEMPORARY_REDIRECT,
                            [("location", "/must-not-follow")],
                            "redirect",
                        )
                    }
                }),
            )
            .route(
                "/must-not-follow",
                post(|| async {
                    panic!("redirect was followed");
                    #[allow(unreachable_code)]
                    ""
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, api).await.unwrap() });
        let gateway = BrokerGateway::build(&origin, true).unwrap();
        assert!(gateway
            .post::<Value>("/v1/challenge", &json!({}))
            .await
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[test]
    fn real_quote_verification_rejects_the_synthetic_wire_fixture() {
        let fixture = native_fixture(crate::epoch_ms());
        let evidence = TdxKeyEvidence {
            quote_base64: "AA==".into(),
            collateral_json: "MOCK Intel collateral".into(),
            events: vec![],
        };
        assert!(verify_guest(&evidence, &fixture.challenge, &fixture.identity).is_err());
        let _: ChallengeRequest = fixture.expected;
    }
}
