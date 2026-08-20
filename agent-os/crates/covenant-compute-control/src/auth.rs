use std::collections::HashSet;

use axum::http::header::AUTHORIZATION;
use axum::http::HeaderMap;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct Principal {
    pub id: String,
    pub spend_cap_usdc_micros: u64,
}

#[derive(Deserialize)]
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

pub struct AuthRegistry {
    credentials: Vec<CredentialHash>,
}

impl AuthRegistry {
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

    pub fn from_json(value: &str) -> Result<Self, AuthConfigError> {
        let credentials = serde_json::from_str(value).map_err(|_| AuthConfigError::InvalidJson)?;
        Self::new(credentials)
    }

    pub fn authenticate(&self, headers: &HeaderMap) -> Result<Principal, AuthError> {
        let value = headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .ok_or(AuthError::Missing)?;
        let token = value.strip_prefix("Bearer ").ok_or(AuthError::Invalid)?;
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

    #[test]
    fn token_is_bound_to_owner_and_cap() {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer a-secret-token-for-tests"),
        );
        let principal = registry().authenticate(&headers).unwrap();
        assert_eq!(principal.id, "beta-a");
        assert_eq!(principal.spend_cap_usdc_micros, 1_000);
    }

    #[test]
    fn malformed_and_unknown_tokens_are_indistinguishable() {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer unknown-secret-token"),
        );
        assert_eq!(
            registry().authenticate(&headers).unwrap_err(),
            AuthError::Invalid
        );

        headers.insert(AUTHORIZATION, HeaderValue::from_static("Basic secret"));
        assert_eq!(
            registry().authenticate(&headers).unwrap_err(),
            AuthError::Invalid
        );
    }
}
