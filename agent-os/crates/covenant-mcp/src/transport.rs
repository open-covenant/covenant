//! JSON-RPC 2.0 transport for talking to MCP servers.
//!
//! [`McpClient`] is the trait the rest of the crate codes against; impls
//! exist for stdio-based subprocess servers ([`StdioMcpClient`]) and for
//! tests ([`MockMcpClient`]). Wire format follows the MCP spec: one JSON
//! message per line on the subprocess's stdin/stdout streams.
//!
//! Lifecycle: spawn → `initialize` request → `notifications/initialized`
//! notification → request/response loop → drop kills the child via
//! `kill_on_drop(true)`. The reader task ends when stdout EOFs and at that
//! point any in-flight requests resolve with [`McpClientError::Closed`].

use async_trait::async_trait;
use serde::de::{Error as DeError, Unexpected, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
// parking_lot::Mutex for the sync-side `pending` map. The std::sync
// version's PoisonError path was never reached (the only locker is the
// reader task or the request handler), but the .expect("pending lock")
// calls were a panic surface that gained nothing. parking_lot's lock()
// returns the guard directly.
use parking_lot::Mutex as StdMutex;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

/// JSON-RPC 2.0 request envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

/// JSON-RPC 2.0 notification (no `id`, no response expected).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: &'static str,
    pub method: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

/// JSON-RPC 2.0 response envelope. Either `result` or `error` is set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    #[serde(default)]
    pub jsonrpc: String,
    #[serde(default, deserialize_with = "deserialize_jsonrpc_id")]
    pub id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

const TRANSPORT_CLOSED_CODE: i64 = -32099;
const TRANSPORT_CLOSED_MESSAGE: &str = "transport closed";

#[derive(Debug, thiserror::Error)]
pub enum McpClientError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("rpc error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("transport closed")]
    Closed,
    #[error("timeout after {0:?}")]
    Timeout(Duration),
    #[error("server crashed before responding")]
    ServerCrashed,
}

impl From<JsonRpcError> for McpClientError {
    fn from(e: JsonRpcError) -> Self {
        if e.code == TRANSPORT_CLOSED_CODE && e.message == TRANSPORT_CLOSED_MESSAGE {
            return McpClientError::Closed;
        }
        McpClientError::Rpc {
            code: e.code,
            message: e.message,
        }
    }
}

fn deserialize_jsonrpc_id<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct IdVisitor;

    impl<'de> Visitor<'de> for IdVisitor {
        type Value = Option<u64>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a JSON-RPC id as a number or numeric string")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            Ok(None)
        }

        fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            Ok(Some(v))
        }

        fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            if v < 0 {
                return Err(DeError::invalid_value(Unexpected::Signed(v), &self));
            }
            Ok(Some(v as u64))
        }

        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            trimmed
                .parse::<u64>()
                .map(Some)
                .map_err(|_| DeError::invalid_value(Unexpected::Str(v), &self))
        }

        fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            self.visit_str(&v)
        }
    }

    deserializer.deserialize_any(IdVisitor)
}

#[async_trait]
pub trait McpClient: Send + Sync {
    /// Send a JSON-RPC request and await its response.
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpClientError>;
    /// Fire a JSON-RPC notification (no response expected).
    async fn notify(&self, method: &str, params: Value) -> Result<(), McpClientError>;
}

// ---------- StdioMcpClient ----------

type Pending = Arc<StdMutex<HashMap<u64, oneshot::Sender<Result<Value, JsonRpcError>>>>>;

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub struct StdioMcpClient {
    stdin: Mutex<ChildStdin>,
    pending: Pending,
    next_id: AtomicU64,
    request_timeout: Duration,
    // Holding `Child` keeps the process alive. `kill_on_drop(true)` on the
    // builder means dropping this struct also reaps the subprocess.
    _child: Mutex<Option<Child>>,
    _reader: JoinHandle<()>,
}

impl StdioMcpClient {
    /// Spawn `command` with `args` and start the JSON-RPC reader loop.
    /// Caller is responsible for invoking `initialize` afterwards.
    pub async fn spawn(command: &str, args: &[String]) -> Result<Arc<Self>, McpClientError> {
        Self::spawn_with_env(command, args, &BTreeMap::new()).await
    }

    /// Spawn `command` with explicit environment overrides for this server.
    /// The child still inherits the daemon environment.
    pub async fn spawn_with_env(
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
    ) -> Result<Arc<Self>, McpClientError> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd.envs(env);
        let mut child = cmd.spawn()?;

        let stdin = child.stdin.take().ok_or(McpClientError::Closed)?;
        let stdout = child.stdout.take().ok_or(McpClientError::Closed)?;
        if let Some(stderr) = child.stderr.take() {
            spawn_stderr_logger(stderr);
        }

        let pending: Pending = Arc::new(StdMutex::new(HashMap::new()));
        let reader_pending = pending.clone();
        let reader = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<JsonRpcResponse>(&line) {
                            Ok(resp) => deliver_response(&reader_pending, resp),
                            Err(e) => warn!(error = %e, line, "mcp: bad json on stdout"),
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        warn!(error = %e, "mcp: stdout read failed");
                        break;
                    }
                }
            }
            // EOF or read error: surface to anything still waiting.
            let mut map = reader_pending.lock();
            for (_, tx) in map.drain() {
                let _ = tx.send(Err(JsonRpcError {
                    code: -32099,
                    message: "transport closed".into(),
                    data: None,
                }));
            }
        });

        Ok(Arc::new(Self {
            stdin: Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            _child: Mutex::new(Some(child)),
            _reader: reader,
        }))
    }

    async fn write_line(&self, msg: &[u8]) -> Result<(), McpClientError> {
        let mut s = self.stdin.lock().await;
        s.write_all(msg).await?;
        s.write_all(b"\n").await?;
        s.flush().await?;
        Ok(())
    }
}

fn deliver_response(pending: &Pending, resp: JsonRpcResponse) {
    let id = match resp.id {
        Some(id) => id,
        None => return, // Response without id can't be matched; drop.
    };
    let tx = {
        let mut map = pending.lock();
        map.remove(&id)
    };
    if let Some(tx) = tx {
        let outcome = match (resp.result, resp.error) {
            (Some(v), _) => Ok(v),
            (None, Some(e)) => Err(e),
            (None, None) => Err(JsonRpcError {
                code: -32603,
                message: "response missing both result and error".into(),
                data: None,
            }),
        };
        let _ = tx.send(outcome);
    } else {
        debug!(id, "mcp: response with unknown id");
    }
}

fn spawn_stderr_logger(stderr: tokio::process::ChildStderr) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if !line.trim().is_empty() {
                debug!(target: "mcp.stderr", "{line}");
            }
        }
    });
}

#[async_trait]
impl McpClient for StdioMcpClient {
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        let msg = serde_json::to_vec(&JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        })?;
        {
            let mut map = self.pending.lock();
            map.insert(id, tx);
        }
        if let Err(e) = self.write_line(&msg).await {
            // Roll back the pending entry so it doesn't sit until reader
            // EOF. Without this the slot leaks for the lifetime of the
            // transport on every write failure.
            let mut map = self.pending.lock();
            map.remove(&id);
            return Err(e);
        }
        let timeout = self.request_timeout;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => Err(e.into()),
            Ok(Err(_)) => Err(McpClientError::Closed),
            Err(_) => {
                let mut map = self.pending.lock();
                map.remove(&id);
                Err(McpClientError::Timeout(timeout))
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), McpClientError> {
        let msg = serde_json::to_vec(&JsonRpcNotification {
            jsonrpc: "2.0",
            method: method.to_string(),
            params,
        })?;
        self.write_line(&msg).await
    }
}

// ---------- MockMcpClient (tests + Phase 4 audit) ----------

type MockHandler = dyn Fn(&str, &Value) -> Result<Value, McpClientError> + Send + Sync;

/// In-process MCP client backed by a closure. Lets tests drive
/// [`McpClient`] without spawning anything. The handler is called for
/// requests; notifications are recorded silently.
pub struct MockMcpClient {
    handler: Arc<MockHandler>,
    notifications: StdMutex<Vec<(String, Value)>>,
}

impl MockMcpClient {
    pub fn new<F>(handler: F) -> Self
    where
        F: Fn(&str, &Value) -> Result<Value, McpClientError> + Send + Sync + 'static,
    {
        Self {
            handler: Arc::new(handler),
            notifications: StdMutex::new(Vec::new()),
        }
    }

    pub fn notifications(&self) -> Vec<(String, Value)> {
        self.notifications.lock().clone()
    }
}

#[async_trait]
impl McpClient for MockMcpClient {
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpClientError> {
        (self.handler)(method, &params)
    }
    async fn notify(&self, method: &str, params: Value) -> Result<(), McpClientError> {
        self.notifications
            .lock()
            .push((method.to_string(), params));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_rpc_request_serialises_with_jsonrpc_2() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id: 7,
            method: "tools/list".into(),
            params: Value::Null,
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
        assert!(s.contains("\"id\":7"));
        assert!(s.contains("\"method\":\"tools/list\""));
        assert!(!s.contains("params"));
    }

    #[test]
    fn json_rpc_response_with_error_parses() {
        let s = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"method not found"}}"#;
        let r: JsonRpcResponse = serde_json::from_str(s).unwrap();
        assert_eq!(r.id, Some(1));
        assert!(r.result.is_none());
        let e = r.error.unwrap();
        assert_eq!(e.code, -32601);
    }

    #[test]
    fn json_rpc_response_with_numeric_string_id_parses() {
        let s = r#"{"jsonrpc":"2.0","id":"7","result":{"ok":true}}"#;
        let r: JsonRpcResponse = serde_json::from_str(s).unwrap();
        assert_eq!(r.id, Some(7));
        assert_eq!(r.result.unwrap()["ok"], true);
    }

    #[test]
    fn json_rpc_response_with_whitespace_string_id_parses() {
        let s = r#"{"jsonrpc":"2.0","id":" 7 ","result":{"ok":true}}"#;
        let r: JsonRpcResponse = serde_json::from_str(s).unwrap();
        assert_eq!(r.id, Some(7));
    }

    #[test]
    fn json_rpc_response_with_non_numeric_string_id_errors() {
        let s = r#"{"jsonrpc":"2.0","id":"nope","result":{"ok":true}}"#;
        assert!(serde_json::from_str::<JsonRpcResponse>(s).is_err());
    }

    #[test]
    fn json_rpc_response_serde_pins_result_error_mutual_exclusivity() {
        // JSON-RPC 2.0 requires result and error be mutually exclusive
        // on the wire. JsonRpcResponse::result and ::error both carry
        // #[serde(default, skip_serializing_if = "Option::is_none")] so
        // an ok response emits only result and an error response emits
        // only error. JsonRpcError::data rides the same skip-empty
        // contract for the optional error-detail field.
        let ok = JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: Some(1),
            result: Some(serde_json::json!({ "ok": true })),
            error: None,
        };
        let wire = serde_json::to_value(&ok).unwrap();
        let obj = wire.as_object().expect("response serializes as a JSON object");
        assert!(
            obj.contains_key("result"),
            "ok response must include result on the wire",
        );
        assert!(
            !obj.contains_key("error"),
            "ok response must omit error on the wire; a dropped skip_serializing_if violates JSON-RPC 2.0 mutual exclusivity and breaks strict MCP clients",
        );

        let err = JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: Some(1),
            result: None,
            error: Some(JsonRpcError {
                code: -32601,
                message: "method not found".into(),
                data: None,
            }),
        };
        let wire = serde_json::to_value(&err).unwrap();
        let obj = wire.as_object().expect("response serializes as a JSON object");
        assert!(
            obj.contains_key("error"),
            "error response must include error on the wire",
        );
        assert!(
            !obj.contains_key("result"),
            "error response must omit result on the wire; a dropped skip_serializing_if violates JSON-RPC 2.0 mutual exclusivity",
        );

        let error_obj = obj
            .get("error")
            .and_then(|v| v.as_object())
            .expect("error field must be a JSON object");
        assert!(
            !error_obj.contains_key("data"),
            "JsonRpcError::data=None must be skipped on the wire; a dropped skip_serializing_if surfaces \"data\":null on every error envelope",
        );

        let round_trip_ok: JsonRpcResponse =
            serde_json::from_value(serde_json::to_value(&ok).unwrap()).unwrap();
        assert_eq!(round_trip_ok.id, Some(1));
        assert!(round_trip_ok.error.is_none());
        assert_eq!(round_trip_ok.result.unwrap()["ok"], true);

        let round_trip_err: JsonRpcResponse =
            serde_json::from_value(serde_json::to_value(&err).unwrap()).unwrap();
        assert_eq!(round_trip_err.id, Some(1));
        assert!(round_trip_err.result.is_none());
        let parsed_err = round_trip_err.error.unwrap();
        assert_eq!(parsed_err.code, -32601);
        assert_eq!(parsed_err.message, "method not found");
        assert!(parsed_err.data.is_none());
    }

    #[test]
    fn transport_closed_error_maps_to_closed() {
        let e = JsonRpcError {
            code: TRANSPORT_CLOSED_CODE,
            message: TRANSPORT_CLOSED_MESSAGE.to_string(),
            data: None,
        };
        let mapped: McpClientError = e.into();
        assert!(matches!(mapped, McpClientError::Closed));
    }

    #[tokio::test]
    async fn mock_client_dispatches_request_to_handler() {
        let c = MockMcpClient::new(|method, params| {
            assert_eq!(method, "echo");
            Ok(params.clone())
        });
        let r = c
            .request("echo", serde_json::json!({ "x": 1 }))
            .await
            .unwrap();
        assert_eq!(r["x"], 1);
    }

    #[tokio::test]
    async fn mock_client_records_notifications() {
        let c = MockMcpClient::new(|_, _| Ok(Value::Null));
        c.notify("notifications/initialized", Value::Null)
            .await
            .unwrap();
        let n = c.notifications();
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].0, "notifications/initialized");
    }
}
