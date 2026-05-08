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

/// Read-only, redacted view of a registry record. Crosses IPC/HTTP
/// wires; **never** carries the full `PeerToken` (a peer's secret).
/// `token_prefix` is the 6-char base58 prefix matching `PeerToken::Debug`
/// redaction so an operator can correlate a summary row with grep'd
/// debug logs without ever recovering the rest of the secret.
/// `revoked_at` is `Some(ts)` for tombstoned entries — kept on purpose
/// so post-incident triage can answer "is this audit-flagged peer still
/// live?" in one look. Sprint 62.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerSummary {
    pub agent_id: AgentId,
    pub token_prefix: String,
    pub registered_at: u64,
    pub revoked_at: Option<u64>,
}

/// Outcome of [`PeerRegistry::revoke_by_token_prefix`]. The operator
/// runs `peers list --prefix <pubkey>` to find a registry entry, copies
/// the 6-char `token_prefix`, then runs `peers revoke <prefix>`. The
/// prefix matches against `entry.token.to_b58().starts_with(prefix)`
/// (the full base58 of the token, not just the redacted 6-char view) so
/// supplying any number of leading characters works; longer is more
/// specific. Sprint 65.
///
/// Carries [`PeerSummary`] (not [`PeerEntry`]) on the wire so token
/// bytes never leak — same invariant as [`Response::PeerList`].
///
/// Sprint 69 added [`SelfRevokeForbidden`] for the daemon-side guard
/// against revoking the operator's own bootstrap token. The variant is
/// produced by `Server::revoke_peer` (not the registry trait) so the
/// storage layer stays peer-agnostic; the daemon peeks via
/// [`PeerRegistry::find_unique_live_by_token_prefix`] before committing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RevokeOutcome {
    /// Exactly one entry matched; it was live; it is now revoked.
    /// `summary.revoked_at` carries the moment of revocation.
    Revoked(PeerSummary),
    /// Exactly one entry matched but it was already in the revoked map.
    /// Idempotent — the operator's intent ("ensure this is revoked") is
    /// satisfied. `summary.revoked_at` carries the *original* timestamp,
    /// which is informative for incident postmortems.
    AlreadyRevoked(PeerSummary),
    /// No entry's full base58 token matched the supplied prefix.
    NotFound,
    /// More than one entry matched. The operator narrows by re-running
    /// with a longer prefix. Each [`PeerSummary`] carries its current
    /// `revoked_at` so the operator can see live-vs-tombstoned at a
    /// glance. The registry is unchanged.
    Ambiguous { matches: Vec<PeerSummary> },
    /// The unique live match is the operator's own bootstrap row and
    /// the request did not pass `force: true`. The registry is unchanged.
    /// Sprint 69 — defence-in-depth across IPC + HTTP + CLI against the
    /// "fat-finger via web UI bypassed by curl" failure mode flagged in
    /// Sprint 66's EFM #1; pairs with Sprint 67's UI-only guard.
    /// `summary.revoked_at` is `None` (the entry remained live).
    SelfRevokeForbidden(PeerSummary),
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
    /// Operator-triage view, newest-first up to `limit`. Includes both
    /// live and revoked entries so the operator can answer "is this
    /// pubkey already revoked?" from a single read; `revoked_at`
    /// distinguishes them. `pubkey_prefix` filters server-side on
    /// `bs58::encode(agent_id.pubkey)` — the same string that
    /// [`AuditKind::OperatorTokenRotationRejected.peer_pubkey_b58`]
    /// records, so an operator can paste the audit row's b58 directly.
    /// Empty/`None` prefix means "no filter". Sprint 62.
    async fn list_summaries(
        &self,
        limit: usize,
        pubkey_prefix: Option<&str>,
    ) -> Result<Vec<PeerSummary>, PeerError>;
    /// Drop revocation tombstones with `revoked_at < before_ms` along
    /// with their matching `Registered` entries. Returns the number of
    /// revocations dropped (= number of `Registered` entries also
    /// dropped, modulo any pre-existing orphaned revocations — those
    /// drop too). Live entries (registered but never revoked) are
    /// untouched. Mirrors `CapabilityStore::purge_revoked_older_than`.
    async fn purge_revoked_older_than(&self, before_ms: u64) -> Result<u64, PeerError>;
    /// Find the registry entry whose full base58 token starts with
    /// `prefix`, then either tombstone it or report what stopped the
    /// revocation (no match, ambiguous, already revoked). Operator
    /// triage flow: paste the `token_prefix` from `peers list` output.
    /// Sprint 65.
    ///
    /// Match semantics: `entry.token.to_b58().starts_with(prefix)`. A
    /// 6-char prefix matches what the operator copy-pastes from `peers
    /// list`; a full b58 (the operator's own freshly-rotated token, e.g.)
    /// is a strict subset of the same predicate. An empty prefix is the
    /// caller's bug — the daemon-side `revoke_peer` rejects it before
    /// it reaches this method, but the registry-side behaviour would be
    /// `Ambiguous { matches: <every entry> }` if it did.
    async fn revoke_by_token_prefix(&self, prefix: &str) -> Result<RevokeOutcome, PeerError>;

    /// Read-only peek used by the daemon's Sprint 69 self-revoke guard.
    /// Returns `Ok(Some(summary))` only when exactly one entry matches
    /// `prefix` AND that entry is currently live (not tombstoned).
    /// Returns `Ok(None)` for no-match, ambiguous-multi, or
    /// matches-only-revoked. The returned summary's `revoked_at` is
    /// always `None` (live by construction).
    ///
    /// The daemon uses this to decide whether the unique live match is
    /// the operator's own bootstrap row before deciding whether to
    /// short-circuit with `SelfRevokeForbidden` or fall through to
    /// [`Self::revoke_by_token_prefix`]. Keeping this method peer-agnostic
    /// (it does not know about caller identity) preserves the storage
    /// layer's separation from daemon concerns; the identity comparison
    /// happens at the `Server` boundary.
    ///
    /// TOCTOU: between this peek and a subsequent revoke, another caller
    /// could tombstone or register a colliding-prefix entry. Both races
    /// are benign — the subsequent `revoke_by_token_prefix` will return
    /// `AlreadyRevoked` or `Ambiguous` accordingly, and the operator's
    /// authoritative pubkey (`Server::identity`) does not change across
    /// token rotation (Sprint 60 rotates the token, not the keypair).
    async fn find_unique_live_by_token_prefix(
        &self,
        prefix: &str,
    ) -> Result<Option<PeerSummary>, PeerError>;
}

/// 6-char base58 prefix of `token`. Same redaction posture as
/// `PeerToken::Debug` and the audit log's `*_token_prefix` fields.
fn token_b58_prefix(token: &PeerToken) -> String {
    let s = token.to_b58();
    s.chars().take(6).collect()
}

fn summary_from(entry: &PeerEntry, revoked_at: Option<u64>) -> PeerSummary {
    PeerSummary {
        agent_id: entry.agent_id.clone(),
        token_prefix: token_b58_prefix(&entry.token),
        registered_at: entry.registered_at,
        revoked_at,
    }
}

fn summary_matches(s: &PeerSummary, prefix: Option<&str>) -> bool {
    match prefix {
        None | Some("") => true,
        Some(p) => s.agent_id.pubkey_base58().starts_with(p),
    }
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

    async fn list_summaries(
        &self,
        limit: usize,
        pubkey_prefix: Option<&str>,
    ) -> Result<Vec<PeerSummary>, PeerError> {
        let entries = self.entries.lock().await;
        let revoked = self.revoked.lock().await;
        Ok(entries
            .iter()
            .rev()
            .map(|e| summary_from(e, revoked.get(e.token.as_bytes()).copied()))
            .filter(|s| summary_matches(s, pubkey_prefix))
            .take(limit)
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

    async fn revoke_by_token_prefix(&self, prefix: &str) -> Result<RevokeOutcome, PeerError> {
        let entries = self.entries.lock().await;
        let mut revoked = self.revoked.lock().await;
        let matched: Vec<&PeerEntry> = entries
            .iter()
            .filter(|e| e.token.to_b58().starts_with(prefix))
            .collect();
        if matched.is_empty() {
            return Ok(RevokeOutcome::NotFound);
        }
        if matched.len() > 1 {
            let summaries = matched
                .iter()
                .map(|e| summary_from(e, revoked.get(e.token.as_bytes()).copied()))
                .collect();
            return Ok(RevokeOutcome::Ambiguous { matches: summaries });
        }
        let entry = matched[0];
        if let Some(rev_at) = revoked.get(entry.token.as_bytes()).copied() {
            return Ok(RevokeOutcome::AlreadyRevoked(summary_from(
                entry,
                Some(rev_at),
            )));
        }
        let revoked_at = epoch_ms();
        revoked.insert(*entry.token.as_bytes(), revoked_at);
        Ok(RevokeOutcome::Revoked(summary_from(
            entry,
            Some(revoked_at),
        )))
    }

    async fn find_unique_live_by_token_prefix(
        &self,
        prefix: &str,
    ) -> Result<Option<PeerSummary>, PeerError> {
        let entries = self.entries.lock().await;
        let revoked = self.revoked.lock().await;
        let matched: Vec<&PeerEntry> = entries
            .iter()
            .filter(|e| e.token.to_b58().starts_with(prefix))
            .collect();
        if matched.len() != 1 {
            return Ok(None);
        }
        let entry = matched[0];
        if revoked.contains_key(entry.token.as_bytes()) {
            return Ok(None);
        }
        Ok(Some(summary_from(entry, None)))
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

    async fn list_summaries(
        &self,
        limit: usize,
        pubkey_prefix: Option<&str>,
    ) -> Result<Vec<PeerSummary>, PeerError> {
        let entries = self.entries.lock().await;
        let revoked = self.revoked.lock().await;
        Ok(entries
            .iter()
            .rev()
            .map(|e| summary_from(e, revoked.get(e.token.as_bytes()).copied()))
            .filter(|s| summary_matches(s, pubkey_prefix))
            .take(limit)
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

    async fn revoke_by_token_prefix(&self, prefix: &str) -> Result<RevokeOutcome, PeerError> {
        let _g = self.file_lock.lock().await;
        // Snapshot under both inner locks; release before file IO so
        // resolve() / list_summaries can run during the append.
        let entry_to_revoke = {
            let entries = self.entries.lock().await;
            let revoked = self.revoked.lock().await;
            let matched: Vec<&PeerEntry> = entries
                .iter()
                .filter(|e| e.token.to_b58().starts_with(prefix))
                .collect();
            if matched.is_empty() {
                return Ok(RevokeOutcome::NotFound);
            }
            if matched.len() > 1 {
                let summaries = matched
                    .iter()
                    .map(|e| summary_from(e, revoked.get(e.token.as_bytes()).copied()))
                    .collect();
                return Ok(RevokeOutcome::Ambiguous { matches: summaries });
            }
            let entry = matched[0];
            if let Some(rev_at) = revoked.get(entry.token.as_bytes()).copied() {
                return Ok(RevokeOutcome::AlreadyRevoked(summary_from(
                    entry,
                    Some(rev_at),
                )));
            }
            entry.clone()
        };
        let revoked_at = epoch_ms();
        self.append(&PeerEvent::Revoked {
            token: entry_to_revoke.token,
            revoked_at,
        })
        .await?;
        self.revoked
            .lock()
            .await
            .insert(*entry_to_revoke.token.as_bytes(), revoked_at);
        Ok(RevokeOutcome::Revoked(summary_from(
            &entry_to_revoke,
            Some(revoked_at),
        )))
    }

    async fn find_unique_live_by_token_prefix(
        &self,
        prefix: &str,
    ) -> Result<Option<PeerSummary>, PeerError> {
        let entries = self.entries.lock().await;
        let revoked = self.revoked.lock().await;
        let matched: Vec<&PeerEntry> = entries
            .iter()
            .filter(|e| e.token.to_b58().starts_with(prefix))
            .collect();
        if matched.len() != 1 {
            return Ok(None);
        }
        let entry = matched[0];
        if revoked.contains_key(entry.token.as_bytes()) {
            return Ok(None);
        }
        Ok(Some(summary_from(entry, None)))
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

    fn entry_with_pubkey(name: &str, pubkey_byte0: u8) -> (PeerToken, PeerEntry) {
        let mut pubkey = [0u8; 32];
        pubkey[0] = pubkey_byte0;
        let token = PeerToken::generate();
        let entry = PeerEntry {
            token,
            agent_id: AgentId::new(name, pubkey),
            registered_at: 1_700_000_000_000,
        };
        (token, entry)
    }

    #[tokio::test]
    async fn list_summaries_includes_revoked_with_timestamp() {
        let r = InMemoryPeerRegistry::new();
        let (live_tok, live_ent) = entry("alice@local");
        let (dead_tok, dead_ent) = entry("bob@local");
        r.register(live_ent).await.unwrap();
        r.register(dead_ent).await.unwrap();
        r.revoke(&dead_tok).await.unwrap();

        let s = r.list_summaries(10, None).await.unwrap();
        assert_eq!(s.len(), 2, "live and revoked both surface");
        let dead_summary = s
            .iter()
            .find(|x| x.agent_id.display == "bob@local")
            .expect("revoked entry surfaces");
        assert!(
            dead_summary.revoked_at.is_some(),
            "revoked entry carries timestamp"
        );
        assert_eq!(dead_summary.token_prefix.len(), 6);
        assert_eq!(dead_summary.token_prefix, &dead_tok.to_b58()[..6]);
        let live_summary = s
            .iter()
            .find(|x| x.agent_id.display == "alice@local")
            .expect("live entry surfaces");
        assert!(live_summary.revoked_at.is_none());
        assert_eq!(live_summary.token_prefix, &live_tok.to_b58()[..6]);
    }

    #[tokio::test]
    async fn list_summaries_orders_newest_first() {
        let r = InMemoryPeerRegistry::new();
        let (_, e_old) = entry("old@local");
        let (_, e_mid) = entry("mid@local");
        let (_, e_new) = entry("new@local");
        r.register(e_old).await.unwrap();
        r.register(e_mid).await.unwrap();
        r.register(e_new).await.unwrap();

        let s = r.list_summaries(10, None).await.unwrap();
        let displays: Vec<&str> = s.iter().map(|x| x.agent_id.display.as_str()).collect();
        assert_eq!(
            displays,
            vec!["new@local", "mid@local", "old@local"],
            "register order reversed: newest first"
        );
    }

    #[tokio::test]
    async fn list_summaries_filters_on_pubkey_b58_prefix() {
        // Pick a pubkey-byte0 such that the b58 encoding is fully
        // determined by the byte (the rest are zero). bs58::encode
        // of `[0xff, 0, 0, ..., 0]` starts with a stable prefix.
        let r = InMemoryPeerRegistry::new();
        let (_, e_match) = entry_with_pubkey("match@local", 0xff);
        let (_, e_other) = entry_with_pubkey("other@local", 0x01);
        r.register(e_match.clone()).await.unwrap();
        r.register(e_other.clone()).await.unwrap();

        let target_b58 = bs58::encode(e_match.agent_id.pubkey).into_string();
        let prefix: String = target_b58.chars().take(4).collect();
        let s = r.list_summaries(10, Some(&prefix)).await.unwrap();
        assert_eq!(s.len(), 1, "only the matching pubkey surfaces");
        assert_eq!(s[0].agent_id.display, "match@local");

        let none = r.list_summaries(10, Some("zzzzzz")).await.unwrap();
        assert!(none.is_empty(), "non-matching prefix returns empty");

        let all = r.list_summaries(10, Some("")).await.unwrap();
        assert_eq!(all.len(), 2, "empty prefix is no filter");
    }

    #[tokio::test]
    async fn list_summaries_never_emits_full_token_b58() {
        let r = InMemoryPeerRegistry::new();
        let (tok, ent) = entry("p@local");
        r.register(ent).await.unwrap();
        let s = r.list_summaries(10, None).await.unwrap();
        let json = serde_json::to_string(&s).unwrap();
        let full_b58 = tok.to_b58();
        assert!(
            !json.contains(&full_b58),
            "wire form must never carry the full token"
        );
        assert!(json.contains(&full_b58[..6]), "6-char prefix is fine");
    }

    #[tokio::test]
    async fn jsonl_list_summaries_replays_revoked_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.jsonl");
        let (live_tok, live_ent) = entry("live@local");
        let (dead_tok, dead_ent) = entry("dead@local");
        {
            let r = JsonlPeerRegistry::open(path.clone()).await.unwrap();
            r.register(live_ent).await.unwrap();
            r.register(dead_ent).await.unwrap();
            r.revoke(&dead_tok).await.unwrap();
        }

        let r2 = JsonlPeerRegistry::open(path).await.unwrap();
        let s = r2.list_summaries(10, None).await.unwrap();
        assert_eq!(s.len(), 2);
        let dead = s
            .iter()
            .find(|x| x.token_prefix == dead_tok.to_b58()[..6])
            .expect("revoked entry replayed");
        assert!(dead.revoked_at.is_some());
        let live = s
            .iter()
            .find(|x| x.token_prefix == live_tok.to_b58()[..6])
            .expect("live entry replayed");
        assert!(live.revoked_at.is_none());
    }

    #[tokio::test]
    async fn revoke_by_token_prefix_revokes_unique_match() {
        let r = InMemoryPeerRegistry::new();
        let (token, ent) = entry("alice@local");
        r.register(ent.clone()).await.unwrap();
        let prefix: String = token.to_b58().chars().take(6).collect();
        let outcome = r.revoke_by_token_prefix(&prefix).await.unwrap();
        match outcome {
            RevokeOutcome::Revoked(s) => {
                assert_eq!(s.agent_id, ent.agent_id);
                assert!(s.revoked_at.is_some());
                assert_eq!(s.token_prefix, &token.to_b58()[..6]);
            }
            other => panic!("expected Revoked, got {other:?}"),
        }
        // Post-revoke: the token does not resolve.
        assert_eq!(r.resolve(&token).await.unwrap(), None);
    }

    #[tokio::test]
    async fn revoke_by_token_prefix_returns_not_found_for_unknown_prefix() {
        let r = InMemoryPeerRegistry::new();
        let outcome = r.revoke_by_token_prefix("zzzzzzzz").await.unwrap();
        assert!(matches!(outcome, RevokeOutcome::NotFound));
    }

    #[tokio::test]
    async fn revoke_by_token_prefix_returns_already_revoked_for_revoked_match() {
        let r = InMemoryPeerRegistry::new();
        let (token, ent) = entry("dead@local");
        r.register(ent).await.unwrap();
        assert!(r.revoke(&token).await.unwrap());
        let original_ts = *r.revoked.lock().await.get(token.as_bytes()).unwrap();

        let prefix: String = token.to_b58().chars().take(6).collect();
        let outcome = r.revoke_by_token_prefix(&prefix).await.unwrap();
        match outcome {
            RevokeOutcome::AlreadyRevoked(s) => {
                assert_eq!(s.revoked_at, Some(original_ts));
                assert_eq!(s.token_prefix, &token.to_b58()[..6]);
            }
            other => panic!("expected AlreadyRevoked, got {other:?}"),
        }
    }

    /// Generate a `PeerEntry` whose `token.to_b58()` starts with the
    /// supplied prefix. Random-rejection sampling — converges in ~58
    /// iterations per leading char of base58. Used by tests that need
    /// two tokens with a deterministic shared prefix to drive the
    /// `Ambiguous` outcome.
    fn entry_with_token_b58_starting_with(prefix: &str, name: &str) -> (PeerToken, PeerEntry) {
        for _ in 0..10_000 {
            let t = PeerToken::generate();
            if t.to_b58().starts_with(prefix) {
                let ent = PeerEntry {
                    token: t,
                    agent_id: AgentId::new(name, [0u8; 32]),
                    registered_at: 1_700_000_000_000,
                };
                return (t, ent);
            }
        }
        panic!("could not find token starting with {prefix:?} after 10000 tries");
    }

    #[tokio::test]
    async fn revoke_by_token_prefix_returns_ambiguous_for_multiple_matches() {
        let r = InMemoryPeerRegistry::new();
        let (t1, e1) = entry_with_token_b58_starting_with("1", "a@local");
        let (t2, e2) = entry_with_token_b58_starting_with("1", "b@local");
        r.register(e1).await.unwrap();
        r.register(e2).await.unwrap();
        let outcome = r.revoke_by_token_prefix("1").await.unwrap();
        match outcome {
            RevokeOutcome::Ambiguous { matches } => {
                assert_eq!(matches.len(), 2);
                // Neither tombstoned.
                assert!(matches.iter().all(|s| s.revoked_at.is_none()));
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
        // Registry unchanged — both still resolve.
        assert!(r.resolve(&t1).await.unwrap().is_some());
        assert!(r.resolve(&t2).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn revoke_by_token_prefix_outcome_never_serializes_full_token_b58() {
        let r = InMemoryPeerRegistry::new();
        let (token, ent) = entry("p@local");
        r.register(ent).await.unwrap();
        let prefix: String = token.to_b58().chars().take(6).collect();
        let outcome = r.revoke_by_token_prefix(&prefix).await.unwrap();
        let json = serde_json::to_string(&outcome).unwrap();
        let full_b58 = token.to_b58();
        assert!(
            !json.contains(&full_b58),
            "wire form must never carry the full token"
        );
        assert!(json.contains(&full_b58[..6]), "6-char prefix is fine");
    }

    #[tokio::test]
    async fn jsonl_revoke_by_token_prefix_persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.jsonl");
        let (token, ent) = entry("p@local");
        {
            let r = JsonlPeerRegistry::open(path.clone()).await.unwrap();
            r.register(ent).await.unwrap();
            let prefix: String = token.to_b58().chars().take(6).collect();
            let outcome = r.revoke_by_token_prefix(&prefix).await.unwrap();
            assert!(matches!(outcome, RevokeOutcome::Revoked(_)));
        }
        let r2 = JsonlPeerRegistry::open(path).await.unwrap();
        assert_eq!(r2.resolve(&token).await.unwrap(), None);
        // The summary list still surfaces the revoked entry.
        let s = r2.list_summaries(10, None).await.unwrap();
        assert_eq!(s.len(), 1);
        assert!(s[0].revoked_at.is_some());
    }

    #[tokio::test]
    async fn find_unique_live_returns_summary_for_unique_live_match() {
        let r = InMemoryPeerRegistry::new();
        let (token, ent) = entry("solo@local");
        r.register(ent.clone()).await.unwrap();
        let prefix: String = token.to_b58().chars().take(6).collect();
        let s = r
            .find_unique_live_by_token_prefix(&prefix)
            .await
            .unwrap()
            .expect("unique live match");
        assert_eq!(s.agent_id, ent.agent_id);
        assert_eq!(s.token_prefix, &token.to_b58()[..6]);
        assert!(s.revoked_at.is_none());
    }

    #[tokio::test]
    async fn find_unique_live_returns_none_for_no_match() {
        let r = InMemoryPeerRegistry::new();
        assert!(r
            .find_unique_live_by_token_prefix("zzzzzzzz")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn find_unique_live_returns_none_for_revoked_match() {
        let r = InMemoryPeerRegistry::new();
        let (token, ent) = entry("dead@local");
        r.register(ent).await.unwrap();
        assert!(r.revoke(&token).await.unwrap());
        let prefix: String = token.to_b58().chars().take(6).collect();
        assert!(r
            .find_unique_live_by_token_prefix(&prefix)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn find_unique_live_returns_none_for_ambiguous_matches() {
        let r = InMemoryPeerRegistry::new();
        let (_, e1) = entry_with_token_b58_starting_with("1", "a@local");
        let (_, e2) = entry_with_token_b58_starting_with("1", "b@local");
        r.register(e1).await.unwrap();
        r.register(e2).await.unwrap();
        assert!(r
            .find_unique_live_by_token_prefix("1")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn find_unique_live_does_not_mutate_registry() {
        let r = InMemoryPeerRegistry::new();
        let (token, ent) = entry("snapshot@local");
        r.register(ent).await.unwrap();
        let prefix: String = token.to_b58().chars().take(6).collect();
        let _ = r.find_unique_live_by_token_prefix(&prefix).await.unwrap();
        assert!(r.resolve(&token).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn jsonl_find_unique_live_returns_summary_for_unique_live_match() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.jsonl");
        let r = JsonlPeerRegistry::open(path).await.unwrap();
        let (token, ent) = entry("solo@local");
        r.register(ent.clone()).await.unwrap();
        let prefix: String = token.to_b58().chars().take(6).collect();
        let s = r
            .find_unique_live_by_token_prefix(&prefix)
            .await
            .unwrap()
            .expect("unique live match");
        assert_eq!(s.agent_id, ent.agent_id);
        assert!(s.revoked_at.is_none());
    }
}
