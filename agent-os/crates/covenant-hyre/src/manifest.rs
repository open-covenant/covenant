//! Parse Hyre's published OpenAPI into a flat list of paid endpoints.
//!
//! Hyre serves a standard OpenAPI 3.1 document. The Covenant-relevant
//! signal lives in the `x-payment-info` extension on each operation —
//! a fixed USD price plus the x402 protocol descriptor. We read only
//! the fields the gateway and the tool layer need and ignore the rest,
//! so an OpenAPI shape we don't model never breaks the parse.
//!
//! Only the Solana root paths are kept. The same endpoints are mirrored
//! under `/base/*` and `/skale/*` for the EVM rails; v1 settles on
//! Solana through PayAI, so the mirrors are dropped here rather than
//! surfaced as duplicate tools. The unpaid `/agents/*` ME-protocol
//! routes carry no `x-payment-info` and are skipped.

use serde_json::Value;

use crate::{HyreError, Result};

/// USDC has six decimals; one USD is 1_000_000 atomic units.
const USDC_DECIMALS: u32 = 6;

/// Where a request argument binds on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamIn {
    Path,
    Query,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub location: ParamIn,
    pub required: bool,
    pub description: String,
}

/// A field in a JSON request body (e.g. `/ask` takes `{ "query": … }`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyField {
    pub name: String,
    pub required: bool,
    pub description: String,
}

/// One paid Hyre endpoint, distilled from the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// Path template as published, e.g. `/trenches/token/{mint}/snipers`.
    pub path: String,
    /// Upper-case HTTP verb.
    pub method: String,
    pub operation_id: String,
    pub summary: String,
    pub description: String,
    /// Price in atomic USDC (six decimals). `80_000` == $0.08.
    pub price_micro_usdc: u128,
    pub params: Vec<Param>,
    pub body: Vec<BodyField>,
}

impl Endpoint {
    /// Slug used as the catalog key — the path without its leading
    /// slash, braces intact (`trenches/token/{mint}/snipers`).
    pub fn slug(&self) -> String {
        self.path.trim_start_matches('/').to_string()
    }

    /// Stable MCP tool name: `hyre.` plus the path with separators
    /// flattened to dots and path-parameter braces stripped, so
    /// `/trenches/token/{mint}/snipers` becomes
    /// `hyre.trenches.token.mint.snipers`.
    pub fn tool_name(&self) -> String {
        let body = self
            .path
            .trim_start_matches('/')
            .replace(['{', '}'], "")
            .replace('/', ".");
        format!("hyre.{body}")
    }

    /// USD-pegged budget credits (cents). $0.08 → 8 credits, matching
    /// the daemon's existing x402 accounting convention.
    pub fn credits(&self) -> u64 {
        (self.price_micro_usdc / 10_000) as u64
    }
}

/// Parse the manifest JSON into the Solana-root paid endpoints.
pub fn parse(manifest_json: &str) -> Result<Vec<Endpoint>> {
    let doc: Value = serde_json::from_str(manifest_json)
        .map_err(|e| HyreError::Manifest(format!("decode openapi: {e}")))?;

    let paths = doc
        .get("paths")
        .and_then(Value::as_object)
        .ok_or_else(|| HyreError::Manifest("openapi has no paths object".into()))?;

    let mut out = Vec::new();
    for (path, item) in paths {
        if !is_solana_root(path) {
            continue;
        }
        let Some(item) = item.as_object() else {
            continue;
        };
        let shared_params = parse_params(item.get("parameters"));
        for method in ["get", "post"] {
            let Some(op) = item.get(method).and_then(Value::as_object) else {
                continue;
            };
            let Some(price) = op
                .get("x-payment-info")
                .and_then(|p| p.get("price"))
                .and_then(|p| p.get("amount"))
                .and_then(Value::as_str)
            else {
                continue; // unpriced (e.g. /agents/*): not a paid endpoint
            };

            let mut params = shared_params.clone();
            params.extend(parse_params(op.get("parameters")));
            params.retain(|p| p.name != "Authorization"); // MPP credential, daemon-supplied

            out.push(Endpoint {
                path: path.clone(),
                method: method.to_uppercase(),
                operation_id: str_field(op, "operationId"),
                summary: str_field(op, "summary"),
                description: str_field(op, "description"),
                price_micro_usdc: usd_to_micro(price)?,
                params,
                body: parse_body(op.get("requestBody")),
            });
        }
    }

    if out.is_empty() {
        return Err(HyreError::Manifest("no priced endpoints found".into()));
    }
    out.sort_by_key(|e| e.tool_name());
    Ok(out)
}

fn is_solana_root(path: &str) -> bool {
    !path.starts_with("/base/") && !path.starts_with("/skale/")
}

fn str_field(obj: &serde_json::Map<String, Value>, key: &str) -> String {
    obj.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn parse_params(raw: Option<&Value>) -> Vec<Param> {
    let Some(arr) = raw.and_then(Value::as_array) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|p| {
            let name = p.get("name")?.as_str()?.to_string();
            let location = match p.get("in").and_then(Value::as_str)? {
                "path" => ParamIn::Path,
                "query" => ParamIn::Query,
                _ => return None, // header/cookie params aren't agent-facing
            };
            Some(Param {
                name,
                location,
                required: p.get("required").and_then(Value::as_bool).unwrap_or(false),
                description: p
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect()
}

fn parse_body(raw: Option<&Value>) -> Vec<BodyField> {
    let Some(schema) = raw
        .and_then(|b| b.get("content"))
        .and_then(|c| c.get("application/json"))
        .and_then(|j| j.get("schema"))
    else {
        return Vec::new();
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    props
        .iter()
        .map(|(name, spec)| BodyField {
            name: name.clone(),
            required: required.contains(&name.as_str()),
            description: spec
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        })
        .collect()
}

/// Convert a fixed USD decimal string (e.g. `"0.080000"`) to atomic
/// USDC. Parses the decimal directly to avoid float rounding on prices
/// that must settle exactly on-chain.
fn usd_to_micro(usd: &str) -> Result<u128> {
    let (whole, frac) = usd.split_once('.').unwrap_or((usd, ""));
    let whole: u128 = whole
        .parse()
        .map_err(|_| HyreError::Manifest(format!("price integer part: {usd:?}")))?;
    if frac.len() > USDC_DECIMALS as usize {
        // Hyre publishes exactly six places; more would silently lose
        // precision on conversion, so reject rather than truncate.
        return Err(HyreError::Manifest(format!(
            "price has more than {USDC_DECIMALS} decimals: {usd:?}"
        )));
    }
    let mut padded = frac.to_string();
    while padded.len() < USDC_DECIMALS as usize {
        padded.push('0');
    }
    let frac: u128 = if padded.is_empty() {
        0
    } else {
        padded
            .parse()
            .map_err(|_| HyreError::Manifest(format!("price fraction: {usd:?}")))?
    };
    Ok(whole * 10u128.pow(USDC_DECIMALS) + frac)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(paths: Value) -> String {
        serde_json::json!({ "openapi": "3.1.0", "paths": paths }).to_string()
    }

    fn priced_op(amount: &str) -> Value {
        serde_json::json!({
            "operationId": "op",
            "summary": "s",
            "description": "d",
            "x-payment-info": {
                "price": { "mode": "fixed", "currency": "USD", "amount": amount },
                "protocols": [{ "x402": { "network": "solana", "facilitator": "https://facilitator.payai.network" } }]
            }
        })
    }

    #[test]
    fn usd_to_micro_handles_six_decimals() {
        assert_eq!(usd_to_micro("0.080000").unwrap(), 80_000);
        assert_eq!(usd_to_micro("0.010000").unwrap(), 10_000);
        assert_eq!(usd_to_micro("0.250000").unwrap(), 250_000);
        assert_eq!(usd_to_micro("1").unwrap(), 1_000_000);
        assert_eq!(usd_to_micro("2.5").unwrap(), 2_500_000);
    }

    #[test]
    fn usd_to_micro_rejects_excess_precision() {
        assert!(usd_to_micro("0.0000001").is_err());
        assert!(usd_to_micro("abc").is_err());
    }

    #[test]
    fn credits_are_cents() {
        let ep = Endpoint {
            path: "/x".into(),
            method: "GET".into(),
            operation_id: String::new(),
            summary: String::new(),
            description: String::new(),
            price_micro_usdc: 80_000,
            params: vec![],
            body: vec![],
        };
        assert_eq!(ep.credits(), 8);
    }

    #[test]
    fn tool_name_flattens_path_and_strips_braces() {
        let ep = Endpoint {
            path: "/trenches/token/{mint}/snipers".into(),
            method: "GET".into(),
            operation_id: String::new(),
            summary: String::new(),
            description: String::new(),
            price_micro_usdc: 0,
            params: vec![],
            body: vec![],
        };
        assert_eq!(ep.tool_name(), "hyre.trenches.token.mint.snipers");
        assert_eq!(ep.slug(), "trenches/token/{mint}/snipers");
    }

    #[test]
    fn parse_keeps_solana_root_drops_mirrors_and_unpriced() {
        let json = doc(serde_json::json!({
            "/trenches/new-tokens": { "get": priced_op("0.080000") },
            "/base/trenches/new-tokens": { "get": priced_op("0.080000") },
            "/skale/trenches/new-tokens": { "get": priced_op("0.080000") },
            "/agents/register": { "post": { "operationId": "reg", "summary": "", "description": "" } },
        }));
        let eps = parse(&json).unwrap();
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].path, "/trenches/new-tokens");
        assert_eq!(eps[0].price_micro_usdc, 80_000);
    }

    #[test]
    fn parse_collects_path_query_params_and_drops_authorization() {
        let mut op = priced_op("0.010000");
        op["parameters"] = serde_json::json!([
            { "name": "mint", "in": "path", "required": true, "description": "Token mint" },
            { "name": "curve_key", "in": "query", "required": false, "description": "Curve" },
            { "name": "Authorization", "in": "query", "required": false, "description": "MPP credential" },
        ]);
        let json = doc(serde_json::json!({ "/trenches/curve/{mint}": { "get": op } }));
        let eps = parse(&json).unwrap();
        let p = &eps[0].params;
        assert_eq!(p.len(), 2, "Authorization must be dropped");
        assert_eq!(p[0].name, "mint");
        assert_eq!(p[0].location, ParamIn::Path);
        assert!(p[0].required);
        assert_eq!(p[1].name, "curve_key");
        assert_eq!(p[1].location, ParamIn::Query);
    }

    #[test]
    fn parse_reads_request_body_fields() {
        let mut op = priced_op("0.250000");
        op["requestBody"] = serde_json::json!({
            "required": true,
            "content": { "application/json": { "schema": {
                "type": "object",
                "required": ["query"],
                "properties": { "query": { "type": "string", "description": "NL question" } }
            }}}
        });
        let json = doc(serde_json::json!({ "/ask": { "post": op } }));
        let eps = parse(&json).unwrap();
        assert_eq!(eps[0].method, "POST");
        assert_eq!(eps[0].body.len(), 1);
        assert_eq!(eps[0].body[0].name, "query");
        assert!(eps[0].body[0].required);
    }

    #[test]
    fn parse_real_vendored_manifest() {
        let eps = parse(crate::VENDORED_MANIFEST).expect("vendored manifest parses");
        // 23 priced GETs + /ask on the Solana root.
        assert_eq!(eps.len(), 24, "got {} endpoints", eps.len());
        assert!(eps.iter().any(|e| e.tool_name() == "hyre.ask"));
        assert!(eps.iter().all(|e| e.price_micro_usdc > 0));
        // Names are unique — the dotted scheme must not collide.
        let mut names: Vec<_> = eps.iter().map(|e| e.tool_name()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), eps.len(), "tool names must be unique");
    }
}
