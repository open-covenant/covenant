use serde_json::Value;

use crate::error::{Error, Result};
use crate::signer::RequestSigner;
use crate::transport::{HttpRequest, Transport};
use crate::types::{OrderRequest, Side};

/// Client over the Robinhood Crypto Trading API. Reads are safe to run against
/// a live account; the write methods (`place_order` / `cancel_order`) are meant
/// to be reached through the daemon capability gate, never called raw in prod.
///
/// Signing is pluggable: in-process via `RobinhoodSigner`, or out-of-process via
/// a sidecar that holds the key (so the daemon never sees the seed).
pub struct RobinhoodClient<T: Transport> {
    base_url: String,
    signer: Box<dyn RequestSigner>,
    transport: T,
}

impl<T: Transport> RobinhoodClient<T> {
    pub fn new(signer: impl RequestSigner + 'static, transport: T) -> Self {
        Self {
            base_url: crate::DEFAULT_BASE_URL.to_string(),
            signer: Box::new(signer),
            transport,
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    async fn request(&self, method: &str, path: &str, body: Option<String>) -> Result<Value> {
        let body_str = body.clone().unwrap_or_default();
        let h = self
            .signer
            .signed_headers(method, path, &body_str, crate::unix_now())
            .await?;
        let mut headers = vec![
            ("x-api-key".to_string(), h.api_key),
            ("x-timestamp".to_string(), h.timestamp),
            ("x-signature".to_string(), h.signature),
        ];
        if body.is_some() {
            headers.push(("Content-Type".to_string(), "application/json".to_string()));
        }
        let resp = self
            .transport
            .send(HttpRequest {
                method: method.to_string(),
                url: format!("{}{}", self.base_url, path),
                headers,
                body,
            })
            .await?;
        if !(200..300).contains(&resp.status) {
            return Err(Error::Http {
                status: resp.status,
                body: resp.body,
            });
        }
        if resp.body.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&resp.body).map_err(|e| Error::Decode(e.to_string()))
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.request("GET", path, None).await
    }

    pub async fn account(&self) -> Result<Value> {
        self.get("/api/v1/crypto/trading/accounts/").await
    }

    pub async fn holdings(&self, asset_codes: &[&str]) -> Result<Value> {
        let path = with_query(
            "/api/v1/crypto/trading/holdings/",
            &pairs("asset_code", asset_codes),
        );
        self.get(&path).await
    }

    pub async fn best_bid_ask(&self, symbols: &[&str]) -> Result<Value> {
        let path = with_query(
            "/api/v1/crypto/marketdata/best_bid_ask/",
            &pairs("symbol", symbols),
        );
        self.get(&path).await
    }

    pub async fn estimated_price(
        &self,
        symbol: &str,
        side: Side,
        quantities: &[f64],
    ) -> Result<Value> {
        let side_s = match side {
            Side::Buy => "ask",
            Side::Sell => "bid",
        };
        let qtys = quantities
            .iter()
            .map(|q| q.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let query = vec![
            ("symbol".to_string(), symbol.to_string()),
            ("side".to_string(), side_s.to_string()),
            ("quantity".to_string(), qtys),
        ];
        let path = with_query("/api/v1/crypto/marketdata/estimated_price/", &query);
        self.get(&path).await
    }

    pub async fn trading_pairs(&self, symbols: &[&str]) -> Result<Value> {
        let path = with_query(
            "/api/v1/crypto/trading/trading_pairs/",
            &pairs("symbol", symbols),
        );
        self.get(&path).await
    }

    pub async fn order(&self, order_id: &str) -> Result<Value> {
        self.get(&format!("/api/v1/crypto/trading/orders/{order_id}/"))
            .await
    }

    pub async fn place_order(&self, order: &OrderRequest) -> Result<Value> {
        let body = serde_json::to_string(order).map_err(|e| Error::Decode(e.to_string()))?;
        self.request("POST", "/api/v1/crypto/trading/orders/", Some(body))
            .await
    }

    pub async fn cancel_order(&self, order_id: &str) -> Result<Value> {
        let path = format!("/api/v1/crypto/trading/orders/{order_id}/cancel/");
        self.request("POST", &path, Some(String::new())).await
    }
}

fn pairs(key: &str, values: &[&str]) -> Vec<(String, String)> {
    values
        .iter()
        .map(|v| (key.to_string(), (*v).to_string()))
        .collect()
}

fn with_query(path: &str, query: &[(String, String)]) -> String {
    if query.is_empty() {
        return path.to_string();
    }
    let qs = query
        .iter()
        .map(|(k, v)| format!("{}={}", k, urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{path}?{qs}")
}

/// Encode a query value. The path is signed verbatim and sent verbatim, so the
/// encoding here must match the bytes on the wire. Robinhood's symbols and
/// quantity lists (`BTC-USD`, `0.1,1,5`) keep dash, dot, and comma literal.
fn urlencode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' | ',' => c.to_string(),
            _ => format!("%{:02X}", c as u32 & 0xFF),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signer::RobinhoodSigner;
    use crate::transport::MockTransport;
    use ed25519_dalek::SigningKey;
    use serde_json::json;

    fn client(mock: MockTransport) -> RobinhoodClient<MockTransport> {
        let signer = RobinhoodSigner::new("k", SigningKey::from_bytes(&[1u8; 32]));
        RobinhoodClient::new(signer, mock)
    }

    #[tokio::test]
    async fn best_bid_ask_signs_and_carries_query() {
        let mock = MockTransport::new().json(
            "GET",
            "/best_bid_ask/",
            json!({"results":[{"symbol":"BTC-USD","price":"60000"}]}),
        );
        let c = client(mock);
        let v = c.best_bid_ask(&["BTC-USD"]).await.unwrap();
        assert_eq!(crate::types::extract_price(&v, "BTC-USD"), Some(60000.0));

        let last = c.transport().last().unwrap();
        assert_eq!(last.method, "GET");
        assert!(last.url.contains("/best_bid_ask/?symbol=BTC-USD"));
        assert!(last.headers.iter().any(|(k, _)| k == "x-signature"));
        assert!(last
            .headers
            .iter()
            .any(|(k, v)| k == "x-api-key" && v == "k"));
    }

    #[tokio::test]
    async fn place_order_posts_signed_json_body() {
        let mock =
            MockTransport::new().json("POST", "/orders/", json!({"id":"ord_1","state":"open"}));
        let c = client(mock);
        let v = c
            .place_order(&OrderRequest::market("BTC-USD", Side::Buy, 0.01))
            .await
            .unwrap();
        assert_eq!(v["id"], "ord_1");

        let last = c.transport().last().unwrap();
        assert_eq!(last.method, "POST");
        let body = last.body.unwrap();
        assert!(body.contains("\"symbol\":\"BTC-USD\""));
        assert!(body.contains("\"type\":\"market\""));
        assert!(body.contains("\"side\":\"buy\""));
        assert!(last
            .headers
            .iter()
            .any(|(k, v)| k == "Content-Type" && v == "application/json"));
    }

    #[tokio::test]
    async fn non_2xx_maps_to_http_error() {
        let mock = MockTransport::new().route("GET", "/accounts/", 401, "unauthorized");
        let c = client(mock);
        match c.account().await {
            Err(Error::Http { status, .. }) => assert_eq!(status, 401),
            other => panic!("expected http 401, got {other:?}"),
        }
    }
}
