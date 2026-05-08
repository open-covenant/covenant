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
    /// Drop revocation tombstones with `revoked_at < before_ms` along
    /// with their matching `Registered` entries. Returns the number of
    /// revocations dropped (= number of `Registered` entries also
    /// dropped, modulo any pre-existing orphaned revocations — those
    /// drop too). Live entries (registered but never revoked) are
    /// untouched. Mirrors `CapabilityStore::purge_revoked_older_than`.
    async fn purge_revoked_older_than(&self, before_ms: u64) -> Result<u64, PeerError>;
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

    async fn purge_revoked_older_than(&self, before_ms: u64) -> Result<u64, PeerError> {
        let mut revoked = self.revoked.lock().await;
        let drop_tokens: Vec<[u8; 32]> = revoked
            .iter()
            .filter(|(_, ts)| **ts < before_ms)
            .map(|(t, _)| *t)
            .collect();
        let purged = drop_tokens.len() as u64;
        if purged == 0 {
            return Ok(0);
        }
        let drop_set: std::collections::HashSet<[u8; 32]> = drop_tokens.iter().copied().collect();
        for t in &drop_tokens {
            revoked.remove(t);
        }
        let mut entries = self.entries.lock().await;
        entries.retain(|e| !drop_set.contains(e.token.as_bytes()));
        Ok(purged)
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

    async fn purge_revoked_older_than(&self, before_ms: u64) -> Result<u64, PeerError> {
        // Hold file_lock across the whole read-filter-rewrite so a
        // concurrent register / revoke can't race with the rewrite.
        // Atomicity of the rewrite comes from tempfile + rename.
        let _g = self.file_lock.lock().await;

        let raw = match fs::read_to_string(&self.path).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        let events: Vec<PeerEvent> = raw
            .lines()
            .filter(|l| !l.is_empty())
            .map(serde_json::from_str)
            .collect::<Result<Vec<_>, _>>()?;

        let drop_tokens: std::collections::HashSet<[u8; 32]> = events
            .iter()
            .filter_map(|ev| match ev {
                PeerEvent::Revoked { token, revoked_at } if *revoked_at < before_ms => {
                    Some(*token.as_bytes())
                }
                _ => None,
            })
            .collect();
        let purged = drop_tokens.len() as u64;
        if purged == 0 {
            return Ok(0);
        }

        let kept: Vec<&PeerEvent> = events
            .iter()
            .filter(|ev| match ev {
                PeerEvent::Registered(entry) => !drop_tokens.contains(entry.token.as_bytes()),
                PeerEvent::Revoked { token, .. } => !drop_tokens.contains(token.as_bytes()),
            })
            .collect();

        let tmp_path = self.path.with_extension("jsonl.tmp");
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)
            .await?;
        for ev in &kept {
            let line = serde_json::to_string(ev)?;
            f.write_all(line.as_bytes()).await?;
            f.write_all(b"\n").await?;
        }
        f.flush().await?;
        drop(f);
        fs::rename(&tmp_path, &self.path).await?;

        // Mirror the on-disk drop into in-memory state. Entries-first,
        // revoked-second is load-bearing: `resolve()` checks `revoked`
        // before `entries`, so the intermediate state must keep a
        // dropped token's tombstone visible until its entry is gone.
        // Reversing the order would expose a TOCTOU window where a
        // recently-purged token's `Registered` entry is still in
        // `entries` after its tombstone was removed from `revoked` —
        // a concurrent `resolve()` would authenticate the dead token.
        {
            let mut entries = self.entries.lock().await;
            entries.retain(|e| !drop_tokens.contains(e.token.as_bytes()));
        }
        {
            let mut revoked = self.revoked.lock().await;
            for t in &drop_tokens {
                revoked.remove(t);
            }
        }
        Ok(purged)
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

    #[tokio::test]
    async fn in_memory_purge_drops_old_revocations_and_their_entries() {
        let r = InMemoryPeerRegistry::new();
        let (live_token, live_entry) = entry("live@local");
        let (dead_token, dead_entry) = entry("dead@local");
        r.register(live_entry.clone()).await.unwrap();
        r.register(dead_entry.clone()).await.unwrap();
        assert!(r.revoke(&dead_token).await.unwrap());

        // Force the revocation timestamp into the past.
        r.revoked.lock().await.insert(*dead_token.as_bytes(), 50);

        let purged = r.purge_revoked_older_than(100).await.unwrap();
        assert_eq!(purged, 1);

        // Live token still resolves.
        assert_eq!(
            r.resolve(&live_token).await.unwrap(),
            Some(live_entry.agent_id)
        );
        // Dead token no longer resolves; tombstone is gone but the
        // entry is gone too, so resolution returns None.
        assert_eq!(r.resolve(&dead_token).await.unwrap(), None);
        // Recent shows only the live entry.
        let recent = r.recent(10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].token, live_token);
    }

    #[tokio::test]
    async fn jsonl_purge_rewrites_atomically_and_keeps_live_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.jsonl");

        let (live_token, live_entry) = entry("live@local");
        let (dead_token, dead_entry) = entry("dead@local");
        let r = JsonlPeerRegistry::open(path.clone()).await.unwrap();
        r.register(live_entry.clone()).await.unwrap();
        r.register(dead_entry.clone()).await.unwrap();
        assert!(r.revoke(&dead_token).await.unwrap());

        // Hand-rewrite the on-disk revocation with a deterministic past
        // timestamp (the in-process revoke just stamped now).
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = raw.lines().map(String::from).collect();
        for line in lines.iter_mut() {
            if line.contains("\"revoked\"") {
                let ev: PeerEvent = serde_json::from_str(line).unwrap();
                if let PeerEvent::Revoked { token, .. } = ev {
                    *line = serde_json::to_string(&PeerEvent::Revoked {
                        token,
                        revoked_at: 50,
                    })
                    .unwrap();
                }
            }
        }
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        // Reopen so the in-memory state matches the rewritten file.
        let r2 = JsonlPeerRegistry::open(path.clone()).await.unwrap();
        let purged = r2.purge_revoked_older_than(100).await.unwrap();
        assert_eq!(purged, 1);

        // Reopen again — only the live entry survived.
        let r3 = JsonlPeerRegistry::open(path.clone()).await.unwrap();
        assert_eq!(
            r3.resolve(&live_token).await.unwrap(),
            Some(live_entry.agent_id)
        );
        assert_eq!(r3.resolve(&dead_token).await.unwrap(), None);
        let recent = r3.recent(10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].token, live_token);

        // No tempfile.tmp left lying around.
        assert!(!path.with_extension("jsonl.tmp").exists());
    }

    #[tokio::test]
    async fn jsonl_purge_no_op_when_no_revocations_match() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.jsonl");
        let r = JsonlPeerRegistry::open(path.clone()).await.unwrap();
        let (token, e) = entry("a@local");
        r.register(e).await.unwrap();
        r.revoke(&token).await.unwrap();

        // Fresh revocation — `before_ms = 100` matches nothing.
        let purged = r.purge_revoked_older_than(100).await.unwrap();
        assert_eq!(purged, 0);
        // No tempfile.tmp left behind.
        assert!(!path.with_extension("jsonl.tmp").exists());
        // Tombstone still on disk and resolves correctly.
        assert_eq!(r.resolve(&token).await.unwrap(), None);
    }

    #[tokio::test]
    async fn jsonl_purge_concurrent_resolve_never_returns_purged_token() {
        // Regression test for the Sprint 55 mid-sprint security-review
        // MEDIUM finding: between the two in-memory mutation blocks of
        // `purge_revoked_older_than`, a concurrent `resolve()` of a
        // recently-purged token used to observe `not in revoked` AND
        // `entry in entries` simultaneously — authenticating a token
        // whose tombstone was just dropped. Fix: mutate `entries` first
        // so the intermediate state keeps the tombstone visible.
        //
        // This stress test fires many concurrent resolves while the
        // purge runs; with the fix, every resolve must return None.
        // Without the fix, the race surfaced reliably under tokio's
        // task scheduler within a few hundred iterations.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.jsonl");
        let r = std::sync::Arc::new(JsonlPeerRegistry::open(path.clone()).await.unwrap());

        let (token, e) = entry("dead@local");
        r.register(e).await.unwrap();
        r.revoke(&token).await.unwrap();
        // Force the in-memory revocation timestamp into the past so the
        // purge picks it up.
        r.revoked.lock().await.insert(*token.as_bytes(), 50);

        // Spawn a small army of concurrent resolves *during* the purge.
        let r_resolve = r.clone();
        let resolve_handle = tokio::spawn(async move {
            let mut max_some_seen = 0usize;
            for _ in 0..2000 {
                if r_resolve.resolve(&token).await.unwrap().is_some() {
                    max_some_seen += 1;
                }
                tokio::task::yield_now().await;
            }
            max_some_seen
        });

        let _ = r.purge_revoked_older_than(100).await.unwrap();
        let some_count = resolve_handle.await.unwrap();
        assert_eq!(
            some_count, 0,
            "no concurrent resolve should ever authenticate a purged token"
        );
    }

    #[tokio::test]
    async fn jsonl_purge_replay_yields_same_state_as_before() {
        // Replay-equivalence: after compact, reopening the registry
        // must yield an identical resolve/recent surface vs. the
        // pre-compact state for every live token.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.jsonl");

        let (live_a, e_a) = entry("a@local");
        let (live_b, e_b) = entry("b@local");
        let (dead, e_dead) = entry("dead@local");

        let snapshot_pre = {
            let r = JsonlPeerRegistry::open(path.clone()).await.unwrap();
            r.register(e_a.clone()).await.unwrap();
            r.register(e_b.clone()).await.unwrap();
            r.register(e_dead.clone()).await.unwrap();
            r.revoke(&dead).await.unwrap();
            (
                r.resolve(&live_a).await.unwrap(),
                r.resolve(&live_b).await.unwrap(),
                r.resolve(&dead).await.unwrap(),
                r.recent(10).await.unwrap().len(),
            )
        };

        // Hand-stamp the dead-token revocation timestamp into the past,
        // then compact via a fresh handle.
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = raw.lines().map(String::from).collect();
        for line in lines.iter_mut() {
            if line.contains("\"revoked\"") {
                let ev: PeerEvent = serde_json::from_str(line).unwrap();
                if let PeerEvent::Revoked { token, .. } = ev {
                    *line = serde_json::to_string(&PeerEvent::Revoked {
                        token,
                        revoked_at: 50,
                    })
                    .unwrap();
                }
            }
        }
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let r_compact = JsonlPeerRegistry::open(path.clone()).await.unwrap();
        r_compact.purge_revoked_older_than(100).await.unwrap();

        // Snapshot the post-compact state from yet another fresh handle
        // — the operator's restart-after-compact path.
        let r_post = JsonlPeerRegistry::open(path).await.unwrap();
        let snapshot_post = (
            r_post.resolve(&live_a).await.unwrap(),
            r_post.resolve(&live_b).await.unwrap(),
            r_post.resolve(&dead).await.unwrap(),
            r_post.recent(10).await.unwrap().len(),
        );
        assert_eq!(snapshot_pre, snapshot_post);
    }
}
