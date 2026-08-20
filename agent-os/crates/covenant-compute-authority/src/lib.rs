//! Spend authority for compute launches.
//!
//! The authority separates a user-visible quote from permission to spend:
//!
//! 1. A quote is inserted once and never updated.
//! 2. A launch authorization binds one owner and one spend account to that
//!    exact quote.
//! 3. Preparing a launch atomically consumes the authorization, reserves the
//!    quote total, creates the job, and records its idempotency key.
//! 4. A terminal provider result atomically commits the actual charge and
//!    releases the remainder, or releases the full reservation on failure.
//!
//! [`AuthorityStorage`] is the persistence boundary. Production adapters must
//! provide durable transactions for every mutating method; splitting a method
//! across independent writes violates the trait contract. This crate includes
//! [`ReferenceMemoryStorage`] only for tests and local simulation behind the
//! `reference-memory` feature. [`ComputeAuthority::new`] rejects it.

#![deny(unsafe_code)]

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;
use uuid::Uuid;

macro_rules! uuid_id {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord,
        )]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

uuid_id!(QuoteId);
uuid_id!(AuthorizationId);
uuid_id!(SpendAccountId);
uuid_id!(JobId);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct PrincipalId(String);

impl PrincipalId {
    pub fn new(value: impl Into<String>) -> Result<Self, AuthorityError> {
        let value = value.into();
        validate_token("principal", &value, 200)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PrincipalId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn new(value: impl Into<String>) -> Result<Self, AuthorityError> {
        let value = value.into();
        validate_token("idempotency key", &value, 128)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Quote {
    pub id: QuoteId,
    pub owner: PrincipalId,
    pub offer_id: String,
    pub app_id: String,
    pub workload_digest: String,
    pub rate_microusdc_per_hour: u64,
    pub duration_seconds: u64,
    pub total_microusdc: u64,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateQuote {
    pub owner: PrincipalId,
    pub offer_id: String,
    pub app_id: String,
    pub workload_digest: String,
    pub rate_microusdc_per_hour: u64,
    pub duration_seconds: u64,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpendAccount {
    pub id: SpendAccountId,
    pub owner: PrincipalId,
    pub cap_microusdc: u64,
    pub reserved_microusdc: u64,
    pub committed_microusdc: u64,
    pub created_at_ms: u64,
}

impl SpendAccount {
    pub fn available_microusdc(&self) -> u64 {
        self.cap_microusdc
            .saturating_sub(self.reserved_microusdc)
            .saturating_sub(self.committed_microusdc)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateSpendAccount {
    pub owner: PrincipalId,
    pub cap_microusdc: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchAuthorization {
    pub id: AuthorizationId,
    pub owner: PrincipalId,
    pub quote_id: QuoteId,
    pub spend_account_id: SpendAccountId,
    pub authorized_microusdc: u64,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub consumed_by: Option<JobId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizeLaunch {
    pub owner: PrincipalId,
    pub quote_id: QuoteId,
    pub spend_account_id: SpendAccountId,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Prepared,
    Running,
    CancelRequested,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComputeJob {
    pub id: JobId,
    pub owner: PrincipalId,
    pub quote_id: QuoteId,
    pub authorization_id: AuthorizationId,
    pub spend_account_id: SpendAccountId,
    pub idempotency_key: IdempotencyKey,
    pub reserved_microusdc: u64,
    pub committed_microusdc: Option<u64>,
    pub state: JobState,
    pub provider_job_id: Option<String>,
    pub failure_code: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepareLaunch {
    pub owner: PrincipalId,
    pub quote_id: QuoteId,
    pub authorization_id: AuthorizationId,
    pub spend_account_id: SpendAccountId,
    pub idempotency_key: IdempotencyKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeginLaunch {
    pub job_id: JobId,
    pub owner: PrincipalId,
    pub quote_id: QuoteId,
    pub authorization_id: AuthorizationId,
    pub spend_account_id: SpendAccountId,
    pub idempotency_key: IdempotencyKey,
    pub now_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchDisposition {
    Created,
    Replay,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchResult {
    pub job: ComputeJob,
    pub disposition: LaunchDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalOutcome {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSettlement {
    pub outcome: TerminalOutcome,
    pub charge_microusdc: u64,
    pub failure_code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageDurability {
    Durable,
    EphemeralReference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resource {
    Quote,
    SpendAccount,
    Authorization,
    Job,
}

impl fmt::Display for Resource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Quote => formatter.write_str("quote"),
            Self::SpendAccount => formatter.write_str("spend account"),
            Self::Authorization => formatter.write_str("authorization"),
            Self::Job => formatter.write_str("job"),
        }
    }
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum AuthorityError {
    #[error("invalid {field}: {reason}")]
    InvalidInput {
        field: &'static str,
        reason: &'static str,
    },
    #[error("{0} not found")]
    NotFound(Resource),
    #[error("{0} already exists")]
    AlreadyExists(Resource),
    #[error("operation is not permitted for this owner")]
    ForeignOwner,
    #[error("quote has expired")]
    QuoteExpired,
    #[error("authorization has expired")]
    AuthorizationExpired,
    #[error("authorization does not match the launch scope")]
    AuthorizationScopeMismatch,
    #[error("authorization has already been consumed")]
    AuthorizationConsumed,
    #[error(
        "spend cap exceeded: cap {cap_microusdc}, committed {committed_microusdc}, reserved {reserved_microusdc}, requested {requested_microusdc} micro-USDC"
    )]
    SpendCapExceeded {
        cap_microusdc: u64,
        committed_microusdc: u64,
        reserved_microusdc: u64,
        requested_microusdc: u64,
    },
    #[error("idempotency key was already used for a different launch")]
    IdempotencyConflict,
    #[error("cannot {operation} a job in state {state:?}")]
    InvalidJobState {
        operation: &'static str,
        state: JobState,
    },
    #[error("charge {charge_microusdc} exceeds reservation {reserved_microusdc} micro-USDC")]
    ChargeExceedsReservation {
        charge_microusdc: u64,
        reserved_microusdc: u64,
    },
    #[error("arithmetic overflow")]
    ArithmeticOverflow,
    #[error("ephemeral reference storage is not accepted by the production constructor")]
    EphemeralStorageRejected,
    #[error("storage backend failed: {0}")]
    Storage(String),
}

/// Durable persistence contract for the compute authority.
///
/// `begin_launch`, `request_cancel`, `mark_running`, `settle_job`, and
/// `fail_launch` are transactions. Each must commit all affected records or none.
/// In particular, `begin_launch` may not expose a consumed authorization
/// without the matching reservation, job, and idempotency record.
#[async_trait]
pub trait AuthorityStorage: Send + Sync + 'static {
    fn durability(&self) -> StorageDurability;

    async fn insert_quote(&self, quote: Quote) -> Result<(), AuthorityError>;
    async fn quote(&self, id: QuoteId) -> Result<Quote, AuthorityError>;

    async fn insert_spend_account(&self, account: SpendAccount) -> Result<(), AuthorityError>;
    async fn spend_account(&self, id: SpendAccountId) -> Result<SpendAccount, AuthorityError>;

    async fn insert_authorization(
        &self,
        authorization: LaunchAuthorization,
    ) -> Result<(), AuthorityError>;
    async fn authorization(
        &self,
        id: AuthorizationId,
    ) -> Result<LaunchAuthorization, AuthorityError>;

    async fn begin_launch(&self, launch: BeginLaunch) -> Result<LaunchResult, AuthorityError>;
    async fn job(&self, id: JobId) -> Result<ComputeJob, AuthorityError>;

    async fn request_cancel(
        &self,
        owner: &PrincipalId,
        id: JobId,
        now_ms: u64,
    ) -> Result<ComputeJob, AuthorityError>;

    async fn mark_running(
        &self,
        id: JobId,
        provider_job_id: String,
        now_ms: u64,
    ) -> Result<ComputeJob, AuthorityError>;

    async fn settle_job(
        &self,
        id: JobId,
        settlement: JobSettlement,
        now_ms: u64,
    ) -> Result<ComputeJob, AuthorityError>;

    async fn fail_launch(
        &self,
        id: JobId,
        failure_code: String,
        now_ms: u64,
    ) -> Result<ComputeJob, AuthorityError>;
}

pub struct ComputeAuthority<S> {
    storage: Arc<S>,
}

impl<S> Clone for ComputeAuthority<S> {
    fn clone(&self) -> Self {
        Self {
            storage: Arc::clone(&self.storage),
        }
    }
}

impl<S: AuthorityStorage> ComputeAuthority<S> {
    pub fn new(storage: Arc<S>) -> Result<Self, AuthorityError> {
        if storage.durability() != StorageDurability::Durable {
            return Err(AuthorityError::EphemeralStorageRejected);
        }
        Ok(Self { storage })
    }

    #[cfg(any(test, feature = "reference-memory"))]
    pub fn new_reference(storage: Arc<S>) -> Self {
        assert_eq!(
            storage.durability(),
            StorageDurability::EphemeralReference,
            "new_reference only accepts ephemeral reference storage"
        );
        Self { storage }
    }

    pub async fn create_quote(
        &self,
        request: CreateQuote,
        now_ms: u64,
    ) -> Result<Quote, AuthorityError> {
        validate_token("offer_id", &request.offer_id, 200)?;
        validate_token("app_id", &request.app_id, 200)?;
        validate_digest(&request.workload_digest)?;
        if request.rate_microusdc_per_hour == 0 {
            return Err(invalid("rate_microusdc_per_hour", "must be non-zero"));
        }
        if request.duration_seconds == 0 {
            return Err(invalid("duration_seconds", "must be non-zero"));
        }
        if request.expires_at_ms <= now_ms {
            return Err(invalid("expires_at_ms", "must be in the future"));
        }

        let total_microusdc =
            quote_maximum(request.rate_microusdc_per_hour, request.duration_seconds)?;
        let quote = Quote {
            id: QuoteId::new(),
            owner: request.owner,
            offer_id: request.offer_id,
            app_id: request.app_id,
            workload_digest: request.workload_digest,
            rate_microusdc_per_hour: request.rate_microusdc_per_hour,
            duration_seconds: request.duration_seconds,
            total_microusdc,
            created_at_ms: now_ms,
            expires_at_ms: request.expires_at_ms,
        };
        self.storage.insert_quote(quote.clone()).await?;
        Ok(quote)
    }

    pub async fn quote(&self, id: QuoteId) -> Result<Quote, AuthorityError> {
        self.storage.quote(id).await
    }

    pub async fn create_spend_account(
        &self,
        request: CreateSpendAccount,
        now_ms: u64,
    ) -> Result<SpendAccount, AuthorityError> {
        if request.cap_microusdc == 0 {
            return Err(invalid("cap_microusdc", "must be non-zero"));
        }
        let account = SpendAccount {
            id: SpendAccountId::new(),
            owner: request.owner,
            cap_microusdc: request.cap_microusdc,
            reserved_microusdc: 0,
            committed_microusdc: 0,
            created_at_ms: now_ms,
        };
        self.storage.insert_spend_account(account.clone()).await?;
        Ok(account)
    }

    pub async fn spend_account(&self, id: SpendAccountId) -> Result<SpendAccount, AuthorityError> {
        self.storage.spend_account(id).await
    }

    pub async fn authorize_launch(
        &self,
        request: AuthorizeLaunch,
        now_ms: u64,
    ) -> Result<LaunchAuthorization, AuthorityError> {
        if request.expires_at_ms <= now_ms {
            return Err(invalid("expires_at_ms", "must be in the future"));
        }
        let quote = self.storage.quote(request.quote_id).await?;
        let account = self.storage.spend_account(request.spend_account_id).await?;
        if quote.owner != request.owner || account.owner != request.owner {
            return Err(AuthorityError::ForeignOwner);
        }
        if quote.expires_at_ms <= now_ms {
            return Err(AuthorityError::QuoteExpired);
        }
        let expires_at_ms = request.expires_at_ms.min(quote.expires_at_ms);
        let authorization = LaunchAuthorization {
            id: AuthorizationId::new(),
            owner: request.owner,
            quote_id: quote.id,
            spend_account_id: account.id,
            authorized_microusdc: quote.total_microusdc,
            created_at_ms: now_ms,
            expires_at_ms,
            consumed_by: None,
        };
        self.storage
            .insert_authorization(authorization.clone())
            .await?;
        Ok(authorization)
    }

    pub async fn authorization(
        &self,
        id: AuthorizationId,
    ) -> Result<LaunchAuthorization, AuthorityError> {
        self.storage.authorization(id).await
    }

    pub async fn prepare_launch(
        &self,
        request: PrepareLaunch,
        now_ms: u64,
    ) -> Result<LaunchResult, AuthorityError> {
        self.storage
            .begin_launch(BeginLaunch {
                job_id: JobId::new(),
                owner: request.owner,
                quote_id: request.quote_id,
                authorization_id: request.authorization_id,
                spend_account_id: request.spend_account_id,
                idempotency_key: request.idempotency_key,
                now_ms,
            })
            .await
    }

    pub async fn job(&self, id: JobId) -> Result<ComputeJob, AuthorityError> {
        self.storage.job(id).await
    }

    pub async fn request_cancel(
        &self,
        owner: &PrincipalId,
        id: JobId,
        now_ms: u64,
    ) -> Result<ComputeJob, AuthorityError> {
        self.storage.request_cancel(owner, id, now_ms).await
    }

    pub async fn mark_running(
        &self,
        id: JobId,
        provider_job_id: impl Into<String>,
        now_ms: u64,
    ) -> Result<ComputeJob, AuthorityError> {
        let provider_job_id = provider_job_id.into();
        validate_token("provider_job_id", &provider_job_id, 300)?;
        self.storage.mark_running(id, provider_job_id, now_ms).await
    }

    pub async fn settle_job(
        &self,
        id: JobId,
        settlement: JobSettlement,
        now_ms: u64,
    ) -> Result<ComputeJob, AuthorityError> {
        match (&settlement.outcome, &settlement.failure_code) {
            (TerminalOutcome::Failed, Some(code)) => {
                validate_token("failure_code", code, 100)?;
            }
            (TerminalOutcome::Failed, None) => {
                return Err(invalid(
                    "failure_code",
                    "is required for a failed settlement",
                ));
            }
            (_, Some(_)) => {
                return Err(invalid(
                    "failure_code",
                    "is only valid for a failed settlement",
                ));
            }
            (_, None) => {}
        }
        self.storage.settle_job(id, settlement, now_ms).await
    }

    pub async fn fail_launch(
        &self,
        id: JobId,
        failure_code: impl Into<String>,
        now_ms: u64,
    ) -> Result<ComputeJob, AuthorityError> {
        let failure_code = failure_code.into();
        validate_token("failure_code", &failure_code, 100)?;
        self.storage.fail_launch(id, failure_code, now_ms).await
    }
}

fn validate_token(field: &'static str, value: &str, max_len: usize) -> Result<(), AuthorityError> {
    if value.trim().is_empty() {
        return Err(invalid(field, "must not be empty"));
    }
    if value.len() > max_len {
        return Err(invalid(field, "is too long"));
    }
    if value.chars().any(char::is_control) {
        return Err(invalid(field, "must not contain control characters"));
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), AuthorityError> {
    validate_token("workload_digest", value, 200)?;
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(invalid("workload_digest", "must be a sha256 digest"));
    };
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("workload_digest", "must be a sha256 digest"));
    }
    Ok(())
}

fn quote_maximum(rate_per_hour: u64, duration_seconds: u64) -> Result<u64, AuthorityError> {
    rate_per_hour
        .checked_mul(duration_seconds)
        .and_then(|value| value.checked_add(3_599))
        .and_then(|value| value.checked_div(3_600))
        .ok_or(AuthorityError::ArithmeticOverflow)
}

const fn invalid(field: &'static str, reason: &'static str) -> AuthorityError {
    AuthorityError::InvalidInput { field, reason }
}

#[cfg(any(test, feature = "reference-memory"))]
mod reference_memory;

#[cfg(any(test, feature = "reference-memory"))]
pub use reference_memory::ReferenceMemoryStorage;

#[cfg(test)]
mod tests;
