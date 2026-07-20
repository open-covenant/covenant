//! Read-only REST client for FairScale's reputation, agent-trust, and credit
//! reads. Reputation lives on one host, agent + credit on another. Auth is an
//! optional `fairkey` header (free tier) or an x402 payer: on a 402 the client
//! settles the quoted amount (USDC on Solana mainnet, bounded by a per-read
//! cap) and retries once with the `x-payment` header. With neither, a keyless
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
    /// answers keyless reads with a 404 (both observed live 2026-07-20).
    pub async fn score(&self, wallet: &str) -> Result<FairScore> {
        let url = if self.payer.is_some() {
            format!("{}/v1/score?wallet={wallet}", self.agent_base)
        } else {
            format!("{}/score?wallet={wallet}", self.reputation_base)
        };
        Ok(FairScore {
            raw: self.get(&url, self.read_cap_atomic).await?,
        })
    }

    /// Agent allow/deny gate. `GET {agent}/v1/trust-gate?wallet=&min_score=`.
    pub async fn trust_gate(&self, wallet: &str, min_score: u32) -> Result<TrustGate> {
        let url = format!(
            "{}/v1/trust-gate?wallet={wallet}&min_score={min_score}",
            self.agent_base
        );
        let raw = self.get(&url, self.read_cap_atomic).await?;
        serde_json::from_value(raw).map_err(|e| FairScaleError::Decode(format!("trust-gate: {e}")))
    }

    /// Credit underwriting read. `GET {agent}/v1/credit?wallet=&amount=`.
    pub async fn credit(&self, wallet: &str, amount_usd: u64) -> Result<CreditAssessment> {
        let url = format!(
            "{}/v1/credit?wallet={wallet}&amount={amount_usd}",
            self.agent_base
        );
        Ok(CreditAssessment {
            raw: self.get(&url, self.credit_cap_atomic).await?,
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
        let body = challenge_resp.text().await?;
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
        let amount: u64 = option.amount.parse().map_err(|_| {
            FairScaleError::Payment(format!("unparseable challenge amount {:?}", option.amount))
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
        let decimals = option.extra.as_ref().and_then(|e| e.decimals).unwrap_or(6);
        let requirements = PaymentRequirements {
            network: option.network.clone(),
            asset: option.asset.clone(),
            amount: option.amount.clone(),
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
            "fairscale 402 settled; retrying with x-payment"
        );
        let mut req = self.http.get(url).header("x-payment", header);
        if let Some(key) = &self.fairkey {
            req = req.header("fairkey", key);
        }
        let resp = req.send().await?;
        if matches!(resp.status().as_u16(), 401 | 402) {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(FairScaleError::Payment(format!(
                "read still rejected ({status}) after payment: {}",
                truncate(&body)
            )));
        }
        self.read(resp).await
    }

    async fn read(&self, resp: reqwest::Response) -> Result<Value> {
        let status = resp.status();
        let body = resp.text().await?;
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

/// 429 (free tier is 10 req/min and FairScale prescribes backoff-and-retry)
/// plus the gateway 5xxs a serverless cold start throws.
fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 502..=504)
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(200 * 2u64.pow(attempt - 1))
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
    async fn retries_a_429_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/score"))
            .respond_with(ResponseTemplate::new(429))
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
    async fn credit_reads_lending_terms_from_underwriting() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/credit"))
            .and(query_param("amount", "1000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "wallet": "x", "credit_score": 71.0, "risk_band": "prime",
                "underwriting": {
                    "opinion": "approve",
                    "lending_terms": { "max_credit_line": 2500, "suggested_apr_range": "8-12%" }
                }
            })))
            .mount(&server)
            .await;
        let client = FairScaleClient::new(server.uri(), server.uri());
        let c = client.credit("wallet", 1000).await.unwrap();
        assert_eq!(c.credit_score(), Some(71.0));
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
        // The paid retry must carry the exact header the signer built from the
        // chosen option; MockSigner encodes network + amount, pinning both.
        Mock::given(method("GET"))
            .and(path("/v1/score"))
            .and(header(
                "x-payment",
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
                "credit_score": 71.0, "risk_band": "prime"
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
        assert_eq!(c.credit_score(), Some(71.0));
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
}
