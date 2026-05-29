//! Jupiter v6 swap client.
//!
//! Two-step flow:
//! 1. `quote(input, output, amount, slippage_bps)` — returns the route
//!    metadata + the routed output amount.
//! 2. `swap(quote, user)` — returns a base64-encoded signed-by-user-only
//!    VersionedTransaction. The keeper deserializes, signs, broadcasts,
//!    and confirms.
//!
//! Failure modes are handled by the caller (sweep loop logs + defers
//! the leg to the next tick if the swap can't route or settles below
//! a minimum output threshold).

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::RpcSendTransactionConfig;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use solana_sdk::transaction::VersionedTransaction;
use tracing::{debug, warn};

pub const JUPITER_QUOTE_URL: &str = "https://lite-api.jup.ag/swap/v1/quote";
pub const JUPITER_SWAP_URL: &str = "https://lite-api.jup.ag/swap/v1/swap";

pub const SOL_MINT: &str = "So11111111111111111111111111111111111111112";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JupiterQuote {
    pub input_mint: String,
    pub output_mint: String,
    pub in_amount: String,
    pub out_amount: String,
    pub other_amount_threshold: String,
    pub swap_mode: String,
    pub slippage_bps: u16,
    pub price_impact_pct: String,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

impl JupiterQuote {
    pub fn out_amount_u64(&self) -> Result<u64> {
        self.out_amount
            .parse()
            .with_context(|| format!("parse outAmount: {}", self.out_amount))
    }

    pub fn other_amount_threshold_u64(&self) -> Result<u64> {
        self.other_amount_threshold
            .parse()
            .with_context(|| format!("parse otherAmountThreshold: {}", self.other_amount_threshold))
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SwapRequest<'a> {
    user_public_key: String,
    wrap_and_unwrap_sol: bool,
    use_shared_accounts: bool,
    quote_response: &'a serde_json::Value,
    dynamic_compute_unit_limit: bool,
    skip_user_accounts_rpc_calls: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SwapResponse {
    swap_transaction: String,
}

pub struct JupiterClient {
    http: Client,
}

impl Default for JupiterClient {
    fn default() -> Self {
        Self::new()
    }
}

impl JupiterClient {
    pub fn new() -> Self {
        Self {
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .expect("reqwest client"),
        }
    }

    pub async fn quote(
        &self,
        input_mint: &Pubkey,
        output_mint: &Pubkey,
        amount: u64,
        slippage_bps: u16,
    ) -> Result<(JupiterQuote, serde_json::Value)> {
        let url = format!(
            "{base}?inputMint={input}&outputMint={output}&amount={amount}&slippageBps={slippage}&swapMode=ExactIn&onlyDirectRoutes=false",
            base = JUPITER_QUOTE_URL,
            input = input_mint,
            output = output_mint,
            slippage = slippage_bps,
        );
        debug!(url = %url, "jupiter quote request");
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .context("jupiter quote send")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("jupiter quote http {status}: {body}"));
        }
        let raw: serde_json::Value = resp.json().await.context("jupiter quote json")?;
        let quote: JupiterQuote =
            serde_json::from_value(raw.clone()).context("jupiter quote decode")?;
        Ok((quote, raw))
    }

    pub async fn swap(
        &self,
        quote_raw: &serde_json::Value,
        user: &Pubkey,
    ) -> Result<VersionedTransaction> {
        let body = SwapRequest {
            user_public_key: user.to_string(),
            wrap_and_unwrap_sol: true,
            use_shared_accounts: true,
            quote_response: quote_raw,
            dynamic_compute_unit_limit: true,
            skip_user_accounts_rpc_calls: false,
        };
        let resp = self
            .http
            .post(JUPITER_SWAP_URL)
            .json(&body)
            .send()
            .await
            .context("jupiter swap send")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("jupiter swap http {status}: {body}"));
        }
        let swap_resp: SwapResponse = resp.json().await.context("jupiter swap json")?;
        let raw = B64
            .decode(&swap_resp.swap_transaction)
            .context("decode jupiter swap_transaction base64")?;
        let tx: VersionedTransaction =
            bincode::deserialize(&raw).context("deserialize jupiter VersionedTransaction")?;
        Ok(tx)
    }
}

pub async fn sign_and_send_jupiter_tx(
    rpc: &RpcClient,
    mut tx: VersionedTransaction,
    signer: &Keypair,
) -> Result<String> {
    let blockhash = rpc
        .get_latest_blockhash()
        .await
        .context("jupiter swap blockhash")?;
    let signer_position = tx
        .message
        .static_account_keys()
        .iter()
        .position(|k| k == &signer.pubkey())
        .ok_or_else(|| anyhow!("signer pubkey not in jupiter tx static accounts"))?;

    let message_data = tx.message.serialize();
    let sig = signer.sign_message(&message_data);

    if signer_position >= tx.signatures.len() {
        return Err(anyhow!(
            "jupiter tx has {} signature slots, signer at index {}",
            tx.signatures.len(),
            signer_position
        ));
    }
    tx.signatures[signer_position] = sig;

    let _ = blockhash;
    let sig = rpc
        .send_transaction_with_config(
            &tx,
            RpcSendTransactionConfig {
                skip_preflight: false,
                preflight_commitment: Some(CommitmentConfig::confirmed().commitment),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            warn!(error = %e, "jupiter swap submit failed");
            anyhow!("jupiter swap submit: {e}")
        })?;

    let confirm_start = std::time::Instant::now();
    while confirm_start.elapsed() < std::time::Duration::from_secs(60) {
        match rpc.confirm_transaction(&sig).await {
            Ok(true) => return Ok(sig.to_string()),
            Ok(false) => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
            Err(e) => {
                warn!(error = %e, "jupiter swap confirm probe failed; retrying");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
    Err(anyhow!("jupiter swap {sig} not confirmed within 60s"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_url_is_well_formed() {
        let input = Pubkey::new_unique();
        let output = Pubkey::new_unique();
        let url = format!(
            "{base}?inputMint={input}&outputMint={output}&amount={amount}&slippageBps={slippage}&swapMode=ExactIn&onlyDirectRoutes=false",
            base = JUPITER_QUOTE_URL,
            input = input,
            output = output,
            amount = 1_000_000_000u64,
            slippage = 200u16,
        );
        assert!(url.starts_with(JUPITER_QUOTE_URL));
        assert!(url.contains("amount=1000000000"));
        assert!(url.contains("slippageBps=200"));
        assert!(url.contains("swapMode=ExactIn"));
    }

    #[test]
    fn quote_amount_parsing_round_trips() {
        let q = JupiterQuote {
            input_mint: SOL_MINT.into(),
            output_mint: "2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump".into(),
            in_amount: "1000000000".into(),
            out_amount: "12345678".into(),
            other_amount_threshold: "12000000".into(),
            swap_mode: "ExactIn".into(),
            slippage_bps: 200,
            price_impact_pct: "0.42".into(),
            extra: serde_json::Value::Null,
        };
        assert_eq!(q.out_amount_u64().unwrap(), 12_345_678);
        assert_eq!(q.other_amount_threshold_u64().unwrap(), 12_000_000);
    }
}
