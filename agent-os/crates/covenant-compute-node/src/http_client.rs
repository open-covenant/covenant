//! Real `Coordinator` client — the operator's outbound-only link to the
//! coordinator service (build-notes-phase1-foundation.md §1.5, §3 open
//! seam #1: "a real `impl Coordinator for HttpCoordinatorClient` ...
//! is a thin layer once the coordinator service exists"). Every call
//! dials out; the node never accepts an inbound connection
//! (design-02-federation.md §5's NAT/home-operator solution).
//!
//! Transport failures (connection refused, timeout, DNS failure) retry
//! with capped exponential backoff plus a little jitter — design-02 §4's
//! "reconnect w/ backoff+jitter at transport" failure-mode defense. A
//! landed 4xx is the coordinator's considered rejection (bad signature,
//! unknown job, stale session) and surfaces at once as
//! [`CoordinatorError::Protocol`], final. A landed 5xx (or a 408/429) is
//! the server transiently unable — a coordinator mid-deploy behind a
//! proxy answers 502/503/504 — so it joins [`CoordinatorError::Transport`],
//! the class every caller already treats as "no answer yet": the result
//! outbox holds the receipt, an accepted job stays booked, the serve loop
//! backs off and retries. Misreading a 5xx as final is how a coordinator
//! deploy used to cost an operator a finished, unpaid job.

use std::time::Duration;

use async_trait::async_trait;
use covenant_compute_protocol::{
    coordinator_reason, FundingSource, HeartbeatRequest, HeartbeatResponse, JobAccept, JobOffer,
    JobResultAck, JobResultMessage, RegisterRequest, RegisterResponse, StreamPush,
    PROTOCOL_VERSION, PROTOCOL_VERSION_HEADER,
};
use covenant_types::AgentId;
use parking_lot::Mutex;
use reqwest::RequestBuilder;

use crate::coordinator::{Coordinator, CoordinatorError};

const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_LONG_POLL_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_RETRIES: u32 = 3;
const BASE_BACKOFF: Duration = Duration::from_millis(250);
const MAX_BACKOFF: Duration = Duration::from_secs(5);

/// Byte ceiling on a coordinator response body, so a hostile or buggy
/// coordinator (or a proxy) cannot stream an unbounded body into node memory —
/// the same posture the model-backend reads already take
/// (`executor::read_body_capped`). The largest legitimate response is a
/// next-job offer carrying the buyer's signed envelope, itself bounded by the
/// coordinator's 8 MiB inbound body limit (the shared wire frame cap); this
/// sits at twice that, so the offer's wrapper around a max-size envelope is
/// never what trips the ceiling, while a runaway body still stops here.
const MAX_COORDINATOR_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Reads a coordinator response body, bounded to [`MAX_COORDINATOR_BODY_BYTES`].
/// The read stops at the ceiling and returns what it has rather than buffering
/// an unbounded stream: a truncated success body then fails to decode and
/// surfaces as a retryable [`CoordinatorError::Transport`] (the same class a
/// mangled 2xx already takes), and a truncated error body still yields a
/// bounded snippet for the message.
async fn read_capped(mut resp: reqwest::Response) -> Result<Vec<u8>, CoordinatorError> {
    let mut buf = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(piece)) => {
                let room = MAX_COORDINATOR_BODY_BYTES - buf.len();
                if piece.len() >= room {
                    buf.extend_from_slice(&piece[..room]);
                    return Ok(buf);
                }
                buf.extend_from_slice(&piece);
            }
            Ok(None) => return Ok(buf),
            Err(e) => return Err(CoordinatorError::Transport(e.to_string())),
        }
    }
}

fn jittered(backoff: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    backoff + Duration::from_millis((nanos % 100) as u64)
}

/// Maps a landed non-2xx response to the right error class. A 5xx — or a
/// 408/429 — is the server transiently unable (a coordinator mid-deploy
/// behind a proxy answers 502/503/504), not a verdict on the request, so
/// it becomes [`CoordinatorError::Transport`], the class every caller
/// already holds the work durably and retries on. A 4xx is the
/// coordinator's considered rejection and stays
/// [`CoordinatorError::Protocol`], final.
fn classify_landed(status: reqwest::StatusCode, body: &str) -> CoordinatorError {
    let detail = format!(
        "{status}: {}",
        coordinator_reason(body)
            .chars()
            .take(500)
            .collect::<String>()
    );
    if status.is_server_error()
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
    {
        CoordinatorError::Transport(detail)
    } else {
        CoordinatorError::Protocol(detail)
    }
}

pub struct HttpCoordinatorClient {
    http: reqwest::Client,
    base_url: String,
    long_poll_timeout: Duration,
    max_retries: u32,
    session: Mutex<Option<String>>,
    coordinator_protocol: Mutex<Option<u32>>,
}

impl HttpCoordinatorClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_config(base_url, DEFAULT_LONG_POLL_TIMEOUT, DEFAULT_MAX_RETRIES)
    }

    pub fn with_config(
        base_url: impl Into<String>,
        long_poll_timeout: Duration,
        max_retries: u32,
    ) -> Self {
        // Every call declares this build's wire version, so a
        // coordinator that raised its floor past a breaking change can
        // refuse by name ("upgrade this node") instead of failing a
        // parse three layers deep.
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::HeaderName::from_static(PROTOCOL_VERSION_HEADER),
            reqwest::header::HeaderValue::from(PROTOCOL_VERSION),
        );
        let http = reqwest::Client::builder()
            .timeout(DEFAULT_CALL_TIMEOUT)
            .default_headers(headers)
            .build()
            .expect("reqwest client builds with default TLS backend");
        Self {
            http,
            // Strip a trailing slash so a pasted `https://host/` doesn't
            // build `//federation/...`, which the coordinator's router
            // (no path normalization) 404s — a 404 the register loop
            // retries forever, stranding the node while `status` (which
            // reads the URL through its own slash-stripping path) still
            // shows the coordinator reachable.
            base_url: base_url.into().trim_end_matches('/').to_string(),
            long_poll_timeout,
            max_retries,
            session: Mutex::new(None),
            coordinator_protocol: Mutex::new(None),
        }
    }

    /// The wire version the coordinator declared on the last register
    /// reply — `None` before the first register or against a
    /// coordinator that predates versioning.
    pub fn coordinator_protocol(&self) -> Option<u32> {
        *self.coordinator_protocol.lock()
    }

    /// Records the coordinator's declared wire version and tells the
    /// operator's log when the two sides skew — once per change, not
    /// per re-register.
    fn note_coordinator_protocol(&self, resp: &reqwest::Response) {
        let declared = resp
            .headers()
            .get(PROTOCOL_VERSION_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u32>().ok());
        let mut stored = self.coordinator_protocol.lock();
        if *stored == declared {
            return;
        }
        *stored = declared;
        match declared {
            Some(theirs) if theirs != PROTOCOL_VERSION => tracing::warn!(
                coordinator = theirs,
                node = PROTOCOL_VERSION,
                "coordinator speaks a different wire version — upgrade whichever side \
                 is older before the next breaking change strands this node"
            ),
            None => tracing::info!(
                "coordinator declared no wire version — it predates versioning; \
                 consider upgrading it"
            ),
            Some(_) => {}
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    fn with_session(&self, req: RequestBuilder) -> RequestBuilder {
        match self.session.lock().clone() {
            Some(token) => req.bearer_auth(token),
            None => req,
        }
    }

    /// Sends the request `build` constructs, retrying only transport
    /// failures (never a landed HTTP response) with capped, jittered
    /// exponential backoff.
    async fn send_with_retry(
        &self,
        build: impl Fn() -> RequestBuilder,
    ) -> Result<reqwest::Response, CoordinatorError> {
        let mut backoff = BASE_BACKOFF;
        let mut last_err = None;
        for attempt in 0..=self.max_retries {
            match build().send().await {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    last_err = Some(e.to_string());
                    if attempt == self.max_retries {
                        break;
                    }
                    tokio::time::sleep(jittered(backoff)).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
        Err(CoordinatorError::Transport(last_err.unwrap_or_else(|| {
            "transport failed with no error detail".into()
        })))
    }

    async fn body_or_protocol_error<T: serde::de::DeserializeOwned>(
        resp: reqwest::Response,
    ) -> Result<T, CoordinatorError> {
        let status = resp.status();
        let body = read_capped(resp).await?;
        if !status.is_success() {
            return Err(classify_landed(status, &String::from_utf8_lossy(&body)));
        }
        serde_json::from_slice(&body).map_err(|e| {
            // A 2xx we can't decode is a mangled or truncated success — a
            // proxy rewriting the body, a version skew — not the
            // coordinator's considered rejection, so it is Transport (the
            // retryable class), the same way classify_landed treats a
            // transient 5xx. This matters most for submit_result: the result
            // endpoint is idempotent (it echoes the final verdict on
            // redelivery), so a held outbox entry reads the real settlement
            // on the next retry instead of the node dropping earnings for
            // work the coordinator already released.
            CoordinatorError::Transport(format!("decode {}: {e}", std::any::type_name::<T>()))
        })
    }

    /// Fetches this operator's books — per-job amounts and payout
    /// confirmations — under a fresh signed read by `identity` (the
    /// books open only to the operator's own key). Each call signs its
    /// own timestamp: the reconcile loop runs for the node's whole
    /// lifetime, far past the read-auth skew window.
    pub async fn operator_jobs(
        &self,
        identity: &covenant_identity::LocalIdentity,
    ) -> Result<Vec<OperatorJobRow>, CoordinatorError> {
        let path = format!(
            "/federation/operators/{}/jobs",
            identity.agent_id().pubkey_base58()
        );
        let signed_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let signature = covenant_compute_protocol::sign_read(identity, &path, signed_at_ms)
            .map_err(|e| CoordinatorError::Protocol(format!("sign operator read: {e}")))?;
        let url = self.url(&path);
        let resp = self
            .send_with_retry(|| {
                self.http
                    .get(&url)
                    .header(
                        covenant_compute_protocol::READ_SIGNED_AT_HEADER,
                        signed_at_ms.to_string(),
                    )
                    .header(
                        covenant_compute_protocol::READ_SIGNATURE_HEADER,
                        signature.clone(),
                    )
            })
            .await?;
        Self::body_or_protocol_error(resp).await
    }
}

/// One row of the coordinator's operator books
/// (`GET /federation/operators/:operator/jobs`) — the payout
/// confirmation feed the earnings reconcile loop applies. This crate's
/// mirror of the coordinator's serializer, same idiom as the buyer
/// crate's wire rows; unknown fields are ignored so the coordinator
/// can grow the row.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct OperatorJobRow {
    pub job_id: uuid::Uuid,
    pub status: String,
    /// Why an unpaid row's money went back — `buyer_cancelled` never
    /// faults this operator's standing; the attributed reasons do.
    /// `None` on paid rows and on coordinators that predate the field.
    #[serde(default)]
    pub refund_reason: Option<String>,
    /// The buyer disputed this job's output after release. The money
    /// is never clawed back — this names WHICH job the standing's
    /// `disputed` count is about. `false` from coordinators that
    /// predate the field.
    #[serde(default)]
    pub disputed: bool,
    /// The buyer's signed complaint, verbatim. `None` on undisputed
    /// rows and on coordinators that predate the field.
    #[serde(default)]
    pub dispute_reason: Option<String>,
    pub price_micro_usdc: u64,
    pub fee_micro_usdc: u64,
    pub net_micro_usdc: u64,
    /// Whether the job's escrow was organic buyer revenue or a disclosed
    /// bootstrap subsidy — carried so a backfilled earnings row (see
    /// [`reconcile_paid_rows`](crate::earnings::reconcile_paid_rows))
    /// records the same tag a normal credit would. `Organic` from
    /// coordinators that predate the field.
    #[serde(default = "organic_funding")]
    pub funding_source: FundingSource,
    pub issued_at_ms: u64,
    pub payout: Option<PayoutConfirmation>,
}

fn organic_funding() -> FundingSource {
    FundingSource::Organic
}

/// The push that settled a row, as the coordinator recorded it.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PayoutConfirmation {
    pub amount_micro_usdc: u64,
    /// `None` when the coordinator's payout backend reported success
    /// without an on-chain transaction (a mock or off-chain backend).
    pub tx_signature: Option<String>,
    pub recorded_at_ms: u64,
}

#[async_trait]
impl Coordinator for HttpCoordinatorClient {
    async fn register(&self, req: RegisterRequest) -> Result<RegisterResponse, CoordinatorError> {
        let url = self.url("/federation/operators/register");
        let resp = self
            .send_with_retry(|| self.http.post(&url).json(&req))
            .await?;
        self.note_coordinator_protocol(&resp);
        let body: RegisterResponse = Self::body_or_protocol_error(resp).await?;
        if let Some(session) = &body.operator_session {
            *self.session.lock() = Some(session.clone());
        }
        Ok(body)
    }

    async fn heartbeat(
        &self,
        req: HeartbeatRequest,
    ) -> Result<HeartbeatResponse, CoordinatorError> {
        let url = self.url("/federation/operators/heartbeat");
        let resp = self
            .send_with_retry(|| self.with_session(self.http.post(&url).json(&req)))
            .await?;
        Self::body_or_protocol_error(resp).await
    }

    async fn poll_next_job(
        &self,
        operator: &AgentId,
    ) -> Result<Option<JobOffer>, CoordinatorError> {
        let url = self.url(&format!(
            "/federation/operators/{}/next-job",
            operator.pubkey_base58()
        ));
        let timeout = self.long_poll_timeout + DEFAULT_CALL_TIMEOUT;
        let resp = self
            .send_with_retry(|| self.with_session(self.http.get(&url)).timeout(timeout))
            .await?;
        Self::body_or_protocol_error(resp).await
    }

    async fn accept_job(&self, decision: JobAccept) -> Result<(), CoordinatorError> {
        let job_id = match &decision {
            JobAccept::Accept { job_id } | JobAccept::Reject { job_id, .. } => *job_id,
        };
        let url = self.url(&format!("/federation/jobs/{job_id}/accept"));
        let resp = self
            .send_with_retry(|| self.with_session(self.http.post(&url).json(&decision)))
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = read_capped(resp).await.unwrap_or_default();
            return Err(classify_landed(status, &String::from_utf8_lossy(&body)));
        }
        Ok(())
    }

    async fn submit_result(
        &self,
        result: JobResultMessage,
    ) -> Result<JobResultAck, CoordinatorError> {
        let job_id = result.receipt.receipt.job_id;
        let url = self.url(&format!("/federation/jobs/{job_id}/result"));
        let resp = self
            .send_with_retry(|| self.http.post(&url).json(&result))
            .await?;
        Self::body_or_protocol_error(resp).await
    }

    async fn push_stream(&self, push: StreamPush) -> Result<(), CoordinatorError> {
        let url = self.url(&format!("/federation/jobs/{}/stream", push.job_id));
        // Transport retries are safe here for the same reason they are
        // everywhere else in this client — and doubly so: the
        // coordinator ignores seqs it already holds, so a batch retried
        // after a half-landed push cannot duplicate.
        let resp = self
            .send_with_retry(|| self.with_session(self.http.post(&url).json(&push)))
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = read_capped(resp).await.unwrap_or_default();
            return Err(classify_landed(status, &String::from_utf8_lossy(&body)));
        }
        Ok(())
    }
}

/// Watches the coordinator's public lease view for the buyer's close.
/// Read-only and unauthenticated by design: it asks one question about
/// a job the node is already serving, and the answer is a boolean the
/// buyer set. A failed poll answers "not closed" — the window is still
/// the backstop, and dropping a session because a health check blipped
/// would cost the operator a job they were serving correctly.
#[async_trait]
impl crate::lease::LeaseCloseSource for HttpCoordinatorClient {
    async fn is_closed(&self, job_id: uuid::Uuid) -> bool {
        let url = self.url(&format!("/federation/jobs/{job_id}/lease"));
        let Ok(resp) = self.http.get(&url).send().await else {
            return false;
        };
        if !resp.status().is_success() {
            return false;
        }
        let Ok(body) = read_capped(resp).await else {
            return false;
        };
        serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("close_requested").and_then(|c| c.as_bool()))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post;
    use axum::{Json, Router};
    use covenant_compute_protocol::{
        CapabilityProfile, HardwareClass, JobKind, PriceAsk, PriceUnit,
    };
    use covenant_identity::LocalIdentity;

    async fn spawn(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn profile(identity: &LocalIdentity) -> CapabilityProfile {
        CapabilityProfile {
            operator: identity.agent_id(),
            hardware: HardwareClass::CpuOnly,
            vram_gb: 0,
            models_served: vec!["any".into()],
            job_kinds: vec![JobKind::BatchJob],
            price: PriceAsk {
                unit: PriceUnit::PerJob,
                micro_usdc: 1,
            },
            tee_capable: false,
        }
    }

    #[tokio::test]
    async fn register_happy_path_stores_the_session() {
        async fn handler(Json(_req): Json<RegisterRequest>) -> Json<RegisterResponse> {
            Json(RegisterResponse {
                accepted: true,
                operator_session: Some("sess-123".into()),
                reason: None,
                fee_bps: 0,
            })
        }
        let base =
            spawn(Router::new().route("/federation/operators/register", post(handler))).await;
        let client = HttpCoordinatorClient::new(base);
        let identity = LocalIdentity::generate("operator@local");
        let req = RegisterRequest::sign(profile(&identity), "payout".into(), &identity).unwrap();

        let resp = client.register(req).await.unwrap();
        assert!(resp.accepted);
        assert_eq!(client.session.lock().as_deref(), Some("sess-123"));
    }

    #[tokio::test]
    async fn an_oversized_coordinator_body_is_capped_not_read_whole() {
        // A hostile or buggy coordinator answers 200 with a body past the
        // ceiling. The client stops at MAX_COORDINATOR_BODY_BYTES and surfaces
        // a retryable transport error (the truncated body can't decode) rather
        // than buffering the whole stream into node memory.
        async fn flood() -> String {
            "a".repeat(MAX_COORDINATOR_BODY_BYTES + 1024)
        }
        let base = spawn(Router::new().fallback(flood)).await;
        let client = HttpCoordinatorClient::new(base);
        let identity = LocalIdentity::generate("operator@local");
        let err = client
            .poll_next_job(&identity.agent_id())
            .await
            .expect_err("an oversized body is refused, not buffered whole");
        assert!(
            matches!(err, CoordinatorError::Transport(_)),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn non_success_status_maps_to_protocol_error() {
        // The coordinator answers a refusal in its `ApiError` shape; the
        // serve loop's log line must read the reason, not the JSON
        // envelope around it.
        async fn handler() -> (axum::http::StatusCode, &'static str) {
            (
                axum::http::StatusCode::BAD_REQUEST,
                r#"{"error":"bad signature"}"#,
            )
        }
        let base =
            spawn(Router::new().route("/federation/operators/register", post(handler))).await;
        let client = HttpCoordinatorClient::new(base);
        let identity = LocalIdentity::generate("operator@local");
        let req = RegisterRequest::sign(profile(&identity), "payout".into(), &identity).unwrap();

        let err = client.register(req).await.unwrap_err();
        let CoordinatorError::Protocol(m) = err else {
            panic!("a 4xx is a protocol rejection, not a transport error: {err:?}");
        };
        assert!(m.contains("bad signature"), "names the reason: {m}");
        assert!(!m.contains(r#"{"error""#), "drops the JSON envelope: {m}");
    }

    #[test]
    fn a_landed_5xx_or_throttle_is_transport_a_4xx_stays_protocol() {
        use reqwest::StatusCode;
        // Transient: the server is up but can't answer right now (a
        // coordinator mid-deploy behind a proxy). The caller must hold
        // the work and retry, not treat it as the coordinator's verdict.
        for status in [
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::GATEWAY_TIMEOUT,
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_MANY_REQUESTS,
        ] {
            assert!(
                matches!(
                    classify_landed(status, "{}"),
                    CoordinatorError::Transport(_)
                ),
                "{status} is transient"
            );
        }
        // Final: the coordinator considered the request and rejected it —
        // including the 404 that tells a heartbeat to re-register.
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::NOT_FOUND,
            StatusCode::CONFLICT,
        ] {
            assert!(
                matches!(classify_landed(status, "{}"), CoordinatorError::Protocol(_)),
                "{status} is final"
            );
        }
    }

    #[test]
    fn a_trailing_slash_on_the_base_url_does_not_double_up_the_path() {
        // A pasted `https://host/` must not build `//federation/...`,
        // which the coordinator's un-normalized router 404s and the
        // register loop then retries forever.
        for base in [
            "http://coord.test",
            "http://coord.test/",
            "http://coord.test///",
        ] {
            let client = HttpCoordinatorClient::new(base);
            assert_eq!(
                client.url("/federation/operators/register"),
                "http://coord.test/federation/operators/register",
            );
        }
    }

    #[tokio::test]
    async fn a_server_error_on_submit_surfaces_as_transport_so_the_outbox_holds_the_result() {
        // A coordinator rolling a deploy answers 503 behind its proxy.
        // The node must read that as "no answer yet" (Transport) — the
        // class `execute_accepted` queues the finished receipt on — not a
        // final rejection that would drop the operator's unpaid work.
        async fn handler() -> (axum::http::StatusCode, &'static str) {
            (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "backend starting",
            )
        }
        let base =
            spawn(Router::new().route("/federation/operators/register", post(handler))).await;
        let client = HttpCoordinatorClient::new(base);
        let identity = LocalIdentity::generate("operator@local");
        let req = RegisterRequest::sign(profile(&identity), "payout".into(), &identity).unwrap();
        let err = client.register(req).await.unwrap_err();
        assert!(
            matches!(err, CoordinatorError::Transport(_)),
            "a 5xx is transient, not the coordinator's final word: {err:?}"
        );
    }

    #[test]
    fn operator_job_rows_parse_across_coordinator_vintages() {
        // A coordinator that predates the dispute fields: the row
        // still parses, undisputed.
        let old: OperatorJobRow = serde_json::from_value(serde_json::json!({
            "job_id": "6fa459ea-ee8a-3ca4-894e-db77e160355e",
            "status": "completed",
            "price_micro_usdc": 1_000,
            "fee_micro_usdc": 100,
            "net_micro_usdc": 900,
            "issued_at_ms": 1,
            "payout": null,
        }))
        .unwrap();
        assert!(!old.disputed);
        assert_eq!(old.dispute_reason, None);

        // A current coordinator's disputed row carries the complaint
        // through verbatim.
        let disputed: OperatorJobRow = serde_json::from_value(serde_json::json!({
            "job_id": "6fa459ea-ee8a-3ca4-894e-db77e160355e",
            "status": "completed",
            "disputed": true,
            "dispute_reason": "output was unrelated to the prompt",
            "price_micro_usdc": 1_000,
            "fee_micro_usdc": 100,
            "net_micro_usdc": 900,
            "issued_at_ms": 1,
            "payout": null,
        }))
        .unwrap();
        assert!(disputed.disputed);
        assert_eq!(
            disputed.dispute_reason.as_deref(),
            Some("output was unrelated to the prompt")
        );
    }

    #[tokio::test]
    async fn poll_next_job_decodes_a_null_body_as_none() {
        async fn handler() -> Json<Option<JobOffer>> {
            Json(None)
        }
        let base = spawn(Router::new().route(
            "/federation/operators/:operator/next-job",
            axum::routing::get(handler),
        ))
        .await;
        let client = HttpCoordinatorClient::with_config(base, Duration::from_millis(200), 1);
        let operator = LocalIdentity::generate("operator@local").agent_id();
        assert!(client.poll_next_job(&operator).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_2xx_with_an_undecodable_body_is_transport_not_protocol() {
        // A proxy or a version skew can hand back a success whose body won't
        // decode. That is a retryable transport anomaly, not the
        // coordinator's considered rejection, so the caller holds the work
        // and redelivers (submit_result rides an idempotent endpoint that
        // answers the retry with the real verdict) instead of dropping it as
        // a final protocol refusal.
        async fn handler() -> (axum::http::StatusCode, &'static str) {
            (axum::http::StatusCode::OK, "not a JobOffer")
        }
        let base = spawn(Router::new().route(
            "/federation/operators/:operator/next-job",
            axum::routing::get(handler),
        ))
        .await;
        let client = HttpCoordinatorClient::with_config(base, Duration::from_millis(200), 1);
        let operator = LocalIdentity::generate("operator@local").agent_id();
        let err = client.poll_next_job(&operator).await.unwrap_err();
        assert!(
            matches!(err, CoordinatorError::Transport(_)),
            "a 2xx with an undecodable body must be retryable transport, not a final drop; got {err}"
        );
    }

    #[tokio::test]
    async fn no_listener_exhausts_retries_as_transport_error() {
        // Nothing bound on this port: every attempt fails at connect time.
        let client =
            HttpCoordinatorClient::with_config("http://127.0.0.1:1", Duration::from_secs(1), 1);
        let identity = LocalIdentity::generate("operator@local");
        let req = RegisterRequest::sign(profile(&identity), "payout".into(), &identity).unwrap();
        let err = client.register(req).await.unwrap_err();
        assert!(matches!(err, CoordinatorError::Transport(_)));
    }

    #[tokio::test]
    async fn register_declares_the_wire_version_and_reads_the_coordinators() {
        // The hermetic coordinator echoes whether the node's request
        // declared the current wire version, and answers as a NEWER
        // build (version 7) — the exact skew the operator's log must
        // name before a breaking change strands the node.
        async fn handler(
            headers: axum::http::HeaderMap,
            Json(_req): Json<RegisterRequest>,
        ) -> impl axum::response::IntoResponse {
            let declared_current = headers
                .get(PROTOCOL_VERSION_HEADER)
                .and_then(|v| v.to_str().ok())
                == Some(PROTOCOL_VERSION.to_string().as_str());
            (
                [(PROTOCOL_VERSION_HEADER, "7")],
                Json(RegisterResponse {
                    accepted: declared_current,
                    operator_session: None,
                    reason: None,
                    fee_bps: 0,
                }),
            )
        }
        let base =
            spawn(Router::new().route("/federation/operators/register", post(handler))).await;
        let client = HttpCoordinatorClient::new(base);
        assert_eq!(client.coordinator_protocol(), None, "nothing read yet");

        let identity = LocalIdentity::generate("operator@local");
        let req = RegisterRequest::sign(profile(&identity), "payout".into(), &identity).unwrap();
        let resp = client.register(req).await.unwrap();
        assert!(
            resp.accepted,
            "every request must declare this build's wire version"
        );
        assert_eq!(client.coordinator_protocol(), Some(7));
    }

    #[tokio::test]
    async fn accept_job_posts_the_decision_to_the_right_job_path() {
        async fn handler(Json(decision): Json<JobAccept>) -> axum::http::StatusCode {
            match decision {
                JobAccept::Accept { .. } => axum::http::StatusCode::OK,
                JobAccept::Reject { .. } => axum::http::StatusCode::OK,
            }
        }
        let job_id = uuid::Uuid::new_v4();
        let base =
            spawn(Router::new().route(&format!("/federation/jobs/{job_id}/accept"), post(handler)))
                .await;
        let client = HttpCoordinatorClient::new(base);
        client
            .accept_job(JobAccept::Accept { job_id })
            .await
            .unwrap();
    }
}
