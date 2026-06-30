//! REST client for `api.saidprotocol.com`. Off-chain and xchain only;
//! on-chain instructions go through the worker module.

use std::time::Duration;

use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::Serialize;
use url::Url;

use crate::{BridgeError, Result};

const MAX_REST_BODY_BYTES: u64 = 1 << 20;

pub(crate) fn build_client(timeout: Duration) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(concat!("covenant-said-bridge/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| BridgeError::Rest(format!("build client: {e}")))
}

fn join(base: &str, path: &str) -> Result<Url> {
    let base = Url::parse(base).map_err(|e| BridgeError::Invalid(format!("bad api base: {e}")))?;
    base.join(path)
        .map_err(|e| BridgeError::Invalid(format!("bad path {path}: {e}")))
}

pub(crate) async fn post_json<P, T>(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    payload: &P,
) -> Result<T>
where
    P: Serialize,
    T: DeserializeOwned,
{
    let url = join(base, path)?;
    let response = client
        .post(url)
        .json(payload)
        .send()
        .await
        .map_err(|e| BridgeError::Rest(e.to_string()))?;
    decode(response).await
}

pub(crate) async fn get_json<T>(client: &reqwest::Client, base: &str, path: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    let url = join(base, path)?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| BridgeError::Rest(e.to_string()))?;
    decode(response).await
}

async fn read_capped(response: reqwest::Response) -> Result<Vec<u8>> {
    if let Some(len) = response.content_length() {
        if len > MAX_REST_BODY_BYTES {
            return Err(BridgeError::Rest(format!(
                "response body advertises {len} bytes, cap is {MAX_REST_BODY_BYTES}"
            )));
        }
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| BridgeError::Rest(format!("read body: {e}")))?;
    if bytes.len() as u64 > MAX_REST_BODY_BYTES {
        return Err(BridgeError::Rest(format!(
            "response body is {} bytes, cap is {MAX_REST_BODY_BYTES}",
            bytes.len()
        )));
    }
    Ok(bytes.to_vec())
}

async fn decode<T: DeserializeOwned>(response: reqwest::Response) -> Result<T> {
    let status = response.status();
    if status == StatusCode::PAYMENT_REQUIRED {
        let body =
            String::from_utf8_lossy(&read_capped(response).await.unwrap_or_default()).into_owned();
        return Err(BridgeError::Http { status: 402, body });
    }
    if !status.is_success() {
        let body =
            String::from_utf8_lossy(&read_capped(response).await.unwrap_or_default()).into_owned();
        return Err(BridgeError::Http {
            status: status.as_u16(),
            body,
        });
    }
    let body = read_capped(response).await?;
    serde_json::from_slice::<T>(&body).map_err(|e| BridgeError::Decode(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_rejects_bad_base() {
        let err = join("not a url", "/api/agents").unwrap_err();
        assert!(matches!(err, BridgeError::Invalid(_)));
    }

    #[test]
    fn join_resolves_relative() {
        let url = join("https://api.saidprotocol.com", "/api/agents/abc").unwrap();
        assert_eq!(url.as_str(), "https://api.saidprotocol.com/api/agents/abc");
    }
}
