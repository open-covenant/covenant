//! Daemon-mediated secret broker.
//!
//! Tools and agents that need a credential ask the daemon for it by name at
//! call time instead of receiving it in their process environment, where a
//! compromised agent could read it. The daemon releases a named secret only to
//! a peer holding a `secret.access` capability whose scope admits the name, and
//! records every release and refusal to the audit chain — so secret use is
//! least-privilege and accountable.
//!
//! A [`SecretSource`] is the pluggable backend the broker reads from. This
//! module ships [`MapSecretSource`], an in-memory map an operator wires in at
//! boot via `Server::with_secret_source`. Sourcing real credentials from an
//! external store — and removing them from agent environments — is operator
//! configuration kept out of this mechanism slice.

use std::collections::BTreeMap;

use async_trait::async_trait;

/// A read-only backend the broker pulls named secrets from. `Ok(None)` means
/// the backend holds no secret under that name (the broker turns it into a
/// denial); `Err` is reserved for a backend that itself failed — a remote
/// store being unreachable, say — so the broker can fail closed instead of
/// leaking which names exist.
#[async_trait]
pub trait SecretSource: Send + Sync {
    async fn get(&self, name: &str) -> Result<Option<String>, SecretError>;
}

#[derive(Debug, thiserror::Error)]
#[error("secret backend unavailable: {0}")]
pub struct SecretError(pub String);

/// An in-memory [`SecretSource`] backed by a fixed name→value map. The map is
/// supplied at construction and never mutated, so lookups never fail. It
/// derives no `Debug` so a stray `{:?}` can never spill secret material.
pub struct MapSecretSource {
    secrets: BTreeMap<String, String>,
}

impl MapSecretSource {
    pub fn new(secrets: BTreeMap<String, String>) -> Self {
        Self { secrets }
    }
}

#[async_trait]
impl SecretSource for MapSecretSource {
    async fn get(&self, name: &str) -> Result<Option<String>, SecretError> {
        Ok(self.secrets.get(name).cloned())
    }
}
