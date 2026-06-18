//! x402 facilitator for ER-settled payments.
//!
//! Counterpart to [`covenant_x402::EphemeralSigner`]. Where the signer pays by
//! running `consume_credits` in a MagicBlock ephemeral rollup and hands back the
//! signature, this verifies that proof and gates a paid endpoint:
//!
//! - `GET /paid` with no `x-payment` header → `402` + a challenge carrying a fresh
//!   single-use `nonce` and the `amountCredits` price.
//! - `GET /paid` with an `x-payment` header → decode the envelope, fetch the consume
//!   on the ER, and check: it's a `consume_credits` to the settlement program, the
//!   `amount >= price`, `receipt_hash == sha256(nonce)`, and the nonce was one we
//!   issued and have not yet consumed (anti-replay). Pass → `200` + content.
//!
//! It talks to the ER over JSON-RPC (`getTransaction`) and works in base58 strings,
//! so it needs neither solana-sdk nor the on-chain ER SDK.

#![deny(unsafe_code)]

use std::collections::HashMap;
use std::time::{Duration, Instant};
use std::sync::{Arc, Mutex};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::json;
use sha2::{Digest, Sha256};

/// `sha256("global:consume_credits")[..8]` — the settlement program's instruction
/// discriminator for `consume_credits`.
pub fn consume_discriminator() -> [u8; 8] {
    let mut out = [0u8; 8];
    out.copy_from_slice(&Sha256::digest(b"global:consume_credits")[..8]);
    out
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum VerifyError {
    #[error("bad x-payment envelope: {0}")]
    Envelope(String),
    #[error("nonce mismatch")]
    NonceMismatch,
    #[error("ER rpc: {0}")]
    Rpc(String),
    #[error("consume tx not found on ER")]
    NotFound,
    #[error("consume tx failed on ER")]
    TxFailed,
    #[error("no consume_credits to the settlement program in tx")]
    NoConsume,
    #[error("underpaid: {paid} < {required}")]
    Underpaid { paid: u64, required: u64 },
    #[error("receipt_hash does not match nonce")]
    ReceiptMismatch,
}

/// A verified ER-settled payment.
#[derive(Debug, Clone, PartialEq)]
pub struct Verified {
    pub amount: u64,
    pub signature: String,
    /// The credit account that was metered (read from the on-chain tx, not the
    /// client-supplied envelope).
    pub credit_account: String,
}

/// Verifies ER consume_credits payments against a settlement program on an ER.
pub struct ErPaymentVerifier {
    program_id: String,
    er_rpc_url: String,
    http: reqwest::Client,
}

impl ErPaymentVerifier {
    pub fn new(program_id: impl Into<String>, er_rpc_url: impl Into<String>) -> Self {
        Self {
            program_id: program_id.into(),
            er_rpc_url: er_rpc_url.into(),
            http: reqwest::Client::new(),
        }
    }

    pub fn program_id(&self) -> &str {
        &self.program_id
    }

    /// Verifies that `x_payment_b64` proves a `consume_credits` of at least
    /// `min_amount`, bound to `expected_nonce`. Reads the credit account from the
    /// on-chain transaction so a spoofed envelope field cannot lie about it.
    pub async fn verify(
        &self,
        x_payment_b64: &str,
        expected_nonce: &str,
        min_amount: u64,
    ) -> Result<Verified, VerifyError> {
        let raw = BASE64
            .decode(x_payment_b64.trim())
            .map_err(|e| VerifyError::Envelope(e.to_string()))?;
        let envelope: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|e| VerifyError::Envelope(e.to_string()))?;
        let payload = envelope
            .get("payload")
            .ok_or_else(|| VerifyError::Envelope("no payload".into()))?;
        let signature = payload
            .get("signature")
            .and_then(|v| v.as_str())
            .ok_or_else(|| VerifyError::Envelope("no signature".into()))?;
        let nonce = payload
            .get("nonce")
            .and_then(|v| v.as_str())
            .ok_or_else(|| VerifyError::Envelope("no nonce".into()))?;
        if nonce != expected_nonce {
            return Err(VerifyError::NonceMismatch);
        }

        let result = self.fetch_transaction(signature).await?;
        if result
            .pointer("/meta/err")
            .map(|v| !v.is_null())
            .unwrap_or(false)
        {
            return Err(VerifyError::TxFailed);
        }
        let keys = result
            .pointer("/transaction/message/accountKeys")
            .and_then(|v| v.as_array())
            .ok_or(VerifyError::NoConsume)?;
        let ixs = result
            .pointer("/transaction/message/instructions")
            .and_then(|v| v.as_array())
            .ok_or(VerifyError::NoConsume)?;

        let disc = consume_discriminator();
        let expected_receipt = Sha256::digest(expected_nonce.as_bytes());
        for ix in ixs {
            let pid = ix
                .get("programIdIndex")
                .and_then(|v| v.as_u64())
                .and_then(|i| keys.get(i as usize))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if pid != self.program_id {
                continue;
            }
            let data = ix
                .get("data")
                .and_then(|v| v.as_str())
                .and_then(|d| bs58::decode(d).into_vec().ok())
                .ok_or_else(|| VerifyError::Envelope("undecodable ix data".into()))?;
            if data.len() < 48 || data[..8] != disc {
                continue;
            }
            let amount = u64::from_le_bytes(data[8..16].try_into().unwrap());
            if amount < min_amount {
                return Err(VerifyError::Underpaid {
                    paid: amount,
                    required: min_amount,
                });
            }
            if data[16..48] != expected_receipt[..] {
                return Err(VerifyError::ReceiptMismatch);
            }
            // ConsumeCredits accounts: [config, credits, owner]. Credit account is #1.
            let credit_account = ix
                .get("accounts")
                .and_then(|v| v.as_array())
                .and_then(|a| a.get(1))
                .and_then(|v| v.as_u64())
                .and_then(|i| keys.get(i as usize))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            return Ok(Verified {
                amount,
                signature: signature.to_string(),
                credit_account,
            });
        }
        Err(VerifyError::NoConsume)
    }

    async fn fetch_transaction(&self, signature: &str) -> Result<serde_json::Value, VerifyError> {
        let body = json!({
            "jsonrpc": "2.0", "id": 1, "method": "getTransaction",
            "params": [signature, {"encoding": "json", "commitment": "confirmed", "maxSupportedTransactionVersion": 0}]
        });
        let resp = self
            .http
            .post(&self.er_rpc_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| VerifyError::Rpc(e.to_string()))?;
        let parsed: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| VerifyError::Rpc(e.to_string()))?;
        if let Some(err) = parsed.get("error").filter(|e| !e.is_null()) {
            return Err(VerifyError::Rpc(err.to_string()));
        }
        parsed
            .get("result")
            .filter(|v| !v.is_null())
            .cloned()
            .ok_or(VerifyError::NotFound)
    }
}

/// A paid endpoint gated by ER-settled payments.
/// How long an issued challenge nonce stays valid. A payment must present its
/// `x-payment` within this window or the nonce is evicted (fail-closed).
const NONCE_TTL: Duration = Duration::from_secs(300);
/// Backstop bound on outstanding nonces so a flood of unpaid challenges cannot
/// grow memory without limit. With TTL eviction the steady state is far below this.
const MAX_OUTSTANDING_NONCES: usize = 50_000;

/// Drop nonces older than [`NONCE_TTL`]. Called under the lock on every request so
/// the issued set stays bounded by (issuance rate x TTL), and expired challenges
/// can never be redeemed.
fn evict_expired(issued: &mut HashMap<String, Instant>) {
    let now = Instant::now();
    issued.retain(|_, &mut t| now.duration_since(t) < NONCE_TTL);
}

pub struct PaidEndpoint {
    pub verifier: ErPaymentVerifier,
    pub price: u64,
    pub content: serde_json::Value,
    /// Single-use nonces we have issued and not yet consumed, with their issue
    /// time. TTL-evicted (see [`evict_expired`]) and bounded by
    /// [`MAX_OUTSTANDING_NONCES`], so unpaid challenges don't accumulate.
    issued: Mutex<HashMap<String, Instant>>,
}

impl PaidEndpoint {
    pub fn new(verifier: ErPaymentVerifier, price: u64, content: serde_json::Value) -> Arc<Self> {
        Arc::new(Self {
            verifier,
            price,
            content,
            issued: Mutex::new(HashMap::new()),
        })
    }
}

pub fn router(state: Arc<PaidEndpoint>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/paid", get(handler))
        .with_state(state)
}

/// Liveness probe for the deploy platform (Render health check). Always 200.
async fn health() -> Response {
    (StatusCode::OK, "ok").into_response()
}

fn nonce_from_envelope(x_payment_b64: &str) -> Option<String> {
    let raw = BASE64.decode(x_payment_b64.trim()).ok()?;
    let env: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    env.pointer("/payload/nonce")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn random_nonce() -> String {
    format!("{:032x}", rand::random::<u128>())
}

fn pay402(why: &str) -> Response {
    (StatusCode::PAYMENT_REQUIRED, Json(json!({"error": why}))).into_response()
}

async fn handler(State(state): State<Arc<PaidEndpoint>>, headers: HeaderMap) -> Response {
    let Some(hdr) = headers.get("x-payment") else {
        let nonce = random_nonce();
        {
            let mut issued = state.issued.lock().unwrap();
            evict_expired(&mut issued);
            if issued.len() >= MAX_OUTSTANDING_NONCES {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({ "error": "too many outstanding challenges; retry shortly" })),
                )
                    .into_response();
            }
            issued.insert(nonce.clone(), Instant::now());
        }
        // An x402 challenge body: an array of acceptable payment options. The
        // shape matches covenant-x402's `PaymentRequirements` so its `Client`
        // parses it directly; the per-request nonce rides in `extra.nonce`, which
        // the `EphemeralSigner` reads to bind the on-chain receipt_hash.
        return (
            StatusCode::PAYMENT_REQUIRED,
            Json(json!([{
                "network": "solana-er:devnet",
                "asset": "credits",
                "amount": state.price.to_string(),
                "amountUsdc": 0.0,
                "payTo": state.verifier.program_id(),
                "scheme": "exact-er",
                "extra": { "nonce": nonce },
            }])),
        )
            .into_response();
    };

    let Ok(hdr) = hdr.to_str() else {
        return pay402("invalid x-payment header");
    };
    let Some(nonce) = nonce_from_envelope(hdr) else {
        return pay402("bad x-payment envelope");
    };
    // Consume the nonce atomically: unknown, already-used, or expired nonces are
    // rejected. Single-use consumption is the primary replay guard; the TTL eviction
    // means an expired challenge is already gone here and fails closed.
    {
        let mut issued = state.issued.lock().unwrap();
        evict_expired(&mut issued);
        if issued.remove(&nonce).is_none() {
            return pay402("unknown, already-used, or expired nonce");
        }
    }
    match state.verifier.verify(hdr, &nonce, state.price).await {
        Ok(v) => (
            StatusCode::OK,
            Json(json!({
                "content": state.content,
                "paidCredits": v.amount,
                "creditAccount": v.credit_account,
                "settledBy": v.signature,
            })),
        )
            .into_response(),
        Err(e) => pay402(&e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const PROGRAM: &str = "cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y";

    #[test]
    fn evict_expired_drops_old_nonces_keeps_fresh() {
        let mut issued: HashMap<String, Instant> = HashMap::new();
        let stale = Instant::now()
            .checked_sub(NONCE_TTL + Duration::from_secs(60))
            .expect("instant sub");
        issued.insert("stale".into(), stale);
        issued.insert("fresh".into(), Instant::now());
        evict_expired(&mut issued);
        assert!(!issued.contains_key("stale"), "expired nonce must be evicted");
        assert!(issued.contains_key("fresh"), "fresh nonce must survive");
    }

    fn envelope(signature: &str, nonce: &str) -> String {
        BASE64.encode(
            json!({"x402Version":1,"scheme":"exact-er","payload":{"signature":signature,"nonce":nonce}})
                .to_string(),
        )
    }

    /// Builds a getTransaction result whose instruction is a consume_credits of
    /// `amount` with `receipt_hash = sha256(receipt_nonce)`.
    fn consume_tx_result(amount: u64, receipt_nonce: &str, program: &str) -> serde_json::Value {
        let mut data = Vec::new();
        data.extend_from_slice(&consume_discriminator());
        data.extend_from_slice(&amount.to_le_bytes());
        data.extend_from_slice(&Sha256::digest(receipt_nonce.as_bytes()));
        let data_b58 = bs58::encode(data).into_string();
        json!({
            "meta": {"err": null},
            "transaction": {"message": {
                "accountKeys": ["ConfigPDA11111111111111111111111111111111", "CreditPDA1111111111111111111111111111111", "Owner11111111111111111111111111111111111", program],
                "instructions": [{"programIdIndex": 3, "accounts": [0, 1, 2], "data": data_b58}]
            }}
        })
    }

    async fn server_with(result: serde_json::Value) -> (MockServer, ErPaymentVerifier) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("getTransaction"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0", "id": 1, "result": result
            })))
            .mount(&server)
            .await;
        let verifier = ErPaymentVerifier::new(PROGRAM, server.uri());
        (server, verifier)
    }

    #[tokio::test]
    async fn verifies_a_good_payment() {
        let (_s, v) = server_with(consume_tx_result(5, "abc123", PROGRAM)).await;
        let out = v
            .verify(&envelope("SiGmAtUrE", "abc123"), "abc123", 5)
            .await
            .expect("verify");
        assert_eq!(out.amount, 5);
        assert_eq!(out.signature, "SiGmAtUrE");
        assert_eq!(out.credit_account, "CreditPDA1111111111111111111111111111111");
    }

    #[tokio::test]
    async fn rejects_nonce_mismatch_before_rpc() {
        let (_s, v) = server_with(consume_tx_result(5, "abc123", PROGRAM)).await;
        let err = v
            .verify(&envelope("sig", "different"), "abc123", 5)
            .await
            .unwrap_err();
        assert_eq!(err, VerifyError::NonceMismatch);
    }

    #[tokio::test]
    async fn rejects_underpayment() {
        let (_s, v) = server_with(consume_tx_result(2, "abc123", PROGRAM)).await;
        let err = v
            .verify(&envelope("sig", "abc123"), "abc123", 5)
            .await
            .unwrap_err();
        assert_eq!(err, VerifyError::Underpaid { paid: 2, required: 5 });
    }

    #[tokio::test]
    async fn rejects_receipt_not_bound_to_nonce() {
        // on-chain receipt_hash is sha256("wrong"), but the challenge nonce is "abc123"
        let (_s, v) = server_with(consume_tx_result(5, "wrong", PROGRAM)).await;
        let err = v
            .verify(&envelope("sig", "abc123"), "abc123", 5)
            .await
            .unwrap_err();
        assert_eq!(err, VerifyError::ReceiptMismatch);
    }

    #[tokio::test]
    async fn rejects_consume_to_a_different_program() {
        let other = "11111111111111111111111111111111";
        let (_s, v) = server_with(consume_tx_result(5, "abc123", other)).await;
        let err = v
            .verify(&envelope("sig", "abc123"), "abc123", 5)
            .await
            .unwrap_err();
        assert_eq!(err, VerifyError::NoConsume);
    }
}
