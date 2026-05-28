//! Wire-format types for the x402 protocol.
//!
//! These mirror the JSON shape upstreams return in a 402 challenge.
//! Field names match the wire ones via `#[serde(rename = …)]` so
//! downstream consumers can round-trip the same object back through
//! serde without translation.

use serde::{Deserialize, Serialize};

/// One payment option the upstream will accept.
///
/// Servers return an array of these in a 402 challenge body. Treat
/// the array as "list of acceptable payment options" — any one
/// suffices, not all of them. The client picks the first option
/// whose `network` + `asset` match the supplied capability and whose
/// `amount` is within the per-call cap.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PaymentRequirements {
    /// CAIP-2 chain identifier, e.g.
    /// `"solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp"` for Solana mainnet.
    pub network: String,
    /// Token mint (Solana) or contract address (EVM) the payment
    /// must be denominated in. USDC is the common default.
    pub asset: String,
    /// Atomic amount as a decimal string. Stringified to preserve
    /// precision across languages that lack u64.
    pub amount: String,
    /// Convenience field: the same amount expressed as USDC for
    /// display and quick pre-flight comparison. Not authoritative —
    /// `amount` + `asset` are.
    #[serde(rename = "amountUsdc")]
    pub amount_usdc: f64,
    /// Destination address for the payment.
    #[serde(rename = "payTo")]
    pub pay_to: String,
    /// x402 scheme. `"exact"` means the agent must pay this amount
    /// exactly; forward-defined schemes (e.g. `"upto"`) are opaque
    /// strings until the client grows explicit support.
    pub scheme: String,
    /// Provider-specific extension fields off the 402 option's `extra`
    /// block. Currently used to carry PayAI's sponsored-gas `feePayer`
    /// so the signer can set the v0 message's payer slot. Optional and
    /// `skip_serializing_if = "Option::is_none"` so the xona payload
    /// shape on the wire stays byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<PaymentExtra>,
}

/// Provider-specific extension fields lifted off the 402 option's
/// `extra` block. Only the fields the signer needs are typed; anything
/// else upstream sends is dropped silently.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PaymentExtra {
    /// Sponsor's fee-payer pubkey for sponsored-gas Solana flows
    /// (currently PayAI). When set, the signer builds a v0
    /// VersionedTransaction whose payer slot is this pubkey and
    /// partial-signs as the funder only.
    #[serde(rename = "feePayer", default, skip_serializing_if = "Option::is_none")]
    pub fee_payer: Option<String>,
}

/// A daemon-issued authorization to call paid endpoints.
///
/// The daemon constructs one of these per (agent, provider) tuple
/// and hands it to [`super::Client::request_paid`]. This struct only
/// carries the per-call dimensions the HTTP layer needs to match a
/// payment requirement. Per-day caps, total budgets, TTL, and
/// endpoint allowlists live in the daemon's budget and permissions
/// subsystems and are enforced before this struct is constructed.
#[derive(Debug, Clone)]
pub struct Capability {
    /// Free-form provider tag (e.g. `"xona"`). Audit only — not
    /// enforced at the HTTP layer.
    pub provider: String,
    /// CAIP-2 network the daemon can settle on. The client picks
    /// a payment requirement whose `network` matches.
    pub network: String,
    /// Asset the daemon can pay in (mint or contract address).
    pub asset: String,
    /// Maximum atomic amount allowed for a single call. Any
    /// requirement exceeding this is rejected before the signer is
    /// invoked.
    pub per_call_cap: u128,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact JSON shape the upstream registry serves, captured
    /// from the Xona payload during integration design. If this
    /// round-trip ever breaks, upstream changed the wire format.
    const XONA_SAMPLE: &str = r#"[
        {
            "network": "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
            "asset": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            "amount": "80000",
            "amountUsdc": 0.08,
            "payTo": "9VaDVp1Wb78G4Wm6VuTiMrpESjrUymXefQTHcJGRSTEA",
            "scheme": "exact"
        }
    ]"#;

    #[test]
    fn decodes_upstream_sample() {
        let parsed: Vec<PaymentRequirements> =
            serde_json::from_str(XONA_SAMPLE).expect("decode");
        assert_eq!(parsed.len(), 1);
        let r = &parsed[0];
        assert_eq!(
            r.network,
            "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp"
        );
        assert_eq!(r.amount, "80000");
        assert_eq!(r.amount_usdc, 0.08);
        assert_eq!(r.scheme, "exact");
    }

    #[test]
    fn round_trips_through_serde() {
        let parsed: Vec<PaymentRequirements> =
            serde_json::from_str(XONA_SAMPLE).expect("decode");
        let re_encoded = serde_json::to_string(&parsed).expect("encode");
        let re_parsed: Vec<PaymentRequirements> =
            serde_json::from_str(&re_encoded).expect("re-decode");
        assert_eq!(parsed, re_parsed);
    }
}
