//! Beta-token authentication. A customer presents `Authorization: Bearer
//! <token>`; the token resolves to an owner and a spend cap. Tokens are
//! provisioned out of band as a JSON array in the environment, hashed at
//! startup, and compared in constant time — the raw token is never
//! stored and a malformed token is indistinguishable from an unknown one.

use std::collections::HashSet;

use axum::http::header::AUTHORIZATION;
use axum::http::HeaderMap;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;

/// The resolved caller: who they are and how much they may commit across
/// all their live sessions.
#[derive(Debug, Clone)]
pub struct Principal {
    pub id: String,
    pub spend_cap_usdc_micros: u64,
}

/// One entry of the configured beta roster.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BetaCredential {
    pub owner: String,
    pub token: String,
    pub spend_cap_usdc_micros: u64,
}

struct CredentialHash {
    owner: String,
    token_hash: [u8; 32],
    spend_cap_usdc_micros: u64,
}

/// The set of valid beta tokens, keyed by hash. Built once at startup and
/// read on every request.
pub struct AuthRegistry {
    credentials: Vec<CredentialHash>,
}

impl AuthRegistry {
    /// Validates and hashes the roster. Owners and tokens must each be
    /// unique, every cap must be non-zero, and the set may not be empty —
    /// an empty roster is a misconfiguration, not an open door.
    pub fn new(credentials: Vec<BetaCredential>) -> Result<Self, AuthConfigError> {
        if credentials.is_empty() {
            return Err(AuthConfigError::Empty);
        }

        let mut owners = HashSet::new();
        let mut hashes = HashSet::new();
        let mut configured = Vec::with_capacity(credentials.len());
        for credential in credentials {
            validate_owner(&credential.owner)?;
            validate_token(&credential.token)?;
            if credential.spend_cap_usdc_micros == 0 {
                return Err(AuthConfigError::InvalidSpendCap);
            }

            let token_hash = hash_token(credential.token.as_bytes());
            if !owners.insert(credential.owner.clone()) {
                return Err(AuthConfigError::DuplicateOwner);
            }
            if !hashes.insert(token_hash) {
                return Err(AuthConfigError::DuplicateToken);
            }
            configured.push(CredentialHash {
                owner: credential.owner,
                token_hash,
                spend_cap_usdc_micros: credential.spend_cap_usdc_micros,
            });
        }

        Ok(Self {
            credentials: configured,
        })
    }

    /// Parses the roster from its JSON environment form.
    pub fn from_json(value: &str) -> Result<Self, AuthConfigError> {
        let credentials = serde_json::from_str(value).map_err(|_| AuthConfigError::InvalidJson)?;
        Self::new(credentials)
    }

    /// Resolves the bearer token in `headers` to its principal. The scheme
    /// is matched case-insensitively per RFC 7235, and the hash comparison
    /// scans every credential so a match takes the same time as a miss.
    pub fn authenticate(&self, headers: &HeaderMap) -> Result<Principal, AuthError> {
        let value = headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .ok_or(AuthError::Missing)?;
        let (scheme, token) = value.split_once(' ').ok_or(AuthError::Invalid)?;
        if !scheme.eq_ignore_ascii_case("Bearer") {
            return Err(AuthError::Invalid);
        }
        validate_token(token).map_err(|_| AuthError::Invalid)?;
        let supplied = hash_token(token.as_bytes());

        let mut match_index = None;
        for (index, credential) in self.credentials.iter().enumerate() {
            if credential.token_hash.ct_eq(&supplied).into() {
                match_index = Some(index);
            }
        }
        let credential = match_index
            .and_then(|index| self.credentials.get(index))
            .ok_or(AuthError::Invalid)?;

        Ok(Principal {
            id: credential.owner.clone(),
            spend_cap_usdc_micros: credential.spend_cap_usdc_micros,
        })
    }
}

fn hash_token(token: &[u8]) -> [u8; 32] {
    Sha256::digest(token).into()
}

fn validate_owner(owner: &str) -> Result<(), AuthConfigError> {
    if owner.is_empty()
        || owner.len() > 128
        || !owner
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(AuthConfigError::InvalidOwner);
    }
    Ok(())
}

fn validate_token(token: &str) -> Result<(), AuthConfigError> {
    if token.len() < 16
        || token.len() > 4_096
        || token.trim() != token
        || token.chars().any(char::is_control)
    {
        return Err(AuthConfigError::InvalidToken);
    }
    Ok(())
}

/// Why a beta roster failed to load. Surfaced once, at startup.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AuthConfigError {
    #[error("beta credential configuration is not valid JSON")]
    InvalidJson,
    #[error("at least one beta credential is required")]
    Empty,
    #[error("beta credential owner is invalid")]
    InvalidOwner,
    #[error("beta credential token is invalid")]
    InvalidToken,
    #[error("beta credential spend cap must be non-zero")]
    InvalidSpendCap,
    #[error("beta credential owner is duplicated")]
    DuplicateOwner,
    #[error("beta credential token is duplicated")]
    DuplicateToken,
}

/// Why a request was not authenticated. The two cases are kept distinct
/// so a missing header can prompt for one while a bad token cannot probe
/// which tokens exist.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AuthError {
    #[error("authorization is required")]
    Missing,
    #[error("authorization is invalid")]
    Invalid,
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};

    use super::*;

    fn registry() -> AuthRegistry {
        AuthRegistry::new(vec![BetaCredential {
            owner: "beta-a".into(),
            token: "a-secret-token-for-tests".into(),
            spend_cap_usdc_micros: 1_000,
        }])
        .unwrap()
    }

    fn bearer(value: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static(value));
        headers
    }

    #[test]
    fn a_valid_token_resolves_to_its_owner_and_cap() {
        let principal = registry()
            .authenticate(&bearer("Bearer a-secret-token-for-tests"))
            .unwrap();
        assert_eq!(principal.id, "beta-a");
        assert_eq!(principal.spend_cap_usdc_micros, 1_000);
    }

    #[test]
    fn the_bearer_scheme_is_case_insensitive() {
        assert_eq!(
            registry()
                .authenticate(&bearer("bearer a-secret-token-for-tests"))
                .unwrap()
                .id,
            "beta-a"
        );
    }

    #[test]
    fn a_missing_header_is_distinct_from_a_bad_one() {
        assert_eq!(
            registry().authenticate(&HeaderMap::new()).unwrap_err(),
            AuthError::Missing
        );
        assert_eq!(
            registry()
                .authenticate(&bearer("Bearer unknown-secret-token"))
                .unwrap_err(),
            AuthError::Invalid
        );
        assert_eq!(
            registry()
                .authenticate(&bearer("Basic a-secret-token-for-tests"))
                .unwrap_err(),
            AuthError::Invalid
        );
    }

    #[test]
    fn a_roster_must_be_non_empty_and_unique() {
        // AuthRegistry holds token hashes and so is not Debug; assert on
        // the error arm without unwrapping the Ok side.
        assert!(matches!(
            AuthRegistry::new(vec![]),
            Err(AuthConfigError::Empty)
        ));
        let dup = || BetaCredential {
            owner: "beta-a".into(),
            token: "a-secret-token-for-tests".into(),
            spend_cap_usdc_micros: 1_000,
        };
        assert!(matches!(
            AuthRegistry::new(vec![dup(), dup()]),
            Err(AuthConfigError::DuplicateOwner)
        ));
        assert!(matches!(
            AuthRegistry::from_json("not json"),
            Err(AuthConfigError::InvalidJson)
        ));
    }

    #[test]
    fn a_roster_refuses_an_ambiguous_token_a_weak_one_and_a_dead_cap() {
        let cred = |owner: &str, token: &str, cap: u64| BetaCredential {
            owner: owner.into(),
            token: token.into(),
            spend_cap_usdc_micros: cap,
        };

        // Two owners, one token: authenticate scans to the last match, so a
        // shared token would silently bind to whichever owner was listed
        // last. The roster refuses to load it rather than resolve a token
        // ambiguously.
        assert!(matches!(
            AuthRegistry::new(vec![
                cred("beta-a", "a-secret-token-for-tests", 1_000),
                cred("beta-b", "a-secret-token-for-tests", 2_000),
            ]),
            Err(AuthConfigError::DuplicateToken)
        ));

        // A token too short to resist guessing never enters the roster.
        assert!(matches!(
            AuthRegistry::new(vec![cred("beta-a", "short", 1_000)]),
            Err(AuthConfigError::InvalidToken)
        ));

        // A zero cap would authenticate an owner who can never launch a thing;
        // refuse it at load rather than serve a dead credential.
        assert!(matches!(
            AuthRegistry::new(vec![cred("beta-a", "a-secret-token-for-tests", 0)]),
            Err(AuthConfigError::InvalidSpendCap)
        ));
    }
}
