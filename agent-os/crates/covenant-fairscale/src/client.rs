//! Read-only REST client for FairScale's reputation, agent-trust, and credit
//! reads. Reputation lives on one host, agent + credit on another. Auth is an
//! optional `fairkey` header (free tier); an unauthenticated read gets a 401
//! (live behavior) or a 402 (documented behavior), both surfaced as the same
//! clear error until the x402 pay path is wired.
//!
//! Response bodies vary in shape across FairScale's products and their own spec
//! carries a score-scale inconsistency (0-100 vs ~0-1000 across endpoints), so
//! each type keeps the raw JSON and exposes typed accessors for the fields we
//! use rather than a brittle full deserialize.

use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use crate::{FairScaleError, Result};

const MAX_ATTEMPTS: u32 = 3;

#[derive(Clone)]
pub struct FairScaleClient {
    http: reqwest::Client,
    reputation_base: String,
    agent_base: String,
    fairkey: Option<String>,
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
        }
    }

    /// Attach a FairScale API key, sent as the `fairkey` header. Without one,
    /// FairScale answers a read with a 401 (live) or 402 (documented x402 path).
    pub fn with_fairkey(mut self, key: impl Into<String>) -> Self {
        let key = key.into();
        self.fairkey = if key.trim().is_empty() {
            None
        } else {
            Some(key)
        };
        self
    }

    /// Reputation FairScore for a wallet. `GET {reputation}/score?wallet=`.
    pub async fn score(&self, wallet: &str) -> Result<FairScore> {
        let url = format!("{}/score?wallet={wallet}", self.reputation_base);
        Ok(FairScore {
            raw: self.get(&url).await?,
        })
    }

    /// Agent allow/deny gate. `GET {agent}/v1/trust-gate?wallet=&min_score=`.
    pub async fn trust_gate(&self, wallet: &str, min_score: u32) -> Result<TrustGate> {
        let url = format!(
            "{}/v1/trust-gate?wallet={wallet}&min_score={min_score}",
            self.agent_base
        );
        let raw = self.get(&url).await?;
        serde_json::from_value(raw).map_err(|e| FairScaleError::Decode(format!("trust-gate: {e}")))
    }

    /// Credit underwriting read. `GET {agent}/v1/credit?wallet=&amount=`.
    pub async fn credit(&self, wallet: &str, amount_usd: u64) -> Result<CreditAssessment> {
        let url = format!(
            "{}/v1/credit?wallet={wallet}&amount={amount_usd}",
            self.agent_base
        );
        Ok(CreditAssessment {
            raw: self.get(&url).await?,
        })
    }

    /// GET with the `fairkey` header when set, a bounded retry on a transient or
    /// cold-start failure, and a clear message when a 402 means "pay over x402".
    async fn get(&self, url: &str) -> Result<Value> {
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

    async fn read(&self, resp: reqwest::Response) -> Result<Value> {
        let status = resp.status();
        let body = resp.text().await?;
        // The docs promise a 402 on no-auth; the live API returns a 401.
        // Treat both as the same actionable condition. The PAYMENT-REQUIRED
        // header and `accepts` body carry the x402 challenge; capture them
        // here when the paid-read increment lands.
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

    #[test]
    fn truncate_is_char_safe_on_multibyte() {
        let multibyte = "é".repeat(600);
        assert_eq!(truncate(&multibyte).chars().count(), 501);
        let ascii = "a".repeat(600);
        assert_eq!(truncate(&ascii).chars().count(), 501);
        assert_eq!(truncate("short"), "short");
    }
}
