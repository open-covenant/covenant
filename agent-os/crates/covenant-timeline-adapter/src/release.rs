use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::AdapterError;

pub const OBSERVATION_SCHEMA: &str = "covenant.release-temporal-observation.v1";
pub const TEMPORAL_CONTRACT_SCHEMA: &str = "covenant.timeline.contract.v0alpha3";
pub const TEMPORAL_EVENT_SCHEMA: &str = "covenant.timeline.event.v0alpha3";
pub const TEMPORAL_RUN_SCHEMA: &str = "covenant.timeline.run.v0alpha3";
pub const MAX_JSON_BYTES: usize = 1024 * 1024;

const AXIS_ID: &str = "unix-ms";
const CONTEXT_ID: &str = "actual";
const PROVISIONAL_ASSERTION_ID: &str = "release.publication.provisional.v1";

#[derive(Debug, thiserror::Error)]
pub enum ReleaseWorkflowError {
    #[error("read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parse {path}: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("{path} exceeds the {maximum}-byte JSON input limit")]
    InputTooLarge { path: PathBuf, maximum: usize },
    #[error("serialize release Timeline state: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error("write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("release Timeline state already exists: {0}")]
    StateExists(PathBuf),
    #[error("release Timeline state is locked by another process: {0}")]
    StateLocked(PathBuf),
    #[error("release Timeline state changed during reconciliation")]
    StateChanged,
    #[error("release Timeline state has already been reconciled")]
    AlreadyReconciled,
    #[error("invalid {observation} observation: {reason}")]
    Observation { observation: String, reason: String },
    #[error("release observations identify different releases or tagged commits")]
    IdentityMismatch,
    #[error("invalid release Timeline state: {0}")]
    State(String),
    #[error(transparent)]
    Adapter(#[from] AdapterError),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseObservation {
    pub schema: String,
    pub id: String,
    pub repository: String,
    pub release: String,
    #[serde(rename = "tagCommit")]
    pub tag_commit: String,
    pub source: Map<String, Value>,
    pub fact: ReleaseFact,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum ReleaseFact {
    #[serde(rename = "release.created", rename_all = "camelCase")]
    Created {
        occurred_at: String,
        coordinate_ms: i64,
    },
    #[serde(rename = "release.readiness-recorded", rename_all = "camelCase")]
    ReadinessRecorded {
        occurred_at: String,
        coordinate_ms: i64,
        commit: String,
        ready: bool,
    },
    #[serde(rename = "release.published", rename_all = "camelCase")]
    Published {
        occurred_at: String,
        coordinate_ms: i64,
    },
}

impl ReleaseFact {
    pub fn occurred_at(&self) -> &str {
        match self {
            Self::Created { occurred_at, .. }
            | Self::ReadinessRecorded { occurred_at, .. }
            | Self::Published { occurred_at, .. } => occurred_at,
        }
    }

    pub fn coordinate_ms(&self) -> i64 {
        match self {
            Self::Created { coordinate_ms, .. }
            | Self::ReadinessRecorded { coordinate_ms, .. }
            | Self::Published { coordinate_ms, .. } => *coordinate_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseTimelineRun {
    pub schema: String,
    pub contract: TemporalContract,
    pub events: Vec<TemporalEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemporalContract {
    pub schema: String,
    pub id: String,
    pub subject: TemporalSubject,
    pub axes: Vec<TemporalAxis>,
    pub contexts: Vec<TemporalContext>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemporalSubject {
    pub kind: String,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemporalAxis {
    pub id: String,
    pub kind: String,
    pub unit: String,
    pub origin: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemporalContext {
    pub id: String,
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum TemporalEvent {
    #[serde(rename = "point.declared")]
    PointDeclared {
        schema: String,
        id: String,
        sequence: u64,
        point: TemporalPoint,
    },
    #[serde(rename = "coordinate.asserted")]
    CoordinateAsserted {
        schema: String,
        id: String,
        sequence: u64,
        assertion: CoordinateAssertion,
    },
    #[serde(rename = "assertion.retracted", rename_all = "camelCase")]
    AssertionRetracted {
        schema: String,
        id: String,
        sequence: u64,
        assertion_id: String,
        evidence_refs: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemporalPoint {
    pub id: String,
    pub context_id: String,
    pub axis_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CoordinateAssertion {
    pub id: String,
    pub context_id: String,
    pub point_id: String,
    pub coordinate: TemporalCoordinate,
    pub evidence_refs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemporalCoordinate {
    pub minimum: i64,
    pub maximum: i64,
}

pub fn read_observation(
    path: impl AsRef<Path>,
) -> Result<ReleaseObservation, ReleaseWorkflowError> {
    read_json(path.as_ref())
}

pub fn observation_digest(
    observation: &ReleaseObservation,
) -> Result<String, ReleaseWorkflowError> {
    let value = serde_json::to_value(observation).map_err(AdapterError::from)?;
    Ok(crate::digest(&value)?)
}

pub fn build_initial_run(
    created: &ReleaseObservation,
    readiness: &ReleaseObservation,
) -> Result<ReleaseTimelineRun, ReleaseWorkflowError> {
    validate_observation(created, "release-created", FactKind::Created)?;
    validate_observation(readiness, "readiness-recorded", FactKind::ReadinessRecorded)?;
    validate_identity(created, readiness)?;

    let contract = expected_contract(&created.repository, &created.release)?;
    let created_digest = observation_digest(created)?;
    let readiness_digest = observation_digest(readiness)?;

    Ok(ReleaseTimelineRun {
        schema: TEMPORAL_RUN_SCHEMA.into(),
        contract,
        events: vec![
            point_event(0, "event.release-publication-point", "artifacts-published"),
            point_event(
                1,
                "event.tagged-commit-readiness-point",
                "tagged-commit-readiness-recorded",
            ),
            coordinate_event(
                2,
                "event.release-publication-provisional",
                PROVISIONAL_ASSERTION_ID,
                "artifacts-published",
                created.fact.coordinate_ms(),
                created_digest,
                None,
            ),
            coordinate_event(
                3,
                "event.tagged-commit-readiness",
                "release.tagged-commit-readiness.v1",
                "tagged-commit-readiness-recorded",
                readiness.fact.coordinate_ms(),
                readiness_digest,
                None,
            ),
        ],
    })
}

pub fn initialize_release_timeline(
    created_path: impl AsRef<Path>,
    readiness_path: impl AsRef<Path>,
    state_path: impl AsRef<Path>,
) -> Result<ReleaseTimelineRun, ReleaseWorkflowError> {
    let created_path = created_path.as_ref();
    let readiness_path = readiness_path.as_ref();
    let state_path = state_path.as_ref();
    with_state_lock(state_path, || {
        let created = read_observation(created_path)?;
        let readiness = read_observation(readiness_path)?;
        let run = build_initial_run(&created, &readiness)?;
        write_new(state_path, &run)?;
        Ok(run)
    })
}

pub fn reconcile_release_timeline(
    created_path: impl AsRef<Path>,
    readiness_path: impl AsRef<Path>,
    state_path: impl AsRef<Path>,
    published_path: impl AsRef<Path>,
) -> Result<ReleaseTimelineRun, ReleaseWorkflowError> {
    let created_path = created_path.as_ref();
    let readiness_path = readiness_path.as_ref();
    let state_path = state_path.as_ref();
    let published_path = published_path.as_ref();
    with_state_lock(state_path, || {
        let created = read_observation(created_path)?;
        let readiness = read_observation(readiness_path)?;
        let expected_initial = build_initial_run(&created, &readiness)?;
        let original = read_bytes(state_path)?;
        let mut run: ReleaseTimelineRun =
            serde_json::from_slice(&original).map_err(|source| ReleaseWorkflowError::Parse {
                path: state_path.to_path_buf(),
                source,
            })?;
        let published = read_observation(published_path)?;
        validate_observation(&published, "release-published", FactKind::Published)?;
        validate_identity(&created, &published)?;

        let contract = expected_contract(&published.repository, &published.release)?;
        validate_initial_state(&run, &contract)?;
        if run.events[..4] != expected_initial.events {
            return Err(ReleaseWorkflowError::State(
                "initial event prefix does not match its admitted observations".into(),
            ));
        }
        if run.events.len() == 6 {
            validate_reconciliation(&run, &published)?;
            return Err(ReleaseWorkflowError::AlreadyReconciled);
        }
        if run.events.len() != 4 {
            return Err(ReleaseWorkflowError::State(format!(
                "expected 4 events before reconciliation, found {}",
                run.events.len()
            )));
        }

        let published_digest = observation_digest(&published)?;
        run.events.push(coordinate_event(
            4,
            "event.release-publication-authoritative",
            "release.publication.github.v1",
            "artifacts-published",
            published.fact.coordinate_ms(),
            published_digest.clone(),
            None,
        ));
        run.events.push(TemporalEvent::AssertionRetracted {
            schema: TEMPORAL_EVENT_SCHEMA.into(),
            id: "event.release-publication-reconciled".into(),
            sequence: 5,
            assertion_id: PROVISIONAL_ASSERTION_ID.into(),
            evidence_refs: vec![published_digest],
        });

        if read_bytes(state_path)? != original {
            return Err(ReleaseWorkflowError::StateChanged);
        }
        write_replace(state_path, &run)?;
        Ok(run)
    })
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, ReleaseWorkflowError> {
    let bytes = read_bytes(path)?;
    serde_json::from_slice(&bytes).map_err(|source| ReleaseWorkflowError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, ReleaseWorkflowError> {
    let file = File::open(path).map_err(|source| ReleaseWorkflowError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let mut bytes = Vec::new();
    file.take((MAX_JSON_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| ReleaseWorkflowError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() > MAX_JSON_BYTES {
        return Err(ReleaseWorkflowError::InputTooLarge {
            path: path.to_path_buf(),
            maximum: MAX_JSON_BYTES,
        });
    }
    Ok(bytes)
}

fn write_new(path: &Path, run: &ReleaseTimelineRun) -> Result<(), ReleaseWorkflowError> {
    let parent = parent_directory(path);
    let mut temporary = TemporaryState::new(parent, path)?;
    write_run(&mut temporary, path, run)?;
    temporary.persist_noclobber(path)?;
    sync_directory(parent, path)
}

fn write_replace(path: &Path, run: &ReleaseTimelineRun) -> Result<(), ReleaseWorkflowError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| ReleaseWorkflowError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.file_type().is_file() {
        return Err(ReleaseWorkflowError::State(
            "state path is not a regular file".into(),
        ));
    }

    let parent = parent_directory(path);
    let mut temporary = TemporaryState::new(parent, path)?;
    write_run(&mut temporary, path, run)?;
    temporary.persist_replace(path)?;
    sync_directory(parent, path)
}

fn write_run(
    temporary: &mut TemporaryState,
    path: &Path,
    run: &ReleaseTimelineRun,
) -> Result<(), ReleaseWorkflowError> {
    let mut bytes = serde_json::to_vec_pretty(run).map_err(ReleaseWorkflowError::Serialize)?;
    bytes.push(b'\n');
    let file = temporary
        .file
        .as_mut()
        .expect("temporary state file is open");
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|source| ReleaseWorkflowError::Write {
            path: path.to_path_buf(),
            source,
        })
}

fn sync_directory(parent: &Path, path: &Path) -> Result<(), ReleaseWorkflowError> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| ReleaseWorkflowError::Write {
            path: path.to_path_buf(),
            source,
        })
}

fn parent_directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

pub fn state_lock_path(state_path: impl AsRef<Path>) -> PathBuf {
    let mut path = state_path.as_ref().as_os_str().to_os_string();
    path.push(".lock");
    PathBuf::from(path)
}

fn with_state_lock<T>(
    state_path: &Path,
    operation: impl FnOnce() -> Result<T, ReleaseWorkflowError>,
) -> Result<T, ReleaseWorkflowError> {
    let _lock = StateLock::acquire(state_path)?;
    operation()
}

struct StateLock {
    _file: File,
}

impl StateLock {
    fn acquire(state_path: &Path) -> Result<Self, ReleaseWorkflowError> {
        let path = state_lock_path(state_path);
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .map_err(|source| ReleaseWorkflowError::Write {
                path: path.clone(),
                source,
            })?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => Err(ReleaseWorkflowError::StateLocked(path)),
            Err(std::fs::TryLockError::Error(source)) => {
                Err(ReleaseWorkflowError::Write { path, source })
            }
        }
    }
}

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TemporaryState {
    path: PathBuf,
    file: Option<File>,
}

impl TemporaryState {
    fn new(parent: &Path, target: &Path) -> Result<Self, ReleaseWorkflowError> {
        let name = target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("timeline-state");
        for _ in 0..1_000 {
            let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(".{name}.{}.{sequence}.tmp", std::process::id()));
            let mut options = OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(source) => {
                    return Err(ReleaseWorkflowError::Write {
                        path: target.to_path_buf(),
                        source,
                    });
                }
            }
        }
        Err(ReleaseWorkflowError::Write {
            path: target.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "could not allocate a temporary state file",
            ),
        })
    }

    fn persist_noclobber(&mut self, target: &Path) -> Result<(), ReleaseWorkflowError> {
        drop(self.file.take());
        fs::hard_link(&self.path, target).map_err(|source| {
            if source.kind() == std::io::ErrorKind::AlreadyExists {
                ReleaseWorkflowError::StateExists(target.to_path_buf())
            } else {
                ReleaseWorkflowError::Write {
                    path: target.to_path_buf(),
                    source,
                }
            }
        })?;
        if fs::remove_file(&self.path).is_ok() {
            self.path.clear();
        }
        Ok(())
    }

    fn persist_replace(&mut self, target: &Path) -> Result<(), ReleaseWorkflowError> {
        drop(self.file.take());
        fs::rename(&self.path, target)
            .map_err(|source| ReleaseWorkflowError::Write {
                path: target.to_path_buf(),
                source,
            })
            .map(|_| {
                self.path.clear();
            })
    }
}

impl Drop for TemporaryState {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Clone, Copy)]
enum FactKind {
    Created,
    ReadinessRecorded,
    Published,
}

fn validate_observation(
    observation: &ReleaseObservation,
    expected_id: &str,
    expected_kind: FactKind,
) -> Result<(), ReleaseWorkflowError> {
    let invalid = |reason: &str| ReleaseWorkflowError::Observation {
        observation: expected_id.into(),
        reason: reason.into(),
    };
    if observation.schema != OBSERVATION_SCHEMA {
        return Err(invalid("schema does not match"));
    }
    if observation.id != expected_id {
        return Err(invalid("id does not match"));
    }
    crate::validate_identifier(&observation.id)?;
    crate::validate_identifier(&observation.repository)?;
    crate::validate_identifier(&observation.release)?;
    if !is_lowercase_hex(&observation.tag_commit, 40) {
        return Err(invalid("tagCommit must be a 40-character lowercase hex id"));
    }

    let kind_matches = matches!(
        (&observation.fact, expected_kind),
        (ReleaseFact::Created { .. }, FactKind::Created)
            | (
                ReleaseFact::ReadinessRecorded { .. },
                FactKind::ReadinessRecorded
            )
            | (ReleaseFact::Published { .. }, FactKind::Published)
    );
    if !kind_matches {
        return Err(invalid("fact kind does not match"));
    }
    if matches!(
        observation.fact,
        ReleaseFact::ReadinessRecorded { ready: false, .. }
    ) {
        return Err(invalid("readiness must be true"));
    }
    if let ReleaseFact::ReadinessRecorded { commit, .. } = &observation.fact {
        if !is_lowercase_hex(commit, 40) {
            return Err(invalid("commit must be a 40-character lowercase hex id"));
        }
        if commit != &observation.tag_commit {
            return Err(invalid("readiness commit does not match tagCommit"));
        }
    }

    let coordinate_ms = observation.fact.coordinate_ms();
    if !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&coordinate_ms) {
        return Err(invalid(
            "coordinateMs is outside the JSON safe integer range",
        ));
    }
    if parse_utc_milliseconds(observation.fact.occurred_at()) != Some(coordinate_ms) {
        return Err(invalid("occurredAt does not match coordinateMs"));
    }
    Ok(())
}

fn validate_identity(
    left: &ReleaseObservation,
    right: &ReleaseObservation,
) -> Result<(), ReleaseWorkflowError> {
    if left.repository != right.repository
        || left.release != right.release
        || left.tag_commit != right.tag_commit
    {
        return Err(ReleaseWorkflowError::IdentityMismatch);
    }
    Ok(())
}

fn expected_contract(
    repository: &str,
    release: &str,
) -> Result<TemporalContract, ReleaseWorkflowError> {
    crate::validate_identifier(repository)?;
    crate::validate_identifier(release)?;
    let subject_id = format!("{repository}/{release}");
    crate::validate_identifier(&subject_id)?;
    let id = format!("covenant.release.{release}.temporal.v1");
    crate::validate_identifier(&id)?;

    Ok(TemporalContract {
        schema: TEMPORAL_CONTRACT_SCHEMA.into(),
        id,
        subject: TemporalSubject {
            kind: "release".into(),
            id: subject_id,
        },
        axes: vec![TemporalAxis {
            id: AXIS_ID.into(),
            kind: "metric".into(),
            unit: "millisecond".into(),
            origin: "unix.epoch".into(),
        }],
        contexts: vec![TemporalContext {
            id: CONTEXT_ID.into(),
            mode: "actual".into(),
        }],
    })
}

fn point_event(sequence: u64, id: &str, point_id: &str) -> TemporalEvent {
    TemporalEvent::PointDeclared {
        schema: TEMPORAL_EVENT_SCHEMA.into(),
        id: id.into(),
        sequence,
        point: TemporalPoint {
            id: point_id.into(),
            context_id: CONTEXT_ID.into(),
            axis_id: AXIS_ID.into(),
        },
    }
}

fn coordinate_event(
    sequence: u64,
    id: &str,
    assertion_id: &str,
    point_id: &str,
    coordinate_ms: i64,
    evidence_ref: String,
    supersedes: Option<Vec<String>>,
) -> TemporalEvent {
    TemporalEvent::CoordinateAsserted {
        schema: TEMPORAL_EVENT_SCHEMA.into(),
        id: id.into(),
        sequence,
        assertion: CoordinateAssertion {
            id: assertion_id.into(),
            context_id: CONTEXT_ID.into(),
            point_id: point_id.into(),
            coordinate: TemporalCoordinate {
                minimum: coordinate_ms,
                maximum: coordinate_ms,
            },
            evidence_refs: vec![evidence_ref],
            supersedes,
        },
    }
}

fn validate_initial_state(
    run: &ReleaseTimelineRun,
    expected_contract: &TemporalContract,
) -> Result<(), ReleaseWorkflowError> {
    if run.schema != TEMPORAL_RUN_SCHEMA {
        return Err(ReleaseWorkflowError::State(
            "run schema does not match v0alpha3".into(),
        ));
    }
    if &run.contract != expected_contract {
        return Err(ReleaseWorkflowError::State(
            "contract does not match the published release".into(),
        ));
    }
    if run.events.len() < 4 {
        return Err(ReleaseWorkflowError::State(format!(
            "expected at least 4 events, found {}",
            run.events.len()
        )));
    }

    if run.events[0] != point_event(0, "event.release-publication-point", "artifacts-published")
        || run.events[1]
            != point_event(
                1,
                "event.tagged-commit-readiness-point",
                "tagged-commit-readiness-recorded",
            )
    {
        return Err(ReleaseWorkflowError::State(
            "point declarations do not match the release workflow".into(),
        ));
    }
    validate_initial_coordinate(
        &run.events[2],
        2,
        "event.release-publication-provisional",
        PROVISIONAL_ASSERTION_ID,
        "artifacts-published",
    )?;
    validate_initial_coordinate(
        &run.events[3],
        3,
        "event.tagged-commit-readiness",
        "release.tagged-commit-readiness.v1",
        "tagged-commit-readiness-recorded",
    )
}

fn validate_initial_coordinate(
    event: &TemporalEvent,
    expected_sequence: u64,
    expected_event_id: &str,
    expected_assertion_id: &str,
    expected_point_id: &str,
) -> Result<(), ReleaseWorkflowError> {
    let TemporalEvent::CoordinateAsserted {
        schema,
        id,
        sequence,
        assertion,
    } = event
    else {
        return Err(ReleaseWorkflowError::State(format!(
            "event {expected_sequence} is not a coordinate assertion"
        )));
    };

    if schema != TEMPORAL_EVENT_SCHEMA
        || id != expected_event_id
        || *sequence != expected_sequence
        || assertion.id != expected_assertion_id
        || assertion.context_id != CONTEXT_ID
        || assertion.point_id != expected_point_id
        || assertion.coordinate.minimum != assertion.coordinate.maximum
        || assertion.supersedes.is_some()
        || assertion.evidence_refs.len() != 1
        || !is_digest(&assertion.evidence_refs[0])
    {
        return Err(ReleaseWorkflowError::State(format!(
            "event {expected_sequence} does not match the release workflow"
        )));
    }
    Ok(())
}

fn validate_reconciliation(
    run: &ReleaseTimelineRun,
    published: &ReleaseObservation,
) -> Result<(), ReleaseWorkflowError> {
    if run.events.len() != 6 {
        return Err(ReleaseWorkflowError::State(format!(
            "expected 6 reconciled events, found {}",
            run.events.len()
        )));
    }
    let digest = observation_digest(published)?;
    let authoritative = coordinate_event(
        4,
        "event.release-publication-authoritative",
        "release.publication.github.v1",
        "artifacts-published",
        published.fact.coordinate_ms(),
        digest.clone(),
        None,
    );
    let retraction = TemporalEvent::AssertionRetracted {
        schema: TEMPORAL_EVENT_SCHEMA.into(),
        id: "event.release-publication-reconciled".into(),
        sequence: 5,
        assertion_id: PROVISIONAL_ASSERTION_ID.into(),
        evidence_refs: vec![digest],
    };
    if run.events[4] != authoritative || run.events[5] != retraction {
        return Err(ReleaseWorkflowError::State(
            "reconciliation events do not match the publication evidence".into(),
        ));
    }
    Ok(())
}

fn is_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| is_lowercase_hex(hex, 64))
}

fn is_lowercase_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn parse_utc_milliseconds(value: &str) -> Option<i64> {
    let (date_time, fraction_ms) = if let Some(prefix) = value.strip_suffix('Z') {
        match prefix.split_once('.') {
            Some((date_time, fraction)) => {
                if fraction.len() != 3 || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
                    return None;
                }
                (date_time, fraction.parse::<i64>().ok()?)
            }
            None => (prefix, 0),
        }
    } else {
        return None;
    };
    let bytes = date_time.as_bytes();
    if bytes.len() != 19
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }

    let year = parse_decimal(&bytes[0..4])?;
    let month = parse_decimal(&bytes[5..7])?;
    let day = parse_decimal(&bytes[8..10])?;
    let hour = parse_decimal(&bytes[11..13])?;
    let minute = parse_decimal(&bytes[14..16])?;
    let second = parse_decimal(&bytes[17..19])?;
    if !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }

    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days_since_epoch = era * 146_097 + day_of_era - 719_468;
    days_since_epoch
        .checked_mul(86_400_000)?
        .checked_add(hour * 3_600_000 + minute * 60_000 + second * 1_000 + fraction_ms)
}

fn parse_decimal(bytes: &[u8]) -> Option<i64> {
    bytes.iter().try_fold(0_i64, |value, byte| {
        byte.is_ascii_digit()
            .then(|| value * 10 + i64::from(byte - b'0'))
    })
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 400 == 0 || (year % 4 == 0 && year % 100 != 0) => 29,
        2 => 28,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixture_timestamp_forms() {
        assert_eq!(
            parse_utc_milliseconds("2026-05-28T08:33:12Z"),
            Some(1_779_957_192_000)
        );
        assert_eq!(
            parse_utc_milliseconds("2026-05-28T08:41:45.698Z"),
            Some(1_779_957_705_698)
        );
    }

    #[test]
    fn rejects_invalid_calendar_dates() {
        assert_eq!(parse_utc_milliseconds("2026-02-29T00:00:00Z"), None);
        assert_eq!(parse_utc_milliseconds("2026-05-28T24:00:00Z"), None);
        assert_eq!(parse_utc_milliseconds("2026-05-28T08:é:12Z"), None);
    }
}
