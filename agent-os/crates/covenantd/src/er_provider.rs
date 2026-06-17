//! Local registry of ER-settled x402 providers, exposed to agents as tools.
//!
//! An operator registers a provider (an x402 endpoint that settles in a MagicBlock
//! ephemeral rollup) once, via config. Each registered provider appears in the agent
//! tool list as `er.<slug>` and, when called, routes through the daemon's x402 pay
//! path. Because the provider's `network` is `solana-er:*`,
//! [`crate::x402::X402Config::signer_for`] selects the ER signer sidecar, so the call
//! settles by metering credits in the rollup. The capability (`tool.call.er.<slug>`),
//! budget debit, settlement receipt, and audit event are the same ones every other
//! tool and paid call use.

use covenant_mcp::ToolSpec;
use serde::Deserialize;
use serde_json::json;

/// A registered ER-settled provider. `network` should be a `solana-er:*` CAIP-2 id
/// so the daemon routes the call to the ER signer sidecar. Deserializes from the
/// operator's registry config; only `slug`, `endpoint`, and `per_call_cap` are
/// required (the rest default to a GET call on `solana-er:devnet` in `credits`).
#[derive(Debug, Clone, Deserialize)]
pub struct ErProvider {
    pub slug: String,
    pub endpoint: String,
    /// HTTP method for the call (usually GET).
    #[serde(default = "default_method")]
    pub method: String,
    #[serde(default = "default_network")]
    pub network: String,
    #[serde(default = "default_asset")]
    pub asset: String,
    /// Atomic per-call price cap (credits), the amount advertised to the signer.
    pub per_call_cap: u128,
    /// USD-pegged budget cost debited from the caller per call.
    #[serde(default = "default_credits")]
    pub credits: u64,
    #[serde(default)]
    pub description: String,
}

fn default_method() -> String {
    "GET".into()
}
fn default_network() -> String {
    "solana-er:devnet".into()
}
fn default_asset() -> String {
    "credits".into()
}
fn default_credits() -> u64 {
    1
}

impl ErProvider {
    pub fn tool_name(&self) -> String {
        format!("er.{}", self.slug)
    }
}

/// Tool specs for the registered ER providers: one `er.<slug>` per provider.
pub fn er_specs(providers: &[ErProvider]) -> Vec<ToolSpec> {
    providers
        .iter()
        .map(|p| ToolSpec {
            name: p.tool_name(),
            description: format!(
                "{} Settled by gasless credit metering in a MagicBlock ephemeral rollup \
                 ({} {} per call).",
                p.description, p.per_call_cap, p.asset
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "body": {
                        "type": "object",
                        "description": "Optional JSON body forwarded to the endpoint."
                    }
                }
            }),
        })
        .collect()
}

/// Resolve an `er.<slug>` tool name to its registered provider.
pub fn find<'a>(providers: &'a [ErProvider], tool_name: &str) -> Option<&'a ErProvider> {
    let slug = tool_name.strip_prefix("er.")?;
    providers.iter().find(|p| p.slug == slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<ErProvider> {
        vec![ErProvider {
            slug: "quote".into(),
            endpoint: "http://127.0.0.1:8402/paid".into(),
            method: "GET".into(),
            network: "solana-er:devnet".into(),
            asset: "credits".into(),
            per_call_cap: 100,
            credits: 1,
            description: "A market quote.".into(),
        }]
    }

    #[test]
    fn specs_name_tools_by_slug() {
        let specs = er_specs(&sample());
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "er.quote");
        assert!(specs[0].description.contains("ephemeral rollup"));
    }

    #[test]
    fn find_resolves_prefixed_name_and_rejects_unknown() {
        let p = sample();
        assert_eq!(find(&p, "er.quote").unwrap().slug, "quote");
        assert!(find(&p, "er.missing").is_none());
        assert!(find(&p, "quote").is_none()); // must carry the er. prefix
    }
}
