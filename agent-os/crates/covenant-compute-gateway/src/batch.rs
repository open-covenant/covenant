//! One customer authorization for an ordered, immutable set of native jobs.
//! A later child failure never reverses earlier operator payments. Unknown
//! outcomes reconcile their original job; they never create a replacement.
use crate::*;
use covenant_compute_protocol::inference_batch::{
    InferenceBatchResult, InferenceBatchSpec, MAX_BATCH_ITEMS,
};
use std::io::Read;

/// Initial sequential execution qualification. The proof protocol still allows
/// 32 children so future admissions and existing journals retain wire compatibility.
pub const MAX_EXECUTION_BATCH_ITEMS: usize = 2;
const CHILD_DELIVERY_MARGIN_MS: u64 = 45_000;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_CHILD_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_PROGRESS_BYTES: usize = 10 * 1024 * 1024;
const MAX_BATCH_OUTPUT_BYTES: usize = 9 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BatchIdentity {
    pub version: u8,
    pub batch_id: Uuid,
    pub request_sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BatchPrice {
    pub job_id: Uuid,
    pub model: String,
    pub max_output_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<String>,
    pub amount_atomic: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BatchPricingManifest {
    pub version: u8,
    pub batch_id: Uuid,
    pub children: Vec<BatchPrice>,
}
impl BatchPricingManifest {
    pub fn total(&self) -> Result<u64> {
        if self.version != 1
            || self.batch_id.is_nil()
            || self.children.is_empty()
            || self.children.len() > MAX_BATCH_ITEMS
        {
            return Err(GatewayError::Invalid(
                "invalid batch pricing manifest".into(),
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        self.children.iter().try_fold(0u64, |sum, child| {
            if child.job_id.is_nil()
                || !ids.insert(child.job_id)
                || child.model.is_empty()
                || child.max_output_tokens == 0
                || child.amount_atomic == 0
                || child
                    .pricing
                    .as_deref()
                    .is_some_and(|p| p != "model_tariff_v1")
            {
                return Err(GatewayError::Invalid("invalid batch child pricing".into()));
            }
            sum.checked_add(child.amount_atomic)
                .filter(|n| *n <= MAX_SAFE_INTEGER)
                .ok_or_else(|| GatewayError::Invalid("batch retail price overflow".into()))
        })
    }
}
pub fn parse(request: &Value) -> Result<InferenceBatchSpec> {
    let spec: InferenceBatchSpec = serde_json::from_value(request.clone())
        .map_err(|e| GatewayError::Invalid(e.to_string()))?;
    spec.validate()
        .map_err(|e| GatewayError::Invalid(e.to_string()))?;
    Ok(spec)
}
pub fn resource_url(single: &str) -> Result<String> {
    let mut url = reqwest::Url::parse(single).map_err(|e| GatewayError::Invalid(e.to_string()))?;
    url.set_path("/v1/inference/batches");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}
pub struct BatchExecutor {
    pub inner: Arc<dyn Executor>,
    pub directory: PathBuf,
    /// Only new quotes and payment admissions depend on this flag. Recovery
    /// always understands and executes persisted batch identities.
    pub enabled: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Progress {
    version: u8,
    identity: BatchIdentity,
    prepared_sha256: String,
    outputs: Vec<Option<Value>>,
    terminal_failure: Option<String>,
    terminal_seller_loss: bool,
}
fn failure(message: impl ToString) -> ExecutionFailure {
    ExecutionFailure {
        message: message.to_string(),
        definitely_unpaid: false,
        terminal_seller_loss: false,
    }
}
fn save(path: &Path, state: &Progress) -> Result<()> {
    let bytes = serde_json::to_vec(state)?;
    if bytes.len() > MAX_PROGRESS_BYTES {
        return Err(GatewayError::Unavailable(
            "batch journal exceeds limit".into(),
        ));
    }
    let mut file = tempfile::NamedTempFile::new_in(path)?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    file.persist(path.join("progress.json"))
        .map_err(|e| e.error)?;
    File::open(path)?.sync_all()?;
    Ok(())
}
fn load(path: &Path) -> Result<Option<Progress>> {
    let file = match File::open(path.join("progress.json")) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if file.metadata()?.len() > MAX_PROGRESS_BYTES as u64 {
        return Err(GatewayError::Unavailable(
            "batch journal exceeds limit".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_PROGRESS_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_PROGRESS_BYTES {
        return Err(GatewayError::Unavailable(
            "batch journal exceeds limit".into(),
        ));
    }
    Ok(Some(serde_json::from_slice(&bytes)?))
}
impl BatchExecutor {
    fn require_enabled(&self) -> Result<()> {
        if self.enabled {
            Ok(())
        } else {
            Err(GatewayError::Unavailable(
                "new batch admissions disabled".into(),
            ))
        }
    }
    fn admission_expiry(prepared: &PreparedExecution) -> Result<u64> {
        Self::validate_prepared(prepared)?;
        let completion_margin =
            (prepared.job_envelopes.len() as u64 + 1) * CHILD_DELIVERY_MARGIN_MS;
        let mut expires = u64::MAX;
        for (index, envelope) in prepared.job_envelopes.iter().enumerate() {
            let payload = &envelope.payload;
            let dispatch_margin = (index as u64 + 1) * CHILD_DELIVERY_MARGIN_MS;
            expires = expires.min(
                payload
                    .issued_at_ms
                    .saturating_add(
                        payload
                            .deadline_ms
                            .min(covenant_compute_protocol::JOB_FRESHNESS_WINDOW_MS),
                    )
                    .saturating_sub(dispatch_margin),
            );
            expires = expires.min(
                payload
                    .issued_at_ms
                    .saturating_add(payload.deadline_ms)
                    .saturating_sub(completion_margin),
            );
            if let Some(quote) = covenant_compute_protocol::parse_tariff_input(&payload.input)
                .map_err(|e| GatewayError::Invalid(e.to_string()))?
            {
                expires = expires.min(
                    quote
                        .tariff
                        .valid_until_ms
                        .saturating_sub(completion_margin),
                );
            }
        }
        Ok(expires)
    }
    fn validate_prepared(prepared: &PreparedExecution) -> Result<()> {
        let identity = prepared
            .operation
            .as_ref()
            .ok_or_else(|| GatewayError::Invalid("batch identity missing".into()))?;
        if identity.version != 1
            || identity.batch_id.is_nil()
            || identity.request_sha256.len() != 64
            || !identity
                .request_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || prepared.job_envelopes.is_empty()
            || prepared.job_envelopes.len() > MAX_BATCH_ITEMS
        {
            return Err(GatewayError::Invalid(
                "invalid persisted batch identity".into(),
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        let cost = prepared.job_envelopes.iter().try_fold(0u64, |sum, e| {
            if e.payload.job_id.is_nil()
                || !ids.insert(e.payload.job_id)
                || e.payload.price_micro_usdc == 0
            {
                return Err(GatewayError::Invalid(
                    "invalid batch child identity/cost".into(),
                ));
            }
            sum.checked_add(e.payload.price_micro_usdc)
                .filter(|n| *n <= MAX_SAFE_INTEGER)
                .ok_or_else(|| GatewayError::Invalid("batch native cost overflow".into()))
        })?;
        if cost != prepared.maximum_cost_micro_usdc {
            return Err(GatewayError::Invalid("batch native total mismatch".into()));
        }
        Ok(())
    }
}
#[async_trait]
impl Executor for BatchExecutor {
    fn batch_supported(&self) -> bool {
        true
    }
    fn validate_request(&self, request: &Value) -> Result<()> {
        if request.get("requests").is_none() {
            return self.inner.validate_request(request);
        }
        self.require_enabled()?;
        let spec = parse(request)?;
        if spec.requests.len() > MAX_EXECUTION_BATCH_ITEMS {
            return Err(GatewayError::Invalid(
                "initial batch execution supports one or two sequential requests".into(),
            ));
        }
        for child in spec.requests {
            self.inner.validate_request(&child)?;
        }
        Ok(())
    }
    async fn discovery_offers(
        &self,
        now: u64,
        legacy_price: u64,
    ) -> Result<Vec<discovery::DiscoveryOffer>> {
        self.inner.discovery_offers(now, legacy_price).await
    }
    fn models(&self, now: u64, legacy_price: u64) -> Vec<Value> {
        self.inner.models(now, legacy_price)
    }
    async fn check_available(
        &self,
        request: &Value,
        now: u64,
        legacy_price: u64,
        remaining: u64,
    ) -> Result<()> {
        if request.get("requests").is_none() {
            return self
                .inner
                .check_available(request, now, legacy_price, remaining)
                .await;
        }
        self.validate_request(request)?;
        // One bounded live capacity observation for the entire child set.
        let offers = self.inner.discovery_offers(now, legacy_price).await?;
        let mut available = remaining;
        for child in parse(request)?.requests {
            let offer = offers
                .iter()
                .find(|offer| offer.matches_request(&child))
                .ok_or_else(|| {
                    GatewayError::Unavailable("batch child has no observed live supply".into())
                })?;
            let cap = child["max_tokens"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| GatewayError::Invalid("output cap required".into()))?;
            available = available
                .checked_sub(offer.native_cost(cap)?)
                .ok_or_else(|| GatewayError::Unavailable("batch exceeds compute reserve".into()))?;
        }
        self.inner
            .validate_batch_budget(remaining - available, crate::now_ms().max(now))
    }
    fn prepare(&self, request: &Value) -> Result<PreparedExecution> {
        if request.get("requests").is_none() {
            return self.inner.prepare(request);
        }
        Err(GatewayError::Invalid(
            "batch requires a fully priced frozen quote".into(),
        ))
    }
    fn prepare_legacy(&self, request: &Value) -> Result<PreparedExecution> {
        if request.get("requests").is_some() {
            return Err(GatewayError::Invalid(
                "batch cannot recover without its original signed child set".into(),
            ));
        }
        self.inner.prepare_legacy(request)
    }
    fn quote(&self, request: &Value, legacy_price: u64, now: u64) -> Result<ExecutionQuote> {
        if request.get("requests").is_none() {
            return self.inner.quote(request, legacy_price, now);
        }
        self.validate_request(request)?;
        let identity = BatchIdentity {
            version: 1,
            batch_id: Uuid::new_v4(),
            request_sha256: request_digest(request)?,
        };
        let mut manifest = BatchPricingManifest {
            version: 1,
            batch_id: identity.batch_id,
            children: vec![],
        };
        let mut prepared: Option<PreparedExecution> = None;
        let mut expires_at_ms = u64::MAX;
        for child in parse(request)?.requests {
            let quote = self.inner.quote(&child, legacy_price, now)?;
            if quote.batch_pricing.is_some()
                || quote.prepared.operation.is_some()
                || quote.prepared.job_envelopes.len() != 1
                || quote.amount_atomic < quote.prepared.maximum_cost_micro_usdc
            {
                return Err(GatewayError::Invalid(
                    "batch child quote is not a covered single execution".into(),
                ));
            }
            manifest.children.push(BatchPrice {
                job_id: quote.prepared.job_envelopes[0].payload.job_id,
                model: child["model"]
                    .as_str()
                    .ok_or_else(|| GatewayError::Invalid("model required".into()))?
                    .into(),
                max_output_tokens: child["max_tokens"]
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| GatewayError::Invalid("output cap required".into()))?,
                pricing: child
                    .get("pricing")
                    .map(|p| {
                        p.as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| GatewayError::Invalid("invalid pricing mode".into()))
                    })
                    .transpose()?,
                amount_atomic: quote.amount_atomic,
            });
            expires_at_ms = expires_at_ms.min(quote.expires_at_ms);
            if let Some(all) = &mut prepared {
                if all.buyer != quote.prepared.buyer
                    || all.coordinator != quote.prepared.coordinator
                {
                    return Err(GatewayError::Invalid(
                        "batch child execution identity changed".into(),
                    ));
                }
                all.maximum_cost_micro_usdc = all
                    .maximum_cost_micro_usdc
                    .checked_add(quote.prepared.maximum_cost_micro_usdc)
                    .ok_or_else(|| GatewayError::Invalid("batch native total overflow".into()))?;
                all.job_envelopes.extend(quote.prepared.job_envelopes);
            } else {
                prepared = Some(quote.prepared);
            }
        }
        let mut prepared = prepared.expect("validated nonempty batch");
        prepared.operation = Some(identity);
        Self::validate_prepared(&prepared)?;
        expires_at_ms = expires_at_ms.min(Self::admission_expiry(&prepared)?);
        if expires_at_ms <= now {
            return Err(GatewayError::Unavailable(
                "batch cannot fit signed freshness, execution and settlement margins".into(),
            ));
        }
        self.inner
            .validate_batch_budget(prepared.maximum_cost_micro_usdc, now)?;
        Ok(ExecutionQuote {
            amount_atomic: manifest.total()?,
            batch_pricing: Some(manifest),
            prepared,
            expires_at_ms,
        })
    }
    fn validate_admission(&self, prepared: &PreparedExecution, now: u64) -> Result<()> {
        if prepared.operation.is_some() {
            self.require_enabled()?;
            Self::validate_prepared(prepared)?;
            if prepared.job_envelopes.len() > MAX_EXECUTION_BATCH_ITEMS
                || Self::admission_expiry(prepared)? <= now
            {
                return Err(GatewayError::Unavailable(
                    "batch no longer fits signed freshness, execution and settlement margins"
                        .into(),
                ));
            }
            self.inner
                .validate_batch_budget(prepared.maximum_cost_micro_usdc, now)?;
        }
        self.inner.validate_admission(prepared, now)
    }
    fn discard_progress(&self, batch_id: Option<Uuid>) -> Result<()> {
        if let Some(batch_id) = batch_id {
            if batch_id.is_nil() {
                return Err(GatewayError::Invalid("nil batch progress identity".into()));
            }
            let path = self.directory.join(batch_id.to_string());
            match std::fs::remove_dir_all(&path) {
                Ok(()) => File::open(&self.directory)?.sync_all()?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
    async fn execute(
        &self,
        prepared: &PreparedExecution,
    ) -> std::result::Result<Value, ExecutionFailure> {
        let Some(identity) = &prepared.operation else {
            return self.inner.execute(prepared).await;
        };
        Self::validate_prepared(prepared).map_err(failure)?;
        let path = self.directory.join(identity.batch_id.to_string());
        std::fs::create_dir_all(&path).map_err(failure)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.directory, std::fs::Permissions::from_mode(0o700))
                .map_err(failure)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .map_err(failure)?;
        }
        let mut opts = OpenOptions::new();
        opts.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let lock = opts.open(path.join("lock")).map_err(failure)?;
        lock.try_lock()
            .map_err(|_| failure("batch recovery in another process"))?;
        let prepared_hash =
            request_digest(&serde_json::to_value(prepared).map_err(failure)?).map_err(failure)?;
        let mut state = load(&path).map_err(failure)?.unwrap_or_else(|| Progress {
            version: 1,
            identity: identity.clone(),
            prepared_sha256: prepared_hash.clone(),
            outputs: vec![None; prepared.job_envelopes.len()],
            terminal_failure: None,
            terminal_seller_loss: false,
        });
        if state.version != 1
            || state.identity != *identity
            || state.prepared_sha256 != prepared_hash
            || state.outputs.len() != prepared.job_envelopes.len()
        {
            return Err(failure("batch journal identity mismatch"));
        }
        if let Some(message) = &state.terminal_failure {
            return Err(ExecutionFailure {
                message: message.clone(),
                definitely_unpaid: !state.terminal_seller_loss,
                terminal_seller_loss: state.terminal_seller_loss,
            });
        }
        save(&path, &state).map_err(failure)?;
        for (i, envelope) in prepared.job_envelopes.iter().enumerate() {
            if state.outputs[i].is_some() {
                continue;
            }
            let child = PreparedExecution {
                operation: None,
                buyer: prepared.buyer.clone(),
                coordinator: prepared.coordinator.clone(),
                job_envelopes: vec![envelope.clone()],
                maximum_cost_micro_usdc: envelope.payload.price_micro_usdc,
            };
            let outcome = match self.inner.execute(&child).await {
                Ok(output)
                    if serde_json::to_vec(&output).map_err(failure)?.len()
                        <= MAX_CHILD_OUTPUT_BYTES =>
                {
                    state.outputs[i] = Some(output);
                    save(&path, &state).map_err(failure)?;
                    continue;
                }
                Ok(_) => ExecutionFailure {
                    message: "verified child output exceeds batch retention limit".into(),
                    definitely_unpaid: false,
                    terminal_seller_loss: true,
                },
                Err(error) => error,
            };
            if outcome.definitely_unpaid || outcome.terminal_seller_loss {
                state.terminal_seller_loss =
                    outcome.terminal_seller_loss || state.outputs.iter().any(Option::is_some);
                state.terminal_failure =
                    Some(format!("batch child {i} failed: {}", outcome.message));
                save(&path, &state).map_err(failure)?;
                return Err(ExecutionFailure {
                    message: state.terminal_failure.unwrap(),
                    definitely_unpaid: !state.terminal_seller_loss,
                    terminal_seller_loss: state.terminal_seller_loss,
                });
            }
            return Err(outcome);
        }
        let results = prepared
            .job_envelopes
            .iter()
            .zip(state.outputs)
            .map(|(job, output)| (job.payload.job_id, output.expect("all children completed")))
            .collect();
        let result = serde_json::to_value(
            InferenceBatchResult::build(
                identity.batch_id,
                identity.request_sha256.clone(),
                results,
            )
            .map_err(failure)?,
        )
        .map_err(failure)?;
        if serde_json::to_vec(&result).map_err(failure)?.len() > MAX_BATCH_OUTPUT_BYTES {
            return Err(ExecutionFailure {
                message: "verified batch output exceeds retention limit".into(),
                definitely_unpaid: false,
                terminal_seller_loss: true,
            });
        }
        Ok(result)
    }
}
