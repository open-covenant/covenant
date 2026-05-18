//! Minimal HTTP client for a gitlawb node.
//!
//! All write paths sign the outbound request with the configured signer using
//! the RFC 9421 scheme in [`crate::http_sig`]. Reads (`health`) do not require
//! a signature.
//!
//! Only the endpoints Covenant needs today are surfaced as typed methods. For
//! anything else, drop down to [`GitlawbClient::signed_post`] / [`GitlawbClient::signed_get`]
//! and pass the path + body directly; the signer wiring stays the same.

use ed25519_dalek::SigningKey;
use reqwest::{header::HeaderMap, Client as HttpClient, Method, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::config::Config;
use crate::http_sig::sign_request;
use crate::{GitlawbError, Result};

const SIG_INPUT_HEADER: &str = "signature-input";
const SIGNATURE_HEADER: &str = "signature";
const CONTENT_DIGEST_HEADER: &str = "content-digest";

/// A client bound to one gitlawb node URL and one signing key.
///
/// `Clone` is cheap, the underlying `reqwest::Client` and `SigningKey` are
/// both designed to be shared.
#[derive(Clone)]
pub struct GitlawbClient {
    config: Config,
    http: HttpClient,
    signer: SigningKey,
}

impl GitlawbClient {
    pub fn new(config: Config, signer: SigningKey) -> Self {
        Self {
            config,
            http: HttpClient::new(),
            signer,
        }
    }

    /// GET /health, unsigned. Returns `true` on a 2xx response.
    pub async fn health(&self) -> Result<bool> {
        self.ensure_enabled()?;
        let url = self.config.node_url.join("/health")?;
        let resp = self.http.get(url).send().await?;
        Ok(resp.status().is_success())
    }

    /// POST /api/register, registers `signer.verifying_key()` as a new agent
    /// on the node, returning the bootstrap UCAN the node issues back.
    pub async fn register(
        &self,
        capabilities: Vec<String>,
        model: Option<String>,
    ) -> Result<RegisterResponse> {
        let did = crate::did::did_key_from_verifying_key(&self.signer.verifying_key());
        let body = RegisterRequest {
            did,
            capabilities,
            model,
        };
        self.signed_post("/api/register", &body).await
    }

    /// Generic signed POST. Body is JSON-encoded, signed per RFC 9421, then
    /// sent. Response is parsed as JSON into `R`.
    pub async fn signed_post<B: Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<R> {
        self.signed_request(Method::POST, path, body).await
    }

    /// Generic signed GET. Useful for endpoints that require signed reads
    /// (some private repo lookups under gitlawb's auth middleware do).
    pub async fn signed_get<R: DeserializeOwned>(&self, path: &str) -> Result<R> {
        self.signed_request_no_body(Method::GET, path).await
    }

    async fn signed_request<B: Serialize, R: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: &B,
    ) -> Result<R> {
        self.ensure_enabled()?;
        let path_and_query = sanitize_path(path)?;
        let url = self.config.node_url.join(&path_and_query)?;
        let body_bytes = serde_json::to_vec(body)?;

        let headers = sign_request(&self.signer, method.as_str(), &path_and_query, &body_bytes);
        let req = self
            .http
            .request(method, url)
            .header("content-type", "application/json")
            .header(CONTENT_DIGEST_HEADER, &headers.content_digest)
            .header(SIG_INPUT_HEADER, &headers.signature_input)
            .header(SIGNATURE_HEADER, &headers.signature)
            .body(body_bytes);

        let resp = req.send().await?;
        decode_response(resp).await
    }

    async fn signed_request_no_body<R: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
    ) -> Result<R> {
        self.ensure_enabled()?;
        let path_and_query = sanitize_path(path)?;
        let url = self.config.node_url.join(&path_and_query)?;
        let headers = sign_request(&self.signer, method.as_str(), &path_and_query, &[]);
        let req = self
            .http
            .request(method, url)
            .header(CONTENT_DIGEST_HEADER, &headers.content_digest)
            .header(SIG_INPUT_HEADER, &headers.signature_input)
            .header(SIGNATURE_HEADER, &headers.signature);
        let resp = req.send().await?;
        decode_response(resp).await
    }

    fn ensure_enabled(&self) -> Result<()> {
        if self.config.enabled {
            Ok(())
        } else {
            Err(GitlawbError::Disabled)
        }
    }

    /// The `did:key` this client signs as.
    pub fn did(&self) -> String {
        crate::did::did_key_from_verifying_key(&self.signer.verifying_key())
    }

    /// Extract the three signature headers from a response, for callers that
    /// want to verify the node's reply. The node signs every response in the
    /// same RFC 9421 envelope.
    pub fn extract_signed_headers(headers: &HeaderMap) -> Option<crate::http_sig::SignedHeaders> {
        let content_digest = headers
            .get(CONTENT_DIGEST_HEADER)?
            .to_str()
            .ok()?
            .to_string();
        let signature_input = headers.get(SIG_INPUT_HEADER)?.to_str().ok()?.to_string();
        let signature = headers.get(SIGNATURE_HEADER)?.to_str().ok()?.to_string();
        Some(crate::http_sig::SignedHeaders {
            content_digest,
            signature_input,
            signature,
        })
    }
}

async fn decode_response<R: DeserializeOwned>(resp: reqwest::Response) -> Result<R> {
    let status = resp.status();
    if status.is_success() || status == StatusCode::CREATED {
        Ok(resp.json::<R>().await?)
    } else {
        let code = status.as_u16();
        let body = resp.text().await.unwrap_or_default();
        Err(GitlawbError::Rpc { status: code, body })
    }
}

fn sanitize_path(path: &str) -> Result<String> {
    if !path.starts_with('/') {
        return Err(GitlawbError::Invalid(format!(
            "path must start with '/', got '{path}'"
        )));
    }
    Ok(path.to_string())
}

#[derive(Debug, Clone, Serialize)]
pub struct RegisterRequest {
    pub did: String,
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegisterResponse {
    pub status: String,
    pub did: String,
    pub ucan: String,
    pub node: String,
    pub expires: String,
    pub trust_score: f64,
    pub capabilities: Vec<String>,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;
    use rand::TryRngCore;

    fn fresh() -> SigningKey {
        let mut seed = [0u8; 32];
        OsRng.try_fill_bytes(&mut seed).unwrap();
        SigningKey::from_bytes(&seed)
    }

    #[tokio::test]
    async fn disabled_client_short_circuits() {
        let client = GitlawbClient::new(Config::disabled(), fresh());
        let err = client.health().await.unwrap_err();
        assert!(matches!(err, GitlawbError::Disabled));
        let err: Result<RegisterResponse> = client.register(vec![], None).await;
        assert!(matches!(err.unwrap_err(), GitlawbError::Disabled));
    }

    #[test]
    fn did_helper_matches_signer() {
        let sk = fresh();
        let vk_did = crate::did::did_key_from_verifying_key(&sk.verifying_key());
        let client = GitlawbClient::new(Config::disabled(), sk);
        assert_eq!(client.did(), vk_did);
    }

    #[test]
    fn rejects_relative_path() {
        let err = sanitize_path("api/register").unwrap_err();
        assert!(matches!(err, GitlawbError::Invalid(_)));
    }
}
