//! Signed reads: proof of key possession for endpoints that return a
//! principal's own private data. A job envelope carries the buyer's
//! actual input (prompts, conversations), a receipt carries the
//! output, and the operator books carry per-job amounts and payout
//! confirmations — so none of these listings may open to anyone who
//! merely knows a pubkey or a job id; pubkeys are public in receipts
//! and audit rows, and job ids leak into logs.
//!
//! The key holder signs `{path, signed_at_ms}` under a fixed domain
//! and sends both values as headers; the server verifies possession of
//! the exact key the path names and bounds the timestamp to a skew
//! window. The path binding is what scopes a signature to one surface
//! — a signed buyer-history read cannot open an operator-books route,
//! or anyone else's. Endpoints are read-only, so a replay inside the
//! window re-reads data the key holder could read anyway — no nonce
//! store needed.

use covenant_identity::LocalIdentity;
use serde::Serialize;

use crate::sign::{sign_domain, to_canonical_json, verify_domain, ProtocolError};

/// The literal says `buyer_read` because buyer histories were the
/// first signed-read surface; it is pinned as the wire domain every
/// deployed signer already uses. Path binding, not the domain, is
/// what separates buyer routes from operator routes.
pub const SIGNED_READ_DOMAIN: &[u8] = b"covenant.compute.buyer_read.v1\n";
pub const SIGNED_READ_MAX_SKEW_MS: u64 = 120_000;
pub const READ_SIGNED_AT_HEADER: &str = "x-compute-read-signed-at-ms";
pub const READ_SIGNATURE_HEADER: &str = "x-compute-read-signature";

#[derive(Serialize)]
struct ReadClaim<'a> {
    path: &'a str,
    signed_at_ms: u64,
}

/// Signs a read of `path` at `signed_at_ms`. Send the timestamp in
/// [`READ_SIGNED_AT_HEADER`] and the returned signature in
/// [`READ_SIGNATURE_HEADER`]. `path` is the route's canonical
/// form (no scheme, host, or query) — both sides format it the same
/// way rather than trusting a raw URI.
pub fn sign_read(
    identity: &LocalIdentity,
    path: &str,
    signed_at_ms: u64,
) -> Result<String, ProtocolError> {
    let payload = to_canonical_json(&ReadClaim { path, signed_at_ms })?;
    let (signature_b58, _) = sign_domain(identity, SIGNED_READ_DOMAIN, &payload);
    Ok(signature_b58)
}

/// Verifies that the holder of `expected_pubkey_b58` signed a read of
/// exactly this `path` within the skew window around `now_ms`.
pub fn verify_read(
    expected_pubkey_b58: &str,
    path: &str,
    signed_at_ms: u64,
    signature_b58: &str,
    now_ms: u64,
) -> Result<(), ProtocolError> {
    if now_ms.abs_diff(signed_at_ms) > SIGNED_READ_MAX_SKEW_MS {
        return Err(ProtocolError::Invalid(format!(
            "read signature signed_at {signed_at_ms} is outside the \
             {SIGNED_READ_MAX_SKEW_MS}ms window around {now_ms}"
        )));
    }
    let payload = to_canonical_json(&ReadClaim { path, signed_at_ms })?;
    verify_domain(
        SIGNED_READ_DOMAIN,
        &payload,
        signature_b58,
        expected_pubkey_b58,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_signature_by_the_named_key_verifies() {
        let buyer = LocalIdentity::generate("buyer@local");
        let pubkey = bs58::encode(buyer.pubkey_bytes()).into_string();
        let sig = sign_read(&buyer, "/federation/buyers/k/jobs", 1_000).unwrap();
        verify_read(&pubkey, "/federation/buyers/k/jobs", 1_000, &sig, 2_000)
            .expect("verifies inside the window");
    }

    #[test]
    fn a_different_key_a_different_path_or_a_stale_stamp_all_fail() {
        let buyer = LocalIdentity::generate("buyer@local");
        let other = LocalIdentity::generate("other@local");
        let buyer_pk = bs58::encode(buyer.pubkey_bytes()).into_string();
        let other_pk = bs58::encode(other.pubkey_bytes()).into_string();
        let sig = sign_read(&buyer, "/federation/buyers/k/jobs", 1_000).unwrap();

        // Someone else's key: possession is exactly what's being proven.
        assert!(verify_read(&other_pk, "/federation/buyers/k/jobs", 1_000, &sig, 2_000).is_err());
        // A signature for one path must not open another.
        assert!(verify_read(&buyer_pk, "/federation/buyers/x/jobs", 1_000, &sig, 2_000).is_err());
        // Outside the replay window, either direction.
        let now_past_window = 1_000 + SIGNED_READ_MAX_SKEW_MS + 1;
        let err = verify_read(
            &buyer_pk,
            "/federation/buyers/k/jobs",
            1_000,
            &sig,
            now_past_window,
        )
        .unwrap_err();
        assert!(err.to_string().contains("window"), "got: {err}");
        // A tampered timestamp breaks the signature even in-window.
        assert!(verify_read(&buyer_pk, "/federation/buyers/k/jobs", 1_500, &sig, 2_000).is_err());
    }
}
