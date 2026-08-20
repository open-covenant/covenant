//! Per-request payment gate for the paid routes, following the x402 shape: an
//! unpaid request gets `402` with the `accepts[]` that describe how to pay, and a
//! retry carrying a valid `X-PAYMENT` header is verified before it is served.
//!
//! The verifier is a trait so the settlement backend is swappable. The one wired
//! today, [`DevPrefundedVerifier`], settles nothing — it matches a configured dev
//! allow-token and guards against replay, which is enough to prove the whole
//! `402 → pay → serve → receipt` loop end to end without touching a chain or a key.
//! The real-money path, [`SolanaUsdcVerifier`], is a deliberately inert seam.

use std::collections::HashSet;
use std::sync::Mutex;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};

/// The `402` challenge body. Mirrors the x402-v1 wire shape: a version, the list of
/// acceptable payment options, and a human-readable error.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PaymentRequired {
    #[serde(rename = "x402Version")]
    pub version: u32,
    pub accepts: Vec<Accept>,
    pub error: String,
}

/// One acceptable way to pay for the resource.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Accept {
    pub scheme: String,
    pub network: String,
    #[serde(rename = "maxAmountRequired")]
    pub max_amount_required: String,
    pub resource: String,
    #[serde(rename = "payTo")]
    pub pay_to: String,
    pub asset: String,
}

/// What a payment is being checked against: the priced resource and the rail the
/// gateway advertises. Built per request from the metering config.
#[derive(Debug, Clone)]
pub struct PaymentContext {
    pub resource: String,
    pub price_micro: u64,
    pub pay_to: String,
    pub network: String,
    pub asset: String,
}

impl PaymentContext {
    pub fn challenge(&self, error: &str) -> PaymentRequired {
        PaymentRequired {
            version: 1,
            accepts: vec![Accept {
                scheme: "exact".into(),
                network: self.network.clone(),
                max_amount_required: self.price_micro.to_string(),
                resource: self.resource.clone(),
                pay_to: self.pay_to.clone(),
                asset: self.asset.clone(),
            }],
            error: error.into(),
        }
    }
}

/// The decoded `X-PAYMENT` header. The envelope mirrors the Solana x402 payment
/// envelope (`x402Version`/`scheme`/`network`/`payload`); the fields the dev
/// verifier reads live under `payload`.
#[derive(Debug, Clone, Deserialize)]
pub struct PaymentPayload {
    #[serde(rename = "x402Version", default)]
    pub version: u32,
    pub scheme: String,
    pub network: String,
    pub payload: PaymentInner,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PaymentInner {
    /// Single-use idempotency key. The replay ledger is keyed on this.
    pub reference: String,
    /// Dev prefunded allow-token. Ignored by the real-money verifier.
    #[serde(default)]
    pub credential: String,
    /// Atomic amount offered, as a decimal string (mirrors x402 `maxAmountRequired`).
    pub amount: String,
    #[serde(rename = "payTo", default)]
    pub pay_to: String,
    #[serde(default)]
    pub asset: String,
}

/// A verified payment, threaded into the receipt and used to refund on downstream
/// failure. Carries no key material or on-chain proof — that belongs to whichever
/// verifier produced it.
#[derive(Debug, Clone)]
pub struct PaymentReceipt {
    pub reference: String,
    pub amount_micro: u64,
    pub network: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PaymentError {
    #[error("malformed payment payload")]
    Malformed,
    #[error("unsupported payment scheme")]
    UnsupportedScheme,
    #[error("payment does not match the requirement: {0}")]
    Mismatch(&'static str),
    #[error("payment credential rejected")]
    BadCredential,
    #[error("payment below the required amount")]
    Insufficient,
    #[error("payment reference already used")]
    Replay,
    #[error("real-money settlement is gated")]
    Gated,
}

pub fn decode_payment(header: &str) -> Result<PaymentPayload, PaymentError> {
    let raw = BASE64
        .decode(header.trim())
        .map_err(|_| PaymentError::Malformed)?;
    serde_json::from_slice(&raw).map_err(|_| PaymentError::Malformed)
}

/// Verifies a payment against the requirement it claims to satisfy. Implementations
/// own their settlement backend and their replay guard.
pub trait PaymentVerifier: Send + Sync {
    fn verify(
        &self,
        payment: &PaymentPayload,
        req: &PaymentContext,
    ) -> Result<PaymentReceipt, PaymentError>;

    /// Reverse a previously-verified payment when the request it paid for could not
    /// be served. The default is a no-op; verifiers with a replay ledger release the
    /// reference so the caller can retry with the same payment.
    fn refund(&self, _receipt: &PaymentReceipt) {}
}

/// In-memory single-use guard keyed by payment reference. `reserve` is the replay
/// check; `release` backs the refund-on-failure path.
#[derive(Default)]
pub struct ReplayLedger {
    seen: Mutex<HashSet<String>>,
}

impl ReplayLedger {
    pub fn reserve(&self, reference: &str) -> bool {
        self.seen
            .lock()
            .expect("replay ledger mutex poisoned")
            .insert(reference.to_owned())
    }

    pub fn release(&self, reference: &str) {
        self.seen
            .lock()
            .expect("replay ledger mutex poisoned")
            .remove(reference);
    }
}

/// Accepts a payment that carries the configured dev allow-token and a fresh
/// reference. It settles nothing: it exists so the metering flow — challenge, pay,
/// serve, receipt — is exercisable without a chain, a facilitator, or a funding key.
pub struct DevPrefundedVerifier {
    allow_token: String,
    ledger: ReplayLedger,
}

impl DevPrefundedVerifier {
    pub fn new(allow_token: impl Into<String>) -> Self {
        Self {
            allow_token: allow_token.into(),
            ledger: ReplayLedger::default(),
        }
    }
}

impl PaymentVerifier for DevPrefundedVerifier {
    fn verify(
        &self,
        payment: &PaymentPayload,
        req: &PaymentContext,
    ) -> Result<PaymentReceipt, PaymentError> {
        if payment.scheme != "exact" {
            return Err(PaymentError::UnsupportedScheme);
        }
        if payment.network != req.network {
            return Err(PaymentError::Mismatch("network"));
        }
        let inner = &payment.payload;
        if !inner.pay_to.is_empty() && inner.pay_to != req.pay_to {
            return Err(PaymentError::Mismatch("payTo"));
        }
        if !inner.asset.is_empty() && inner.asset != req.asset {
            return Err(PaymentError::Mismatch("asset"));
        }
        if inner.reference.trim().is_empty() {
            return Err(PaymentError::Malformed);
        }
        if !constant_time_eq(&inner.credential, &self.allow_token) {
            return Err(PaymentError::BadCredential);
        }
        let amount: u64 = inner.amount.parse().map_err(|_| PaymentError::Malformed)?;
        if amount < req.price_micro {
            return Err(PaymentError::Insufficient);
        }
        if !self.ledger.reserve(&inner.reference) {
            return Err(PaymentError::Replay);
        }
        Ok(PaymentReceipt {
            reference: inner.reference.clone(),
            amount_micro: req.price_micro,
            network: req.network.clone(),
        })
    }

    fn refund(&self, receipt: &PaymentReceipt) {
        self.ledger.release(&receipt.reference);
    }
}

/// Real-money verifier seam. A live implementation would confirm a Solana USDC
/// transfer (or an EIP-3009-style transfer intent) against RPC using the
/// covenant-x402 envelope shapes, bind the payer to the settled amount, and only
/// then release the request. None of that is wired here.
pub struct SolanaUsdcVerifier {
    _private: (),
}

impl SolanaUsdcVerifier {
    /// Constructs the inert seam. It is also the safe default verifier: if payment
    /// is switched on without a real verifier configured, every payment is refused
    /// rather than silently accepted.
    pub fn gated() -> Self {
        Self { _private: () }
    }
}

impl PaymentVerifier for SolanaUsdcVerifier {
    fn verify(
        &self,
        _payment: &PaymentPayload,
        _req: &PaymentContext,
    ) -> Result<PaymentReceipt, PaymentError> {
        // GATED: real-money cutover, operator-approved task. No RPC client, no mint,
        // and no funding key is wired here on purpose.
        Err(PaymentError::Gated)
    }
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> PaymentContext {
        PaymentContext {
            resource: "/v1/chat/completions".into(),
            price_micro: 1_000,
            pay_to: "PayToWallet".into(),
            network: "solana:devnet".into(),
            asset: "UsdcDevnetMint".into(),
        }
    }

    fn payload(reference: &str, credential: &str, amount: &str) -> PaymentPayload {
        PaymentPayload {
            version: 1,
            scheme: "exact".into(),
            network: "solana:devnet".into(),
            payload: PaymentInner {
                reference: reference.into(),
                credential: credential.into(),
                amount: amount.into(),
                pay_to: "PayToWallet".into(),
                asset: "UsdcDevnetMint".into(),
            },
        }
    }

    #[test]
    fn challenge_body_has_the_x402_shape_and_fields() {
        let json = serde_json::to_value(context().challenge("payment required")).unwrap();
        assert_eq!(json["x402Version"], 1);
        assert_eq!(json["error"], "payment required");
        let accept = &json["accepts"][0];
        assert_eq!(accept["scheme"], "exact");
        assert_eq!(accept["network"], "solana:devnet");
        assert_eq!(accept["maxAmountRequired"], "1000");
        assert_eq!(accept["resource"], "/v1/chat/completions");
        assert_eq!(accept["payTo"], "PayToWallet");
        assert_eq!(accept["asset"], "UsdcDevnetMint");
    }

    #[test]
    fn dev_verifier_accepts_a_matching_prefunded_payment() {
        let verifier = DevPrefundedVerifier::new("dev-token");
        let receipt = verifier
            .verify(&payload("ref-1", "dev-token", "1000"), &context())
            .unwrap();
        assert_eq!(receipt.reference, "ref-1");
        assert_eq!(receipt.amount_micro, 1_000);
    }

    #[test]
    fn dev_verifier_rejects_a_bad_credential_and_wrong_rail() {
        let verifier = DevPrefundedVerifier::new("dev-token");
        assert!(matches!(
            verifier.verify(&payload("ref-1", "wrong", "1000"), &context()),
            Err(PaymentError::BadCredential)
        ));
        let mut cheap = payload("ref-2", "dev-token", "999");
        assert!(matches!(
            verifier.verify(&cheap, &context()),
            Err(PaymentError::Insufficient)
        ));
        cheap.network = "solana:mainnet".into();
        cheap.payload.amount = "1000".into();
        assert!(matches!(
            verifier.verify(&cheap, &context()),
            Err(PaymentError::Mismatch("network"))
        ));
    }

    #[test]
    fn dev_verifier_rejects_a_replayed_reference_until_refunded() {
        let verifier = DevPrefundedVerifier::new("dev-token");
        let receipt = verifier
            .verify(&payload("ref-1", "dev-token", "1000"), &context())
            .unwrap();
        assert!(matches!(
            verifier.verify(&payload("ref-1", "dev-token", "1000"), &context()),
            Err(PaymentError::Replay)
        ));
        verifier.refund(&receipt);
        assert!(verifier
            .verify(&payload("ref-1", "dev-token", "1000"), &context())
            .is_ok());
    }

    #[test]
    fn real_money_verifier_is_gated() {
        let verifier = SolanaUsdcVerifier::gated();
        assert!(matches!(
            verifier.verify(&payload("ref-1", "anything", "1000"), &context()),
            Err(PaymentError::Gated)
        ));
    }

    #[test]
    fn payment_header_round_trips_through_base64() {
        let encoded = BASE64.encode(
            serde_json::to_vec(&serde_json::json!({
                "x402Version": 1,
                "scheme": "exact",
                "network": "solana:devnet",
                "payload": { "reference": "ref-9", "credential": "dev-token", "amount": "1000" }
            }))
            .unwrap(),
        );
        let decoded = decode_payment(&encoded).unwrap();
        assert_eq!(decoded.payload.reference, "ref-9");
        assert!(matches!(
            decode_payment("not-base64!!"),
            Err(PaymentError::Malformed)
        ));
    }
}
