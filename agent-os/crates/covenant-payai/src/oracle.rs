//! On-demand reputation oracle: fetch recent PayAI settlements, compute a
//! wallet's settlement-grounded reputation, and sign it. This is what the
//! daemon's MCP tool / HTTP read call. It computes over the recent settlement
//! window on demand; a background indexer with a persistent store (full
//! per-wallet history, cached) is the production follow-up.

use covenant_identity::LocalIdentity;

use crate::index::SettlementIndexer;
use crate::reputation::{compute_reputation, sign_reputation};
use crate::sign::SignedEnvelope;
use crate::types::PayaiReputation;
use crate::Result;

/// Fetch the latest `limit` settlements from `rpc_url` (watching `fee_payer`,
/// or the PayAI default fee-payer when `None`), compute `wallet`'s reputation,
/// and sign it. Returns the plain reputation plus the signed credential.
pub async fn signed_reputation_for(
    rpc_url: &str,
    fee_payer: Option<&str>,
    wallet: &str,
    limit: usize,
    identity: &LocalIdentity,
) -> Result<(PayaiReputation, SignedEnvelope)> {
    let mut ix = SettlementIndexer::new(rpc_url);
    if let Some(fp) = fee_payer {
        ix = ix.with_fee_payer(fp);
    }
    let settlements = ix.recent_settlements(limit).await?;
    let rep = compute_reputation(&settlements, wallet, ix.fee_payer());
    let signed = sign_reputation(identity, &rep)?;
    Ok((rep, signed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::USDC_MAINNET_MINT;
    use serde_json::json;
    use wiremock::matchers::{body_string_contains, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn computes_and_signs_reputation_over_fetched_settlements() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("getSignaturesForAddress"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0", "id": 1,
                "result": [{"signature": "Sig1", "slot": 1, "blockTime": 1i64, "err": null}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("getTransaction"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0", "id": 1,
                "result": {
                    "slot": 1, "blockTime": 1i64,
                    "meta": {"err": null, "postTokenBalances": [
                        {"accountIndex": 1, "mint": USDC_MAINNET_MINT, "owner": "Seller"}
                    ]},
                    "transaction": {"message": {
                        "accountKeys": [{"pubkey": "FeePayer"}, {"pubkey": "SellerAta"}],
                        "instructions": [{
                            "program": "spl-token",
                            "parsed": {"type": "transferChecked", "info": {
                                "source": "BuyerAta", "destination": "SellerAta",
                                "mint": USDC_MAINNET_MINT, "authority": "Buyer",
                                "tokenAmount": {"amount": "2500000", "decimals": 6}
                            }}
                        }]
                    }}
                }
            })))
            .mount(&server)
            .await;

        let id = LocalIdentity::generate("daemon@local");
        let (rep, signed) = signed_reputation_for(&server.uri(), None, "Seller", 10, &id)
            .await
            .unwrap();
        assert_eq!(rep.settled_jobs, 1);
        assert_eq!(rep.volume_micro_usdc, 2_500_000);
        signed.verify().unwrap();
        assert_eq!(
            signed.deserialize::<PayaiReputation>().unwrap(),
            rep,
            "signed payload round-trips to the same reputation"
        );
    }
}
