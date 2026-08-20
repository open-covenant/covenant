//! Phase 1: credit-backed x402 draws. BUILT BUT OFF.
//!
//! The composition we want eventually: a Covenant agent hits a 402,
//! is short on USDC, draws a Krexa credit line to cover the shortfall,
//! pays, and repays from earnings via Krexa's Revenue Router. This
//! module implements the draw against Krexa's documented oracle flow
//! (`POST /solana/oracle/sign-credit` returns a partially-signed tx the
//! agent co-signs and submits), but every entry point is gated on
//! [`crate::KrexaConfig::credit_enabled`], which defaults to `false`.
//!
//! It is deliberately NOT wired into `covenant-x402`'s live settlement
//! path. Enabling it is blocked on three answers from Krexa: an audit,
//! real vault TVL with a sane default/insurance ratio, and the custody
//! model (does the agent run a Krexa PDA wallet and route earnings
//! through the Revenue Router, or can credit settle to our own signer?).
//! Until then this exists only so the seam is ready and tested.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::KrexaClient;
use crate::{KrexaError, Result};

/// A draw request to the Krexa oracle. Amounts are USDC lamports as
/// decimal strings, matching the documented API.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DrawRequest {
    /// The agent the credit line belongs to.
    pub agent_pubkey: String,
    /// Who signs for and receives the draw. Equal to `agent_pubkey` when the
    /// agent borrows on its own key; the owner or PDA when the agent runs a
    /// Krexa-custodied wallet. Which one applies is the custody question
    /// that gates this whole module.
    pub agent_or_owner_pubkey: String,
    /// USDC lamports as a decimal string, e.g. "500000000".
    pub amount: String,
    pub rate_bps: u32,
    pub credit_level: u8,
    /// USDC lamports of posted collateral, "0" for uncollateralized.
    pub collateral_value_usdc: String,
}

/// The oracle's response: a base64 partially-signed transaction the
/// agent co-signs and submits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignedDraw {
    pub transaction: String,
    #[serde(flatten, default)]
    pub extra: Value,
}

impl KrexaClient {
    /// Ask the Krexa oracle to co-sign a borrow. GATED: returns
    /// [`KrexaError::CreditDisabled`] unless `credit_enabled` is true.
    /// Even when enabled, the caller still has to co-sign and submit the
    /// returned transaction with the agent's own key; this only fetches
    /// the oracle half.
    pub async fn oracle_sign_credit(
        &self,
        credit_enabled: bool,
        req: &DrawRequest,
    ) -> Result<SignedDraw> {
        if !credit_enabled {
            return Err(KrexaError::CreditDisabled);
        }
        let url = format!("{}/solana/oracle/sign-credit", self.base_url());
        let resp = self.http_post_json(&url, req).await?;
        serde_json::from_value(resp).map_err(|e| KrexaError::Decode(format!("sign-credit: {e}")))
    }
}

/// Decide whether a 402 shortfall should be covered by a Krexa draw, and
/// for how much. Pure policy, no network.
///
/// The all-or-nothing rule is deliberate. An x402 payment is atomic, so a
/// draw that only partially covers the gap can't complete the call; it
/// would just accrue interest for nothing. Credit that can't cover the
/// whole shortfall is refused rather than partially drawn.
///
/// All three amounts are atomic USDC. Take the ceiling from
/// [`crate::client::CreditEligibility::available_credit_atomic`], not the
/// raw whole-USDC float, so dollars never get compared against atoms.
///
/// This is only the floor of a draw policy: health-factor and
/// minimum-economical-draw guards belong at the live-line call site, not
/// here. Nothing live calls this yet; it is the seam Phase 1 plugs into.
pub fn shortfall_draw_lamports(
    owed_lamports: u128,
    available_lamports: u128,
    available_credit_lamports: u128,
) -> Option<u128> {
    if available_lamports >= owed_lamports {
        return None;
    }
    let shortfall = owed_lamports - available_lamports;
    if available_credit_lamports < shortfall {
        return None;
    }
    Some(shortfall)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::KrexaConfig;

    #[test]
    fn credit_is_off_by_default() {
        assert!(!KrexaConfig::default().credit_enabled);
    }

    #[test]
    fn shortfall_is_all_or_nothing() {
        // Enough USDC on hand: no draw, at or above the amount owed.
        assert_eq!(shortfall_draw_lamports(100, 100, 1_000), None);
        assert_eq!(shortfall_draw_lamports(100, 120, 1_000), None);
        // Short by 60 with credit to spare: draw exactly the gap.
        assert_eq!(shortfall_draw_lamports(100, 40, 1_000), Some(60));
        // Credit covers the gap exactly: still draws.
        assert_eq!(shortfall_draw_lamports(100, 40, 60), Some(60));
        // Credit one lamport short: refused, never partially drawn.
        assert_eq!(shortfall_draw_lamports(100, 40, 59), None);
    }

    #[tokio::test]
    async fn oracle_sign_credit_is_gated_off() {
        let c = KrexaClient::new("http://127.0.0.1:1"); // never reached
        let req = DrawRequest {
            agent_pubkey: "AG".into(),
            agent_or_owner_pubkey: "AG".into(),
            amount: "500000000".into(),
            rate_bps: 3650,
            credit_level: 1,
            collateral_value_usdc: "0".into(),
        };
        let err = c.oracle_sign_credit(false, &req).await.unwrap_err();
        assert!(matches!(err, KrexaError::CreditDisabled));
    }
}
