use super::*;
use std::collections::HashMap;
use tokio::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
struct LaunchIdentity {
    quote_id: QuoteId,
    authorization_id: AuthorizationId,
    spend_account_id: SpendAccountId,
}

#[derive(Debug, Clone)]
struct IdempotencyRecord {
    identity: LaunchIdentity,
    job_id: JobId,
}

#[derive(Default)]
struct State {
    quotes: HashMap<QuoteId, Quote>,
    accounts: HashMap<SpendAccountId, SpendAccount>,
    authorizations: HashMap<AuthorizationId, LaunchAuthorization>,
    jobs: HashMap<JobId, ComputeJob>,
    idempotency: HashMap<(PrincipalId, IdempotencyKey), IdempotencyRecord>,
}

/// Ephemeral, process-local reference implementation of [`AuthorityStorage`].
///
/// It exists to exercise the transactional contract in tests and simulations.
/// It loses every authorization, reservation, and idempotency record on process
/// exit. The type is excluded from default builds, and
/// [`ComputeAuthority::new`] rejects it even when its feature is enabled.
#[derive(Default)]
pub struct ReferenceMemoryStorage {
    state: Mutex<State>,
}

impl ReferenceMemoryStorage {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl AuthorityStorage for ReferenceMemoryStorage {
    fn durability(&self) -> StorageDurability {
        StorageDurability::EphemeralReference
    }

    async fn insert_quote(&self, quote: Quote) -> Result<(), AuthorityError> {
        let expected_total = quote_maximum(quote.rate_microusdc_per_hour, quote.duration_seconds)?;
        if quote.total_microusdc != expected_total {
            return Err(invalid(
                "total_microusdc",
                "must equal rate multiplied by duration",
            ));
        }

        let mut state = self.state.lock().await;
        if state.quotes.contains_key(&quote.id) {
            return Err(AuthorityError::AlreadyExists(Resource::Quote));
        }
        state.quotes.insert(quote.id, quote);
        Ok(())
    }

    async fn quote(&self, id: QuoteId) -> Result<Quote, AuthorityError> {
        self.state
            .lock()
            .await
            .quotes
            .get(&id)
            .cloned()
            .ok_or(AuthorityError::NotFound(Resource::Quote))
    }

    async fn insert_spend_account(&self, account: SpendAccount) -> Result<(), AuthorityError> {
        if account.cap_microusdc == 0
            || account.reserved_microusdc != 0
            || account.committed_microusdc != 0
        {
            return Err(invalid(
                "spend_account",
                "must start with a non-zero cap and zero balances",
            ));
        }

        let mut state = self.state.lock().await;
        if state.accounts.contains_key(&account.id) {
            return Err(AuthorityError::AlreadyExists(Resource::SpendAccount));
        }
        state.accounts.insert(account.id, account);
        Ok(())
    }

    async fn spend_account(&self, id: SpendAccountId) -> Result<SpendAccount, AuthorityError> {
        self.state
            .lock()
            .await
            .accounts
            .get(&id)
            .cloned()
            .ok_or(AuthorityError::NotFound(Resource::SpendAccount))
    }

    async fn insert_authorization(
        &self,
        authorization: LaunchAuthorization,
    ) -> Result<(), AuthorityError> {
        if authorization.consumed_by.is_some() {
            return Err(invalid("authorization", "must be unconsumed when inserted"));
        }

        let mut state = self.state.lock().await;
        if state.authorizations.contains_key(&authorization.id) {
            return Err(AuthorityError::AlreadyExists(Resource::Authorization));
        }
        let quote = state
            .quotes
            .get(&authorization.quote_id)
            .ok_or(AuthorityError::NotFound(Resource::Quote))?;
        let account = state
            .accounts
            .get(&authorization.spend_account_id)
            .ok_or(AuthorityError::NotFound(Resource::SpendAccount))?;
        if quote.owner != authorization.owner || account.owner != authorization.owner {
            return Err(AuthorityError::ForeignOwner);
        }
        if authorization.authorized_microusdc != quote.total_microusdc {
            return Err(AuthorityError::AuthorizationScopeMismatch);
        }
        state.authorizations.insert(authorization.id, authorization);
        Ok(())
    }

    async fn authorization(
        &self,
        id: AuthorizationId,
    ) -> Result<LaunchAuthorization, AuthorityError> {
        self.state
            .lock()
            .await
            .authorizations
            .get(&id)
            .cloned()
            .ok_or(AuthorityError::NotFound(Resource::Authorization))
    }

    async fn begin_launch(&self, launch: BeginLaunch) -> Result<LaunchResult, AuthorityError> {
        let mut state = self.state.lock().await;
        let idempotency_key = (launch.owner.clone(), launch.idempotency_key.clone());
        let identity = LaunchIdentity {
            quote_id: launch.quote_id,
            authorization_id: launch.authorization_id,
            spend_account_id: launch.spend_account_id,
        };

        if let Some(record) = state.idempotency.get(&idempotency_key) {
            if record.identity != identity {
                return Err(AuthorityError::IdempotencyConflict);
            }
            let job = state.jobs.get(&record.job_id).cloned().ok_or_else(|| {
                AuthorityError::Storage("idempotency record references a missing job".to_owned())
            })?;
            return Ok(LaunchResult {
                job,
                disposition: LaunchDisposition::Replay,
            });
        }

        if state.jobs.contains_key(&launch.job_id) {
            return Err(AuthorityError::AlreadyExists(Resource::Job));
        }
        let quote = state
            .quotes
            .get(&launch.quote_id)
            .cloned()
            .ok_or(AuthorityError::NotFound(Resource::Quote))?;
        let authorization = state
            .authorizations
            .get(&launch.authorization_id)
            .cloned()
            .ok_or(AuthorityError::NotFound(Resource::Authorization))?;
        let account = state
            .accounts
            .get(&launch.spend_account_id)
            .cloned()
            .ok_or(AuthorityError::NotFound(Resource::SpendAccount))?;

        if quote.owner != launch.owner
            || authorization.owner != launch.owner
            || account.owner != launch.owner
        {
            return Err(AuthorityError::ForeignOwner);
        }
        if quote.expires_at_ms <= launch.now_ms {
            return Err(AuthorityError::QuoteExpired);
        }
        if authorization.expires_at_ms <= launch.now_ms {
            return Err(AuthorityError::AuthorizationExpired);
        }
        if authorization.quote_id != quote.id
            || authorization.spend_account_id != account.id
            || authorization.authorized_microusdc != quote.total_microusdc
        {
            return Err(AuthorityError::AuthorizationScopeMismatch);
        }
        if authorization.consumed_by.is_some() {
            return Err(AuthorityError::AuthorizationConsumed);
        }

        let next_reserved = account
            .reserved_microusdc
            .checked_add(quote.total_microusdc)
            .ok_or(AuthorityError::ArithmeticOverflow)?;
        let used = next_reserved
            .checked_add(account.committed_microusdc)
            .ok_or(AuthorityError::ArithmeticOverflow)?;
        if used > account.cap_microusdc {
            return Err(AuthorityError::SpendCapExceeded {
                cap_microusdc: account.cap_microusdc,
                committed_microusdc: account.committed_microusdc,
                reserved_microusdc: account.reserved_microusdc,
                requested_microusdc: quote.total_microusdc,
            });
        }

        let job = ComputeJob {
            id: launch.job_id,
            owner: launch.owner.clone(),
            quote_id: quote.id,
            authorization_id: authorization.id,
            spend_account_id: account.id,
            idempotency_key: launch.idempotency_key.clone(),
            reserved_microusdc: quote.total_microusdc,
            committed_microusdc: None,
            state: JobState::Prepared,
            provider_job_id: None,
            failure_code: None,
            created_at_ms: launch.now_ms,
            updated_at_ms: launch.now_ms,
        };

        state
            .accounts
            .get_mut(&account.id)
            .expect("account was read under the same lock")
            .reserved_microusdc = next_reserved;
        state
            .authorizations
            .get_mut(&authorization.id)
            .expect("authorization was read under the same lock")
            .consumed_by = Some(job.id);
        state.jobs.insert(job.id, job.clone());
        state.idempotency.insert(
            idempotency_key,
            IdempotencyRecord {
                identity,
                job_id: job.id,
            },
        );

        Ok(LaunchResult {
            job,
            disposition: LaunchDisposition::Created,
        })
    }

    async fn job(&self, id: JobId) -> Result<ComputeJob, AuthorityError> {
        self.state
            .lock()
            .await
            .jobs
            .get(&id)
            .cloned()
            .ok_or(AuthorityError::NotFound(Resource::Job))
    }

    async fn request_cancel(
        &self,
        owner: &PrincipalId,
        id: JobId,
        now_ms: u64,
    ) -> Result<ComputeJob, AuthorityError> {
        let mut state = self.state.lock().await;
        let job = state
            .jobs
            .get_mut(&id)
            .ok_or(AuthorityError::NotFound(Resource::Job))?;
        if &job.owner != owner {
            return Err(AuthorityError::ForeignOwner);
        }
        match job.state {
            JobState::Prepared | JobState::Running => {
                job.state = JobState::CancelRequested;
                job.updated_at_ms = now_ms;
            }
            JobState::CancelRequested => {}
            _ => {
                return Err(AuthorityError::InvalidJobState {
                    operation: "cancel",
                    state: job.state,
                });
            }
        }
        Ok(job.clone())
    }

    async fn mark_running(
        &self,
        id: JobId,
        provider_job_id: String,
        now_ms: u64,
    ) -> Result<ComputeJob, AuthorityError> {
        let mut state = self.state.lock().await;
        let job = state
            .jobs
            .get_mut(&id)
            .ok_or(AuthorityError::NotFound(Resource::Job))?;
        match job.state {
            JobState::Prepared => {
                job.provider_job_id = Some(provider_job_id);
                job.state = JobState::Running;
                job.updated_at_ms = now_ms;
            }
            JobState::CancelRequested if job.provider_job_id.is_none() => {
                job.provider_job_id = Some(provider_job_id);
                job.updated_at_ms = now_ms;
            }
            JobState::Running if job.provider_job_id.as_deref() == Some(&provider_job_id) => {}
            JobState::CancelRequested
                if job.provider_job_id.as_deref() == Some(&provider_job_id) => {}
            _ => {
                return Err(AuthorityError::InvalidJobState {
                    operation: "mark running",
                    state: job.state,
                });
            }
        }
        Ok(job.clone())
    }

    async fn settle_job(
        &self,
        id: JobId,
        settlement: JobSettlement,
        now_ms: u64,
    ) -> Result<ComputeJob, AuthorityError> {
        let mut state = self.state.lock().await;
        let current = state
            .jobs
            .get(&id)
            .cloned()
            .ok_or(AuthorityError::NotFound(Resource::Job))?;
        let target_state = match settlement.outcome {
            TerminalOutcome::Succeeded => JobState::Succeeded,
            TerminalOutcome::Failed => JobState::Failed,
            TerminalOutcome::Cancelled => JobState::Cancelled,
        };

        if current.state.is_terminal() {
            if current.state == target_state
                && current.committed_microusdc == Some(settlement.charge_microusdc)
                && current.failure_code == settlement.failure_code
            {
                return Ok(current);
            }
            return Err(AuthorityError::InvalidJobState {
                operation: "settle",
                state: current.state,
            });
        }
        if settlement.outcome == TerminalOutcome::Cancelled
            && current.state != JobState::CancelRequested
        {
            return Err(AuthorityError::InvalidJobState {
                operation: "settle as cancelled",
                state: current.state,
            });
        }
        if settlement.charge_microusdc > current.reserved_microusdc {
            return Err(AuthorityError::ChargeExceedsReservation {
                charge_microusdc: settlement.charge_microusdc,
                reserved_microusdc: current.reserved_microusdc,
            });
        }

        let account = state
            .accounts
            .get(&current.spend_account_id)
            .cloned()
            .ok_or(AuthorityError::NotFound(Resource::SpendAccount))?;
        let next_reserved = account
            .reserved_microusdc
            .checked_sub(current.reserved_microusdc)
            .ok_or_else(|| {
                AuthorityError::Storage("job reservation exceeds account reservation".to_owned())
            })?;
        let next_committed = account
            .committed_microusdc
            .checked_add(settlement.charge_microusdc)
            .ok_or(AuthorityError::ArithmeticOverflow)?;
        let used = next_reserved
            .checked_add(next_committed)
            .ok_or(AuthorityError::ArithmeticOverflow)?;
        if used > account.cap_microusdc {
            return Err(AuthorityError::Storage(
                "settlement would violate the spend cap".to_owned(),
            ));
        }

        let account = state
            .accounts
            .get_mut(&current.spend_account_id)
            .expect("account was read under the same lock");
        account.reserved_microusdc = next_reserved;
        account.committed_microusdc = next_committed;

        let job = state
            .jobs
            .get_mut(&id)
            .expect("job was read under the same lock");
        job.state = target_state;
        job.committed_microusdc = Some(settlement.charge_microusdc);
        job.failure_code = settlement.failure_code;
        job.updated_at_ms = now_ms;
        Ok(job.clone())
    }

    async fn fail_launch(
        &self,
        id: JobId,
        failure_code: String,
        now_ms: u64,
    ) -> Result<ComputeJob, AuthorityError> {
        let mut state = self.state.lock().await;
        let current = state
            .jobs
            .get(&id)
            .cloned()
            .ok_or(AuthorityError::NotFound(Resource::Job))?;
        if current.state == JobState::Failed
            && current.committed_microusdc == Some(0)
            && current.failure_code.as_deref() == Some(&failure_code)
        {
            return Ok(current);
        }
        let launch_not_started = current.provider_job_id.is_none()
            && matches!(
                current.state,
                JobState::Prepared | JobState::CancelRequested
            );
        if !launch_not_started {
            return Err(AuthorityError::InvalidJobState {
                operation: "fail launch",
                state: current.state,
            });
        }

        let account = state
            .accounts
            .get(&current.spend_account_id)
            .cloned()
            .ok_or(AuthorityError::NotFound(Resource::SpendAccount))?;
        let next_reserved = account
            .reserved_microusdc
            .checked_sub(current.reserved_microusdc)
            .ok_or_else(|| {
                AuthorityError::Storage("job reservation exceeds account reservation".to_owned())
            })?;

        state
            .accounts
            .get_mut(&current.spend_account_id)
            .expect("account was read under the same lock")
            .reserved_microusdc = next_reserved;
        let job = state
            .jobs
            .get_mut(&id)
            .expect("job was read under the same lock");
        job.state = JobState::Failed;
        job.committed_microusdc = Some(0);
        job.failure_code = Some(failure_code);
        job.updated_at_ms = now_ms;
        Ok(job.clone())
    }
}
