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
    /// the daemon's existing x402 accounting convention. A price beyond
    /// the u64 credit range saturates to `u64::MAX` rather than wrapping
    /// to a smaller cost — an unrepresentable price is unaffordable, not
    /// free — since the price is manifest-derived and only bounded to
    /// `u128::MAX`.
    pub fn credits(&self) -> u64 {
        u64::try_from(self.price_micro_usdc / 10_000).unwrap_or(u64::MAX)
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
/// that must settle exactly on-chain. A zero price is rejected: a paid
/// endpoint priced at `"0"` is malformed (a genuinely free endpoint
/// omits `x-payment-info` and is skipped by [`parse`]). A price large
/// enough to overflow `u128` when scaled is rejected too, not panicked.
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
    // The integer part is parsed straight from the untrusted manifest with no
    // upper bound, so scaling it can overflow u128. Reject that rather than
    // panic (overflow-checks is on for release too) the way the parser already
    // rejects every other malformed price.
    let micro = whole
        .checked_mul(10u128.pow(USDC_DECIMALS))
        .and_then(|scaled| scaled.checked_add(frac))
        .ok_or_else(|| HyreError::Manifest(format!("price overflows u128: {usd:?}")))?;
    if micro == 0 {
        // An explicit "0" is a malformed paid endpoint: it would register a
        // zero-credit tool and settle a zero-credit debit/receipt the audit
        // verifier treats as drift. Free endpoints omit x-payment-info and
        // never reach here.
        return Err(HyreError::Manifest(format!("price is zero: {usd:?}")));
    }
    Ok(micro)
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
    fn usd_to_micro_rejects_non_numeric_fraction() {
        // The sibling above covers the non-numeric whole part ("abc")
        // and the >6-decimals arm ("0.0000001"). The third arm — a
        // fractional part within six places but carrying a non-digit —
        // passes the decimals check and only fails at padded.parse()
        // (manifest.rs:235). A malformed catalog price must fail loudly
        // here, not coerce to a wrong atomic amount that settles on-chain.
        let err = usd_to_micro("1.2x").unwrap_err();
        assert!(
            matches!(&err, HyreError::Manifest(m) if m.contains("price fraction")),
            "a non-numeric fraction must be rejected with the fraction guard: {err:?}"
        );
    }

    #[test]
    fn usd_to_micro_rejects_zero_price() {
        // A genuinely free endpoint omits x-payment-info (parse() skips it), so
        // every spelling of an explicit zero price is a malformed paid endpoint
        // and must be rejected, not converted to a zero-credit tool.
        for z in ["0", "0.0", "0.000000"] {
            assert!(
                matches!(usd_to_micro(z), Err(HyreError::Manifest(m)) if m.contains("zero")),
                "zero price {z:?} must be rejected",
            );
        }
    }

    #[test]
    fn parse_rejects_zero_priced_endpoint() {
        // The manifest is fetched from an untrusted provider URL; a paid
        // endpoint advertised at "0" must fail the whole refresh closed rather
        // than register a zero-credit paid tool the daemon would settle for free.
        let json = doc(serde_json::json!({
            "/trenches/new-tokens": { "get": priced_op("0") },
        }));
        assert!(
            matches!(parse(&json), Err(HyreError::Manifest(m)) if m.contains("zero")),
            "a zero-priced paid endpoint must be rejected",
        );
    }

    #[test]
    fn usd_to_micro_rejects_overflowing_price() {
        // The integer part is an unbounded u128 parsed from an untrusted
        // manifest. u128::MAX parses cleanly but overflows when scaled to
        // atomic USDC; with overflow-checks on (release included) the bare
        // multiply would panic the refresh rather than convert, so it must
        // reject like every other malformed price.
        let huge = u128::MAX.to_string();
        assert!(
            matches!(usd_to_micro(&huge), Err(HyreError::Manifest(m)) if m.contains("overflow")),
            "a price that overflows u128 when scaled must be rejected",
        );
    }

    #[test]
    fn parse_rejects_overflowing_priced_endpoint() {
        // A remote provider can advertise any integer price string; one large
        // enough to overflow the atomic-USDC scaling must fail the refresh
        // closed rather than panic the parser mid-manifest.
        let json = doc(serde_json::json!({
            "/trenches/new-tokens": { "get": priced_op(&u128::MAX.to_string()) },
        }));
        assert!(
            matches!(parse(&json), Err(HyreError::Manifest(m)) if m.contains("overflow")),
            "an overflowing paid endpoint price must be rejected",
        );
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
    fn credits_saturate_above_u64_range() {
        // price_micro_usdc is manifest-derived and only bounded to u128::MAX,
        // so a price whose cent value exceeds u64::MAX must saturate to
        // u64::MAX (unaffordable) rather than wrap through `as u64` to a small
        // cost the daemon would settle as a cheap tool.
        let ep = Endpoint {
            path: "/x".into(),
            method: "GET".into(),
            operation_id: String::new(),
            summary: String::new(),
            description: String::new(),
            price_micro_usdc: u128::MAX,
            params: vec![],
            body: vec![],
        };
        assert_eq!(ep.credits(), u64::MAX);
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

    #[test]
    fn parse_rejects_structurally_invalid_manifests() {
        // parse distils Hyre's OpenAPI into the paid-endpoint list. Each
        // structural failure must surface a HyreError::Manifest, never an empty
        // endpoint list: a manifest the gateway cannot read is a discovery dead
        // end, and silently treating it as "zero tools" would mask a Hyre
        // wire-format change or a fetch that returned garbage.
        assert!(
            matches!(parse("{not json"), Err(HyreError::Manifest(m)) if m.contains("decode openapi")),
            "a non-JSON manifest must be rejected as a decode-openapi Manifest error",
        );
        assert!(
            matches!(parse(r#"{"openapi":"3.1.0"}"#), Err(HyreError::Manifest(m)) if m.contains("no paths object")),
            "a document with no paths object must be rejected, not read as zero endpoints",
        );
        // Every path here filters out — EVM mirrors and an unpriced /agents
        // route — so the endpoint list collapses to nothing. parse must reject
        // rather than return an empty catalog that offers no tools.
        let all_filtered = doc(serde_json::json!({
            "/base/trenches/new-tokens": { "get": priced_op("0.080000") },
            "/skale/trenches/new-tokens": { "get": priced_op("0.080000") },
            "/agents/register": { "post": { "operationId": "reg", "summary": "", "description": "" } },
        }));
        assert!(
            matches!(parse(&all_filtered), Err(HyreError::Manifest(m)) if m.contains("no priced endpoints")),
            "a manifest that filters down to nothing must be rejected as no-priced-endpoints",
        );
    }

    #[test]
    fn parse_params_drops_non_path_query_and_malformed_entries() {
        // The tool layer models only path and query arguments. A header or
        // cookie param (e.g. an auth header) must never reach a generated
        // tool's schema, and a param object missing `name` or `in` must be
        // skipped — one malformed operation shouldn't abort the parse and
        // erase every tool Hyre publishes.
        let raw = serde_json::json!([
            { "name": "mint", "in": "path", "required": true, "description": "Token mint" },
            { "name": "X-Trace", "in": "header", "required": true },
            { "name": "session", "in": "cookie" },
            { "in": "query", "description": "no name" },
            { "name": "noLocation", "description": "no in" },
        ]);
        let params = parse_params(Some(&raw));
        assert_eq!(params.len(), 1, "only the path param survives the filter");
        assert_eq!(params[0].name, "mint");
        assert_eq!(params[0].location, ParamIn::Path);
    }

    #[test]
    fn parse_params_defaults_required_false_and_empty_description() {
        // `required` and `description` are optional in the manifest; a param
        // that omits them must distill to an optional arg with no help text,
        // not panic or inherit a stale value.
        let raw = serde_json::json!([{ "name": "curve_key", "in": "query" }]);
        let params = parse_params(Some(&raw));
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].location, ParamIn::Query);
        assert!(!params[0].required);
        assert_eq!(params[0].description, "");
    }

    #[test]
    fn parse_body_without_json_schema_is_empty() {
        // A body advertised only as multipart/form-data carries no schema we
        // model, so it must distill to zero body fields rather than a tool
        // argument the agent can't satisfy.
        let raw = serde_json::json!({
            "required": true,
            "content": { "multipart/form-data": { "schema": { "type": "object" } } }
        });
        assert!(parse_body(Some(&raw)).is_empty());
        assert!(parse_body(None).is_empty());
    }

    #[test]
    fn parse_body_without_properties_is_empty() {
        let raw = serde_json::json!({
            "content": { "application/json": { "schema": { "type": "object" } } }
        });
        assert!(
            parse_body(Some(&raw)).is_empty(),
            "a schema with no properties object yields no body fields"
        );
    }

    #[test]
    fn parse_body_absent_required_array_marks_all_optional() {
        // With no `required` array every body field must default to optional;
        // marking them required would make the generated tool reject calls
        // Hyre itself accepts. A field without a description gets an empty one.
        let raw = serde_json::json!({
            "content": { "application/json": { "schema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "NL question" },
                    "limit": { "type": "integer" }
                }
            }}}
        });
        let body = parse_body(Some(&raw));
        assert_eq!(body.len(), 2);
        assert!(
            body.iter().all(|f| !f.required),
            "no required array -> every field optional"
        );
        let query = body.iter().find(|f| f.name == "query").unwrap();
        assert_eq!(query.description, "NL question");
        let limit = body.iter().find(|f| f.name == "limit").unwrap();
        assert_eq!(
            limit.description, "",
            "a field without a description gets an empty one"
        );
    }
}
