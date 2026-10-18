//! Sanitized, read-only rental-canary status. Reading this endpoint cannot
//! schedule work, change a budget, or touch the financial journal.
use axum::{
    http::{HeaderMap, HeaderValue},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::Read,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const MAX_BYTES: u64 = 256 * 1024;
const STALE_SECONDS: u64 = 180;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Verdict {
    Pending,
    Success,
    Failed,
    Blocked,
    Recovered,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Stage {
    Configuration,
    Preflight,
    Funding,
    Budget,
    Launch,
    Ready,
    SshIdentity,
    GpuProbe,
    Close,
    Settlement,
    ProviderCleanup,
    Complete,
    Recovery,
    Monitor,
    #[serde(other)]
    Unknown,
}

// Unknown diagnostics are deliberately reduced to one fixed label: no provider
// response, URL, arbitrary exception, or credential can reach the public feed.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Reason {
    InsufficientFunds,
    InsufficientFundsAfterDeposit,
    NoAffordableGpuCapacity,
    ProviderLimitsUnavailable,
    ProviderBudgetBreach,
    DailyBudgetExhausted,
    PilotBudgetExhausted,
    DailyBudgetExceeded,
    PilotBudgetExceeded,
    BudgetExhausted,
    BudgetStateInvalid,
    LaunchOutcomeUnknownDoNotRetry,
    UnresolvedLaunchDoNotRetry,
    RecoveringPriorRental,
    StageTimeout,
    GpuProbeFailed,
    GpuProbeInvalid,
    PayoutNotVerifiedOnchain,
    EscrowRefundNotReconciled,
    AuthenticatedEvidenceUnavailable,
    HostKeyBindingUnverified,
    CleanupUnverified,
    CleanupIdentityMissing,
    SubprocessFailed,
    SubprocessTimeout,
    Interrupted,
    Disabled,
    PilotComplete,
    UnresolvedRental,
    PilotEntryLimit,
    PriorRentalReconciled,
    #[serde(other)]
    CheckUnavailable,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Run {
    run_id: Uuid,
    job_id: Option<Uuid>,
    started_at_unix: u64,
    finished_at_unix: Option<u64>,
    verdict: Verdict,
    stage: Stage,
    reason: Option<Reason>,
    gpu_model: Option<String>,
    charged_micro_usdc: Option<u64>,
    refunded_micro_usdc: Option<u64>,
    ssh_identity_verified: bool,
    gpu_probe_verified: bool,
    provider_cleanup_verified: bool,
    balance_reconciled: bool,
    settlement_verified_onchain: bool,
    recovery_only: bool,
}

impl Run {
    fn valid(&self, generated: u64) -> bool {
        !self.run_id.is_nil()
            && self.job_id.is_none_or(|id| !id.is_nil())
            && self.started_at_unix > 0
            && self.started_at_unix <= generated
            && self
                .finished_at_unix
                .is_none_or(|end| end >= self.started_at_unix && end <= generated)
            && self
                .gpu_model
                .as_ref()
                .is_none_or(|model| matches!(model.as_str(), "NVIDIA L40S" | "L40S"))
            && !matches!(self.stage, Stage::Unknown)
            && (!matches!(self.verdict, Verdict::Success)
                || (self.ssh_identity_verified
                    && self.gpu_probe_verified
                    && self.provider_cleanup_verified
                    && self.balance_reconciled
                    && self.settlement_verified_onchain
                    && !self.recovery_only
                    && self.finished_at_unix.is_some()
                    && self.job_id.is_some()
                    && self.charged_micro_usdc.is_some()
                    && self.refunded_micro_usdc.is_some()))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Budget {
    daily_limit_micro_usd: u64,
    pilot_limit_micro_usd: u64,
    daily_reserved_micro_usd: u64,
    pilot_reserved_micro_usd: u64,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Monitor {
    consecutive_unsuccessful_windows: u64,
    alert_due: bool,
    last_success_at_unix: Option<u64>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema: u32,
    generated_at_unix: u64,
    enabled: bool,
    cadence_seconds: u64,
    next_due_at_unix: Option<u64>,
    pause_reason: Option<Reason>,
    budget: Option<Budget>,
    monitor: Option<Monitor>,
    latest: Option<Run>,
    history: Vec<Run>,
}

fn unavailable(status: &str, now: u64, enabled: bool) -> Value {
    json!({"schema":1,"server_time_unix":now,"generated_at_unix":null,
        "stale_after_seconds":STALE_SECONDS,"observed_state_age_seconds":null,
        "is_stale":true,"feed_status":status,"enabled":enabled,
        "cadence_seconds":21600,"next_due_at_unix":null,"pause_reason":null,
        "budget":null,"monitor":null,"latest":null,"history":[]})
}

fn snapshot(bytes: &[u8], now: u64) -> Result<Value, ()> {
    let record: Snapshot = serde_json::from_slice(bytes).map_err(|_| ())?;
    if record.schema != 1
        || record.cadence_seconds != 21600
        || record.history.len() > 40
        || record.generated_at_unix == 0
        || record.generated_at_unix > now.saturating_add(30)
        || record.budget.as_ref().is_some_and(|b| {
            b.daily_limit_micro_usd != 2_000_000
                || b.pilot_limit_micro_usd != 20_000_000
                || b.daily_reserved_micro_usd > b.pilot_reserved_micro_usd
        })
        || record
            .latest
            .as_ref()
            .is_some_and(|r| !r.valid(record.generated_at_unix))
        || record
            .history
            .iter()
            .any(|r| !r.valid(record.generated_at_unix))
        || record
            .monitor
            .as_ref()
            .and_then(|m| m.last_success_at_unix)
            .is_some_and(|t| t > record.generated_at_unix)
    {
        return Err(());
    }
    let age = now.saturating_sub(record.generated_at_unix);
    let status = if !record.enabled {
        "disabled"
    } else if age > STALE_SECONDS {
        "stale"
    } else {
        "fresh"
    };
    let mut value = serde_json::to_value(&record).map_err(|_| ())?;
    value["server_time_unix"] = json!(now);
    value["stale_after_seconds"] = json!(STALE_SECONDS);
    value["observed_state_age_seconds"] = json!(age);
    value["is_stale"] = json!(age > STALE_SECONDS);
    value["feed_status"] = json!(status);
    Ok(value)
}

fn read(path: &Path, now: u64) -> Value {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return unavailable("not_observed", now, true)
        }
        Err(_) => return unavailable("invalid", now, true),
    };
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return unavailable("invalid", now, true);
    }
    let parsed = (|| {
        let file = std::fs::File::open(path).map_err(|_| ())?;
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(());
        }
        snapshot(&bytes, now)
    })();
    parsed.unwrap_or_else(|_| unavailable("invalid", now, true))
}

pub async fn public_status() -> (HeaderMap, Json<Value>) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let value = match std::env::var_os("COVENANT_COMPUTE_RENTAL_CHECKS_FILE") {
        Some(path) => read(Path::new(&path), now),
        None => unavailable("disabled", now, false),
    };
    let mut headers = HeaderMap::new();
    headers.insert("cache-control", HeaderValue::from_static("no-store"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    (headers, Json(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Value {
        json!({"schema":1,"generated_at_unix":1000,"enabled":true,"cadence_seconds":21600,
            "next_due_at_unix":21600,"pause_reason":null,"budget":null,"monitor":null,
            "latest":null,"history":[]})
    }
    #[test]
    fn ages_heartbeat_without_inventing_rental_failure() {
        let bytes = serde_json::to_vec(&fixture()).unwrap();
        assert_eq!(snapshot(&bytes, 1100).unwrap()["feed_status"], "fresh");
        let stale = snapshot(&bytes, 1181).unwrap();
        assert_eq!(stale["feed_status"], "stale");
        assert!(stale["latest"].is_null());
        assert!(snapshot(&bytes, 900).is_err());
    }
    #[test]
    fn refuses_unknown_private_fields_and_diagnostic_text() {
        let mut value = fixture();
        value["provider_token"] = json!("secret");
        assert!(snapshot(&serde_json::to_vec(&value).unwrap(), 1000).is_err());
        let mut value = fixture();
        value["pause_reason"] = json!("secret-provider-error");
        let public = snapshot(&serde_json::to_vec(&value).unwrap(), 1000).unwrap();
        assert_eq!(public["pause_reason"], "check_unavailable");
        assert!(!public.to_string().contains("secret-provider"));
    }
    #[test]
    fn missing_and_corrupt_state_are_not_empty_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        assert_eq!(read(&path, 1000)["feed_status"], "not_observed");
        std::fs::write(&path, b"partial").unwrap();
        assert_eq!(read(&path, 1000)["feed_status"], "invalid");
    }
    #[test]
    fn a_partial_probe_is_never_success() {
        let mut value = fixture();
        value["latest"] = json!({"run_id":"10000000-0000-4000-8000-000000000001",
            "job_id":"10000000-0000-4000-8000-000000000002","started_at_unix":900,
            "finished_at_unix":950,"verdict":"success","stage":"complete","reason":null,
            "gpu_model":"NVIDIA L40S","charged_micro_usdc":1,"refunded_micro_usdc":2,
            "ssh_identity_verified":true,"gpu_probe_verified":true,"provider_cleanup_verified":false,
            "balance_reconciled":true,"settlement_verified_onchain":true,"recovery_only":false});
        assert!(snapshot(&serde_json::to_vec(&value).unwrap(), 1000).is_err());
        value["latest"]["provider_cleanup_verified"] = json!(true);
        assert!(snapshot(&serde_json::to_vec(&value).unwrap(), 1000).is_ok());
    }
}
