//! Peer-token registry — binds the calling peer (Unix-socket or HTTP)
//! to an [`AgentId`] so the daemon's capability checks attribute the
//! request to the actual caller, not to a self-asserted wire field.
//!
//! The threat this closes: Sprint 38/41 introduced
//! `a2a.respond.<sender>` as a sender-scoped capability, and Sprint 45
//! validated the shape of `<sender>`. Neither change verifies that the
//! peer making the request *is* that sender. A malicious local
//! process can connect to the daemon's Unix socket and claim to be
//! agent X. The registry adds a random 32-byte token per registered
//! peer; the daemon resolves `token → AgentId` at the start of every
//! connection (or every HTTP request), and uses that resolved
//! `AgentId` as the capability subject.
//!
//! Two storage backends implement [`PeerRegistry`]:
//! [`JsonlPeerRegistry`] for production (event log replays on
//! `open()`), and [`InMemoryPeerRegistry`] for tests. Both honour
//! revocation tombstones written via [`PeerRegistry::revoke`].
//!
//! Sprint 46 ships the registry and the wire types only — the daemon
//! handshake (`Authenticate { token_b58 }`) and the
//! `Server::respond(peer, req)` signature change land in Sprint 47.

#![deny(unsafe_code)]

use async_trait::async_trait;
use covenant_types::AgentId;
use rand::RngCore;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("invalid token base58: {0}")]
    BadTokenB58(String),
}

/// 32-byte opaque peer token. Equality and hashing are constant-time
/// in spirit (the token is a secret), but `PartialEq` here is the
/// stdlib byte-slice compare; callers handling untrusted tokens
/// should not branch on intermediate state.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PeerToken([u8; 32]);

impl PeerToken {
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_b58(&self) -> String {
        bs58::encode(self.0).into_string()
    }

    pub fn from_b58(s: &str) -> Result<Self, PeerError> {
        let bytes = bs58::decode(s)
            .into_vec()
            .map_err(|_| PeerError::BadTokenB58(s.to_owned()))?;
        if bytes.len() != 32 {
            return Err(PeerError::BadTokenB58(s.to_owned()));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(Self(arr))
    }
}

impl std::fmt::Debug for PeerToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Tokens are secrets — log only a 6-char prefix of the base58
        // form so audit grep still works without leaking the rest.
        let s = self.to_b58();
        let prefix = &s[..s.len().min(6)];
        write!(f, "PeerToken({prefix}…)")
    }
}

impl Serialize for PeerToken {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_b58())
    }
}

impl<'de> Deserialize<'de> for PeerToken {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        PeerToken::from_b58(&s).map_err(serde::de::Error::custom)
    }
}

/// One registry record — a token bound to an AgentId at a moment in time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerEntry {
    pub token: PeerToken,
    pub agent_id: AgentId,
    pub registered_at: u64,
}

#[async_trait]
pub trait PeerRegistry: Send + Sync {
    async fn register(&self, entry: PeerEntry) -> Result<(), PeerError>;
    /// Resolve `token` to its bound `AgentId`. Returns `None` for any
    /// token that was never registered or that has since been revoked.
    async fn resolve(&self, token: &PeerToken) -> Result<Option<AgentId>, PeerError>;
    /// Revoke a token. Returns `true` if a live binding existed
    /// (and is now removed), `false` if the token was unknown.
    async fn revoke(&self, token: &PeerToken) -> Result<bool, PeerError>;
    /// Read-only snapshot of the registry, oldest-first up to `limit`.
    /// Returns only currently-live entries (revoked tokens excluded).
    /// Operator-facing.
    async fn recent(&self, limit: usize) -> Result<Vec<PeerEntry>, PeerError>;
}

/// In-process registry suitable for tests.
pub struct InMemoryPeerRegistry {
    entries: Mutex<Vec<PeerEntry>>,
    revoked: Mutex<HashMap<[u8; 32], u64>>,
}

impl Default for InMemoryPeerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryPeerRegistry {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            revoked: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl PeerRegistry for InMemoryPeerRegistry {
    async fn register(&self, entry: PeerEntry) -> Result<(), PeerError> {
        self.entries.lock().await.push(entry);
        Ok(())
    }

    async fn resolve(&self, token: &PeerToken) -> Result<Option<AgentId>, PeerError> {
        if self.revoked.lock().await.contains_key(token.as_bytes()) {
            return Ok(None);
        }
        let entries = self.entries.lock().await;
        Ok(entries
            .iter()
            .find(|e| e.token == *token)
            .map(|e| e.agent_id.clone()))
    }

    async fn revoke(&self, token: &PeerToken) -> Result<bool, PeerError> {
        let was_live = {
            let entries = self.entries.lock().await;
            let revoked = self.revoked.lock().await;
            entries.iter().any(|e| e.token == *token) && !revoked.contains_key(token.as_bytes())
        };
        if was_live {
            self.revoked
                .lock()
                .await
                .insert(*token.as_bytes(), epoch_ms());
        }
        Ok(was_live)
    }

    async fn recent(&self, limit: usize) -> Result<Vec<PeerEntry>, PeerError> {
        let entries = self.entries.lock().await;
        let revoked = self.revoked.lock().await;
        Ok(entries
            .iter()
            .filter(|e| !revoked.contains_key(e.token.as_bytes()))
            .take(limit)
            .cloned()
            .collect())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PeerEvent {
    Registered(PeerEntry),
    Revoked { token: PeerToken, revoked_at: u64 },
}

/// JSONL-backed [`PeerRegistry`]. Append-only event log; `open()`
/// replays the log to rebuild the in-memory state. Mirrors the
/// shape of `JsonlCapabilityStore` and `JsonlMailbox`.
pub struct JsonlPeerRegistry {
    path: PathBuf,
    entries: Mutex<Vec<PeerEntry>>,
    revoked: Mutex<HashMap<[u8; 32], u64>>,
    file_lock: Arc<Mutex<()>>,
}

impl JsonlPeerRegistry {
    /// `path` should typically be `$COVENANT_HOME/peers/registry.jsonl`.
    /// Creates the file (and parent dirs) if missing.
    pub async fn open(path: PathBuf) -> Result<Self, PeerError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;

        let mut entries: Vec<PeerEntry> = Vec::new();
        let mut revoked: HashMap<[u8; 32], u64> = HashMap::new();
        let f = fs::File::open(&path).await?;
        let mut reader = BufReader::new(f);
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
            match serde_json::from_str::<PeerEvent>(trimmed)? {
                PeerEvent::Registered(entry) => entries.push(entry),
                PeerEvent::Revoked { token, revoked_at } => {
                    revoked.insert(*token.as_bytes(), revoked_at);
                }
            }
        }

        Ok(Self {
            path,
            entries: Mutex::new(entries),
            revoked: Mutex::new(revoked),
            file_lock: Arc::new(Mutex::new(())),
        })
    }

    async fn append(&self, ev: &PeerEvent) -> Result<(), PeerError> {
        let line = serde_json::to_string(ev)?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        f.write_all(line.as_bytes()).await?;
        f.write_all(b"\n").await?;
        f.flush().await?;
        Ok(())
    }
}

#[async_trait]
impl PeerRegistry for JsonlPeerRegistry {
    async fn register(&self, entry: PeerEntry) -> Result<(), PeerError> {
        let _g = self.file_lock.lock().await;
        self.append(&PeerEvent::Registered(entry.clone())).await?;
        self.entries.lock().await.push(entry);
        Ok(())
    }

    async fn resolve(&self, token: &PeerToken) -> Result<Option<AgentId>, PeerError> {
        if self.revoked.lock().await.contains_key(token.as_bytes()) {
            return Ok(None);
        }
        let entries = self.entries.lock().await;
        Ok(entries
            .iter()
            .find(|e| e.token == *token)
            .map(|e| e.agent_id.clone()))
    }

    async fn revoke(&self, token: &PeerToken) -> Result<bool, PeerError> {
        let _g = self.file_lock.lock().await;
        let was_live = {
            let entries = self.entries.lock().await;
            let revoked = self.revoked.lock().await;
            entries.iter().any(|e| e.token == *token) && !revoked.contains_key(token.as_bytes())
        };
        if was_live {
            let revoked_at = epoch_ms();
            self.append(&PeerEvent::Revoked {
                token: *token,
                revoked_at,
            })
            .await?;
            self.revoked
                .lock()
                .await
                .insert(*token.as_bytes(), revoked_at);
        }
        Ok(was_live)
    }

    async fn recent(&self, limit: usize) -> Result<Vec<PeerEntry>, PeerError> {
        let entries = self.entries.lock().await;
        let revoked = self.revoked.lock().await;
        Ok(entries
            .iter()
            .filter(|e| !revoked.contains_key(e.token.as_bytes()))
            .take(limit)
            .cloned()
            .collect())
    }
}

fn epoch_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_agent(name: &str) -> AgentId {
        AgentId::new(name, [0u8; 32])
    }

    fn entry(name: &str) -> (PeerToken, PeerEntry) {
        let token = PeerToken::generate();
        let entry = PeerEntry {
            token,
            agent_id: dummy_agent(name),
            registered_at: 1_700_000_000_000,
        };
        (token, entry)
    }

    #[test]
    fn token_round_trips_through_base58() {
        let t = PeerToken::generate();
        let s = t.to_b58();
        let back = PeerToken::from_b58(&s).expect("decode");
        assert_eq!(t, back);
    }

    #[test]
    fn token_from_b58_rejects_wrong_length() {
        // 16 bytes encoded — not 32.
        let short = bs58::encode([0u8; 16]).into_string();
        assert!(matches!(
            PeerToken::from_b58(&short),
            Err(PeerError::BadTokenB58(_))
        ));
    }

    #[test]
    fn token_debug_does_not_leak_full_bytes() {
        let t = PeerToken::generate();
        let s = format!("{t:?}");
        assert!(s.starts_with("PeerToken("));
        assert!(s.contains('…'));
        assert!(!s.contains(&t.to_b58()));
    }

    #[test]
    fn token_generate_yields_unique_values() {
        let a = PeerToken::generate();
        let b = PeerToken::generate();
        assert_ne!(a, b, "rng collision is astronomically improbable");
    }

    #[tokio::test]
    async fn in_memory_resolves_after_register() {
        let r = InMemoryPeerRegistry::new();
        let (token, entry) = entry("alice@local");
        r.register(entry.clone()).await.unwrap();
        let got = r.resolve(&token).await.unwrap();
        assert_eq!(got, Some(entry.agent_id));
    }

    #[tokio::test]
    async fn in_memory_resolves_unknown_to_none() {
        let r = InMemoryPeerRegistry::new();
        let stray = PeerToken::generate();
        assert_eq!(r.resolve(&stray).await.unwrap(), None);
    }

    #[tokio::test]
    async fn in_memory_revoke_reports_live_status() {
        let r = InMemoryPeerRegistry::new();
        let (token, entry) = entry("alice@local");
        r.register(entry).await.unwrap();
        assert!(r.revoke(&token).await.unwrap());
        // Already revoked → false.
        assert!(!r.revoke(&token).await.unwrap());
        // Unknown token → false.
        assert!(!r.revoke(&PeerToken::generate()).await.unwrap());
        // Resolution returns None post-revoke.
        assert_eq!(r.resolve(&token).await.unwrap(), None);
    }

    #[tokio::test]
    async fn in_memory_recent_excludes_revoked() {
        let r = InMemoryPeerRegistry::new();
        let (t1, e1) = entry("a@local");
        let (t2, e2) = entry("b@local");
        r.register(e1).await.unwrap();
        r.register(e2).await.unwrap();
        r.revoke(&t1).await.unwrap();
        let recent = r.recent(10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].token, t2);
    }

    #[tokio::test]
    async fn jsonl_replays_registers_and_revocations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers").join("registry.jsonl");

        let (t1, e1) = entry("alice@local");
        let (t2, e2) = entry("bob@local");
        {
            let r = JsonlPeerRegistry::open(path.clone()).await.unwrap();
            r.register(e1.clone()).await.unwrap();
            r.register(e2.clone()).await.unwrap();
            r.revoke(&t1).await.unwrap();
        }

        // Reopen — replay should reconstruct identical state.
        let r2 = JsonlPeerRegistry::open(path).await.unwrap();
        assert_eq!(r2.resolve(&t1).await.unwrap(), None, "t1 was revoked");
        assert_eq!(
            r2.resolve(&t2).await.unwrap(),
            Some(e2.agent_id),
            "t2 must replay live"
        );
        let recent = r2.recent(10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].token, t2);
    }

    #[tokio::test]
    async fn jsonl_open_on_missing_file_yields_empty_registry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does").join("not").join("exist.jsonl");
        let r = JsonlPeerRegistry::open(path).await.unwrap();
        assert!(r.recent(10).await.unwrap().is_empty());
        assert_eq!(
            r.resolve(&PeerToken::generate()).await.unwrap(),
            None,
            "fresh registry resolves nothing"
        );
    }
}
