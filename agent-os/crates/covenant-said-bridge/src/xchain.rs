//! Cross-chain inbox + outbound messaging via SAID's `/xchain` hub.
//!
//! The free tier covers 10 messages per agent per day at no cost. Beyond
//! that SAID returns 402 with an x402 challenge that the caller settles
//! via `covenant-x402-signer`. This module covers the unpaid surface;
//! the paid send is plumbed in P8.

use serde::{Deserialize, Serialize};

use crate::client::SaidBridge;
use crate::rest;
use crate::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxMessage {
    pub id: String,
    pub source_chain: String,
    pub source_address: String,
    pub target_chain: String,
    pub target_address: String,
    #[serde(default)]
    pub payload: Option<serde_json::Value>,
    #[serde(default)]
    pub created_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Inbox {
    #[serde(default)]
    pub chain: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub messages: Vec<InboxMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendRequest {
    pub source_chain: String,
    pub source_address: String,
    pub target_chain: String,
    pub target_address: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendReceipt {
    pub message_id: String,
    #[serde(default)]
    pub free_tier_remaining: Option<u32>,
    #[serde(default)]
    pub delivered_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FreeTierStatus {
    pub address: String,
    #[serde(default)]
    pub used: u32,
    pub remaining: u32,
    #[serde(default)]
    pub limit: u32,
    #[serde(default)]
    pub paid_price: Option<String>,
    #[serde(default)]
    pub payment_chains: Vec<PaymentChain>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentChain {
    pub name: String,
    pub network: String,
}

impl SaidBridge {
    pub async fn xchain_inbox(&self, chain: &str, address: &str) -> Result<Inbox> {
        self.require_enabled()?;
        let client = rest::build_client(self.config().rest_timeout)?;
        let path = format!("/xchain/inbox/{chain}/{address}");
        rest::get_json(&client, &self.config().api_base_url, &path).await
    }

    pub async fn xchain_free_tier(&self, address: &str) -> Result<FreeTierStatus> {
        self.require_enabled()?;
        let client = rest::build_client(self.config().rest_timeout)?;
        let path = format!("/xchain/free-tier/{address}");
        rest::get_json(&client, &self.config().api_base_url, &path).await
    }

    /// Send a free-tier message. If SAID returns 402 (free tier exhausted)
    /// the caller is responsible for settling via the x402 path; we surface
    /// the 402 verbatim so the outer signer can pick it up.
    pub async fn xchain_send(&self, req: &SendRequest) -> Result<SendReceipt> {
        self.require_enabled()?;
        let client = rest::build_client(self.config().rest_timeout)?;
        rest::post_json(&client, &self.config().api_base_url, "/xchain/message", req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_request_serializes_camel_case() {
        let req = SendRequest {
            source_chain: "solana".into(),
            source_address: "AdChc…".into(),
            target_chain: "base".into(),
            target_address: "0xabc…".into(),
            payload: serde_json::json!({ "hello": "world" }),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["sourceChain"], "solana");
        assert_eq!(json["targetAddress"], "0xabc…");
    }
}
