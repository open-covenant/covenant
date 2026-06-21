use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::types::Settlement;
use crate::{is_usdc, PayaiError, Result, PAYAI_SOLANA_FEE_PAYER, TOKEN_PROGRAM_ID};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureInfo {
    pub signature: String,
    pub slot: u64,
    pub block_time: Option<i64>,
}

/// Reads PayAI's public Solana settlement stream over JSON-RPC. Read-only:
/// watches the facilitator fee-payer (which co-signs every settlement) and
/// parses the USDC transfers. Never signs or sends a transaction (no
/// `solana-sdk`, just `reqwest`).
pub struct SettlementIndexer {
    rpc_url: String,
    fee_payer: String,
    http: reqwest::Client,
}

impl SettlementIndexer {
    pub fn new(rpc_url: impl Into<String>) -> Self {
        // fail fast: some public RPCs stall on getTransaction, can't hang the daemon.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .connect_timeout(std::time::Duration::from_secs(8))
            .build()
            .unwrap_or_default();
        Self {
            rpc_url: rpc_url.into(),
            fee_payer: PAYAI_SOLANA_FEE_PAYER.to_string(),
            http,
        }
    }

    pub fn with_fee_payer(mut self, fee_payer: impl Into<String>) -> Self {
        self.fee_payer = fee_payer.into();
        self
    }

    pub fn with_http(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    pub fn fee_payer(&self) -> &str {
        &self.fee_payer
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let resp = self.http.post(&self.rpc_url).json(&body).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(PayaiError::Rpc(format!(
                "{method} status {status}: {}",
                resp.text().await.unwrap_or_default()
            )));
        }
        let v: Value = resp.json().await?;
        if let Some(e) = v.get("error") {
            if !e.is_null() {
                return Err(PayaiError::Rpc(format!("{method}: {e}")));
            }
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Most recent signatures touching the fee-payer. Failed transactions are
    /// dropped (`err != null`).
    pub async fn fetch_signatures(
        &self,
        limit: usize,
        before: Option<&str>,
    ) -> Result<Vec<SignatureInfo>> {
        let mut opts = json!({ "limit": limit });
        if let Some(b) = before {
            opts["before"] = json!(b);
        }
        let result = self
            .rpc("getSignaturesForAddress", json!([self.fee_payer, opts]))
            .await?;
        let arr = result.as_array().ok_or_else(|| {
            PayaiError::Parse("getSignaturesForAddress: result not an array".into())
        })?;
        let mut out = Vec::new();
        for e in arr {
            if e.get("err").map(|x| !x.is_null()).unwrap_or(false) {
                continue;
            }
            let signature = match e.get("signature").and_then(Value::as_str) {
                Some(s) => s.to_string(),
                None => continue,
            };
            out.push(SignatureInfo {
                signature,
                slot: e.get("slot").and_then(Value::as_u64).unwrap_or(0),
                block_time: e.get("blockTime").and_then(Value::as_i64),
            });
        }
        Ok(out)
    }

    pub async fn fetch_transaction(&self, signature: &str) -> Result<Value> {
        self.rpc(
            "getTransaction",
            json!([
                signature,
                {"encoding": "jsonParsed", "maxSupportedTransactionVersion": 0, "commitment": "confirmed"}
            ]),
        )
        .await
    }

    /// Fetch the latest `limit` signatures and parse every USDC settlement from
    /// them. One RPC per transaction (sequential; fine for the poll cadence).
    pub async fn recent_settlements(&self, limit: usize) -> Result<Vec<Settlement>> {
        let sigs = self.fetch_signatures(limit, None).await?;
        let mut out = Vec::new();
        for s in &sigs {
            let tx = self.fetch_transaction(&s.signature).await?;
            if tx.is_null() {
                continue;
            }
            out.extend(parse_settlements(&tx, &s.signature));
        }
        Ok(out)
    }
}

/// Parse all USDC settlements from a `jsonParsed` transaction; empty for a
/// failed or non-USDC transaction. Attributes transfers to OWNER wallets
/// (resolved from token balances), not token accounts, so reputation keys on
/// agents rather than ATAs.
pub fn parse_settlements(tx: &Value, signature: &str) -> Vec<Settlement> {
    if tx
        .pointer("/meta/err")
        .map(|e| !e.is_null())
        .unwrap_or(false)
    {
        return Vec::new();
    }
    let slot = tx.get("slot").and_then(Value::as_u64).unwrap_or(0);
    let block_time = tx.get("blockTime").and_then(Value::as_i64);

    let keys = account_keys(tx);
    let owner_by_ata = balance_map(tx, &keys, "owner");
    let mint_by_ata = balance_map(tx, &keys, "mint");

    let mut out = Vec::new();
    for ins in spl_token_instructions(tx) {
        let parsed = match ins.get("parsed") {
            Some(p) => p,
            None => continue,
        };
        let typ = parsed.get("type").and_then(Value::as_str).unwrap_or("");
        if typ != "transferChecked" && typ != "transfer" {
            continue;
        }
        let info = match parsed.get("info") {
            Some(i) => i,
            None => continue,
        };
        let source = info.get("source").and_then(Value::as_str).unwrap_or("");
        let dest = info
            .get("destination")
            .and_then(Value::as_str)
            .unwrap_or("");
        if source.is_empty() || dest.is_empty() {
            continue;
        }

        // transferChecked carries the mint inline; plain transfer doesn't, so
        // resolve it from the token-balance map for either account.
        let mint = info
            .get("mint")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| mint_by_ata.get(dest).cloned())
            .or_else(|| mint_by_ata.get(source).cloned());
        let mint = match mint {
            Some(m) => m,
            None => continue,
        };
        if !is_usdc(&mint) {
            continue;
        }

        let amount_micro: u128 = if typ == "transferChecked" {
            info.pointer("/tokenAmount/amount")
                .and_then(Value::as_str)
                .and_then(|s| s.parse().ok())
        } else {
            info.get("amount")
                .and_then(Value::as_str)
                .and_then(|s| s.parse().ok())
        }
        .unwrap_or(0);
        if amount_micro == 0 {
            continue;
        }

        // Buyer = source-ATA authority (present inline); fall back to the
        // source ATA owner, then the ATA itself. Seller = destination owner.
        let payer = info
            .get("authority")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| owner_by_ata.get(source).cloned())
            .unwrap_or_else(|| source.to_string());
        let pay_to = owner_by_ata
            .get(dest)
            .cloned()
            .unwrap_or_else(|| dest.to_string());

        out.push(Settlement {
            signature: signature.to_string(),
            slot,
            block_time,
            mint,
            payer,
            pay_to,
            amount_micro,
        });
    }
    out
}

fn account_keys(tx: &Value) -> Vec<String> {
    let mut keys = Vec::new();
    if let Some(arr) = tx
        .pointer("/transaction/message/accountKeys")
        .and_then(Value::as_array)
    {
        for k in arr {
            if let Some(s) = k.as_str() {
                keys.push(s.to_string());
            } else if let Some(s) = k.get("pubkey").and_then(Value::as_str) {
                keys.push(s.to_string());
            }
        }
    }
    keys
}

/// Build `ata_pubkey -> field` from post+pre token balances (post wins).
fn balance_map(tx: &Value, keys: &[String], field: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let post = tx
        .pointer("/meta/postTokenBalances")
        .and_then(Value::as_array);
    let pre = tx
        .pointer("/meta/preTokenBalances")
        .and_then(Value::as_array);
    for b in post.into_iter().flatten().chain(pre.into_iter().flatten()) {
        let idx = match b.get("accountIndex").and_then(Value::as_u64) {
            Some(i) => i as usize,
            None => continue,
        };
        let ata = match keys.get(idx) {
            Some(k) => k.clone(),
            None => continue,
        };
        if let Some(val) = b.get(field).and_then(Value::as_str) {
            m.entry(ata).or_insert_with(|| val.to_string());
        }
    }
    m
}

fn spl_token_instructions(tx: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let is_spl = |ins: &Value| {
        ins.get("program").and_then(Value::as_str) == Some("spl-token")
            || ins.get("programId").and_then(Value::as_str) == Some(TOKEN_PROGRAM_ID)
    };
    if let Some(arr) = tx
        .pointer("/transaction/message/instructions")
        .and_then(Value::as_array)
    {
        for ins in arr {
            if is_spl(ins) {
                out.push(ins.clone());
            }
        }
    }
    if let Some(groups) = tx
        .pointer("/meta/innerInstructions")
        .and_then(Value::as_array)
    {
        for g in groups {
            if let Some(arr) = g.get("instructions").and_then(Value::as_array) {
                for ins in arr {
                    if is_spl(ins) {
                        out.push(ins.clone());
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::USDC_MAINNET_MINT;
    use wiremock::matchers::{body_string_contains, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // jsonParsed transferChecked of USDC: buyer ATA (idx 1) -> seller ATA
    // (idx 3), 0.5 USDC. postTokenBalances resolve the ATAs to owner wallets.
    fn usdc_tx() -> Value {
        json!({
            "slot": 321,
            "blockTime": 1_781_980_000i64,
            "meta": {
                "err": null,
                "postTokenBalances": [
                    {"accountIndex": 1, "mint": USDC_MAINNET_MINT, "owner": "BuyerWallet"},
                    {"accountIndex": 3, "mint": USDC_MAINNET_MINT, "owner": "SellerWallet"}
                ]
            },
            "transaction": {
                "message": {
                    "accountKeys": [
                        {"pubkey": "FeePayer"},
                        {"pubkey": "BuyerAta"},
                        {"pubkey": USDC_MAINNET_MINT},
                        {"pubkey": "SellerAta"},
                        {"pubkey": "BuyerWallet"}
                    ],
                    "instructions": [
                        {
                            "program": "spl-token",
                            "programId": crate::TOKEN_PROGRAM_ID,
                            "parsed": {
                                "type": "transferChecked",
                                "info": {
                                    "source": "BuyerAta",
                                    "destination": "SellerAta",
                                    "mint": USDC_MAINNET_MINT,
                                    "authority": "BuyerWallet",
                                    "tokenAmount": {"amount": "500000", "decimals": 6}
                                }
                            }
                        }
                    ]
                }
            }
        })
    }

    #[test]
    fn parses_usdc_transfer_to_owner_wallets() {
        let s = parse_settlements(&usdc_tx(), "Sig1");
        assert_eq!(s.len(), 1);
        let s = &s[0];
        assert_eq!(s.signature, "Sig1");
        assert_eq!(s.payer, "BuyerWallet");
        assert_eq!(
            s.pay_to, "SellerWallet",
            "resolved from destination ATA owner"
        );
        assert_eq!(s.amount_micro, 500_000);
        assert_eq!(s.mint, USDC_MAINNET_MINT);
        assert_eq!(s.slot, 321);
        assert_eq!(s.block_time, Some(1_781_980_000));
    }

    #[test]
    fn failed_tx_yields_nothing() {
        let mut tx = usdc_tx();
        tx["meta"]["err"] = json!({"InstructionError": [0, "Custom"]});
        assert!(parse_settlements(&tx, "Sig").is_empty());
    }

    #[test]
    fn non_usdc_transfer_ignored() {
        let mut tx = usdc_tx();
        let other_mint = "So11111111111111111111111111111111111111112";
        tx["transaction"]["message"]["instructions"][0]["parsed"]["info"]["mint"] =
            json!(other_mint);
        assert!(parse_settlements(&tx, "Sig").is_empty());
    }

    #[test]
    fn falls_back_to_ata_when_owner_unresolved() {
        let mut tx = usdc_tx();
        tx["meta"]["postTokenBalances"] = json!([]); // no owner resolution
        let s = parse_settlements(&tx, "Sig");
        assert_eq!(s.len(), 1);
        assert_eq!(
            s[0].pay_to, "SellerAta",
            "falls back to the destination ATA"
        );
        assert_eq!(s[0].payer, "BuyerWallet", "authority still inline");
    }

    #[tokio::test]
    async fn indexer_fetches_signatures_then_settlements() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("getSignaturesForAddress"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0", "id": 1,
                "result": [
                    {"signature": "Sig1", "slot": 321, "blockTime": 1_781_980_000i64, "err": null},
                    {"signature": "SigFailed", "slot": 322, "blockTime": 1i64, "err": {"x": 1}}
                ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("getTransaction"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0", "id": 1, "result": usdc_tx()
            })))
            .mount(&server)
            .await;

        let ix = SettlementIndexer::new(server.uri());
        let sigs = ix.fetch_signatures(10, None).await.unwrap();
        assert_eq!(sigs.len(), 1, "the failed signature is dropped");

        let settlements = ix.recent_settlements(10).await.unwrap();
        assert_eq!(settlements.len(), 1);
        assert_eq!(settlements[0].pay_to, "SellerWallet");
        assert_eq!(settlements[0].amount_micro, 500_000);
    }

    #[tokio::test]
    async fn rpc_error_status_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503).set_body_string("upstream down"))
            .mount(&server)
            .await;
        let ix = SettlementIndexer::new(server.uri());
        let err = ix.fetch_signatures(10, None).await.expect_err("503");
        assert!(matches!(err, PayaiError::Rpc(m) if m.contains("status 503")));
    }

    // Live smoke test against real mainnet settlements. Ignored by default
    // (network + rate limits). Run with:
    //   cargo test -p covenant-payai -- --ignored live_payai_mainnet
    #[tokio::test]
    #[ignore]
    async fn live_payai_mainnet_settlements() {
        // publicnode by default: api.mainnet-beta throttles getTransaction so
        // the N+1 read stalls. Override with SOLANA_RPC_URL.
        let rpc = std::env::var("SOLANA_RPC_URL")
            .unwrap_or_else(|_| "https://solana-rpc.publicnode.com".to_string());
        let ix = SettlementIndexer::new(rpc);
        let settlements = ix.recent_settlements(8).await.expect("live fetch");
        eprintln!("live PayAI settlements parsed: {}", settlements.len());
        for s in settlements.iter().take(3) {
            eprintln!(
                "  {} {} -> {} : {} micro-USDC",
                &s.signature[..8.min(s.signature.len())],
                s.payer,
                s.pay_to,
                s.amount_micro
            );
        }
        assert!(
            !settlements.is_empty(),
            "expected at least one USDC settlement in the recent fee-payer history"
        );
    }
}
