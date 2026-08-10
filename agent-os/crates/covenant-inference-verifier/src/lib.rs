//! The ER-verifier quote-fetch seam.
//!
//! `covenant-tee` (the offline binding crate) turns an agent, a subject
//! commitment, and a fresh nonce into the 64 bytes an enclave should echo as
//! `report_data`. `covenantd::tee` holds the trust policy over which enclave
//! images it accepts. Neither fetches a quote or runs DCAP; that is the one
//! piece this crate owns.
//!
//! Given the enclave's TEE URL and the base64 challenge the binding crate
//! produced, [`verify_quote`] does exactly three things and nothing else:
//!
//! 1. asks the enclave for a TDX quote over that challenge
//!    (`GET /quote?challenge=<base64>`, the call MagicBlock already serves),
//! 2. DCAP-verifies the quote with the Phala QVL against the Intel PCCS, and
//! 3. reads back the fields `covenantd::tee::DcapResult` consumes — the
//!    validator identity, the TCB status, the `mr_td` measurement, and the
//!    `report_data` the enclave echoed.
//!
//! That last field is why this returns `report_data`: the binding crate needs
//! the echoed 64 bytes to confirm the quote is about *its* subject. We also
//! reject up front any quote whose `report_data` is not the challenge we asked
//! for, because a quote answering a different challenge is stale or swapped and
//! has nothing to say about this request.
//!
//! Deliberately absent: constructing the challenge (the binding crate's job)
//! and deciding which `mr_td` is trustworthy (the daemon's policy). This crate
//! is the seam between `covenantd::tee.challenge()` and `covenantd::tee.verify()`
//! and owns only fetch + DCAP + extract.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::{Deserialize, Serialize};

/// Phala's public PCCS. DCAP verification needs the platform's collateral (TCB
/// info, QE identity, CRLs); the QVL fetches it from here keyed by the quote's
/// FMSPC. Same endpoint the magicblock-er verifier proved against.
const PCCS: &str = "https://pccs.phala.network";

/// The DCAP fields the ER verifier returns, in the exact shape
/// `covenantd::tee::DcapResult` consumes. The serde renames are load-bearing:
/// the daemon deserializes this off the wire, so the camelCase names
/// (`validator`, `tcbStatus`, `mrTd`, `reportData`) must match byte for byte.
///
/// `mr_td` and `report_data` are lowercase hex. `report_data` is the 64 bytes
/// the enclave echoed, so the binding crate can recompute the challenge and
/// check the quote names its subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DcapResult {
    /// ER validator identity, base58. Empty if the enclave did not answer
    /// `getIdentity`; it is cosmetic to the binding/TCB checks.
    pub validator: String,
    /// DCAP TCB status verbatim from the QVL, e.g. `UpToDate`.
    pub tcb_status: String,
    /// TDX measurement (`mr_td`), hex: which enclave image ran.
    pub mr_td: String,
    /// The `report_data` the quote carried, hex.
    pub report_data: String,
}

#[derive(Debug, thiserror::Error)]
pub enum VerifierError {
    #[error("http request to the enclave failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("challenge must be base64 of exactly 64 bytes: {0}")]
    Challenge(String),
    #[error("enclave /quote response was malformed: {0}")]
    QuoteResponse(String),
    #[error("collateral fetch from the Intel PCCS failed: {0}")]
    Collateral(String),
    #[error("DCAP verification failed: {0}")]
    Dcap(String),
    #[error("expected a TDX (TD10/TD15) quote, got an SGX enclave quote")]
    NotTdx,
    #[error("enclave echoed a different challenge than requested (stale or swapped quote)")]
    ChallengeEchoMismatch,
}

/// Fetch a TDX quote from `tee_url` for `challenge_base64`, DCAP-verify it
/// against the Intel PCCS, and return the [`DcapResult`] the daemon consumes.
///
/// `challenge_base64` is opaque here: it is the 64 bytes the binding crate
/// produced, standard base64. We decode it only to confirm it is 64 bytes and
/// to check the enclave echoed it back verbatim.
pub async fn verify_quote(
    tee_url: &str,
    challenge_base64: &str,
) -> Result<DcapResult, VerifierError> {
    // Both this crate's reqwest (ring) and dcap-qvl's transitive reqwest compile
    // a rustls provider in. Install ring as the process default so the builder
    // that reads the default provider is never ambiguous. Idempotent: a second
    // call (or a provider already installed) is fine to ignore.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let challenge = B64
        .decode(challenge_base64)
        .map_err(|e| VerifierError::Challenge(e.to_string()))?;
    if challenge.len() != 64 {
        return Err(VerifierError::Challenge(format!(
            "{} bytes after base64 decode",
            challenge.len()
        )));
    }

    let client = reqwest::Client::new();
    let raw_quote = fetch_quote(&client, tee_url, challenge_base64).await?;
    let validator = fetch_identity(&client, tee_url).await.unwrap_or_default();

    // Collateral, then verification. Both come from the QVL; wrap their anyhow
    // errors as strings so this crate's error type stays self-contained.
    let collateral = dcap_qvl::collateral::CollateralClient::with_default_http(PCCS)
        .map_err(|e| VerifierError::Collateral(e.to_string()))?
        .fetch(&raw_quote)
        .await
        .map_err(|e| VerifierError::Collateral(e.to_string()))?;

    let verified = dcap_qvl::verify::verify(&raw_quote, &collateral, now_secs())
        .map_err(|e| VerifierError::Dcap(format!("{e:?}")))?;

    // TD10 and TD15 carry the same report_data/mr_td; TD15 nests TD10 as `base`.
    // An SGX quote has neither in the TDX sense, so it is not something this
    // enclave-attestation path can speak for.
    let (report_data, mr_td) = match &verified.report {
        dcap_qvl::quote::Report::TD10(td) => (td.report_data, td.mr_td),
        dcap_qvl::quote::Report::TD15(td) => (td.base.report_data, td.base.mr_td),
        dcap_qvl::quote::Report::SgxEnclave(_) => return Err(VerifierError::NotTdx),
    };

    // The enclave must have answered *our* challenge. A quote for any other
    // report_data is stale, replayed, or for someone else's request; returning
    // it would hand the caller a genuine-but-irrelevant attestation.
    if report_data.as_slice() != challenge.as_slice() {
        return Err(VerifierError::ChallengeEchoMismatch);
    }

    Ok(DcapResult {
        validator,
        tcb_status: verified.status.clone(),
        mr_td: hex::encode(mr_td),
        report_data: hex::encode(report_data),
    })
}

/// `GET {tee_url}/quote?challenge=<percent-encoded base64>` → the base64 quote
/// in the JSON `quote` field, decoded to DER bytes. Percent-encode the
/// challenge: standard base64 contains `+`, `/`, and `=`.
async fn fetch_quote(
    client: &reqwest::Client,
    tee_url: &str,
    challenge_base64: &str,
) -> Result<Vec<u8>, VerifierError> {
    let url = format!(
        "{}/quote?challenge={}",
        tee_url.trim_end_matches('/'),
        urlencoding::encode(challenge_base64)
    );
    let body: serde_json::Value = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let quote_b64 = body["quote"]
        .as_str()
        .ok_or_else(|| VerifierError::QuoteResponse("no `quote` field".into()))?;
    B64.decode(quote_b64)
        .map_err(|e| VerifierError::QuoteResponse(format!("quote is not base64: {e}")))
}

/// Best-effort validator identity via the ER's JSON-RPC `getIdentity`. The
/// binding and TCB checks do not depend on it, so a failure here is not fatal:
/// the caller gets an empty `validator` rather than a failed verification.
async fn fetch_identity(client: &reqwest::Client, tee_url: &str) -> Option<String> {
    let req = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "getIdentity"});
    let body: serde_json::Value = client
        .post(tee_url.trim_end_matches('/'))
        .json(&req)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    body["result"]["identity"].as_str().map(String::from)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire shape must be byte-identical to `covenantd::tee::DcapResult`,
    /// which is `#[serde(rename_all = "camelCase")]`. If these keys drift, the
    /// daemon silently fails to deserialize a verifier result, so pin them here.
    #[test]
    fn dcap_result_serializes_with_covenantd_camelcase_keys() {
        let dcap = DcapResult {
            validator: "MTEWGuqxUpYZGFJQcp8tLN7x5v9BSeoFHYWQQ3n3xzo".into(),
            tcb_status: "UpToDate".into(),
            mr_td: "feb748a1".into(),
            report_data: "deadbeef".into(),
        };

        let v = serde_json::to_value(&dcap).unwrap();
        // Exactly these four keys, exactly these names, in this order.
        assert_eq!(
            v.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["validator", "tcbStatus", "mrTd", "reportData"]
        );
        assert_eq!(v["validator"], dcap.validator);
        assert_eq!(v["tcbStatus"], dcap.tcb_status);
        assert_eq!(v["mrTd"], dcap.mr_td);
        assert_eq!(v["reportData"], dcap.report_data);

        // And the daemon's camelCase wire form deserializes straight back to us.
        let json = r#"{
            "validator": "MTEWGuqxUpYZGFJQcp8tLN7x5v9BSeoFHYWQQ3n3xzo",
            "tcbStatus": "UpToDate",
            "mrTd": "feb748a1",
            "reportData": "deadbeef"
        }"#;
        let back: DcapResult = serde_json::from_str(json).unwrap();
        assert_eq!(back, dcap);
    }

    /// The whole attested loop against MagicBlock mainnet. Ignored by default
    /// because it needs the network (mainnet-tee + the Intel PCCS); run with
    /// `cargo test -- --ignored live_bound_quote`.
    ///
    /// Drives the seam end to end: build a bound challenge with the offline
    /// binding crate, fetch+DCAP-verify the quote through [`verify_quote`], then
    /// prove the enclave echoed our bound 64 bytes.
    #[test]
    #[ignore = "hits mainnet-tee.magicblock.app and the Intel PCCS"]
    fn live_bound_quote() {
        use covenant_tee::{Binding, BoundQuote, TCB_UP_TO_DATE};
        use sha2::{Digest, Sha256};

        // A fixed triple so the run is reproducible: a real ER agent pubkey, a
        // subject commitment over a stand-in `model_digest || output_hash`, and
        // a fixed 32-byte nonce. In production the nonce is fresh per request.
        const AGENT: &str = "Ep7dD7biX7rZ6NSVzy8uEpgEEYipVfQ8ofwHzZmRM8dF";
        let subject_commitment = hex::encode(Sha256::digest(b"model-id-digest||output-hash demo"));
        let nonce = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

        let binding = Binding::new(AGENT, &subject_commitment, nonce).expect("valid binding");
        let challenge_b64 = binding.challenge_base64();

        // One runtime for the async seam; a couple of retries because either
        // mainnet-tee or the PCCS can be transiently unreachable.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dcap = rt.block_on(async {
            let mut last = None;
            for attempt in 1..=3 {
                match verify_quote("https://mainnet-tee.magicblock.app", &challenge_b64).await {
                    Ok(d) => return Ok(d),
                    Err(e) => {
                        eprintln!("attempt {attempt} failed: {e}");
                        last = Some(e);
                        std::thread::sleep(std::time::Duration::from_secs(3));
                    }
                }
            }
            Err(last.unwrap())
        });
        let dcap = dcap.expect("live quote verification");

        println!("validator   : {}", dcap.validator);
        println!("tcb_status  : {}", dcap.tcb_status);
        println!("mr_td       : {}", dcap.mr_td);
        println!("report_data : {}", dcap.report_data);

        assert_eq!(dcap.tcb_status, TCB_UP_TO_DATE, "platform must be current");
        assert!(!dcap.mr_td.is_empty(), "mr_td must be present");

        // The key assertion: the enclave echoed the 64 bytes our binding
        // produced, so this genuine quote is about *our* agent and subject.
        let bound = binding.matches_report_data(&dcap.report_data);
        println!("binding.matches_report_data = {bound}");
        assert!(
            bound,
            "report_data must be the challenge this binding produces"
        );

        // Reconstruct the daemon's envelope and confirm both halves independently.
        let quote = BoundQuote::new(
            binding,
            dcap.validator.clone(),
            dcap.tcb_status.clone(),
            dcap.mr_td.clone(),
            Vec::new(),
            dcap.report_data.clone(),
            0,
        );
        let check = quote.check();
        println!("bound        = {}", check.bound);
        println!("tcb_current  = {}", check.tcb_current);
        assert!(check.bound, "BoundQuote must bind");
        assert!(check.tcb_current, "BoundQuote must read as current");
    }
}
