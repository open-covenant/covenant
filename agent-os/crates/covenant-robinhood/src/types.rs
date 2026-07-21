use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderType {
    Market,
    Limit,
    StopLimit,
    StopLoss,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeInForce {
    Gtc,
    Ioc,
    Fok,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketOrderConfig {
    pub asset_quantity: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LimitOrderConfig {
    pub asset_quantity: f64,
    pub limit_price: f64,
    pub time_in_force: TimeInForce,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderRequest {
    pub symbol: String,
    pub side: Side,
    #[serde(rename = "type")]
    pub order_type: OrderType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub market_order_config: Option<MarketOrderConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_order_config: Option<LimitOrderConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_order_id: Option<String>,
}

impl OrderRequest {
    pub fn market(symbol: impl Into<String>, side: Side, asset_quantity: f64) -> Self {
        Self {
            symbol: symbol.into(),
            side,
            order_type: OrderType::Market,
            market_order_config: Some(MarketOrderConfig { asset_quantity }),
            limit_order_config: None,
            client_order_id: None,
        }
    }

    pub fn limit(
        symbol: impl Into<String>,
        side: Side,
        asset_quantity: f64,
        limit_price: f64,
    ) -> Self {
        Self {
            symbol: symbol.into(),
            side,
            order_type: OrderType::Limit,
            market_order_config: None,
            limit_order_config: Some(LimitOrderConfig {
                asset_quantity,
                limit_price,
                time_in_force: TimeInForce::Gtc,
            }),
            client_order_id: None,
        }
    }

    pub fn quantity(&self) -> f64 {
        self.market_order_config
            .as_ref()
            .map(|c| c.asset_quantity)
            .or_else(|| self.limit_order_config.as_ref().map(|c| c.asset_quantity))
            .unwrap_or(0.0)
    }

    /// Notional in quote currency at a reference price. Policy caps compare
    /// against this.
    pub fn notional(&self, reference_price: f64) -> f64 {
        self.quantity() * reference_price
    }
}

/// Best-effort mid price from a `best_bid_ask` response, tolerant of shape
/// drift across API versions.
pub fn extract_price(best_bid_ask: &Value, symbol: &str) -> Option<f64> {
    let rows = best_bid_ask.get("results").and_then(|v| v.as_array())?;
    let row = rows
        .iter()
        .find(|r| r.get("symbol").and_then(|s| s.as_str()) == Some(symbol))
        .or_else(|| rows.first())?;
    for key in ["price", "mid_price"] {
        if let Some(p) = row.get(key).and_then(as_f64) {
            return Some(p);
        }
    }
    let bid = row.get("bid_inclusive_of_sell_spread").and_then(as_f64);
    let ask = row.get("ask_inclusive_of_buy_spread").and_then(as_f64);
    match (bid, ask) {
        (Some(b), Some(a)) => Some((b + a) / 2.0),
        (Some(b), None) => Some(b),
        (None, Some(a)) => Some(a),
        _ => None,
    }
}

fn as_f64(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}
