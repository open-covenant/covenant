//! Wire types for the Vantara explorer feeds.
//!
//! Two read-only endpoints back this crate. `/explorer/jobs` returns
//! content-free provenance records — one per completed agent job, carrying
//! only hashes, identifiers, a signed receipt, and a settlement flag, never
//! inputs or outputs. `/explorer/payouts` returns the aggregate on-chain
//! settlement ledger.
//!
//! The jobs feed is self-describing: its [`SigningBlock`] carries the key,
//! encodings, and canonical template needed to verify every record's
//! `vantaraSignature` with nothing fetched out of band.
//!
//! Field names on the wire are camelCase; several are nullable, so those
//! decode as `Option`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct JobsPage {
    pub cluster: String,
    /// Hash function behind every `outputHash`. `sha256` today.
    pub hash_algorithm: String,
    /// Optional feed-level prose. Absent or an explicit `null` both decode
    /// to `None`, so a nulled note never breaks the page.
    #[serde(default)]
    pub note: Option<String>,
    /// How to verify each record's `vantaraSignature`. Absent on older feeds.
    #[serde(default)]
    pub signing: Option<SigningBlock>,
    pub pagination: Pagination,
    pub jobs: Vec<Job>,
}

/// Self-describing recipe for the record signatures. Everything the verifier
/// needs travels in-band: the field carrying the signature, the scheme, the
/// public key and its encoding, the signature encoding, and the canonical
/// string template the signature covers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SigningBlock {
    pub field: String,
    pub scheme: String,
    pub public_key: String,
    pub public_key_encoding: String,
    pub signature_encoding: String,
    pub canonical: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// The Machine Payments Protocol discovery document at `/.well-known/mpp`.
/// Only the provider callback is read — its `publicKey` is the out-of-band
/// trust anchor the feed's self-asserted signing key is checked against.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct MppDoc {
    #[serde(rename = "providerCallback")]
    pub provider_callback: ProviderCallback,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ProviderCallback {
    #[serde(rename = "publicKey")]
    pub public_key: String,
    #[serde(default)]
    pub scheme: Option<String>,
    #[serde(default)]
    pub encoding: Option<String>,
}

/// One completed job. Content-free by design: the output is represented only
/// by its hash, and no prompt, wallet, or payload is exposed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    /// Stable identifier, safe to join on. This is the anchor a Covenant
    /// attestation references.
    pub job_id: String,
    /// Node that ran the job. Null on jobs the network doesn't attribute
    /// to a specific node.
    #[serde(default)]
    pub node_id: Option<String>,
    pub model: String,
    /// SHA-256 (hex) of the job output. The content itself is never served.
    pub output_hash: String,
    /// Network-internal node-to-orchestrator MAC. Verified server-side and
    /// intentionally not third-party verifiable; treated here as opaque.
    #[serde(default)]
    pub proof_signature: Option<String>,
    /// Third-party-verifiable ed25519 receipt over the record's immutable
    /// fields. Verified against the feed's [`SigningBlock`].
    #[serde(default)]
    pub vantara_signature: Option<String>,
    pub completed_at: String,
    /// Content-free payment anchor: whether this job's node reward settled
    /// on-chain, and when. Excluded from the signed receipt because it
    /// mutates when the reward settles.
    #[serde(default)]
    pub settlement: Option<Settlement>,
}

/// Whether a job's node reward settled on-chain. No amount, signature, or
/// wallet — just the fact and the hour it happened.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settlement {
    pub settled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pagination {
    pub limit: u32,
    pub offset: u32,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PayoutsPage {
    pub cluster: String,
    pub pagination: Pagination,
    /// Lifetime settled totals keyed by asset (`USDC`).
    #[serde(default)]
    pub totals: BTreeMap<String, AssetTotal>,
    pub payouts: Vec<Payout>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AssetTotal {
    pub total: f64,
    pub count: u64,
}

/// One settled payment in the aggregate ledger. Payouts are per-wallet hourly
/// aggregates, not per-job, so a payout carries no `jobId`. Per-job settlement
/// lives on the job itself as [`Settlement`]; a wallet is never joined to a
/// job (that would deanonymize node operators).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Payout {
    pub id: String,
    pub wallet_address: String,
    pub asset: String,
    pub amount: f64,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub reason_label: Option<String>,
    /// Solana transaction signature — the on-chain anchor, verifiable on
    /// any mainnet explorer.
    pub tx_signature: String,
    pub created_at: String,
    #[serde(default)]
    pub period_start: Option<String>,
    #[serde(default)]
    pub period_end: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recorded from the live feed, 2026-07-02, with the signing block and a
    /// real `vantaraSignature`.
    const JOBS_SAMPLE: &str = r#"{
        "cluster": "mainnet-beta",
        "hashAlgorithm": "sha256",
        "note": "outputHash is the sha256 of the job output; content is never exposed.",
        "signing": {
            "field": "vantaraSignature",
            "scheme": "ed25519",
            "publicKey": "DBkSpBFu5oUNmPuJwB1J2gBLfRtVHmZHXaynaX3hAs71",
            "publicKeyEncoding": "base58",
            "signatureEncoding": "base64",
            "canonical": "vantara-job-receipt-v1|<jobId>|<nodeId or '-'>|<model>|<outputHash>|<completedAt ISO>"
        },
        "pagination": { "limit": 50, "offset": 0, "total": 2 },
        "jobs": [
            { "jobId": "8e71e03f-d994-4b09-8444-53039ca2839e", "nodeId": null, "model": "claude",
              "outputHash": "881f47e19aac2cdddbc30b1fa29284158ee680c1c2f84ddc73b5fa3606c9b3b5",
              "proofSignature": null,
              "vantaraSignature": "kdFJsw4O94g2EIIN4tN0ycMGAyeRdeP+Cwg6X1ls2gG8eKr+nOGkhnacABGZnyQrkFfYS+w2DSwD6k8NYkXaCw==",
              "completedAt": "2026-07-01T19:19:04.083Z",
              "settlement": { "settled": false, "settledAt": null } }
        ]
    }"#;

    const PAYOUTS_SAMPLE: &str = r#"{
        "cluster": "mainnet-beta",
        "pagination": { "limit": 3, "offset": 0, "total": 3982 },
        "totals": { "USDC": { "total": 367.9631949999996, "count": 3982 } },
        "payouts": [
            { "id": "fb7b3983-302a-454c-a780-39d1bcbb58bf",
              "walletAddress": "HoRpjJmY2hEFh1mcFBNHTraa2HuXJeTEyFy71T27dyGT",
              "asset": "USDC", "amount": 0.382444, "reason": "hourly-aggregate-capped",
              "reasonLabel": "hourly-aggregate-capped",
              "txSignature": "2B9MTUz9s9uWUFFuxD2PoyNRFX8jjDrCuAnkcrTk6Rp3XYBMRCGh2Tus2k8pqf8vMfqzitmP9HcqQRJWbCXkTfVJ",
              "createdAt": "2026-07-02T08:29:25.319Z",
              "periodStart": "2026-07-02T07:29:25.315Z", "periodEnd": "2026-07-02T08:29:25.315Z" }
        ]
    }"#;

    #[test]
    fn parses_jobs_feed_with_signing_block_and_settlement() {
        let page: JobsPage = serde_json::from_str(JOBS_SAMPLE).expect("decode jobs");
        assert_eq!(page.hash_algorithm, "sha256");
        let signing = page.signing.expect("signing block");
        assert_eq!(signing.field, "vantaraSignature");
        assert_eq!(signing.scheme, "ed25519");
        assert_eq!(signing.public_key_encoding, "base58");

        let job = &page.jobs[0];
        assert!(job.node_id.is_none());
        assert!(job.proof_signature.is_none());
        assert!(job.vantara_signature.as_deref().unwrap().ends_with("=="));
        let s = job.settlement.as_ref().expect("settlement");
        assert!(!s.settled);
        assert!(s.settled_at.is_none());
    }

    #[test]
    fn parses_payouts_feed_with_totals_and_signatures() {
        let page: PayoutsPage = serde_json::from_str(PAYOUTS_SAMPLE).expect("decode payouts");
        assert_eq!(page.pagination.total, 3982);
        assert_eq!(page.totals.get("USDC").unwrap().count, 3982);
        assert!(page.payouts[0].tx_signature.starts_with("2B9MTUz9"));
    }

    #[test]
    fn job_round_trips_through_serde() {
        let page: JobsPage = serde_json::from_str(JOBS_SAMPLE).unwrap();
        let encoded = serde_json::to_string(&page.jobs[0]).unwrap();
        let back: Job = serde_json::from_str(&encoded).unwrap();
        assert_eq!(page.jobs[0], back);
    }

    #[test]
    fn tolerates_null_note_and_absent_new_fields() {
        // An older/minimal record with no signing block, vantaraSignature, or
        // settlement, and a null note, must still decode.
        let raw = r#"{"cluster":"mainnet-beta","hashAlgorithm":"sha256","note":null,
            "pagination":{"limit":50,"offset":0,"total":1},
            "jobs":[{"jobId":"x","nodeId":null,"model":"claude",
                "outputHash":"881f47e19aac2cdddbc30b1fa29284158ee680c1c2f84ddc73b5fa3606c9b3b5",
                "proofSignature":null,"completedAt":"2026-07-01T19:19:04.083Z"}]}"#;
        let page: JobsPage = serde_json::from_str(raw).expect("minimal record must decode");
        assert!(page.note.is_none());
        assert!(page.signing.is_none());
        assert!(page.jobs[0].vantara_signature.is_none());
        assert!(page.jobs[0].settlement.is_none());
    }
}
