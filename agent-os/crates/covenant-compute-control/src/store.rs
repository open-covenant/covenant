use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use covenant_compute::{ComputeJob, ComputeReceipt, JobStatus, LaunchPlan};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::provider::ProviderJob;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitDisposition {
    Created,
    Replay,
}

#[derive(Debug, Clone)]
pub struct SubmitResult {
    pub job: StoredJob,
    pub disposition: SubmitDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoredState {
    Prepared,
    Provisioning,
    Running,
    CancelRequested,
    Succeeded,
    Failed,
    Cancelled,
}

impl StoredState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Provisioning => "provisioning",
            Self::Running => "running",
            Self::CancelRequested => "cancel_requested",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "prepared" => Ok(Self::Prepared),
            "provisioning" => Ok(Self::Provisioning),
            "running" => Ok(Self::Running),
            "cancel_requested" => Ok(Self::CancelRequested),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(StoreError::Corrupt),
        }
    }

    fn terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone)]
pub struct StoredJob {
    pub id: String,
    pub owner: String,
    pub idempotency_key: String,
    pub plan: LaunchPlan,
    state: StoredState,
    pub provider_job_id: Option<String>,
    pub committed_usdc_micros: Option<u64>,
    pub error: Option<String>,
    pub receipt: Option<ComputeReceipt>,
    pub created_at_ms: u64,
    pub ready_at_ms: Option<u64>,
}

impl StoredJob {
    pub fn wire(&self) -> ComputeJob {
        let status = match self.state {
            StoredState::Prepared => JobStatus::Funding,
            StoredState::Provisioning => JobStatus::Provisioning,
            StoredState::Running => JobStatus::Running,
            StoredState::CancelRequested => JobStatus::Stopping,
            StoredState::Succeeded => JobStatus::Completed,
            StoredState::Failed => JobStatus::Failed,
            StoredState::Cancelled => JobStatus::Cancelled,
        };
        ComputeJob {
            id: self.id.clone(),
            app_id: self.plan.app.id.clone(),
            offer_id: self.plan.offer.id.clone(),
            status,
            maximum_usdc_micros: self.plan.maximum_usdc_micros,
            access_url: None,
            error: self.error.clone(),
            receipt: self.receipt.clone(),
        }
    }

    pub fn is_prepared(&self) -> bool {
        self.state == StoredState::Prepared
    }

    pub fn is_cancel_requested(&self) -> bool {
        self.state == StoredState::CancelRequested
    }

    pub fn is_terminal(&self) -> bool {
        self.state.terminal()
    }

    pub fn deadline_reached(&self, now_ms: u64) -> bool {
        self.plan
            .duration_secs
            .checked_mul(1_000)
            .and_then(|duration| self.created_at_ms.checked_add(duration))
            .is_none_or(|deadline| now_ms >= deadline)
    }
}

#[derive(Clone)]
pub struct SqliteStore {
    path: Arc<PathBuf>,
}

impl SqliteStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        if path.as_os_str().is_empty() || path == Path::new(":memory:") {
            return Err(StoreError::NonDurablePath);
        }
        let store = Self {
            path: Arc::new(path),
        };
        store
            .run(|connection| {
                connection.execute_batch(
                    r#"
                    PRAGMA journal_mode = WAL;
                    PRAGMA foreign_keys = ON;
                    PRAGMA synchronous = FULL;

                    CREATE TABLE IF NOT EXISTS spend_accounts (
                        owner TEXT PRIMARY KEY,
                        cap_usdc_micros INTEGER NOT NULL CHECK (cap_usdc_micros > 0),
                        reserved_usdc_micros INTEGER NOT NULL DEFAULT 0 CHECK (reserved_usdc_micros >= 0),
                        committed_usdc_micros INTEGER NOT NULL DEFAULT 0 CHECK (committed_usdc_micros >= 0),
                        created_at_ms INTEGER NOT NULL,
                        CHECK (reserved_usdc_micros + committed_usdc_micros <= cap_usdc_micros)
                    );

                    CREATE TABLE IF NOT EXISTS quotes (
                        id TEXT PRIMARY KEY,
                        owner TEXT NOT NULL,
                        offer_id TEXT NOT NULL,
                        app_id TEXT NOT NULL,
                        workload_digest TEXT NOT NULL,
                        rate_usdc_micros_per_hour INTEGER NOT NULL,
                        duration_secs INTEGER NOT NULL,
                        total_usdc_micros INTEGER NOT NULL,
                        created_at_ms INTEGER NOT NULL
                    );

                    CREATE TABLE IF NOT EXISTS launch_authorizations (
                        id TEXT PRIMARY KEY,
                        owner TEXT NOT NULL,
                        quote_id TEXT NOT NULL UNIQUE REFERENCES quotes(id),
                        authorized_usdc_micros INTEGER NOT NULL,
                        consumed_by TEXT NOT NULL UNIQUE,
                        created_at_ms INTEGER NOT NULL
                    );

                    CREATE TABLE IF NOT EXISTS jobs (
                        id TEXT PRIMARY KEY,
                        owner TEXT NOT NULL REFERENCES spend_accounts(owner),
                        quote_id TEXT NOT NULL UNIQUE REFERENCES quotes(id),
                        authorization_id TEXT NOT NULL UNIQUE REFERENCES launch_authorizations(id),
                        idempotency_key TEXT NOT NULL,
                        request_fingerprint TEXT NOT NULL,
                        plan_json TEXT NOT NULL,
                        state TEXT NOT NULL,
                        provider_job_id TEXT,
                        reserved_usdc_micros INTEGER NOT NULL,
                        committed_usdc_micros INTEGER,
                        error TEXT,
                        receipt_json TEXT,
                        created_at_ms INTEGER NOT NULL,
                        updated_at_ms INTEGER NOT NULL,
                        ready_at_ms INTEGER,
                        UNIQUE (owner, idempotency_key)
                    );

                    CREATE INDEX IF NOT EXISTS jobs_owner_created
                    ON jobs(owner, created_at_ms DESC);
                    "#,
                )?;
                // Pre-existing databases lack ready_at_ms; a duplicate-column
                // error means the schema above already carries it.
                if let Err(error) =
                    connection.execute("ALTER TABLE jobs ADD COLUMN ready_at_ms INTEGER", [])
                {
                    if !error.to_string().contains("duplicate column") {
                        return Err(error.into());
                    }
                }
                Ok(())
            })
            .await?;
        Ok(store)
    }

    pub async fn submit(
        &self,
        owner: &str,
        spend_cap_usdc_micros: u64,
        idempotency_key: &str,
        plan: &LaunchPlan,
        now_ms: u64,
    ) -> Result<SubmitResult, StoreError> {
        let owner = owner.to_owned();
        let idempotency_key = idempotency_key.to_owned();
        let plan = plan.clone();
        self.run(move |connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let fingerprint = plan_fingerprint(&plan)?;

            let replay = transaction
                .query_row(
                    "SELECT request_fingerprint, id FROM jobs WHERE owner = ?1 AND idempotency_key = ?2",
                    params![owner, idempotency_key],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?;
            if let Some((existing_fingerprint, job_id)) = replay {
                if existing_fingerprint != fingerprint {
                    return Err(StoreError::IdempotencyConflict);
                }
                let job = load_job(&transaction, &job_id)?;
                transaction.commit()?;
                return Ok(SubmitResult {
                    job,
                    disposition: SubmitDisposition::Replay,
                });
            }

            let cap = sql_u64(spend_cap_usdc_micros)?;
            let reserved = sql_u64(plan.maximum_usdc_micros)?;
            let timestamp = sql_u64(now_ms)?;
            transaction.execute(
                "INSERT OR IGNORE INTO spend_accounts
                 (owner, cap_usdc_micros, reserved_usdc_micros, committed_usdc_micros, created_at_ms)
                 VALUES (?1, ?2, 0, 0, ?3)",
                params![owner, cap, timestamp],
            )?;
            let configured_cap: i64 = transaction.query_row(
                "SELECT cap_usdc_micros FROM spend_accounts WHERE owner = ?1",
                params![owner],
                |row| row.get(0),
            )?;
            if configured_cap != cap {
                return Err(StoreError::SpendCapChanged);
            }
            let updated = transaction.execute(
                "UPDATE spend_accounts
                 SET reserved_usdc_micros = reserved_usdc_micros + ?2
                 WHERE owner = ?1
                   AND reserved_usdc_micros + committed_usdc_micros + ?2 <= cap_usdc_micros",
                params![owner, reserved],
            )?;
            if updated != 1 {
                return Err(StoreError::SpendCapExceeded);
            }

            let job_id = Uuid::new_v4().to_string();
            let quote_id = Uuid::new_v4().to_string();
            let authorization_id = Uuid::new_v4().to_string();
            let image = plan
                .app
                .image
                .as_deref()
                .ok_or(StoreError::InvalidPlan)?;
            let digest = image
                .rsplit_once('@')
                .map(|(_, digest)| digest)
                .ok_or(StoreError::InvalidPlan)?;
            let rate = sql_u64(plan.offer.rate_usdc_micros_per_hour)?;
            let duration = sql_u64(plan.duration_secs)?;
            let plan_json = serde_json::to_string(&plan).map_err(|_| StoreError::Serialization)?;

            transaction.execute(
                "INSERT INTO quotes
                 (id, owner, offer_id, app_id, workload_digest, rate_usdc_micros_per_hour,
                  duration_secs, total_usdc_micros, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    quote_id,
                    owner,
                    plan.offer.id,
                    plan.app.id,
                    digest,
                    rate,
                    duration,
                    reserved,
                    timestamp
                ],
            )?;
            transaction.execute(
                "INSERT INTO launch_authorizations
                 (id, owner, quote_id, authorized_usdc_micros, consumed_by, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    authorization_id,
                    owner,
                    quote_id,
                    reserved,
                    job_id,
                    timestamp
                ],
            )?;
            transaction.execute(
                "INSERT INTO jobs
                 (id, owner, quote_id, authorization_id, idempotency_key, request_fingerprint,
                  plan_json, state, reserved_usdc_micros, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'prepared', ?8, ?9, ?9)",
                params![
                    job_id,
                    owner,
                    quote_id,
                    authorization_id,
                    idempotency_key,
                    fingerprint,
                    plan_json,
                    reserved,
                    timestamp
                ],
            )?;
            let job = load_job(&transaction, &job_id)?;
            transaction.commit()?;
            Ok(SubmitResult {
                job,
                disposition: SubmitDisposition::Created,
            })
        })
        .await
    }

    pub async fn job(&self, owner: &str, id: &str) -> Result<StoredJob, StoreError> {
        let owner = owner.to_owned();
        let id = id.to_owned();
        self.run(move |connection| {
            let job = load_job(connection, &id)?;
            if job.owner != owner {
                return Err(StoreError::NotFound);
            }
            Ok(job)
        })
        .await
    }

    pub async fn jobs(&self, owner: &str) -> Result<Vec<StoredJob>, StoreError> {
        let owner = owner.to_owned();
        self.run(move |connection| {
            let mut statement = connection.prepare(
                "SELECT id FROM jobs WHERE owner = ?1 ORDER BY created_at_ms DESC, id DESC",
            )?;
            let ids = statement
                .query_map(params![owner], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            ids.into_iter()
                .map(|id| load_job(connection, &id))
                .collect()
        })
        .await
    }

    pub async fn recoverable_jobs(&self) -> Result<Vec<StoredJob>, StoreError> {
        self.run(move |connection| {
            let mut statement = connection.prepare(
                "SELECT id FROM jobs
                 WHERE state IN ('prepared', 'provisioning', 'running', 'cancel_requested')
                 ORDER BY created_at_ms, id",
            )?;
            let ids = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            ids.into_iter()
                .map(|id| load_job(connection, &id))
                .collect()
        })
        .await
    }

    pub async fn request_cancel(
        &self,
        owner: &str,
        id: &str,
        now_ms: u64,
    ) -> Result<StoredJob, StoreError> {
        let owner = owner.to_owned();
        let id = id.to_owned();
        self.run(move |connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current = load_job(&transaction, &id)?;
            if current.owner != owner {
                return Err(StoreError::NotFound);
            }
            if !current.state.terminal() && current.state != StoredState::CancelRequested {
                transaction.execute(
                    "UPDATE jobs SET state = 'cancel_requested', updated_at_ms = ?2 WHERE id = ?1",
                    params![id, sql_u64(now_ms)?],
                )?;
            }
            let job = load_job(&transaction, &id)?;
            transaction.commit()?;
            Ok(job)
        })
        .await
    }

    pub async fn record_provider(
        &self,
        id: &str,
        provider: ProviderJob,
        now_ms: u64,
    ) -> Result<StoredJob, StoreError> {
        let id = id.to_owned();
        self.run(move |connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current = load_job(&transaction, &id)?;
            if current.state.terminal() {
                transaction.commit()?;
                return Ok(current);
            }
            validate_provider_job(&current, &provider)?;

            if provider.status.terminal() {
                settle(&transaction, &current, &provider, now_ms)?;
            } else {
                let state = match (current.state, provider.status) {
                    (StoredState::CancelRequested, _) | (_, JobStatus::Stopping) => {
                        StoredState::CancelRequested
                    }
                    (_, JobStatus::Funding | JobStatus::Provisioning) => StoredState::Provisioning,
                    (_, JobStatus::Running) => StoredState::Running,
                    _ => return Err(StoreError::InvalidProviderState),
                };
                transaction.execute(
                    "UPDATE jobs
                     SET state = ?2, provider_job_id = ?3, error = ?4, updated_at_ms = ?5,
                         ready_at_ms = CASE
                             WHEN ?6 AND ready_at_ms IS NULL THEN ?5
                             ELSE ready_at_ms
                         END
                     WHERE id = ?1",
                    params![
                        id,
                        state.as_str(),
                        provider.id,
                        provider.error,
                        sql_u64(now_ms)?,
                        state == StoredState::Running,
                    ],
                )?;
            }
            let job = load_job(&transaction, &id)?;
            transaction.commit()?;
            Ok(job)
        })
        .await
    }

    pub async fn fail_launch(
        &self,
        id: &str,
        failure_code: &str,
        now_ms: u64,
    ) -> Result<StoredJob, StoreError> {
        let id = id.to_owned();
        let failure_code = failure_code.to_owned();
        self.run(move |connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current = load_job(&transaction, &id)?;
            if current.state == StoredState::Failed {
                transaction.commit()?;
                return Ok(current);
            }
            if current.state != StoredState::Prepared || current.provider_job_id.is_some() {
                return Err(StoreError::InvalidState);
            }
            release_reservation(&transaction, &current, 0)?;
            transaction.execute(
                "UPDATE jobs
                 SET state = 'failed', committed_usdc_micros = 0, error = ?2, updated_at_ms = ?3
                 WHERE id = ?1",
                params![id, failure_code, sql_u64(now_ms)?],
            )?;
            let job = load_job(&transaction, &id)?;
            transaction.commit()?;
            Ok(job)
        })
        .await
    }

    async fn run<T, F>(&self, operation: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StoreError> + Send + 'static,
    {
        let path = Arc::clone(&self.path);
        tokio::task::spawn_blocking(move || {
            let mut connection = Connection::open(path.as_ref())?;
            connection.busy_timeout(Duration::from_secs(5))?;
            connection.pragma_update(None, "foreign_keys", "ON")?;
            operation(&mut connection)
        })
        .await
        .map_err(|_| StoreError::Worker)?
    }
}

fn load_job(connection: &Connection, id: &str) -> Result<StoredJob, StoreError> {
    connection
        .query_row(
            "SELECT owner, idempotency_key, plan_json, state, provider_job_id,
                    committed_usdc_micros, error, receipt_json, created_at_ms, ready_at_ms
             FROM jobs WHERE id = ?1",
            params![id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                ))
            },
        )
        .optional()?
        .ok_or(StoreError::NotFound)
        .and_then(
            |(
                owner,
                idempotency_key,
                plan_json,
                state,
                provider_job_id,
                committed,
                error,
                receipt_json,
                created_at_ms,
                ready_at_ms,
            )| {
                Ok(StoredJob {
                    id: id.to_owned(),
                    owner,
                    idempotency_key,
                    plan: serde_json::from_str(&plan_json).map_err(|_| StoreError::Corrupt)?,
                    state: StoredState::parse(&state)?,
                    provider_job_id,
                    committed_usdc_micros: committed.map(read_u64).transpose()?,
                    error,
                    receipt: receipt_json
                        .map(|json| serde_json::from_str(&json).map_err(|_| StoreError::Corrupt))
                        .transpose()?,
                    created_at_ms: read_u64(created_at_ms)?,
                    ready_at_ms: ready_at_ms.map(read_u64).transpose()?,
                })
            },
        )
}

fn validate_provider_job(job: &StoredJob, provider: &ProviderJob) -> Result<(), StoreError> {
    if !valid_provider_text(&provider.id, 300)
        || provider
            .access_url
            .as_deref()
            .is_some_and(|value| !valid_provider_text(value, 4_096))
        || provider
            .error
            .as_deref()
            .is_some_and(|value| !valid_provider_text(value, 200))
    {
        return Err(StoreError::InvalidProviderState);
    }
    if let Some(receipt) = &provider.receipt {
        if !valid_provider_text(&receipt.id, 300)
            || receipt.job_id != job.id
            || receipt.app_id != job.plan.app.id
            || !valid_provider_text(&receipt.provider, 100)
            || receipt.runtime_secs > job.plan.duration_secs
            || receipt.charged_usdc_micros > job.plan.maximum_usdc_micros
            || receipt
                .charged_usdc_micros
                .checked_add(receipt.refunded_usdc_micros)
                != Some(job.plan.maximum_usdc_micros)
            || !valid_provider_text(&receipt.commitment, 500)
            || receipt
                .transaction
                .as_deref()
                .is_some_and(|value| !valid_provider_text(value, 500))
        {
            return Err(StoreError::InvalidProviderState);
        }
    }
    if matches!(provider.status, JobStatus::Completed | JobStatus::Cancelled)
        && provider.receipt.is_none()
    {
        return Err(StoreError::InvalidProviderState);
    }
    if !provider.status.terminal() && provider.receipt.is_some() {
        return Err(StoreError::InvalidProviderState);
    }
    Ok(())
}

fn valid_provider_text(value: &str, max_len: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max_len && !value.chars().any(char::is_control)
}

fn settle(
    transaction: &Transaction<'_>,
    current: &StoredJob,
    provider: &ProviderJob,
    now_ms: u64,
) -> Result<(), StoreError> {
    let charge = provider
        .receipt
        .as_ref()
        .map_or(0, |receipt| receipt.charged_usdc_micros);
    release_reservation(transaction, current, charge)?;
    let state = match provider.status {
        JobStatus::Completed => StoredState::Succeeded,
        JobStatus::Cancelled => StoredState::Cancelled,
        JobStatus::Failed => StoredState::Failed,
        _ => return Err(StoreError::InvalidProviderState),
    };
    let receipt_json = provider
        .receipt
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|_| StoreError::Serialization)?;
    transaction.execute(
        "UPDATE jobs
         SET state = ?2, provider_job_id = ?3,
             committed_usdc_micros = ?4, error = ?5, receipt_json = ?6, updated_at_ms = ?7
         WHERE id = ?1",
        params![
            current.id,
            state.as_str(),
            provider.id,
            sql_u64(charge)?,
            provider.error,
            receipt_json,
            sql_u64(now_ms)?
        ],
    )?;
    Ok(())
}

fn release_reservation(
    transaction: &Transaction<'_>,
    current: &StoredJob,
    charge: u64,
) -> Result<(), StoreError> {
    if charge > current.plan.maximum_usdc_micros {
        return Err(StoreError::InvalidProviderState);
    }
    let updated = transaction.execute(
        "UPDATE spend_accounts
         SET reserved_usdc_micros = reserved_usdc_micros - ?2,
             committed_usdc_micros = committed_usdc_micros + ?3
         WHERE owner = ?1 AND reserved_usdc_micros >= ?2",
        params![
            current.owner,
            sql_u64(current.plan.maximum_usdc_micros)?,
            sql_u64(charge)?
        ],
    )?;
    if updated != 1 {
        return Err(StoreError::Corrupt);
    }
    Ok(())
}

fn plan_fingerprint(plan: &LaunchPlan) -> Result<String, StoreError> {
    let encoded = serde_json::to_vec(plan).map_err(|_| StoreError::Serialization)?;
    Ok(hex(&Sha256::digest(encoded)))
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn sql_u64(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::AmountOutOfRange)
}

fn read_u64(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::Corrupt)
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("SQLite database path must be durable")]
    NonDurablePath,
    #[error("compute record was not found")]
    NotFound,
    #[error("idempotency key was already used for a different launch")]
    IdempotencyConflict,
    #[error("spend cap is exhausted")]
    SpendCapExceeded,
    #[error("configured spend cap differs from the durable account")]
    SpendCapChanged,
    #[error("amount exceeds the durable store range")]
    AmountOutOfRange,
    #[error("launch plan is invalid")]
    InvalidPlan,
    #[error("job state does not permit this operation")]
    InvalidState,
    #[error("provider returned invalid job state")]
    InvalidProviderState,
    #[error("durable compute data is corrupt")]
    Corrupt,
    #[error("compute data serialization failed")]
    Serialization,
    #[error("SQLite operation failed")]
    Sqlite(#[from] rusqlite::Error),
    #[error("SQLite worker failed")]
    Worker,
}
