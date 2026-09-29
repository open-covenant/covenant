//! The handful of JSON-RPC calls a lease needs, over plain `reqwest`.
//!
//! The same endpoint shape serves Solana L1 and a MagicBlock rollup, so one
//! client drives both.

use std::str::FromStr;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::{json, Value};
use solana_sdk::{hash::Hash, pubkey::Pubkey, transaction::Transaction};

pub struct Account {
    pub owner: Pubkey,
    pub executable: bool,
    pub data: Vec<u8>,
}

/// Why a send did not produce a confirmed transaction.
#[derive(Debug)]
pub enum SendError {
    /// The node refused it or it failed on chain. Nothing it carried took
    /// effect.
    Refused(String),
    /// It may still land: the response was lost, the transport failed after
    /// the request left, or confirmation timed out.
    Unknown { signature: String, message: String },
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Refused(message) => f.write_str(message),
            SendError::Unknown { signature, message } => write!(f, "{message} (tx {signature})"),
        }
    }
}

enum CallError {
    /// The request may or may not have been processed.
    Transport(String),
    /// The node answered with a JSON-RPC error object.
    Node(Value),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Transport(message) => f.write_str(message),
            CallError::Node(error) => write!(f, "rpc error {error}"),
        }
    }
}

pub struct Rpc {
    url: String,
    http: reqwest::Client,
}

impl Rpc {
    pub fn new(url: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        Self {
            url: url.to_string(),
            http,
        }
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, CallError> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let response = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| CallError::Transport(format!("{method}: {e}")))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(CallError::Transport(format!(
                "{method}: http {status}: {text}"
            )));
        }
        let parsed: Value = response
            .json()
            .await
            .map_err(|e| CallError::Transport(format!("{method}: decode response: {e}")))?;
        if let Some(error) = parsed.get("error").filter(|e| !e.is_null()) {
            return Err(CallError::Node(error.clone()));
        }
        Ok(parsed.get("result").cloned().unwrap_or(Value::Null))
    }

    pub async fn account(&self, address: &Pubkey) -> Result<Option<Account>, String> {
        let result = self
            .call(
                "getAccountInfo",
                json!([address.to_string(), {"encoding": "base64", "commitment": "confirmed"}]),
            )
            .await
            .map_err(|e| e.to_string())?;
        let value = &result["value"];
        if value.is_null() {
            return Ok(None);
        }
        let owner = value["owner"]
            .as_str()
            .and_then(|s| Pubkey::from_str(s).ok())
            .ok_or_else(|| format!("getAccountInfo {address}: no owner in {value}"))?;
        let data = value["data"][0]
            .as_str()
            .map(|b64| BASE64.decode(b64))
            .transpose()
            .map_err(|e| format!("getAccountInfo {address}: bad base64: {e}"))?
            .unwrap_or_default();
        Ok(Some(Account {
            owner,
            executable: value["executable"].as_bool().unwrap_or(false),
            data,
        }))
    }

    pub async fn identity(&self) -> Result<String, String> {
        let result = self
            .call("getIdentity", json!([]))
            .await
            .map_err(|e| e.to_string())?;
        result["identity"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("getIdentity: no identity in {result}"))
    }

    pub async fn latest_blockhash(&self) -> Result<Hash, String> {
        let result = self
            .call("getLatestBlockhash", json!([{"commitment": "confirmed"}]))
            .await
            .map_err(|e| e.to_string())?;
        result["value"]["blockhash"]
            .as_str()
            .and_then(|s| Hash::from_str(s).ok())
            .ok_or_else(|| format!("getLatestBlockhash: no blockhash in {result}"))
    }

    /// The newest signature that touched `address`, for answering a repeated
    /// request with the transaction that already did the work.
    pub async fn latest_signature(&self, address: &Pubkey) -> Result<Option<String>, String> {
        let result = self
            .call(
                "getSignaturesForAddress",
                json!([address.to_string(), {"limit": 1, "commitment": "confirmed"}]),
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(result[0]["signature"].as_str().map(str::to_string))
    }

    /// The recent signature on `address` whose memo contains `memo`. The RPC
    /// reports each transaction's memo prefixed with its length, so this
    /// matches on containment.
    pub async fn signature_with_memo(
        &self,
        address: &Pubkey,
        memo: &str,
    ) -> Result<Option<String>, String> {
        let result = self
            .call(
                "getSignaturesForAddress",
                json!([address.to_string(), {"limit": 25, "commitment": "confirmed"}]),
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(result
            .as_array()
            .into_iter()
            .flatten()
            .filter(|entry| entry["err"].is_null())
            .find(|entry| entry["memo"].as_str().is_some_and(|m| m.contains(memo)))
            .and_then(|entry| entry["signature"].as_str())
            .map(str::to_string))
    }

    /// Accounts owned by `program` that pass `filters`, as (address, data).
    pub async fn program_accounts(
        &self,
        program: &Pubkey,
        filters: Value,
    ) -> Result<Vec<(Pubkey, Vec<u8>)>, String> {
        let result = self
            .call(
                "getProgramAccounts",
                json!([program.to_string(), {"encoding": "base64", "commitment": "confirmed", "filters": filters}]),
            )
            .await
            .map_err(|e| e.to_string())?;
        result
            .as_array()
            .ok_or_else(|| format!("getProgramAccounts: no result in {result}"))?
            .iter()
            .map(|entry| {
                let address = entry["pubkey"]
                    .as_str()
                    .and_then(|s| Pubkey::from_str(s).ok())
                    .ok_or("getProgramAccounts: entry without an address")?;
                let data = entry["account"]["data"][0]
                    .as_str()
                    .map(|b64| BASE64.decode(b64))
                    .transpose()
                    .map_err(|e| format!("getProgramAccounts: bad base64: {e}"))?
                    .unwrap_or_default();
                Ok((address, data))
            })
            .collect()
    }

    /// Sends a signed transaction and waits for it to be confirmed.
    pub async fn send_and_confirm(
        &self,
        tx: &Transaction,
        timeout: Duration,
    ) -> Result<String, SendError> {
        let signature = tx
            .signatures
            .first()
            .map(|s| s.to_string())
            .ok_or_else(|| SendError::Refused("transaction is unsigned".into()))?;
        let raw =
            bincode::serialize(tx).map_err(|e| SendError::Refused(format!("serialize: {e}")))?;
        match self
            .call(
                "sendTransaction",
                json!([BASE64.encode(raw), {"encoding": "base64", "preflightCommitment": "confirmed"}]),
            )
            .await
        {
            Ok(_) => {}
            Err(CallError::Node(error)) => return Err(SendError::Refused(describe_node_error(&error))),
            Err(CallError::Transport(message)) => return Err(SendError::Unknown { signature, message }),
        }
        self.confirm(&signature, timeout).await
    }

    async fn confirm(&self, signature: &str, timeout: Duration) -> Result<String, SendError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut last_error = String::from("not seen by the cluster");
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let result = match self
                .call(
                    "getSignatureStatuses",
                    json!([[signature], {"searchTransactionHistory": true}]),
                )
                .await
            {
                Ok(result) => result,
                Err(e) => {
                    last_error = e.to_string();
                    continue;
                }
            };
            let status = &result["value"][0];
            if status.is_null() {
                continue;
            }
            if let Some(err) = status.get("err").filter(|e| !e.is_null()) {
                return Err(SendError::Refused(format!(
                    "transaction {signature} failed on chain: {err}"
                )));
            }
            if matches!(
                status["confirmationStatus"].as_str(),
                Some("confirmed" | "finalized")
            ) {
                return Ok(signature.to_string());
            }
        }
        Err(SendError::Unknown {
            signature: signature.to_string(),
            message: format!("not confirmed within {}s: {last_error}", timeout.as_secs()),
        })
    }
}

/// A preflight failure carries the program's logs; the custom error code in
/// them is the useful part.
fn describe_node_error(error: &Value) -> String {
    let message = error["message"].as_str().unwrap_or("rpc error").to_string();
    let logs: Vec<&str> = error["data"]["logs"]
        .as_array()
        .map(|logs| logs.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    match logs.iter().rev().find(|l| l.contains("Error")) {
        Some(line) => format!("{message}: {line}"),
        None => message,
    }
}
