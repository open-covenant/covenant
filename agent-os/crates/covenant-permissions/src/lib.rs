//! Capability-token primitive for Covenant.
//!
//! A [`SignedCapability`] is a [`Capability`] (subject, action, scope,
//! granted-by, optional expiry) plus an ed25519 signature by the
//! granter over a deterministic byte encoding of those fields. The
//! encoder is hand-rolled and length-prefixed; it lives in one place
//! ([`canonical_message`]) and can be replaced without disturbing the
//! wire format.
//!
//! Two storage backends implement [`CapabilityStore`]:
//! [`JsonlCapabilityStore`] for production and
//! [`InMemoryCapabilityStore`] for tests. Both honour revocation
//! tombstones written via [`CapabilityStore::revoke`].

#![deny(unsafe_code)]

use async_trait::async_trait;
use covenant_types::Capability;
use ed25519_dalek::{Signature, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum PermissionError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("ed25519: {0}")]
    Crypto(#[from] ed25519_dalek::SignatureError),
    #[error("capability expired at {0}")]
    Expired(u64),
    #[error("signature does not verify against granted_by pubkey")]
    BadSignature,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SignedCapability {
    pub capability: Capability,
    #[serde(with = "sig_b58")]
    pub signature: [u8; 64],
}

mod sig_b58 {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&bs58::encode(v).into_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        let s = String::deserialize(d)?;
        let bytes = bs58::decode(&s)
            .into_vec()
            .map_err(serde::de::Error::custom)?;
        if bytes.len() != 64 {
            return Err(serde::de::Error::custom(format!(
                "expected 64-byte signature, got {}",
                bytes.len()
            )));
        }
        let mut arr = [0u8; 64];
        arr.copy_from_slice(&bytes);
        Ok(arr)
    }
}

/// Deterministic byte encoding of a capability — what the signer signs.
///
/// Layout:
/// `subject_pubkey[32] || action_len_be[4] || action || scope_len_be[4] ||
///  scope_json_bytes || granted_by_pubkey[32] || expires_tag[1] || expires_at_be[8]`
pub fn canonical_message(cap: &Capability) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&cap.subject.pubkey);

    let action_bytes = cap.action.as_bytes();
    out.extend_from_slice(&(action_bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(action_bytes);

    // scope is a serde_json::Value; the byte encoding is whatever serde_json
    // emits. serde_json::Map preserves insertion order, so the encoding is
    // stable for a given construction. RFC 8785 (JCS) is the proper hardening.
    let scope_bytes = serde_json::to_vec(&cap.scope).expect("scope serialise");
    out.extend_from_slice(&(scope_bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(&scope_bytes);

    out.extend_from_slice(&cap.granted_by.pubkey);
    out.push(if cap.expires_at.is_some() { 1 } else { 0 });
    out.extend_from_slice(&cap.expires_at.unwrap_or(0).to_be_bytes());
    out
}

/// Sign a capability with `granted_by`'s key. The caller must ensure
/// `cap.granted_by.pubkey == verifying_key(of_signing_key).to_bytes()`; this
/// fn does not enforce it (asymmetric authority delegations are valid in
/// principle).
pub fn sign(cap: Capability, signing_key: &SigningKey) -> SignedCapability {
    let msg = canonical_message(&cap);
    let signature = ed25519_dalek::Signer::sign(signing_key, &msg);
    SignedCapability {
        capability: cap,
        signature: signature.to_bytes(),
    }
}

/// Verify the signature against `cap.granted_by.pubkey`. Does **not** check
/// expiry; use `verify_with_clock` for that.
pub fn verify(signed: &SignedCapability) -> Result<(), PermissionError> {
    let vk = VerifyingKey::from_bytes(&signed.capability.granted_by.pubkey)?;
    let sig = Signature::from_bytes(&signed.signature);
    let msg = canonical_message(&signed.capability);
    vk.verify(&msg, &sig)
        .map_err(|_| PermissionError::BadSignature)
}

/// Like `verify` but also rejects an expired capability. `now_ms` is epoch
/// milliseconds; pass the daemon's clock at the point of the check.
pub fn verify_with_clock(signed: &SignedCapability, now_ms: u64) -> Result<(), PermissionError> {
    verify(signed)?;
    if let Some(exp) = signed.capability.expires_at {
        if now_ms > exp {
            return Err(PermissionError::Expired(exp));
        }
    }
    Ok(())
}

#[async_trait]
pub trait CapabilityStore: Send + Sync {
    async fn record(&self, signed: SignedCapability) -> Result<(), PermissionError>;
    /// Returns `true` if a matching live capability was present (and is now
    /// revoked); `false` if no live capability had that signature.
    async fn revoke(&self, signature: [u8; 64]) -> Result<bool, PermissionError>;
    async fn is_revoked(&self, signature: [u8; 64]) -> Result<bool, PermissionError>;
    async fn list_for_subject(
        &self,
        subject_pubkey: [u8; 32],
    ) -> Result<Vec<SignedCapability>, PermissionError>;
    async fn recent(&self, limit: usize) -> Result<Vec<SignedCapability>, PermissionError>;
}

/// Revocation record. The daemon writes one of these per `revoke()` call;
/// a capability is treated as live iff its signature is in `granted.jsonl`
/// and **not** in `revoked.jsonl`. Revocations are themselves not signed
/// (the daemon's local identity is the trust root for v0); a future
/// `Phase 5+` could add countersignatures.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Revocation {
    #[serde(with = "sig_b58")]
    pub signature: [u8; 64],
    pub revoked_at: u64,
}

pub struct JsonlCapabilityStore {
    granted_path: PathBuf,
    revoked_path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl JsonlCapabilityStore {
    /// `granted_path` should typically be `$COVENANT_HOME/capabilities/granted.jsonl`;
    /// the matching revocation log lives next to it as `revoked.jsonl`.
    pub async fn open(granted_path: PathBuf) -> Result<Self, PermissionError> {
        if let Some(parent) = granted_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&granted_path)
            .await?;
        let revoked_path = granted_path
            .parent()
            .map(|p| p.join("revoked.jsonl"))
            .unwrap_or_else(|| PathBuf::from("revoked.jsonl"));
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&revoked_path)
            .await?;
        Ok(Self {
            granted_path,
            revoked_path,
            lock: Arc::new(Mutex::new(())),
        })
    }

    async fn read_all_grants(&self) -> Result<Vec<SignedCapability>, PermissionError> {
        Self::read_jsonl(&self.granted_path).await
    }

    async fn read_all_revocations(&self) -> Result<Vec<Revocation>, PermissionError> {
        Self::read_jsonl(&self.revoked_path).await
    }

    async fn read_jsonl<T: serde::de::DeserializeOwned>(
        path: &std::path::Path,
    ) -> Result<Vec<T>, PermissionError> {
        let f = match fs::File::open(path).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut reader = BufReader::new(f);
        let mut all = Vec::new();
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break;
            }
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                continue;
            }
            all.push(serde_json::from_str(trimmed)?);
        }
        Ok(all)
    }

    fn epoch_ms() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

#[async_trait]
impl CapabilityStore for JsonlCapabilityStore {
    async fn record(&self, signed: SignedCapability) -> Result<(), PermissionError> {
        let _g = self.lock.lock().await;
        let line = serde_json::to_string(&signed)?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.granted_path)
            .await?;
        f.write_all(line.as_bytes()).await?;
        f.write_all(b"\n").await?;
        f.flush().await?;
        Ok(())
    }

    async fn revoke(&self, signature: [u8; 64]) -> Result<bool, PermissionError> {
        let _g = self.lock.lock().await;
        let already_revoked = Self::read_jsonl::<Revocation>(&self.revoked_path)
            .await?
            .iter()
            .any(|r| r.signature == signature);
        if already_revoked {
            return Ok(false);
        }
        let was_granted = Self::read_jsonl::<SignedCapability>(&self.granted_path)
            .await?
            .iter()
            .any(|c| c.signature == signature);
        if !was_granted {
            return Ok(false);
        }
        let rev = Revocation {
            signature,
            revoked_at: Self::epoch_ms(),
        };
        let line = serde_json::to_string(&rev)?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.revoked_path)
            .await?;
        f.write_all(line.as_bytes()).await?;
        f.write_all(b"\n").await?;
        f.flush().await?;
        Ok(true)
    }

    async fn is_revoked(&self, signature: [u8; 64]) -> Result<bool, PermissionError> {
        let _g = self.lock.lock().await;
        Ok(Self::read_jsonl::<Revocation>(&self.revoked_path)
            .await?
            .iter()
            .any(|r| r.signature == signature))
    }

    async fn list_for_subject(
        &self,
        subject_pubkey: [u8; 32],
    ) -> Result<Vec<SignedCapability>, PermissionError> {
        let _g = self.lock.lock().await;
        let revoked: std::collections::HashSet<[u8; 64]> = self
            .read_all_revocations()
            .await?
            .into_iter()
            .map(|r| r.signature)
            .collect();
        Ok(self
            .read_all_grants()
            .await?
            .into_iter()
            .filter(|s| s.capability.subject.pubkey == subject_pubkey)
            .filter(|s| !revoked.contains(&s.signature))
            .collect())
    }

    async fn recent(&self, limit: usize) -> Result<Vec<SignedCapability>, PermissionError> {
        let _g = self.lock.lock().await;
        let revoked: std::collections::HashSet<[u8; 64]> = self
            .read_all_revocations()
            .await?
            .into_iter()
            .map(|r| r.signature)
            .collect();
        let mut live: Vec<SignedCapability> = self
            .read_all_grants()
            .await?
            .into_iter()
            .filter(|s| !revoked.contains(&s.signature))
            .collect();
        let start = live.len().saturating_sub(limit);
        Ok(live.split_off(start))
    }
}

#[derive(Default)]
pub struct InMemoryCapabilityStore {
    granted: Mutex<Vec<SignedCapability>>,
    revoked: Mutex<std::collections::HashSet<[u8; 64]>>,
}

impl InMemoryCapabilityStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl CapabilityStore for InMemoryCapabilityStore {
    async fn record(&self, signed: SignedCapability) -> Result<(), PermissionError> {
        self.granted.lock().await.push(signed);
        Ok(())
    }

    async fn revoke(&self, signature: [u8; 64]) -> Result<bool, PermissionError> {
        let mut revoked = self.revoked.lock().await;
        if revoked.contains(&signature) {
            return Ok(false);
        }
        let granted = self.granted.lock().await;
        if !granted.iter().any(|c| c.signature == signature) {
            return Ok(false);
        }
        revoked.insert(signature);
        Ok(true)
    }

    async fn is_revoked(&self, signature: [u8; 64]) -> Result<bool, PermissionError> {
        Ok(self.revoked.lock().await.contains(&signature))
    }

    async fn list_for_subject(
        &self,
        subject_pubkey: [u8; 32],
    ) -> Result<Vec<SignedCapability>, PermissionError> {
        let revoked = self.revoked.lock().await;
        let granted = self.granted.lock().await;
        Ok(granted
            .iter()
            .filter(|s| s.capability.subject.pubkey == subject_pubkey)
            .filter(|s| !revoked.contains(&s.signature))
            .cloned()
            .collect())
    }

    async fn recent(&self, limit: usize) -> Result<Vec<SignedCapability>, PermissionError> {
        let revoked = self.revoked.lock().await;
        let granted = self.granted.lock().await;
        let live: Vec<SignedCapability> = granted
            .iter()
            .filter(|s| !revoked.contains(&s.signature))
            .cloned()
            .collect();
        let start = live.len().saturating_sub(limit);
        Ok(live[start..].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_identity::LocalIdentity;
    use covenant_types::AgentId;

    fn cap(
        subject: AgentId,
        action: &str,
        granted_by: AgentId,
        expires_at: Option<u64>,
    ) -> Capability {
        Capability {
            subject,
            action: action.into(),
            scope: serde_json::json!({ "path": "research/*" }),
            granted_by,
            expires_at,
        }
    }

    #[test]
    fn sign_and_verify_round_trip() {
        let issuer = LocalIdentity::generate("authority@local");
        let subject = LocalIdentity::generate("research@local").agent_id();
        let signed = sign(
            cap(subject, "tool.web_search", issuer.agent_id(), None),
            issuer.signing_key(),
        );
        assert!(verify(&signed).is_ok());
    }

    #[test]
    fn verify_rejects_tampered_action() {
        let issuer = LocalIdentity::generate("authority@local");
        let subject = LocalIdentity::generate("research@local").agent_id();
        let mut signed = sign(
            cap(subject, "tool.web_search", issuer.agent_id(), None),
            issuer.signing_key(),
        );
        signed.capability.action = "tool.gpu_inference".into();
        assert!(matches!(
            verify(&signed),
            Err(PermissionError::BadSignature)
        ));
    }

    #[test]
    fn verify_with_clock_rejects_expired() {
        let issuer = LocalIdentity::generate("authority@local");
        let subject = LocalIdentity::generate("research@local").agent_id();
        let signed = sign(
            cap(subject, "tool.web_search", issuer.agent_id(), Some(1000)),
            issuer.signing_key(),
        );
        assert!(verify_with_clock(&signed, 999).is_ok());
        assert!(matches!(
            verify_with_clock(&signed, 1001),
            Err(PermissionError::Expired(1000))
        ));
    }

    #[tokio::test]
    async fn in_memory_store_filters_by_subject() {
        let issuer = LocalIdentity::generate("authority@local");
        let alice = LocalIdentity::generate("alice@local").agent_id();
        let bob = LocalIdentity::generate("bob@local").agent_id();

        let s = InMemoryCapabilityStore::new();
        s.record(sign(
            cap(alice.clone(), "tool.web_search", issuer.agent_id(), None),
            issuer.signing_key(),
        ))
        .await
        .unwrap();
        s.record(sign(
            cap(bob.clone(), "memory.write", issuer.agent_id(), None),
            issuer.signing_key(),
        ))
        .await
        .unwrap();

        let alice_caps = s.list_for_subject(alice.pubkey).await.unwrap();
        assert_eq!(alice_caps.len(), 1);
        assert_eq!(alice_caps[0].capability.action, "tool.web_search");
    }

    #[tokio::test]
    async fn jsonl_round_trip_through_a_real_file() {
        let issuer = LocalIdentity::generate("authority@local");
        let subject = LocalIdentity::generate("research@local").agent_id();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("granted.jsonl");
        let s = JsonlCapabilityStore::open(path.clone()).await.unwrap();
        s.record(sign(
            cap(subject, "tool.web_search", issuer.agent_id(), None),
            issuer.signing_key(),
        ))
        .await
        .unwrap();

        let s2 = JsonlCapabilityStore::open(path.clone()).await.unwrap();
        let recent = s2.recent(10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert!(verify(&recent[0]).is_ok());
    }

    #[test]
    fn signed_capability_round_trips_through_serde() {
        let issuer = LocalIdentity::generate("authority@local");
        let subject = LocalIdentity::generate("research@local").agent_id();
        let signed = sign(
            cap(subject, "memory.write", issuer.agent_id(), Some(123_456)),
            issuer.signing_key(),
        );
        let json = serde_json::to_string(&signed).unwrap();
        let back: SignedCapability = serde_json::from_str(&json).unwrap();
        assert_eq!(signed, back);
        assert!(verify(&back).is_ok());
    }

    #[tokio::test]
    async fn in_memory_revoke_removes_from_subject_list() {
        let issuer = LocalIdentity::generate("authority@local");
        let subject = LocalIdentity::generate("research@local").agent_id();
        let signed = sign(
            cap(subject.clone(), "tool.web_search", issuer.agent_id(), None),
            issuer.signing_key(),
        );
        let s = InMemoryCapabilityStore::new();
        s.record(signed.clone()).await.unwrap();

        assert_eq!(s.list_for_subject(subject.pubkey).await.unwrap().len(), 1);
        assert!(s.revoke(signed.signature).await.unwrap());
        assert!(s.is_revoked(signed.signature).await.unwrap());
        assert_eq!(s.list_for_subject(subject.pubkey).await.unwrap().len(), 0);
        // Re-revoking is a no-op.
        assert!(!s.revoke(signed.signature).await.unwrap());
    }

    #[tokio::test]
    async fn jsonl_revoke_persists_across_reopen() {
        let issuer = LocalIdentity::generate("authority@local");
        let subject = LocalIdentity::generate("research@local").agent_id();
        let signed = sign(
            cap(subject, "memory.write", issuer.agent_id(), None),
            issuer.signing_key(),
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capabilities").join("granted.jsonl");
        let s = JsonlCapabilityStore::open(path.clone()).await.unwrap();
        s.record(signed.clone()).await.unwrap();
        assert!(s.revoke(signed.signature).await.unwrap());

        let s2 = JsonlCapabilityStore::open(path).await.unwrap();
        assert!(s2.is_revoked(signed.signature).await.unwrap());
        assert!(s2.recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn revoke_unknown_signature_is_a_no_op() {
        let s = InMemoryCapabilityStore::new();
        let r = s.revoke([0u8; 64]).await.unwrap();
        assert!(!r);
    }
}
