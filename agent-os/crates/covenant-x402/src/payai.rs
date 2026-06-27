//! Solana SPL signer for x402 providers behind PayAI's facilitator.
//!
//! Gated by the `solana` cargo feature. Produces the same canonical
//! `x-payment` header [`crate::solana::SolanaSigner`] does, but the
//! underlying transaction is built for PayAI's sponsored-gas flow:
//!
//! - A v0 [`VersionedTransaction`] (PayAI does not accept legacy
//!   transactions).
//! - `payerKey` set to the sponsor's pubkey lifted off
//!   `requirements.extra.feePayer` — i.e. the PayAI sponsor, not the
//!   funder.
//! - Instructions in the exact order PayAI's facilitator enforces:
//!   `ComputeBudget::set_compute_unit_limit(20_000)`,
//!   `set_compute_unit_price(1)`, then `TransferChecked`. The
//!   `TransferChecked` runs against TOKEN_PROGRAM_ID; TOKEN_2022 mints
//!   are refused — PayAI's v1 doesn't sponsor them today.
//! - Partial signing: only the funder slot is filled in. The fee-payer
//!   slot is left as the zero signature for PayAI to overwrite at
//!   `/settle` time.
//! - Both source and destination ATAs are assumed to exist on-chain;
//!   PayAI removed on-the-fly ATA creation from the hot path
//!   (`hotfix/remove-ata-creation` upstream), so we don't emit a
//!   create-ata instruction here either.
//!
//! Falling back when `requirements.extra.feePayer` is absent is the
//! caller's job (the sidecar dispatches by presence of that field).

use std::path::Path;
use std::str::FromStr;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_sdk::{
    hash::Hash,
    instruction::Instruction,
    message::{v0, VersionedMessage},
    pubkey::Pubkey,
    signature::Signature,
    signer::{
        keypair::{read_keypair_file, Keypair},
        Signer as SolanaKeypairSigner,
    },
    transaction::VersionedTransaction,
};
use spl_associated_token_account::get_associated_token_address;
use tracing::debug;

use crate::http::{read_capped, MAX_RESPONSE_BYTES};
use crate::solana::decimals_for_mint;
use crate::{PaymentRequirements, Result, Signer, X402Error};

/// PayAI's required compute-unit limit on the sponsored tx. Matches
/// what the `@payai/x402` client emits.
const COMPUTE_UNIT_LIMIT: u32 = 20_000;
/// PayAI's required compute-unit price in µ-lamports.
const COMPUTE_UNIT_PRICE_MICRO_LAMPORTS: u64 = 1;

/// Real PayAI-sponsored Solana payment signer.
pub struct PayaiSolanaSigner {
    keypair: Keypair,
    rpc_url: String,
    http: reqwest::Client,
    max_bytes: usize,
}

impl PayaiSolanaSigner {
    pub fn new(keypair: Keypair, rpc_url: impl Into<String>) -> Self {
        Self::with(keypair, rpc_url, reqwest::Client::new())
    }

    pub fn with(keypair: Keypair, rpc_url: impl Into<String>, http: reqwest::Client) -> Self {
        Self::with_limits(keypair, rpc_url, http, MAX_RESPONSE_BYTES)
    }

    fn with_limits(
        keypair: Keypair,
        rpc_url: impl Into<String>,
        http: reqwest::Client,
        max_bytes: usize,
    ) -> Self {
        Self {
            keypair,
            rpc_url: rpc_url.into(),
            http,
            max_bytes,
        }
    }

    /// Loads the funder keypair from a Solana CLI keypair file. The
    /// error message omits the bytes, surfacing only the path and the
    /// underlying reason.
    pub fn from_keypair_file(path: impl AsRef<Path>, rpc_url: impl Into<String>) -> Result<Self> {
        let path = path.as_ref();
        let keypair = read_keypair_file(path)
            .map_err(|e| X402Error::Sign(format!("read funder keypair {}: {e}", path.display())))?;
        Ok(Self::new(keypair, rpc_url))
    }

    pub fn pubkey(&self) -> Pubkey {
        self.keypair.pubkey()
    }

    async fn latest_blockhash(&self) -> Result<Hash> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getLatestBlockhash",
            "params": [{"commitment": "confirmed"}],
        });
        let resp = self.http.post(&self.rpc_url).json(&body).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = read_capped(resp, self.max_bytes, X402Error::Sign)
                .await
                .unwrap_or_default();
            return Err(X402Error::Sign(format!("rpc status {status}: {body}")));
        }
        let text = read_capped(resp, self.max_bytes, X402Error::Sign).await?;
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| X402Error::Sign(format!("decode rpc response: {e}")))?;
        let blockhash_str = parsed
            .pointer("/result/value/blockhash")
            .and_then(|v| v.as_str())
            .ok_or_else(|| X402Error::Sign(format!("rpc: no blockhash in response: {parsed}")))?;
        Hash::from_str(blockhash_str).map_err(|e| X402Error::Sign(format!("parse blockhash: {e}")))
    }

    /// `true` if the account exists on chain. PayAI removed
    /// on-the-fly ATA creation, so the funder's source ATA must already
    /// exist or `/settle` rejects after the envelope is consumed.
    async fn account_exists(&self, account: &Pubkey) -> Result<bool> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getAccountInfo",
            "params": [
                account.to_string(),
                {"commitment": "confirmed", "encoding": "base64"}
            ],
        });
        let resp = self.http.post(&self.rpc_url).json(&body).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = read_capped(resp, self.max_bytes, X402Error::Sign)
                .await
                .unwrap_or_default();
            return Err(X402Error::Sign(format!(
                "rpc getAccountInfo status {status}: {body}"
            )));
        }
        let text = read_capped(resp, self.max_bytes, X402Error::Sign).await?;
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| X402Error::Sign(format!("decode rpc response: {e}")))?;
        Ok(parsed
            .pointer("/result/value")
            .map(|v| !v.is_null())
            .unwrap_or(false))
    }
}

#[async_trait::async_trait]
impl Signer for PayaiSolanaSigner {
    async fn build_payment(&self, requirements: &PaymentRequirements) -> Result<String> {
        let fee_payer_str = requirements
            .extra
            .as_ref()
            .and_then(|e| e.fee_payer.as_deref())
            .ok_or_else(|| {
                X402Error::Sign("PayaiSolanaSigner requires requirements.extra.feePayer".into())
            })?;
        let fee_payer = Pubkey::from_str(fee_payer_str)
            .map_err(|e| X402Error::Sign(format!("parse feePayer {fee_payer_str:?}: {e}")))?;
        let mint = Pubkey::from_str(&requirements.asset)
            .map_err(|e| X402Error::Sign(format!("parse asset {:?}: {e}", requirements.asset)))?;
        let recipient = Pubkey::from_str(&requirements.pay_to)
            .map_err(|e| X402Error::Sign(format!("parse pay_to {:?}: {e}", requirements.pay_to)))?;
        let amount: u64 = requirements
            .amount
            .parse()
            .map_err(|e| X402Error::Sign(format!("parse amount {:?}: {e}", requirements.amount)))?;
        let decimals = decimals_for_mint(&mint).ok_or_else(|| {
            X402Error::Sign(format!(
                "mint {mint} not in PayaiSolanaSigner's known set (currently USDC mainnet+devnet)"
            ))
        })?;

        let funder = self.keypair.pubkey();
        let source_ata = get_associated_token_address(&funder, &mint);
        if !self.account_exists(&source_ata).await? {
            return Err(X402Error::Sign(format!(
                "funder ATA {source_ata} for mint {mint} does not exist on chain; \
                 fund the wallet with at least one transfer of this mint first"
            )));
        }

        let blockhash = self.latest_blockhash().await?;
        debug!(
            %funder, %fee_payer, %mint, %recipient, amount,
            "PayaiSolanaSigner building v0 transfer"
        );

        let tx = build_payai_transaction(
            &self.keypair,
            fee_payer,
            mint,
            recipient,
            amount,
            decimals,
            blockhash,
        )?;
        let serialized =
            bincode::serialize(&tx).map_err(|e| X402Error::Sign(format!("serialize tx: {e}")))?;
        let tx_b64 = BASE64.encode(serialized);

        let envelope = serde_json::json!({
            "x402Version": 1,
            "scheme":      requirements.scheme,
            "network":     short_network(&requirements.network),
            "payload":     { "transaction": tx_b64 },
        });
        Ok(BASE64.encode(envelope.to_string().as_bytes()))
    }
}

/// PayAI v1's Solana facilitator expects the short network identifier
/// (`"solana"`), not the CAIP-2 form, in the envelope. They claim to
/// normalize inbound, but emitting the short form keeps us off the
/// normalization path entirely.
fn short_network(network: &str) -> &str {
    if network.starts_with("solana:") {
        "solana"
    } else {
        network
    }
}

/// Builds the partially-signed v0 VersionedTransaction PayAI expects.
///
/// The funder signs only their own slot; the fee-payer slot is left as
/// `Signature::default()` (32 zero bytes) for PayAI's facilitator to
/// fill at settle time. Instruction order is fixed: ComputeBudget
/// limit, ComputeBudget price, then TransferChecked.
pub fn build_payai_transaction(
    funder: &Keypair,
    fee_payer: Pubkey,
    mint: Pubkey,
    recipient: Pubkey,
    amount: u64,
    decimals: u8,
    recent_blockhash: Hash,
) -> Result<VersionedTransaction> {
    let funder_pubkey = funder.pubkey();
    let source_ata = get_associated_token_address(&funder_pubkey, &mint);
    let dest_ata = get_associated_token_address(&recipient, &mint);

    let transfer = spl_token::instruction::transfer_checked(
        &spl_token::ID,
        &source_ata,
        &mint,
        &dest_ata,
        &funder_pubkey,
        &[&funder_pubkey],
        amount,
        decimals,
    )
    .map_err(|e| X402Error::Sign(format!("build transfer_checked: {e}")))?;

    // PayAI's facilitator enforces this exact instruction order; do not
    // reorder without re-validating against /verify.
    let instructions: Vec<Instruction> = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(COMPUTE_UNIT_LIMIT),
        ComputeBudgetInstruction::set_compute_unit_price(COMPUTE_UNIT_PRICE_MICRO_LAMPORTS),
        transfer,
    ];

    let message = v0::Message::try_compile(&fee_payer, &instructions, &[], recent_blockhash)
        .map_err(|e| X402Error::Sign(format!("compile v0 message: {e}")))?;
    let versioned_message = VersionedMessage::V0(message);

    let num_required_sigs = versioned_message.header().num_required_signatures as usize;
    let mut signatures = vec![Signature::default(); num_required_sigs];

    let funder_slot = versioned_message
        .static_account_keys()
        .iter()
        .position(|k| k == &funder_pubkey)
        .ok_or_else(|| X402Error::Sign("funder pubkey not present in compiled message".into()))?;
    if funder_slot >= num_required_sigs {
        return Err(X402Error::Sign(format!(
            "funder pubkey at non-signer slot {funder_slot} (signers: {num_required_sigs})"
        )));
    }

    let msg_bytes = versioned_message.serialize();
    signatures[funder_slot] = funder.sign_message(&msg_bytes);

    Ok(VersionedTransaction {
        message: versioned_message,
        signatures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PaymentExtra;
    use solana_sdk::signer::keypair::Keypair;
    use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

    const USDC_MAINNET: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    const PAYAI_FEE_PAYER: &str = "2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4";
    const RECIPIENT: &str = "7G73PLhKvAPBGTzG5ESAE4coE7QrVeTTKfhTxQZbyGgC";

    fn known_blockhash() -> Hash {
        Hash::new_from_array([7u8; 32])
    }

    #[test]
    fn instruction_order_is_cb_limit_cb_price_transfer() {
        let funder = Keypair::new();
        let tx = build_payai_transaction(
            &funder,
            Pubkey::from_str(PAYAI_FEE_PAYER).unwrap(),
            Pubkey::from_str(USDC_MAINNET).unwrap(),
            Pubkey::from_str(RECIPIENT).unwrap(),
            80_000,
            6,
            known_blockhash(),
        )
        .expect("build");

        let VersionedMessage::V0(msg) = &tx.message else {
            panic!("not v0");
        };
        assert_eq!(msg.instructions.len(), 3, "exactly three instructions");

        let keys = tx.message.static_account_keys();
        let cb_id = solana_compute_budget_interface::id();
        let token_id = spl_token::ID;
        assert_eq!(
            keys[msg.instructions[0].program_id_index as usize], cb_id,
            "instr 0 is ComputeBudget"
        );
        assert_eq!(
            keys[msg.instructions[1].program_id_index as usize], cb_id,
            "instr 1 is ComputeBudget"
        );
        assert_eq!(
            keys[msg.instructions[2].program_id_index as usize], token_id,
            "instr 2 is SPL token"
        );

        // First byte of the CB instruction data is the discriminator:
        // 2 = SetComputeUnitLimit, 3 = SetComputeUnitPrice.
        assert_eq!(msg.instructions[0].data[0], 2, "first CB = set unit limit");
        assert_eq!(msg.instructions[1].data[0], 3, "second CB = set unit price");
    }

    #[test]
    fn payer_slot_is_fee_payer_not_funder() {
        let funder = Keypair::new();
        let fee_payer = Pubkey::from_str(PAYAI_FEE_PAYER).unwrap();
        let tx = build_payai_transaction(
            &funder,
            fee_payer,
            Pubkey::from_str(USDC_MAINNET).unwrap(),
            Pubkey::from_str(RECIPIENT).unwrap(),
            1,
            6,
            known_blockhash(),
        )
        .expect("build");
        let keys = tx.message.static_account_keys();
        assert_eq!(keys[0], fee_payer, "v0 message payer is at index 0");
        assert_ne!(keys[0], funder.pubkey());
    }

    #[test]
    fn only_funder_slot_is_signed_fee_payer_left_empty() {
        let funder = Keypair::new();
        let tx = build_payai_transaction(
            &funder,
            Pubkey::from_str(PAYAI_FEE_PAYER).unwrap(),
            Pubkey::from_str(USDC_MAINNET).unwrap(),
            Pubkey::from_str(RECIPIENT).unwrap(),
            1,
            6,
            known_blockhash(),
        )
        .expect("build");

        let header = tx.message.header();
        assert_eq!(
            header.num_required_signatures, 2,
            "expected fee_payer + funder as required signers"
        );
        assert_eq!(tx.signatures.len(), 2);
        assert_eq!(
            tx.signatures[0],
            Signature::default(),
            "fee_payer slot must be left empty for PayAI to fill"
        );
        assert_ne!(
            tx.signatures[1],
            Signature::default(),
            "funder slot must be signed"
        );

        let keys = tx.message.static_account_keys();
        assert_eq!(keys[1], funder.pubkey(), "funder is at signer slot 1");
        let msg_bytes = tx.message.serialize();
        assert!(
            tx.signatures[1].verify(funder.pubkey().as_ref(), &msg_bytes),
            "funder signature must verify against the message bytes"
        );
    }

    #[test]
    fn short_network_strips_caip2_solana_prefix_else_passthrough() {
        // The envelope's "network" field is emitted straight from short_network,
        // and PayAI's facilitator expects the short "solana" id, not the CAIP-2
        // form. Pin the exact mapping a mutation could otherwise break silently.
        assert_eq!(
            short_network("solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp"),
            "solana",
            "CAIP-2 mainnet collapses to the short form"
        );
        assert_eq!(
            short_network("solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1"),
            "solana",
            "the match is namespace-prefix based, not an exact CAIP-2 string"
        );
        assert_eq!(
            short_network("solana"),
            "solana",
            "the already-short form passes through the else branch unchanged"
        );
        assert_eq!(
            short_network("ethereum"),
            "ethereum",
            "a non-Solana network must not be relabelled (kills always-solana and swapped branches)"
        );
        assert_eq!(
            short_network("solana-localnet"),
            "solana-localnet",
            "a 'solana'-prefixed label without the CAIP-2 colon is left intact (the ':' is load-bearing)"
        );
    }

    #[tokio::test]
    async fn build_payment_errors_without_fee_payer_in_extra() {
        let signer = PayaiSolanaSigner::new(Keypair::new(), "https://unused");
        let req = PaymentRequirements {
            network: "solana".into(),
            asset: USDC_MAINNET.into(),
            amount: "1".into(),
            amount_usdc: 0.000001,
            pay_to: RECIPIENT.into(),
            scheme: "exact".into(),
            extra: None,
        };
        let err = signer
            .build_payment(&req)
            .await
            .expect_err("must require feePayer");
        assert!(format!("{err}").contains("feePayer"), "got: {err}");
    }

    #[tokio::test]
    async fn build_payment_errors_on_unknown_mint() {
        let signer = PayaiSolanaSigner::new(Keypair::new(), "https://unused");
        let req = PaymentRequirements {
            network: "solana".into(),
            asset: "So11111111111111111111111111111111111111112".into(), // wSOL
            amount: "1".into(),
            amount_usdc: 0.0,
            pay_to: RECIPIENT.into(),
            scheme: "exact".into(),
            extra: Some(PaymentExtra {
                fee_payer: Some(PAYAI_FEE_PAYER.into()),
            }),
        };
        let err = signer.build_payment(&req).await.expect_err("unknown mint");
        let msg = format!("{err}");
        assert!(
            msg.contains("not in PayaiSolanaSigner's known set"),
            "got: {msg}"
        );
    }

    #[tokio::test]
    async fn build_payment_rejects_malformed_fields_and_fee_payer() {
        // feePayer, asset, pay_to, and amount are all parsed before any RPC.
        // feePayer names the PayAI gas sponsor; the rest come from the 402
        // challenge. Each malformed value must fail closed as X402Error::Sign.
        let signer = PayaiSolanaSigner::new(Keypair::new(), "https://unused");
        let valid = || PaymentRequirements {
            network: "solana".into(),
            asset: USDC_MAINNET.into(),
            amount: "1".into(),
            amount_usdc: 0.000001,
            pay_to: RECIPIENT.into(),
            scheme: "exact".into(),
            extra: Some(PaymentExtra {
                fee_payer: Some(PAYAI_FEE_PAYER.into()),
            }),
        };

        let mut req = valid();
        req.extra = Some(PaymentExtra {
            fee_payer: Some("not-a-pubkey".into()),
        });
        assert!(matches!(
            signer.build_payment(&req).await.expect_err("bad feePayer"),
            X402Error::Sign(m) if m.contains("parse feePayer")
        ));

        let mut req = valid();
        req.asset = "not-a-pubkey".into();
        assert!(matches!(
            signer.build_payment(&req).await.expect_err("bad asset"),
            X402Error::Sign(m) if m.contains("parse asset")
        ));

        let mut req = valid();
        req.pay_to = "not-a-pubkey".into();
        assert!(matches!(
            signer.build_payment(&req).await.expect_err("bad pay_to"),
            X402Error::Sign(m) if m.contains("parse pay_to")
        ));

        let mut req = valid();
        req.amount = "not-a-number".into();
        assert!(matches!(
            signer.build_payment(&req).await.expect_err("bad amount"),
            X402Error::Sign(m) if m.contains("parse amount")
        ));
    }

    #[tokio::test]
    async fn latest_blockhash_rejects_faulted_responses() {
        // latest_blockhash backs every PayAI sponsored transfer. Each RPC fault
        // mode must fail closed as a Sign error rather than yield a missing or
        // garbage blockhash that would build an unlandable transaction.
        let down = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&down)
            .await;
        let err = PayaiSolanaSigner::new(Keypair::new(), down.uri())
            .latest_blockhash()
            .await
            .expect_err("rpc error status");
        assert!(matches!(err, X402Error::Sign(msg) if msg.contains("rpc status")));

        let empty = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "result": {"context": {"slot": 1}, "value": {}}
            })))
            .mount(&empty)
            .await;
        let err = PayaiSolanaSigner::new(Keypair::new(), empty.uri())
            .latest_blockhash()
            .await
            .expect_err("missing blockhash");
        assert!(matches!(err, X402Error::Sign(msg) if msg.contains("no blockhash")));

        let unparseable = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1,
                "result": {"context": {"slot": 1}, "value": {"blockhash": "not-base58!"}}
            })))
            .mount(&unparseable)
            .await;
        let err = PayaiSolanaSigner::new(Keypair::new(), unparseable.uri())
            .latest_blockhash()
            .await
            .expect_err("unparseable blockhash");
        assert!(matches!(err, X402Error::Sign(msg) if msg.contains("parse blockhash")));
    }

    #[tokio::test]
    async fn account_exists_rejects_rpc_error_status() {
        // account_exists gates the funder-ATA guard. A non-2xx getAccountInfo
        // response must surface a Sign error, never be read as an existence
        // result: a transient 5xx is not "the account exists" or "it doesn't",
        // and treating it as either would skip the guard or reject a funded
        // wallet on a momentary RPC blip.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503).set_body_string("upstream unavailable"))
            .mount(&server)
            .await;
        let ata = Pubkey::from_str(RECIPIENT).unwrap();
        let err = PayaiSolanaSigner::new(Keypair::new(), server.uri())
            .account_exists(&ata)
            .await
            .expect_err("rpc error status");
        assert!(matches!(err, X402Error::Sign(msg) if msg.contains("getAccountInfo status")));
    }

    #[tokio::test]
    async fn build_payment_rejects_when_funder_ata_missing_on_chain() {
        // The funder's source ATA does not exist on chain: getAccountInfo
        // returns 200 with result.value=null, so account_exists resolves to
        // Ok(false). build_payment must fail closed as a Sign error before
        // building or signing a transfer PayAI could never settle, rather than
        // treat a missing ATA as spendable. Distinct from
        // account_exists_rejects_rpc_error_status, which covers the RPC-fault
        // (Err) propagation, not this Ok(false) guard branch — and from the
        // other build_payment tests, which all fail before any RPC.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1,
                "result": {"context": {"slot": 1}, "value": null}
            })))
            .mount(&server)
            .await;
        let req = PaymentRequirements {
            network: "solana".into(),
            asset: USDC_MAINNET.into(),
            amount: "1".into(),
            amount_usdc: 0.000001,
            pay_to: RECIPIENT.into(),
            scheme: "exact".into(),
            extra: Some(PaymentExtra {
                fee_payer: Some(PAYAI_FEE_PAYER.into()),
            }),
        };
        // account_exists is the first RPC; its Ok(false) returns before
        // latest_blockhash is reached, so a single getAccountInfo mock suffices.
        let err = PayaiSolanaSigner::new(Keypair::new(), server.uri())
            .build_payment(&req)
            .await
            .expect_err("missing funder ATA");
        assert!(
            matches!(&err, X402Error::Sign(msg) if msg.contains("does not exist on chain")),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn latest_blockhash_rejects_oversized_rpc_body() {
        // The Solana RPC is untrusted: a compromised or malicious node returning
        // a body past the cap must fail closed as a Sign error rather than buffer
        // the whole response into the keypair-custody signer and OOM it.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("a".repeat(4096)))
            .mount(&server)
            .await;
        let signer = PayaiSolanaSigner::with_limits(
            Keypair::new(),
            server.uri(),
            reqwest::Client::new(),
            64,
        );
        let err = signer
            .latest_blockhash()
            .await
            .expect_err("oversized rpc body");
        assert!(
            matches!(err, X402Error::Sign(ref msg) if msg.contains("cap")),
            "got: {err:?}"
        );
    }
}
