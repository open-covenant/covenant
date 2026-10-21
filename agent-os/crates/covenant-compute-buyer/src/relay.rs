//! Native, single-stream outbound transport. Every request is buyer-signed;
//! routing metadata is never sufficient to authorize an SSH connection.
use crate::{hardware::HardwareClient, BuyerError};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use covenant_compute_attestation::binding::{HardwareProfile, HardwareScope, HardwareVerdict};
use covenant_compute_protocol::relay::{
    relay_metadata_path, RelayMetadata, MAX_RELAY_CHUNK_BYTES, MAX_RELAY_STREAM_BYTES, RELAY_PATH,
    RELAY_VERSION,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{io::ErrorKind, sync::Mutex, time::Duration};
use tokio::{io::AsyncWriteExt, net::TcpListener};
use uuid::Uuid;

const CHUNK: usize = MAX_RELAY_CHUNK_BYTES;
const STREAM_BYTES: u64 = MAX_RELAY_STREAM_BYTES as u64;
/// Leave bounded space for SSH framing, authentication, and command output.
pub const MAX_TRANSFER_BYTES: u64 = 8 * 1024 * 1024;
const HTTP_TIMEOUT: Duration = Duration::from_secs(12);

fn invalid(message: impl ToString) -> BuyerError {
    BuyerError::Coordinator(message.to_string())
}
fn hex_identity(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn check_metadata(
    metadata: &RelayMetadata,
    scope: &HardwareScope,
    verdict: &HardwareVerdict,
    origin: &str,
) -> Result<(), BuyerError> {
    let identity = verdict
        .phala_identity
        .as_ref()
        .ok_or_else(|| invalid("verified Phala application identity missing"))?;
    if scope.profile != HardwareProfile::PhalaEphemeralDstack061
        || verdict.challenge.scope != *scope
        || metadata.schema_version != 1
        || metadata.job_id != scope.job_id
        || !hex_identity(&metadata.app_id)
        || metadata
            .instance_id
            .as_deref()
            .is_some_and(|id| !hex_identity(id) || id != identity.instance_id)
        || metadata.app_id != identity.app_id
        || metadata.relay_endpoint != format!("https://{}", scope.host)
        || metadata.relay_endpoint != origin
        || metadata.expires_at_ms != scope.lease_expires_at_ms
        || crate::epoch_ms() >= metadata.expires_at_ms
    {
        return Err(invalid("relay metadata differs from original provision, verified identity, or buyer-signed lease"));
    }
    Ok(())
}

/// The guest permits opening only while its original quote challenge is live.
/// A saved verdict's longer lifetime cannot extend that opening authorization.
pub(crate) fn require_opening_grant(verdict: &HardwareVerdict, now: u64) -> Result<(), BuyerError> {
    if now < verdict.challenge.issued_at_ms || now >= verdict.challenge.expires_at_ms {
        return Err(invalid("ephemeral quote opening grant expired; obtain fresh evidence for the same lease, never automatically repurchase"));
    }
    Ok(())
}

/// Must be called only after the verdict has been independently re-verified.
/// It intentionally precedes snapshot download/decryption in workspace restore.
pub(crate) async fn validate_metadata(
    hardware: &HardwareClient<'_>,
    scope: &HardwareScope,
    verdict: &HardwareVerdict,
) -> Result<(), BuyerError> {
    let metadata: RelayMetadata = hardware
        .request(
            reqwest::Method::GET,
            &relay_metadata_path(scope.job_id),
            Vec::new(),
        )
        .await?;
    require_opening_grant(verdict, crate::epoch_ms())?;
    check_metadata(&metadata, scope, verdict, hardware.origin())
}

#[derive(Serialize)]
struct Request<'a, T> {
    version: u8,
    session_id: Uuid,
    operation: &'a str,
    arguments: &'a T,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response<T> {
    version: u8,
    result: T,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstanceAck {
    instance_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestAck {
    request_id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlResult {
    request_id: Uuid,
    result: Option<ControlValue>,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum ControlValue {
    Success(ControlSuccess),
    Failure(ControlFailure),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlSuccess {
    ok: bool,
    value: StreamAck,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlFailure {
    ok: bool,
    error: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StreamAck {
    stream_id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Closed {
    closed: bool,
}

pub(crate) struct RelaySession<'a, 'i> {
    hardware: &'a HardwareClient<'i>,
    job_id: Uuid,
    challenge_id: Uuid,
    challenge_expires_at_ms: u64,
    expires_at_ms: u64,
    lease_expires_at_ms: u64,
    cleanup: CancellationClose,
}
impl<'a, 'i> RelaySession<'a, 'i> {
    pub(crate) async fn authorize(
        hardware: &'a HardwareClient<'i>,
        scope: &HardwareScope,
        verdict: &HardwareVerdict,
    ) -> Result<Self, BuyerError> {
        validate_metadata(hardware, scope, verdict).await?;
        let identity = verdict
            .phala_identity
            .as_ref()
            .ok_or_else(|| invalid("verified Phala identity missing"))?;
        let session = Self {
            hardware,
            job_id: scope.job_id,
            challenge_id: verdict.challenge.challenge_id,
            challenge_expires_at_ms: verdict.challenge.expires_at_ms,
            expires_at_ms: verdict.expires_at_ms.min(scope.lease_expires_at_ms),
            lease_expires_at_ms: scope.lease_expires_at_ms,
            cleanup: CancellationClose::default(),
        };
        let ack: InstanceAck = session
            .request(
                "buyer_pin_instance",
                &serde_json::json!({"instance_id": identity.instance_id}),
            )
            .await?;
        if ack.instance_id != identity.instance_id {
            return Err(invalid("relay changed verified instance pin"));
        }
        Ok(session)
    }
    async fn request<T: DeserializeOwned>(
        &self,
        operation: &str,
        arguments: &impl Serialize,
    ) -> Result<T, BuyerError> {
        let cleanup_active = self.cleanup.0.lock().map_err(invalid)?.is_some();
        if cleanup_active {
            self.refresh_cleanup()?;
        }
        let now = crate::epoch_ms();
        let expires = if operation == "buyer_close_stream" {
            self.lease_expires_at_ms
        } else {
            self.expires_at_ms
        };
        if now >= expires {
            return Err(invalid("verified relay session expired"));
        }
        let body = serde_json::to_vec(&Request {
            version: RELAY_VERSION,
            session_id: self.job_id,
            operation,
            arguments,
        })
        .map_err(invalid)?;
        // One attempt. A lost acknowledgement is terminal; no new operation ID,
        // stream, guest, or purchase is substituted for an uncertain outcome.
        let response: Response<T> = tokio::time::timeout(
            HTTP_TIMEOUT.min(Duration::from_millis(expires - now)),
            self.hardware
                .request(reqwest::Method::POST, RELAY_PATH, body),
        )
        .await
        .map_err(|_| invalid("relay outcome unknown; do not automatically resume or reopen"))??;
        if response.version != RELAY_VERSION {
            return Err(invalid("unsupported relay response version"));
        }
        Ok(response.result)
    }
    async fn open(&self, stream_id: Uuid) -> Result<(), BuyerError> {
        if crate::epoch_ms() >= self.challenge_expires_at_ms {
            return Err(invalid("ephemeral quote opening grant expired"));
        }
        let request_id = Uuid::new_v4();
        let ack: RequestAck = self
            .request(
                "buyer_submit",
                &serde_json::json!({
                    "request_id": request_id, "kind": "open_stream",
                    "payload": {"stream_id": stream_id, "challenge_id": self.challenge_id}
                }),
            )
            .await?;
        if ack.request_id != request_id {
            return Err(invalid("relay changed stream request ID"));
        }
        loop {
            if crate::epoch_ms() >= self.challenge_expires_at_ms {
                return Err(invalid("ephemeral quote opening grant expired"));
            }
            let result: ControlResult = self
                .request(
                    "buyer_result",
                    &serde_json::json!({"request_id":request_id}),
                )
                .await?;
            if result.request_id != request_id {
                return Err(invalid("relay changed control result ID"));
            }
            match result.result {
                Some(ControlValue::Success(result))
                    if result.ok && result.value.stream_id == stream_id =>
                {
                    return Ok(())
                }
                Some(ControlValue::Failure(result)) if !result.ok => {
                    let _ = result.error; // Untrusted guest diagnostics are not commands or state.
                    return Err(invalid(
                        "guest refused relay stream; do not automatically reopen",
                    ));
                }
                Some(_) => return Err(invalid("malformed relay control result")),
                None => tokio::time::sleep(Duration::from_millis(25)).await,
            }
        }
    }
    pub(crate) async fn close(&self) -> Result<(), BuyerError> {
        let ack: Closed = self
            .request("buyer_close_stream", &serde_json::json!({}))
            .await?;
        if !ack.closed {
            return Err(invalid("relay did not acknowledge stream closure"));
        }
        self.cleanup.0.lock().map_err(invalid)?.take();
        Ok(())
    }
    fn refresh_cleanup(&self) -> Result<(), BuyerError> {
        let body = serde_json::to_vec(&Request {
            version: RELAY_VERSION,
            session_id: self.job_id,
            operation: "buyer_close_stream",
            arguments: &serde_json::json!({}),
        })
        .map_err(invalid)?;
        let request = self
            .hardware
            .prepared_request(reqwest::Method::POST, RELAY_PATH, body)?;
        *self.cleanup.0.lock().map_err(invalid)? = Some(request);
        Ok(())
    }
    pub(crate) async fn pump(&self, listener: TcpListener) -> Result<(), BuyerError> {
        self.refresh_cleanup()?;
        let stream_id = Uuid::new_v4();
        self.open(stream_id).await?;
        let (mut socket, _) = listener.accept().await.map_err(invalid)?;
        drop(listener);
        let mut state = StreamState::default();
        let mut closed_output = false;
        loop {
            if !state.local_eof
                && state.outgoing.len() < CHUNK
                && tokio::time::timeout(Duration::from_millis(20), socket.readable())
                    .await
                    .is_ok()
            {
                let mut chunk = vec![0; CHUNK - state.outgoing.len()];
                match socket.try_read(&mut chunk) {
                    Ok(0) => state.local_eof = true,
                    Ok(count) => state.outgoing.extend_from_slice(&chunk[..count]),
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                    Err(error) => return Err(invalid(error)),
                }
            }
            if !state.incoming.is_empty()
                && tokio::time::timeout(Duration::from_millis(20), socket.writable())
                    .await
                    .is_ok()
            {
                match socket.try_write(&state.incoming) {
                    Ok(0) => return Err(invalid("local SSH channel stopped accepting bytes")),
                    Ok(count) => {
                        state.incoming.drain(..count);
                        state.delivered += count as u64;
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                    Err(error) => return Err(invalid(error)),
                }
            }
            let response: Exchange = self.request("buyer_exchange", &serde_json::json!({
                "stream_id":stream_id, "operation_id":Uuid::new_v4(), "send_offset":state.sent,
                "data":STANDARD.encode(&state.outgoing), "receive_ack":state.delivered, "eof":state.local_eof
            })).await?;
            state.apply(response)?;
            if state.remote_eof && state.incoming.is_empty() && !closed_output {
                socket.shutdown().await.map_err(invalid)?;
                closed_output = true;
            }
            if state.complete() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

// Refreshed before each bounded HTTP request, so the pre-signed close remains
// within the authentication window when a future is cancelled. No key copy or
// background tunnel survives cancellation; only one bounded close is sent.
#[derive(Default)]
struct CancellationClose(Mutex<Option<(reqwest::Client, reqwest::Request)>>);
impl Drop for CancellationClose {
    fn drop(&mut self) {
        if let (Ok(request), Ok(runtime)) =
            (self.0.get_mut(), tokio::runtime::Handle::try_current())
        {
            if let Some((http, request)) = request.take() {
                runtime.spawn(async move {
                    let _ = tokio::time::timeout(HTTP_TIMEOUT, http.execute(request)).await;
                });
            }
        }
    }
}

#[derive(Default)]
struct StreamState {
    outgoing: Vec<u8>,
    incoming: Vec<u8>,
    sent: u64,
    delivered: u64,
    local_eof: bool,
    remote_eof: bool,
    sent_eof: bool,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Exchange {
    send_ack: u64,
    send_eof: bool,
    receive_offset: u64,
    data: String,
    eof: bool,
}
impl StreamState {
    fn apply(&mut self, response: Exchange) -> Result<(), BuyerError> {
        if response.send_ack < self.sent
            || response.send_ack > self.sent + self.outgoing.len() as u64
            || response.receive_offset != self.delivered
            || (self.sent_eof && !response.send_eof)
            || response.data.len() > CHUNK.div_ceil(3) * 4
        {
            return Err(invalid(
                "relay changed acknowledged offsets or stream flags",
            ));
        }
        let chunk = STANDARD
            .decode(&response.data)
            .map_err(|_| invalid("malformed relay chunk"))?;
        if chunk.len() > CHUNK
            || STANDARD.encode(&chunk) != response.data
            || chunk.len() < self.incoming.len()
            || !chunk.starts_with(&self.incoming)
            || (self.remote_eof && (!response.eof || chunk.len() != self.incoming.len()))
            || response.send_ack + self.delivered + chunk.len() as u64 > STREAM_BYTES
        {
            return Err(invalid(
                "relay changed replayed bytes or exceeded stream bounds",
            ));
        }
        let accepted = (response.send_ack - self.sent) as usize;
        if response.send_eof && (!self.local_eof || accepted != self.outgoing.len()) {
            return Err(invalid("relay acknowledged EOF before all source bytes"));
        }
        self.outgoing.drain(..accepted);
        self.sent = response.send_ack;
        self.sent_eof = response.send_eof;
        self.incoming = chunk;
        self.remote_eof = response.eof;
        Ok(())
    }
    fn complete(&self) -> bool {
        self.local_eof
            && self.sent_eof
            && self.remote_eof
            && self.outgoing.is_empty()
            && self.incoming.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Bytes,
        extract::State,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        Json, Router,
    };
    use covenant_compute_attestation::binding;
    use covenant_identity::LocalIdentity;
    use serde_json::{json, Value};
    use std::{collections::HashSet, sync::Arc};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn exchange(send_ack: u64, receive_offset: u64, data: &[u8], eof: bool) -> Exchange {
        Exchange {
            send_ack,
            receive_offset,
            data: STANDARD.encode(data),
            send_eof: false,
            eof,
        }
    }
    #[test]
    fn partial_ack_and_replayed_receive_prefix_preserve_every_byte() {
        let mut state = StreamState {
            outgoing: b"abcdef".to_vec(),
            ..Default::default()
        };
        state.apply(exchange(2, 0, b"hello", false)).unwrap();
        assert_eq!(state.outgoing, b"cdef");
        state.incoming.drain(..2);
        state.delivered = 2;
        state.apply(exchange(6, 2, b"llo world", false)).unwrap();
        assert_eq!(state.incoming, b"llo world");
        state.incoming.clear();
        state.delivered = 11;
        state.local_eof = true;
        state
            .apply(Exchange {
                send_eof: true,
                ..exchange(6, 11, b"", true)
            })
            .unwrap();
        assert!(state.complete());
    }
    #[test]
    fn corrupted_offsets_replays_eof_chunks_and_stream_budget_refuse() {
        for case in 0..11 {
            let mut state = StreamState {
                outgoing: b"ab".to_vec(),
                incoming: b"cd".to_vec(),
                sent: 4,
                delivered: 2,
                ..Default::default()
            };
            let mut response = exchange(6, 2, b"cdef", false);
            match case {
                0 => response.send_ack = 3,
                1 => response.send_ack = 7,
                2 => response.receive_offset = 1,
                3 => response.data = STANDARD.encode(b"XYef"),
                4 => response.data = STANDARD.encode(b"c"),
                5 => response.send_eof = true,
                6 => state.sent_eof = true,
                7 => {
                    state.remote_eof = true;
                    response.eof = true;
                }
                8 => response.data = "@@@@".into(),
                9 => response.data = STANDARD.encode(vec![0; CHUNK + 1]),
                _ => {
                    state.sent = STREAM_BYTES;
                    response.send_ack = STREAM_BYTES;
                }
            }
            assert!(state.apply(response).is_err(), "mutation {case}");
        }
        for malformed in [
            r#"{"send_ack":0,"send_ack":1,"send_eof":false,"receive_offset":0,"data":"","eof":false}"#,
            r#"{"send_ack":0,"send_eof":false,"receive_offset":0,"data":"","eof":false,"extra":1}"#,
            r#"{"send_ack":0.0,"send_eof":false,"receive_offset":0,"data":"","eof":false}"#,
        ] {
            assert!(serde_json::from_str::<Exchange>(malformed).is_err());
        }
    }
    #[test]
    fn verified_app_instance_and_signed_origin_are_all_required() {
        use covenant_compute_attestation::{
            binding::{HardwareChallenge, PhalaEphemeralIdentity},
            vendor_types::TrustClass,
        };
        use covenant_compute_protocol::ssh::{SshHostKey, SshTransport};
        let job_id = Uuid::new_v4();
        let now = crate::epoch_ms();
        let key =
            STANDARD.encode([b"\0\0\0\x0bssh-ed25519\0\0\0\x20".as_slice(), &[7; 32]].concat());
        let scope = HardwareScope {
            profile: HardwareProfile::PhalaEphemeralDstack061,
            job_id,
            operator_pubkey_b58: bs58::encode([3; 32]).into_string(),
            buyer_pubkey_b58: bs58::encode([4; 32]).into_string(),
            lease_generation: format!("phala-ephemeral-{job_id}-1"),
            host: "relay.example.com".into(),
            port: 443,
            transport: Some(SshTransport::OutboundRelay),
            username: "covenant".into(),
            guest_channel_key: SshHostKey {
                algorithm: "ssh-ed25519".into(),
                key_base64: key,
            },
            workload_sha256: "ab".repeat(32),
            lease_expires_at_ms: now + 600_000,
        };
        // This fixture exercises post-crypto comparison only; no public
        // verifier accepts this unsigned synthetic verdict.
        let verdict = HardwareVerdict {
            challenge: HardwareChallenge::new(scope.clone(), now).unwrap(),
            evidence_sha256: "ab".repeat(32),
            reference_sha256: binding::reference_digest_for_profile(scope.profile),
            guest_identity: "test".into(),
            phala_identity: Some(PhalaEphemeralIdentity {
                app_id: "ab".repeat(20),
                instance_id: "cd".repeat(20),
            }),
            gpu_identity: None,
            channel_key_fingerprint: "test".into(),
            evidence_class: TrustClass::Attested,
            served_class: TrustClass::Open,
            verifier_version: "test".into(),
            verified_at_ms: now,
            expires_at_ms: now + 120_000,
        };
        let metadata = RelayMetadata {
            schema_version: 1,
            job_id,
            app_id: "ab".repeat(20),
            instance_id: None,
            relay_endpoint: "https://relay.example.com".into(),
            expires_at_ms: scope.lease_expires_at_ms,
        };
        check_metadata(&metadata, &scope, &verdict, &metadata.relay_endpoint).unwrap();
        require_opening_grant(&verdict, now).unwrap();
        let mut stale = verdict.clone();
        stale.challenge.expires_at_ms = now;
        assert!(stale.expires_at_ms > now);
        assert!(require_opening_grant(&stale, now)
            .unwrap_err()
            .to_string()
            .contains("opening grant expired"));
        assert!(require_opening_grant(&verdict, now - 1).is_err());
        for mutation in 0..9 {
            let mut changed = metadata.clone();
            let mut verdict = verdict.clone();
            let mut scope = scope.clone();
            match mutation {
                0 => changed.app_id = "ff".repeat(20),
                1 => changed.instance_id = Some("ff".repeat(20)),
                2 => changed.job_id = Uuid::new_v4(),
                3 => changed.relay_endpoint = "https://other.example.com".into(),
                4 => changed.expires_at_ms += 1,
                5 => changed.schema_version = 2,
                6 => verdict.phala_identity = None,
                7 => scope.profile = HardwareProfile::Standard,
                _ => verdict.challenge.scope.workload_sha256 = "ff".repeat(32),
            }
            assert!(
                check_metadata(&changed, &scope, &verdict, &metadata.relay_endpoint).is_err(),
                "mutation {mutation}"
            );
        }
        assert!(check_metadata(&metadata, &scope, &verdict, "https://other.example.com").is_err());
    }
    #[derive(Default)]
    struct Echo {
        requests: Vec<Value>,
        operations: HashSet<String>,
        received: Vec<u8>,
        outgoing: Vec<u8>,
        acknowledged: u64,
        eof: bool,
        stream: Option<Value>,
        closed: bool,
        fail_open: bool,
    }
    type Shared = Arc<Mutex<Echo>>;
    async fn handler(
        State(state): State<Shared>,
        headers: HeaderMap,
        body: Bytes,
    ) -> axum::response::Response {
        binding::verify_request(
            headers["x-covenant-hardware-actor"].to_str().unwrap(),
            headers["x-covenant-hardware-signature"].to_str().unwrap(),
            "POST",
            RELAY_PATH,
            &body,
            headers["x-covenant-hardware-signed-at"]
                .to_str()
                .unwrap()
                .parse()
                .unwrap(),
            crate::epoch_ms(),
        )
        .unwrap();
        let request: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(request["version"], RELAY_VERSION);
        let mut state = state.lock().unwrap();
        state.requests.push(request.clone());
        let args = &request["arguments"];
        let result = match request["operation"].as_str().unwrap() {
            "buyer_submit" => {
                if state.fail_open {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                assert_eq!(args["kind"], "open_stream");
                assert!(state.stream.is_none());
                state.stream = Some(args["payload"]["stream_id"].clone());
                json!({"request_id":args["request_id"]})
            }
            "buyer_result" => {
                json!({"request_id":args["request_id"],"result":{"ok":true,"value":{"stream_id":state.stream}}})
            }
            "buyer_exchange" => {
                assert_eq!(args["stream_id"], state.stream.clone().unwrap());
                assert!(state
                    .operations
                    .insert(args["operation_id"].as_str().unwrap().into()));
                assert_eq!(
                    args["send_offset"].as_u64().unwrap(),
                    state.received.len() as u64
                );
                let chunk = STANDARD.decode(args["data"].as_str().unwrap()).unwrap();
                let count = chunk.len().min(4096);
                let delivered = args["receive_ack"].as_u64().unwrap();
                let drain = (delivered - state.acknowledged) as usize;
                state.outgoing.drain(..drain);
                state.acknowledged = delivered;
                state.received.extend_from_slice(&chunk[..count]);
                state.outgoing.extend_from_slice(&chunk[..count]);
                state.eof |= args["eof"].as_bool().unwrap() && count == chunk.len();
                json!({"send_ack":state.received.len(),"send_eof":state.eof,"receive_offset":delivered,
                    "data":STANDARD.encode(&state.outgoing[..state.outgoing.len().min(1024)]),
                    "eof":state.eof && state.outgoing.len() <= 1024})
            }
            "buyer_close_stream" => {
                state.closed = true;
                json!({"closed":true})
            }
            _ => panic!("unexpected operation"),
        };
        Json(json!({"version":RELAY_VERSION,"result":result})).into_response()
    }
    async fn server(state: Shared) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route(RELAY_PATH, axum::routing::post(handler))
            .with_state(state);
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (origin, task)
    }
    fn session<'a, 'i>(hardware: &'a HardwareClient<'i>) -> RelaySession<'a, 'i> {
        // Transport fixture only: production constructs this exclusively after
        // independent vendor verification and original-app metadata comparison.
        RelaySession {
            hardware,
            job_id: Uuid::new_v4(),
            challenge_id: Uuid::new_v4(),
            challenge_expires_at_ms: crate::epoch_ms() + 20_000,
            expires_at_ms: crate::epoch_ms() + 20_000,
            lease_expires_at_ms: crate::epoch_ms() + 20_000,
            cleanup: CancellationClose::default(),
        }
    }
    #[tokio::test]
    async fn native_signed_duplex_pump_handles_partial_acks_and_half_close() {
        let state = Arc::new(Mutex::new(Echo::default()));
        let (origin, server) = server(state.clone()).await;
        let buyer = LocalIdentity::generate("relay-test");
        let hardware = HardwareClient::new(&origin, &buyer).unwrap();
        let session = session(&hardware);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let payload = (0..20_731).map(|i| (i % 251) as u8).collect::<Vec<_>>();
        let local = async {
            let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
            socket.write_all(&payload).await.unwrap();
            socket.shutdown().await.unwrap();
            let mut result = Vec::new();
            socket.read_to_end(&mut result).await.unwrap();
            assert_eq!(result, payload);
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            let (pump, ()) = tokio::join!(session.pump(listener), local);
            pump.unwrap();
        })
        .await
        .unwrap();
        session.close().await.unwrap();
        let state = state.lock().unwrap();
        assert_eq!(state.received, payload);
        assert!(state.closed);
        assert!(state
            .requests
            .iter()
            .all(|request| request["session_id"] == session.job_id.to_string()));
        server.abort();
    }
    #[tokio::test]
    async fn unknown_open_is_not_retried_and_cancellation_sends_signed_close() {
        let state = Arc::new(Mutex::new(Echo {
            fail_open: true,
            ..Default::default()
        }));
        let (origin, server) = server(state.clone()).await;
        let buyer = LocalIdentity::generate("relay-unknown");
        let hardware = HardwareClient::new(&origin, &buyer).unwrap();
        {
            let session = session(&hardware);
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            assert!(session.pump(listener).await.is_err());
            assert_eq!(state.lock().unwrap().requests.len(), 1);
            // Cancellation/drop owns no signing key and makes only an
            // idempotent close request; it never reopens an uncertain stream.
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while !state.lock().unwrap().closed {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let state = state.lock().unwrap();
        assert_eq!(state.requests.len(), 2);
        assert_eq!(state.requests[1]["operation"], "buyer_close_stream");
        server.abort();
    }
    #[tokio::test]
    async fn stale_grant_refuses_before_network() {
        let state = Arc::new(Mutex::new(Echo::default()));
        let (origin, server) = server(state.clone()).await;
        let buyer = LocalIdentity::generate("relay-expired");
        let hardware = HardwareClient::new(&origin, &buyer).unwrap();
        let mut session = session(&hardware);
        session.expires_at_ms = crate::epoch_ms();
        assert!(session
            .open(Uuid::new_v4())
            .await
            .unwrap_err()
            .to_string()
            .contains("expired"));
        assert!(state.lock().unwrap().requests.is_empty());
        server.abort();
    }
}
