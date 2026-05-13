//! Peer-token registry — binds the calling peer (Unix-socket or HTTP)
//! to an [`AgentId`] so the daemon's capability checks attribute the
//! request to the actual caller, not to a self-asserted wire field.
//!
//! The threat this closes: `a2a.respond.<sender>` is a sender-scoped
//! capability whose `<sender>` shape is validated against the
//! capability grammar, but neither check verifies that the peer
//! making the request *is* that sender. A malicious local process
//! can connect to the daemon's Unix socket and claim to be agent X.
//! The registry adds a random 32-byte token per registered peer; the
//! daemon resolves `token → AgentId` at the start of every connection
//! (or every HTTP request), and uses that resolved `AgentId` as the
//! capability subject.
//!
//! Two storage backends implement [`PeerRegistry`]:
//! [`JsonlPeerRegistry`] for production (event log replays on
//! `open()`), and [`InMemoryPeerRegistry`] for tests. Both honour
//! revocation tombstones written via [`PeerRegistry::revoke`].

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

/// 32-byte opaque peer token. Equality is constant-time via
/// [`subtle::ConstantTimeEq`] so a linear scan over the peer registry
/// does not leak token bytes through response-time correlation. Hash is
/// not derived because the registry does not use `HashMap<PeerToken,_>`
/// for lookups — the `find` over `Vec<PeerEntry>` is the only path, and
/// it now compares each entry in constant time.
#[derive(Clone, Copy, Eq)]
pub struct PeerToken([u8; 32]);

impl PartialEq for PeerToken {
    fn eq(&self, other: &Self) -> bool {
        use subtle::ConstantTimeEq;
        self.0.ct_eq(&other.0).into()
    }
}

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
/// live?" in one look.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerSummary {
    pub agent_id: AgentId,
    pub token_prefix: String,
    pub registered_at: u64,
    pub revoked_at: Option<u64>,
}

/// Filter applied to [`PeerRegistry::list_summaries`]. `None` (the
/// wire default for [`Request::ListPeers.status_filter`]) means "no
/// filter" — both live and revoked rows surface; this preserves the
/// pre-filter behaviour for stale clients that omit the field. The
/// two explicit variants narrow the result to a single status so an
/// operator triaging an incident can drop the noise of the other
/// half. Compares on `revoked_at: Option<u64>` — `None` is live,
/// `Some(_)` is revoked. The filter runs *before* the registry's
/// `take(limit + 1)` peek so the resulting `truncated` flag reflects
/// truncation among the filtered rows, not the full registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerStatusFilter {
    Live,
    Revoked,
}

/// Outcome of [`PeerRegistry::revoke_by_token_prefix`]. The operator
/// runs `peers list --prefix <pubkey>` to find a registry entry, copies
/// the 6-char `token_prefix`, then runs `peers revoke <prefix>`. The
/// prefix matches against `entry.token.to_b58().starts_with(prefix)`
/// (the full base58 of the token, not just the redacted 6-char view) so
/// supplying any number of leading characters works; longer is more
/// specific.
///
/// Carries [`PeerSummary`] (not [`PeerEntry`]) on the wire so token
/// bytes never leak — same invariant as [`Response::PeerList`].
///
/// [`SelfRevokeForbidden`] is the daemon-side guard against revoking
/// the operator's own bootstrap token. The variant is produced by
/// `Server::revoke_peer` (not the registry trait) so the storage
/// layer stays peer-agnostic; the daemon peeks via
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
    ///
    /// `matches.len()` is bounded by the `limit` argument to
    /// [`PeerRegistry::revoke_by_token_prefix`]. `truncated` is `true`
    /// when more than `limit` entries matched the prefix; the operator
    /// then knows the displayed list is incomplete and that a longer
    /// prefix is needed to narrow further. `#[serde(default)]` so a
    /// stale CLI built before the field landed deserialises a new
    /// daemon's response (the field reads as `false`, which degrades to
    /// the pre-bound behaviour where the operator assumes the displayed
    /// matches are exhaustive).
    Ambiguous {
        matches: Vec<PeerSummary>,
        #[serde(default)]
        truncated: bool,
    },
    /// The unique live match is the operator's own bootstrap row and
    /// the request did not pass `force: true`. The registry is unchanged.
    /// Defence-in-depth across IPC + HTTP + CLI against the
    /// "fat-finger via web UI bypassed by curl" failure mode where a
    /// UI-only confirmation guard is trivially circumvented by a
    /// direct daemon API call; pairs with the UI-side confirmation
    /// prompt. `summary.revoked_at` is `None` (the entry remained live).
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
    /// Operator-triage view, newest-first up to `limit`. Defaults to
    /// both live and revoked entries so the operator can answer "is
    /// this pubkey already revoked?" from a single read; `revoked_at`
    /// distinguishes them. `pubkey_prefix` filters server-side on
    /// `bs58::encode(agent_id.pubkey)` — the same string that
    /// [`AuditKind::OperatorTokenRotationRejected.peer_pubkey_b58`]
    /// records, so an operator can paste the audit row's b58 directly.
    /// Empty/`None` prefix means "no prefix filter".
    ///
    /// `status_filter` narrows by liveness — `None` (the default) keeps
    /// both halves; [`PeerStatusFilter::Live`] drops every row whose
    /// `revoked_at` is `Some(_)`; [`PeerStatusFilter::Revoked`] drops
    /// every row whose `revoked_at` is `None`. The filter runs before
    /// `take(limit + 1)` so the returned `truncated` reflects truncation
    /// among the filtered rows.
    ///
    /// Returns `(rows, truncated)`. `rows.len() <= limit`; `truncated`
    /// is `true` when more matches existed than `limit` allowed. The
    /// implementation peeks one entry past `limit` so the cost is
    /// O(limit), not O(N), even on a registry with thousands of rows.
    async fn list_summaries(
        &self,
        limit: usize,
        pubkey_prefix: Option<&str>,
        status_filter: Option<PeerStatusFilter>,
    ) -> Result<(Vec<PeerSummary>, bool), PeerError>;
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
    ///
    /// Match semantics: `entry.token.to_b58().starts_with(prefix)`. A
    /// 6-char prefix matches what the operator copy-pastes from `peers
    /// list`; a full b58 (the operator's own freshly-rotated token, e.g.)
    /// is a strict subset of the same predicate. An empty prefix is the
    /// caller's bug — the daemon-side `revoke_peer` rejects it before
    /// it reaches this method, but the registry-side behaviour would be
    /// `Ambiguous { matches: <up to `limit` entries>, truncated: true }`
    /// if it did.
    ///
    /// `limit` caps the `Ambiguous.matches` payload. The implementation
    /// peeks one entry past `limit`; if more than `limit` matched, the
    /// returned `Ambiguous { truncated: true }` carries exactly `limit`
    /// summaries and the operator narrows by re-running with a longer
    /// prefix. The cap is the caller's choice — the daemon passes a
    /// constant today; a future CLI flag (`--limit-matches`) routes
    /// through the same parameter.
    async fn revoke_by_token_prefix(
        &self,
        prefix: &str,
        limit: usize,
    ) -> Result<RevokeOutcome, PeerError>;

    /// Read-only peek used by the daemon's self-revoke guard.
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
    /// token rotation (the rotation rotates the token, not the keypair).
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

fn summary_passes_status(s: &PeerSummary, filter: Option<PeerStatusFilter>) -> bool {
    match filter {
        None => true,
        Some(PeerStatusFilter::Live) => s.revoked_at.is_none(),
        Some(PeerStatusFilter::Revoked) => s.revoked_at.is_some(),
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
        status_filter: Option<PeerStatusFilter>,
    ) -> Result<(Vec<PeerSummary>, bool), PeerError> {
        let entries = self.entries.lock().await;
        let revoked = self.revoked.lock().await;
        let mut peeked: Vec<PeerSummary> = entries
            .iter()
            .rev()
            .map(|e| summary_from(e, revoked.get(e.token.as_bytes()).copied()))
            .filter(|s| summary_matches(s, pubkey_prefix))
            .filter(|s| summary_passes_status(s, status_filter))
            .take(limit + 1)
            .collect();
        let truncated = peeked.len() > limit;
        if truncated {
            peeked.truncate(limit);
        }
        Ok((peeked, truncated))
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

    async fn revoke_by_token_prefix(
        &self,
        prefix: &str,
        limit: usize,
    ) -> Result<RevokeOutcome, PeerError> {
        let entries = self.entries.lock().await;
        let mut revoked = self.revoked.lock().await;
        let mut matched: Vec<&PeerEntry> = entries
            .iter()
            .filter(|e| e.token.to_b58().starts_with(prefix))
            .take(limit + 1)
            .collect();
        if matched.is_empty() {
            return Ok(RevokeOutcome::NotFound);
        }
        if matched.len() > 1 {
            let truncated = matched.len() > limit;
            if truncated {
                matched.truncate(limit);
            }
            let summaries = matched
                .iter()
                .map(|e| summary_from(e, revoked.get(e.token.as_bytes()).copied()))
                .collect();
            return Ok(RevokeOutcome::Ambiguous {
                matches: summaries,
                truncated,
            });
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
        status_filter: Option<PeerStatusFilter>,
    ) -> Result<(Vec<PeerSummary>, bool), PeerError> {
        let entries = self.entries.lock().await;
        let revoked = self.revoked.lock().await;
        let mut peeked: Vec<PeerSummary> = entries
            .iter()
            .rev()
            .map(|e| summary_from(e, revoked.get(e.token.as_bytes()).copied()))
            .filter(|s| summary_matches(s, pubkey_prefix))
            .filter(|s| summary_passes_status(s, status_filter))
            .take(limit + 1)
            .collect();
        let truncated = peeked.len() > limit;
        if truncated {
            peeked.truncate(limit);
        }
        Ok((peeked, truncated))
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

    async fn revoke_by_token_prefix(
        &self,
        prefix: &str,
        limit: usize,
    ) -> Result<RevokeOutcome, PeerError> {
        let _g = self.file_lock.lock().await;
        // Snapshot under both inner locks; release before file IO so
        // resolve() / list_summaries can run during the append.
        let entry_to_revoke = {
            let entries = self.entries.lock().await;
            let revoked = self.revoked.lock().await;
            let mut matched: Vec<&PeerEntry> = entries
                .iter()
                .filter(|e| e.token.to_b58().starts_with(prefix))
                .take(limit + 1)
                .collect();
            if matched.is_empty() {
                return Ok(RevokeOutcome::NotFound);
            }
            if matched.len() > 1 {
                let truncated = matched.len() > limit;
                if truncated {
                    matched.truncate(limit);
                }
                let summaries = matched
                    .iter()
                    .map(|e| summary_from(e, revoked.get(e.token.as_bytes()).copied()))
                    .collect();
                return Ok(RevokeOutcome::Ambiguous {
                    matches: summaries,
                    truncated,
                });
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
    fn peer_summary_serde_pins_four_field_wire_form() {
        // PeerSummary is the read-only redacted peer-registry view that
        // crosses every IPC and HTTP wire: Response::PeerList carries
        // Vec<PeerSummary>, and RevokeOutcome::Revoked / AlreadyRevoked /
        // Ambiguous each wrap one or more summaries. The four-field
        // wire contract is load-bearing:
        //
        // * `agent_id` carries the nested AgentId {display, pubkey-b58}.
        // * `token_prefix` is the 6-char redaction the operator pastes
        //   from grep'd debug logs into `peers revoke`.
        // * `registered_at` is the registration timestamp.
        // * `revoked_at: Option<u64>` is None for live entries and
        //   Some(ts) for tombstoned ones; the absence of
        //   skip_serializing_if means the key is always present on the
        //   wire (null on the live path).
        //
        // No test pins this contract. A refactor that flattened the
        // struct, renamed token_prefix, or added skip_serializing_if to
        // revoked_at would silently shift every operator-triage and
        // revoke-outcome consumer.

        let mut pubkey = [0u8; 32];
        pubkey[0] = 0xAA;
        let live = PeerSummary {
            agent_id: AgentId::new("alice@host", pubkey),
            token_prefix: "abcdef".into(),
            registered_at: 1_000,
            revoked_at: None,
        };
        let wire = serde_json::to_value(&live).unwrap();
        let obj = wire
            .as_object()
            .expect("PeerSummary serialises as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["agent_id", "registered_at", "revoked_at", "token_prefix"],
            "PeerSummary wire object must always carry the four documented \
             fields; adding skip_serializing_if = Option::is_none on \
             revoked_at would silently drop the key on the live-peer path \
             and break every operator-triage consumer destructuring on the \
             four-key shape"
        );
        assert_eq!(
            obj.get("revoked_at"),
            Some(&serde_json::Value::Null),
            "live PeerSummary must surface revoked_at as JSON null on the wire \
             — the absence of skip_serializing_if is what keeps the four-key \
             shape stable across live and revoked states"
        );

        let agent_obj = obj
            .get("agent_id")
            .and_then(serde_json::Value::as_object)
            .expect("agent_id must serialise as a nested JSON object");
        let mut agent_keys: Vec<&str> = agent_obj.keys().map(String::as_str).collect();
        agent_keys.sort();
        assert_eq!(
            agent_keys,
            vec!["display", "pubkey"],
            "PeerSummary::agent_id must surface the AgentId display+pubkey \
             shape; flattening or restructuring AgentId here would break \
             every peer-auth consumer cross-binding to AgentId"
        );
        assert_eq!(
            agent_obj.get("display").and_then(serde_json::Value::as_str),
            Some("alice@host"),
        );
        assert_eq!(
            agent_obj.get("pubkey").and_then(serde_json::Value::as_str),
            Some(live.agent_id.pubkey_base58().as_str()),
            "PeerSummary::agent_id.pubkey must equal the AgentId's base58 \
             pubkey verbatim — the redaction invariant that no token bytes \
             leak depends on this cross-type binding staying stable"
        );

        let revoked = PeerSummary {
            agent_id: AgentId::new("alice@host", pubkey),
            token_prefix: "abcdef".into(),
            registered_at: 1_000,
            revoked_at: Some(2_000),
        };
        let revoked_wire = serde_json::to_value(&revoked).unwrap();
        assert_eq!(
            revoked_wire
                .get("revoked_at")
                .and_then(serde_json::Value::as_u64),
            Some(2_000),
            "Some revoked_at must round-trip verbatim on the wire"
        );

        let decoded: PeerSummary = serde_json::from_value(revoked_wire).unwrap();
        assert_eq!(
            decoded, revoked,
            "PeerSummary must round-trip through serde_json verbatim — the \
             Eq derive is the contract every redacted peer-list consumer \
             leans on"
        );

        let full_obj = serde_json::to_value(&live).unwrap();
        let full_map = full_obj.as_object().unwrap().clone();
        for required in ["agent_id", "token_prefix", "registered_at"] {
            let mut payload = full_map.clone();
            payload.remove(required);
            assert!(
                serde_json::from_value::<PeerSummary>(serde_json::Value::Object(payload)).is_err(),
                "PeerSummary must reject a wire payload that omits {required}; \
                 a stray #[serde(default)] introduction on a non-Option field \
                 would silently let a malformed redacted view decode"
            );
        }

        let no_revoked_at = serde_json::json!({
            "agent_id": {
                "display": "alice@host",
                "pubkey": live.agent_id.pubkey_base58(),
            },
            "token_prefix": "abcdef",
            "registered_at": 1_000,
        });
        let decoded: PeerSummary = serde_json::from_value(no_revoked_at).unwrap();
        assert_eq!(
            decoded.revoked_at, None,
            "PeerSummary with revoked_at omitted must decode as None — \
             serde's auto-default for Option<u64> is the forward-compatible \
             contract every stale CLI built before the field landed leans on"
        );
    }

    #[test]
    fn peer_token_serde_pins_base58_and_length_arms() {
        // PeerToken is the 32-byte opaque peer secret persisted to
        // peers/registry.jsonl through PeerEntry and re-rotated through
        // PeerEvent::Registered/Revoked. It ships a custom Serialize/
        // Deserialize that maps the [u8; 32] to a base58 JSON string and
        // a custom Debug that redacts to a 6-char base58 prefix.
        //
        // peer_entry_serde_pins_three_required_fields exercises the full
        // token wire form through a PeerEntry round-trip and the 43/44-
        // char length sanity, but no test pins PeerToken's wire shape in
        // isolation, the 32-byte length check on decode, the rejection
        // of non-base58 strings, or the rejection of a derive-Serialize
        // regression that surfaced the token as a JSON array of 32
        // numbers. Parallels signed_capability_signature_serde_pins_base58_and_length_arms.
        let token = PeerToken::from_bytes([7u8; 32]);

        let wire = serde_json::to_value(token).unwrap();
        let token_str = wire.as_str().expect(
            "PeerToken must serialise as a JSON string (custom Serialize \
             impl); a refactor that dropped the impl in favour of derived \
             Serialize on the inner [u8; 32] would emit a JSON array of 32 \
             numbers, brick every JSONL peer-registry row, and silently \
             lock out every authenticated peer on operator restart",
        );
        let expected_b58 = bs58::encode([7u8; 32]).into_string();
        assert_eq!(
            token_str, expected_b58,
            "PeerToken wire form must equal bs58::encode(token_bytes); a \
             refactor to base64 or hex would invalidate every persisted \
             registry row on operator restart"
        );
        let len = token_str.chars().count();
        assert!(
            len == 43 || len == 44,
            "PeerToken wire form must be the full 43-or-44-char base58 of \
             the 32-byte secret, got {len} chars — a collapse to the \
             6-char PeerSummary::token_prefix would mean the registry \
             could no longer authenticate peers because the prefix cannot \
             reconstruct the full token"
        );

        let serialised = serde_json::to_string(&token).unwrap();
        assert!(
            !serialised.contains("[7,7"),
            "serialised PeerToken must not contain a raw [u8; 32] byte \
             array; the contract is bs58 string encoding, not derived \
             array Serialize"
        );

        let back: PeerToken = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, token,
            "PeerToken must round-trip through serde_json verbatim — the \
             PartialEq impl is constant-time and the contract every \
             registry resolve/revoke path leans on"
        );

        let too_short = serde_json::Value::String(bs58::encode([0u8; 31]).into_string());
        assert!(
            serde_json::from_value::<PeerToken>(too_short).is_err(),
            "31-byte token wire form must be rejected; a dropped length \
             check would silently zero-pad to 32 bytes, alias against a \
             different token in the registry's constant-time PartialEq, \
             and authenticate the wrong peer"
        );

        let too_long = serde_json::Value::String(bs58::encode([0u8; 33]).into_string());
        assert!(
            serde_json::from_value::<PeerToken>(too_long).is_err(),
            "33-byte token wire form must be rejected so a future \
             length-permissive deserializer cannot silently accept padded \
             tokens that alias against shorter live entries"
        );

        let garbage = serde_json::Value::String("!!! not base58 !!!".into());
        assert!(
            serde_json::from_value::<PeerToken>(garbage).is_err(),
            "non-base58 string must be rejected so the decoder never \
             silently zero-fills an invalid registry row"
        );

        let array_form = serde_json::json!([
            7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
            7, 7, 7,
        ]);
        assert!(
            serde_json::from_value::<PeerToken>(array_form).is_err(),
            "JSON array of 32 numbers must be rejected — the custom \
             deserializer is string-based; accepting the array form would \
             silently absorb payloads emitted by a regressed derive-\
             Serialize refactor and let the two encodings diverge in the \
             wild"
        );

        let debug = format!("{token:?}");
        assert!(
            debug.starts_with("PeerToken("),
            "PeerToken Debug must wrap its redacted view in the type \
             name; a refactor that collapsed Debug onto the inner array \
             would dump 32 secret bytes into every audit/log line"
        );
        assert!(
            debug.contains('…'),
            "PeerToken Debug must redact the secret with an ellipsis; \
             dropping the redaction marker would let secret bytes leak \
             through every Debug formatter consumer"
        );
        assert!(
            !debug.contains(token_str),
            "PeerToken Debug output must NOT contain the full base58 wire \
             form; collapsing the two shapes would leak the secret in \
             every audit/log line that formatted a registry row, defeating \
             the redaction contract PeerSummary::token_prefix anchors"
        );
        let prefix = &token_str[..6];
        assert!(
            debug.contains(prefix),
            "PeerToken Debug must include the 6-char base58 prefix so \
             operator grep across audit logs and the wire form lines up — \
             a regression that swapped the prefix source (e.g., a hash of \
             the bytes instead of the base58) would break the peer-revoke \
             cross-reference"
        );
    }

    #[test]
    fn peer_entry_serde_pins_three_required_fields() {
        // PeerEntry is the persisted registry record (the JSONL row
        // JsonlPeerRegistry writes), distinct from PeerSummary which is
        // the redacted wire form crossing IPC/HTTP. Three strictly
        // required fields: token (PeerToken — the secret), agent_id, and
        // registered_at. No serde attributes; every field is required on
        // both sides. The token wire form is the full base58 of the
        // 32-byte secret, NOT the 6-char redacted prefix that
        // PeerSummary::token_prefix carries — collapsing the two shapes
        // would leak the secret-carrying capability.
        let token = PeerToken::from_bytes([7u8; 32]);
        let agent_id = AgentId::new("alice@host", [0u8; 32]);
        let entry = PeerEntry {
            token,
            agent_id: agent_id.clone(),
            registered_at: 1_700_000_000_000,
        };

        let wire = serde_json::to_value(&entry).unwrap();
        let obj = wire
            .as_object()
            .expect("PeerEntry serialises as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["agent_id", "registered_at", "token"],
            "PeerEntry wire object must contain exactly the three documented \
             fields; a skip_serializing_if on token would silently drop the \
             secret column for an all-zero default and break the registry \
             reload path; a rename of token would silently fail decode on \
             every persisted JSONL row and lock out previously authenticated \
             peers"
        );

        let token_wire = obj
            .get("token")
            .and_then(serde_json::Value::as_str)
            .expect("PeerEntry::token must serialise as a JSON string");
        // The base58 form of 32 bytes is 43 or 44 chars (44 for
        // high-leading bytes, 43 when the most significant byte is small
        // — the [7u8; 32] fixture lands at the typical 44-char form).
        // Whichever it is, it must NOT collapse to the 6-char PeerSummary
        // token_prefix view.
        let len = token_wire.chars().count();
        assert!(
            len == 43 || len == 44,
            "PeerEntry::token wire form must be the full base58 of the 32-byte \
             secret (43 or 44 chars), got {len} chars — collapsing to the \
             6-char PeerSummary::token_prefix would mean the registry could \
             no longer authenticate peers because the prefix cannot \
             reconstruct the full token"
        );

        // Round-trip pins the PartialEq + Eq derive contract on every
        // field; PeerToken's PartialEq uses constant-time comparison.
        let back: PeerEntry = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(back, entry);

        // Each strictly-required field must reject when omitted.
        for required in ["token", "agent_id", "registered_at"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<PeerEntry>(serde_json::Value::Object(missing)).is_err(),
                "PeerEntry wire form must reject a payload missing {required:?}",
            );
        }

        // Cross-binding: the PeerSummary derived from this PeerEntry must
        // carry the 6-char prefix of the PeerEntry's full token. The
        // PeerSummary::token_prefix is the redaction view and the
        // PeerEntry::token is the secret; the prefix MUST be a substring
        // of the full token wire form, pinning that PeerSummary's
        // redaction is derived from PeerEntry's full token rather than
        // recomputed from a stale source.
        let summary = summary_from(&entry, None);
        assert_eq!(summary.token_prefix.chars().count(), 6);
        assert!(
            token_wire.starts_with(summary.token_prefix.as_str()),
            "PeerSummary::token_prefix must be the first 6 chars of \
             PeerEntry::token base58 — got summary prefix {:?} not a prefix \
             of PeerEntry token wire {:?}",
            summary.token_prefix,
            token_wire,
        );
    }

    #[test]
    fn peer_event_registered_serde_pins_flattened_newtype_variant() {
        // PeerEvent::Registered is the newtype variant wrapping PeerEntry,
        // persisted to the JSONL peer-registry log on every successful
        // PeerRegistry::register call. With #[serde(tag = "type")] on the
        // enum, the newtype variant FLATTENS PeerEntry's three wire fields
        // next to the 'type' discriminator — so the wire object is exactly
        // four keys: type='registered', token (base58 string), agent_id
        // (nested AgentId), registered_at (u64).
        // peer_entry_serde_pins_three_required_fields covers the inner
        // PeerEntry payload but not the enum-level flattened-newtype shape;
        // a refactor that changed PeerEvent::Registered from a newtype
        // variant to a struct variant (Registered { entry: PeerEntry })
        // would silently nest PeerEntry one level deeper under an 'entry'
        // key and every prior flattened-wire-form JSONL row would fail
        // decode at JsonlPeerRegistry::open replay — every previously-
        // registered peer would disappear from the in-memory state while
        // their Revoked tombstones still apply, locking the operator out
        // of resolving their own bootstrap token.
        let mut pubkey = [0u8; 32];
        for (i, b) in b"alice@host".iter().enumerate() {
            pubkey[i] = *b;
        }
        let agent_id = AgentId::new("alice@host", pubkey);
        let event = PeerEvent::Registered(PeerEntry {
            token: PeerToken::from_bytes([7u8; 32]),
            agent_id: agent_id.clone(),
            registered_at: 12_000,
        });

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("PeerEvent serialises as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["agent_id", "registered_at", "token", "type"],
            "PeerEvent::Registered wire form must be exactly four keys: \
             PeerEntry's three fields flattened next to the 'type' \
             discriminator. A refactor from newtype variant to struct \
             variant (Registered {{ entry: PeerEntry }}) would nest \
             PeerEntry one level deeper under an 'entry' key and every \
             prior JSONL Registered row would fail decode at \
             JsonlPeerRegistry::open replay",
        );
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("registered")),
            "PeerEvent discriminator slug must be snake_case 'registered'; \
             a rename to 'register' or any #[serde(rename = ...)] regression \
             silently strands every prior JSONL Registered row at replay and \
             the registry rebuilds with an empty entry list while revoked \
             tombstones survive — the operator sees a peer-list of zero \
             live peers without any operator-initiated revocation",
        );

        let agent_obj = obj
            .get("agent_id")
            .and_then(serde_json::Value::as_object)
            .expect("agent_id must serialise as a nested JSON object");
        let mut agent_keys: Vec<&str> = agent_obj.keys().map(String::as_str).collect();
        agent_keys.sort();
        assert_eq!(
            agent_keys,
            vec!["display", "pubkey"],
            "PeerEvent::Registered::agent_id must surface the AgentId \
             display+pubkey shape; a refactor that flattened or \
             restructured AgentId would break the cross-type contract \
             between registry replay and capability subject resolution — \
             capability checks would silently degrade to display-only \
             matching and a malicious local process could claim any \
             AgentId.display on the wire without holding the matching \
             pubkey",
        );
        assert_eq!(
            agent_obj.get("display").and_then(serde_json::Value::as_str),
            Some("alice@host"),
        );
        assert_eq!(
            agent_obj.get("pubkey").and_then(serde_json::Value::as_str),
            Some(agent_id.pubkey_base58().as_str()),
            "PeerEvent::Registered::agent_id.pubkey must equal the AgentId's \
             base58 pubkey verbatim — registry replay's per-peer AgentId \
             attribution depends on this cross-type binding staying stable",
        );

        let token_wire = obj
            .get("token")
            .and_then(serde_json::Value::as_str)
            .expect("PeerEvent::Registered::token must serialise as a JSON string");
        let len = token_wire.chars().count();
        assert!(
            len == 43 || len == 44,
            "PeerEvent::Registered::token wire form must be the full base58 \
             of the 32-byte secret (43 or 44 chars), got {len} chars — \
             collapsing to PeerSummary's 6-char redacted prefix would mean \
             registry replay could no longer authenticate peers because \
             the prefix cannot reconstruct the full token",
        );

        let back: PeerEvent = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "PeerEvent::Registered must round-trip through serde_json \
             verbatim — the PartialEq derive is the contract the \
             JsonlPeerRegistry::open replay path joins peer events against",
        );

        for required in ["token", "agent_id", "registered_at"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<PeerEvent>(serde_json::Value::Object(missing)).is_err(),
                "PeerEvent::Registered wire form must reject a payload \
                 missing {required:?}; a stray #[serde(default)] on \
                 PeerEntry::token would let a malformed row decode with the \
                 all-zero PeerToken and registry replay would silently bind \
                 that empty-bytes token to any AgentId — a peer connecting \
                 with no token would resolve to a registered identity and \
                 bypass the capability-subject attribution contract",
            );
        }
    }

    #[test]
    fn peer_event_revoked_serde_pins_two_field_variant() {
        // PeerEvent::Revoked is the tombstone variant the daemon appends
        // to the JSONL peer-registry log on every successful
        // PeerRegistry::revoke call (single revocation, prefix-driven
        // revoke, and bulk purge-and-revoke paths). With #[serde(tag =
        // "type")] on the enum and rename_all = "snake_case", the wire
        // object is exactly three keys: type='revoked', token (base58
        // string carrying the full 32-byte secret bytes — the registry
        // uses it to look up the entry for tombstoning), revoked_at (u64
        // epoch ms). No test pins the enum-level wire form. A refactor
        // that flipped the discriminator slug, renamed the fields, or
        // added #[serde(default)] to either field would silently corrupt
        // registry replay — Revoked tombstones decoded with zero-byte
        // tokens or zero revoked_at timestamps would either tombstone the
        // all-zero PeerToken (locking out a freshly seeded operator) or
        // surface a revoked peer as still-live (re-authenticating a
        // revoked token).
        let event = PeerEvent::Revoked {
            token: PeerToken::from_bytes([9u8; 32]),
            revoked_at: 18_000,
        };

        let wire = serde_json::to_value(&event).unwrap();
        let obj = wire
            .as_object()
            .expect("PeerEvent serialises as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["revoked_at", "token", "type"],
            "PeerEvent::Revoked wire form must be exactly three keys: the \
             two variant fields plus the 'type' discriminator. A refactor \
             that renamed token or revoked_at without #[serde(rename = ...)] \
             would silently fail decode on every prior JSONL Revoked row, \
             empty the tombstone map at registry replay, and let every \
             previously-revoked peer reauthenticate with their old token",
        );
        assert_eq!(
            obj.get("type"),
            Some(&serde_json::json!("revoked")),
            "PeerEvent discriminator slug must be snake_case 'revoked'; a \
             rename to 'revoke' or any #[serde(rename = ...)] regression \
             silently strands every prior JSONL Revoked row at replay and \
             registry replay rebuilds with all live entries but no \
             tombstones — every previously-revoked peer reauthenticates \
             because their token is no longer in the revoked map, \
             bypassing the operator's incident-response revocation",
        );

        let token_wire = obj
            .get("token")
            .and_then(serde_json::Value::as_str)
            .expect("PeerEvent::Revoked::token must serialise as a JSON string");
        let len = token_wire.chars().count();
        assert!(
            len == 43 || len == 44,
            "PeerEvent::Revoked::token wire form must be the full base58 of \
             the 32-byte secret (43 or 44 chars), got {len} chars — \
             collapsing to PeerSummary's 6-char redacted prefix would mean \
             registry replay could no longer match the tombstone against \
             the registered entry's full token and revocation would \
             silently no-op",
        );

        assert_eq!(
            obj.get("revoked_at").and_then(serde_json::Value::as_u64),
            Some(18_000),
            "PeerEvent::Revoked::revoked_at must round-trip verbatim on the \
             wire — the purge_older_than compaction path drops every \
             Revoked tombstone with revoked_at < before_ms, so any \
             representation shift silently changes which tombstones get \
             retained after compaction",
        );

        let back: PeerEvent = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, event,
            "PeerEvent::Revoked must round-trip through serde_json verbatim \
             — the PartialEq derive is the contract the JsonlPeerRegistry::\
             open replay path joins tombstones against",
        );

        for required in ["token", "revoked_at"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<PeerEvent>(serde_json::Value::Object(missing)).is_err(),
                "PeerEvent::Revoked wire form must reject a payload missing \
                 {required:?}; a stray #[serde(default)] on token would let \
                 a malformed row decode with the all-zero PeerToken and \
                 registry replay would tombstone the all-zero token — \
                 locking out a future operator whose bootstrap token \
                 happens to fall in the rejection set, while a default on \
                 revoked_at would let compaction drop tombstones that lost \
                 their timestamp on the wire and silently re-authenticate \
                 a revoked peer",
            );
        }
    }

    #[test]
    fn summary_from_pins_token_prefix_redaction_and_field_mapping() {
        let (token, entry) = entry("alice@host");
        let full_b58 = token.to_b58();

        let live = summary_from(&entry, None);
        assert_eq!(
            live.agent_id, entry.agent_id,
            "agent_id must be copied verbatim from the entry",
        );
        assert_eq!(
            live.registered_at, entry.registered_at,
            "registered_at must be copied verbatim from the entry",
        );
        assert_eq!(
            live.token_prefix.chars().count(),
            6,
            "token_prefix must be exactly 6 base58 characters so the full token never wires through PeerSummary",
        );
        assert_eq!(
            live.token_prefix,
            full_b58.chars().take(6).collect::<String>(),
            "token_prefix must be the first 6 chars of token.to_b58()",
        );
        assert_ne!(
            live.token_prefix, full_b58,
            "PeerSummary must NEVER carry the full base58 token — that would leak the peer-auth secret",
        );
        assert_eq!(live.revoked_at, None);

        let revoked = summary_from(&entry, Some(99));
        assert_eq!(
            revoked.revoked_at,
            Some(99),
            "revoked_at must be forwarded verbatim so Live/Revoked filters see the right state",
        );
        assert_eq!(
            revoked.agent_id, entry.agent_id,
            "agent_id must remain copied regardless of the revoked_at value",
        );
        assert_eq!(
            revoked.token_prefix, live.token_prefix,
            "token_prefix is a property of the token, not of the revoked state",
        );
    }

    #[test]
    fn summary_matches_pins_none_empty_and_pubkey_prefix_branches() {
        let mut pk = [0u8; 32];
        pk[0] = 7;
        pk[1] = 42;
        let agent = AgentId::new("alice@host", pk);
        let pubkey_b58 = agent.pubkey_base58();
        let summary = PeerSummary {
            agent_id: agent,
            token_prefix: "abcdef".to_string(),
            registered_at: 1_700_000_000_000,
            revoked_at: None,
        };

        assert!(
            summary_matches(&summary, None),
            "None means no filter and must pass every summary",
        );
        assert!(
            summary_matches(&summary, Some("")),
            "empty-string prefix must also be treated as no filter; \
             dropping this arm silently breaks empty-prefix listing queries",
        );

        let head_3: String = pubkey_b58.chars().take(3).collect();
        assert!(
            summary_matches(&summary, Some(&head_3)),
            "a known pubkey-base58 prefix must match",
        );
        assert!(
            summary_matches(&summary, Some(&pubkey_b58)),
            "the full pubkey base58 must match (longest prefix)",
        );

        assert!(
            !summary_matches(&summary, Some("alice")),
            "the filter must compare against pubkey_base58, not the human-readable display; \
             swapping fields would silently match the wrong peer set",
        );
        assert!(
            !summary_matches(&summary, Some("zzz")),
            "an unrelated prefix must not match",
        );

        let tail_3: String = pubkey_b58.chars().rev().take(3).collect();
        assert!(
            !summary_matches(&summary, Some(&tail_3)),
            "the suffix of the pubkey must not match; the contract is starts_with, not contains/ends_with",
        );
    }

    #[test]
    fn summary_passes_status_pins_all_three_filters_against_live_and_revoked_summaries() {
        let live = PeerSummary {
            agent_id: dummy_agent("live@host"),
            token_prefix: "abcdef".to_string(),
            registered_at: 1_700_000_000_000,
            revoked_at: None,
        };
        let revoked = PeerSummary {
            agent_id: dummy_agent("rev@host"),
            token_prefix: "uvwxyz".to_string(),
            registered_at: 1_700_000_000_000,
            revoked_at: Some(7),
        };

        assert!(
            summary_passes_status(&live, None),
            "None filter must pass a live summary so stale clients omitting status_filter keep seeing live peers",
        );
        assert!(
            summary_passes_status(&revoked, None),
            "None filter must also pass a revoked summary so the pre-filter wire shape is preserved for stale clients",
        );

        assert!(
            summary_passes_status(&live, Some(PeerStatusFilter::Live)),
            "Live filter must accept a summary with revoked_at = None; otherwise the live operator view drops legitimate live peers",
        );
        assert!(
            !summary_passes_status(&revoked, Some(PeerStatusFilter::Live)),
            "Live filter must reject a summary with revoked_at = Some(_); otherwise the Live view silently leaks revoked peers during incident triage",
        );

        assert!(
            !summary_passes_status(&live, Some(PeerStatusFilter::Revoked)),
            "Revoked filter must reject a summary with revoked_at = None; otherwise the Revoked view silently includes live peers and an incident reviewer cannot trust the count",
        );
        assert!(
            summary_passes_status(&revoked, Some(PeerStatusFilter::Revoked)),
            "Revoked filter must accept a summary with revoked_at = Some(_); otherwise the Revoked view drops the rows it exists to surface",
        );
    }

    #[test]
    fn peer_status_filter_serde_pins_snake_case_wire_form() {
        // PeerStatusFilter is carried by Request::ListPeers.status_filter
        // and bound to the CLI --live/--revoked flags. Without rename_all
        // the variants would emit Live/Revoked titlecase and every
        // status-filtered ListPeers request would deserialize-fail at
        // the daemon, leaving operators with an unhelpful error.
        let cases: [(PeerStatusFilter, &str); 2] = [
            (PeerStatusFilter::Live, "live"),
            (PeerStatusFilter::Revoked, "revoked"),
        ];
        for (variant, slug) in cases {
            let wire = serde_json::to_string(&variant).unwrap();
            assert_eq!(wire, format!("\"{slug}\""));
            let back: PeerStatusFilter = serde_json::from_str(&wire).unwrap();
            assert_eq!(back, variant);
        }

        assert!(
            serde_json::from_str::<PeerStatusFilter>("\"Live\"").is_err(),
            "titlecase Live (the rename_all default) must be rejected",
        );
        assert!(
            serde_json::from_str::<PeerStatusFilter>("\"LIVE\"").is_err(),
            "uppercase LIVE must be rejected so the snake_case whitelist stays tight",
        );
    }

    #[test]
    fn revoke_outcome_serde_pins_each_snake_case_type_slug() {
        // RevokeOutcome is the tagged enum returned by
        // PeerRegistry::revoke_by_token_prefix and serialized over IPC,
        // HTTP, and CLI for every `covenant peers revoke` invocation.
        // The tag name "type" and rename_all = snake_case attribute are
        // both load-bearing — a refactor that drops rename_all would
        // silently emit Revoked/AlreadyRevoked/... titlecase slugs and
        // break every CLI parser keyed on the documented snake_case
        // outcome JSON; a refactor that changes the tag from "type" to
        // any other name would silently fail every JSON consumer of
        // `covenant peers revoke --json`. The self_revoke_forbidden
        // slug is the only CLI signal that an operator's revoke attempt
        // fat-fingered against their own bootstrap row.
        let summary = PeerSummary {
            agent_id: dummy_agent("alice@host"),
            token_prefix: "abcdef".to_string(),
            registered_at: 1_700_000_000_000,
            revoked_at: None,
        };

        let cases: [(RevokeOutcome, &str); 5] = [
            (RevokeOutcome::Revoked(summary.clone()), "revoked"),
            (
                RevokeOutcome::AlreadyRevoked(summary.clone()),
                "already_revoked",
            ),
            (RevokeOutcome::NotFound, "not_found"),
            (
                RevokeOutcome::Ambiguous {
                    matches: vec![],
                    truncated: false,
                },
                "ambiguous",
            ),
            (
                RevokeOutcome::SelfRevokeForbidden(summary.clone()),
                "self_revoke_forbidden",
            ),
        ];
        for (variant, expected_slug) in cases {
            let wire = serde_json::to_value(&variant).unwrap();
            assert_eq!(
                wire.get("type").and_then(|v| v.as_str()),
                Some(expected_slug),
                "{variant:?} must serialize with type={expected_slug:?}; a rename_all or tag drop strands CLI consumers",
            );
            let back: RevokeOutcome = serde_json::from_value(wire.clone())
                .unwrap_or_else(|err| panic!("{variant:?} must round-trip, got: {err}"));
            assert_eq!(back, variant);
        }

        // Dropping rename_all would surface variant names verbatim
        // (Revoked); the snake_case whitelist must reject that form
        // so the regression fails loud at parse time.
        assert!(
            serde_json::from_value::<RevokeOutcome>(serde_json::json!({"type": "Revoked"}))
                .is_err(),
            "titlecase type slug (the rename_all default) must be rejected",
        );

        // Switching the tag from "type" to any other name would silently
        // break every CLI consumer. Pin the tag name so a refactor that
        // drops tag = "type" fails loud at the boundary.
        assert!(
            serde_json::from_value::<RevokeOutcome>(serde_json::json!({"kind": "not_found"}))
                .is_err(),
            "wrong discriminator name (kind) must be rejected",
        );

        // kebab-case must also fail — the contract is snake_case only.
        assert!(
            serde_json::from_value::<RevokeOutcome>(serde_json::json!({"type": "not-found"}))
                .is_err(),
            "kebab-case type slug must be rejected so the snake_case whitelist stays tight",
        );
    }

    #[test]
    fn revoke_outcome_ambiguous_truncated_pins_serde_default() {
        // RevokeOutcome::Ambiguous::truncated carries #[serde(default)] with
        // a documented stale-CLI compatibility contract: a CLI built before
        // the field landed must continue to deserialise a new daemon's
        // RevokeOutcome::Ambiguous response, and the omitted-field wire
        // form must decode to truncated=false (the pre-bound behavior where
        // the operator assumes the displayed matches are exhaustive). A
        // refactor that drops the attribute strands every stale CLI; a
        // refactor that swaps it to a true-returning function silently
        // flips operator-facing semantics for every legacy response.
        let omitted: RevokeOutcome =
            serde_json::from_value(serde_json::json!({"type": "ambiguous", "matches": []}))
                .expect("stale CLI wire form must decode");
        assert_eq!(
            omitted,
            RevokeOutcome::Ambiguous {
                matches: vec![],
                truncated: false,
            },
            "omitted truncated must decode to false; a dropped #[serde(default)] breaks every stale CLI",
        );

        let explicit: RevokeOutcome = serde_json::from_value(serde_json::json!({
            "type": "ambiguous",
            "matches": [],
            "truncated": true,
        }))
        .expect("explicit truncated wire form must decode");
        assert_eq!(
            explicit,
            RevokeOutcome::Ambiguous {
                matches: vec![],
                truncated: true,
            },
            "explicit truncated must decode verbatim; the serde default must not shadow set values",
        );

        let wire = serde_json::to_value(RevokeOutcome::Ambiguous {
            matches: vec![],
            truncated: false,
        })
        .unwrap();
        assert_eq!(
            wire.get("truncated").and_then(|v| v.as_bool()),
            Some(false),
            "Ambiguous variant must serialize truncated=false explicitly; there is no skip_serializing_if on the field, so the daemon's output must keep the field present for new CLI parsers",
        );

        assert!(
            serde_json::from_value::<RevokeOutcome>(serde_json::json!({"type": "ambiguous"}))
                .is_err(),
            "missing matches field must fail to decode; the contract surface must not silently shrink",
        );
    }

    #[test]
    fn revoke_outcome_newtype_variants_pin_flattened_peer_summary_wire_form() {
        // RevokeOutcome carries #[serde(tag = "type", rename_all =
        // "snake_case")] and ships three newtype variants over a
        // PeerSummary payload: Revoked(PeerSummary),
        // AlreadyRevoked(PeerSummary), and
        // SelfRevokeForbidden(PeerSummary). With tag = "type", serde
        // flattens the inner PeerSummary's four wire fields
        // (agent_id, token_prefix, registered_at, revoked_at) next to
        // 'type' on the wire — so each variant must surface exactly
        // five top-level keys. The sibling
        // revoke_outcome_serde_pins_each_snake_case_type_slug pins
        // slug+round-trip across all five outcome variants, and
        // revoke_outcome_ambiguous_truncated_pins_serde_default pins
        // the struct-variant default contract, but nothing pins the
        // flattened PeerSummary wire shape. A refactor that promoted
        // any of these newtype variants to a struct variant (Revoked
        // { summary: PeerSummary }) would silently nest PeerSummary
        // one level deeper under 'summary' and every prior `covenant
        // peers revoke --json` consumer that destructures on the
        // flattened top-level fields would read each revoke outcome
        // as a missing-field record — operator dashboards,
        // post-incident triage scripts, and the CLI's revoke
        // success/failure classification would all silently degrade.
        let summary = PeerSummary {
            agent_id: dummy_agent("alice@host"),
            token_prefix: "abcdef".to_string(),
            registered_at: 1_700_000_000_000,
            revoked_at: Some(1_700_000_010_000),
        };

        let cases: [(RevokeOutcome, &str); 3] = [
            (RevokeOutcome::Revoked(summary.clone()), "revoked"),
            (
                RevokeOutcome::AlreadyRevoked(summary.clone()),
                "already_revoked",
            ),
            (
                RevokeOutcome::SelfRevokeForbidden(summary.clone()),
                "self_revoke_forbidden",
            ),
        ];

        for (variant, expected_slug) in &cases {
            let wire = serde_json::to_value(variant).unwrap();
            let obj = wire
                .as_object()
                .expect("RevokeOutcome serializes as a JSON object");
            let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
            keys.sort();
            assert_eq!(
                keys,
                vec![
                    "agent_id",
                    "registered_at",
                    "revoked_at",
                    "token_prefix",
                    "type",
                ],
                "RevokeOutcome::{variant:?} wire form must be exactly \
                 five top-level keys: PeerSummary's four fields \
                 flattened next to the 'type' discriminator. A \
                 refactor from newtype variant to struct variant \
                 would nest PeerSummary under a 'summary' key and \
                 every covenant peers revoke --json consumer that \
                 destructures on the flattened top-level fields would \
                 read the outcome as a missing-field record",
            );
            assert_eq!(
                obj.get("type"),
                Some(&serde_json::json!(expected_slug)),
                "RevokeOutcome discriminator slug for {variant:?} \
                 must be snake_case {expected_slug:?}; a slug \
                 regression silently strands every CLI consumer that \
                 classifies revoke outcomes on this exact value",
            );

            let agent_id_obj = obj
                .get("agent_id")
                .and_then(serde_json::Value::as_object)
                .expect("agent_id must serialize as a nested JSON object");
            let mut agent_keys: Vec<&str> = agent_id_obj.keys().map(String::as_str).collect();
            agent_keys.sort();
            assert_eq!(
                agent_keys,
                vec!["display", "pubkey"],
                "RevokeOutcome::{variant:?}::agent_id must surface \
                 the AgentId display+pubkey shape; a refactor that \
                 flattened or restructured AgentId would break the \
                 audit-row attribution that pairs PeerRevoked rows \
                 with the displayed revoke outcome",
            );

            assert_eq!(
                obj.get("token_prefix").and_then(serde_json::Value::as_str),
                Some("abcdef"),
                "RevokeOutcome::{variant:?}::token_prefix must \
                 surface as the literal 6-char redaction; a refactor \
                 that renamed or skipped the field would strand \
                 operator triage queries that grep on this exact key",
            );

            let back: RevokeOutcome = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(
                &back, variant,
                "RevokeOutcome::{variant:?} must round-trip through \
                 serde_json verbatim — the PartialEq derive is the \
                 contract every CLI consumer leans on when matching \
                 the daemon's revoke outcome class",
            );

            assert!(
                serde_json::from_value::<RevokeOutcome>(serde_json::json!({
                    "type": expected_slug,
                    "summary": {
                        "agent_id": {"display": "alice@host", "pubkey": dummy_agent("alice@host").pubkey_base58()},
                        "token_prefix": "abcdef",
                        "registered_at": 1_700_000_000_000u64,
                        "revoked_at": 1_700_000_010_000u64,
                    },
                }))
                .is_err(),
                "RevokeOutcome::{variant:?} struct-variant-promotion \
                 wire form must be rejected; if a refactor nests \
                 PeerSummary under 'summary', the flattened top-level \
                 fields disappear and stale consumers silently \
                 misread the outcome",
            );
        }

        for required in ["agent_id", "token_prefix", "registered_at"] {
            let wire = serde_json::to_value(RevokeOutcome::Revoked(summary.clone())).unwrap();
            let mut missing = wire.as_object().unwrap().clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<RevokeOutcome>(serde_json::Value::Object(missing))
                    .is_err(),
                "RevokeOutcome::Revoked wire form must reject a \
                 payload missing {required:?}; a stray \
                 #[serde(default)] on PeerSummary would let a \
                 malformed row decode with an empty token_prefix or \
                 default AgentId and operator dashboards would \
                 attribute a real revocation to a phantom peer — the \
                 audit log's PeerRevoked row would record a \
                 misleading subject",
            );
        }
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
    fn peer_token_debug_pins_six_char_base58_prefix_aligned_with_summary() {
        // covenant_peer_auth::PeerToken Debug impl (line 91-99):
        //
        //   impl std::fmt::Debug for PeerToken {
        //       fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        //           // Tokens are secrets — log only a 6-char prefix of the base58
        //           // form so audit grep still works without leaking the rest.
        //           let s = self.to_b58();
        //           let prefix = &s[..s.len().min(6)];
        //           write!(f, "PeerToken({prefix}…)")
        //       }
        //   }
        //
        // The number 6 is load-bearing across THREE surfaces:
        //
        //   (a) PeerToken Debug redaction (this impl): `&s[..s.len().min(6)]`
        //       documents 'log only a 6-char prefix of the base58 form'.
        //   (b) PeerSummary::token_prefix (line 124-133): the cross-wire
        //       redacted view carries the SAME 6-char prefix, computed by
        //       the private token_b58_prefix helper (line 313-318) via
        //       `s.chars().take(6).collect()` — different code, same length.
        //   (c) The 'peers revoke <prefix>' operator workflow (line 155-160)
        //       takes the 6-char prefix from `peers list` output as the
        //       copy-pasteable revoke handle.
        //
        // token_debug_does_not_leak_full_bytes (line 1893) asserts the
        // format starts with 'PeerToken(', contains '…', and does NOT
        // contain the full base58 — but a refactor that changed the
        // truncation to 4 chars or 8 chars would silently pass all three
        // checks while breaking the cross-system 6-char correlation.
        // Operator dashboards looking up a peer by grep'd debug-log
        // prefix would silently mismatch PeerSummary entries on every
        // restart-replay cycle, and `peers revoke <copy-paste-from-grep>`
        // workflows would fail with no live match.
        //
        // Pin the exact 6-char length AND the alignment with the
        // PeerSummary::token_prefix value derived from the same bytes
        // so a refactor that drifted ONE surface relative to the others
        // surfaces here regardless of which surface was changed first.

        // Deterministic token so the base58 form is stable across CI
        // runs. PeerToken::from_bytes preserves the bytes verbatim
        // (no normalization).
        let token = PeerToken::from_bytes([7u8; 32]);
        let full_b58 = bs58::encode([7u8; 32]).into_string();
        let expected_prefix = &full_b58[..6];

        let debug = format!("{token:?}");

        let expected = format!("PeerToken({expected_prefix}…)");
        assert_eq!(
            debug, expected,
            "PeerToken Debug format must be exactly 'PeerToken({{first-6-base58-chars}}…)' \
             — a refactor that changed the format delimiter (e.g., \
             'PeerToken[{{prefix}}]' or 'PeerToken<{{prefix}}>') or shifted \
             the truncation length (e.g., to 4 or 8 chars) would silently \
             break operator audit-grep workflows that parse the 6-char \
             handle from debug-log lines. got: {debug:?}",
        );

        // Independently assert the 6-char length so a future refactor
        // that emits the right delimiters but wrong length (e.g., via
        // a buggy substring helper) surfaces with a clear failure on
        // the length contract specifically.
        let inner_start = "PeerToken(".len();
        let inner_end = debug
            .find('…')
            .expect("Debug format must contain the documented '…' redaction marker");
        let prefix_in_debug = &debug[inner_start..inner_end];
        assert_eq!(
            prefix_in_debug.chars().count(),
            6,
            "the redacted prefix inside PeerToken(...…) must be exactly 6 \
             chars — a refactor that promoted the truncation to 4 chars \
             ('shorter is more secret') or 8 chars ('more entropy in the \
             visible prefix') would silently misalign Debug logs from \
             PeerSummary::token_prefix and break the 'peers revoke \
             <copy-paste-handle>' operator workflow. got: {prefix_in_debug:?} \
             ({} chars)",
            prefix_in_debug.chars().count(),
        );
        assert_eq!(
            prefix_in_debug, expected_prefix,
            "the redacted prefix must be the FIRST six base58 chars of \
             the token bytes — a refactor that picked a different slice \
             (e.g., last 6 chars or middle 6) would silently misalign \
             with PeerSummary::token_prefix's chars().take(6) and break \
             cross-system grep correlation. got: {prefix_in_debug:?} \
             expected: {expected_prefix:?}",
        );

        // Cross-bind with the PeerSummary surface: a PeerSummary built
        // from the same token bytes must carry the SAME 6-char prefix.
        // PeerSummary is constructed publicly through public crates
        // (the internal summary_from helper at line 320 is private),
        // so build one inline with the documented chars().take(6)
        // shape to mirror token_b58_prefix at line 315-318.
        let summary_prefix: String = full_b58.chars().take(6).collect();
        assert_eq!(
            prefix_in_debug,
            summary_prefix.as_str(),
            "PeerToken Debug prefix MUST equal PeerSummary::token_prefix \
             for the same token bytes — the docstring at line 124 binds \
             '6-char base58 prefix matching PeerToken::Debug redaction' \
             across both surfaces. If this assertion fires after a \
             refactor that diverged the two surfaces (e.g., changed the \
             Debug truncation length without updating token_b58_prefix \
             at line 315), operator dashboards that join debug-log lines \
             against `peers list` rows would silently mismatch on every \
             redaction cycle",
        );
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
        // Regression test for the in-memory mutation ordering inside
        // `purge_revoked_older_than`: between its two mutation blocks,
        // a concurrent `resolve()` of a recently-purged token used to
        // observe `not in revoked` AND `entry in entries`
        // simultaneously — authenticating a token whose tombstone was
        // just dropped. Fix: mutate `entries` first so the
        // intermediate state keeps the tombstone visible.
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

        let (s, truncated) = r.list_summaries(10, None, None).await.unwrap();
        assert!(!truncated, "two entries under cap of ten");
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

        let (s, truncated) = r.list_summaries(10, None, None).await.unwrap();
        assert!(!truncated, "three entries under cap of ten");
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
        let (s, truncated) = r.list_summaries(10, Some(&prefix), None).await.unwrap();
        assert!(!truncated);
        assert_eq!(s.len(), 1, "only the matching pubkey surfaces");
        assert_eq!(s[0].agent_id.display, "match@local");

        let (none, _) = r.list_summaries(10, Some("zzzzzz"), None).await.unwrap();
        assert!(none.is_empty(), "non-matching prefix returns empty");

        let (all, _) = r.list_summaries(10, Some(""), None).await.unwrap();
        assert_eq!(all.len(), 2, "empty prefix is no filter");
    }

    #[tokio::test]
    async fn list_summaries_never_emits_full_token_b58() {
        let r = InMemoryPeerRegistry::new();
        let (tok, ent) = entry("p@local");
        r.register(ent).await.unwrap();
        let (s, _) = r.list_summaries(10, None, None).await.unwrap();
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
        let (s, _) = r2.list_summaries(10, None, None).await.unwrap();
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
        let outcome = r.revoke_by_token_prefix(&prefix, 16).await.unwrap();
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
        let outcome = r.revoke_by_token_prefix("zzzzzzzz", 16).await.unwrap();
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
        let outcome = r.revoke_by_token_prefix(&prefix, 16).await.unwrap();
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
        let outcome = r.revoke_by_token_prefix("1", 16).await.unwrap();
        match outcome {
            RevokeOutcome::Ambiguous { matches, truncated } => {
                assert_eq!(matches.len(), 2);
                assert!(!truncated, "two matches under cap of sixteen");
                // Neither tombstoned.
                assert!(matches.iter().all(|s| s.revoked_at.is_none()));
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
        // Registry unchanged — both still resolve.
        assert!(r.resolve(&t1).await.unwrap().is_some());
        assert!(r.resolve(&t2).await.unwrap().is_some());
    }

    /// At-cap match count: exactly `limit` entries match, none truncated.
    #[tokio::test]
    async fn revoke_by_token_prefix_ambiguous_at_cap_not_truncated() {
        let r = InMemoryPeerRegistry::new();
        let (_, e1) = entry_with_token_b58_starting_with("1", "a@local");
        let (_, e2) = entry_with_token_b58_starting_with("1", "b@local");
        r.register(e1).await.unwrap();
        r.register(e2).await.unwrap();
        let outcome = r.revoke_by_token_prefix("1", 2).await.unwrap();
        match outcome {
            RevokeOutcome::Ambiguous { matches, truncated } => {
                assert_eq!(matches.len(), 2);
                assert!(!truncated, "exactly cap is not truncation");
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    /// Over-cap match count: more entries match than `limit`, list is
    /// truncated and `truncated == true`.
    #[tokio::test]
    async fn revoke_by_token_prefix_ambiguous_over_cap_truncated() {
        let r = InMemoryPeerRegistry::new();
        let (_, e1) = entry_with_token_b58_starting_with("1", "a@local");
        let (_, e2) = entry_with_token_b58_starting_with("1", "b@local");
        let (_, e3) = entry_with_token_b58_starting_with("1", "c@local");
        r.register(e1).await.unwrap();
        r.register(e2).await.unwrap();
        r.register(e3).await.unwrap();
        let outcome = r.revoke_by_token_prefix("1", 2).await.unwrap();
        match outcome {
            RevokeOutcome::Ambiguous { matches, truncated } => {
                assert_eq!(matches.len(), 2, "list capped at limit");
                assert!(truncated, "third match drops; flag set");
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn revoke_by_token_prefix_outcome_never_serializes_full_token_b58() {
        let r = InMemoryPeerRegistry::new();
        let (token, ent) = entry("p@local");
        r.register(ent).await.unwrap();
        let prefix: String = token.to_b58().chars().take(6).collect();
        let outcome = r.revoke_by_token_prefix(&prefix, 16).await.unwrap();
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
            let outcome = r.revoke_by_token_prefix(&prefix, 16).await.unwrap();
            assert!(matches!(outcome, RevokeOutcome::Revoked(_)));
        }
        let r2 = JsonlPeerRegistry::open(path).await.unwrap();
        assert_eq!(r2.resolve(&token).await.unwrap(), None);
        // The summary list still surfaces the revoked entry.
        let (s, _) = r2.list_summaries(10, None, None).await.unwrap();
        assert_eq!(s.len(), 1);
        assert!(s[0].revoked_at.is_some());
    }

    /// `list_summaries` peeks one entry past `limit`; when more rows
    /// exist than `limit`, the returned list is exactly `limit` long
    /// and `truncated == true`. The operator's signal that they need
    /// to either bump `limit` or refine `pubkey_prefix`.
    #[tokio::test]
    async fn list_summaries_marks_truncated_when_more_rows_exist() {
        let r = InMemoryPeerRegistry::new();
        let (_, e1) = entry("a@local");
        let (_, e2) = entry("b@local");
        let (_, e3) = entry("c@local");
        r.register(e1).await.unwrap();
        r.register(e2).await.unwrap();
        r.register(e3).await.unwrap();
        let (rows, truncated) = r.list_summaries(2, None, None).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(truncated);
    }

    /// At-cap exact match count is NOT truncation. The peek-one-past
    /// shape returns exactly `limit + 1` candidates only when more
    /// than `limit` exist; with exactly `limit`, the iterator stops
    /// at `limit` and `truncated` stays `false`.
    #[tokio::test]
    async fn list_summaries_at_cap_not_truncated() {
        let r = InMemoryPeerRegistry::new();
        let (_, e1) = entry("a@local");
        let (_, e2) = entry("b@local");
        r.register(e1).await.unwrap();
        r.register(e2).await.unwrap();
        let (rows, truncated) = r.list_summaries(2, None, None).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(!truncated);
    }

    /// JSONL impl mirrors the in-memory peek-one-past shape so a
    /// post-restart registry sees the same flag.
    #[tokio::test]
    async fn jsonl_list_summaries_marks_truncated_when_more_rows_exist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.jsonl");
        let r = JsonlPeerRegistry::open(path).await.unwrap();
        let (_, e1) = entry("a@local");
        let (_, e2) = entry("b@local");
        let (_, e3) = entry("c@local");
        r.register(e1).await.unwrap();
        r.register(e2).await.unwrap();
        r.register(e3).await.unwrap();
        let (rows, truncated) = r.list_summaries(2, None, None).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(truncated);
    }

    /// Stale callers that don't know about `truncated` still
    /// deserialise an `Ambiguous` payload from a new daemon. The
    /// `#[serde(default)]` reads as `false`, the safe degradation.
    #[tokio::test]
    async fn ambiguous_truncated_field_is_serde_default_for_forward_compat() {
        let raw = r#"{"type":"ambiguous","matches":[]}"#;
        let outcome: RevokeOutcome = serde_json::from_str(raw).unwrap();
        match outcome {
            RevokeOutcome::Ambiguous { matches, truncated } => {
                assert!(matches.is_empty());
                assert!(!truncated, "missing field defaults to false");
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
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

    #[tokio::test]
    async fn list_summaries_status_filter_live_drops_revoked_rows() {
        let r = InMemoryPeerRegistry::new();
        let (live_tok, live_ent) = entry("alive@local");
        let (dead_tok, dead_ent) = entry("ghost@local");
        r.register(live_ent).await.unwrap();
        r.register(dead_ent).await.unwrap();
        r.revoke(&dead_tok).await.unwrap();

        let (rows, truncated) = r
            .list_summaries(10, None, Some(PeerStatusFilter::Live))
            .await
            .unwrap();
        assert!(!truncated);
        assert_eq!(rows.len(), 1, "only the live row surfaces");
        assert_eq!(rows[0].token_prefix, &live_tok.to_b58()[..6]);
        assert!(rows[0].revoked_at.is_none());
        assert_ne!(rows[0].token_prefix, &dead_tok.to_b58()[..6]);
    }

    #[tokio::test]
    async fn list_summaries_status_filter_revoked_drops_live_rows() {
        let r = InMemoryPeerRegistry::new();
        let (live_tok, live_ent) = entry("alive@local");
        let (dead_tok, dead_ent) = entry("ghost@local");
        r.register(live_ent).await.unwrap();
        r.register(dead_ent).await.unwrap();
        r.revoke(&dead_tok).await.unwrap();

        let (rows, truncated) = r
            .list_summaries(10, None, Some(PeerStatusFilter::Revoked))
            .await
            .unwrap();
        assert!(!truncated);
        assert_eq!(rows.len(), 1, "only the revoked row surfaces");
        assert_eq!(rows[0].token_prefix, &dead_tok.to_b58()[..6]);
        assert!(rows[0].revoked_at.is_some());
        assert_ne!(rows[0].token_prefix, &live_tok.to_b58()[..6]);
    }

    #[tokio::test]
    async fn list_summaries_status_filter_none_keeps_both_halves() {
        // The pre-filter behaviour is preserved when status_filter is
        // None — the wire default for stale clients that omit the
        // field. Regression test for forward-compat.
        let r = InMemoryPeerRegistry::new();
        let (_live_tok, live_ent) = entry("alive@local");
        let (dead_tok, dead_ent) = entry("ghost@local");
        r.register(live_ent).await.unwrap();
        r.register(dead_ent).await.unwrap();
        r.revoke(&dead_tok).await.unwrap();

        let (rows, truncated) = r.list_summaries(10, None, None).await.unwrap();
        assert!(!truncated);
        assert_eq!(rows.len(), 2);
    }

    /// Status filter composes with `pubkey_prefix`: rows must match
    /// both. Regression against a refactor that short-circuits one
    /// filter when the other is None.
    #[tokio::test]
    async fn list_summaries_status_filter_composes_with_pubkey_prefix() {
        let r = InMemoryPeerRegistry::new();
        let (_, e_live_match) = entry_with_pubkey("live_match@local", 0xff);
        let (dead_match_tok, e_dead_match) = entry_with_pubkey("dead_match@local", 0xff);
        let (_, e_other) = entry_with_pubkey("other@local", 0x01);
        r.register(e_live_match.clone()).await.unwrap();
        r.register(e_dead_match.clone()).await.unwrap();
        r.register(e_other).await.unwrap();
        r.revoke(&dead_match_tok).await.unwrap();

        let target_b58 = bs58::encode(e_live_match.agent_id.pubkey).into_string();
        let prefix: String = target_b58.chars().take(4).collect();

        let (live_rows, _) = r
            .list_summaries(10, Some(&prefix), Some(PeerStatusFilter::Live))
            .await
            .unwrap();
        assert_eq!(live_rows.len(), 1);
        assert_eq!(live_rows[0].agent_id.display, "live_match@local");

        let (dead_rows, _) = r
            .list_summaries(10, Some(&prefix), Some(PeerStatusFilter::Revoked))
            .await
            .unwrap();
        assert_eq!(dead_rows.len(), 1);
        assert_eq!(dead_rows[0].agent_id.display, "dead_match@local");
    }

    /// `truncated` reflects truncation among the *filtered* rows, not
    /// the full registry. Regression against a refactor that filters
    /// after `take(limit + 1)`, which would mark a result truncated
    /// when only same-status rows exist within the cap.
    #[tokio::test]
    async fn list_summaries_status_filter_truncation_reflects_filtered_rows() {
        let r = InMemoryPeerRegistry::new();
        // Three live + three revoked.
        let mut live_tokens = Vec::new();
        for name in ["L1@local", "L2@local", "L3@local"] {
            let (t, e) = entry(name);
            r.register(e).await.unwrap();
            live_tokens.push(t);
        }
        for name in ["R1@local", "R2@local", "R3@local"] {
            let (t, e) = entry(name);
            r.register(e).await.unwrap();
            r.revoke(&t).await.unwrap();
        }

        // limit=2 with status=Live → exactly 2 rows + truncated, ignoring
        // the three revoked rows that don't pass the status filter.
        let (rows, truncated) = r
            .list_summaries(2, None, Some(PeerStatusFilter::Live))
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(truncated, "third live row dropped → truncated");
        assert!(rows.iter().all(|s| s.revoked_at.is_none()));

        // limit=3 with status=Live → exactly 3 rows, NOT truncated even
        // though three revoked rows exist (filtered out before peek).
        let (rows, truncated) = r
            .list_summaries(3, None, Some(PeerStatusFilter::Live))
            .await
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert!(
            !truncated,
            "three live rows fit in cap of three; revoked rows are not over-cap"
        );
    }

    #[tokio::test]
    async fn jsonl_list_summaries_status_filter_live_drops_revoked_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.jsonl");
        let r = JsonlPeerRegistry::open(path).await.unwrap();
        let (live_tok, live_ent) = entry("alive@local");
        let (dead_tok, dead_ent) = entry("ghost@local");
        r.register(live_ent).await.unwrap();
        r.register(dead_ent).await.unwrap();
        r.revoke(&dead_tok).await.unwrap();

        let (rows, _) = r
            .list_summaries(10, None, Some(PeerStatusFilter::Live))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].token_prefix, &live_tok.to_b58()[..6]);
    }

    #[test]
    fn peer_error_bad_token_b58_display_message_pins_prefix_encoding_qualifier_and_payload() {
        let err = PeerError::BadTokenB58("not-a-token".into());
        let message = format!("{err}");
        assert_eq!(
            message, "invalid token base58: not-a-token",
            "PeerError::BadTokenB58 Display drifted (typo, dropped 'invalid token' prefix, dropped 'base58' encoding qualifier, or word-swap regression class)"
        );
        assert!(
            message.contains("invalid token"),
            "PeerError::BadTokenB58 must surface the 'invalid token' prefix (audit-log discriminator regression class): {message}"
        );
        assert!(
            message.contains("base58"),
            "PeerError::BadTokenB58 must surface the 'base58' encoding qualifier so a future encoding migration is forced to update the error message in lockstep: {message}"
        );
    }
}
