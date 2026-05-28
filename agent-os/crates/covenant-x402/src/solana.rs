//! Solana SPL signer for the x402 payment loop.
//!
//! Gated by the `solana` cargo feature. When the feature is on, the
//! crate exports [`SolanaSigner`] — a [`crate::Signer`]
//! implementation that builds an SPL `TransferChecked` instruction
//! against the recipient's Associated Token Account, signs it with
//! a [`solana_sdk::signer::keypair::Keypair`], and wraps the signed
//! transaction in the canonical x402 payment envelope.
//!
//! ## Envelope
//!
//! The `x-payment` header value is a base64-encoded JSON envelope:
//!
//! ```json
//! {
//!   "x402Version": 1,
//!   "scheme":      "<from requirements>",
//!   "network":     "<from requirements>",
//!   "payload":     { "transaction": "<base64 of bincode-serialized signed tx>" }
//! }
//! ```
//!
//! This matches the Coinbase x402 reference. Facilitators that
//! expect a different envelope (Kamiyo's Kizuna, for example) need
//! a parallel signer impl — the trait surface stays the same.
//!
//! ## Decimals
//!
//! `TransferChecked` requires the mint's decimal count. USDC
//! mainnet + devnet (both 6) resolve through a hardcoded fast path
//! with no RPC. Any other mint is resolved on-chain via
//! `getAccountInfo` (`jsonParsed`) — see [`SolanaSigner::resolve_decimals`].
//! A mint that does not exist or is not an SPL mint surfaces
//! [`crate::X402Error::Sign`].
//!
//! ## RPC dependency
//!
//! The signer needs a fresh blockhash. It hits the configured RPC
//! URL with a minimal `getLatestBlockhash` JSON-RPC call (no
//! `solana-client` dep — just `reqwest`).

use std::path::Path;
use std::str::FromStr;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use solana_sdk::{
    hash::Hash,
    pubkey::Pubkey,
    signer::{
        keypair::{read_keypair_file, Keypair},
        Signer as SolanaKeypairSigner,
    },
    transaction::Transaction,
};
use spl_associated_token_account::{
    get_associated_token_address,
    instruction::create_associated_token_account_idempotent,
};
use tracing::debug;

use crate::{PaymentRequirements, Result, Signer, X402Error};

/// USDC mint on Solana mainnet-beta.
pub const USDC_MAINNET_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
/// USDC mint on Solana devnet (Circle's official devnet mint).
pub const USDC_DEVNET_MINT: &str = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU";

/// Real Solana payment signer.
pub struct SolanaSigner {
    keypair: Keypair,
    rpc_url: String,
    http: reqwest::Client,
}

impl SolanaSigner {
    /// Builds a signer with a default reqwest client.
    pub fn new(keypair: Keypair, rpc_url: impl Into<String>) -> Self {
        Self::with(keypair, rpc_url, reqwest::Client::new())
    }

    /// Customised builder — pass an existing reqwest client when
    /// you want to share connection pooling.
    pub fn with(
        keypair: Keypair,
        rpc_url: impl Into<String>,
        http: reqwest::Client,
    ) -> Self {
        Self {
            keypair,
            rpc_url: rpc_url.into(),
            http,
        }
    }

    /// Loads the funding keypair from a Solana CLI keypair file (the
    /// JSON byte-array format `solana-keygen` writes) and builds a
    /// signer. The error message deliberately omits the key bytes —
    /// only the path and the underlying reason are surfaced.
    pub fn from_keypair_file(
        path: impl AsRef<Path>,
        rpc_url: impl Into<String>,
    ) -> Result<Self> {
        let path = path.as_ref();
        let keypair = read_keypair_file(path).map_err(|e| {
            X402Error::Sign(format!("read funding keypair {}: {e}", path.display()))
        })?;
        Ok(Self::new(keypair, rpc_url))
    }

    /// The signer's pubkey, useful for funding the account or
    /// pre-creating the sender's ATA.
    pub fn pubkey(&self) -> Pubkey {
        self.keypair.pubkey()
    }

    /// Resolves the decimal count for an SPL mint.
    ///
    /// Tries the hardcoded fast path ([`decimals_for_mint`]) first so
    /// USDC payments never incur an extra RPC round-trip. For any
    /// other mint it falls back to `getAccountInfo` with `jsonParsed`
    /// encoding and reads `value.data.parsed.info.decimals`. This is
    /// what lets the signer pay providers in the orbit registry that
    /// price in mints other than USDC.
    pub async fn resolve_decimals(&self, mint: &Pubkey) -> Result<u8> {
        if let Some(d) = decimals_for_mint(mint) {
            return Ok(d);
        }
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getAccountInfo",
            "params": [
                mint.to_string(),
                {"encoding": "jsonParsed", "commitment": "confirmed"}
            ]
        });
        let resp = self.http.post(&self.rpc_url).json(&body).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(X402Error::Sign(format!(
                "rpc getAccountInfo status {}: {}",
                status,
                resp.text().await.unwrap_or_default()
            )));
        }
        let parsed: serde_json::Value = resp.json().await?;
        // A missing account surfaces as `result.value == null`.
        if parsed.pointer("/result/value").map(|v| v.is_null()) == Some(true) {
            return Err(X402Error::Sign(format!(
                "mint {mint} not found on-chain"
            )));
        }
        let decimals = parsed
            .pointer("/result/value/data/parsed/info/decimals")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                X402Error::Sign(format!(
                    "mint {mint}: no parsed decimals in getAccountInfo response \
                     (not an SPL mint, or node lacks jsonParsed support)"
                ))
            })?;
        u8::try_from(decimals).map_err(|_| {
            X402Error::Sign(format!("mint {mint}: implausible decimals {decimals}"))
        })
    }

    async fn latest_blockhash(&self) -> Result<Hash> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getLatestBlockhash",
            "params": [{"commitment": "confirmed"}]
        });
        let resp = self.http.post(&self.rpc_url).json(&body).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(X402Error::Sign(format!(
                "rpc status {}: {}",
                status,
                resp.text().await.unwrap_or_default()
            )));
        }
        let parsed: serde_json::Value = resp.json().await?;
        let blockhash_str = parsed
            .pointer("/result/value/blockhash")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                X402Error::Sign(format!(
                    "rpc: no blockhash in response: {}",
                    parsed
                ))
            })?;
        Hash::from_str(blockhash_str)
            .map_err(|e| X402Error::Sign(format!("parse blockhash: {e}")))
    }
}

#[async_trait::async_trait]
impl Signer for SolanaSigner {
    async fn build_payment(
        &self,
        requirements: &PaymentRequirements,
    ) -> Result<String> {
        if !requirements.network.starts_with("solana:") {
            return Err(X402Error::Sign(format!(
                "SolanaSigner cannot handle network {:?}",
                requirements.network
            )));
        }

        let mint = Pubkey::from_str(&requirements.asset)
            .map_err(|e| X402Error::Sign(format!("parse asset {:?}: {e}", requirements.asset)))?;
        let pay_to = Pubkey::from_str(&requirements.pay_to)
            .map_err(|e| X402Error::Sign(format!("parse pay_to {:?}: {e}", requirements.pay_to)))?;
        let amount: u64 = requirements
            .amount
            .parse()
            .map_err(|e| X402Error::Sign(format!("parse amount {:?}: {e}", requirements.amount)))?;
        let decimals = self.resolve_decimals(&mint).await?;

        let blockhash = self.latest_blockhash().await?;
        debug!(
            payer = %self.pubkey(),
            mint = %mint,
            recipient = %pay_to,
            amount,
            "SolanaSigner building transfer"
        );

        let tx = build_transfer_transaction(
            &self.keypair,
            mint,
            pay_to,
            amount,
            decimals,
            blockhash,
        )?;

        let serialized = bincode::serialize(&tx)
            .map_err(|e| X402Error::Sign(format!("serialize tx: {e}")))?;
        let tx_b64 = BASE64.encode(serialized);

        let envelope = serde_json::json!({
            "x402Version": 1,
            "scheme":      requirements.scheme,
            "network":     requirements.network,
            "payload":     { "transaction": tx_b64 },
        });
        Ok(BASE64.encode(envelope.to_string().as_bytes()))
    }
}

/// Looks up decimals for known SPL mints. Returns None for unknown
/// mints — callers should surface that as a configuration error so
/// the operator knows to register the mint.
pub fn decimals_for_mint(mint: &Pubkey) -> Option<u8> {
    let s = mint.to_string();
    if s == USDC_MAINNET_MINT || s == USDC_DEVNET_MINT {
        Some(6)
    } else {
        None
    }
}

/// Builds a signed transaction that pays `recipient` in `mint`.
///
/// Emits two instructions:
/// 1. An idempotent `CreateAssociatedTokenAccount` for the
///    recipient's ATA. It is a no-op when the ATA already exists
///    (the common case for established payTo addresses) and creates
///    it otherwise — the payer funds the rent. This removes the
///    "recipient has no ATA → SPL rejects the transfer" failure mode
///    without an extra pre-flight RPC.
/// 2. The `TransferChecked` from the payer's ATA to the recipient's.
///
/// The payer's own ATA is assumed to exist — if it does not, the
/// payer holds no balance of `mint` to spend and the call could not
/// succeed regardless.
pub fn build_transfer_transaction(
    payer: &Keypair,
    mint: Pubkey,
    recipient: Pubkey,
    amount: u64,
    decimals: u8,
    recent_blockhash: Hash,
) -> Result<Transaction> {
    let payer_pubkey = payer.pubkey();
    let source_ata = get_associated_token_address(&payer_pubkey, &mint);
    let dest_ata = get_associated_token_address(&recipient, &mint);

    let create_dest_ata = create_associated_token_account_idempotent(
        &payer_pubkey,
        &recipient,
        &mint,
        &spl_token::ID,
    );

    let transfer = spl_token::instruction::transfer_checked(
        &spl_token::ID,
        &source_ata,
        &mint,
        &dest_ata,
        &payer_pubkey,
        &[&payer_pubkey],
        amount,
        decimals,
    )
    .map_err(|e| X402Error::Sign(format!("build transfer_checked: {e}")))?;

    let mut tx =
        Transaction::new_with_payer(&[create_dest_ata, transfer], Some(&payer_pubkey));
    tx.try_sign(&[payer], recent_blockhash)
        .map_err(|e| X402Error::Sign(format!("sign tx: {e}")))?;
    Ok(tx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

    fn xona_requirements(network: &str, asset: &str, amount: &str) -> PaymentRequirements {
        PaymentRequirements {
            network: network.into(),
            asset: asset.into(),
            amount: amount.into(),
            amount_usdc: 0.08,
            pay_to: "9VaDVp1Wb78G4Wm6VuTiMrpESjrUymXefQTHcJGRSTEA".into(),
            scheme: "exact".into(),
            extra: None,
        }
    }

    #[test]
    fn from_keypair_file_round_trips_pubkey() {
        use solana_sdk::signer::keypair::write_keypair_file;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("funding.json");
        let kp = Keypair::new();
        let expected = kp.pubkey();
        write_keypair_file(&kp, path.to_str().unwrap()).expect("write keypair");

        let signer =
            SolanaSigner::from_keypair_file(&path, "https://rpc.example/").expect("load");
        assert_eq!(signer.pubkey(), expected);
    }

    #[test]
    fn from_keypair_file_errors_on_missing_path() {
        let result = SolanaSigner::from_keypair_file(
            "/nonexistent/path/funding.json",
            "https://rpc.example/",
        );
        match result {
            Ok(_) => panic!("expected error for missing file"),
            Err(X402Error::Sign(msg)) => assert!(msg.contains("read funding keypair"), "got: {msg}"),
            Err(e) => panic!("expected Sign error, got: {e:?}"),
        }
    }

    #[test]
    fn usdc_mainnet_decimals_are_six() {
        let mint = Pubkey::from_str(USDC_MAINNET_MINT).unwrap();
        assert_eq!(decimals_for_mint(&mint), Some(6));
    }

    #[test]
    fn usdc_devnet_decimals_are_six() {
        let mint = Pubkey::from_str(USDC_DEVNET_MINT).unwrap();
        assert_eq!(decimals_for_mint(&mint), Some(6));
    }

    #[test]
    fn unknown_mint_returns_none() {
        // Arbitrary, valid base58 pubkey that isn't one of the
        // hardcoded USDC mints.
        let mint = Pubkey::from_str("11111111111111111111111111111111").unwrap();
        assert!(decimals_for_mint(&mint).is_none());
    }

    #[test]
    fn build_transaction_has_single_signature_and_two_instructions() {
        let payer = Keypair::new();
        let mint = Pubkey::from_str(USDC_MAINNET_MINT).unwrap();
        let recipient = Pubkey::from_str(
            "9VaDVp1Wb78G4Wm6VuTiMrpESjrUymXefQTHcJGRSTEA",
        )
        .unwrap();
        let blockhash = Hash::new_from_array([7u8; 32]);

        let tx = build_transfer_transaction(&payer, mint, recipient, 80_000, 6, blockhash)
            .expect("build tx");

        assert_eq!(tx.signatures.len(), 1, "payer is the only signer");
        assert_eq!(
            tx.message.instructions.len(),
            2,
            "idempotent ATA-create followed by TransferChecked",
        );
        assert_eq!(
            tx.message.recent_blockhash, blockhash,
            "recent_blockhash must round-trip from input",
        );
    }

    #[test]
    fn build_transaction_targets_correct_atas() {
        let payer = Keypair::new();
        let mint = Pubkey::from_str(USDC_MAINNET_MINT).unwrap();
        let recipient = Pubkey::from_str(
            "9VaDVp1Wb78G4Wm6VuTiMrpESjrUymXefQTHcJGRSTEA",
        )
        .unwrap();
        let blockhash = Hash::new_from_array([7u8; 32]);

        let tx = build_transfer_transaction(&payer, mint, recipient, 80_000, 6, blockhash)
            .expect("build tx");

        let expected_source = get_associated_token_address(&payer.pubkey(), &mint);
        let expected_dest = get_associated_token_address(&recipient, &mint);

        // Account keys order: signer/payer first, then writable
        // accounts in instruction order. The TransferChecked
        // instruction references source, mint, destination, owner.
        let keys = &tx.message.account_keys;
        assert!(keys.contains(&expected_source), "source ATA must appear in account keys");
        assert!(keys.contains(&expected_dest), "dest ATA must appear in account keys");
        assert!(keys.contains(&mint), "mint must appear in account keys");
    }

    #[tokio::test]
    async fn build_payment_round_trips_envelope() {
        // Wiremock acts as the RPC. It serves a fixed blockhash so
        // the test can assert on the envelope structure
        // deterministically.
        let server = MockServer::start().await;
        let fixed_blockhash = "11111111111111111111111111111112"; // base58 of [0,..,0,1]
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "context": {"slot": 1},
                        "value": {
                            "blockhash": fixed_blockhash,
                            "lastValidBlockHeight": 1
                        }
                    }
                }),
            ))
            .mount(&server)
            .await;

        let payer = Keypair::new();
        let signer = SolanaSigner::new(payer, server.uri());

        let req = xona_requirements(
            "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
            USDC_MAINNET_MINT,
            "80000",
        );
        let header = signer.build_payment(&req).await.expect("build");

        // The header is base64(JSON). Round-trip to verify the
        // envelope shape we promised in the module docstring.
        let envelope_bytes = BASE64.decode(header).expect("base64 decode");
        let envelope: serde_json::Value =
            serde_json::from_slice(&envelope_bytes).expect("json decode");
        assert_eq!(envelope["x402Version"], 1);
        assert_eq!(envelope["scheme"], "exact");
        assert_eq!(envelope["network"], req.network);
        assert!(envelope["payload"]["transaction"]
            .as_str()
            .map(|s| !s.is_empty())
            .unwrap_or(false));
    }

    #[tokio::test]
    async fn build_payment_rejects_non_solana_network() {
        let server = MockServer::start().await; // never hit
        let signer = SolanaSigner::new(Keypair::new(), server.uri());
        let req = xona_requirements("base:8453", USDC_MAINNET_MINT, "80000");
        let err = signer.build_payment(&req).await.expect_err("non-solana");
        assert!(matches!(err, X402Error::Sign(msg) if msg.contains("base:8453")));
    }

    #[tokio::test]
    async fn build_payment_rejects_missing_mint_account() {
        // Unknown mint → on-chain lookup. The RPC reports the account
        // does not exist (`value: null`); build_payment must surface
        // that as a Sign error rather than charging against a
        // nonexistent mint.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": { "context": {"slot": 1}, "value": null }
            })))
            .mount(&server)
            .await;
        let signer = SolanaSigner::new(Keypair::new(), server.uri());
        let req = xona_requirements(
            "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
            "11111111111111111111111111111111",
            "80000",
        );
        let err = signer.build_payment(&req).await.expect_err("missing mint");
        assert!(matches!(err, X402Error::Sign(msg) if msg.contains("not found on-chain")));
    }

    #[tokio::test]
    async fn resolve_decimals_uses_hardcoded_path_without_rpc() {
        // No mock mounted: if resolve_decimals hit the RPC for USDC
        // it would 404 and error. It must short-circuit instead.
        let server = MockServer::start().await;
        let signer = SolanaSigner::new(Keypair::new(), server.uri());
        let mint = Pubkey::from_str(USDC_MAINNET_MINT).unwrap();
        assert_eq!(signer.resolve_decimals(&mint).await.expect("hardcoded"), 6);
    }

    #[tokio::test]
    async fn resolve_decimals_reads_on_chain_for_unknown_mint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "context": {"slot": 1},
                    "value": {
                        "data": {
                            "parsed": {
                                "info": { "decimals": 9 },
                                "type": "mint"
                            },
                            "program": "spl-token",
                            "space": 82
                        },
                        "executable": false,
                        "lamports": 1000000,
                        "owner": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
                    }
                }
            })))
            .mount(&server)
            .await;
        let signer = SolanaSigner::new(Keypair::new(), server.uri());
        // A valid base58 pubkey that isn't a hardcoded USDC mint.
        let mint = Pubkey::from_str("So11111111111111111111111111111111111111112").unwrap();
        assert_eq!(signer.resolve_decimals(&mint).await.expect("on-chain"), 9);
    }

    #[tokio::test]
    async fn resolve_decimals_errors_when_not_a_mint() {
        // Account exists but has no parsed mint decimals (e.g. a
        // plain system account). Must surface a Sign error.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "context": {"slot": 1},
                    "value": {
                        "data": ["", "base64"],
                        "executable": false,
                        "lamports": 1000000,
                        "owner": "11111111111111111111111111111111"
                    }
                }
            })))
            .mount(&server)
            .await;
        let signer = SolanaSigner::new(Keypair::new(), server.uri());
        let mint = Pubkey::from_str("So11111111111111111111111111111111111111112").unwrap();
        let err = signer.resolve_decimals(&mint).await.expect_err("not a mint");
        assert!(matches!(err, X402Error::Sign(msg) if msg.contains("no parsed decimals")));
    }
}
