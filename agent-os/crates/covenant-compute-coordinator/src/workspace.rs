//! Durable workspace metadata and short-lived ciphertext-only object grants.
//! Disabled unless a deployment explicitly configures its S3 bucket. Metadata
//! is committed before grants escape and before in-memory versions advance.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path as RoutePath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use covenant_compute_protocol::workspace::*;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::jobs::JobPhase;
use crate::state::CoordinatorState;

#[path = "workspace_s3.rs"]
mod s3;
pub use s3::{S3WorkspaceConfig, S3WorkspaceObjects};

pub const GRANT_TTL_MS: u64 = 300_000;
const UPLOAD_WINDOW_MS: u64 = 900_000;
const CLEANUP_SKEW_MS: u64 = 120_000;
const OBJECT_ACTIONS_PER_OWNER_DAY: u64 = 256;
const OBJECT_ACTIONS_PER_DAY: u64 = 4096;
const MAX_ALLOCATED_OBJECTS: u64 = 4096;

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("workspace not found")]
    NotFound,
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Unauthorized(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("workspace capacity limit reached")]
    Capacity,
    #[error("workspace persistence: {0}")]
    Persist(String),
    #[error("workspace object storage: {0}")]
    Storage(String),
    #[error("workspace configuration: {0}")]
    Config(String),
}

impl IntoResponse for WorkspaceError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::Capacity => StatusCode::TOO_MANY_REQUESTS,
            Self::Storage(_) => StatusCode::BAD_GATEWAY,
            Self::Persist(_) | Self::Config(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let message = if matches!(&self, Self::Persist(_) | Self::Config(_)) {
            tracing::error!(error = %self, "workspace operation failed");
            "workspace service is unavailable".to_owned()
        } else {
            self.to_string()
        };
        (status, Json(serde_json::json!({"error":message}))).into_response()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectMetadata {
    pub ciphertext_bytes: u64,
    pub ciphertext_sha256: String,
}

#[async_trait]
pub trait WorkspaceObjects: Send + Sync {
    fn put_grant(
        &self,
        key: &str,
        upload: &WorkspaceUpload,
        now_ms: u64,
    ) -> Result<ObjectGrant, WorkspaceError>;
    fn get_grant(&self, key: &str, now_ms: u64, ttl_ms: u64)
        -> Result<ObjectGrant, WorkspaceError>;
    async fn head(&self, key: &str, now_ms: u64) -> Result<ObjectMetadata, WorkspaceError>;
    /// Only for isolated test objects whose writers have definitively ended.
    /// Production cleanup uses `fence` and must never delete a fenced key.
    async fn delete(&self, key: &str, now_ms: u64) -> Result<(), WorkspaceError>;
    /// Replace retired ciphertext with a permanent empty object. A successful
    /// fence must prevent every previously issued conditional PUT committing.
    async fn fence(&self, key: &str, now_ms: u64) -> Result<(), WorkspaceError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PendingUpload {
    request: WorkspaceUpload,
    key: String,
    expires_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Garbage {
    key: String,
    after_ms: u64,
    bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkspaceRow {
    #[serde(default = "schema_v1")]
    schema_version: u8,
    #[serde(default)]
    usage_day: u64,
    #[serde(default)]
    object_actions: u64,
    #[serde(default)]
    fenced_objects: u64,
    view: WorkspaceView,
    current_key: Option<String>,
    committed_upload_id: Option<Uuid>,
    pending: Option<PendingUpload>,
    garbage: Vec<Garbage>,
    deleted: bool,
}

fn schema_v1() -> u8 {
    1
}

struct WorkspaceBook {
    rows: BTreeMap<(String, Uuid), WorkspaceRow>,
    file: Option<std::fs::File>,
    _lock: std::fs::File,
    path: PathBuf,
    compact_after_bytes: u64,
    max_owners: usize,
    max_bytes: u64,
}

impl WorkspaceBook {
    fn open(path: &Path, max_owners: usize, max_bytes: u64) -> Result<Self, WorkspaceError> {
        let mut lock_options = std::fs::OpenOptions::new();
        lock_options
            .create(true)
            .read(true)
            .write(true)
            .truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            lock_options.mode(0o600);
        }
        let lock = lock_options
            .open(path.with_extension("lock"))
            .map_err(|e| WorkspaceError::Persist(e.to_string()))?;
        lock.try_lock().map_err(|_| {
            WorkspaceError::Persist("workspace journal is in use by another process".into())
        })?;
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(WorkspaceError::Persist(e.to_string())),
        };
        let valid = bytes
            .iter()
            .rposition(|b| *b == b'\n')
            .map(|n| n + 1)
            .unwrap_or(0);
        let mut rows = BTreeMap::new();
        for (line_number, line) in bytes[..valid].split(|b| *b == b'\n').enumerate() {
            if line.is_empty() {
                continue;
            }
            let row: WorkspaceRow = serde_json::from_slice(line)
                .map_err(|e| WorkspaceError::Persist(format!("line {}: {e}", line_number + 1)))?;
            if row.schema_version != 1 {
                return Err(WorkspaceError::Persist(
                    "unsupported workspace journal schema".into(),
                ));
            }
            rows.insert((row.view.owner.clone(), row.view.id), row);
        }
        // Atomic compaction retains deleted-id/version tombstones and pending
        // cleanup work, so a restart can neither resurrect nor leak objects.
        let tmp = path.with_extension(format!("{}.compacting", Uuid::new_v4()));
        let persist = |e: std::io::Error| WorkspaceError::Persist(e.to_string());
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(persist)?;
        for row in rows.values() {
            let mut line =
                serde_json::to_vec(row).map_err(|e| WorkspaceError::Persist(e.to_string()))?;
            line.push(b'\n');
            file.write_all(&line).map_err(persist)?;
        }
        file.sync_all().map_err(persist)?;
        std::fs::rename(&tmp, path).map_err(persist)?;
        if let Some(parent) = path.parent() {
            std::fs::File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(persist)?;
        }
        let file = std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .map_err(persist)?;
        let compact_after_bytes = file
            .metadata()
            .map_err(persist)?
            .len()
            .saturating_mul(2)
            .saturating_add(1024 * 1024);
        Ok(Self {
            rows,
            file: Some(file),
            _lock: lock,
            path: path.to_path_buf(),
            compact_after_bytes,
            max_owners,
            max_bytes,
        })
    }

    fn reserve_object_action(
        &mut self,
        owner: &str,
        id: Uuid,
        now_ms: u64,
    ) -> Result<(), WorkspaceError> {
        let day =
            (now_ms / 86_400_000).max(self.rows.values().map(|r| r.usage_day).max().unwrap_or(0));
        let mut total = 0u64;
        let mut owned = 0u64;
        for row in self.rows.values().filter(|r| r.usage_day == day) {
            total = total.saturating_add(row.object_actions);
            if row.view.owner == owner {
                owned = owned.saturating_add(row.object_actions);
            }
        }
        if total >= OBJECT_ACTIONS_PER_DAY || owned >= OBJECT_ACTIONS_PER_OWNER_DAY {
            return Err(WorkspaceError::Capacity);
        }
        let mut row = self.row(owner, id)?.clone();
        if row.usage_day != day {
            row.usage_day = day;
            row.object_actions = 0;
        }
        row.object_actions = row.object_actions.saturating_add(1);
        self.persist(row)
    }

    fn reserved_bytes(&self) -> u64 {
        self.rows.values().fold(0u64, |total, row| {
            let current = if row.current_key.is_some() {
                row.view.snapshot.as_ref().map_or(0, |s| s.ciphertext_bytes)
            } else {
                0
            };
            let pending = row
                .pending
                .as_ref()
                .map_or(0, |p| p.request.ciphertext_bytes);
            row.garbage.iter().fold(
                total.saturating_add(current).saturating_add(pending),
                |sum, garbage| sum.saturating_add(garbage.bytes),
            )
        })
    }

    fn allocated_objects(&self) -> u64 {
        self.rows.values().fold(0u64, |total, row| {
            total
                .saturating_add(row.fenced_objects)
                .saturating_add(u64::from(row.current_key.is_some()))
                .saturating_add(u64::from(row.pending.is_some()))
                .saturating_add(row.garbage.len() as u64)
        })
    }

    fn persist(&mut self, row: WorkspaceRow) -> Result<(), WorkspaceError> {
        if let Some(file) = self.file.as_mut() {
            let mut line =
                serde_json::to_vec(&row).map_err(|e| WorkspaceError::Persist(e.to_string()))?;
            line.push(b'\n');
            let previous = file
                .metadata()
                .map_err(|e| WorkspaceError::Persist(e.to_string()))?
                .len();
            if let Err(e) = file.write_all(&line).and_then(|_| file.sync_data()) {
                // Refuse to append after a partial line. A failed rollback is
                // fatal for this process; continuing could acknowledge a state
                // that cannot be replayed safely after restart.
                if file
                    .set_len(previous)
                    .and_then(|_| file.sync_data())
                    .is_err()
                {
                    panic!("workspace journal rollback failed after append error");
                }
                return Err(WorkspaceError::Persist(e.to_string()));
            }
        }
        self.rows.insert((row.view.owner.clone(), row.view.id), row);
        if self
            .file
            .as_ref()
            .map(|f| f.metadata().map(|m| m.len()))
            .transpose()
            .map_err(|e| WorkspaceError::Persist(e.to_string()))?
            .is_some_and(|len| len >= self.compact_after_bytes)
        {
            self.compact()?;
        }
        Ok(())
    }

    // Bound append growth while retaining every tombstone, quota counter and
    // cleanup obligation. The process lock covers the rename and new handle.
    fn compact(&mut self) -> Result<(), WorkspaceError> {
        let temporary = self
            .path
            .with_extension(format!("{}.compacting", Uuid::new_v4()));
        let result = (|| {
            let mut options = std::fs::OpenOptions::new();
            options.create_new(true).append(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&temporary)
                .map_err(|e| WorkspaceError::Persist(e.to_string()))?;
            for row in self.rows.values() {
                let mut line =
                    serde_json::to_vec(row).map_err(|e| WorkspaceError::Persist(e.to_string()))?;
                line.push(b'\n');
                file.write_all(&line)
                    .map_err(|e| WorkspaceError::Persist(e.to_string()))?;
            }
            file.sync_all()
                .map_err(|e| WorkspaceError::Persist(e.to_string()))?;
            let length = file
                .metadata()
                .map_err(|e| WorkspaceError::Persist(e.to_string()))?
                .len();
            std::fs::rename(&temporary, &self.path)
                .map_err(|e| WorkspaceError::Persist(e.to_string()))?;
            self.file = Some(file);
            if let Some(parent) = self.path.parent() {
                // Continuing after an uncertain directory sync could append to
                // a name not durable after a crash. Restart is the safe boundary.
                if std::fs::File::open(parent)
                    .and_then(|f| f.sync_all())
                    .is_err()
                {
                    panic!("workspace journal compaction directory sync failed");
                }
            }
            self.compact_after_bytes = length.saturating_mul(2).saturating_add(1024 * 1024);
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }

    fn row(&self, owner: &str, id: Uuid) -> Result<&WorkspaceRow, WorkspaceError> {
        self.rows
            .get(&(owner.to_owned(), id))
            .filter(|r| !r.deleted)
            .ok_or(WorkspaceError::NotFound)
    }
}

pub struct WorkspaceService {
    book: Mutex<WorkspaceBook>,
    objects: Arc<dyn WorkspaceObjects>,
    coordinator: CoordinatorState,
    writes_enabled: bool,
}

impl WorkspaceService {
    pub fn open(
        path: &Path,
        objects: Arc<dyn WorkspaceObjects>,
        coordinator: CoordinatorState,
        max_owners: usize,
    ) -> Result<Self, WorkspaceError> {
        if max_owners == 0 {
            return Err(WorkspaceError::Config(
                "workspace max owners must be positive".into(),
            ));
        }
        Ok(Self {
            book: Mutex::new(WorkspaceBook::open(path, max_owners, 1024 * 1024 * 1024)?),
            objects,
            coordinator,
            writes_enabled: true,
        })
    }

    /// Counts committed ciphertext, outstanding upload reservations and garbage
    /// until deletion succeeds. Lowering a ceiling stops admissions; recovery and
    /// cleanup continue without discarding prior records.
    pub fn with_writes_enabled(mut self, enabled: bool) -> Self {
        self.writes_enabled = enabled;
        self
    }

    pub fn with_byte_limit(self, max_bytes: u64) -> Result<Self, WorkspaceError> {
        if max_bytes == 0 {
            return Err(WorkspaceError::Config(
                "workspace max bytes must be positive".into(),
            ));
        }
        self.book.lock().max_bytes = max_bytes;
        Ok(self)
    }

    pub fn create(
        &self,
        owner: &str,
        request: CreateWorkspace,
    ) -> Result<WorkspaceView, WorkspaceError> {
        if !self.writes_enabled {
            return Err(WorkspaceError::Forbidden(
                "new workspace admissions are disabled".into(),
            ));
        }
        if request.id.is_nil()
            || request.name.trim().is_empty()
            || request.name.len() > 64
            || request.name.chars().any(char::is_control)
        {
            return Err(WorkspaceError::BadRequest(
                "workspace needs a UUID and a 1..64 byte name without control characters".into(),
            ));
        }
        let mut book = self.book.lock();
        if let Some(existing) = book.rows.get(&(owner.to_owned(), request.id)) {
            if !existing.deleted
                && existing.view.name == request.name
                && existing.view.trust_floor == request.trust_floor
            {
                return Ok(existing.view.clone());
            }
            return Err(WorkspaceError::Conflict(
                "workspace id already exists with different metadata or was deleted".into(),
            ));
        }
        // Bound tombstones separately: deletion frees an active workspace slot
        // without giving one identity an unbounded durable metadata bill.
        let owners: std::collections::BTreeSet<_> = book.rows.keys().map(|(o, _)| o).collect();
        if !owners.contains(&owner.to_owned()) && owners.len() >= book.max_owners {
            return Err(WorkspaceError::Capacity);
        }
        if book
            .rows
            .values()
            .filter(|r| r.view.owner == owner && !r.deleted)
            .count()
            >= MAX_WORKSPACES_PER_OWNER
            || book.rows.keys().filter(|(o, _)| o == owner).count() >= 1024
        {
            return Err(WorkspaceError::Capacity);
        }
        let view = WorkspaceView {
            id: request.id,
            owner: owner.into(),
            name: request.name,
            trust_floor: request.trust_floor,
            version: 0,
            snapshot: None,
        };
        book.persist(WorkspaceRow {
            schema_version: 1,
            usage_day: 0,
            object_actions: 0,
            fenced_objects: 0,
            view: view.clone(),
            current_key: None,
            committed_upload_id: None,
            pending: None,
            garbage: Vec::new(),
            deleted: false,
        })?;
        Ok(view)
    }

    pub fn list(&self, owner: &str) -> Vec<WorkspaceView> {
        self.book
            .lock()
            .rows
            .values()
            .filter(|r| r.view.owner == owner && !r.deleted)
            .map(|r| r.view.clone())
            .collect()
    }

    pub fn get(&self, owner: &str, id: Uuid) -> Result<WorkspaceView, WorkspaceError> {
        Ok(self.book.lock().row(owner, id)?.view.clone())
    }

    pub fn upload(
        &self,
        owner: &str,
        id: Uuid,
        request: WorkspaceUpload,
        now_ms: u64,
    ) -> Result<WorkspaceUploadGrant, WorkspaceError> {
        let expected_size = ciphertext_len(request.plaintext_bytes)
            .map_err(|e| WorkspaceError::BadRequest(e.to_string()))?;
        if request.upload_id.is_nil()
            || request.ciphertext_bytes != expected_size
            || !valid_digest(&request.ciphertext_sha256)
        {
            return Err(WorkspaceError::BadRequest(
                "invalid upload id, ciphertext size or SHA-256".into(),
            ));
        }
        let mut book = self.book.lock();
        book.reserve_object_action(owner, id, now_ms)?;
        let mut row = book.row(owner, id)?.clone();
        if row.committed_upload_id == Some(request.upload_id)
            && request.expected_version.checked_add(1) == Some(row.view.version)
            && row.view.snapshot.as_ref().is_some_and(|s| {
                s.plaintext_bytes == request.plaintext_bytes
                    && s.ciphertext_bytes == request.ciphertext_bytes
                    && s.ciphertext_sha256 == request.ciphertext_sha256
            })
        {
            return Ok(WorkspaceUploadGrant {
                upload_id: request.upload_id,
                version: row.view.version,
                grant: self.objects.put_grant(
                    row.current_key.as_deref().ok_or(WorkspaceError::NotFound)?,
                    &request,
                    now_ms,
                )?,
            });
        }
        if request.expected_version != row.view.version {
            return Err(WorkspaceError::Conflict(
                "workspace version changed; refresh before sealing another snapshot".into(),
            ));
        }
        if row.garbage.len() >= 64 {
            return Err(WorkspaceError::Capacity);
        }
        let version = row
            .view
            .version
            .checked_add(1)
            .ok_or_else(|| WorkspaceError::Conflict("workspace version exhausted".into()))?;
        if let Some(pending) = &row.pending {
            if pending.request == request
                && now_ms.saturating_add(GRANT_TTL_MS) <= pending.expires_at_ms
            {
                return Ok(WorkspaceUploadGrant {
                    upload_id: request.upload_id,
                    version,
                    grant: self.objects.put_grant(&pending.key, &request, now_ms)?,
                });
            }
            if now_ms < pending.expires_at_ms {
                return Err(WorkspaceError::Conflict(
                    "another snapshot upload is reserved, or this grant can no longer be renewed"
                        .into(),
                ));
            }
            row.garbage.push(Garbage {
                key: pending.key.clone(),
                after_ms: pending.expires_at_ms.saturating_add(CLEANUP_SKEW_MS),
                bytes: pending.request.ciphertext_bytes,
            });
        }
        if book
            .reserved_bytes()
            .saturating_add(request.ciphertext_bytes)
            > book.max_bytes
            || book.allocated_objects() >= MAX_ALLOCATED_OBJECTS
        {
            return Err(WorkspaceError::Capacity);
        }
        if !self.writes_enabled {
            return Err(WorkspaceError::Forbidden(
                "new workspace admissions are disabled".into(),
            ));
        }
        // One random object identity per reservation; no caller chooses an S3
        // key or gains a path into another owner's prefix.
        let key = format!("{}/{}/{}", sha256_hex(owner.as_bytes()), id, Uuid::new_v4());
        let grant = self.objects.put_grant(&key, &request, now_ms)?;
        row.pending = Some(PendingUpload {
            request: request.clone(),
            key,
            expires_at_ms: now_ms.saturating_add(UPLOAD_WINDOW_MS),
        });
        book.persist(row)?;
        Ok(WorkspaceUploadGrant {
            upload_id: request.upload_id,
            version,
            grant,
        })
    }

    pub async fn commit(
        &self,
        owner: &str,
        id: Uuid,
        request: CommitWorkspace,
        now_ms: u64,
    ) -> Result<WorkspaceView, WorkspaceError> {
        let pending = {
            let book = self.book.lock();
            let row = book.row(owner, id)?;
            if row.committed_upload_id == Some(request.upload_id)
                && request.expected_version.checked_add(1) == Some(row.view.version)
            {
                return Ok(row.view.clone());
            }
            let pending = row
                .pending
                .as_ref()
                .ok_or_else(|| WorkspaceError::Conflict("no upload is reserved".into()))?;
            if row.view.version != request.expected_version
                || pending.request.upload_id != request.upload_id
                || now_ms >= pending.expires_at_ms
            {
                return Err(WorkspaceError::Conflict(
                    "upload reservation is stale or expired".into(),
                ));
            }
            pending.clone()
        };
        self.book.lock().reserve_object_action(owner, id, now_ms)?;
        let check_started = std::time::Instant::now();
        let actual = self.objects.head(&pending.key, now_ms).await?;
        let checked_at_ms = now_ms.saturating_add(check_started.elapsed().as_millis() as u64);
        if actual.ciphertext_bytes != pending.request.ciphertext_bytes
            || actual.ciphertext_sha256 != pending.request.ciphertext_sha256
        {
            return Err(WorkspaceError::Conflict(
                "uploaded object does not match its committed size and SHA-256".into(),
            ));
        }
        let mut book = self.book.lock();
        let mut row = book.row(owner, id)?.clone();
        if row.committed_upload_id == Some(request.upload_id)
            && request.expected_version.checked_add(1) == Some(row.view.version)
        {
            return Ok(row.view.clone());
        }
        if row.view.version != request.expected_version
            || row.pending.as_ref() != Some(&pending)
            || checked_at_ms >= pending.expires_at_ms
        {
            return Err(WorkspaceError::Conflict(
                "workspace changed while checking upload".into(),
            ));
        }
        let version = row
            .view
            .version
            .checked_add(1)
            .ok_or_else(|| WorkspaceError::Conflict("workspace version exhausted".into()))?;
        let previous_bytes = row.view.snapshot.as_ref().map_or(0, |s| s.ciphertext_bytes);
        row.view.version = version;
        row.view.snapshot = Some(WorkspaceSnapshot {
            version,
            plaintext_bytes: pending.request.plaintext_bytes,
            ciphertext_bytes: actual.ciphertext_bytes,
            ciphertext_sha256: actual.ciphertext_sha256,
            created_at_ms: now_ms,
        });
        if let Some(key) = row.current_key.replace(pending.key) {
            row.garbage.push(Garbage {
                key,
                after_ms: now_ms.saturating_add(GRANT_TTL_MS + CLEANUP_SKEW_MS),
                bytes: previous_bytes,
            });
        }
        row.pending = None;
        row.committed_upload_id = Some(request.upload_id);
        let view = row.view.clone();
        book.persist(row)?;
        Ok(view)
    }

    pub fn download(
        &self,
        owner: &str,
        id: Uuid,
        request: DownloadWorkspace,
        now_ms: u64,
    ) -> Result<WorkspaceDownloadGrant, WorkspaceError> {
        let mut book = self.book.lock();
        let row = book.row(owner, id)?.clone();
        if row.view.version != request.version {
            return Err(WorkspaceError::Conflict(
                "requested snapshot is not the latest committed version".into(),
            ));
        }
        let key = row.current_key.as_deref().ok_or(WorkspaceError::NotFound)?;
        let mut ttl_ms = GRANT_TTL_MS;
        if let Some(job_id) = request.lease_job_id {
            let job = self
                .coordinator
                .jobs()
                .get(job_id)
                .ok_or(WorkspaceError::NotFound)?;
            if job.envelope.payload.buyer.pubkey_base58() != owner
                || job.envelope.payload.kind != covenant_compute_protocol::JobKind::LeaseSession
                || job.phase != JobPhase::Accepted
                || job
                    .envelope
                    .payload
                    .issued_at_ms
                    .saturating_add(job.envelope.payload.deadline_ms)
                    <= now_ms
                || job.close_requested_at_ms.is_some()
            {
                return Err(WorkspaceError::Forbidden(
                    "restore needs this owner's live accepted lease".into(),
                ));
            }
            let terms = covenant_compute_protocol::parse_lease_terms(&job.envelope.payload.input)
                .ok()
                .flatten()
                .ok_or_else(|| {
                    WorkspaceError::Forbidden("lease has no verified duration terms".into())
                })?;
            if job.accepted_at_ms.is_none_or(|start| {
                start.saturating_add(terms.max_duration_secs.saturating_mul(1000)) <= now_ms
            }) {
                return Err(WorkspaceError::Forbidden(
                    "lease has exhausted its paid window".into(),
                ));
            }
            let lease_end = job
                .accepted_at_ms
                .unwrap_or(0)
                .saturating_add(terms.max_duration_secs.saturating_mul(1000))
                .min(
                    job.envelope
                        .payload
                        .issued_at_ms
                        .saturating_add(job.envelope.payload.deadline_ms),
                );
            ttl_ms = ttl_ms.min(lease_end.saturating_sub(now_ms));
            if ttl_ms < 1000 {
                return Err(WorkspaceError::Forbidden(
                    "lease expires before a download grant can be used".into(),
                ));
            }
            if row.view.trust_floor > WorkspaceTrustFloor::Open {
                return Err(WorkspaceError::Forbidden(
                    "verified hardware trust is unavailable on this deployment".into(),
                ));
            }
        }
        book.reserve_object_action(owner, id, now_ms)?;
        Ok(WorkspaceDownloadGrant {
            workspace: row.view.clone(),
            grant: self.objects.get_grant(key, now_ms, ttl_ms)?,
        })
    }

    pub fn delete(&self, owner: &str, id: Uuid, now_ms: u64) -> Result<(), WorkspaceError> {
        let mut book = self.book.lock();
        let Some(mut row) = book.rows.get(&(owner.to_owned(), id)).cloned() else {
            return Ok(());
        };
        if row.deleted {
            return Ok(());
        }
        row.deleted = true;
        if let Some(key) = row.current_key.take() {
            row.garbage.push(Garbage {
                key,
                after_ms: now_ms.saturating_add(GRANT_TTL_MS + CLEANUP_SKEW_MS),
                bytes: row.view.snapshot.as_ref().map_or(0, |s| s.ciphertext_bytes),
            });
        }
        if let Some(pending) = row.pending.take() {
            row.garbage.push(Garbage {
                key: pending.key,
                after_ms: pending.expires_at_ms.saturating_add(CLEANUP_SKEW_MS),
                bytes: pending.request.ciphertext_bytes,
            });
        }
        book.persist(row)
    }

    /// Expired reservations and superseded snapshots are durable cleanup work.
    /// Wait out grants, then replace ciphertext with permanent empty fences.
    /// Expiry alone cannot stop an in-flight PUT: its signed If-None-Match
    /// condition must encounter the fence when it tries to commit. Unknown
    /// outcomes keep the full byte reservation; retrying a fence is idempotent.
    pub async fn cleanup(&self, now_ms: u64) -> Result<usize, WorkspaceError> {
        let candidates = {
            let mut book = self.book.lock();
            let keys: Vec<_> = book.rows.keys().cloned().collect();
            let mut candidates = Vec::new();
            for key in keys {
                let mut row = book.rows[&key].clone();
                if row
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.expires_at_ms <= now_ms)
                {
                    let pending = row.pending.take().expect("checked pending");
                    row.garbage.push(Garbage {
                        key: pending.key,
                        after_ms: pending.expires_at_ms.saturating_add(CLEANUP_SKEW_MS),
                        bytes: pending.request.ciphertext_bytes,
                    });
                    book.persist(row.clone())?;
                }
                candidates.extend(
                    row.garbage
                        .iter()
                        .filter(|g| g.after_ms <= now_ms)
                        .map(|g| (key.clone(), g.key.clone())),
                );
            }
            candidates
        };
        let mut deleted = 0;
        for (owner_key, object_key) in candidates {
            self.objects.fence(&object_key, now_ms).await?;
            let mut book = self.book.lock();
            let mut row = book.rows[&owner_key].clone();
            let before = row.garbage.len();
            row.garbage.retain(|g| g.key != object_key);
            row.fenced_objects = row
                .fenced_objects
                .saturating_add((before - row.garbage.len()) as u64);
            book.persist(row)?;
            deleted += 1;
        }
        Ok(deleted)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn build_from_env(
    coordinator: CoordinatorState,
    journal_path: &Path,
) -> Result<Option<Arc<WorkspaceService>>, WorkspaceError> {
    let endpoint = match std::env::var("COVENANT_COMPUTE_WORKSPACE_S3_ENDPOINT") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => return Ok(None),
    };
    let required = |name: &str| {
        std::env::var(name)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                WorkspaceError::Config(format!("{name} is required when workspaces are enabled"))
            })
    };
    let objects = S3WorkspaceObjects::new(S3WorkspaceConfig {
        endpoint,
        region: required("COVENANT_COMPUTE_WORKSPACE_S3_REGION")?,
        bucket: required("COVENANT_COMPUTE_WORKSPACE_S3_BUCKET")?,
        prefix: std::env::var("COVENANT_COMPUTE_WORKSPACE_S3_PREFIX")
            .unwrap_or_else(|_| "covenant-workspaces".into()),
        access_key: required("COVENANT_COMPUTE_WORKSPACE_S3_ACCESS_KEY")?,
        secret_key: required("COVENANT_COMPUTE_WORKSPACE_S3_SECRET_KEY")?,
        session_token: std::env::var("COVENANT_COMPUTE_WORKSPACE_S3_SESSION_TOKEN")
            .ok()
            .filter(|s| !s.is_empty()),
    })?;
    let max_owners = std::env::var("COVENANT_COMPUTE_WORKSPACE_MAX_OWNERS")
        .ok()
        .map(|s| s.parse::<usize>())
        .transpose()
        .map_err(|_| WorkspaceError::Config("invalid workspace max owners".into()))?
        .unwrap_or(1024);
    let max_bytes = std::env::var("COVENANT_COMPUTE_WORKSPACE_MAX_BYTES")
        .ok()
        .map(|s| s.parse::<u64>())
        .transpose()
        .map_err(|_| WorkspaceError::Config("invalid workspace max bytes".into()))?
        .unwrap_or(1024 * 1024 * 1024);
    let writes_enabled = match std::env::var("COVENANT_COMPUTE_WORKSPACE_WRITES_ENABLED").as_deref()
    {
        Ok("true" | "1") => true,
        Err(_) | Ok("false" | "0") => false,
        _ => {
            return Err(WorkspaceError::Config(
                "workspace writes enabled must be true or false".into(),
            ))
        }
    };
    Ok(Some(Arc::new(
        WorkspaceService::open(
            &journal_path.with_file_name("workspaces.jsonl"),
            Arc::new(objects),
            coordinator,
            max_owners,
        )?
        .with_byte_limit(max_bytes)?
        .with_writes_enabled(writes_enabled),
    )))
}

pub fn spawn_cleanup(service: Arc<WorkspaceService>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            timer.tick().await;
            if let Err(error) = service.cleanup(now_ms()).await {
                tracing::error!(%error,"workspace object cleanup failed; will retry");
            }
        }
    })
}

pub fn router(service: Arc<WorkspaceService>) -> Router {
    Router::new()
        .route("/workspaces/:owner", post(create).get(list))
        .route("/workspaces/:owner/:id", get(detail).delete(remove))
        .route("/workspaces/:owner/:id/upload", post(upload))
        .route("/workspaces/:owner/:id/commit", post(commit))
        .route("/workspaces/:owner/:id/download", post(download))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .with_state(service)
}

fn authorize(
    owner: &str,
    method: &str,
    path: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), WorkspaceError> {
    let signed_at = headers
        .get(WORKSPACE_SIGNED_AT_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| {
            WorkspaceError::Unauthorized("workspace signature timestamp is missing".into())
        })?;
    let signature = headers
        .get(WORKSPACE_SIGNATURE_HEADER)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| WorkspaceError::Unauthorized("workspace signature is missing".into()))?;
    verify_workspace(owner, method, path, body, signed_at, signature, now_ms())
        .map_err(|_| WorkspaceError::Unauthorized("workspace signature rejected".into()))
}

fn decode<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, WorkspaceError> {
    serde_json::from_slice(body).map_err(|e| WorkspaceError::BadRequest(e.to_string()))
}

async fn create(
    State(service): State<Arc<WorkspaceService>>,
    RoutePath(owner): RoutePath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<WorkspaceView>, WorkspaceError> {
    authorize(
        &owner,
        "POST",
        &format!("/workspaces/{owner}"),
        &headers,
        &body,
    )?;
    Ok(Json(service.create(&owner, decode(&body)?)?))
}
async fn list(
    State(service): State<Arc<WorkspaceService>>,
    RoutePath(owner): RoutePath<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<WorkspaceView>>, WorkspaceError> {
    authorize(
        &owner,
        "GET",
        &format!("/workspaces/{owner}"),
        &headers,
        &[],
    )?;
    Ok(Json(service.list(&owner)))
}
async fn detail(
    State(service): State<Arc<WorkspaceService>>,
    RoutePath((owner, id)): RoutePath<(String, Uuid)>,
    headers: HeaderMap,
) -> Result<Json<WorkspaceView>, WorkspaceError> {
    authorize(
        &owner,
        "GET",
        &format!("/workspaces/{owner}/{id}"),
        &headers,
        &[],
    )?;
    Ok(Json(service.get(&owner, id)?))
}
async fn remove(
    State(service): State<Arc<WorkspaceService>>,
    RoutePath((owner, id)): RoutePath<(String, Uuid)>,
    headers: HeaderMap,
) -> Result<StatusCode, WorkspaceError> {
    authorize(
        &owner,
        "DELETE",
        &format!("/workspaces/{owner}/{id}"),
        &headers,
        &[],
    )?;
    service.delete(&owner, id, now_ms())?;
    Ok(StatusCode::NO_CONTENT)
}
async fn upload(
    State(service): State<Arc<WorkspaceService>>,
    RoutePath((owner, id)): RoutePath<(String, Uuid)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<WorkspaceUploadGrant>, WorkspaceError> {
    authorize(
        &owner,
        "POST",
        &format!("/workspaces/{owner}/{id}/upload"),
        &headers,
        &body,
    )?;
    Ok(Json(service.upload(
        &owner,
        id,
        decode(&body)?,
        now_ms(),
    )?))
}
async fn commit(
    State(service): State<Arc<WorkspaceService>>,
    RoutePath((owner, id)): RoutePath<(String, Uuid)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<WorkspaceView>, WorkspaceError> {
    authorize(
        &owner,
        "POST",
        &format!("/workspaces/{owner}/{id}/commit"),
        &headers,
        &body,
    )?;
    Ok(Json(
        service.commit(&owner, id, decode(&body)?, now_ms()).await?,
    ))
}
async fn download(
    State(service): State<Arc<WorkspaceService>>,
    RoutePath((owner, id)): RoutePath<(String, Uuid)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<WorkspaceDownloadGrant>, WorkspaceError> {
    authorize(
        &owner,
        "POST",
        &format!("/workspaces/{owner}/{id}/download"),
        &headers,
        &body,
    )?;
    Ok(Json(service.download(
        &owner,
        id,
        decode(&body)?,
        now_ms(),
    )?))
}

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
