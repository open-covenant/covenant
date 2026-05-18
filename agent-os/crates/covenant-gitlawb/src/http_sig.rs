//! RFC 9421 HTTP Message Signatures, in the exact shape gitlawb-node accepts.
//!
//! Every write to a gitlawb node carries three headers:
//!
//! ```text
//! Content-Digest:   sha-256=:<base64(sha256(body))>:
//! Signature-Input:  sig1=("@method" "@path" "content-digest");keyid="did:key:z6Mk...";alg="ed25519";created=<unix>
//! Signature:        sig1=:<base64(ed25519_sign(signing_string))>:
//! ```
//!
//! The signing string is the RFC 9421 §2.5 canonical form:
//!
//! ```text
//! "@method": POST
//! "@path": /api/v1/repos
//! "content-digest": sha-256=:...:
//! "@signature-params": ("@method" "@path" "content-digest");keyid="...";alg="ed25519";created=...
//! ```
//!
//! This module is a verbatim port of gitlawb-core's `http_sig.rs` covered
//! components, so a request signed here verifies on the node side.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chrono::Utc;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::did::did_key_from_verifying_key;
use crate::{GitlawbError, Result};

/// Covered components in every gitlawb-node signature.
pub const COVERED_COMPONENTS: &[&str] = &["@method", "@path", "content-digest"];

/// Default clock skew tolerance the gitlawb node enforces on inbound
/// signatures, in seconds.
pub const DEFAULT_CLOCK_SKEW_SECS: i64 = 300;

/// The three headers a signer emits per request.
#[derive(Debug, Clone)]
pub struct SignedHeaders {
    /// `Content-Digest: sha-256=:<b64>:`
    pub content_digest: String,
    /// `Signature-Input: sig1=(...);keyid="...";alg="ed25519";created=<unix>`
    pub signature_input: String,
    /// `Signature: sig1=:<b64>:`
    pub signature: String,
}

/// Sign an HTTP request with `signing_key` per RFC 9421, returning the three
/// headers to attach to the outbound request.
pub fn sign_request(
    signing_key: &SigningKey,
    method: &str,
    path_and_query: &str,
    body: &[u8],
) -> SignedHeaders {
    sign_request_at(
        signing_key,
        method,
        path_and_query,
        body,
        Utc::now().timestamp(),
    )
}

/// Same as [`sign_request`] but with the `created` timestamp pinned. Exposed
/// for deterministic test vectors; production callers should use
/// [`sign_request`].
pub fn sign_request_at(
    signing_key: &SigningKey,
    method: &str,
    path_and_query: &str,
    body: &[u8],
    created: i64,
) -> SignedHeaders {
    let did = did_key_from_verifying_key(&signing_key.verifying_key());
    let content_digest = compute_content_digest(body);

    let signature_input = format!(
        r#"sig1=("@method" "@path" "content-digest");keyid="{did}";alg="ed25519";created={created}"#
    );
    let sig_params_value = &signature_input["sig1=".len()..];

    let signing_string = signing_string(
        &method.to_uppercase(),
        path_and_query,
        &content_digest,
        sig_params_value,
    );

    let sig: Signature = signing_key.sign(signing_string.as_bytes());
    let signature = format!("sig1=:{}:", B64.encode(sig.to_bytes()));

    SignedHeaders {
        content_digest,
        signature_input,
        signature,
    }
}

/// Build the RFC 9421 §2.5 signing string for the three covered components.
fn signing_string(
    method_upper: &str,
    path_and_query: &str,
    content_digest: &str,
    sig_params_value: &str,
) -> String {
    format!(
        "\"@method\": {method_upper}\n\"@path\": {path_and_query}\n\"content-digest\": {content_digest}\n\"@signature-params\": {sig_params_value}"
    )
}

/// `sha-256=:<base64(SHA-256(body))>:`
pub fn compute_content_digest(body: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(body);
    format!("sha-256=:{}:", B64.encode(h.finalize()))
}

/// Verify a set of headers received from a peer.
///
/// Reconstructs the signing string from the headers + the known method, path,
/// and body, then verifies the ed25519 signature against the `keyid` DID.
/// Returns the verifying key on success so the caller can carry it forward.
///
/// `clock_skew_secs` defaults to [`DEFAULT_CLOCK_SKEW_SECS`] when `None`.
pub fn verify_request(
    headers: &SignedHeaders,
    method: &str,
    path_and_query: &str,
    body: &[u8],
    clock_skew_secs: Option<i64>,
) -> Result<VerifyingKey> {
    let parsed = ParsedInput::parse(&headers.signature_input)?;

    // 1. Reject if Content-Digest does not match the body we have.
    let computed_digest = compute_content_digest(body);
    if headers.content_digest != computed_digest {
        return Err(GitlawbError::Signature(format!(
            "content-digest mismatch: header={}, computed={}",
            headers.content_digest, computed_digest
        )));
    }

    // 2. Reject if any required component is missing from the cover list.
    for required in COVERED_COMPONENTS {
        if !parsed.components.iter().any(|c| c == required) {
            return Err(GitlawbError::Signature(format!(
                "missing covered component '{required}'"
            )));
        }
    }

    // 3. Clock skew.
    let now = Utc::now().timestamp();
    let max_skew = clock_skew_secs.unwrap_or(DEFAULT_CLOCK_SKEW_SECS);
    if (now - parsed.created).abs() > max_skew {
        return Err(GitlawbError::Signature(format!(
            "clock skew too large: {}s (max {max_skew}s)",
            (now - parsed.created).abs()
        )));
    }

    // 4. Algorithm.
    if parsed.alg != "ed25519" {
        return Err(GitlawbError::Signature(format!(
            "unsupported alg '{}', only ed25519 is accepted",
            parsed.alg
        )));
    }

    // 5. Decode the signature bytes.
    let sig_b64 = headers
        .signature
        .strip_prefix("sig1=:")
        .and_then(|s| s.strip_suffix(':'))
        .ok_or_else(|| {
            GitlawbError::Signature("Signature header must be 'sig1=:<base64>:'".into())
        })?;
    let sig_bytes: [u8; 64] = B64
        .decode(sig_b64)
        .map_err(|e| GitlawbError::Signature(format!("base64: {e}")))?
        .try_into()
        .map_err(|_| GitlawbError::Signature("signature must be 64 bytes".into()))?;
    let signature = Signature::from_bytes(&sig_bytes);

    // 6. Reconstruct the signing string and verify.
    let sig_params_value = &headers.signature_input["sig1=".len()..];
    let signing = signing_string(
        &method.to_uppercase(),
        path_and_query,
        &headers.content_digest,
        sig_params_value,
    );

    let vk = crate::did::verifying_key_from_did_key(&parsed.keyid)?;
    vk.verify(signing.as_bytes(), &signature)
        .map_err(|e| GitlawbError::Signature(format!("ed25519 verify: {e}")))?;

    Ok(vk)
}

struct ParsedInput {
    components: Vec<String>,
    keyid: String,
    alg: String,
    created: i64,
}

impl ParsedInput {
    fn parse(sig_input: &str) -> Result<Self> {
        let rest = sig_input
            .trim()
            .strip_prefix("sig1=")
            .ok_or_else(|| GitlawbError::Signature("expected 'sig1=' label".into()))?;

        let open = rest
            .find('(')
            .ok_or_else(|| GitlawbError::Signature("missing '(' in Signature-Input".into()))?;
        let close = rest
            .find(')')
            .ok_or_else(|| GitlawbError::Signature("missing ')' in Signature-Input".into()))?;

        let components: Vec<String> = rest[open + 1..close]
            .split_whitespace()
            .map(|s| s.trim_matches('"').to_string())
            .collect();

        let mut keyid = None;
        let mut alg = None;
        let mut created = None;

        for part in rest[close + 1..].split(';') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if let Some((k, v)) = part.split_once('=') {
                let v = v.trim_matches('"');
                match k.trim() {
                    "keyid" => keyid = Some(v.to_string()),
                    "alg" => alg = Some(v.to_string()),
                    "created" => {
                        created = Some(v.parse::<i64>().map_err(|_| {
                            GitlawbError::Signature("invalid 'created' timestamp".into())
                        })?)
                    }
                    _ => {} // ignore unknown params
                }
            }
        }

        Ok(Self {
            components,
            keyid: keyid.ok_or_else(|| GitlawbError::Signature("missing keyid param".into()))?,
            alg: alg.ok_or_else(|| GitlawbError::Signature("missing alg param".into()))?,
            created: created
                .ok_or_else(|| GitlawbError::Signature("missing created param".into()))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;
    use rand::TryRngCore;

    fn fresh() -> SigningKey {
        let mut seed = [0u8; 32];
        OsRng.try_fill_bytes(&mut seed).unwrap();
        SigningKey::from_bytes(&seed)
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let sk = fresh();
        let body = br#"{"name":"my-repo"}"#;
        let headers = sign_request(&sk, "POST", "/api/v1/repos", body);

        let vk = verify_request(&headers, "POST", "/api/v1/repos", body, None).unwrap();
        assert_eq!(vk.to_bytes(), sk.verifying_key().to_bytes());
    }

    #[test]
    fn header_shape_matches_protocol() {
        let sk = fresh();
        let h = sign_request(&sk, "POST", "/api/v1/repos", b"{}");
        assert!(h.content_digest.starts_with("sha-256=:"));
        assert!(h.content_digest.ends_with(':'));
        assert!(h.signature_input.starts_with("sig1=("));
        assert!(h.signature_input.contains(r#"alg="ed25519""#));
        assert!(h.signature_input.contains("keyid=\"did:key:z6Mk"));
        assert!(h.signature.starts_with("sig1=:"));
        assert!(h.signature.ends_with(':'));
    }

    #[test]
    fn method_is_uppercased() {
        let sk = fresh();
        let h = sign_request(&sk, "post", "/x", b"");
        // Verify with the *correct* canonical method works.
        verify_request(&h, "POST", "/x", b"", None).unwrap();
    }

    #[test]
    fn tampered_body_fails_verify() {
        let sk = fresh();
        let h = sign_request(&sk, "POST", "/x", b"original");
        let err = verify_request(&h, "POST", "/x", b"tampered", None).unwrap_err();
        assert!(matches!(err, GitlawbError::Signature(_)));
    }

    #[test]
    fn tampered_path_fails_verify() {
        let sk = fresh();
        let h = sign_request(&sk, "POST", "/a", b"");
        let err = verify_request(&h, "POST", "/b", b"", None).unwrap_err();
        assert!(matches!(err, GitlawbError::Signature(_)));
    }

    #[test]
    fn stale_created_rejected() {
        let sk = fresh();
        // 10 minutes in the past, outside the 300s default skew.
        let h = sign_request_at(&sk, "POST", "/x", b"", Utc::now().timestamp() - 600);
        let err = verify_request(&h, "POST", "/x", b"", None).unwrap_err();
        assert!(matches!(err, GitlawbError::Signature(_)));
    }

    #[test]
    fn content_digest_is_deterministic() {
        let a = compute_content_digest(b"same");
        let b = compute_content_digest(b"same");
        assert_eq!(a, b);
        let c = compute_content_digest(b"different");
        assert_ne!(a, c);
    }

    #[test]
    fn empty_body_digest_is_valid() {
        let d = compute_content_digest(b"");
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        // base64 = 47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=
        assert_eq!(d, "sha-256=:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=:");
    }

    #[test]
    fn parse_rejects_missing_label() {
        let bad = r#"sigX=("@method");keyid="did:key:z";alg="ed25519";created=1"#;
        assert!(ParsedInput::parse(bad).is_err());
    }
}
