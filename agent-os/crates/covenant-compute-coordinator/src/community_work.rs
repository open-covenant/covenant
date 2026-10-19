//! Explicit, operator-reviewed public exports. Private buyer evidence is never
//! discovered or copied here. Unknown JSON fields are discarded on serialization.
use crate::{http::ApiError, state::CoordinatorState};
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue},
    response::{IntoResponse, Response},
    Json,
};
use covenant_compute_protocol::sha256_hex;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path as FsPath, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;

const MAX_BYTES: u64 = 1024 * 1024;
const MAX_TIME: u64 = 8_640_000_000_000_000;
const MAX_SLOT: u64 = 9_007_199_254_740_991;
const MAX_REPORT_AGE_MS: u64 = 180_000;
const ENV: &str = "COVENANT_COMPUTE_WORK_DIR";
const PUBLISH_TOKEN: &str = "COVENANT_COMPUTE_WORK_PUBLISH_TOKEN";
fn unavailable() -> ApiError {
    ApiError::ServiceUnavailable("Public work data is unavailable".into())
}
fn missing() -> ApiError {
    ApiError::NotFound("Published work evidence not found".into())
}
fn require(ok: bool) -> Result<(), ApiError> {
    if ok {
        Ok(())
    } else {
        Err(unavailable())
    }
}
fn id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 80
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn text(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.chars().count() <= max
}
fn amount(s: &str) -> bool {
    s == "0"
        || (!s.is_empty()
            && s.len() <= 39
            && !s.starts_with('0')
            && s.bytes().all(|b| b.is_ascii_digit()))
}
fn time(t: u64) -> bool {
    t > 0 && t <= MAX_TIME
}
fn positive_decimal(s: &str) -> bool {
    let parts: Vec<_> = s.split('.').collect();
    parts.len() <= 2
        && amount(parts[0])
        && parts
            .get(1)
            .is_none_or(|v| !v.is_empty() && v.len() <= 38 && v.bytes().all(|b| b.is_ascii_digit()))
        && s.parse::<f64>().is_ok_and(|v| v.is_finite() && v > 0.0)
}
fn fresh_report(t: u64, pool_observed: u64, generated: u64) -> bool {
    time(t) && t <= pool_observed && t <= generated && generated - t <= MAX_REPORT_AGE_MS
}
fn base58(s: &str, lo: usize, hi: usize) -> bool {
    (lo..=hi).contains(&s.len())
        && s.bytes()
            .all(|b| b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz".contains(&b))
}
fn source_url(s: &Option<String>) -> bool {
    s.as_ref().is_none_or(|s| {
        reqwest::Url::parse(s).ok().is_some_and(|u| {
            let p: Vec<_> = u.path().split('/').collect();
            text(s, 2000)
                && u.scheme() == "https"
                && u.host_str() == Some("github.com")
                && u.port().is_none()
                && u.username().is_empty()
                && u.password().is_none()
                && u.query().is_none()
                && p.len() >= 4
                && p[1..3].iter().all(|s| {
                    !s.is_empty()
                        && s.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
                })
        })
    })
}
// Decimal strings can exceed u128; compare attribution without lossy floats.
fn decimal_sum_le(a: &str, b: &str, ceiling: &str) -> bool {
    let mut a = a.bytes().rev();
    let mut b = b.bytes().rev();
    let mut out = Vec::new();
    let mut carry = 0;
    loop {
        let x = a.next();
        let y = b.next();
        if x.is_none() && y.is_none() && carry == 0 {
            break;
        }
        let sum = x.map_or(0, |v| v - b'0') + y.map_or(0, |v| v - b'0') + carry;
        out.push(b'0' + sum % 10);
        carry = sum / 10;
    }
    out.reverse();
    let sum = String::from_utf8(out).unwrap_or_default();
    sum.len() < ceiling.len() || (sum.len() == ceiling.len() && sum.as_str() <= ceiling)
}
macro_rules! choices { ($name:ident { $($v:ident),+ }) => {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")] pub enum $name { $($v),+ }
}; }
choices!(Mode {
    Unfunded,
    Rehearsal,
    Live
});
choices!(WorkStatus {
    Candidate,
    Approved,
    Active,
    Failed,
    Completed,
    Ineligible
});
choices!(Settlement {
    Unfunded,
    Pending,
    Settled
});
choices!(EvidenceStatus {
    Unpublished,
    Unchecked,
    Verified,
    Failed
});
choices!(UpstreamStatus {
    NotSubmitted,
    Submitted,
    Merged
});
choices!(TransactionKind {
    Harvest,
    Conversion,
    Deposit,
    Settlement
});
choices!(AttemptStatus {
    Active,
    Failed,
    Completed
});
choices!(Range { Full, Custom });
choices!(MarketSource { OrcaPublicApi });
choices!(ValuationSource {
    OperatorReviewedMarks
});
choices!(PriceBasis { UnscaledToken });
choices!(VerificationStatus {
    Unchecked,
    Verified,
    Failed
});
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub label: String,
    pub passed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub kind: TransactionKind,
    pub signature: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attempt {
    pub id: String,
    pub status: AttemptStatus,
    pub cost_usdc_micros: String,
    pub summary: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Work {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub status: WorkStatus,
    pub max_budget_usdc_micros: String,
    pub acceptance: Vec<String>,
    pub source_url: Option<String>,
    pub completed_at_ms: Option<u64>,
    pub cost_usdc_micros: String,
    pub fee_funded_usdc_micros: String,
    pub subsidy_usdc_micros: String,
    pub settlement: Settlement,
    pub evidence_status: EvidenceStatus,
    pub before: Option<String>,
    pub after: Option<String>,
    pub patch_url: Option<String>,
    pub upstream_status: UpstreamStatus,
    pub upstream_url: Option<String>,
    pub checks: Vec<Check>,
    pub transactions: Vec<Transaction>,
    pub attempts: Vec<Attempt>,
}
impl Work {
    fn validate(&self, generated: u64) -> Result<(), ApiError> {
        require(
            id(&self.id)
                && text(&self.title, 180)
                && text(&self.summary, 2000)
                && [
                    &self.max_budget_usdc_micros,
                    &self.cost_usdc_micros,
                    &self.fee_funded_usdc_micros,
                    &self.subsidy_usdc_micros,
                ]
                .iter()
                .all(|s| amount(s))
                && self.acceptance.len() <= 30
                && self.acceptance.iter().all(|s| text(s, 2000))
                && source_url(&self.source_url)
                && source_url(&self.patch_url)
                && source_url(&self.upstream_url)
                && self
                    .completed_at_ms
                    .is_none_or(|t| time(t) && t <= generated)
                && (self.status != WorkStatus::Completed || self.completed_at_ms.is_some())
                && (!matches!(self.status, WorkStatus::Approved | WorkStatus::Active)
                    || (self.max_budget_usdc_micros != "0" && !self.acceptance.is_empty()))
                && (self.upstream_status == UpstreamStatus::NotSubmitted
                    || self.upstream_url.is_some())
                && [&self.before, &self.after]
                    .iter()
                    .all(|s| s.as_ref().is_none_or(|v| text(v, 2000)))
                && self.checks.len() <= 100
                && self.checks.iter().all(|c| text(&c.label, 2000))
                && self.transactions.len() <= 100
                && self
                    .transactions
                    .iter()
                    .all(|t| base58(&t.signature, 64, 88))
                && self
                    .transactions
                    .iter()
                    .map(|t| &t.signature)
                    .collect::<HashSet<_>>()
                    .len()
                    == self.transactions.len()
                && self.attempts.len() <= 100
                && self
                    .attempts
                    .iter()
                    .all(|a| id(&a.id) && amount(&a.cost_usdc_micros) && text(&a.summary, 2000))
                && self
                    .attempts
                    .iter()
                    .map(|a| &a.id)
                    .collect::<HashSet<_>>()
                    .len()
                    == self.attempts.len(),
        )?;
        require(decimal_sum_le(
            &self.fee_funded_usdc_micros,
            &self.subsidy_usdc_micros,
            &self.cost_usdc_micros,
        ))
    }
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Market {
    pub source: MarketSource,
    pub observed_at_ms: u64,
    pub source_updated_at_ms: u64,
    pub source_updated_slot: u64,
    pub window_seconds: u64,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Valuation {
    pub source: ValuationSource,
    pub observed_at_ms: u64,
    pub baseline_at_ms: u64,
    pub baseline_slot: u64,
    pub baseline_sha256: String,
    pub marks_sha256: String,
    pub price_basis: PriceBasis,
    pub cvnt_price_usdc: String,
    pub nvdax_price_usdc: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Pool {
    pub address: String,
    pub position_address: String,
    pub active: bool,
    pub finalized_slot: Option<u64>,
    pub observed_at_ms: u64,
    pub fee_bps: u64,
    pub range: Range,
    pub cvnt_raw: String,
    pub nvdax_raw: String,
    pub cvnt_fees_owed_raw: String,
    pub nvdax_fees_owed_raw: String,
    pub nvdax_multiplier: String,
    pub volume_usdc_micros: Option<String>,
    pub lp_value_usdc_micros: Option<String>,
    pub hold_value_usdc_micros: Option<String>,
    #[serde(default)]
    pub market: Option<Market>,
    #[serde(default)]
    pub valuation: Option<Valuation>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Fees {
    pub cvnt_retained_raw: String,
    pub nvdax_collected_raw: String,
    pub nvdax_converted_raw: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Budget {
    pub available_usdc_micros: String,
    pub reserved_usdc_micros: String,
    pub spent_usdc_micros: String,
    pub subsidy_usdc_micros: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema_version: u32,
    pub generated_at_ms: u64,
    pub mode: Mode,
    pub pool: Option<Pool>,
    pub fees: Fees,
    pub budget: Budget,
    pub work: Vec<Work>,
}
impl Snapshot {
    fn validate(&self) -> Result<(), ApiError> {
        require(
            self.schema_version == 1
                && time(self.generated_at_ms)
                && self.work.len() <= 200
                && self
                    .work
                    .iter()
                    .map(|w| &w.id)
                    .collect::<HashSet<_>>()
                    .len()
                    == self.work.len()
                && self
                    .work
                    .iter()
                    .filter(|w| matches!(w.status, WorkStatus::Approved | WorkStatus::Active))
                    .count()
                    <= 3
                && [
                    &self.fees.cvnt_retained_raw,
                    &self.fees.nvdax_collected_raw,
                    &self.fees.nvdax_converted_raw,
                    &self.budget.available_usdc_micros,
                    &self.budget.reserved_usdc_micros,
                    &self.budget.spent_usdc_micros,
                    &self.budget.subsidy_usdc_micros,
                ]
                .iter()
                .all(|s| amount(s)),
        )?;
        if let Some(p) = &self.pool {
            require(
                base58(&p.address, 32, 44)
                    && base58(&p.position_address, 32, 44)
                    && time(p.observed_at_ms)
                    && p.observed_at_ms <= self.generated_at_ms
                    && p.finalized_slot.is_none_or(|s| s <= MAX_SLOT)
                    && p.fee_bps <= 10000
                    && amount(&p.cvnt_raw)
                    && amount(&p.nvdax_raw)
                    && amount(&p.cvnt_fees_owed_raw)
                    && amount(&p.nvdax_fees_owed_raw)
                    && positive_decimal(&p.nvdax_multiplier)
                    && [
                        &p.volume_usdc_micros,
                        &p.lp_value_usdc_micros,
                        &p.hold_value_usdc_micros,
                    ]
                    .iter()
                    .all(|s| s.as_ref().is_none_or(|s| amount(s))),
            )?;
            require(
                p.volume_usdc_micros.is_some() == p.market.is_some()
                    && p.lp_value_usdc_micros.is_some() == p.valuation.is_some()
                    && p.hold_value_usdc_micros.is_some() == p.valuation.is_some(),
            )?;
            if let Some(m) = &p.market {
                require(
                    fresh_report(m.observed_at_ms, p.observed_at_ms, self.generated_at_ms)
                        && fresh_report(
                            m.source_updated_at_ms,
                            p.observed_at_ms,
                            self.generated_at_ms,
                        )
                        && m.source_updated_at_ms <= m.observed_at_ms
                        && m.source_updated_slot > 0
                        && p.finalized_slot.is_some_and(|s| m.source_updated_slot <= s)
                        && m.window_seconds == 86_400,
                )?;
            }
            if let Some(v) = &p.valuation {
                require(
                    fresh_report(v.observed_at_ms, p.observed_at_ms, self.generated_at_ms)
                        && time(v.baseline_at_ms)
                        && v.baseline_at_ms <= v.observed_at_ms
                        && v.baseline_slot > 0
                        && p.finalized_slot.is_some_and(|s| v.baseline_slot <= s)
                        && digest(&v.baseline_sha256)
                        && digest(&v.marks_sha256)
                        && positive_decimal(&v.cvnt_price_usdc)
                        && positive_decimal(&v.nvdax_price_usdc),
                )?;
            }
        }
        self.work
            .iter()
            .try_for_each(|w| w.validate(self.generated_at_ms))
    }
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Detail {
    pub schema_version: u32,
    pub generated_at_ms: u64,
    pub mode: Mode,
    pub work: Work,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct ManifestFile {
    pub name: String,
    pub sha256: String,
    pub bytes: u64,
    pub download_url: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Verification {
    pub work: VerificationStatus,
    pub funding: VerificationStatus,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub work_id: String,
    pub generated_at_ms: u64,
    pub files: Vec<ManifestFile>,
    pub verification: Verification,
    pub limitations: Vec<String>,
}
impl Manifest {
    fn validate(&self, work_id: &str) -> Result<(), ApiError> {
        require(
            self.schema_version == 1
                && self.work_id == work_id
                && time(self.generated_at_ms)
                && self.files.len() <= 50
                && self
                    .files
                    .iter()
                    .map(|f| &f.name)
                    .collect::<HashSet<_>>()
                    .len()
                    == self.files.len()
                && self.files.iter().all(|f| {
                    digest(&f.sha256)
                        && f.bytes <= MAX_BYTES
                        && text(&f.name, 100)
                        && f.name.ends_with(".json")
                        && f.name.as_bytes()[0].is_ascii_alphanumeric()
                        && f.name
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                        && f.download_url
                            == format!(
                            "https://compute-api.opencovenant.org/community-work/artifacts/{}.json",
                            f.sha256
                        )
                })
                && self.limitations.len() <= 50
                && self.limitations.iter().all(|s| text(s, 2000)),
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Adoption {
    schema_version: u32,
    work_id: String,
    status: UpstreamStatus,
    url: String,
}
impl Adoption {
    fn validate(&self, work_id: &str) -> Result<(), ApiError> {
        let valid_url = reqwest::Url::parse(&self.url).ok().is_some_and(|u| {
            let p: Vec<_> = u.path().split('/').collect();
            p.len() == 5
                && p[1..3].iter().all(|s| {
                    !s.is_empty()
                        && s.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
                })
                && p[3] == "pull"
                && !p[4].is_empty()
                && !p[4].starts_with('0')
                && p[4].bytes().all(|b| b.is_ascii_digit())
                && self.url == format!("https://github.com/{}/{}/pull/{}", p[1], p[2], p[4])
        });
        require(
            self.schema_version == 1
                && self.work_id == work_id
                && matches!(
                    self.status,
                    UpstreamStatus::Submitted | UpstreamStatus::Merged
                )
                && text(&self.url, 2000)
                && valid_url,
        )
    }
}

struct Publication {
    root: PathBuf,
}
impl Publication {
    fn configured() -> Result<Self, ApiError> {
        let root = std::env::var_os(ENV)
            .filter(|v| !v.is_empty())
            .ok_or_else(unavailable)?;
        Ok(Self {
            root: PathBuf::from(root),
        })
    }
    fn bytes(&self, components: &[&str]) -> Result<Vec<u8>, ApiError> {
        let file = safe_open(&self.root, components).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                missing()
            } else {
                unavailable()
            }
        })?;
        let metadata = file.metadata().map_err(|_| unavailable())?;
        require(metadata.is_file() && metadata.len() <= MAX_BYTES)?;
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| unavailable())?;
        require(bytes.len() as u64 <= MAX_BYTES)?;
        Ok(bytes)
    }
    fn json<T: DeserializeOwned>(&self, components: &[&str]) -> Result<T, ApiError> {
        serde_json::from_slice(&self.bytes(components)?).map_err(|_| unavailable())
    }
    fn snapshot(&self) -> Result<Snapshot, ApiError> {
        let s: Snapshot = self.json(&["snapshot.json"])?;
        s.validate()?;
        Ok(s)
    }
    fn detail_core(&self, work_id: &str) -> Result<Detail, ApiError> {
        if !id(work_id) {
            return Err(missing());
        }
        let snapshot = self.snapshot()?;
        if !snapshot.work.iter().any(|w| w.id == work_id) {
            return Err(missing());
        }
        let d: Detail = self.json(&["works", &format!("{work_id}.json")])?;
        require(
            d.schema_version == 1
                && time(d.generated_at_ms)
                && d.work.id == work_id
                && d.mode == snapshot.mode,
        )?;
        d.work.validate(d.generated_at_ms)?;
        Ok(d)
    }
    fn detail(&self, work_id: &str) -> Result<Detail, ApiError> {
        let mut d = self.detail_core(work_id)?;
        // Adoption is an operator-reported annotation. Never rewrite the frozen
        // completed detail, its original timestamp, or any signed evidence.
        let adoption: Adoption = match self.json(&["adoption", &format!("{work_id}.json")]) {
            Ok(value) => value,
            Err(ApiError::NotFound(_)) => return Ok(d),
            Err(error) => return Err(error),
        };
        adoption.validate(work_id)?;
        d.work.upstream_status = adoption.status;
        d.work.upstream_url = Some(adoption.url);
        Ok(d)
    }
    fn manifest(&self, work_id: &str) -> Result<Manifest, ApiError> {
        let d = self.detail_core(work_id)?;
        if d.work.evidence_status == EvidenceStatus::Unpublished {
            return Err(missing());
        }
        let m: Manifest = self.json(&["manifests", &format!("{work_id}.json")])?;
        m.validate(work_id)?;
        Ok(m)
    }
    fn deposit_record(
        &self,
        signature: &str,
        accounts: &crate::accounts::BuyerAccounts,
    ) -> Result<serde_json::Value, ApiError> {
        if !base58(signature, 64, 88) {
            return Err(missing());
        }
        #[derive(Deserialize)]
        struct StagedDeposit {
            deposit_id: String,
        }
        let staged: StagedDeposit = self.json(&["deposits", &format!("{signature}.json")])?;
        if staged.deposit_id != signature || !accounts.has_deposit(signature) {
            return Err(missing());
        }
        Ok(serde_json::json!({"deposit_id":signature,"recorded":true}))
    }
    fn artifact(&self, sha: &str) -> Result<Vec<u8>, ApiError> {
        if !digest(sha) {
            return Err(missing());
        }
        let snapshot = self.snapshot()?;
        for w in snapshot
            .work
            .iter()
            .filter(|w| w.evidence_status != EvidenceStatus::Unpublished)
        {
            // Search only typed, bounded manifests first. Loading a full detail
            // and re-reading the snapshot for every historical task amplifies
            // public artifact requests unnecessarily.
            let m: Manifest = self.json(&["manifests", &format!("{}.json", w.id)])?;
            m.validate(&w.id)?;
            if let Some(f) = m.files.iter().find(|f| f.sha256 == sha) {
                let detail = self.detail_core(&w.id)?;
                if detail.work.evidence_status == EvidenceStatus::Unpublished {
                    return Err(missing());
                }
                let bytes = self.bytes(&["artifacts", &format!("{sha}.json")])?;
                require(bytes.len() as u64 == f.bytes && sha256_hex(&bytes) == sha)?;
                serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|_| unavailable())?;
                return Ok(bytes);
            }
        }
        Err(missing())
    }
}
// The publication directory is operator-owned and atomically replaced on publish.
// Refuse symlink components and escapes; O_NOFOLLOW also closes the final-file
// check/open race. Never point this at a directory writable by untrusted jobs.
#[cfg(unix)]
fn safe_open(root: &FsPath, components: &[&str]) -> std::io::Result<File> {
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
    let invalid = || std::io::Error::from(std::io::ErrorKind::InvalidInput);
    if std::fs::symlink_metadata(root)?.file_type().is_symlink() {
        return Err(invalid());
    }
    let root = root.canonicalize()?;
    let mut path = root.clone();
    for component in components {
        if component.is_empty()
            || component.contains('/')
            || *component == "."
            || *component == ".."
        {
            return Err(invalid());
        }
        path.push(component);
        if std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
            return Err(invalid());
        }
    }
    if !path.canonicalize()?.starts_with(&root) {
        return Err(invalid());
    }
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
}
#[cfg(not(unix))]
fn safe_open(_root: &FsPath, _components: &[&str]) -> std::io::Result<File> {
    Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
}
fn public_response<T: Serialize>(value: T) -> Response {
    let mut response = Json(value).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
pub async fn snapshot() -> Result<Response, ApiError> {
    Ok(public_response(Publication::configured()?.snapshot()?))
}
pub async fn detail(Path(id): Path<String>) -> Result<Response, ApiError> {
    Ok(public_response(Publication::configured()?.detail(&id)?))
}
pub async fn manifest(Path(id): Path<String>) -> Result<Response, ApiError> {
    Ok(public_response(Publication::configured()?.manifest(&id)?))
}
pub async fn artifact(Path(filename): Path<String>) -> Result<Response, ApiError> {
    let sha = filename.strip_suffix(".json").ok_or_else(missing)?;
    let bytes = Publication::configured()?.artifact(sha)?;
    let mut r = bytes.into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    r.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{sha}.json\""))
            .map_err(|_| unavailable())?,
    );
    Ok(r)
}
pub async fn deposit(
    State(state): State<CoordinatorState>,
    Path(signature): Path<String>,
) -> Result<Response, ApiError> {
    Ok(public_response(
        Publication::configured()?.deposit_record(&signature, state.accounts())?,
    ))
}

// The transport receives only the explicit public output of publish.py. It has
// no access to the private journal and never changes account credits. Exact
// reviewed bytes (including the publisher's final newline) are retained.
fn invalid_upload() -> ApiError {
    ApiError::BadRequest("Invalid reviewed public work publication".into())
}
fn upload_require(ok: bool) -> Result<(), ApiError> {
    if ok {
        Ok(())
    } else {
        Err(invalid_upload())
    }
}
fn typed_upload<T: DeserializeOwned + Serialize>(value: &serde_json::Value) -> Result<T, ApiError> {
    let typed: T = serde_json::from_value(value.clone()).map_err(|_| invalid_upload())?;
    // Reads remain backwards compatible. Uploads reject, rather than store,
    // unknown fields that might contain private operational data.
    upload_require(serde_json::to_value(&typed).map_err(|_| invalid_upload())? == *value)?;
    Ok(typed)
}
fn ordered_json(value: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let mut pairs: Vec<_> = map.iter().collect();
            pairs.sort_by(|a, b| a.0.cmp(b.0));
            let mut result = serde_json::Map::new();
            for (key, child) in pairs {
                result.insert(key.clone(), ordered_json(child));
            }
            Value::Object(result)
        }
        Value::Array(rows) => Value::Array(rows.iter().map(ordered_json).collect()),
        _ => value.clone(),
    }
}
fn upload_json(bytes: &[u8]) -> Result<serde_json::Value, ApiError> {
    upload_require(!bytes.is_empty() && bytes.len() as u64 <= MAX_BYTES)?;
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| invalid_upload())?;
    // The generated publication format is canonical JSON plus one newline.
    // This also refuses duplicate keys, floats and ambiguous alternate parses.
    fn bounded(v: &serde_json::Value, depth: usize) -> bool {
        if depth > 48 {
            return false;
        }
        match v {
            serde_json::Value::Array(a) => {
                a.len() <= 10_000 && a.iter().all(|x| bounded(x, depth + 1))
            }
            serde_json::Value::Object(m) => {
                m.len() <= 1000 && m.values().all(|x| bounded(x, depth + 1))
            }
            serde_json::Value::Number(n) => n.is_i64() || n.is_u64(),
            _ => true,
        }
    }
    upload_require(value.is_object() && bounded(&value, 0))?;
    let mut canonical = serde_json::to_vec(&ordered_json(&value)).map_err(|_| invalid_upload())?;
    canonical.push(b'\n');
    upload_require(canonical == bytes)?;
    Ok(value)
}
fn upload_components<'a>(kind: &'a str, filename: &'a str) -> Result<Vec<&'a str>, ApiError> {
    let stem = filename.strip_suffix(".json").ok_or_else(invalid_upload)?;
    let valid = match kind {
        "snapshots" => filename == "snapshot.json",
        "works" | "manifests" | "adoption" => id(stem),
        "deposits" => base58(stem, 64, 88),
        "artifacts" => digest(stem),
        _ => false,
    };
    upload_require(valid)?;
    Ok(if kind == "snapshots" {
        vec![filename]
    } else {
        vec![kind, filename]
    })
}
fn publication_root(root: &FsPath) -> Result<(), ApiError> {
    upload_require(root.is_absolute())?;
    for ancestor in root.ancestors() {
        let metadata = fs::symlink_metadata(ancestor).map_err(|_| unavailable())?;
        upload_require(metadata.is_dir() && !metadata.file_type().is_symlink())?;
    }
    Ok(())
}
fn staged_write(
    root: &FsPath,
    components: &[&str],
    bytes: &[u8],
    immutable: bool,
) -> Result<(), ApiError> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let mut parent = root.to_path_buf();
    for component in &components[..components.len() - 1] {
        parent.push(component);
        match fs::DirBuilder::new().mode(0o700).create(&parent) {
            Ok(()) => {
                File::open(parent.parent().ok_or_else(unavailable)?)
                    .and_then(|f| f.sync_all())
                    .map_err(|_| unavailable())?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(unavailable()),
        }
        publication_root(&parent)?;
    }
    let path = parent.join(components.last().ok_or_else(invalid_upload)?);
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        upload_require(metadata.is_file() && !metadata.file_type().is_symlink())?;
    }
    let temporary = parent.join(format!(".work-upload-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|_| unavailable())?;
        f.write_all(bytes)
            .and_then(|_| f.sync_all())
            .map_err(|_| unavailable())?;
        if immutable {
            match fs::hard_link(&temporary, &path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    upload_require(
                        Publication {
                            root: root.to_path_buf(),
                        }
                        .bytes(components)?
                            == bytes,
                    )?;
                }
                Err(_) => return Err(unavailable()),
            }
        } else {
            fs::rename(&temporary, &path).map_err(|_| unavailable())?;
        }
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    result?;
    File::open(parent)
        .and_then(|f| f.sync_all())
        .map_err(|_| unavailable())?;
    Ok(())
}
fn optional_bytes(p: &Publication, components: &[&str]) -> Result<Option<Vec<u8>>, ApiError> {
    match p.bytes(components) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(ApiError::NotFound(_)) => Ok(None),
        Err(error) => Err(error),
    }
}
fn manifest_dependencies(p: &Publication, m: &Manifest) -> Result<(), ApiError> {
    for item in &m.files {
        let bytes = p.bytes(&["artifacts", &format!("{}.json", item.sha256)])?;
        upload_require(bytes.len() as u64 == item.bytes && sha256_hex(&bytes) == item.sha256)?;
        upload_json(&bytes)?;
    }
    Ok(())
}
fn funding_shape(value: &serde_json::Value) -> bool {
    fn keys(v: &serde_json::Value, expected: &[&str]) -> bool {
        v.as_object().is_some_and(|m| {
            m.len() == expected.len() && expected.iter().all(|k| m.contains_key(*k))
        })
    }
    fn rows(v: &serde_json::Value, expected: &[&str]) -> bool {
        v.as_array()
            .is_some_and(|a| a.iter().all(|row| keys(row, expected)))
    }
    keys(
        value,
        &[
            "schema_version",
            "kind",
            "work_id",
            "contract_sha256",
            "max_budget_usdc_micros",
            "agent_evidence_sha256",
            "accepted_patch_sha256",
            "public_config",
            "harvests",
            "conversions",
            "deposit_lots",
            "attempts",
            "limitations",
        ],
    ) && keys(
        &value["public_config"],
        &[
            "pool_address",
            "position_address",
            "owner_address",
            "whirlpools_config",
            "cvnt_account",
            "stock_account",
            "usdc_account",
            "deposit_account",
            "position_token_account",
            "cvnt_mint",
            "stock_mint",
            "usdc_mint",
            "buyer",
            "max_slippage_bps",
            "stock_multiplier",
        ],
    ) && rows(
        &value["harvests"],
        &[
            "id",
            "signature",
            "position_address",
            "stock_raw",
            "cvnt_raw",
            "stock_multiplier",
            "finalized_slot",
            "block_time_s",
        ],
    ) && rows(
        &value["conversions"],
        &[
            "id",
            "harvest_id",
            "signature",
            "stock_raw",
            "usdc_micros",
            "finalized_slot",
            "block_time_s",
        ],
    ) && rows(
        &value["deposit_lots"],
        &[
            "id",
            "source",
            "deposit_signature",
            "credited_usdc_micros",
            "conversion_id",
            "coordinator_credit",
            "finalized_slot",
            "block_time_s",
        ],
    ) && rows(
        &value["attempts"],
        &[
            "id",
            "job_id",
            "status",
            "outcome",
            "coordinator_charge_usdc_micros",
            "charge_source",
            "allocations",
            "settlement_signature",
            "settled_gross_usdc_micros",
            "job_payload_sha256",
            "signed_parent_receipt_sha256",
            "job_issued_at_ms",
            "executed_at_ms",
            "finalized_slot",
            "block_time_s",
        ],
    ) && value["attempts"].as_array().is_some_and(|a| {
        a.iter().all(|row| {
            rows(
                &row["allocations"],
                &["lot_id", "spent_usdc_micros", "reserved_usdc_micros"],
            )
        })
    })
}
fn upload_file(root: &FsPath, kind: &str, filename: &str, bytes: &[u8]) -> Result<(), ApiError> {
    use std::os::unix::fs::OpenOptionsExt;
    let components = upload_components(kind, filename)?;
    publication_root(root)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join(".publish.lock"))
        .map_err(|_| unavailable())?;
    upload_require(lock.metadata().map_err(|_| unavailable())?.is_file())?;
    lock.lock().map_err(|_| unavailable())?;
    let p = Publication {
        root: root.to_path_buf(),
    };
    let value = upload_json(bytes)?;
    let stem = filename.strip_suffix(".json").ok_or_else(invalid_upload)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let previous = optional_bytes(&p, &components)?;
    let mut immutable = matches!(kind, "artifacts" | "manifests" | "deposits");
    match kind {
        "snapshots" => {
            let s: Snapshot = typed_upload(&value)?;
            s.validate()?;
            upload_require(s.mode != Mode::Rehearsal && s.generated_at_ms <= now + 30_000)?;
            if s.mode == Mode::Unfunded {
                upload_require(
                    s.pool.is_none()
                        && [
                            &s.fees.cvnt_retained_raw,
                            &s.fees.nvdax_collected_raw,
                            &s.fees.nvdax_converted_raw,
                            &s.budget.available_usdc_micros,
                            &s.budget.reserved_usdc_micros,
                            &s.budget.spent_usdc_micros,
                            &s.budget.subsidy_usdc_micros,
                        ]
                        .iter()
                        .all(|x| x.as_str() == "0")
                        && s.work.iter().all(|w| {
                            matches!(w.status, WorkStatus::Candidate | WorkStatus::Ineligible)
                                && w.settlement == Settlement::Unfunded
                                && w.evidence_status == EvidenceStatus::Unpublished
                                && w.attempts.is_empty()
                                && w.transactions.is_empty()
                                && [
                                    &w.max_budget_usdc_micros,
                                    &w.cost_usdc_micros,
                                    &w.fee_funded_usdc_micros,
                                    &w.subsidy_usdc_micros,
                                ]
                                .iter()
                                .all(|x| x.as_str() == "0")
                        }),
                )?;
            }
            if let Some(old) = &previous {
                let old: Snapshot = serde_json::from_slice(old).map_err(|_| unavailable())?;
                old.validate()?;
                upload_require(
                    s.generated_at_ms >= old.generated_at_ms
                        && (s.generated_at_ms != old.generated_at_ms
                            || previous.as_deref() == Some(bytes))
                        && !(old.mode == Mode::Live && s.mode != Mode::Live)
                        && old
                            .work
                            .iter()
                            .all(|w| s.work.iter().any(|next| next.id == w.id)),
                )?;
            }
            for w in &s.work {
                let detail: Detail = p.json(&["works", &format!("{}.json", w.id)])?;
                detail.work.validate(detail.generated_at_ms)?;
                upload_require(
                    detail.schema_version == 1
                        && detail.mode == s.mode
                        && detail.generated_at_ms <= s.generated_at_ms,
                )?;
                let mut actual = detail.work;
                if let Some(bytes) = optional_bytes(&p, &["adoption", &format!("{}.json", w.id)])? {
                    let adoption: Adoption =
                        serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
                    adoption.validate(&w.id)?;
                    actual.upstream_status = adoption.status;
                    actual.upstream_url = Some(adoption.url);
                }
                upload_require(
                    serde_json::to_value(actual).map_err(|_| unavailable())?
                        == serde_json::to_value(w).map_err(|_| unavailable())?,
                )?;
                if w.evidence_status != EvidenceStatus::Unpublished {
                    let manifest: Manifest = p.json(&["manifests", &format!("{}.json", w.id)])?;
                    manifest.validate(&w.id)?;
                    manifest_dependencies(&p, &manifest)?;
                }
            }
        }
        "works" => {
            let d: Detail = typed_upload(&value)?;
            upload_require(
                d.schema_version == 1
                    && d.work.id == stem
                    && time(d.generated_at_ms)
                    && d.generated_at_ms <= now + 30_000
                    && d.mode != Mode::Rehearsal,
            )?;
            d.work.validate(d.generated_at_ms)?;
            if let Some(old) = &previous {
                let old: Detail = serde_json::from_slice(old).map_err(|_| unavailable())?;
                upload_require(
                    d.generated_at_ms >= old.generated_at_ms
                        && !(old.mode == Mode::Live && d.mode != Mode::Live),
                )?;
                immutable = old.work.status == WorkStatus::Completed
                    && old.work.evidence_status == EvidenceStatus::Verified;
            }
            if d.work.evidence_status == EvidenceStatus::Verified {
                upload_require(
                    d.mode == Mode::Live
                        && d.work.status == WorkStatus::Completed
                        && d.work.settlement == Settlement::Settled,
                )?;
                let m: Manifest = p.json(&["manifests", filename])?;
                m.validate(stem)?;
                upload_require(
                    m.generated_at_ms == d.generated_at_ms
                        && !m.files.is_empty()
                        && m.verification.work == VerificationStatus::Verified
                        && m.verification.funding == VerificationStatus::Verified,
                )?;
                manifest_dependencies(&p, &m)?;
            }
        }
        "manifests" => {
            let m: Manifest = typed_upload(&value)?;
            m.validate(stem)?;
            upload_require(m.generated_at_ms <= now + 30_000)?;
            manifest_dependencies(&p, &m)?;
        }
        "adoption" => {
            let a: Adoption = typed_upload(&value)?;
            a.validate(stem)?;
            let d: Detail = p.json(&["works", filename])?;
            upload_require(
                d.work.id == stem
                    && d.work.status == WorkStatus::Completed
                    && d.work.evidence_status == EvidenceStatus::Verified,
            )?;
            if let Some(old) = &previous {
                let old: Adoption = serde_json::from_slice(old).map_err(|_| unavailable())?;
                old.validate(stem)?;
                upload_require(
                    old.url == a.url
                        && !(old.status == UpstreamStatus::Merged
                            && a.status != UpstreamStatus::Merged),
                )?;
            }
        }
        "deposits" => {
            upload_require(value == serde_json::json!({"deposit_id":stem}))?;
        }
        "artifacts" => {
            upload_require(sha256_hex(bytes) == stem)?;
            // Only the two public artifact families emitted by publish.py.
            // Signed payload/output text still needs the explicit human review;
            // this transport cannot determine whether those strings are private.
            if value.get("kind").and_then(serde_json::Value::as_str)
                == Some("covenant.community-work.funding")
            {
                upload_require(funding_shape(&value) && value["schema_version"] == 1)?;
            } else {
                let bundle: covenant_compute_protocol::AgentEvidenceBundle = typed_upload(&value)?;
                upload_require(bundle.schema_version == 1)?;
            }
        }
        _ => return Err(invalid_upload()),
    }
    staged_write(root, &components, bytes, immutable)
}
pub async fn upload(
    Path((kind, filename)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let secret = std::env::var(PUBLISH_TOKEN)
        .map_err(|_| ApiError::Unauthorized("Work publication disabled".into()))?;
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if secret.len() < 32 || !bool::from(secret.as_bytes().ct_eq(supplied.as_bytes())) {
        return Err(ApiError::Unauthorized("Work publication denied".into()));
    }
    upload_require(body.len() as u64 <= MAX_BYTES)?;
    let hash = sha256_hex(&body);
    upload_require(
        headers
            .get("x-publication-review-sha256")
            .and_then(|v| v.to_str().ok())
            == Some(hash.as_str()),
    )?;
    let root = Publication::configured()?.root;
    tokio::task::spawn_blocking(move || upload_file(&root, &kind, &filename, &body))
        .await
        .map_err(|_| unavailable())??;
    Ok(public_response(
        serde_json::json!({"published_sha256":hash}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn upload_bytes(value: &serde_json::Value) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(&ordered_json(value)).unwrap();
        bytes.push(b'\n');
        bytes
    }
    fn empty_snapshot(at: u64) -> serde_json::Value {
        json!({"schema_version":1,"generated_at_ms":at,"mode":"unfunded","pool":null,
            "fees":{"cvnt_retained_raw":"0","nvdax_collected_raw":"0","nvdax_converted_raw":"0"},
            "budget":{"available_usdc_micros":"0","reserved_usdc_micros":"0","spent_usdc_micros":"0","subsidy_usdc_micros":"0"},"work":[]})
    }
    fn funding_fixture() -> serde_json::Value {
        let mut config = serde_json::Map::new();
        for key in [
            "pool_address",
            "position_address",
            "owner_address",
            "whirlpools_config",
            "cvnt_account",
            "stock_account",
            "usdc_account",
            "deposit_account",
            "position_token_account",
            "cvnt_mint",
            "stock_mint",
            "usdc_mint",
            "buyer",
        ] {
            config.insert(key.into(), json!("1".repeat(32)));
        }
        config.insert("max_slippage_bps".into(), json!(50));
        config.insert("stock_multiplier".into(), json!("1"));
        json!({"schema_version":1,"kind":"covenant.community-work.funding","work_id":"first-work",
            "contract_sha256":"a".repeat(64),"max_budget_usdc_micros":"0","agent_evidence_sha256":"b".repeat(64),
            "accepted_patch_sha256":"c".repeat(64),"public_config":config,"harvests":[],"conversions":[],"deposit_lots":[],"attempts":[],"limitations":["Synthetic transport fixture, not funding evidence."]})
    }
    #[test]
    fn upload_empty_snapshot_retries_monotonically_and_refuses_private_fields() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let original = upload_bytes(&empty_snapshot(1000));
        upload_file(&root, "snapshots", "snapshot.json", &original).unwrap();
        upload_file(&root, "snapshots", "snapshot.json", &original).unwrap();
        assert_eq!(
            Publication { root: root.clone() }
                .bytes(&["snapshot.json"])
                .unwrap(),
            original
        );
        let mut invalid = empty_snapshot(1001);
        invalid["private_buyer"] = json!("must not persist");
        assert!(upload_file(&root, "snapshots", "snapshot.json", &upload_bytes(&invalid)).is_err());
        let mut invalid = empty_snapshot(1001);
        invalid["budget"]["available_usdc_micros"] = json!("1");
        assert!(upload_file(&root, "snapshots", "snapshot.json", &upload_bytes(&invalid)).is_err());
        assert!(upload_file(
            &root,
            "snapshots",
            "snapshot.json",
            &upload_bytes(&empty_snapshot(999))
        )
        .is_err());
        let mut changed = empty_snapshot(1000);
        changed["mode"] = json!("live");
        assert!(upload_file(&root, "snapshots", "snapshot.json", &upload_bytes(&changed)).is_err());
        upload_file(
            &root,
            "snapshots",
            "snapshot.json",
            &upload_bytes(&empty_snapshot(1001)),
        )
        .unwrap();
    }
    #[test]
    fn upload_paths_json_and_newline_hashes_are_strict() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let bytes = upload_bytes(&funding_fixture());
        let sha = sha256_hex(&bytes);
        assert_ne!(sha, sha256_hex(&bytes[..bytes.len() - 1]));
        upload_file(&root, "artifacts", &format!("{sha}.json"), &bytes).unwrap();
        assert_eq!(
            Publication { root: root.clone() }
                .bytes(&["artifacts", &format!("{sha}.json")])
                .unwrap(),
            bytes
        );
        for (kind, file) in [
            ("private", "snapshot.json"),
            ("works", "../secret.json"),
            ("artifacts", "BAD.json"),
            ("snapshots", "other.json"),
        ] {
            assert!(upload_file(&root, kind, file, &bytes).is_err());
        }
        assert!(upload_file(
            &root,
            "artifacts",
            &format!("{}.json", "a".repeat(64)),
            &bytes
        )
        .is_err());
        assert!(upload_json(b"{\"x\":1,\"x\":2}\n").is_err());
        assert!(upload_json(b"{\"x\":1.0}\n").is_err());
        assert!(upload_json(&vec![b' '; MAX_BYTES as usize + 1]).is_err());
        let mut private = funding_fixture();
        private["public_config"]["private_path"] = json!("secret");
        let private = upload_bytes(&private);
        let hash = sha256_hex(&private);
        assert!(upload_file(&root, "artifacts", &format!("{hash}.json"), &private).is_err());
    }
    #[test]
    fn upload_completed_evidence_is_immutable_and_snapshot_waits_for_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut w = serde_json::to_value(serde_json::from_value::<Work>(work()).unwrap()).unwrap();
        w["status"] = json!("completed");
        w["completed_at_ms"] = json!(900);
        w["settlement"] = json!("settled");
        w["evidence_status"] = json!("verified");
        let d = json!({"schema_version":1,"generated_at_ms":1000,"mode":"live","work":w});
        let mut snapshot = empty_snapshot(1000);
        snapshot["mode"] = json!("live");
        snapshot["work"] = json!([w]);
        assert!(upload_file(
            &root,
            "snapshots",
            "snapshot.json",
            &upload_bytes(&snapshot)
        )
        .is_err());
        assert!(upload_file(&root, "works", "first-work.json", &upload_bytes(&d)).is_err());
        let artifact = upload_bytes(&funding_fixture());
        let sha = sha256_hex(&artifact);
        let m = json!({"schema_version":1,"work_id":"first-work","generated_at_ms":1000,"files":[{"name":"funding.json","sha256":sha,"bytes":artifact.len(),"download_url":format!("https://compute-api.opencovenant.org/community-work/artifacts/{sha}.json")}],"verification":{"work":"verified","funding":"verified"},"limitations":["Transport fixture only"]});
        assert!(upload_file(&root, "manifests", "first-work.json", &upload_bytes(&m)).is_err());
        upload_file(&root, "artifacts", &format!("{sha}.json"), &artifact).unwrap();
        upload_file(&root, "manifests", "first-work.json", &upload_bytes(&m)).unwrap();
        upload_file(&root, "works", "first-work.json", &upload_bytes(&d)).unwrap();
        upload_file(
            &root,
            "snapshots",
            "snapshot.json",
            &upload_bytes(&snapshot),
        )
        .unwrap();
        upload_file(&root, "works", "first-work.json", &upload_bytes(&d)).unwrap();
        let mut changed = d.clone();
        changed["work"]["summary"] = json!("changed after acceptance");
        assert!(upload_file(&root, "works", "first-work.json", &upload_bytes(&changed)).is_err());
        let mut changed = m.clone();
        changed["limitations"] = json!([]);
        assert!(upload_file(
            &root,
            "manifests",
            "first-work.json",
            &upload_bytes(&changed)
        )
        .is_err());
        let mut removed = snapshot.clone();
        removed["generated_at_ms"] = json!(1001);
        removed["work"] = json!([]);
        assert!(upload_file(&root, "snapshots", "snapshot.json", &upload_bytes(&removed)).is_err());
        let p = Publication { root };
        assert_eq!(p.artifact(&sha).unwrap(), artifact);
    }
    #[test]
    fn upload_deposit_only_stages_allowlist_and_interrupted_staging_is_retryable() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let signature = "1".repeat(64);
        let name = format!("{signature}.json");
        let bytes = upload_bytes(&json!({"deposit_id":signature}));
        fs::write(root.join(".work-upload-interrupted"), b"partial").unwrap();
        upload_file(&root, "deposits", &name, &bytes).unwrap();
        upload_file(&root, "deposits", &name, &bytes).unwrap();
        let accounts = crate::accounts::BuyerAccounts::new();
        assert!(Publication { root: root.clone() }
            .deposit_record(&signature, &accounts)
            .is_err());
        assert!(!accounts.has_deposit(&signature));
        fs::remove_file(root.join("deposits").join(&name)).unwrap();
        std::os::unix::fs::symlink("/nonexistent", root.join("deposits").join(&name)).unwrap();
        assert!(upload_file(&root, "deposits", &name, &bytes).is_err());
    }
    #[tokio::test]
    async fn upload_requires_dedicated_auth_and_exact_explicit_review_hash() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::env::set_var(ENV, &root);
        std::env::set_var(PUBLISH_TOKEN, "x".repeat(32));
        let body = Bytes::from(upload_bytes(&empty_snapshot(1000)));
        let mut headers = HeaderMap::new();
        assert!(upload(
            Path(("snapshots".into(), "snapshot.json".into())),
            headers.clone(),
            body.clone()
        )
        .await
        .is_err());
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"),
        );
        assert!(upload(
            Path(("snapshots".into(), "snapshot.json".into())),
            headers.clone(),
            body.clone()
        )
        .await
        .is_err());
        headers.insert(
            "x-publication-review-sha256",
            HeaderValue::from_str(&"a".repeat(64)).unwrap(),
        );
        assert!(upload(
            Path(("snapshots".into(), "snapshot.json".into())),
            headers.clone(),
            body.clone()
        )
        .await
        .is_err());
        headers.insert(
            "x-publication-review-sha256",
            HeaderValue::from_str(&sha256_hex(&body)).unwrap(),
        );
        assert!(upload(
            Path(("snapshots".into(), "snapshot.json".into())),
            headers,
            body
        )
        .await
        .is_ok());
        std::env::remove_var(ENV);
        std::env::remove_var(PUBLISH_TOKEN);
    }
    fn work() -> serde_json::Value {
        json!({"id":"first-work","title":"Fix a bug","summary":"Public work","status":"candidate","max_budget_usdc_micros":"0","acceptance":[],"source_url":null,"completed_at_ms":null,"cost_usdc_micros":"0","fee_funded_usdc_micros":"0","subsidy_usdc_micros":"0","settlement":"unfunded","evidence_status":"unchecked","before":null,"after":null,"patch_url":null,"upstream_status":"not_submitted","upstream_url":null,"checks":[],"transactions":[],"attempts":[],"private_buyer":"must disappear"})
    }
    fn fixture() -> (tempfile::TempDir, Publication) {
        let dir = tempfile::tempdir().unwrap();
        for d in ["works", "manifests", "artifacts"] {
            std::fs::create_dir(dir.path().join(d)).unwrap()
        }
        let w = work();
        let snap = json!({"schema_version":1,"generated_at_ms":1000,"mode":"unfunded","pool":null,"fees":{"cvnt_retained_raw":"0","nvdax_collected_raw":"0","nvdax_converted_raw":"0"},"budget":{"available_usdc_micros":"0","reserved_usdc_micros":"0","spent_usdc_micros":"0","subsidy_usdc_micros":"0"},"work":[w.clone()],"private_credentials":"must disappear"});
        std::fs::write(
            dir.path().join("snapshot.json"),
            serde_json::to_vec(&snap).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("works/first-work.json"),
            serde_json::to_vec(
                &json!({"schema_version":1,"generated_at_ms":1000,"mode":"unfunded","work":w}),
            )
            .unwrap(),
        )
        .unwrap();
        let store = Publication {
            root: dir.path().to_path_buf(),
        };
        (dir, store)
    }
    #[test]
    fn typed_publication_strips_private_fields_and_keeps_original_age() {
        let (_d, s) = fixture();
        let value = serde_json::to_value(s.snapshot().unwrap()).unwrap();
        assert_eq!(value["generated_at_ms"], 1000);
        assert!(value.get("private_credentials").is_none());
        assert!(value["work"][0].get("private_buyer").is_none());
    }
    fn snapshot_with_reports() -> serde_json::Value {
        json!({
            "schema_version":1,"generated_at_ms":1_000_000,"mode":"live",
            "pool":{
                "address":"1".repeat(32),"position_address":"2".repeat(32),
                "active":true,"finalized_slot":500,"observed_at_ms":900_000,
                "fee_bps":30,"range":"full","cvnt_raw":"100","nvdax_raw":"200",
                "cvnt_fees_owed_raw":"0","nvdax_fees_owed_raw":"0","nvdax_multiplier":"1.25",
                "volume_usdc_micros":"1000000","lp_value_usdc_micros":"900000000",
                "hold_value_usdc_micros":"910000000",
                "market":{
                    "source":"orca_public_api","observed_at_ms":899_000,
                    "source_updated_at_ms":898_000,"source_updated_slot":499,"window_seconds":86400,
                    "private_request_headers":{"Authorization":"must disappear"}
                },
                "valuation":{
                    "source":"operator_reviewed_marks","observed_at_ms":899_000,
                    "baseline_at_ms":1_000,"baseline_slot":1,"baseline_sha256":"a".repeat(64),
                    "marks_sha256":"b".repeat(64),"price_basis":"unscaled_token",
                    "cvnt_price_usdc":"0.01","nvdax_price_usdc":"200.125",
                    "private_operator_notes":"must disappear"
                }
            },
            "fees":{"cvnt_retained_raw":"0","nvdax_collected_raw":"0","nvdax_converted_raw":"0"},
            "budget":{"available_usdc_micros":"0","reserved_usdc_micros":"0","spent_usdc_micros":"0","subsidy_usdc_micros":"0"},
            "work":[]
        })
    }
    fn valid_snapshot(value: serde_json::Value) -> bool {
        serde_json::from_value::<Snapshot>(value).is_ok_and(|s| s.validate().is_ok())
    }
    #[test]
    fn pool_reports_strip_private_fields_and_require_metric_provenance() {
        let value = snapshot_with_reports();
        let snapshot: Snapshot = serde_json::from_value(value.clone()).unwrap();
        assert!(snapshot.validate().is_ok());
        let public = serde_json::to_value(snapshot).unwrap();
        assert!(public["pool"]["market"]
            .get("private_request_headers")
            .is_none());
        assert!(public["pool"]["valuation"]
            .get("private_operator_notes")
            .is_none());
        assert_eq!(public["pool"]["market"]["source"], "orca_public_api");
        assert_eq!(public["pool"]["valuation"]["price_basis"], "unscaled_token");
        for field in [
            "market",
            "valuation",
            "volume_usdc_micros",
            "lp_value_usdc_micros",
            "hold_value_usdc_micros",
        ] {
            let mut invalid = value.clone();
            invalid["pool"][field] = serde_json::Value::Null;
            assert!(!valid_snapshot(invalid), "missing {field}");
        }
        let mut old = value.clone();
        for field in ["market", "valuation"] {
            old["pool"].as_object_mut().unwrap().remove(field);
        }
        assert!(!valid_snapshot(old.clone()));
        for field in [
            "volume_usdc_micros",
            "lp_value_usdc_micros",
            "hold_value_usdc_micros",
        ] {
            old["pool"][field] = serde_json::Value::Null;
        }
        assert!(valid_snapshot(old.clone()));
        old["pool"]["market"] = serde_json::Value::Null;
        old["pool"]["valuation"] = serde_json::Value::Null;
        assert!(valid_snapshot(old));
        for field in [
            "volume_usdc_micros",
            "lp_value_usdc_micros",
            "hold_value_usdc_micros",
        ] {
            let mut invalid = value.clone();
            invalid["pool"][field] = json!("9".repeat(40));
            assert!(!valid_snapshot(invalid), "unbounded {field}");
        }
    }
    #[test]
    fn market_reports_require_fresh_ordered_timestamps_and_finalized_slots() {
        let value = snapshot_with_reports();
        for (field, replacement) in [
            ("source", json!("unreviewed_api")),
            ("observed_at_ms", json!(900_001)),
            ("observed_at_ms", json!(0)),
            ("observed_at_ms", json!(819_999)),
            ("source_updated_at_ms", json!(899_001)),
            ("source_updated_at_ms", json!(0)),
            ("source_updated_at_ms", json!(819_999)),
            ("source_updated_slot", json!(0)),
            ("source_updated_slot", json!(501)),
            ("window_seconds", json!(3600)),
        ] {
            let mut invalid = value.clone();
            invalid["pool"]["market"][field] = replacement;
            assert!(!valid_snapshot(invalid), "invalid {field}");
        }
        for slot in [serde_json::Value::Null, json!(0), json!(MAX_SLOT + 1)] {
            let mut invalid = value.clone();
            invalid["pool"]["finalized_slot"] = slot;
            assert!(!valid_snapshot(invalid));
        }
        let mut boundary = value;
        boundary["pool"]["market"]["observed_at_ms"] = json!(820_000);
        boundary["pool"]["market"]["source_updated_at_ms"] = json!(820_000);
        boundary["pool"]["market"]["source_updated_slot"] = json!(500);
        assert!(valid_snapshot(boundary.clone()));
        // Refreshing only the publication timestamp cannot renew old observations.
        boundary["generated_at_ms"] = json!(1_000_001);
        assert!(!valid_snapshot(boundary));
    }
    #[test]
    fn valuation_reports_require_fresh_marks_bound_baselines_and_positive_prices() {
        let value = snapshot_with_reports();
        for (field, replacement) in [
            ("source", json!("chain_confirmed")),
            ("price_basis", json!("display_token")),
            ("observed_at_ms", json!(900_001)),
            ("observed_at_ms", json!(0)),
            ("observed_at_ms", json!(819_999)),
            ("baseline_at_ms", json!(900_000)),
            ("baseline_at_ms", json!(0)),
            ("baseline_slot", json!(0)),
            ("baseline_slot", json!(501)),
            ("baseline_sha256", json!("A".repeat(64))),
            ("marks_sha256", json!("b".repeat(63))),
        ] {
            let mut invalid = value.clone();
            invalid["pool"]["valuation"][field] = replacement;
            assert!(!valid_snapshot(invalid), "invalid {field}");
        }
        for field in ["cvnt_price_usdc", "nvdax_price_usdc"] {
            for price in [
                "0".into(),
                "-1".into(),
                "1e3".into(),
                "NaN".into(),
                "Infinity".into(),
                "01".into(),
                "1.".into(),
                "9".repeat(40),
                format!("0.{}1", "0".repeat(38)),
            ] {
                let mut invalid = value.clone();
                invalid["pool"]["valuation"][field] = json!(price);
                assert!(!valid_snapshot(invalid), "invalid {field}: {price}");
            }
        }
        let mut boundary = value;
        boundary["pool"]["valuation"]["observed_at_ms"] = json!(820_000);
        boundary["pool"]["valuation"]["baseline_at_ms"] = json!(820_000);
        boundary["pool"]["valuation"]["baseline_slot"] = json!(500);
        boundary["pool"]["valuation"]["cvnt_price_usdc"] = json!("9".repeat(39));
        boundary["pool"]["valuation"]["nvdax_price_usdc"] = json!(format!("0.{}1", "0".repeat(37)));
        assert!(valid_snapshot(boundary.clone()));
        boundary["generated_at_ms"] = json!(1_000_001);
        assert!(!valid_snapshot(boundary));
    }
    #[test]
    fn work_contract_and_decimal_attribution_are_bounded() {
        let mut w: Work = serde_json::from_value(work()).unwrap();
        w.status = WorkStatus::Approved;
        assert!(w.validate(1000).is_err());
        w.max_budget_usdc_micros = "10".into();
        w.acceptance = vec!["Check passes".into()];
        assert!(w.validate(1000).is_ok());
        w.fee_funded_usdc_micros = "1".into();
        assert!(w.validate(1000).is_err());
        assert!(decimal_sum_le(
            "999999999999999999999999999999999999998",
            "1",
            "999999999999999999999999999999999999999"
        ));
        assert!(!decimal_sum_le(
            "999999999999999999999999999999999999999",
            "1",
            "999999999999999999999999999999999999999"
        ));
    }
    #[test]
    fn artifact_must_be_published_bound_and_hash_correct() {
        let (d, s) = fixture();
        let bytes = b"{\"public\":true}";
        let sha = sha256_hex(bytes);
        std::fs::write(d.path().join(format!("artifacts/{sha}.json")), bytes).unwrap();
        assert!(s.artifact(&sha).is_err());
        let m = json!({"schema_version":1,"work_id":"first-work","generated_at_ms":1000,"files":[{"name":"evidence.json","sha256":sha,"bytes":bytes.len(),"download_url":format!("https://compute-api.opencovenant.org/community-work/artifacts/{sha}.json")}],"verification":{"work":"unchecked","funding":"unchecked"},"limitations":["Coordinator assertions remain separate"]});
        std::fs::write(
            d.path().join("manifests/first-work.json"),
            serde_json::to_vec(&m).unwrap(),
        )
        .unwrap();
        assert_eq!(s.artifact(&sha).unwrap(), bytes);
        std::fs::write(
            d.path().join(format!("artifacts/{sha}.json")),
            b"{\"public\":null}",
        )
        .unwrap();
        assert!(s.artifact(&sha).is_err());
        assert!(s.detail("../private").is_err());
        assert!(s.artifact(&"a".repeat(64)).is_err());
    }
    #[test]
    fn adoption_overlay_preserves_completed_core_and_evidence() {
        let (d, s) = fixture();
        let core_path = d.path().join("works/first-work.json");
        let mut core: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&core_path).unwrap()).unwrap();
        core["work"]["status"] = json!("completed");
        core["work"]["completed_at_ms"] = json!(900);
        let core_bytes = serde_json::to_vec(&core).unwrap();
        std::fs::write(&core_path, &core_bytes).unwrap();
        assert_eq!(
            s.detail("first-work").unwrap().work.upstream_status,
            UpstreamStatus::NotSubmitted
        );
        let manifest = json!({"schema_version":1,"work_id":"first-work","generated_at_ms":1000,"files":[],"verification":{"work":"unchecked","funding":"unchecked"},"limitations":[]});
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let manifest_path = d.path().join("manifests/first-work.json");
        std::fs::write(&manifest_path, &manifest_bytes).unwrap();
        std::fs::create_dir(d.path().join("adoption")).unwrap();
        let adoption_path = d.path().join("adoption/first-work.json");
        for status in ["submitted", "merged"] {
            std::fs::write(&adoption_path, serde_json::to_vec(&json!({"schema_version":1,"work_id":"first-work","status":status,"url":"https://github.com/forum-labs/payfetch/pull/99"})).unwrap()).unwrap();
            let detail = serde_json::to_value(s.detail("first-work").unwrap()).unwrap();
            assert_eq!(detail["work"]["upstream_status"], status);
            assert_eq!(
                detail["work"]["upstream_url"],
                "https://github.com/forum-labs/payfetch/pull/99"
            );
            assert_eq!(detail["generated_at_ms"], 1000);
            assert_eq!(detail["work"]["completed_at_ms"], 900);
            assert_eq!(std::fs::read(&core_path).unwrap(), core_bytes);
            assert_eq!(std::fs::read(&manifest_path).unwrap(), manifest_bytes);
        }
        // A malformed mutable annotation cannot revoke immutable evidence.
        std::fs::write(&adoption_path, b"{broken").unwrap();
        assert!(s.detail("first-work").is_err());
        assert!(s.manifest("first-work").is_ok());
    }
    #[test]
    fn adoption_annotation_rejects_wrong_identity_status_and_non_pr_urls() {
        let (d, s) = fixture();
        std::fs::create_dir(d.path().join("adoption")).unwrap();
        let path = d.path().join("adoption/first-work.json");
        let valid = json!({"schema_version":1,"work_id":"first-work","status":"submitted","url":"https://github.com/forum-labs/payfetch/pull/99"});
        for (key, value) in [
            ("schema_version", json!(2)),
            ("work_id", json!("other-work")),
            ("status", json!("not_submitted")),
        ] {
            let mut invalid = valid.clone();
            invalid[key] = value;
            std::fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
            assert!(s.detail("first-work").is_err());
        }
        for url in [
            "https://github.com/forum-labs/payfetch/issues/99",
            "https://github.com/forum-labs/payfetch/pull/99?x=secret",
            "https://github.com/forum-labs/payfetch/pull/99#comment",
            "https://github.com:443/forum-labs/payfetch/pull/99",
            "https://user@github.com/forum-labs/payfetch/pull/99",
            "https://evil.example/forum-labs/payfetch/pull/99",
        ] {
            let mut invalid = valid.clone();
            invalid["url"] = json!(url);
            std::fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
            assert!(s.detail("first-work").is_err(), "{url}");
        }
    }
    #[cfg(unix)]
    #[test]
    fn adoption_symlink_is_not_treated_as_absent() {
        let (d, s) = fixture();
        std::fs::create_dir(d.path().join("adoption")).unwrap();
        std::os::unix::fs::symlink("/nonexistent", d.path().join("adoption/first-work.json"))
            .unwrap();
        assert!(s.detail("first-work").is_err());
    }
    #[test]
    fn malformed_and_oversized_data_fail_closed() {
        let (d, s) = fixture();
        std::fs::write(d.path().join("snapshot.json"), b"{broken").unwrap();
        assert!(s.snapshot().is_err());
        std::fs::write(
            d.path().join("snapshot.json"),
            vec![b' '; MAX_BYTES as usize + 1],
        )
        .unwrap();
        assert!(s.snapshot().is_err());
    }
    #[cfg(unix)]
    #[test]
    fn symlinks_are_never_followed() {
        let (d, s) = fixture();
        let secret = tempfile::NamedTempFile::new().unwrap();
        std::fs::remove_file(d.path().join("snapshot.json")).unwrap();
        std::os::unix::fs::symlink(secret.path(), d.path().join("snapshot.json")).unwrap();
        assert!(s.snapshot().is_err());
        std::fs::remove_dir_all(d.path().join("works")).unwrap();
        std::os::unix::fs::symlink(std::env::temp_dir(), d.path().join("works")).unwrap();
        assert!(s.bytes(&["works", "secret.json"]).is_err());
    }
    #[test]
    fn deposit_receipt_requires_both_explicit_public_staging_and_book_credit() {
        let (d, s) = fixture();
        let a = crate::accounts::BuyerAccounts::new();
        let signature = "1".repeat(64);
        a.credit_deposit(&signature, "private-buyer", 10).unwrap();
        assert!(s.deposit_record(&signature, &a).is_err());
        std::fs::create_dir(d.path().join("deposits")).unwrap();
        let path = d.path().join(format!("deposits/{signature}.json"));
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({"deposit_id":signature,"private_buyer":"must disappear"}))
                .unwrap(),
        )
        .unwrap();
        let value = s.deposit_record(&signature, &a).unwrap();
        assert_eq!(value, json!({"deposit_id":signature,"recorded":true}));
        assert!(s
            .deposit_record(&signature, &crate::accounts::BuyerAccounts::new())
            .is_err());
        std::fs::write(&path, b"{\"deposit_id\":\"another signature\"}").unwrap();
        assert!(s.deposit_record(&signature, &a).is_err());
        assert!(s.deposit_record("../private", &a).is_err());
    }
}
