//! Read-only REST client for FairScale's reputation, agent-trust, and credit
//! reads. Reputation lives on one host, agent + credit on another. Auth is an
//! optional `fairkey` header (free tier) or an x402 payer: on a 402 the client
//! settles the quoted amount (USDC on Solana mainnet, bounded by a per-read
//! cap) and retries once with the envelope under both `x-payment` (x402
//! standard) and `payment-signature` (FairScale's documented name). With
//! neither key nor payer, a keyless
//! read surfaces the 401/402 as one clear error.
//!
//! Response bodies vary in shape across FairScale's products and their own spec
//! carries a score-scale inconsistency (0-100 vs ~0-1000 across endpoints), so
//! each type keeps the raw JSON and exposes typed accessors for the fields we
//! use rather than a brittle full deserialize.

use std::sync::Arc;
use std::time::Duration;

use covenant_x402::{PaymentExtra, PaymentRequirements, Signer};
use serde::Deserialize;
use serde_json::Value;

use crate::{FairScaleError, Result};

const MAX_ATTEMPTS: u32 = 3;

/// CAIP-2 network FairScale settles on, from the live 402 challenge.
pub const SOLANA_MAINNET_CAIP2: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";

/// USDC mint on Solana mainnet, the only asset FairScale quotes.
pub const USDC_MINT_SOLANA: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

#[derive(Clone)]
pub struct FairScaleClient {
    http: reqwest::Client,
    reputation_base: String,
    agent_base: String,
    fairkey: Option<String>,
    payer: Option<Arc<dyn Signer>>,
    read_cap_atomic: u64,
    credit_cap_atomic: u64,
}

/// A reputation `/score` read. Scale is 0-100 for `fairscore`.
#[derive(Debug, Clone)]
pub struct FairScore {
    pub raw: Value,
}

/// An agent `/v1/trust-gate` read: an allow/deny decision plus its basis.
#[derive(Debug, Clone, Deserialize)]
pub struct TrustGate {
    #[serde(default)]
    pub decision: String,
    #[serde(default)]
    pub recommendation: String,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default)]
    pub verification: Value,
}

/// A `/v1/credit` underwriting read. Advisory only; FairScale moves no funds.
#[derive(Debug, Clone)]
pub struct CreditAssessment {
    pub raw: Value,
}

impl FairScaleClient {
    /// # Panics
    ///
    /// If reqwest cannot initialize its TLS backend. That fault is
    /// startup-only and unrecoverable, so construction panics rather than
    /// returning an error nothing can handle.
    pub fn new(reputation_base: impl Into<String>, agent_base: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .build()
            .expect("build fairscale http client");
        Self {
            http,
            reputation_base: reputation_base.into().trim_end_matches('/').to_string(),
            agent_base: agent_base.into().trim_end_matches('/').to_string(),
            fairkey: None,
            payer: None,
            read_cap_atomic: 0,
            credit_cap_atomic: 0,
        }
    }

    /// Attach an x402 payer. On a 402 the client settles the quoted amount and
    /// retries once. `read_cap_atomic` bounds score and trust-gate reads,
    /// `credit_cap_atomic` the credit read; both are atomic USDC (6 decimals).
    /// A quote above the cap fails the read before anything is signed.
    pub fn with_payer(
        mut self,
        payer: Arc<dyn Signer>,
        read_cap_atomic: u64,
        credit_cap_atomic: u64,
    ) -> Self {
        self.payer = Some(payer);
        self.read_cap_atomic = read_cap_atomic;
        self.credit_cap_atomic = credit_cap_atomic;
        self
    }

    /// Attach a FairScale API key, sent as the `fairkey` header. Without one,
    /// reads fail: 401 on the reputation host, 402 on the agent host.
    pub fn with_fairkey(mut self, key: impl Into<String>) -> Self {
        let key = key.into();
        self.fairkey = if key.trim().is_empty() {
            None
        } else {
            Some(key)
        };
        self
    }

    /// Reputation FairScore for a wallet. Keyed tier: `GET {reputation}/score`.
    /// With a payer the read targets `GET {agent}/v1/score` instead: FairScale
    /// serves the score behind x402 on the agent host, and the reputation host
    /// 401s keyless reads (both observed live 2026-07-20).
    pub async fn score(&self, wallet: &str) -> Result<FairScore> {
        let url = if self.payer.is_some() {
            build_url(&self.agent_base, "/v1/score", &[("wallet", wallet)])?
        } else {
            build_url(&self.reputation_base, "/score", &[("wallet", wallet)])?
        };
        Ok(FairScore {
            raw: self.get(url.as_str(), self.read_cap_atomic).await?,
        })
    }

    /// Agent allow/deny gate. `GET {agent}/v1/trust-gate?wallet=&min_score=`.
    pub async fn trust_gate(&self, wallet: &str, min_score: u32) -> Result<TrustGate> {
        let min_score = min_score.to_string();
        let url = build_url(
            &self.agent_base,
            "/v1/trust-gate",
            &[("wallet", wallet), ("min_score", &min_score)],
        )?;
        let raw = self.get(url.as_str(), self.read_cap_atomic).await?;
        serde_json::from_value(raw).map_err(|e| FairScaleError::Decode(format!("trust-gate: {e}")))
    }

    /// Credit underwriting read. `GET {agent}/v1/credit?wallet=&amount=`.
    pub async fn credit(&self, wallet: &str, amount_usd: u64) -> Result<CreditAssessment> {
        let amount_usd = amount_usd.to_string();
        let url = build_url(
            &self.agent_base,
            "/v1/credit",
            &[("wallet", wallet), ("amount", &amount_usd)],
        )?;
        Ok(CreditAssessment {
            raw: self.get(url.as_str(), self.credit_cap_atomic).await?,
        })
    }

    /// GET with the `fairkey` header when set, a bounded retry on a transient
    /// or cold-start failure, x402 settlement on a 402 when a payer is wired,
    /// and a clear message when a keyless read is rejected.
    async fn get(&self, url: &str, cap_atomic: u64) -> Result<Value> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let mut req = self.http.get(url);
            if let Some(key) = &self.fairkey {
                req = req.header("fairkey", key);
            }
            match req.send().await {
                Ok(resp) => {
                    if attempt < MAX_ATTEMPTS && is_retryable_status(resp.status()) {
                        tokio::time::sleep(backoff(attempt)).await;
                        continue;
                    }
                    if resp.status().as_u16() == 402 {
                        if let Some(payer) = self.payer.clone() {
                            return self.pay_and_retry(url, resp, cap_atomic, payer).await;
                        }
                    }
                    return self.read(resp).await;
                }
                Err(e) if attempt < MAX_ATTEMPTS && (e.is_timeout() || e.is_connect()) => {
                    tokio::time::sleep(backoff(attempt)).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Settle a 402 challenge and retry the read once. FairScale returns an
    /// x402 v2 envelope in the JSON body, `{"error", "accepts": [...],
    /// "resource"}` (mirrored base64 in a `payment-required` header; the body
    /// is what this parses, shape captured live 2026-07-20). Pays at most
    /// `cap_atomic`, never signs a zero quote, and never pays twice: a 401/402
    /// on the paid retry is surfaced, not re-settled.
    async fn pay_and_retry(
        &self,
        url: &str,
        challenge_resp: reqwest::Response,
        cap_atomic: u64,
        payer: Arc<dyn Signer>,
    ) -> Result<Value> {
        let body = read_body_capped(challenge_resp, MAX_RESPONSE_BYTES).await?;
        let challenge: Challenge = serde_json::from_str(&body).map_err(|e| {
            FairScaleError::Payment(format!(
                "undecodable 402 challenge: {e}; body: {}",
                truncate(&body)
            ))
        })?;
        let option = challenge
            .accepts
            .iter()
            .find(|o| {
                o.scheme == "exact"
                    && o.network == SOLANA_MAINNET_CAIP2
                    && o.asset == USDC_MINT_SOLANA
            })
            .ok_or_else(|| {
                FairScaleError::Payment(format!(
                    "no exact/USDC/Solana-mainnet option among {} offered",
                    challenge.accepts.len()
                ))
            })?;
        // x402's canonical quote field is `maxAmountRequired`; FairScale also
        // mirrors it as `amount` today. Prefer the standard field so the pay
        // path survives the mirror being dropped.
        let quoted = if option.max_amount_required.is_empty() {
            &option.amount
        } else {
            &option.max_amount_required
        };
        let amount: u64 = quoted.parse().map_err(|_| {
            FairScaleError::Payment(format!("unparseable challenge amount {quoted:?}"))
        })?;
        if amount == 0 {
            return Err(FairScaleError::Payment(
                "zero-amount challenge; a zero quote is malformed, not a free call".into(),
            ));
        }
        if amount > cap_atomic {
            return Err(FairScaleError::Payment(format!(
                "quoted {amount} atomic USDC exceeds the per-read cap {cap_atomic}"
            )));
        }
        // Display-only: the transfer and the cap check both run on the atomic
        // `amount`. Clamped so a hostile `decimals` cannot inf/NaN the float.
        let decimals = option
            .extra
            .as_ref()
            .and_then(|e| e.decimals)
            .unwrap_or(6)
            .min(12);
        let requirements = PaymentRequirements {
            network: option.network.clone(),
            asset: option.asset.clone(),
            amount: quoted.clone(),
            amount_usdc: amount as f64 / 10f64.powi(decimals as i32),
            pay_to: option.pay_to.clone(),
            scheme: option.scheme.clone(),
            extra: option.extra.as_ref().map(|e| PaymentExtra {
                fee_payer: e.fee_payer.clone(),
                name: None,
                version: None,
            }),
        };
        let header = payer
            .build_payment(&requirements)
            .await
            .map_err(|e| FairScaleError::Payment(format!("build payment: {e}")))?;
        tracing::debug!(
            url,
            amount,
            "fairscale 402 settled; retrying with payment headers"
        );
        // FairScale reads the envelope from `payment-signature` (their docs)
        // and silently ignores the x402-standard `x-payment`; send both so we
        // work against FairScale today and any standard-compliant wall later.
        let mut req = self
            .http
            .get(url)
            .header("x-payment", header.clone())
            .header("payment-signature", header);
        if let Some(key) = &self.fairkey {
            req = req.header("fairkey", key);
        }
        let resp = req.send().await?;
        if matches!(resp.status().as_u16(), 401 | 402) {
            let status = resp.status().as_u16();
            let body = read_body_capped(resp, MAX_RESPONSE_BYTES)
                .await
                .unwrap_or_default();
            // An "insufficient funds" simulation failure means the funder's
            // token account can no longer cover the quoted amount; say so
            // plainly instead of only echoing truncated simulator logs.
            let hint = if body.to_ascii_lowercase().contains("insufficient") {
                format!(
                    " (funder token balance below the required {amount} base units — top up the funding wallet)"
                )
            } else {
                String::new()
            };
            return Err(FairScaleError::Payment(format!(
                "read still rejected ({status}) after payment{hint}: {}",
                truncate(&body)
            )));
        }
        // Money left the funding wallet and a signal came back; keep that
        // visible to operators without debug logging.
        tracing::info!(url, amount, "fairscale x402 read settled");
        self.read(resp).await
    }

    async fn read(&self, resp: reqwest::Response) -> Result<Value> {
        let status = resp.status();
        let body = read_body_capped(resp, MAX_RESPONSE_BYTES).await?;
        // The docs promise a 402 on no-auth; live, the reputation host 401s
        // and the agent host issues an x402 challenge. With a payer wired the
        // 402 was already settled in `pay_and_retry`; reaching here keyless,
        // both statuses are the same actionable condition.
        if matches!(status.as_u16(), 401 | 402) {
            return Err(FairScaleError::Api {
                status: status.as_u16(),
                message: "FairScale rejected the read as unauthenticated: set a fairkey or pay \
                          over x402"
                    .into(),
            });
        }
        if !status.is_success() {
            return Err(FairScaleError::Api {
                status: status.as_u16(),
                message: truncate(&body),
            });
        }
        serde_json::from_str(&body)
            .map_err(|e| FairScaleError::Decode(format!("{e}; body: {}", truncate(&body))))
    }
}

impl FairScore {
    /// The composite `fairscore` (0-100), if present.
    pub fn fairscore(&self) -> Option<f64> {
        self.raw.get("fairscore").and_then(Value::as_f64)
    }

    /// The reputation tier (bronze/silver/gold/platinum/diamond), if present.
    pub fn tier(&self) -> Option<&str> {
        self.raw.get("tier").and_then(Value::as_str)
    }
}

impl CreditAssessment {
    pub fn credit_score(&self) -> Option<f64> {
        self.raw.get("credit_score").and_then(Value::as_f64)
    }

    pub fn risk_band(&self) -> Option<&str> {
        self.raw.get("risk_band").and_then(Value::as_str)
    }

    /// FairScale nests the terms under `underwriting`, not at the top level.
    pub fn lending_terms(&self) -> Option<&Value> {
        self.raw
            .get("underwriting")
            .and_then(|u| u.get("lending_terms"))
    }
}

/// FairScale's 402 challenge body: an x402 v2 envelope. Only the fields the
/// pay path needs are typed; anything else FairScale sends is ignored.
#[derive(Debug, Deserialize)]
struct Challenge {
    #[serde(default)]
    accepts: Vec<ChallengeOption>,
}

#[derive(Debug, Deserialize)]
struct ChallengeOption {
    #[serde(default)]
    scheme: String,
    #[serde(default)]
    network: String,
    #[serde(default)]
    amount: String,
    #[serde(rename = "maxAmountRequired", default)]
    max_amount_required: String,
    #[serde(default)]
    asset: String,
    #[serde(rename = "payTo", default)]
    pay_to: String,
    #[serde(default)]
    extra: Option<ChallengeExtra>,
}

#[derive(Debug, Deserialize)]
struct ChallengeExtra {
    #[serde(rename = "feePayer", default)]
    fee_payer: Option<String>,
    #[serde(default)]
    decimals: Option<u32>,
}

/// The gateway 5xxs a serverless cold start throws. A 429 is deliberately not
/// retried: the free tier meters 10 req/min, so a sub-second re-hit cannot
/// clear the window and only burns attempts. FairScale prescribes
/// backoff-and-retry, but at minute granularity; for a soft signal, surfacing
/// beats stalling.
fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 502..=504)
}

fn backoff(attempt: u32) -> Duration {
    // Saturating so a future MAX_ATTEMPTS bump cannot overflow the shift.
    Duration::from_millis(200u64.saturating_mul(1u64 << (attempt - 1).min(20)))
}

/// Query values are percent-encoded by `Url`'s serializer, so a caller-supplied
/// wallet cannot smuggle extra parameters or path segments into the request.
/// The tool layer already validates base58; this holds for direct client users.
fn build_url(base: &str, path: &str, query: &[(&str, &str)]) -> Result<reqwest::Url> {
    reqwest::Url::parse_with_params(&format!("{base}{path}"), query)
        .map_err(|e| FairScaleError::Decode(format!("build request url {base}{path}: {e}")))
}

/// Maximum response body buffered into memory. The remote answering a
/// (possibly paid) read controls its response, so an unbounded `.text()` lets
/// a hostile or compromised host exhaust a worker (the same guard covenantd
/// applies to paid-call bodies). 16 MiB sits far above any real FairScale
/// response while stopping a runaway stream.
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Read a body into a string, refusing anything past `max`. The Content-Length
/// check rejects an oversized declared body before it streams; the running
/// accumulation check is the real guard, since the header is optional and
/// remote-controlled.
async fn read_body_capped(mut resp: reqwest::Response, max: usize) -> Result<String> {
    if let Some(len) = resp.content_length() {
        if len > max as u64 {
            return Err(FairScaleError::ResponseTooLarge(len));
        }
    }
    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if buf.len() + chunk.len() > max {
            return Err(FairScaleError::ResponseTooLarge(
                (buf.len() + chunk.len()) as u64,
            ));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn truncate(s: &str) -> String {
    const MAX: usize = 500;
    let cut: String = s.chars().take(MAX).collect();
    if cut.len() < s.len() {
        format!("{cut}…")
    } else {
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn score_read_sends_fairkey_and_types_the_score() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/score"))
            .and(query_param(
                "wallet",
                "EnteGjokMnFqTDcZSBitXDQEctMCnqV33HbPKw2LnDCg",
            ))
            .and(header("fairkey", "zpka_test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "wallet": "EnteGjokMnFqTDcZSBitXDQEctMCnqV33HbPKw2LnDCg",
                "fairscore": 65.3, "fairscore_base": 58.1, "tier": "gold",
                "badges": ["diamond_hands"]
            })))
            .mount(&server)
            .await;
        let client = FairScaleClient::new(server.uri(), server.uri()).with_fairkey("zpka_test");
        let s = client
            .score("EnteGjokMnFqTDcZSBitXDQEctMCnqV33HbPKw2LnDCg")
            .await
            .unwrap();
        assert_eq!(s.fairscore(), Some(65.3));
        assert_eq!(s.tier(), Some("gold"));
    }

    #[tokio::test]
    async fn trust_gate_types_the_decision() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/trust-gate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "wallet": "x", "decision": "deny", "fairscore": 24, "tier": "bronze",
                "recommendation": "unverified", "reasons": ["score_below_threshold: 24 < 60"],
                "verification": { "said": false, "erc8004": false, "sati": true }
            })))
            .mount(&server)
            .await;
        let client = FairScaleClient::new(server.uri(), server.uri());
        let g = client.trust_gate("wallet", 60).await.unwrap();
        assert_eq!(g.decision, "deny");
        assert_eq!(g.recommendation, "unverified");
        assert_eq!(g.verification["sati"], json!(true));
    }

    #[tokio::test]
    async fn unauthenticated_401_and_402_read_as_pay_over_x402() {
        for code in [401u16, 402] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/score"))
                .respond_with(ResponseTemplate::new(code))
                .mount(&server)
                .await;
            let client = FairScaleClient::new(server.uri(), server.uri());
            let err = client.score("wallet").await.unwrap_err();
            match err {
                FairScaleError::Api { status, message } => {
                    assert_eq!(status, code);
                    assert!(message.contains("fairkey"), "{message}");
                }
                other => panic!("expected api error, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn retries_a_cold_start_5xx_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/score"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/score"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "fairscore": 40.0 })))
            .mount(&server)
            .await;
        let client = FairScaleClient::new(server.uri(), server.uri());
        let s = client.score("wallet").await.unwrap();
        assert_eq!(s.fairscore(), Some(40.0));
    }

    #[tokio::test]
    async fn a_429_is_surfaced_immediately_not_retried() {
        // The free tier meters requests per minute; a sub-second retry cannot
        // clear the window, so the client must surface the 429 on the spot.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/score"))
            .respond_with(ResponseTemplate::new(429))
            .expect(1)
            .mount(&server)
            .await;
        let client = FairScaleClient::new(server.uri(), server.uri());
        let err = client.score("wallet").await.unwrap_err();
        match err {
            FairScaleError::Api { status, .. } => assert_eq!(status, 429),
            other => panic!("expected api error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn credit_reads_lending_terms_from_underwriting() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/credit"))
            .and(query_param("amount", "1000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "wallet": "x", "credit_score": 78.0, "risk_band": "prime",
                "underwriting": {
                    "opinion": "approve",
                    "lending_terms": { "max_credit_line": 2500, "suggested_apr_range": "8-12%" }
                }
            })))
            .mount(&server)
            .await;
        let client = FairScaleClient::new(server.uri(), server.uri());
        let c = client.credit("wallet", 1000).await.unwrap();
        assert_eq!(c.credit_score(), Some(78.0));
        assert_eq!(c.risk_band(), Some("prime"));
        assert_eq!(c.lending_terms().unwrap()["max_credit_line"], json!(2500));
    }

    /// The live challenge shape, captured from agent-api.fairscale.xyz on
    /// 2026-07-20. Amount parameterized; everything else verbatim.
    fn live_envelope(amount: &str) -> serde_json::Value {
        json!({
            "error": "Payment required",
            "accepts": [{
                "scheme": "exact",
                "network": "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
                "amount": amount,
                "maxAmountRequired": amount,
                "asset": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                "payTo": "fairAUEuR1SCcHL254Vb3F3XpUWLruJ2a11f6QfANEN",
                "maxTimeoutSeconds": 60,
                "extra": { "feePayer": "DeXterR2kQm8AvRHnNPatWkE46TfAcMeBDjb6FySoAb8", "decimals": 6 }
            }],
            "resource": { "url": "http://x/v1/score", "mimeType": "application/json" }
        })
    }

    #[tokio::test]
    async fn keyless_402_pays_and_retries_with_x_payment() {
        let agent = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/score"))
            .respond_with(ResponseTemplate::new(402).set_body_json(live_envelope("5000")))
            .up_to_n_times(1)
            .mount(&agent)
            .await;
        // The paid retry must carry the exact envelope the signer built from
        // the chosen option, under BOTH the x402-standard `x-payment` and
        // FairScale's documented `payment-signature`; matching on both pins
        // the dual-header behavior and the network + amount encoding.
        Mock::given(method("GET"))
            .and(path("/v1/score"))
            .and(header(
                "x-payment",
                "mock:solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp:5000",
            ))
            .and(header(
                "payment-signature",
                "mock:solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp:5000",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "fairscore": 61.0, "tier": "gold"
            })))
            .mount(&agent)
            .await;
        // Unroutable reputation host: proves a paying score read targets the
        // agent host and never touches the reputation one.
        let client = FairScaleClient::new("http://127.0.0.1:1", agent.uri()).with_payer(
            std::sync::Arc::new(covenant_x402::MockSigner),
            10_000,
            600_000,
        );
        let s = client.score("wallet").await.unwrap();
        assert_eq!(s.fairscore(), Some(61.0));
        assert_eq!(s.tier(), Some("gold"));
    }

    #[tokio::test]
    async fn over_cap_quote_fails_before_signing() {
        let agent = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/score"))
            .respond_with(ResponseTemplate::new(402).set_body_json(live_envelope("500000")))
            .mount(&agent)
            .await;
        let client = FairScaleClient::new("http://127.0.0.1:1", agent.uri()).with_payer(
            std::sync::Arc::new(covenant_x402::MockSigner),
            10_000,
            600_000,
        );
        let err = client.score("wallet").await.unwrap_err();
        match err {
            FairScaleError::Payment(msg) => assert!(msg.contains("cap"), "{msg}"),
            other => panic!("expected payment error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn credit_read_pays_under_its_own_cap() {
        let agent = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/credit"))
            .respond_with(ResponseTemplate::new(402).set_body_json(live_envelope("500000")))
            .up_to_n_times(1)
            .mount(&agent)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/credit"))
            .and(header(
                "x-payment",
                "mock:solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp:500000",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "credit_score": 78.0, "risk_band": "prime"
            })))
            .mount(&agent)
            .await;
        // 500000 busts the 10k read cap but fits the 600k credit cap; a pass
        // proves the caps are plumbed per endpoint, not shared.
        let client = FairScaleClient::new("http://127.0.0.1:1", agent.uri()).with_payer(
            std::sync::Arc::new(covenant_x402::MockSigner),
            10_000,
            600_000,
        );
        let c = client.credit("wallet", 1000).await.unwrap();
        assert_eq!(c.credit_score(), Some(78.0));
    }

    #[tokio::test]
    async fn paid_retry_still_402_is_surfaced_not_repaid() {
        let agent = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/score"))
            .respond_with(ResponseTemplate::new(402).set_body_json(live_envelope("5000")))
            .mount(&agent)
            .await;
        let client = FairScaleClient::new("http://127.0.0.1:1", agent.uri()).with_payer(
            std::sync::Arc::new(covenant_x402::MockSigner),
            10_000,
            600_000,
        );
        let err = client.score("wallet").await.unwrap_err();
        match err {
            FairScaleError::Payment(msg) => assert!(msg.contains("after payment"), "{msg}"),
            other => panic!("expected payment error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn undecodable_challenge_is_a_payment_error() {
        let agent = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/score"))
            .respond_with(ResponseTemplate::new(402).set_body_string("upstream oops"))
            .mount(&agent)
            .await;
        let client = FairScaleClient::new("http://127.0.0.1:1", agent.uri()).with_payer(
            std::sync::Arc::new(covenant_x402::MockSigner),
            10_000,
            600_000,
        );
        let err = client.score("wallet").await.unwrap_err();
        match err {
            FairScaleError::Payment(msg) => assert!(msg.contains("undecodable"), "{msg}"),
            other => panic!("expected payment error, got {other:?}"),
        }
    }

    #[test]
    fn truncate_is_char_safe_on_multibyte() {
        let multibyte = "é".repeat(600);
        assert_eq!(truncate(&multibyte).chars().count(), 501);
        let ascii = "a".repeat(600);
        assert_eq!(truncate(&ascii).chars().count(), 501);
        assert_eq!(truncate("short"), "short");
    }

    #[tokio::test]
    async fn challenge_with_only_max_amount_required_still_pays() {
        // x402's canonical quote field; FairScale's `amount` mirror may vanish
        // if they align to spec, and the pay path must survive that.
        let mut envelope = live_envelope("5000");
        envelope["accepts"][0]
            .as_object_mut()
            .unwrap()
            .remove("amount");
        let agent = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/score"))
            .respond_with(ResponseTemplate::new(402).set_body_json(envelope))
            .up_to_n_times(1)
            .mount(&agent)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/score"))
            .and(header(
                "x-payment",
                "mock:solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp:5000",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "fairscore": 50.0 })))
            .mount(&agent)
            .await;
        let client = FairScaleClient::new("http://127.0.0.1:1", agent.uri()).with_payer(
            std::sync::Arc::new(covenant_x402::MockSigner),
            10_000,
            600_000,
        );
        assert_eq!(
            client.score("wallet").await.unwrap().fairscore(),
            Some(50.0)
        );
    }

    #[tokio::test]
    async fn response_bodies_are_read_capped() {
        // The remote controls its body; past the cap the read fails instead of
        // buffering without bound.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("a".repeat(4096)))
            .mount(&server)
            .await;
        let resp = reqwest::get(server.uri()).await.unwrap();
        let err = read_body_capped(resp, 64).await.unwrap_err();
        assert!(
            matches!(err, FairScaleError::ResponseTooLarge(_)),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn client_percent_encodes_query_values() {
        // The tool layer validates base58, but the client methods are pub; a
        // raw caller's wallet string must not smuggle extra query params.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/score"))
            .and(query_param("wallet", "a&admin=1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "fairscore": 1.0 })))
            .mount(&server)
            .await;
        let s = FairScaleClient::new(server.uri(), server.uri())
            .score("a&admin=1")
            .await
            .unwrap();
        assert_eq!(s.fairscore(), Some(1.0));
    }
}
