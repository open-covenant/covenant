//! Ephemeral-rollup payment signer for the x402 loop.
//!
//! Gated by the `solana` feature. Where [`crate::SolanaSigner`] pays by signing an
//! SPL `TransferChecked`, [`EphemeralSigner`] pays by metering: it runs the Covenant
//! settlement program's `consume_credits` against a credit account that has been
//! delegated to a MagicBlock ephemeral rollup, then returns the ER transaction
//! signature as proof. No SPL transfer, no per-call L1 fee — the consume is gasless
//! in the rollup.
//!
//! Session model: the caller delegates the credit account once (out of band), serves
//! many gasless paid calls through this signer, then undelegates. This signer only
//! does the per-call consume.
//!
//! ## Envelope
//!
//! Same outer shape as the Coinbase x402 reference, ER-specific payload:
//!
//! ```json
//! {
//!   "x402Version": 1,
//!   "scheme":      "<from requirements, e.g. exact-er>",
//!   "network":     "<from requirements>",
//!   "payload":     { "signature": "<ER tx sig>", "nonce": "<challenge nonce>",
//!                    "creditAccount": "<credits PDA>" }
//! }
//! ```
//!
//! The facilitator verifies the signature on the ER: the tx is a `consume_credits`
//! to the settlement program against `creditAccount`, `amount >= price`, and
//! `receipt_hash == sha256(nonce)` (binds the payment to this request). The nonce is
//! carried in the 402 challenge's `extra.nonce`.
//!
//! ## RPC
//!
//! Talks directly to the pinned ER validator RPC (where the delegated account lives)
//! with `getLatestBlockhash` / `sendTransaction` / `getSignatureStatuses`, via
//! `reqwest` — no `solana-client` dep, matching [`crate::SolanaSigner`].

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use sha2::{Digest, Sha256};
use solana_sdk::{
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signer::{
        keypair::{read_keypair_file, Keypair},
        Signer as SolanaKeypairSigner,
    },
    transaction::Transaction,
};
use tracing::debug;

use crate::http::{read_capped, MAX_RESPONSE_BYTES};
use crate::{PaymentRequirements, Result, Signer, X402Error};

/// Settlement-program instruction discriminator: `sha256("global:consume_credits")[..8]`.
fn consume_discriminator() -> [u8; 8] {
    let digest = Sha256::digest(b"global:consume_credits");
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

/// `receipt_hash = sha256(nonce)` — binds an on-chain consume to a specific 402
/// challenge so a facilitator can match payment to request and reject replays.
fn receipt_hash(nonce: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Sha256::digest(nonce.as_bytes()));
    out
}

/// Builds the `consume_credits(amount, receipt_hash)` instruction. Account order
/// matches the program's `ConsumeCredits`: `config` (ro), `credits` (w), `owner`
/// (signer, ro). PDAs are derived from `program_id`.
pub fn consume_instruction(
    program_id: &Pubkey,
    owner: &Pubkey,
    amount: u64,
    receipt_hash: [u8; 32],
) -> Instruction {
    let (config, _) = Pubkey::find_program_address(&[b"config"], program_id);
    let credits = credit_account(program_id, owner);
    let mut data = Vec::with_capacity(48);
    data.extend_from_slice(&consume_discriminator());
    data.extend_from_slice(&amount.to_le_bytes());
    data.extend_from_slice(&receipt_hash);
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new_readonly(config, false),
            AccountMeta::new(credits, false),
            AccountMeta::new_readonly(*owner, true),
        ],
        data,
    }
}

/// The credit-account PDA `[b"credits", owner]` for a settlement program.
pub fn credit_account(program_id: &Pubkey, owner: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"credits", owner.as_ref()], program_id).0
}

/// Settles x402 calls by metering a delegated credit account in an ephemeral rollup.
pub struct EphemeralSigner {
    keypair: Keypair,
    program_id: Pubkey,
    /// Pinned ER validator RPC where the delegated account lives.
    er_rpc_url: String,
    http: reqwest::Client,
    max_bytes: usize,
}

impl EphemeralSigner {
    pub fn new(keypair: Keypair, program_id: Pubkey, er_rpc_url: impl Into<String>) -> Self {
        Self {
            keypair,
            program_id,
            er_rpc_url: er_rpc_url.into(),
            http: reqwest::Client::new(),
            max_bytes: MAX_RESPONSE_BYTES,
        }
    }

    pub fn from_keypair_file(
        path: impl AsRef<Path>,
        program_id: Pubkey,
        er_rpc_url: impl Into<String>,
    ) -> Result<Self> {
        let path = path.as_ref();
        let keypair = read_keypair_file(path)
            .map_err(|e| X402Error::Sign(format!("read funding keypair {}: {e}", path.display())))?;
        Ok(Self::new(keypair, program_id, er_rpc_url))
    }

    pub fn pubkey(&self) -> Pubkey {
        self.keypair.pubkey()
    }

    /// The credit-account PDA this signer meters.
    pub fn credit_account(&self) -> Pubkey {
        credit_account(&self.program_id, &self.keypair.pubkey())
    }

    async fn rpc(&self, body: serde_json::Value) -> Result<serde_json::Value> {
        let resp = self.http.post(&self.er_rpc_url).json(&body).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = read_capped(resp, self.max_bytes, X402Error::Sign)
                .await
                .unwrap_or_default();
            return Err(X402Error::Sign(format!("ER rpc status {status}: {text}")));
        }
        let text = read_capped(resp, self.max_bytes, X402Error::Sign).await?;
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| X402Error::Sign(format!("decode ER rpc response: {e}")))?;
        if let Some(err) = parsed.get("error") {
            return Err(X402Error::Sign(format!("ER rpc error: {err}")));
        }
        Ok(parsed)
    }

    async fn latest_blockhash(&self) -> Result<Hash> {
        let parsed = self
            .rpc(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "getLatestBlockhash",
                "params": [{"commitment": "confirmed"}]
            }))
            .await?;
        let s = parsed
            .pointer("/result/value/blockhash")
            .and_then(|v| v.as_str())
            .ok_or_else(|| X402Error::Sign(format!("ER rpc: no blockhash: {parsed}")))?;
        Hash::from_str(s).map_err(|e| X402Error::Sign(format!("parse blockhash: {e}")))
    }

    /// Submits the signed consume to the ER and waits for confirmation, so a
    /// facilitator querying the signature right away will find it.
    async fn submit_and_confirm(&self, tx: &Transaction) -> Result<String> {
        let serialized =
            bincode::serialize(tx).map_err(|e| X402Error::Sign(format!("serialize tx: {e}")))?;
        let tx_b64 = BASE64.encode(serialized);
        let parsed = self
            .rpc(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "sendTransaction",
                "params": [tx_b64, {"encoding": "base64", "preflightCommitment": "confirmed"}]
            }))
            .await?;
        let sig = parsed
            .pointer("/result")
            .and_then(|v| v.as_str())
            .ok_or_else(|| X402Error::Sign(format!("ER rpc: no signature: {parsed}")))?
            .to_string();

        for _ in 0..25 {
            let parsed = self
                .rpc(serde_json::json!({
                    "jsonrpc": "2.0", "id": 1, "method": "getSignatureStatuses",
                    "params": [[sig], {"searchTransactionHistory": false}]
                }))
                .await?;
            let status = parsed.pointer("/result/value/0");
            if let Some(status) = status.filter(|s| !s.is_null()) {
                if let Some(err) = status.get("err").filter(|e| !e.is_null()) {
                    return Err(X402Error::Sign(format!("consume failed on ER: {err}")));
                }
                let level = status
                    .get("confirmationStatus")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if level == "confirmed" || level == "finalized" {
                    return Ok(sig);
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        Err(X402Error::Sign(format!(
            "consume {sig} not confirmed on ER within timeout"
        )))
    }
}

#[async_trait::async_trait]
impl Signer for EphemeralSigner {
    async fn build_payment(&self, requirements: &PaymentRequirements) -> Result<String> {
        let amount: u64 = requirements
            .amount
            .parse()
            .map_err(|e| X402Error::Sign(format!("parse amount {:?}: {e}", requirements.amount)))?;
        let nonce = requirements
            .extra
            .as_ref()
            .and_then(|x| x.nonce.as_deref())
            .ok_or_else(|| {
                X402Error::Sign("ER payment requires a nonce in the 402 challenge's extra".into())
            })?;

        let owner = self.keypair.pubkey();
        let ix = consume_instruction(&self.program_id, &owner, amount, receipt_hash(nonce));
        let blockhash = self.latest_blockhash().await?;
        let mut tx = Transaction::new_with_payer(&[ix], Some(&owner));
        tx.try_sign(&[&self.keypair], blockhash)
            .map_err(|e| X402Error::Sign(format!("sign consume: {e}")))?;

        debug!(payer = %owner, amount, "EphemeralSigner metering consume in ER");
        let signature = self.submit_and_confirm(&tx).await?;

        let envelope = serde_json::json!({
            "x402Version": 1,
            "scheme":  requirements.scheme,
            "network": requirements.network,
            "payload": {
                "signature":     signature,
                "nonce":         nonce,
                "creditAccount": self.credit_account().to_string(),
            },
        });
        Ok(BASE64.encode(envelope.to_string().as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PaymentExtra;
    use wiremock::matchers::{body_string_contains, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn keypair() -> Keypair {
        Keypair::new()
    }

    #[test]
    fn consume_instruction_layout() {
        let program = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let receipt = [7u8; 32];
        let ix = consume_instruction(&program, &owner, 42, receipt);

        assert_eq!(ix.program_id, program);
        assert_eq!(ix.accounts.len(), 3);
        // [config (ro), credits (w), owner (signer, ro)]
        assert!(!ix.accounts[0].is_writable && !ix.accounts[0].is_signer);
        assert!(ix.accounts[1].is_writable && !ix.accounts[1].is_signer);
        assert_eq!(ix.accounts[1].pubkey, credit_account(&program, &owner));
        assert!(ix.accounts[2].is_signer && !ix.accounts[2].is_writable);
        assert_eq!(ix.accounts[2].pubkey, owner);

        assert_eq!(&ix.data[..8], &consume_discriminator());
        assert_eq!(&ix.data[8..16], &42u64.to_le_bytes());
        assert_eq!(&ix.data[16..48], &receipt);
    }

    #[test]
    fn receipt_hash_is_sha256_of_nonce() {
        let mut expected = [0u8; 32];
        expected.copy_from_slice(&Sha256::digest(b"abc"));
        assert_eq!(receipt_hash("abc"), expected);
    }

    fn er_requirements(nonce: Option<&str>) -> PaymentRequirements {
        PaymentRequirements {
            network: "solana-er:devnet".into(),
            asset: "credits".into(),
            amount: "1".into(),
            amount_usdc: 0.0,
            pay_to: "credits".into(),
            scheme: "exact-er".into(),
            extra: nonce.map(|n| PaymentExtra {
                fee_payer: None,
                nonce: Some(n.into()),
            }),
        }
    }

    #[tokio::test]
    async fn missing_nonce_is_a_sign_error() {
        let signer = EphemeralSigner::new(keypair(), Pubkey::new_unique(), "http://unused");
        let err = signer
            .build_payment(&er_requirements(None))
            .await
            .expect_err("must require a nonce");
        assert!(matches!(err, X402Error::Sign(_)));
    }

    #[tokio::test]
    async fn build_payment_meters_and_wraps_signature() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("getLatestBlockhash"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1,
                "result": {"context": {"slot": 1},
                           "value": {"blockhash": "11111111111111111111111111111111", "lastValidBlockHeight": 1}}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("sendTransaction"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "result": "5SIGtestsignaturevalue"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("getSignatureStatuses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1,
                "result": {"context": {"slot": 1}, "value": [{"confirmationStatus": "confirmed", "err": null}]}
            })))
            .mount(&server)
            .await;

        let kp = keypair();
        let owner = kp.pubkey();
        let program = Pubkey::new_unique();
        let signer = EphemeralSigner::new(kp, program, server.uri());

        let header = signer
            .build_payment(&er_requirements(Some("deadbeef")))
            .await
            .expect("build_payment");

        let json: serde_json::Value =
            serde_json::from_slice(&BASE64.decode(header).expect("b64")).expect("json");
        assert_eq!(json["x402Version"], 1);
        assert_eq!(json["scheme"], "exact-er");
        assert_eq!(json["payload"]["signature"], "5SIGtestsignaturevalue");
        assert_eq!(json["payload"]["nonce"], "deadbeef");
        assert_eq!(
            json["payload"]["creditAccount"],
            credit_account(&program, &owner).to_string()
        );
    }
}
