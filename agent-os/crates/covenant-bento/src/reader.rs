//! On-chain reader for an agent's Bento standing.
//!
//! The fetch path is built: a keyless `getAccountInfo` JSON-RPC call that
//! returns an account's raw bytes by pubkey (no solana-sdk dependency, the
//! same shape covenant-x402 uses). The two pieces that need Bento to publish
//! their on-chain details are isolated as seams: deriving the reputation PDA
//! (the seeds) and decoding the account ([`decode_standing`]). Until those
//! land, [`BentoReader::standing`] returns [`BentoError::LayoutPending`].

use std::time::Duration;

use base64::Engine;
use serde_json::{json, Value};

use crate::types::BentoStanding;
use crate::{BentoError, Result};

pub struct BentoReader {
    http: reqwest::Client,
    rpc_url: String,
}

impl BentoReader {
    pub fn new(rpc_url: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .expect("build bento rpc client");
        Self {
            http,
            rpc_url: rpc_url.into(),
        }
    }

    /// An agent's Bento standing, derived from its on-chain accounts.
    ///
    /// Blocked on [`BentoError::LayoutPending`] at PDA derivation, before any
    /// network call: deriving the reputation PDA and decoding it both need
    /// Bento's program id, seeds, and layout. The account fetch underneath
    /// ([`Self::account_bytes`]) is live and tested, so this becomes a
    /// two-function change once the layout is known. An agent that has never
    /// interacted with Bento has no account and surfaces as
    /// [`BentoError::NotFound`], distinct from the layout gate.
    pub async fn standing(&self, agent_pubkey: &str, program_id: &str) -> Result<BentoStanding> {
        let pda = derive_reputation_pda(agent_pubkey, program_id)?;
        let bytes = self
            .account_bytes(&pda)
            .await?
            .ok_or(BentoError::NotFound)?;
        decode_standing(&bytes)
    }

    /// Raw account bytes by pubkey via `getAccountInfo`, or `None` when the
    /// account does not exist. A malformed-but-2xx body is a decode error, not
    /// a silent "absent".
    pub async fn account_bytes(&self, pubkey: &str) -> Result<Option<Vec<u8>>> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getAccountInfo",
            "params": [pubkey, { "encoding": "base64" }],
        });
        let resp = self.http.post(&self.rpc_url).json(&body).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(BentoError::Decode(format!(
                "getAccountInfo [{}]: {}",
                status.as_u16(),
                truncate(&text)
            )));
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| {
            BentoError::Decode(format!("getAccountInfo: {e}; body: {}", truncate(&text)))
        })?;
        if let Some(err) = v.get("error") {
            return Err(BentoError::Decode(format!("rpc error: {err}")));
        }
        let value = v.pointer("/result/value").ok_or_else(|| {
            BentoError::Decode(format!(
                "getAccountInfo: missing result.value; body: {}",
                truncate(&text)
            ))
        })?;
        if value.is_null() {
            return Ok(None);
        }
        let b64 = value
            .pointer("/data/0")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BentoError::Decode("getAccountInfo: account present but no base64 data".into())
            })?;
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map(Some)
            .map_err(|e| BentoError::Decode(format!("account base64: {e}")))
    }
}

/// Derive the reputation PDA for an agent. SEAM: the program id is known
/// (`config::DEVNET_PROGRAM_ID`), but the per-agent reputation account lives on
/// Bento's MagicBlock ER, not base chain (only the global config account sits
/// on devnet), so the read needs the ER RPC and the agent PDA seeds, neither
/// published yet.
fn derive_reputation_pda(_agent_pubkey: &str, _program_id: &str) -> Result<String> {
    Err(BentoError::LayoutPending)
}

/// Decode a Bento reputation account into a standing. SEAM: the account layout
/// is Bento's and not yet published. The live relayer config pins the model
/// (block at 70000/100000, max 5 strikes, EMA alpha 0.7), but the field offsets
/// are not.
fn decode_standing(_bytes: &[u8]) -> Result<BentoStanding> {
    Err(BentoError::LayoutPending)
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
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn account_bytes_decodes_base64() {
        let server = MockServer::start().await;
        // "hi" base64 = "aGk=".
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0", "id": 1,
                "result": { "value": { "data": ["aGk=", "base64"], "owner": "x", "lamports": 1 } }
            })))
            .mount(&server)
            .await;
        let reader = BentoReader::new(server.uri());
        let bytes = reader.account_bytes("Agent111").await.unwrap();
        assert_eq!(bytes.as_deref(), Some(&b"hi"[..]));
    }

    #[tokio::test]
    async fn null_value_is_none() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0", "id": 1, "result": { "value": null }
            })))
            .mount(&server)
            .await;
        let reader = BentoReader::new(server.uri());
        assert!(reader.account_bytes("Agent111").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn malformed_2xx_is_a_decode_error_not_absent() {
        // A 2xx that lacks result.value must not be swallowed as "no account".
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0", "id": 1
            })))
            .mount(&server)
            .await;
        let reader = BentoReader::new(server.uri());
        let err = reader.account_bytes("Agent111").await.unwrap_err();
        assert!(matches!(err, BentoError::Decode(_)));
    }

    #[tokio::test]
    async fn standing_is_gated_at_pda_derivation() {
        // The read is blocked at PDA derivation, before any network call; it
        // must not silently fabricate a value.
        let reader = BentoReader::new("http://127.0.0.1:1");
        let err = reader.standing("Agent111", "Program111").await.unwrap_err();
        assert!(matches!(err, BentoError::LayoutPending));
    }
}
