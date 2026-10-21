//! Bounded outbound SSH ciphertext relay. Durable session tombstones forbid
//! restart/reconnect adoption; credentials and ciphertext exist only in RAM.
use crate::{hardware::HardwareService, jobs::JobPhase, state::CoordinatorState};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, OriginalUri, Path as RoutePath, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use covenant_compute_attestation::binding;
use covenant_compute_protocol::{
    phala::{parse_phala_ephemeral_lease_spec, PhalaRelayConfig},
    relay::*,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use uuid::Uuid;

const MAX_ACTIVE: usize = 8;
const MAX_EXCHANGES_PER_ROLE: usize = 32_768;
const MAX_TOMBSTONES: usize = 16_384;
const MAX_RESULT: usize = 512 * 1024;
type Result<T> = std::result::Result<T, RelayError>;

#[derive(Debug, thiserror::Error)]
#[error("{code}")]
pub struct RelayError {
    status: StatusCode,
    code: &'static str,
}
fn error(code: &'static str, status: StatusCode) -> RelayError {
    RelayError { code, status }
}
fn reject(code: &'static str) -> RelayError {
    error(code, StatusCode::CONFLICT)
}
fn bad(code: &'static str) -> RelayError {
    error(code, StatusCode::BAD_REQUEST)
}
fn unavailable() -> RelayError {
    error("relay_unavailable", StatusCode::SERVICE_UNAVAILABLE)
}
impl IntoResponse for RelayError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({"error": self.code}))).into_response()
    }
}
fn exact(value: &Value, names: &[&str]) -> Result<()> {
    let map = value.as_object().ok_or_else(|| bad("unexpected_fields"))?;
    if map.len() != names.len() || names.iter().any(|name| !map.contains_key(*name)) {
        return Err(bad("unexpected_fields"));
    }
    Ok(())
}
fn string(value: &Value) -> Result<&str> {
    value.as_str().ok_or_else(|| bad("invalid_string"))
}
fn hex(value: &Value, length: usize) -> Result<String> {
    let s = string(value)?;
    if s.len() != length
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(bad("invalid_hex"));
    }
    Ok(s.into())
}
fn uuid(value: &Value) -> Result<Uuid> {
    let s = string(value)?;
    let id = Uuid::parse_str(s).map_err(|_| bad("invalid_uuid"))?;
    if id.is_nil() || id.to_string() != s {
        return Err(bad("invalid_uuid"));
    }
    Ok(id)
}
fn integer(value: &Value) -> Result<usize> {
    let n = value
        .as_u64()
        .filter(|n| *n < (1u64 << 53))
        .ok_or_else(|| bad("invalid_integer"))?;
    usize::try_from(n).map_err(|_| bad("invalid_integer"))
}
fn fingerprint(value: &Value) -> Result<[u8; 32]> {
    Ok(Sha256::digest(serde_json::to_vec(value).map_err(|_| bad("invalid_json"))?).into())
}
fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u8,
    endpoint: String,
    watermark_ms: u64,
    rows: BTreeMap<Uuid, RelayMetadata>,
}
struct Book {
    path: PathBuf,
    _lock: File,
    data: Snapshot,
    healthy: bool,
    sessions: BTreeMap<Uuid, Session>,
}
impl Book {
    fn open(path: &Path, endpoint: &str) -> Result<Self> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(unavailable)?;
        std::fs::create_dir_all(parent).map_err(|_| unavailable())?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(path.with_extension("relay.lock"))
            .map_err(|_| unavailable())?;
        lock.try_lock().map_err(|_| unavailable())?;
        let data = match std::fs::read(path) {
            Ok(raw) if raw.len() <= 16 * 1024 * 1024 => {
                serde_json::from_slice::<Snapshot>(&raw).map_err(|_| unavailable())?
            }
            Ok(_) => return Err(unavailable()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Snapshot {
                version: 1,
                endpoint: endpoint.into(),
                watermark_ms: 0,
                rows: BTreeMap::new(),
            },
            Err(_) => return Err(unavailable()),
        };
        if data.version != 1 || data.endpoint != endpoint || data.rows.len() > MAX_TOMBSTONES {
            return Err(unavailable());
        }
        for (id, row) in &data.rows {
            if row.schema_version != 1
                || row.job_id != *id
                || id.is_nil()
                || row.relay_endpoint != endpoint
                || row.expires_at_ms == 0
                || hex(&json!(row.app_id), 40).is_err()
                || row
                    .instance_id
                    .as_ref()
                    .is_some_and(|s| hex(&json!(s), 40).is_err())
            {
                return Err(unavailable());
            }
        }
        // Deliberately restore no sessions. Every prior ID is a permanent
        // tombstone; node recovery remains responsible for provider deletion.
        Ok(Self {
            path: path.into(),
            _lock: lock,
            data,
            healthy: true,
            sessions: BTreeMap::new(),
        })
    }
    fn advance(&mut self, now: u64) -> Result<()> {
        if !self.healthy || now < self.data.watermark_ms {
            return Err(unavailable());
        }
        self.data.watermark_ms = now;
        Ok(())
    }
    fn save(&mut self) -> Result<()> {
        if !self.healthy {
            return Err(unavailable());
        }
        let saved = (|| -> std::io::Result<()> {
            let parent = self.path.parent().unwrap();
            let mut temp = tempfile::NamedTempFile::new_in(parent)?;
            temp.write_all(&serde_json::to_vec(&self.data)?)?;
            temp.as_file().sync_all()?;
            temp.persist(&self.path).map_err(|e| e.error)?;
            File::open(parent)?.sync_all()
        })();
        if saved.is_err() {
            self.healthy = false;
            return Err(unavailable());
        }
        Ok(())
    }
}

struct Operation {
    fingerprint: [u8; 32],
    request: Value,
    result: Option<Value>,
}
#[derive(Default)]
struct Pipe {
    start: usize,
    end: usize,
    buffer: VecDeque<u8>,
    eof: bool,
}
struct Stream {
    id: Uuid,
    buyer: Pipe,
    guest: Pipe,
    closed: bool,
    bytes: usize,
    deadline: Instant,
    buyer_history: VecDeque<(Uuid, [u8; 32], Value)>,
    guest_history: VecDeque<(Uuid, [u8; 32], Value)>,
    buyer_spent: HashSet<Uuid>,
    guest_spent: HashSet<Uuid>,
}
struct Session {
    registration: Option<Value>,
    guest_token: Option<String>,
    pinned: Option<String>,
    rejected: bool,
    operations: Vec<(Uuid, Operation)>,
    stream: Option<Stream>,
    stream_terminal: bool,
    deadline: Instant,
    expires_at_ms: u64,
    last_ms: u64,
}
impl Session {
    fn new(now: u64, expiry: u64) -> Self {
        Self {
            registration: None,
            guest_token: None,
            pinned: None,
            rejected: false,
            operations: Vec::new(),
            stream: None,
            stream_terminal: false,
            deadline: Instant::now() + Duration::from_millis(expiry - now),
            expires_at_ms: expiry,
            last_ms: now,
        }
    }
    fn live(&mut self, now: u64) -> Result<()> {
        if self.rejected
            || now < self.last_ms
            || now >= self.expires_at_ms
            || Instant::now() >= self.deadline
        {
            self.rejected = true;
            if let Some(stream) = &mut self.stream {
                stream.closed = true;
            }
            return Err(error("session_closed", StatusCode::GONE));
        }
        self.last_ms = now;
        Ok(())
    }
    fn guest_auth(&self, token: Option<&str>) -> Result<()> {
        if let (Some(supplied), Some(expected)) = (token, self.guest_token.as_deref()) {
            if supplied.len() == 64 && bool::from(supplied.as_bytes().ct_eq(expected.as_bytes())) {
                return Ok(());
            }
        }
        Err(error("unauthorized", StatusCode::UNAUTHORIZED))
    }
    fn dispatch(
        &mut self,
        operation: &str,
        args: &Value,
        token: Option<&str>,
        now: u64,
    ) -> Result<Value> {
        self.live(now)?;
        if operation == "guest_register" {
            exact(args, &["registration_id", "instance_id"])?;
            uuid(&args["registration_id"])?;
            let instance = hex(&args["instance_id"], 40)?;
            if self.pinned.as_ref().is_some_and(|s| s != &instance) {
                return Err(error("wrong_instance", StatusCode::FORBIDDEN));
            }
            if self.registration.is_none() {
                self.registration = Some(args.clone());
                let bytes = [
                    Uuid::new_v4().as_bytes().as_slice(),
                    Uuid::new_v4().as_bytes().as_slice(),
                ]
                .concat();
                self.guest_token = Some(
                    Sha256::digest(bytes)
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect(),
                );
            }
            if self.registration.as_ref() != Some(args) {
                return Err(reject("guest_replacement_forbidden"));
            }
            return Ok(
                json!({"guest_token": self.guest_token, "expires_at_ms": self.expires_at_ms, "instance_pinned": self.pinned.is_some()}),
            );
        }
        if operation.starts_with("guest_") {
            self.guest_auth(token)?;
        }
        match operation {
            "buyer_submit" => {
                exact(args, &["request_id", "kind", "payload"])?;
                let id = uuid(&args["request_id"])?;
                let kind = string(&args["kind"])?;
                if !matches!(kind, "identity" | "quote" | "open_stream") {
                    return Err(bad("unknown_control"));
                }
                if serde_json::to_vec(&args["payload"])
                    .map_err(|_| bad("invalid_json"))?
                    .len()
                    > 16 * 1024
                {
                    return Err(error("control_too_large", StatusCode::PAYLOAD_TOO_LARGE));
                }
                match kind {
                    "identity" => exact(&args["payload"], &[])?,
                    "quote" => {
                        if uuid(&args["payload"]["challenge_id"])? != id {
                            return Err(bad("quote_request_id_differs"));
                        }
                    }
                    _ => {
                        exact(&args["payload"], &["stream_id", "challenge_id"])?;
                        uuid(&args["payload"]["stream_id"])?;
                        uuid(&args["payload"]["challenge_id"])?;
                    }
                }
                let digest = fingerprint(args)?;
                if let Some((_, prior)) = self.operations.iter().find(|(key, _)| *key == id) {
                    if prior.fingerprint != digest {
                        return Err(reject("operation_id_collision"));
                    }
                } else {
                    if self.operations.len() >= MAX_RELAY_OPERATIONS {
                        return Err(error(
                            "operation_budget_exhausted",
                            StatusCode::TOO_MANY_REQUESTS,
                        ));
                    }
                    if kind == "open_stream" {
                        if self.stream_terminal {
                            return Err(error("stream_closed", StatusCode::GONE));
                        }
                        if self.pinned.is_none()
                            || self
                                .registration
                                .as_ref()
                                .and_then(|v| v["instance_id"].as_str())
                                != self.pinned.as_deref()
                        {
                            return Err(error(
                                "verified_instance_pin_required",
                                StatusCode::FORBIDDEN,
                            ));
                        }
                        if self.stream.is_some()
                            || self
                                .operations
                                .iter()
                                .any(|(_, op)| op.request["kind"] == "open_stream")
                        {
                            return Err(reject("only_one_stream_allowed"));
                        }
                    }
                    if kind == "quote"
                        && self
                            .operations
                            .iter()
                            .filter(|(_, op)| op.request["kind"] == "quote")
                            .count()
                            >= 8
                    {
                        return Err(error(
                            "quote_budget_exhausted",
                            StatusCode::TOO_MANY_REQUESTS,
                        ));
                    }
                    self.operations.push((
                        id,
                        Operation {
                            fingerprint: digest,
                            request: args.clone(),
                            result: None,
                        },
                    ));
                }
                Ok(json!({"request_id": id}))
            }
            "buyer_result" => {
                exact(args, &["request_id"])?;
                let id = uuid(&args["request_id"])?;
                let op = self
                    .operations
                    .iter()
                    .find(|(key, _)| *key == id)
                    .ok_or_else(|| error("unknown_request", StatusCode::NOT_FOUND))?;
                Ok(json!({"request_id": id, "result": op.1.result}))
            }
            "guest_poll" => {
                exact(args, &[])?;
                Ok(
                    json!({"operation": self.operations.iter().find(|(_, op)| op.result.is_none()).map(|(_, op)| &op.request)}),
                )
            }
            "guest_reply" => {
                exact(args, &["request_id", "result"])?;
                let id = uuid(&args["request_id"])?;
                let result = &args["result"];
                let ok = result["ok"]
                    .as_bool()
                    .ok_or_else(|| bad("invalid_result"))?;
                exact(
                    result,
                    if ok {
                        &["ok", "value"]
                    } else {
                        &["ok", "error"]
                    },
                )?;
                if !ok && (!result["error"].is_string() || string(&result["error"])?.len() > 80) {
                    return Err(bad("invalid_result"));
                }
                if serde_json::to_vec(result)
                    .map_err(|_| bad("invalid_json"))?
                    .len()
                    > MAX_RESULT
                {
                    return Err(error("result_too_large", StatusCode::PAYLOAD_TOO_LARGE));
                }
                let (_, op) = self
                    .operations
                    .iter_mut()
                    .find(|(key, _)| *key == id)
                    .ok_or_else(|| error("unknown_request", StatusCode::NOT_FOUND))?;
                if let Some(previous) = &op.result {
                    if previous != result {
                        return Err(reject("result_collision"));
                    }
                } else {
                    if op.request["kind"] == "open_stream" && ok {
                        if self.stream_terminal {
                            return Err(error("stream_closed", StatusCode::GONE));
                        }
                        let stream_id = uuid(&op.request["payload"]["stream_id"])?;
                        if self.stream.is_some() {
                            return Err(reject("only_one_stream_allowed"));
                        }
                        if result["value"] != json!({"stream_id": stream_id}) {
                            return Err(reject("wrong_stream_reply"));
                        }
                        self.stream = Some(Stream {
                            id: stream_id,
                            buyer: Pipe::default(),
                            guest: Pipe::default(),
                            closed: false,
                            bytes: 0,
                            deadline: self.deadline.min(
                                Instant::now() + Duration::from_secs(MAX_RELAY_STREAM_SECONDS),
                            ),
                            buyer_history: VecDeque::new(),
                            guest_history: VecDeque::new(),
                            buyer_spent: HashSet::new(),
                            guest_spent: HashSet::new(),
                        });
                    }
                    op.result = Some(result.clone());
                }
                Ok(json!({"request_id": id, "stored": true}))
            }
            "buyer_exchange" | "guest_exchange" => {
                self.exchange(operation == "buyer_exchange", args)
            }
            "buyer_close_stream" => {
                exact(args, &[])?;
                self.stream_terminal = true;
                if let Some(stream) = &mut self.stream {
                    stream.closed = true;
                }
                Ok(json!({"closed": true}))
            }
            _ => Err(error("unknown_operation", StatusCode::NOT_FOUND)),
        }
    }
    fn exchange(&mut self, buyer: bool, args: &Value) -> Result<Value> {
        exact(
            args,
            &[
                "stream_id",
                "operation_id",
                "send_offset",
                "data",
                "receive_ack",
                "eof",
            ],
        )?;
        let id = uuid(&args["stream_id"])?;
        let stream = self
            .stream
            .as_mut()
            .filter(|s| s.id == id)
            .ok_or_else(|| error("wrong_stream", StatusCode::NOT_FOUND))?;
        if stream.closed {
            return Err(error("stream_closed", StatusCode::GONE));
        }
        if Instant::now() >= stream.deadline {
            stream.closed = true;
            return Err(error("stream_expired", StatusCode::GONE));
        }
        let operation_id = uuid(&args["operation_id"])?;
        let digest = fingerprint(args)?;
        let (own, peer, history, spent) = if buyer {
            (
                &mut stream.buyer,
                &mut stream.guest,
                &mut stream.buyer_history,
                &mut stream.buyer_spent,
            )
        } else {
            (
                &mut stream.guest,
                &mut stream.buyer,
                &mut stream.guest_history,
                &mut stream.guest_spent,
            )
        };
        if let Some((_, previous, result)) = history.iter().find(|(id, _, _)| *id == operation_id) {
            if previous != &digest {
                return Err(reject("exchange_id_collision"));
            }
            return Ok(result.clone());
        }
        if spent.contains(&operation_id) {
            return Err(reject("exchange_retry_expired"));
        }
        if spent.len() >= MAX_EXCHANGES_PER_ROLE {
            stream.closed = true;
            return Err(error(
                "exchange_budget_exhausted",
                StatusCode::TOO_MANY_REQUESTS,
            ));
        }
        let data = string(&args["data"])?;
        if data.len() > MAX_RELAY_CHUNK_BYTES.div_ceil(3) * 4 {
            return Err(error("chunk_too_large", StatusCode::PAYLOAD_TOO_LARGE));
        }
        let raw = STANDARD.decode(data).map_err(|_| bad("invalid_base64"))?;
        if raw.len() > MAX_RELAY_CHUNK_BYTES || STANDARD.encode(&raw) != data {
            return Err(bad("invalid_chunk"));
        }
        let eof = args["eof"].as_bool().ok_or_else(|| bad("invalid_eof"))?;
        if integer(&args["send_offset"])? != own.end {
            return Err(reject("wrong_send_offset"));
        }
        let ack = integer(&args["receive_ack"])?;
        if ack < peer.start || ack > peer.end {
            return Err(reject("wrong_receive_ack"));
        }
        if own.eof && !raw.is_empty() {
            return Err(reject("data_after_eof"));
        }
        let accepted = raw.len().min(MAX_RELAY_BUFFER_BYTES - own.buffer.len());
        if stream.bytes + accepted > MAX_RELAY_STREAM_BYTES {
            stream.closed = true;
            return Err(error(
                "stream_byte_budget_exhausted",
                StatusCode::PAYLOAD_TOO_LARGE,
            ));
        }
        peer.buffer.drain(..ack - peer.start);
        peer.start = ack;
        own.buffer.extend(&raw[..accepted]);
        own.end += accepted;
        stream.bytes += accepted;
        if eof && accepted == raw.len() {
            own.eof = true;
        }
        let received: Vec<u8> = peer
            .buffer
            .iter()
            .take(MAX_RELAY_CHUNK_BYTES)
            .copied()
            .collect();
        let result = json!({"send_ack": own.end, "send_eof": own.eof, "receive_offset": peer.start, "data": STANDARD.encode(received), "eof": peer.eof && peer.buffer.len() <= MAX_RELAY_CHUNK_BYTES});
        spent.insert(operation_id);
        history.push_back((operation_id, digest, result.clone()));
        while history.len() > 4 {
            history.pop_front();
        }
        Ok(result)
    }
}

#[derive(Clone)]
pub struct RelayService {
    state: Arc<CoordinatorState>,
    hardware: Arc<HardwareService>,
    book: Arc<Mutex<Book>>,
    endpoint: String,
}
struct Lease {
    buyer: String,
    operator: String,
    expires_at_ms: u64,
}
impl RelayService {
    pub fn open(
        state: Arc<CoordinatorState>,
        hardware: Arc<HardwareService>,
        path: &Path,
        endpoint: String,
    ) -> Result<Self> {
        PhalaRelayConfig {
            url: endpoint.clone(),
            session_id: Uuid::new_v4().to_string(),
        }
        .validate()
        .map_err(|_| bad("invalid_relay_endpoint"))?;
        Ok(Self {
            state,
            hardware,
            book: Arc::new(Mutex::new(Book::open(path, &endpoint)?)),
            endpoint,
        })
    }
    fn lease(&self, job: Uuid, now: u64) -> Result<Lease> {
        let record = self
            .state
            .jobs()
            .get(job)
            .ok_or_else(|| error("unknown_session", StatusCode::NOT_FOUND))?;
        if record.phase != JobPhase::Accepted || record.close_requested_at_ms.is_some() {
            return Err(error("lease_not_live", StatusCode::GONE));
        }
        let spec = parse_phala_ephemeral_lease_spec(&record.envelope.payload.input)
            .map_err(|_| reject("invalid_lease_profile"))?
            .ok_or_else(|| reject("ephemeral_profile_required"))?;
        spec.validate_for_job(&record.envelope.payload)
            .map_err(|_| reject("invalid_lease_profile"))?;
        if spec.operator_pubkey_b58 != record.operator_pubkey_b58
            || spec.relay.url != self.endpoint
            || spec.relay.session_id != job.to_string()
        {
            return Err(reject("relay_scope_changed"));
        }
        let expiry = spec.lease_expires_at_ms.min(
            record
                .envelope
                .payload
                .issued_at_ms
                .saturating_add(record.envelope.payload.deadline_ms),
        );
        if now < record.envelope.payload.issued_at_ms
            || record.accepted_at_ms.is_none_or(|at| now < at)
            || now >= expiry
        {
            return Err(error("lease_expired", StatusCode::GONE));
        }
        Ok(Lease {
            buyer: record.envelope.payload.buyer.pubkey_base58(),
            operator: record.operator_pubkey_b58,
            expires_at_ms: expiry,
        })
    }
    fn authorize(lease: &Lease, actor: &str) -> Result<()> {
        if actor != lease.buyer && actor != lease.operator {
            return Err(error("unauthorized", StatusCode::FORBIDDEN));
        }
        Ok(())
    }
    /// Public immutable claims only; unlike GET, this remains available for
    /// teardown/recovery diagnostics after close and process restart.
    pub fn binding(&self, job: Uuid) -> Option<RelayMetadata> {
        self.book.lock().data.rows.get(&job).cloned()
    }
    pub fn metadata(&self, job: Uuid, actor: &str, now: u64) -> Result<RelayMetadata> {
        let lease = self.lease(job, now)?;
        Self::authorize(&lease, actor)?;
        let mut book = self.book.lock();
        let now = epoch_ms();
        book.advance(now)?;
        self.lease(job, now)?;
        book.sessions
            .get_mut(&job)
            .ok_or_else(|| error("session_not_active", StatusCode::GONE))?
            .live(now)?;
        book.data.rows.get(&job).cloned().ok_or_else(unavailable)
    }
    fn dispatch(
        &self,
        request: &RelayRequest,
        actor: Option<&str>,
        token: Option<&str>,
        now: u64,
    ) -> Result<Value> {
        if request.version != RELAY_VERSION || request.session_id.is_nil() {
            return Err(bad("wrong_protocol"));
        }
        let job = request.session_id;
        let lease = match self.lease(job, now) {
            Ok(lease) => lease,
            Err(e) => {
                if let Some(session) = self.book.lock().sessions.get_mut(&job) {
                    session.rejected = true;
                }
                return Err(e);
            }
        };
        let operation = request.operation.as_str();
        let args = &request.arguments;
        if !operation.starts_with("guest_") {
            Self::authorize(
                &lease,
                actor.ok_or_else(|| error("unauthorized", StatusCode::UNAUTHORIZED))?,
            )?;
        }
        if operation == "operator_open" {
            if actor != Some(lease.operator.as_str()) {
                return Err(error("assigned_operator_required", StatusCode::FORBIDDEN));
            }
            exact(args, &["app_id"])?;
            let app = hex(&args["app_id"], 40)?;
            let mut book = self.book.lock();
            let now = epoch_ms();
            book.advance(now)?;
            self.lease(job, now)?;
            if let Some(row) = book.data.rows.get(&job) {
                if row.app_id != app {
                    return Err(reject("app_replacement_forbidden"));
                }
                book.sessions
                    .get_mut(&job)
                    .ok_or_else(|| error("session_restart_forbidden", StatusCode::GONE))?
                    .live(now)?;
                return Ok(json!({"app_id": app}));
            }
            book.sessions
                .retain(|id, session| self.lease(*id, now).is_ok() && session.live(now).is_ok());
            if book.sessions.len() >= MAX_ACTIVE || book.data.rows.len() >= MAX_TOMBSTONES {
                return Err(error(
                    "relay_capacity_exhausted",
                    StatusCode::TOO_MANY_REQUESTS,
                ));
            }
            book.data.rows.insert(
                job,
                RelayMetadata {
                    schema_version: 1,
                    job_id: job,
                    app_id: app.clone(),
                    instance_id: None,
                    relay_endpoint: self.endpoint.clone(),
                    expires_at_ms: lease.expires_at_ms,
                },
            );
            book.save()?;
            book.sessions
                .insert(job, Session::new(now, lease.expires_at_ms));
            return Ok(json!({"app_id": app}));
        }
        if operation == "buyer_submit" && args["kind"] == "open_stream" {
            if actor != Some(lease.buyer.as_str()) {
                return Err(error("buyer_required", StatusCode::FORBIDDEN));
            }
            let challenge = uuid(&args["payload"]["challenge_id"])?;
            let row = self
                .binding(job)
                .ok_or_else(|| error("unknown_session", StatusCode::NOT_FOUND))?;
            let instance = row
                .instance_id
                .as_deref()
                .ok_or_else(|| error("verified_instance_pin_required", StatusCode::FORBIDDEN))?;
            self.hardware
                .require_relay_stream(job, &lease.buyer, challenge, &row.app_id, instance, now)
                .map_err(|_| error("verified_hardware_required", StatusCode::FORBIDDEN))?;
        }
        if matches!(operation, "buyer_exchange" | "buyer_close_stream")
            && actor != Some(lease.buyer.as_str())
        {
            return Err(error("buyer_required", StatusCode::FORBIDDEN));
        }
        // Recheck after potentially blocking hardware verification, and before
        // every byte/registration/control mutation. Never extend the deadline.
        let mut book = self.book.lock();
        let now = epoch_ms();
        book.advance(now)?;
        self.lease(job, now)?;
        if operation == "buyer_pin_instance" {
            exact(args, &["instance_id"])?;
            let instance = hex(&args["instance_id"], 40)?;
            let row = book
                .data
                .rows
                .get(&job)
                .ok_or_else(|| error("unknown_session", StatusCode::NOT_FOUND))?;
            if row.instance_id.as_ref().is_some_and(|id| id != &instance) {
                return Err(reject("instance_replacement_forbidden"));
            }
            let session = book
                .sessions
                .get_mut(&job)
                .ok_or_else(|| error("session_not_active", StatusCode::GONE))?;
            session.live(now)?;
            if session
                .registration
                .as_ref()
                .is_some_and(|v| v["instance_id"] != instance)
            {
                session.rejected = true;
                return Err(error("candidate_instance_mismatch", StatusCode::FORBIDDEN));
            }
            book.data.rows.get_mut(&job).unwrap().instance_id = Some(instance.clone());
            book.save()?;
            book.sessions.get_mut(&job).unwrap().pinned = Some(instance.clone());
            return Ok(json!({"instance_id": instance}));
        }
        book.sessions
            .get_mut(&job)
            .ok_or_else(|| error("session_not_active", StatusCode::GONE))?
            .dispatch(operation, args, token, now)
    }
}

fn authenticate(
    headers: &HeaderMap,
    method: &str,
    path: &str,
    body: &[u8],
    now: u64,
) -> Result<String> {
    let header = |name: &str| -> Result<&str> {
        if headers.get_all(name).iter().count() != 1 {
            return Err(error("unauthorized", StatusCode::UNAUTHORIZED));
        }
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| error("unauthorized", StatusCode::UNAUTHORIZED))
    };
    let actor = header("x-covenant-hardware-actor")?;
    let signed = header("x-covenant-hardware-signed-at")?
        .parse()
        .map_err(|_| bad("invalid_signature_time"))?;
    binding::verify_request(
        actor,
        header("x-covenant-hardware-signature")?,
        method,
        path,
        body,
        signed,
        now,
    )
    .map_err(|_| error("unauthorized", StatusCode::UNAUTHORIZED))?;
    Ok(actor.into())
}

// serde_json::Value accepts duplicate map keys by default. Reject ambiguity at
// every level, including guest arguments, before interpreting the wire request.
struct StrictValue(Value);
impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = StrictValue;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("unambiguous integer JSON")
            }
            fn visit_bool<E: serde::de::Error>(
                self,
                v: bool,
            ) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(json!(v)))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(json!(v)))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(json!(v)))
            }
            fn visit_str<E: serde::de::Error>(
                self,
                v: &str,
            ) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(json!(v)))
            }
            fn visit_string<E: serde::de::Error>(
                self,
                v: String,
            ) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(json!(v)))
            }
            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(StrictValue(v)) = seq.next_element()? {
                    values.push(v);
                }
                Ok(StrictValue(Value::Array(values)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some((key, StrictValue(v))) = map.next_entry::<String, StrictValue>()? {
                    if values.insert(key, v).is_some() {
                        return Err(serde::de::Error::custom("duplicate field"));
                    }
                }
                Ok(StrictValue(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}
fn parse_request(body: &[u8]) -> Result<RelayRequest> {
    let value = serde_json::from_slice::<StrictValue>(body)
        .map_err(|_| bad("invalid_json"))?
        .0;
    uuid(&value["session_id"])?;
    serde_json::from_value(value).map_err(|_| bad("invalid_request"))
}
async fn relay_http(
    State(service): State<Arc<RelayService>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<RelayResponse>> {
    if uri
        .path_and_query()
        .is_none_or(|part| part.as_str() != RELAY_PATH)
    {
        return Err(bad("invalid_request_path"));
    }
    let now = epoch_ms();
    let request = parse_request(&body)?;
    let actor = if request.operation.starts_with("guest_") {
        None
    } else {
        Some(authenticate(&headers, "POST", RELAY_PATH, &body, now)?)
    };
    if headers.get_all("authorization").iter().count() > 1 {
        return Err(bad("invalid_authorization"));
    }
    let token = headers
        .get("authorization")
        .map(|v| {
            v.to_str()
                .ok()
                .and_then(|s| s.strip_prefix("Bearer "))
                .ok_or_else(|| bad("invalid_authorization"))
        })
        .transpose()?
        .map(str::to_owned);
    let pending = matches!(request.operation.as_str(), "guest_poll" | "buyer_result");
    for attempt in 0..5 {
        let service = service.clone();
        let request = request.clone();
        let actor = actor.clone();
        let token = token.clone();
        let result = tokio::task::spawn_blocking(move || {
            service.dispatch(&request, actor.as_deref(), token.as_deref(), epoch_ms())
        })
        .await
        .map_err(|_| unavailable())??;
        if pending
            && attempt < 4
            && (result.get("operation") == Some(&Value::Null)
                || result.get("result") == Some(&Value::Null))
        {
            tokio::time::sleep(Duration::from_millis(250)).await;
            continue;
        }
        return Ok(Json(RelayResponse {
            version: RELAY_VERSION,
            result,
        }));
    }
    Err(unavailable())
}
async fn metadata_http(
    State(service): State<Arc<RelayService>>,
    OriginalUri(uri): OriginalUri,
    RoutePath(job): RoutePath<Uuid>,
    headers: HeaderMap,
) -> Result<Json<RelayMetadata>> {
    if uri
        .path_and_query()
        .is_none_or(|part| part.as_str() != relay_metadata_path(job))
    {
        return Err(bad("invalid_request_path"));
    }
    let now = epoch_ms();
    let actor = authenticate(&headers, "GET", &relay_metadata_path(job), &[], now)?;
    tokio::task::spawn_blocking(move || service.metadata(job, &actor, epoch_ms()))
        .await
        .map_err(|_| unavailable())?
        .map(Json)
}
pub fn router(service: Arc<RelayService>) -> Router {
    let slots = Arc::new(tokio::sync::Semaphore::new(64));
    Router::new()
        .route(RELAY_PATH, post(relay_http))
        .route("/federation/hardware/:job/relay", get(metadata_http))
        .layer(DefaultBodyLimit::max(MAX_RELAY_HTTP_BYTES))
        .layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let slots = slots.clone();
                async move {
                    let Ok(_permit) = slots.try_acquire_owned() else {
                        return error("relay_capacity_exhausted", StatusCode::TOO_MANY_REQUESTS)
                            .into_response();
                    };
                    let mut response = match tokio::time::timeout(
                        Duration::from_secs(15),
                        next.run(request),
                    )
                    .await
                    {
                        Ok(response) => response,
                        Err(_) => error("relay_request_timeout", StatusCode::GATEWAY_TIMEOUT)
                            .into_response(),
                    };
                    response.headers_mut().insert(
                        axum::http::header::CACHE_CONTROL,
                        axum::http::HeaderValue::from_static("no-store"),
                    );
                    response
                }
            },
        ))
        .with_state(service)
}
pub fn build_from_env(
    state: Arc<CoordinatorState>,
    hardware: Option<Arc<HardwareService>>,
    journal_path: &Path,
) -> Result<Option<Arc<RelayService>>> {
    match std::env::var("COVENANT_COMPUTE_EPHEMERAL_RELAY") {
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Ok(value) if value == "0" => return Ok(None),
        Ok(value) if value == "1" => {}
        _ => return Err(bad("invalid_relay_activation")),
    }
    let endpoint = std::env::var("COVENANT_COMPUTE_EPHEMERAL_RELAY_ORIGIN")
        .map_err(|_| bad("relay_origin_required"))?;
    let hardware = hardware.ok_or_else(|| bad("hardware_service_required"))?;
    if std::env::var("COVENANT_COMPUTE_HARDWARE_LEASE_PINS")
        .ok()
        .as_deref()
        != Some("1")
    {
        return Err(bad("signed_lease_pins_required"));
    }
    RelayService::open(
        state,
        hardware,
        &journal_path.with_extension("relay.json"),
        endpoint,
    )
    .map(|service| Some(Arc::new(service)))
}

#[cfg(test)]
#[path = "relay_tests.rs"]
mod tests;
