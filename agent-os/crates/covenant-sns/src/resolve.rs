//! SNS read client.
//!
//! Resolution over the hosted SNS REST resolver: a keyless GET that returns
//! `{"s":"ok","result":...}`. Nothing leaves the host but the name being
//! looked up. A trait so tests (and a future on-chain RPC fallback) can stand
//! in without HTTP.

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum SnsError {
    #[error("SNS resolver is not configured")]
    NotConfigured,
    #[error("SNS transport: {0}")]
    Transport(String),
    #[error("SNS resolver error: {0}")]
    Resolver(String),
    #[error("not found: {0}")]
    NotFound(String),
}

/// One domain owned by a wallet, from a reverse lookup.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResolvedDomain {
    /// Full name including the `.sol` suffix (`bonfida.sol`).
    pub domain: String,
    /// The domain's on-chain account pubkey.
    pub key: String,
}

#[async_trait]
pub trait SnsResolver: Send + Sync {
    /// Forward: a `.sol` name to its owner wallet pubkey.
    async fn resolve(&self, name: &str) -> Result<String, SnsError>;
    /// Reverse: the `.sol` domains a wallet owns.
    async fn reverse(&self, owner: &str) -> Result<Vec<ResolvedDomain>, SnsError>;
    /// A typed record (`url`, `ipfs`, `email`, ...) on a domain, decoded to text.
    async fn record(&self, name: &str, record: &str) -> Result<String, SnsError>;
}

/// HTTP-backed resolver. Constructing it with an empty base URL is allowed and
/// every call returns [`SnsError::NotConfigured`], so the daemon can build it
/// unconditionally and let config gate whether the tools are advertised.
pub struct HttpSnsResolver {
    base: String,
    http: reqwest::Client,
}

impl HttpSnsResolver {
    pub fn new(base: impl Into<String>) -> Self {
        // A short timeout matters: resolution sits in front of interactive CLI
        // and verify flows, so a stalled resolver must fail fast.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .connect_timeout(std::time::Duration::from_secs(6))
            .build()
            .unwrap_or_default();
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            http,
        }
    }

    async fn get(&self, path: &str) -> Result<Value, SnsError> {
        if self.base.is_empty() {
            return Err(SnsError::NotConfigured);
        }
        let url = format!("{}/{}", self.base, path);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| SnsError::Transport(e.to_string()))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| SnsError::Transport(format!("{status}: {e}")))?;
        match body.get("s").and_then(Value::as_str) {
            Some("ok") => Ok(body.get("result").cloned().unwrap_or(Value::Null)),
            _ => Err(SnsError::Resolver(
                body.get("result")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown resolver error")
                    .to_string(),
            )),
        }
    }
}

#[async_trait]
impl SnsResolver for HttpSnsResolver {
    async fn resolve(&self, name: &str) -> Result<String, SnsError> {
        let n = crate::normalize_domain(name);
        if n.is_empty() {
            return Err(SnsError::NotFound(name.to_string()));
        }
        let v = self.get(&format!("resolve/{n}")).await?;
        v.as_str()
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| SnsError::NotFound(name.to_string()))
    }

    async fn reverse(&self, owner: &str) -> Result<Vec<ResolvedDomain>, SnsError> {
        let v = self.get(&format!("domains/{owner}")).await?;
        let arr = v
            .as_array()
            .ok_or_else(|| SnsError::Resolver("expected an array of domains".into()))?;
        Ok(arr
            .iter()
            .filter_map(|e| {
                let domain = e.get("domain").and_then(Value::as_str)?;
                let key = e.get("key").and_then(Value::as_str)?;
                Some(ResolvedDomain {
                    domain: format!("{domain}{}", crate::SOL_TLD),
                    key: key.to_string(),
                })
            })
            .collect())
    }

    async fn record(&self, name: &str, record: &str) -> Result<String, SnsError> {
        let n = crate::normalize_domain(name);
        let v = self.get(&format!("record/{n}/{record}")).await?;
        let raw = v
            .as_str()
            .ok_or_else(|| SnsError::NotFound(format!("{name}/{record}")))?;
        Ok(decode_record(raw))
    }
}

/// SNS records come back base64-encoded and right-padded with null bytes to a
/// fixed width. Decode and drop the padding; fall back to the raw string if it
/// is not valid base64.
fn decode_record(b64: &str) -> String {
    match B64.decode(b64) {
        Ok(bytes) => {
            let content: Vec<u8> = bytes.into_iter().take_while(|b| *b != 0).collect();
            String::from_utf8_lossy(&content).to_string()
        }
        Err(_) => b64.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn empty_base_is_not_configured() {
        let r = HttpSnsResolver::new("");
        assert!(matches!(
            r.resolve("bonfida.sol").await,
            Err(SnsError::NotConfigured)
        ));
    }

    #[tokio::test]
    async fn resolve_strips_suffix_and_returns_owner() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/resolve/bonfida"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "s": "ok", "result": "Fw1ETanDZafof7xEULsnq9UY6o71Tpds89tNwPkWLb1v"
            })))
            .mount(&server)
            .await;
        let r = HttpSnsResolver::new(server.uri());
        assert_eq!(
            r.resolve("bonfida.sol").await.unwrap(),
            "Fw1ETanDZafof7xEULsnq9UY6o71Tpds89tNwPkWLb1v"
        );
    }

    #[tokio::test]
    async fn resolver_error_maps_to_resolver_variant() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/resolve/ghostzzz"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "s": "error", "result": "Domain not found"
            })))
            .mount(&server)
            .await;
        let r = HttpSnsResolver::new(server.uri());
        assert!(matches!(
            r.resolve("ghostzzz").await,
            Err(SnsError::Resolver(m)) if m.contains("not found")
        ));
    }

    #[tokio::test]
    async fn reverse_lists_domains_with_suffix() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/domains/OwnerWallet"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "s": "ok",
                "result": [
                    { "key": "Key1", "domain": "alpha" },
                    { "key": "Key2", "domain": "beta" }
                ]
            })))
            .mount(&server)
            .await;
        let r = HttpSnsResolver::new(server.uri());
        let domains = r.reverse("OwnerWallet").await.unwrap();
        assert_eq!(domains.len(), 2);
        assert_eq!(domains[0].domain, "alpha.sol");
        assert_eq!(domains[0].key, "Key1");
        assert_eq!(domains[1].domain, "beta.sol");
    }

    #[tokio::test]
    async fn record_decodes_base64_and_strips_null_padding() {
        let server = MockServer::start().await;
        let mut padded = b"https://opencovenant.org".to_vec();
        padded.extend([0u8; 40]);
        let encoded = B64.encode(&padded);
        Mock::given(method("GET"))
            .and(path("/record/bonfida/url"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "s": "ok", "result": encoded
            })))
            .mount(&server)
            .await;
        let r = HttpSnsResolver::new(server.uri());
        assert_eq!(
            r.record("bonfida.sol", "url").await.unwrap(),
            "https://opencovenant.org"
        );
    }
}
