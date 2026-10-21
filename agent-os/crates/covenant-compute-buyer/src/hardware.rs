//! Buyers re-run vendor verification locally before using an attested channel.
//! The same signed transport lets an operator submit externally collected raw
//! SNP/TDX and GPU reports; this client never manufactures hardware evidence.
//! Diagnostic evidence does not promote the network's released trust ceiling.
use crate::BuyerError;
use covenant_compute_attestation::{
    binding::{
        self, HardwareChallenge, HardwareEvidence, HardwareProfile, HardwareScope, HardwareVerdict,
        MAX_EVIDENCE_BYTES,
    },
    vendor_types::TrustClass,
};
use covenant_identity::LocalIdentity;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HardwareEvidenceResponse {
    pub evidence: HardwareEvidence,
    pub verdict: HardwareVerdict,
}
pub struct HardwareClient<'a> {
    http: reqwest::Client,
    base: String,
    identity: &'a LocalIdentity,
}
fn invalid(e: impl ToString) -> BuyerError {
    BuyerError::Coordinator(e.to_string())
}

/// Derive a scope from the buyer's original signed purchase and a fresh lease
/// view. The coordinator/operator cannot supply the expected workload digest.
pub fn scope_for_phala_lease(
    envelope: &covenant_compute_protocol::SignedJobEnvelope,
    view: &covenant_compute_protocol::LeaseView,
    expected_buyer: &str,
    now: u64,
) -> Result<HardwareScope, BuyerError> {
    use covenant_compute_protocol::{phala, ssh::SshTransport};
    envelope.verify().map_err(invalid)?;
    let job = &envelope.payload;
    job.validate_input().map_err(invalid)?;
    let (profile, operator, generation, expires, workload, relay_host) = match (
        phala::parse_phala_lease_spec(&job.input).map_err(invalid)?,
        phala::parse_phala_ephemeral_lease_spec(&job.input).map_err(invalid)?,
    ) {
        (Some(spec), None) => (
            HardwareProfile::Standard,
            spec.operator_pubkey_b58,
            spec.lease_generation,
            spec.lease_expires_at_ms,
            spec.workload_sha256,
            None,
        ),
        (None, Some(spec)) => {
            let host = spec.relay.host().map_err(invalid)?.to_owned();
            (
                HardwareProfile::PhalaEphemeralDstack061,
                spec.operator_pubkey_b58,
                spec.lease_generation,
                spec.lease_expires_at_ms,
                spec.workload_sha256,
                Some(host),
            )
        }
        _ => return Err(invalid("exactly one signed Phala workload pin is required")),
    };
    if job.buyer.pubkey_base58() != expected_buyer
        || view.job_id != job.job_id
        || view.status != "accepted"
        || view.close_requested
        || now < job.issued_at_ms
        || now >= expires
    {
        return Err(invalid(
            "Phala lease is not live or belongs to another buyer",
        ));
    }
    let access = view
        .access
        .as_ref()
        .ok_or_else(|| invalid("Phala lease is not ready"))?;
    access.validate().map_err(invalid)?;
    let ssh = access
        .ssh
        .as_ref()
        .ok_or_else(|| invalid("structured Phala SSH channel missing"))?;
    if access.job_id != job.job_id
        || access.ready_at_ms < job.issued_at_ms
        || access.ready_at_ms > now
        || ssh.lease_generation != generation
        || ssh.transport
            != Some(if relay_host.is_some() {
                SshTransport::OutboundRelay
            } else {
                SshTransport::Tls
            })
        || ssh.port != 443
        || ssh.username != "covenant"
        || ssh.observed_host_keys.len() != 1
    {
        return Err(invalid("Phala SSH channel differs from the signed lease"));
    }
    if let Some(expected_host) = relay_host {
        if ssh.host != expected_host {
            return Err(invalid("relay host differs from the buyer-signed origin"));
        }
    } else {
        let suffix = format!("-1022.{}", phala::PHALA_GATEWAY);
        let instance = ssh
            .host
            .strip_suffix(&suffix)
            .ok_or_else(|| invalid("unexpected Phala SSH endpoint"))?;
        if instance.len() != 40
            || !instance
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(invalid("invalid Phala instance endpoint"));
        }
    }
    Ok(HardwareScope {
        profile,
        job_id: job.job_id,
        operator_pubkey_b58: operator,
        buyer_pubkey_b58: expected_buyer.into(),
        lease_generation: generation,
        host: ssh.host.clone(),
        port: ssh.port,
        transport: ssh.transport,
        username: ssh.username.clone(),
        guest_channel_key: ssh.observed_host_keys[0].clone(),
        workload_sha256: workload,
        lease_expires_at_ms: expires,
    })
}

fn require_live_scope(scope: &HardwareScope, now: u64) -> Result<(), BuyerError> {
    if scope.job_id.is_nil() || now >= scope.lease_expires_at_ms {
        return Err(invalid("hardware lease is invalid or expired"));
    }
    Ok(())
}

/// Bound JSON while writing it, before a signature or network request exists.
fn request_body(value: &impl Serialize) -> Result<Vec<u8>, BuyerError> {
    struct BoundedBody(Vec<u8>);
    impl std::io::Write for BoundedBody {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0.len().saturating_add(bytes.len()) > MAX_EVIDENCE_BYTES {
                return Err(std::io::Error::other("hardware request exceeds limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut body = BoundedBody(Vec::new());
    serde_json::to_writer(&mut body, value).map_err(invalid)?;
    Ok(body.0)
}

impl<'a> HardwareClient<'a> {
    pub fn new(coordinator_url: &str, identity: &'a LocalIdentity) -> Result<Self, BuyerError> {
        let url = reqwest::Url::parse(coordinator_url).map_err(invalid)?;
        let loopback = url
            .host_str()
            .is_some_and(|h| h == "localhost" || h == "127.0.0.1" || h == "[::1]" || h == "::1");
        if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(invalid(
                "hardware coordinator requires a bare HTTPS origin (HTTP only for loopback)",
            ));
        }
        let http = reqwest::Client::builder()
            .user_agent("Covenant-Compute-Outbound/1")
            .retry(reqwest::retry::never())
            .connect_timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(invalid)?;
        Ok(Self {
            http,
            base: url.origin().ascii_serialization(),
            identity,
        })
    }
    pub(crate) async fn request<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Vec<u8>,
    ) -> Result<T, BuyerError> {
        if body.len() > MAX_EVIDENCE_BYTES {
            return Err(invalid("hardware request exceeds limit"));
        }
        let (http, request) = self.prepared_request(method, path, body)?;
        let mut response = http
            .execute(request)
            .await
            .map_err(|_| invalid("hardware request failed"))?;
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|length| length > (MAX_EVIDENCE_BYTES + 32 * 1024) as u64)
        {
            return Err(invalid("hardware response exceeds limit"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| invalid("hardware response failed"))?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_EVIDENCE_BYTES + 32 * 1024 {
                return Err(invalid("hardware response exceeds limit"));
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(invalid(format!("hardware request returned {status}")));
        }
        serde_json::from_slice(&bytes).map_err(|_| invalid("malformed hardware response"))
    }
    /// An owned, already signed request allows bounded cancellation cleanup
    /// without copying the buyer's key into a background task.
    pub(crate) fn prepared_request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Vec<u8>,
    ) -> Result<(reqwest::Client, reqwest::Request), BuyerError> {
        if body.len() > MAX_EVIDENCE_BYTES {
            return Err(invalid("hardware request exceeds limit"));
        }
        let now = crate::epoch_ms();
        let sig = binding::sign_request(self.identity, method.as_str(), path, &body, now)
            .map_err(invalid)?;
        let request = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header(
                "x-covenant-hardware-actor",
                self.identity.agent_id().pubkey_base58(),
            )
            .header("x-covenant-hardware-signed-at", now.to_string())
            .header("x-covenant-hardware-signature", sig)
            .header("content-type", "application/json")
            .body(body)
            .build()
            .map_err(invalid)?;
        Ok((self.http.clone(), request))
    }
    pub(crate) fn origin(&self) -> &str {
        &self.base
    }

    /// Caller supplies scope from its purchased lease and an independently pinned
    /// workload. Never adopt a server-selected scope as the expected identity.
    /// Either the buyer or assigned operator may request the challenge. The
    /// coordinator also checks that the lease is still accepted and not closing.
    pub async fn challenge(
        &self,
        expected: &HardwareScope,
        host_key_index: usize,
    ) -> Result<HardwareChallenge, BuyerError> {
        require_live_scope(expected, crate::epoch_ms())?;
        let actor = self.identity.agent_id().pubkey_base58();
        if actor != expected.buyer_pubkey_b58 && actor != expected.operator_pubkey_b58 {
            return Err(invalid(
                "hardware challenge belongs to another buyer or operator",
            ));
        }
        if host_key_index >= 8 {
            return Err(invalid("hardware host key index exceeds lease limit"));
        }
        let result: HardwareChallenge = self
            .request(
                reqwest::Method::POST,
                &format!("/federation/hardware/{}/challenge", expected.job_id),
                request_body(&serde_json::json!({"host_key_index":host_key_index}))?,
            )
            .await?;
        result.validate(crate::epoch_ms()).map_err(invalid)?;
        if &result.scope != expected {
            return Err(invalid(
                "hardware challenge differs from purchased lease or workload pin",
            ));
        }
        Ok(result)
    }
    pub async fn submit(
        &self,
        evidence: &HardwareEvidence,
    ) -> Result<HardwareEvidenceResponse, BuyerError> {
        if evidence.challenge.scope.operator_pubkey_b58 != self.identity.agent_id().pubkey_base58()
        {
            return Err(invalid(
                "hardware evidence must be submitted by the assigned operator",
            ));
        }
        evidence
            .challenge
            .validate(crate::epoch_ms())
            .map_err(invalid)?;
        let body = request_body(evidence)?;
        let result: HardwareEvidenceResponse = self
            .request(
                reqwest::Method::POST,
                &format!(
                    "/federation/hardware/{}/evidence",
                    evidence.challenge.scope.job_id
                ),
                body.clone(),
            )
            .await?;
        if request_body(&result.evidence)? != body {
            return Err(invalid("hardware response differs from submitted evidence"));
        }
        binding::verify_saved_verdict(
            &result.evidence,
            &result.verdict,
            &evidence.challenge.scope,
            crate::epoch_ms(),
        )
        .map_err(invalid)?;
        Ok(result)
    }
    /// Re-verify evidence for the caller's current purchased scope, including
    /// its independently pinned workload and observed lease generation/key.
    /// The coordinator must also refuse a closed or replaced lease. A verified
    /// evidence class does not override the released served-trust ceiling.
    pub async fn verify_lease(
        &self,
        expected: &HardwareScope,
        floor: TrustClass,
    ) -> Result<HardwareVerdict, BuyerError> {
        if expected.buyer_pubkey_b58 != self.identity.agent_id().pubkey_base58() {
            return Err(invalid("hardware lease belongs to another buyer"));
        }
        require_live_scope(expected, crate::epoch_ms())?;
        if expected.profile == HardwareProfile::PhalaEphemeralDstack061 && floor > TrustClass::Open
        {
            return Err(invalid("ephemeral Phala currently serves only Open trust; refusing stronger workspace floor"));
        }
        let result: HardwareEvidenceResponse = self
            .request(
                reqwest::Method::GET,
                &format!("/federation/hardware/{}/verdict", expected.job_id),
                Vec::new(),
            )
            .await?;
        let now = crate::epoch_ms();
        binding::verify_saved_verdict_for_profile(
            &result.evidence,
            &result.verdict,
            expected,
            now,
            expected.profile,
        )
        .map_err(invalid)?;
        result
            .verdict
            .require_floor(expected, floor, now)
            .map_err(invalid)?;
        if expected.profile == HardwareProfile::PhalaEphemeralDstack061 {
            crate::relay::require_opening_grant(&result.verdict, crate::epoch_ms())?;
            crate::relay::validate_metadata(self, expected, &result.verdict).await?;
        }
        Ok(result.verdict)
    }
    /// Formats only the single hardware-bound key after local crypto and floor
    /// checks. Pass this file to OpenSSH with StrictHostKeyChecking=yes.
    /// This formatting step does not promote an Open service to a higher tier.
    pub async fn verified_known_hosts(
        &self,
        expected: &HardwareScope,
        floor: TrustClass,
    ) -> Result<String, BuyerError> {
        self.verify_lease(expected, floor).await?;
        let host = if expected.port == 22 && !expected.host.contains(':') {
            expected.host.clone()
        } else {
            format!("[{}]:{}", expected.host, expected.port)
        };
        Ok(format!(
            "{host} {} {}\n",
            expected.guest_channel_key.algorithm, expected.guest_channel_key.key_base64
        ))
    }
    pub fn evidence_path(job: Uuid) -> String {
        format!("/federation/hardware/{job}/verdict")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, Bytes},
        http::{HeaderMap, Method, StatusCode},
        Router,
    };
    use base64::Engine as _;
    use covenant_compute_attestation::binding::GuestEvidence;
    use covenant_compute_protocol::ssh::SshHostKey;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct RequestRecord {
        method: Method,
        path: String,
        headers: HeaderMap,
        body: Vec<u8>,
    }

    struct Server {
        url: String,
        records: Arc<Mutex<Vec<RequestRecord>>>,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn serve(body: Vec<u8>, status: StatusCode, streamed: bool) -> Server {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let records = Arc::new(Mutex::new(Vec::new()));
        let received = records.clone();
        let router = Router::new().fallback(move |request: axum::extract::Request| {
            let body = body.clone();
            let received = received.clone();
            async move {
                let (parts, request_body) = request.into_parts();
                let request_body = axum::body::to_bytes(request_body, MAX_EVIDENCE_BYTES)
                    .await
                    .unwrap();
                received.lock().unwrap().push(RequestRecord {
                    method: parts.method,
                    path: parts.uri.path().into(),
                    headers: parts.headers,
                    body: request_body.to_vec(),
                });
                let response_body = if streamed {
                    Body::from_stream(futures::stream::iter(
                        body.chunks(32 * 1024)
                            .map(|chunk| Ok::<_, std::io::Error>(Bytes::copy_from_slice(chunk)))
                            .collect::<Vec<_>>(),
                    ))
                } else {
                    Body::from(body)
                };
                axum::http::Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .header("location", "/redirect-must-not-be-followed")
                    .body(response_body)
                    .unwrap()
            }
        });
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Server { url, records, task }
    }

    fn scope(buyer: &LocalIdentity, operator: &LocalIdentity) -> HardwareScope {
        let mut key = Vec::new();
        key.extend_from_slice(&11u32.to_be_bytes());
        key.extend_from_slice(b"ssh-ed25519");
        key.extend_from_slice(&32u32.to_be_bytes());
        key.extend_from_slice(&[0x9e; 32]);
        HardwareScope {
            profile: Default::default(),
            job_id: Uuid::new_v4(),
            operator_pubkey_b58: operator.agent_id().pubkey_base58(),
            buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
            lease_generation: "lease-generation-1".into(),
            host: "gpu.example".into(),
            port: 22,
            transport: None,
            username: "worker".into(),
            guest_channel_key: SshHostKey {
                algorithm: "ssh-ed25519".into(),
                key_base64: base64::engine::general_purpose::STANDARD.encode(key),
            },
            workload_sha256: "ab".repeat(32),
            lease_expires_at_ms: crate::epoch_ms() + 600_000,
        }
    }

    fn forged_response(scope: HardwareScope) -> HardwareEvidenceResponse {
        let now = crate::epoch_ms();
        let challenge = HardwareChallenge::new(scope, now).unwrap();
        HardwareEvidenceResponse {
            evidence: HardwareEvidence {
                challenge: challenge.clone(),
                guest: GuestEvidence::SevSnp {
                    report_base64: String::new(),
                    certificate_chain_base64: Vec::new(),
                },
                gpu_cc: None,
            },
            verdict: HardwareVerdict {
                challenge,
                evidence_sha256: "00".repeat(32),
                reference_sha256: binding::reference_digest(),
                guest_identity: "untrusted-coordinator-claim".into(),
                phala_identity: None,
                gpu_identity: None,
                channel_key_fingerprint: "untrusted-key-claim".into(),
                evidence_class: TrustClass::Attested,
                served_class: TrustClass::Open,
                verifier_version: "covenant.hardware/1".into(),
                verified_at_ms: now,
                expires_at_ms: now + 300_000,
            },
        }
    }

    #[test]
    fn ephemeral_scope_comes_only_from_original_signed_workload_and_relay() {
        use covenant_compute_protocol::{
            phala,
            ssh::{SshAccess, SshTransport},
            LeaseAccess, LeaseView,
        };
        let buyer = LocalIdentity::generate("scope@buyer");
        let operator = LocalIdentity::generate("scope@operator");
        let base = scope(&buyer, &operator);
        let args = crate::phala::PreparePhala {
            job_id: base.job_id,
            operator_pubkey_b58: base.operator_pubkey_b58.clone(),
            image_digest: format!("sha256:{}", "ab".repeat(32)),
            duration_secs: 600,
            rate_micro_usdc_per_sec: 1,
            buyer_ssh_public_key: format!(
                "{} {}",
                base.guest_channel_key.algorithm, base.guest_channel_key.key_base64
            ),
        };
        let relay = phala::PhalaRelayConfig {
            url: "https://relay.example.com".into(),
            session_id: base.job_id.to_string(),
        };
        let directory = tempfile::tempdir().unwrap();
        let prepared = crate::phala::prepare_ephemeral_to_directory(
            &buyer,
            &args,
            &relay,
            600,
            &directory.path().join("prepared"),
        )
        .unwrap();
        let spec = phala::parse_phala_ephemeral_lease_spec(&prepared.envelope.payload.input)
            .unwrap()
            .unwrap();
        let now = prepared.envelope.payload.issued_at_ms + 1;
        let view = LeaseView {
            job_id: base.job_id,
            status: "accepted".into(),
            close_requested: false,
            rate_micro_usdc_per_sec: 1,
            max_duration_secs: 600,
            elapsed_ms: 0,
            charged_micro_usdc: 0,
            access: Some(LeaseAccess {
                job_id: base.job_id,
                endpoint: "untrusted human description".into(),
                ready_at_ms: now,
                note: None,
                source_price: None,
                ssh: Some(SshAccess {
                    host: "relay.example.com".into(),
                    port: 443,
                    username: "covenant".into(),
                    lease_generation: spec.lease_generation.clone(),
                    transport: Some(SshTransport::OutboundRelay),
                    observed_host_keys: vec![base.guest_channel_key],
                }),
            }),
        };
        let expected =
            scope_for_phala_lease(&prepared.envelope, &view, &base.buyer_pubkey_b58, now).unwrap();
        assert_eq!(expected.profile, HardwareProfile::PhalaEphemeralDstack061);
        assert_eq!(expected.workload_sha256, spec.workload_sha256);
        for case in 0..8 {
            let mut changed = view.clone();
            match case {
                0 => {
                    changed.access.as_mut().unwrap().ssh.as_mut().unwrap().host =
                        "other.example.com".into()
                }
                1 => {
                    changed
                        .access
                        .as_mut()
                        .unwrap()
                        .ssh
                        .as_mut()
                        .unwrap()
                        .transport = Some(SshTransport::Tls)
                }
                2 => changed
                    .access
                    .as_mut()
                    .unwrap()
                    .ssh
                    .as_mut()
                    .unwrap()
                    .lease_generation
                    .push('x'),
                3 => changed.access.as_mut().unwrap().ready_at_ms = now + 1,
                4 => changed.close_requested = true,
                5 => changed.status = "closed".into(),
                6 => changed.job_id = Uuid::new_v4(),
                _ => changed
                    .access
                    .as_mut()
                    .unwrap()
                    .ssh
                    .as_mut()
                    .unwrap()
                    .observed_host_keys
                    .clear(),
            }
            assert!(
                scope_for_phala_lease(&prepared.envelope, &changed, &base.buyer_pubkey_b58, now)
                    .is_err(),
                "case {case}"
            );
        }
        assert!(scope_for_phala_lease(
            &prepared.envelope,
            &view,
            &base.buyer_pubkey_b58,
            spec.lease_expires_at_ms
        )
        .is_err());
        let mut substituted = prepared.envelope.clone();
        substituted.payload.capability_requirement.model_id = Some(phala::PHALA_CPU_MODEL.into());
        assert!(scope_for_phala_lease(&substituted, &view, &base.buyer_pubkey_b58, now).is_err());
    }

    #[tokio::test]
    async fn ephemeral_stronger_floor_refuses_before_http_and_forged_quote_never_reaches_metadata()
    {
        let buyer = LocalIdentity::generate("buyer@ephemeral");
        let operator = LocalIdentity::generate("operator@ephemeral");
        let mut expected = scope(&buyer, &operator);
        expected.profile = HardwareProfile::PhalaEphemeralDstack061;
        expected.lease_generation = format!("phala-ephemeral-{}-1", expected.job_id);
        expected.host = "relay.example.com".into();
        expected.port = 443;
        expected.transport = Some(covenant_compute_protocol::ssh::SshTransport::OutboundRelay);
        expected.username = "covenant".into();
        let forged = forged_response(expected.clone());
        let server = serve(serde_json::to_vec(&forged).unwrap(), StatusCode::OK, false).await;
        let client = HardwareClient::new(&server.url, &buyer).unwrap();
        for floor in [
            TrustClass::Isolated,
            TrustClass::Attested,
            TrustClass::Confidential,
        ] {
            assert!(client
                .verify_lease(&expected, floor)
                .await
                .unwrap_err()
                .to_string()
                .contains("only Open"));
        }
        assert!(server.records.lock().unwrap().is_empty());
        assert!(client
            .verify_lease(&expected, TrustClass::Open)
            .await
            .is_err());
        let records = server.records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].path.ends_with("/verdict"));
    }

    #[test]
    fn refuses_unsafe_origins() {
        let identity = LocalIdentity::generate("hardware@buyer");
        for origin in [
            "http://gpu.example",
            "https://user:pass@gpu.example",
            "https://gpu.example/subpath",
            "https://gpu.example?token=x",
            "https://gpu.example/#fragment",
            "http://localhost.evil.example",
            "http://127.0.0.1.evil.example",
            "http://192.168.1.1",
            "file:///tmp/hardware",
            "ftp://gpu.example",
        ] {
            assert!(HardwareClient::new(origin, &identity).is_err());
        }
        for origin in [
            "http://127.0.0.1:1234",
            "http://localhost:1234",
            "http://[::1]:1234",
            "https://gpu.example",
        ] {
            assert!(HardwareClient::new(origin, &identity).is_ok());
        }
        assert_eq!(
            HardwareClient::new("HTTPS://GPU.EXAMPLE:443/", &identity)
                .unwrap()
                .base,
            "https://gpu.example"
        );
    }

    #[test]
    fn bounds_json_as_it_serializes() {
        let value = "x".repeat(MAX_EVIDENCE_BYTES - 2);
        assert_eq!(request_body(&value).unwrap().len(), MAX_EVIDENCE_BYTES);
        assert!(request_body(&format!("{value}x"))
            .unwrap_err()
            .to_string()
            .contains("exceeds limit"));
    }

    #[tokio::test]
    async fn challenge_signs_exact_actor_method_path_and_body() {
        let buyer = LocalIdentity::generate("buyer@hardware");
        let operator = LocalIdentity::generate("operator@hardware");
        let expected = scope(&buyer, &operator);
        let challenge = HardwareChallenge::new(expected.clone(), crate::epoch_ms()).unwrap();
        let server = serve(
            serde_json::to_vec(&challenge).unwrap(),
            StatusCode::OK,
            false,
        )
        .await;
        for identity in [&buyer, &operator] {
            let client = HardwareClient::new(&server.url, identity).unwrap();
            assert_eq!(client.challenge(&expected, 0).await.unwrap(), challenge);
        }
        let records = server.records.lock().unwrap();
        assert_eq!(records.len(), 2);
        for (record, identity) in records.iter().zip([&buyer, &operator]) {
            assert_eq!(record.method, Method::POST);
            assert_eq!(record.headers["user-agent"], "Covenant-Compute-Outbound/1");
            assert_eq!(
                record.path,
                format!("/federation/hardware/{}/challenge", expected.job_id)
            );
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&record.body).unwrap(),
                serde_json::json!({"host_key_index": 0})
            );
            let actor = record.headers["x-covenant-hardware-actor"]
                .to_str()
                .unwrap();
            assert_eq!(actor, identity.agent_id().pubkey_base58());
            let signed_at = record.headers["x-covenant-hardware-signed-at"]
                .to_str()
                .unwrap()
                .parse()
                .unwrap();
            binding::verify_request(
                actor,
                record.headers["x-covenant-hardware-signature"]
                    .to_str()
                    .unwrap(),
                "POST",
                &record.path,
                &record.body,
                signed_at,
                crate::epoch_ms(),
            )
            .unwrap();
        }
    }

    #[tokio::test]
    async fn rejects_wrong_actor_expired_lease_and_oversized_submission_before_http() {
        let buyer = LocalIdentity::generate("buyer@hardware");
        let operator = LocalIdentity::generate("operator@hardware");
        let stranger = LocalIdentity::generate("stranger@hardware");
        let expected = scope(&buyer, &operator);
        let server = serve(b"{}".to_vec(), StatusCode::OK, false).await;
        let stranger_client = HardwareClient::new(&server.url, &stranger).unwrap();
        assert!(stranger_client
            .challenge(&expected, 0)
            .await
            .unwrap_err()
            .to_string()
            .contains("another buyer or operator"));
        assert!(stranger_client
            .verify_lease(&expected, TrustClass::Open)
            .await
            .unwrap_err()
            .to_string()
            .contains("another buyer"));
        let client = HardwareClient::new(&server.url, &buyer).unwrap();
        assert!(client.challenge(&expected, 8).await.is_err());
        let mut expired = expected.clone();
        expired.lease_expires_at_ms = crate::epoch_ms();
        assert!(client
            .challenge(&expired, 0)
            .await
            .unwrap_err()
            .to_string()
            .contains("expired"));
        assert!(client
            .verify_lease(&expired, TrustClass::Open)
            .await
            .unwrap_err()
            .to_string()
            .contains("expired"));
        let mut evidence = forged_response(expected).evidence;
        assert!(client
            .submit(&evidence)
            .await
            .unwrap_err()
            .to_string()
            .contains("assigned operator"));
        let operator_client = HardwareClient::new(&server.url, &operator).unwrap();
        if let GuestEvidence::SevSnp { report_base64, .. } = &mut evidence.guest {
            *report_base64 = "x".repeat(MAX_EVIDENCE_BYTES);
        }
        assert!(operator_client
            .submit(&evidence)
            .await
            .unwrap_err()
            .to_string()
            .contains("exceeds limit"));
        evidence.challenge.expires_at_ms = crate::epoch_ms();
        assert!(operator_client
            .submit(&evidence)
            .await
            .unwrap_err()
            .to_string()
            .contains("stale or invalid challenge"));
        assert!(server.records.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn refuses_server_substitution_of_any_purchased_scope_field() {
        let buyer = LocalIdentity::generate("buyer@hardware");
        let operator = LocalIdentity::generate("operator@hardware");
        let expected = scope(&buyer, &operator);
        for field in 0..10 {
            let mut changed = expected.clone();
            match field {
                0 => changed.job_id = Uuid::new_v4(),
                1 => changed.operator_pubkey_b58 = buyer.agent_id().pubkey_base58(),
                2 => changed.buyer_pubkey_b58 = operator.agent_id().pubkey_base58(),
                3 => changed.lease_generation.push('x'),
                4 => changed.host = "other.example".into(),
                5 => changed.port = 2222,
                6 => changed.username = "another".into(),
                7 => {
                    changed.guest_channel_key.key_base64 =
                        changed.guest_channel_key.key_base64.replace("np6e", "p6en")
                }
                8 => changed.workload_sha256 = "cd".repeat(32),
                _ => changed.lease_expires_at_ms += 1,
            }
            let challenge = HardwareChallenge::new(changed, crate::epoch_ms()).unwrap();
            let server = serve(
                serde_json::to_vec(&challenge).unwrap(),
                StatusCode::OK,
                false,
            )
            .await;
            let client = HardwareClient::new(&server.url, &buyer).unwrap();
            assert!(
                client
                    .challenge(&expected, 0)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("differs from purchased lease"),
                "field {field}"
            );
        }
    }

    #[tokio::test]
    async fn refuses_expired_or_future_challenges() {
        let buyer = LocalIdentity::generate("buyer@hardware");
        let operator = LocalIdentity::generate("operator@hardware");
        let expected = scope(&buyer, &operator);
        for future in [false, true] {
            let mut challenge =
                HardwareChallenge::new(expected.clone(), crate::epoch_ms()).unwrap();
            if future {
                challenge.issued_at_ms += 10_000;
            } else {
                challenge.expires_at_ms = crate::epoch_ms().saturating_sub(1);
            }
            let server = serve(
                serde_json::to_vec(&challenge).unwrap(),
                StatusCode::OK,
                false,
            )
            .await;
            let client = HardwareClient::new(&server.url, &buyer).unwrap();
            assert!(client
                .challenge(&expected, 0)
                .await
                .unwrap_err()
                .to_string()
                .contains("stale or invalid challenge"));
        }
    }

    #[tokio::test]
    async fn coordinator_claim_cannot_replace_local_vendor_verification_or_release_trust() {
        let buyer = LocalIdentity::generate("buyer@hardware");
        let operator = LocalIdentity::generate("operator@hardware");
        let expected = scope(&buyer, &operator);
        assert_eq!(binding::SERVED_TRUST_CEILING, TrustClass::Open);
        for class in [
            TrustClass::Open,
            TrustClass::Attested,
            TrustClass::Confidential,
        ] {
            let mut response = forged_response(expected.clone());
            response.verdict.served_class = class;
            let server = serve(
                serde_json::to_vec(&response).unwrap(),
                StatusCode::OK,
                false,
            )
            .await;
            let client = HardwareClient::new(&server.url, &buyer).unwrap();
            // A forged verdict with a perfectly matching scope still reaches
            // the production verifier and fails on its missing vendor chain.
            assert!(client
                .verify_lease(&expected, TrustClass::Open)
                .await
                .unwrap_err()
                .to_string()
                .contains("certificate chain exceeds limits"));
            assert!(client
                .verified_known_hosts(&expected, TrustClass::Open)
                .await
                .is_err());
        }
    }

    #[tokio::test]
    async fn rejects_verdict_times_and_current_lease_substitution() {
        let buyer = LocalIdentity::generate("buyer@hardware");
        let operator = LocalIdentity::generate("operator@hardware");
        let expected = scope(&buyer, &operator);
        for mutation in 0..3 {
            let mut response = forged_response(expected.clone());
            match mutation {
                0 => response.verdict.verified_at_ms += 10_000,
                1 => response.verdict.expires_at_ms = crate::epoch_ms(),
                _ => response.evidence.challenge.scope.lease_generation.push('x'),
            }
            let server = serve(
                serde_json::to_vec(&response).unwrap(),
                StatusCode::OK,
                false,
            )
            .await;
            let client = HardwareClient::new(&server.url, &buyer).unwrap();
            assert!(client
                .verify_lease(&expected, TrustClass::Open)
                .await
                .unwrap_err()
                .to_string()
                .contains("does not bind the current lease"));
        }
    }

    #[tokio::test]
    async fn operator_submission_requires_exact_evidence_echo_and_local_verification() {
        let buyer = LocalIdentity::generate("buyer@hardware");
        let operator = LocalIdentity::generate("operator@hardware");
        let response = forged_response(scope(&buyer, &operator));
        for substitute in [false, true] {
            let mut returned = response.clone();
            if substitute {
                returned.evidence.challenge.nonce_hex = "cd".repeat(32);
            }
            let server = serve(
                serde_json::to_vec(&returned).unwrap(),
                StatusCode::OK,
                false,
            )
            .await;
            let client = HardwareClient::new(&server.url, &operator).unwrap();
            let error = client
                .submit(&response.evidence)
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(if substitute {
                "differs from submitted evidence"
            } else {
                "certificate chain exceeds limits"
            }));
        }
    }

    #[tokio::test]
    async fn transport_bounds_fixed_and_streamed_responses_and_never_follows_redirects() {
        let identity = LocalIdentity::generate("buyer@hardware");
        let limit = MAX_EVIDENCE_BYTES + 32 * 1024;
        for (body, status, streamed, expected) in [
            (
                vec![b'x'; limit + 1],
                StatusCode::OK,
                false,
                "exceeds limit",
            ),
            (vec![b'x'; limit + 1], StatusCode::OK, true, "exceeds limit"),
            (
                b"not-json".to_vec(),
                StatusCode::OK,
                false,
                "malformed hardware response",
            ),
            (b"{}".to_vec(), StatusCode::TEMPORARY_REDIRECT, false, "307"),
            (
                b"{}".to_vec(),
                StatusCode::SERVICE_UNAVAILABLE,
                false,
                "503",
            ),
        ] {
            let server = serve(body, status, streamed).await;
            let client = HardwareClient::new(&server.url, &identity).unwrap();
            let error = client
                .request::<serde_json::Value>(reqwest::Method::GET, "/fixture", Vec::new())
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{error}");
            assert_eq!(server.records.lock().unwrap().len(), 1);
        }
    }
}
