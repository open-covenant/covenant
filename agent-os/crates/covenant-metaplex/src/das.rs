//! Digital Asset Standard (DAS) read client.
//!
//! DAS is Metaplex's unified JSON-RPC read interface over MPL Core,
//! Token Metadata, and compressed (Bubblegum) assets — the same surface
//! Helius / Triton / QuickNode expose. Reads are stateless and
//! key-free: this client only ever POSTs a JSON-RPC envelope and returns
//! the `result` value. No signing, no funds, nothing leaves the host but
//! a query.

use async_trait::async_trait;
use serde_json::{json, Value};

#[derive(Debug, thiserror::Error)]
pub enum DasError {
    #[error("DAS endpoint is not configured")]
    NotConfigured,
    #[error("DAS transport: {0}")]
    Transport(String),
    #[error("DAS rpc error: {0}")]
    Rpc(String),
}

/// The read surface the daemon's tools call. A trait so tests (and a
/// future native indexer client) can stand in without HTTP.
#[async_trait]
pub trait DasClient: Send + Sync {
    /// `getAsset` — one asset (NFT / cNFT / fungible) by id.
    async fn get_asset(&self, id: &str) -> Result<Value, DasError>;
    /// `getAssetProof` — the merkle proof for a compressed asset.
    async fn get_asset_proof(&self, id: &str) -> Result<Value, DasError>;
    /// `getAssetsByOwner` — page through a wallet's assets.
    async fn get_assets_by_owner(
        &self,
        owner: &str,
        limit: u32,
        page: u32,
    ) -> Result<Value, DasError>;
    /// `searchAssets` — pass-through of the caller's structured query.
    async fn search_assets(&self, params: Value) -> Result<Value, DasError>;
}

/// HTTP-backed DAS client. Holds only an endpoint URL and a reqwest
/// client; constructing it with an empty endpoint is allowed and every
/// call returns [`DasError::NotConfigured`] so the daemon can build it
/// unconditionally and let config gate whether the tools are advertised.
pub struct HttpDasClient {
    endpoint: String,
    http: reqwest::Client,
}

impl HttpDasClient {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            http: reqwest::Client::new(),
        }
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value, DasError> {
        if self.endpoint.is_empty() {
            return Err(DasError::NotConfigured);
        }
        let body = json!({
            "jsonrpc": "2.0",
            "id": "covenant-metaplex",
            "method": method,
            "params": params,
        });
        let resp = self
            .http
            .post(&self.endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| DasError::Transport(e.to_string()))?;
        let status = resp.status();
        let value: Value = resp
            .json()
            .await
            .map_err(|e| DasError::Transport(format!("{status}: {e}")))?;
        if let Some(err) = value.get("error").filter(|e| !e.is_null()) {
            return Err(DasError::Rpc(err.to_string()));
        }
        Ok(value.get("result").cloned().unwrap_or(Value::Null))
    }
}

#[async_trait]
impl DasClient for HttpDasClient {
    async fn get_asset(&self, id: &str) -> Result<Value, DasError> {
        self.rpc("getAsset", json!({ "id": id })).await
    }

    async fn get_asset_proof(&self, id: &str) -> Result<Value, DasError> {
        self.rpc("getAssetProof", json!({ "id": id })).await
    }

    async fn get_assets_by_owner(
        &self,
        owner: &str,
        limit: u32,
        page: u32,
    ) -> Result<Value, DasError> {
        self.rpc(
            "getAssetsByOwner",
            json!({ "ownerAddress": owner, "limit": limit, "page": page }),
        )
        .await
    }

    async fn search_assets(&self, params: Value) -> Result<Value, DasError> {
        self.rpc("searchAssets", params).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn empty_endpoint_is_not_configured() {
        let c = HttpDasClient::new("");
        let err = c.get_asset("abc").await.expect_err("must refuse");
        assert!(matches!(err, DasError::NotConfigured));
    }

    #[tokio::test]
    async fn every_method_refuses_an_empty_endpoint() {
        let c = HttpDasClient::new("");
        assert!(matches!(
            c.get_asset_proof("abc").await.expect_err("proof"),
            DasError::NotConfigured
        ));
        assert!(matches!(
            c.get_assets_by_owner("owner", 10, 1)
                .await
                .expect_err("owner"),
            DasError::NotConfigured
        ));
        assert!(matches!(
            c.search_assets(json!({})).await.expect_err("search"),
            DasError::NotConfigured
        ));
    }

    async fn das_returning(body: ResponseTemplate) -> (MockServer, HttpDasClient) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(body)
            .mount(&server)
            .await;
        let client = HttpDasClient::new(server.uri());
        (server, client)
    }

    #[tokio::test]
    async fn rpc_returns_the_result_value() {
        let (_s, c) = das_returning(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": "covenant-metaplex",
            "result": { "id": "asset-1", "ownership": { "owner": "wallet" } }
        })))
        .await;
        let v = c.get_asset("asset-1").await.expect("result");
        assert_eq!(
            v,
            json!({ "id": "asset-1", "ownership": { "owner": "wallet" } })
        );
    }

    #[tokio::test]
    async fn rpc_surfaces_a_jsonrpc_error_as_rpc_error() {
        let (_s, c) = das_returning(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "error": { "code": -32000, "message": "asset not found" }
        })))
        .await;
        let err = c.get_asset("missing").await.expect_err("rpc error");
        assert!(
            matches!(err, DasError::Rpc(m) if m.contains("asset not found")),
            "an rpc error must not read as a successful empty result"
        );
    }

    #[tokio::test]
    async fn rpc_treats_an_explicit_null_error_as_success() {
        // Some providers always include the "error" key; a null error is not an
        // error and must not discard a valid result.
        let (_s, c) = das_returning(ResponseTemplate::new(200).set_body_json(json!({
            "error": null,
            "result": { "ok": 1 }
        })))
        .await;
        let v = c.get_asset("x").await.expect("null error is not an error");
        assert_eq!(v, json!({ "ok": 1 }));
    }

    #[tokio::test]
    async fn rpc_missing_result_field_is_null() {
        let (_s, c) =
            das_returning(ResponseTemplate::new(200).set_body_json(json!({ "jsonrpc": "2.0" })))
                .await;
        let v = c.get_asset("x").await.expect("no result field");
        assert_eq!(v, Value::Null);
    }

    #[tokio::test]
    async fn rpc_non_json_body_is_a_transport_error() {
        let (_s, c) =
            das_returning(ResponseTemplate::new(200).set_body_string("<html>gateway</html>")).await;
        let err = c.get_asset("x").await.expect_err("non-json");
        assert!(matches!(err, DasError::Transport(_)));
    }
}
