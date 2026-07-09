//! Read-only REST client for Krexa's credit/score API.
//!
//! Three GETs, all public (no auth) and free: the Krexit score, credit
//! eligibility, and the active credit line. The score blob mixes camel
//! and snake casing across nesting levels, so [`KrexitScore`] keeps the
//! raw JSON and exposes typed accessors for the few fields we actually
//! use, rather than a brittle full deserialize. Eligibility and the
//! credit line are clean camelCase and are typed.

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::onchain::KrexitScoreAccount;
use crate::{KrexaError, Result};

#[derive(Clone)]
pub struct KrexaClient {
    http: reqwest::Client,
    base_url: String,
    rpc_url: Option<String>,
}

impl KrexaClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        // Krexa's backend runs on Render, which cold-starts parked
        // instances, so a request after a quiet spell can take seconds to
        // connect. Short connect timeout, generous overall, so a wedged
        // backend can't hang the calling daemon thread.
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .build()
            .expect("build krexa http client");
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            rpc_url: None,
        }
    }

    /// Point the client at a Solana RPC so [`Self::score_onchain`] can
    /// decode the `KrexitScore` account. Krexa's programs are on mainnet;
    /// pass a mainnet endpoint.
    pub fn with_rpc_url(mut self, rpc_url: impl Into<String>) -> Self {
        self.rpc_url = Some(rpc_url.into());
        self
    }

    /// The configured RPC endpoint, if any. `None` means REST-only.
    pub fn rpc_url(&self) -> Option<&str> {
        self.rpc_url.as_deref()
    }

    /// REST base, for modules building their own paths (e.g. credit).
    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    /// GET with a bounded cold-start retry. Krexa's Render backend parks idle
    /// instances; the first request after a quiet spell can time out or return
    /// 502/503/504 while it spins up, and the next one hits a warm service.
    /// Those (and connect failures) retry up to [`MAX_ATTEMPTS`] with a short
    /// backoff. A 4xx is a real client error and is never retried.
    async fn get(&self, path: &str) -> Result<Value> {
        let url = format!("{}/{}", self.base_url, path.trim_start_matches('/'));
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.http.get(&url).send().await {
                Ok(resp) => {
                    if attempt < MAX_ATTEMPTS && is_cold_start_status(resp.status()) {
                        tokio::time::sleep(backoff(attempt)).await;
                        continue;
                    }
                    return self.read(resp, path).await;
                }
                Err(e) if attempt < MAX_ATTEMPTS && is_transient(&e) => {
                    tokio::time::sleep(backoff(attempt)).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// POST `body` as JSON, used by the gated credit path. Shares the same
    /// status/decoding handling as [`Self::get`], but is not retried: a credit
    /// draw is not idempotent.
    pub(crate) async fn http_post_json<T: serde::Serialize>(
        &self,
        url: &str,
        body: &T,
    ) -> Result<Value> {
        let resp = self.http.post(url).json(body).send().await?;
        self.read(resp, url).await
    }

    async fn read(&self, resp: reqwest::Response, ctx: &str) -> Result<Value> {
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(KrexaError::Api {
                status: status.as_u16(),
                message: truncate(&body),
            });
        }
        serde_json::from_str(&body)
            .map_err(|e| KrexaError::Decode(format!("{ctx}: {e}; body: {}", truncate(&body))))
    }

    /// Krexit score + risk/underwriting blob for any agent pubkey
    /// (registered or not). `GET /solana/score/:agent`.
    pub async fn score(&self, agent_pubkey: &str) -> Result<KrexitScore> {
        let raw = self.get(&format!("solana/score/{agent_pubkey}")).await?;
        Ok(KrexitScore { raw })
    }

    /// Borrowing eligibility + available credit. `GET /solana/credit/:agent/eligibility`.
    pub async fn eligibility(&self, agent_pubkey: &str) -> Result<CreditEligibility> {
        let raw = self
            .get(&format!("solana/credit/{agent_pubkey}/eligibility"))
            .await?;
        serde_json::from_value(raw).map_err(|e| KrexaError::Decode(format!("eligibility: {e}")))
    }

    /// Active credit line, if any. `GET /solana/credit/:agent/line`.
    pub async fn credit_line(&self, agent_pubkey: &str) -> Result<CreditLine> {
        let raw = self
            .get(&format!("solana/credit/{agent_pubkey}/line"))
            .await?;
        serde_json::from_value(raw).map_err(|e| KrexaError::Decode(format!("line: {e}")))
    }

    /// Read and decode the on-chain `KrexitScore` account at `score_pda`
    /// (as returned by [`KrexitScore::score_pda`]) and verify it belongs to
    /// `agent_pubkey`. This is the trustless path: the score is whatever the
    /// chain holds, not what Krexa's server reports. Every failure — RPC
    /// error, missing account, or an agent mismatch — is a
    /// [`KrexaError::Chain`], so a caller can degrade to the REST soft
    /// signal on error. Requires an RPC url ([`Self::with_rpc_url`]).
    pub async fn score_onchain(
        &self,
        agent_pubkey: &str,
        score_pda: &str,
    ) -> Result<OnchainScore> {
        let rpc_url = self
            .rpc_url
            .as_deref()
            .ok_or_else(|| KrexaError::Chain("no rpc_url configured".into()))?;
        let want = decode_pubkey32(agent_pubkey)?;

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getAccountInfo",
            "params": [score_pda, { "commitment": "confirmed", "encoding": "base64" }],
        });
        let resp = self.http.post(rpc_url).json(&body).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(KrexaError::Chain(format!(
                "getAccountInfo status {status}: {}",
                truncate(&text)
            )));
        }
        let parsed: Value = serde_json::from_str(&text)
            .map_err(|e| KrexaError::Chain(format!("rpc body: {e}; {}", truncate(&text))))?;
        if let Some(err) = parsed.get("error") {
            return Err(KrexaError::Chain(format!("rpc error: {err}")));
        }
        if parsed.pointer("/result/value").is_none_or(Value::is_null) {
            return Err(KrexaError::Chain(format!(
                "no KrexitScore account at {score_pda}"
            )));
        }
        let data_b64 = parsed
            .pointer("/result/value/data/0")
            .and_then(Value::as_str)
            .ok_or_else(|| KrexaError::Chain("account data not base64-encoded".into()))?;
        let owner_program = parsed
            .pointer("/result/value/owner")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let bytes = B64
            .decode(data_b64)
            .map_err(|e| KrexaError::Chain(format!("account base64: {e}")))?;
        let account = KrexitScoreAccount::decode(&bytes)?;
        if !account.matches_agent(&want) {
            return Err(KrexaError::Chain(
                "on-chain agent mismatch: scorePda belongs to a different agent".into(),
            ));
        }
        Ok(OnchainScore {
            account,
            score_pda: score_pda.to_string(),
            owner_program,
        })
    }
}

/// A Krexit score response. Keeps the full blob; accessors pull the
/// fields the reputation signal uses, tolerant of Krexa's mixed casing.
#[derive(Debug, Clone, PartialEq)]
pub struct KrexitScore {
    pub raw: Value,
}

impl KrexitScore {
    /// The headline Krexit score (200..=850), read from the top-level
    /// `score`. Deliberately not `krexit.creditScore`, which the live
    /// response also carries on a different internal scale. `None` if absent.
    pub fn score(&self) -> Option<i64> {
        self.raw.get("score").and_then(Value::as_i64)
    }
    /// Credit level tier (1..=4).
    pub fn credit_level(&self) -> Option<u64> {
        self.raw.get("creditLevel").and_then(Value::as_u64)
    }
    /// Risk band, e.g. `"deep_subprime"` (nested under `krexit`).
    pub fn risk_band(&self) -> Option<String> {
        self.raw
            .get("krexit")
            .and_then(|k| k.get("riskBand"))
            .and_then(Value::as_str)
            .map(String::from)
    }
    /// Whether Krexa has the agent registered/underwritten.
    pub fn registered(&self) -> bool {
        self.raw
            .get("isRegistered")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
    /// Whether Krexa's SNS `.sol` boost was applied. Krexa rewards agents
    /// that hold a `.sol` name, which Covenant's identity layer already
    /// issues, so a Covenant agent tends to score better here for free.
    pub fn sns_boost_applied(&self) -> bool {
        self.raw
            .get("snsBoost")
            .and_then(|b| b.get("applied"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
    /// Underwriting recommendation, e.g. `"Approve with strict limits"`.
    pub fn recommendation(&self) -> Option<String> {
        self.raw
            .get("krexit")
            .and_then(|k| k.get("underwriting"))
            .and_then(|u| u.get("opinion"))
            .and_then(Value::as_str)
            .map(String::from)
    }
    /// Krexa's self-attestation hash, read from `krexit.attestation.payload_hash`
    /// as confirmed against the live response. A 32-byte SHA-256 hex digest,
    /// not a signature, so it is a soft signal, not a verifiable proof.
    /// Returns `None` for any other shape rather than guessing at a key
    /// Krexa doesn't emit.
    pub fn attestation_hash(&self) -> Option<String> {
        self.raw
            .get("krexit")
            .and_then(|k| k.get("attestation"))
            .and_then(|a| a.get("payload_hash"))
            .and_then(Value::as_str)
            .map(String::from)
    }

    /// The on-chain Krexit score PDA, if present (`scorePda`). The
    /// trustless read path decodes this account directly instead of
    /// trusting REST, and is blocked only on Krexa publishing the account
    /// layout or IDL. Captured now so that switch is a one-file change.
    pub fn score_pda(&self) -> Option<String> {
        self.raw
            .get("scorePda")
            .and_then(Value::as_str)
            .map(String::from)
    }
}

/// The decoded, agent-verified on-chain score from [`KrexaClient::score_onchain`].
/// `account.score` is the trustless number; `owner_program` is Krexa's score
/// program, recovered from the account's on-chain owner (pin it once Krexa
/// confirms the canonical id).
#[derive(Debug, Clone, PartialEq)]
pub struct OnchainScore {
    pub account: KrexitScoreAccount,
    pub score_pda: String,
    pub owner_program: String,
}

/// `GET /solana/credit/:agent/eligibility`. Amounts here are whole USDC as
/// floats, unlike [`CreditLine`] which is atomic USDC. Size a draw via
/// [`CreditEligibility::available_credit_atomic`], never the raw float.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditEligibility {
    pub eligible: bool,
    #[serde(default)]
    pub credit_level: u8,
    #[serde(default)]
    pub max_credit: f64,
    #[serde(default)]
    pub current_debt: f64,
    #[serde(default)]
    pub available_credit: f64,
    #[serde(default)]
    pub rate_bps: u32,
}

impl CreditEligibility {
    /// Available credit in atomic USDC (6 decimals), the unit the draw path
    /// and [`crate::credit::shortfall_draw_lamports`] work in. This is the
    /// one bridge across the whole-USDC-float vs atomic-u128 seam, so a
    /// caller never feeds dollars into a lamports calc. Rounds to the
    /// nearest atom; input that can't map into range (non-finite, negative,
    /// or overflowing) clamps to zero.
    pub fn available_credit_atomic(&self) -> u128 {
        whole_usdc_to_atomic(self.available_credit)
    }
}

/// Whole USDC (float, as Krexa's eligibility endpoint sends it) to atomic
/// USDC. Anything that doesn't map cleanly into u128 atomic range (NaN,
/// negative, or a finite value large enough to overflow the scale) clamps
/// to zero, so garbage denies credit instead of granting the maximum.
fn whole_usdc_to_atomic(whole: f64) -> u128 {
    let scaled = whole * 1_000_000.0;
    if !scaled.is_finite() || scaled < 0.0 || scaled >= u128::MAX as f64 {
        return 0;
    }
    scaled.round() as u128
}

/// `GET /solana/credit/:agent/line`. Amounts are USDC lamports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditLine {
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub principal: u64,
    #[serde(default)]
    pub accrued_interest: u64,
    #[serde(default)]
    pub total_owed: u64,
    #[serde(default)]
    pub rate_bps: u32,
    /// Encoding is unconfirmed (Krexa hasn't published it), so keep it
    /// permissive: a no-debt line may carry a large sentinel or a
    /// fractional ratio that a tight integer type would reject on decode
    /// and sink the whole line read.
    #[serde(default)]
    pub health_factor: Option<f64>,
    #[serde(default)]
    pub opened_at: Option<String>,
}

/// Total GET attempts before giving up (one try, then two retries).
const MAX_ATTEMPTS: u32 = 3;

/// Backoff before the next GET attempt: 400ms, then 800ms.
fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(400 * u64::from(attempt))
}

/// Statuses a cold-starting Render service returns while it spins up; the next
/// attempt usually lands on a warm instance.
fn is_cold_start_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 502 | 503 | 504 | 429)
}

/// A connect failure or timeout against a parked instance; retryable.
fn is_transient(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect()
}

/// Base58 pubkey → 32 raw bytes, the form [`KrexitScoreAccount::matches_agent`]
/// compares against. A non-base58 or wrong-length string is a
/// [`KrexaError::Chain`].
fn decode_pubkey32(pubkey: &str) -> Result<[u8; 32]> {
    let bytes = bs58::decode(pubkey)
        .into_vec()
        .map_err(|e| KrexaError::Chain(format!("agent pubkey not base58: {e}")))?;
    bytes
        .try_into()
        .map_err(|_| KrexaError::Chain("agent pubkey is not 32 bytes".into()))
}

fn truncate(s: &str) -> String {
    let cut: String = s.chars().take(200).collect();
    if cut.chars().count() < s.chars().count() {
        format!("{cut}...")
    } else {
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Trimmed from the real live /solana/score response (2026-06-29):
    // attestation is nested under `krexit`, scorePda is top-level.
    fn live_score() -> Value {
        json!({
            "score": 342, "creditLevel": 1, "isRegistered": false,
            "scorePda": "DtbfDxdDZtx7pQRAufzeAGTMPbMB9F2shKk8X7FkF9er",
            "snsBoost": { "applied": false, "boost": 0 },
            "krexit": {
                "creditScore": 31, "riskBand": "deep_subprime",
                "underwriting": { "opinion": "Approve with strict limits" },
                "attestation": { "type": "krexa_v1", "payload_hash": "610f0de3705d5dbb5a65e9cbe94aeed7002aed57464bc85d1b1032cc5e3bb527" }
            }
        })
    }

    #[test]
    fn score_accessors_read_mixed_casing() {
        let s = KrexitScore { raw: live_score() };
        assert_eq!(s.score(), Some(342));
        assert_eq!(s.credit_level(), Some(1));
        assert_eq!(s.risk_band().as_deref(), Some("deep_subprime"));
        assert!(!s.registered());
        assert!(!s.sns_boost_applied());
        assert_eq!(
            s.recommendation().as_deref(),
            Some("Approve with strict limits")
        );
        assert_eq!(
            s.attestation_hash().as_deref(),
            Some("610f0de3705d5dbb5a65e9cbe94aeed7002aed57464bc85d1b1032cc5e3bb527")
        );
        assert_eq!(
            s.score_pda().as_deref(),
            Some("DtbfDxdDZtx7pQRAufzeAGTMPbMB9F2shKk8X7FkF9er")
        );
    }

    #[tokio::test]
    async fn reads_score_eligibility_line() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/solana/score/AG"))
            .respond_with(ResponseTemplate::new(200).set_body_json(live_score()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/solana/credit/AG/eligibility"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "eligible": true, "creditLevel": 1, "maxCredit": 500.0,
                "currentDebt": 0.0, "availableCredit": 500.0, "rateBps": 3650
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/solana/credit/AG/line"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "active": true, "principal": 500000000u64, "accruedInterest": 2500000u64,
                "totalOwed": 502500000u64, "rateBps": 3650, "healthFactor": 15000
            })))
            .mount(&server)
            .await;

        let c = KrexaClient::new(server.uri());
        assert_eq!(c.score("AG").await.unwrap().score(), Some(342));
        let e = c.eligibility("AG").await.unwrap();
        assert!(e.eligible && e.available_credit == 500.0);
        // The draw path consumes atomic USDC, not the whole-USDC float.
        assert_eq!(e.available_credit_atomic(), 500_000_000);
        let l = c.credit_line("AG").await.unwrap();
        assert!(l.active && l.total_owed == 502_500_000 && l.health_factor == Some(15000.0));
    }

    #[test]
    fn available_credit_bridges_whole_usdc_to_atomic() {
        let mk = |c: f64| CreditEligibility {
            eligible: true,
            credit_level: 1,
            max_credit: c,
            current_debt: 0.0,
            available_credit: c,
            rate_bps: 0,
        };
        assert_eq!(mk(500.0).available_credit_atomic(), 500_000_000);
        assert_eq!(mk(0.05).available_credit_atomic(), 50_000);
        assert_eq!(mk(0.0).available_credit_atomic(), 0);
        // Garbage in (negative, NaN, or a finite value past the scale)
        // clamps to zero rather than wrapping or saturating to the max.
        assert_eq!(mk(-1.0).available_credit_atomic(), 0);
        assert_eq!(mk(f64::NAN).available_credit_atomic(), 0);
        assert_eq!(mk(1e40).available_credit_atomic(), 0);
    }

    #[tokio::test]
    async fn credit_line_tolerates_unconfirmed_health_factor() {
        // A no-debt line that reports an out-of-u32-range health factor must
        // not sink the whole line decode.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/solana/credit/HF/line"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "active": true, "principal": 0u64, "totalOwed": 0u64,
                "healthFactor": 9_999_999_999u64
            })))
            .mount(&server)
            .await;
        let l = KrexaClient::new(server.uri())
            .credit_line("HF")
            .await
            .unwrap();
        assert_eq!(l.health_factor, Some(9_999_999_999.0));
    }

    #[tokio::test]
    async fn non_2xx_is_api_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/solana/score/BAD"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;
        let err = KrexaClient::new(server.uri())
            .score("BAD")
            .await
            .unwrap_err();
        assert!(matches!(err, KrexaError::Api { status: 404, .. }));
    }

    #[tokio::test]
    async fn retries_cold_start_503_then_succeeds() {
        // Render returns 503 while the parked instance spins up. The one-shot
        // 503 is mounted first (wiremock prefers the earliest match); once it
        // is used up, the retry lands on the warm 200.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/solana/score/AG"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/solana/score/AG"))
            .respond_with(ResponseTemplate::new(200).set_body_json(live_score()))
            .mount(&server)
            .await;
        let c = KrexaClient::new(server.uri());
        assert_eq!(c.score("AG").await.unwrap().score(), Some(342));
    }

    #[tokio::test]
    async fn does_not_retry_4xx() {
        // A 4xx is a real client error: one attempt, returned immediately.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/solana/score/X"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        let err = KrexaClient::new(server.uri()).score("X").await.unwrap_err();
        assert!(matches!(err, KrexaError::Api { status: 404, .. }));
    }

    /// A valid 648-byte KrexitScore buffer with `agent` and `score` set and
    /// every other field zero — enough to exercise decode + the agent guard.
    fn account_bytes(agent: [u8; 32], score: u16) -> Vec<u8> {
        let mut b = vec![0u8; crate::onchain::KREXIT_SCORE_LEN];
        b[..8].copy_from_slice(&crate::onchain::KREXIT_SCORE_DISCRIMINATOR);
        b[8..40].copy_from_slice(&agent);
        b[72..74].copy_from_slice(&score.to_le_bytes());
        b
    }

    /// A `getAccountInfo` success envelope carrying `bytes` as base64 and
    /// `owner` as the account's program.
    fn rpc_account(bytes: &[u8], owner: &str) -> Value {
        json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {
                "context": { "slot": 1 },
                "value": {
                    "owner": owner,
                    "lamports": 2_000_000u64,
                    "data": [B64.encode(bytes), "base64"],
                    "executable": false,
                    "rentEpoch": 0
                }
            }
        })
    }

    #[tokio::test]
    async fn score_onchain_decodes_and_verifies_agent() {
        let agent = "EnteGjokMnFqTDcZSBitXDQEctMCnqV33HbPKw2LnDCg";
        let agent_bytes = decode_pubkey32(agent).unwrap();
        let score_prog = "BuutifX7Wj8ysKeLQt1pag3JYWQdZHjktCA6ePUfHFHs";
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(rpc_account(&account_bytes(agent_bytes, 742), score_prog)),
            )
            .mount(&server)
            .await;
        let c = KrexaClient::new("http://rest.invalid").with_rpc_url(server.uri());
        let oc = c
            .score_onchain(agent, "DtbfDxdDZtx7pQRAufzeAGTMPbMB9F2shKk8X7FkF9er")
            .await
            .unwrap();
        assert_eq!(oc.account.score, 742);
        assert!(oc.account.matches_agent(&agent_bytes));
        assert_eq!(oc.owner_program, score_prog);
    }

    #[tokio::test]
    async fn score_onchain_rejects_mismatched_agent() {
        // The returned account belongs to a different agent than queried.
        let queried = "EnteGjokMnFqTDcZSBitXDQEctMCnqV33HbPKw2LnDCg";
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(rpc_account(&account_bytes([0xAA; 32], 800), "prog")),
            )
            .mount(&server)
            .await;
        let c = KrexaClient::new("http://rest.invalid").with_rpc_url(server.uri());
        let err = c.score_onchain(queried, "pda").await.unwrap_err();
        assert!(matches!(err, KrexaError::Chain(_)));
    }

    #[tokio::test]
    async fn score_onchain_reports_missing_account() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0", "id": 1,
                "result": { "context": { "slot": 1 }, "value": null }
            })))
            .mount(&server)
            .await;
        let c = KrexaClient::new("http://rest.invalid").with_rpc_url(server.uri());
        let err = c
            .score_onchain("EnteGjokMnFqTDcZSBitXDQEctMCnqV33HbPKw2LnDCg", "pda")
            .await
            .unwrap_err();
        assert!(matches!(err, KrexaError::Chain(_)));
    }

    #[tokio::test]
    async fn score_onchain_requires_rpc_url() {
        // REST-only client: the trustless read is unavailable, not a panic.
        let c = KrexaClient::new("http://rest.invalid");
        let err = c
            .score_onchain("EnteGjokMnFqTDcZSBitXDQEctMCnqV33HbPKw2LnDCg", "pda")
            .await
            .unwrap_err();
        assert!(matches!(err, KrexaError::Chain(_)));
    }

    /// Live read against the deployed Krexa backend:
    ///   cargo test -p covenant-krexa live_krexa_score -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_krexa_score() {
        let c = KrexaClient::new(crate::config::BASE_URL);
        let s = c
            .score("EnteGjokMnFqTDcZSBitXDQEctMCnqV33HbPKw2LnDCg")
            .await
            .expect("live score");
        println!(
            "score={:?} band={:?} attn={:?}",
            s.score(),
            s.risk_band(),
            s.attestation_hash()
        );
        assert!(s.score().is_some());
    }
}
