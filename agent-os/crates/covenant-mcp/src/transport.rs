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
        self.notifications.lock().push((method.to_string(), params));
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
    fn json_rpc_request_serde_pins_three_required_and_params_skip_null() {
        // JsonRpcRequest is the outgoing JSON-RPC 2.0 request envelope
        // every StdioMcpClient::request frames. Four fields: jsonrpc
        // (the string "2.0"), id (u64), method (String), params (Value,
        // #[serde(default, skip_serializing_if = "Value::is_null")]).
        // The existing json_rpc_request_serialises_with_jsonrpc_2 test
        // only checks substring presence; a refactor that flattens
        // params or swaps the skip predicate to a non-equivalent
        // function would silently change every MCP server's request
        // shape and pass the loose substring assertion. Pin the exact
        // key set on both the empty-params (3-key) and populated-params
        // (4-key) paths, plus the id-as-JSON-number contract.
        let empty = JsonRpcRequest {
            jsonrpc: "2.0",
            id: 7,
            method: "tools/list".into(),
            params: Value::Null,
        };
        let empty_wire = serde_json::to_value(&empty).unwrap();
        let empty_obj = empty_wire
            .as_object()
            .expect("request serializes as a JSON object");
        let empty_keys: std::collections::BTreeSet<&str> =
            empty_obj.keys().map(String::as_str).collect();
        let three: std::collections::BTreeSet<&str> =
            ["jsonrpc", "id", "method"].into_iter().collect();
        assert_eq!(
            empty_keys, three,
            "empty-params JsonRpcRequest wire form must be exactly three keys; \
             a refactor that drops the skip_serializing_if = Value::is_null \
             on params would surface params: null and silently change every \
             MCP server's request shape",
        );
        assert_eq!(empty_obj.get("jsonrpc"), Some(&serde_json::json!("2.0")));
        assert_eq!(
            empty_obj.get("method"),
            Some(&serde_json::json!("tools/list"))
        );
        // id must serialise as a JSON number, not a string. A Serialize
        // override that stringified id would silently break every
        // JSON-RPC 2.0 server that destructures id as u64.
        assert_eq!(
            empty_obj.get("id").and_then(serde_json::Value::as_u64),
            Some(7),
            "id must surface as a JSON number, not a string",
        );

        let populated = JsonRpcRequest {
            jsonrpc: "2.0",
            id: 8,
            method: "tools/call".into(),
            params: serde_json::json!({ "name": "echo" }),
        };
        let populated_wire = serde_json::to_value(&populated).unwrap();
        let populated_obj = populated_wire.as_object().unwrap();
        let populated_keys: std::collections::BTreeSet<&str> =
            populated_obj.keys().map(String::as_str).collect();
        let four: std::collections::BTreeSet<&str> =
            ["jsonrpc", "id", "method", "params"].into_iter().collect();
        assert_eq!(
            populated_keys, four,
            "populated JsonRpcRequest wire form must be exactly four keys; \
             a #[serde(flatten)] on params would lift inner params keys to \
             the envelope and break JSON-RPC 2.0 routing",
        );
        assert_eq!(
            populated_obj.get("params"),
            Some(&serde_json::json!({ "name": "echo" })),
            "non-null params must surface verbatim; a different skip \
             predicate or a flatten would silently drop or merge the payload",
        );
    }

    #[test]
    fn json_rpc_notification_serde_pins_no_id_and_skip_empty_params() {
        // JsonRpcNotification differs from JsonRpcRequest by carrying no
        // id field at all — the server treats a notification as
        // fire-and-forget. params rides #[serde(default,
        // skip_serializing_if = "Value::is_null")] so a no-params
        // notification stays compact on the wire. A refactor that adds
        // an id field would silently misroute notifications as requests
        // expecting a response; a refactor that drops or swaps the
        // skip-null predicate would silently inflate every notification
        // frame.
        let empty = JsonRpcNotification {
            jsonrpc: "2.0",
            method: "notifications/initialized".into(),
            params: Value::Null,
        };
        let wire = serde_json::to_value(&empty).unwrap();
        let obj = wire
            .as_object()
            .expect("notification serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["jsonrpc", "method"],
            "no-params notification wire object must contain exactly jsonrpc and method; an unexpected id field or non-skipped params silently changes the wire shape every MCP server consumes",
        );

        let populated = JsonRpcNotification {
            jsonrpc: "2.0",
            method: "notifications/progress".into(),
            params: serde_json::json!({ "step": 1 }),
        };
        let wire = serde_json::to_value(&populated).unwrap();
        assert_eq!(
            wire.get("params"),
            Some(&serde_json::json!({ "step": 1 })),
            "non-null params must surface verbatim; a skip predicate other than Value::is_null silently drops populated payloads",
        );
        assert!(
            wire.as_object().unwrap().keys().all(|k| k != "id"),
            "JsonRpcNotification must never serialize an id field",
        );

        let parsed: JsonRpcNotification =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .expect("notification with no params must decode via #[serde(default)]");
        assert_eq!(parsed.method, "notifications/initialized");
        assert!(parsed.params.is_null());
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
        let obj = wire
            .as_object()
            .expect("response serializes as a JSON object");
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
        let obj = wire
            .as_object()
            .expect("response serializes as a JSON object");
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
    fn json_rpc_response_id_deserializer_pins_missing_null_empty_and_negative() {
        // JsonRpcResponse::id is Option<u64> with #[serde(default,
        // deserialize_with = "deserialize_jsonrpc_id")]. The custom
        // deserializer is what allows MCP servers to send numeric,
        // string-numeric, or whitespace-padded string ids while
        // rejecting non-numeric strings. The existing happy-path tests
        // (json_rpc_response_with_error_parses,
        // json_rpc_response_with_numeric_string_id_parses,
        // json_rpc_response_with_whitespace_string_id_parses) and the
        // single rejection (json_rpc_response_with_non_numeric_string_id_errors)
        // do not pin four meaningful edge cases that ride the custom
        // visitor's None-emitting and negative-rejection branches:
        //
        // * Missing id field decodes to None via #[serde(default)] —
        //   the visit_none path in the visitor is dead for this case
        //   (#[serde(default)] short-circuits before invoking the
        //   custom deserializer), but the contract that a missing id
        //   reads as None is what MCP transport's request/response
        //   correlation depends on for parse-error envelopes the
        //   server emits before it can extract the request's id.
        // * Explicit JSON null id decodes to None via the visitor's
        //   visit_unit branch — defensive parsing for MCP servers
        //   that emit "id": null on parse-error envelopes.
        // * Empty or whitespace-only string id decodes to None via
        //   the visitor's visit_str trimmed-empty branch — same
        //   defensive parsing surface for misbehaving servers.
        // * Negative integer id is rejected via the visitor's
        //   visit_i64 negative-value branch — JSON-RPC 2.0 ids must
        //   be non-negative integers; a relaxation that wraps to a
        //   large unsigned value would silently break in-flight
        //   request/response correlation.
        //
        // A refactor that swaps the custom deserializer for derived
        // Option<u64> would drop branches (3) and (4) without
        // breaking any existing test.
        let missing: JsonRpcResponse =
            serde_json::from_str(r#"{"jsonrpc":"2.0","result":{"ok":true}}"#).expect(
                "JsonRpcResponse with no id field must decode via #[serde(default)] \
                 to id: None; a dropped default would refuse parse-error envelopes \
                 from MCP servers that cannot extract the request id",
            );
        assert_eq!(
            missing.id, None,
            "missing id field must decode to None — JSON-RPC 2.0 parse-error \
             envelopes routinely omit id and the transport layer's \
             request/response correlation routes them as uncorrelated",
        );

        let null: JsonRpcResponse =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":null,"result":{"ok":true}}"#).expect(
                "JsonRpcResponse with explicit \"id\": null must decode via the \
                 visitor's visit_unit branch to id: None; a refactor that swapped \
                 the custom deserializer for a derived Option<u64> still handles \
                 null but a refactor that hardened the visitor to reject visit_unit \
                 would break MCP servers emitting null ids on parse-error envelopes",
            );
        assert_eq!(null.id, None, "explicit JSON null id must decode to None");

        let empty: JsonRpcResponse =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":"","result":{"ok":true}}"#).expect(
                "JsonRpcResponse with \"id\": \"\" must decode via the visitor's \
                 visit_str trimmed-empty branch to id: None; this is the \
                 defensive-parsing surface a derived Option<u64> deserializer \
                 would reject outright",
            );
        assert_eq!(empty.id, None, "empty-string id must decode to None");

        let whitespace: JsonRpcResponse =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":"   ","result":{"ok":true}}"#).expect(
                "JsonRpcResponse with whitespace-only \"id\": \"   \" must decode \
                 via the visitor's visit_str trimmed-empty branch to id: None",
            );
        assert_eq!(
            whitespace.id, None,
            "whitespace-only string id must decode to None after trim",
        );

        assert!(
            serde_json::from_str::<JsonRpcResponse>(
                r#"{"jsonrpc":"2.0","id":-1,"result":{"ok":true}}"#
            )
            .is_err(),
            "JsonRpcResponse with negative integer id must be rejected by the \
             visitor's visit_i64 negative-value branch; JSON-RPC 2.0 ids are \
             non-negative integers and a relaxation that wrapped -1 to \
             0xFFFFFFFFFFFFFFFF would silently break in-flight request/response \
             correlation by mapping the negative id to a wraparound u64 that \
             does not match any positive numeric id the client issued",
        );
    }

    #[test]
    fn json_rpc_error_serde_pins_two_required_and_data_skip_empty() {
        // JsonRpcError is the per-error payload riding inside
        // JsonRpcResponse::error. Three fields: code (i64, the
        // JSON-RPC 2.0 numeric error code), message (String), and
        // data (Option<Value>, #[serde(default, skip_serializing_if =
        // "Option::is_none")]). The mutual-exclusivity test above
        // exercises the data=None arm indirectly via the response
        // envelope; pin the standalone envelope key set on both None
        // and Some paths, and the data=None forward-compat decode.
        // The code-as-signed-JSON-number arm cross-binds to
        // transport_closed_error_maps_to_closed, which routes on
        // code == -32099 as i64.
        let none = JsonRpcError {
            code: -32601,
            message: "method not found".into(),
            data: None,
        };
        let none_wire = serde_json::to_value(&none).unwrap();
        let none_obj = none_wire
            .as_object()
            .expect("JsonRpcError serializes as a JSON object");
        let none_keys: std::collections::BTreeSet<&str> =
            none_obj.keys().map(String::as_str).collect();
        let two: std::collections::BTreeSet<&str> = ["code", "message"].into_iter().collect();
        assert_eq!(
            none_keys, two,
            "data=None JsonRpcError wire form must be exactly two keys; a \
             dropped skip_serializing_if surfaces data: null on every error \
             envelope and splits operator dashboards filtering on field presence",
        );
        // Cross-binding: code must surface as a signed JSON number so
        // the McpClientError::Closed routing on code == -32099 holds.
        assert_eq!(
            none_obj.get("code").and_then(serde_json::Value::as_i64),
            Some(-32601),
            "code must surface as a signed JSON number, not a string",
        );

        let some = JsonRpcError {
            code: -32602,
            message: "invalid params".into(),
            data: Some(serde_json::json!({ "field": "x" })),
        };
        let some_wire = serde_json::to_value(&some).unwrap();
        let some_obj = some_wire.as_object().unwrap();
        let some_keys: std::collections::BTreeSet<&str> =
            some_obj.keys().map(String::as_str).collect();
        let three: std::collections::BTreeSet<&str> =
            ["code", "message", "data"].into_iter().collect();
        assert_eq!(
            some_keys, three,
            "data=Some JsonRpcError wire form must be exactly three keys",
        );
        assert_eq!(
            some_obj.get("data"),
            Some(&serde_json::json!({ "field": "x" })),
        );

        // Each strictly-required field must reject when omitted. data
        // is optional and exercised below.
        for required in ["code", "message"] {
            let mut missing = none_obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<JsonRpcError>(serde_json::Value::Object(missing)).is_err(),
                "JsonRpcError wire form must reject a payload missing {required:?}",
            );
        }

        // #[serde(default)] on data: a payload that omits data must
        // still decode and yield None. A refactor that drops the
        // default would refuse minimal error envelopes from older or
        // strict-shape JSON-RPC 2.0 servers.
        let omitted: JsonRpcError = serde_json::from_value(serde_json::json!({
            "code": -32601,
            "message": "x",
        }))
        .unwrap();
        assert!(omitted.data.is_none());
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

    #[test]
    fn transport_closed_constants_pin_minus_32099_code_and_transport_closed_message() {
        // TRANSPORT_CLOSED_CODE (line 74) and TRANSPORT_CLOSED_MESSAGE
        // (line 75) are the JSON-RPC sentinel the reader's EOF/shutdown
        // path emits when a stdio MCP child process exits. The emit
        // site at line 253-257 in NewStdioClient::spawn uses LITERAL
        // values:
        //
        //     code: -32099,
        //     message: "transport closed".into(),
        //
        // The consume site at line 95 in From<JsonRpcError> for
        // McpClientError uses the named constants in the conjunction
        // `e.code == TRANSPORT_CLOSED_CODE && e.message ==
        // TRANSPORT_CLOSED_MESSAGE → McpClientError::Closed`. The
        // constants and the literal must agree exactly: a drift would
        // silently desync the consume side from the emit side, causing
        // EOF events to route to McpClientError::Rpc instead of
        // McpClientError::Closed and breaking connection-recovery
        // logic.
        //
        // transport_closed_error_maps_to_closed (above) and
        // from_jsonrpc_error_for_mcp_client_error_pins_closed_vs_rpc_boundary_arms
        // (below) both construct JsonRpcError values with
        // TRANSPORT_CLOSED_CODE and TRANSPORT_CLOSED_MESSAGE directly,
        // so neither test catches a refactor that changed the
        // constants while the literal at line 253-257 stayed at -32099
        // / "transport closed" — the test fixtures and the matched
        // constant remain in lockstep through the constant, masking
        // the desync.
        //
        // -32099 sits at the high-magnitude end of JSON-RPC 2.0's
        // implementation-defined "Server error" range (-32000 to
        // -32099), distinct from the spec's reserved standard codes
        // (-32700 parse-error, -32600 invalid-request, -32601
        // method-not-found, -32602 invalid-params, -32603
        // internal-error).
        assert_eq!(
            TRANSPORT_CLOSED_CODE, -32099,
            "TRANSPORT_CLOSED_CODE must remain -32099 — the literal \
             value the reader's EOF path emits at line 253-257. A \
             refactor that consolidated the sentinel into a different \
             value (e.g., -32098 under a 'cleaner sentinel range' \
             rationale, or -32603 under a 'use the JSON-RPC standard \
             internal-error code' rationale) without also updating \
             the emit literal would silently desync the consume side: \
             every EOF JsonRpcError would route to McpClientError::Rpc \
             instead of McpClientError::Closed, and connection-recovery \
             logic gating on Closed would never trigger. A move to a \
             JSON-RPC 2.0 reserved standard code would also collide \
             with the spec's documented meaning for external MCP \
             clients reading the code on the wire",
        );
        assert_eq!(
            TRANSPORT_CLOSED_MESSAGE, "transport closed",
            "TRANSPORT_CLOSED_MESSAGE must remain the literal string \
             'transport closed' the reader emits at line 255. The \
             consume side uses AND (not OR) in the boundary \
             conjunction at line 95, so a rename of the constant (e.g., \
             to 'closed' or 'transport_closed' for snake_case \
             consistency) against an emit-side literal of 'transport \
             closed' would always fail the conjunction and route every \
             transport-close event to Rpc. The exact-string pin \
             anchors the constant to the literal",
        );
    }

    #[test]
    fn from_jsonrpc_error_for_mcp_client_error_pins_closed_vs_rpc_boundary_arms() {
        // covenant_mcp::transport::From<JsonRpcError> for McpClientError
        // (line 93-103) routes a JSON-RPC error envelope into the
        // McpClient error enum. Two arms with one load-bearing
        // conjunction:
        //
        //   (1) e.code == TRANSPORT_CLOSED_CODE (-32099) AND
        //       e.message == TRANSPORT_CLOSED_MESSAGE ("transport closed")
        //       → McpClientError::Closed
        //   (2) otherwise → McpClientError::Rpc { code, message }
        //
        // The AND-not-OR conjunction shapes connection-recovery logic:
        // Closed routes to reconnection while Rpc surfaces as a
        // client-visible error.
        //
        // transport_closed_error_maps_to_closed (line 825) pins arm 1
        // exactly but leaves the conjunction boundary (code matches but
        // message doesn't, or vice versa), the generic Rpc arm, and
        // the field-preservation contract on the Rpc arm all unpinned.
        // A refactor that loosened the conjunction from AND to OR
        // under the rationale 'defensive about partial sentinels'
        // would silently classify any -32099-coded server error as a
        // transport closure and trigger reconnection logic on
        // unrelated errors.

        // (1) Boundary: code matches sentinel but message doesn't →
        // must map to Rpc with code preserved.
        let code_only = JsonRpcError {
            code: TRANSPORT_CLOSED_CODE,
            message: "different message".to_string(),
            data: None,
        };
        let mapped: McpClientError = code_only.into();
        match mapped {
            McpClientError::Rpc { code, message } => {
                assert_eq!(
                    code, TRANSPORT_CLOSED_CODE,
                    "Rpc arm must preserve the original code verbatim — a refactor that wrapped code into a generic combined String during a 'simplify' pass would silently strip the protocol-specific error class, breaking downstream client code that reads code for -32601/-32602/etc. mapping",
                );
                assert_eq!(
                    message, "different message",
                    "Rpc arm must preserve the original message verbatim — the message is the operator-visible diagnostic surface and any rewrite would mask the original error category",
                );
            }
            other => panic!(
                "code-only sentinel match must route to Rpc, not Closed — a refactor that loosened the AND conjunction to OR on the code side would silently route every -32099-coded server error to Closed even when the message is an in-band rpc-specific error string; got: {other:?}",
            ),
        }

        // (2) Boundary: message matches sentinel but code doesn't →
        // must map to Rpc with code preserved.
        let message_only = JsonRpcError {
            code: -32601,
            message: TRANSPORT_CLOSED_MESSAGE.to_string(),
            data: None,
        };
        let mapped: McpClientError = message_only.into();
        match mapped {
            McpClientError::Rpc { code, message } => {
                assert_eq!(
                    code, -32601,
                    "Rpc arm must preserve the original code verbatim on the message-only boundary",
                );
                assert_eq!(message, TRANSPORT_CLOSED_MESSAGE);
            }
            other => panic!(
                "message-only sentinel match must route to Rpc, not Closed — a refactor that loosened the AND conjunction to OR on the message side would silently route every 'transport closed' message string to Closed even when the code is a protocol-specific rpc error code; got: {other:?}",
            ),
        }

        // (3) Generic non-sentinel error → must map to Rpc with both
        // fields preserved verbatim. Pins the Rpc arm's field
        // round-trip contract that downstream client code joins on
        // for protocol-specific error classification.
        let generic = JsonRpcError {
            code: -32601,
            message: "method not found".to_string(),
            data: None,
        };
        let mapped: McpClientError = generic.into();
        match mapped {
            McpClientError::Rpc { code, message } => {
                assert_eq!(code, -32601);
                assert_eq!(message, "method not found");
            }
            other => panic!("generic non-sentinel must route to Rpc; got: {other:?}"),
        }

        // (4) data field must be dropped in both arms — the McpClient
        // error enum carries only code and message; e.data is
        // intentionally absent from both McpClientError::Closed (no
        // fields) and McpClientError::Rpc (only code and message).
        // Pin this on a sentinel-match payload with a non-empty data
        // field so the contract is observable.
        let closed_with_data = JsonRpcError {
            code: TRANSPORT_CLOSED_CODE,
            message: TRANSPORT_CLOSED_MESSAGE.to_string(),
            data: Some(serde_json::json!({"hint": "reconnect"})),
        };
        assert!(
            matches!(closed_with_data.into(), McpClientError::Closed),
            "sentinel-match with a non-empty data field must still route to Closed — data is intentionally absent from the McpClient enum; a refactor that gated the Closed arm on data.is_none() would silently break the contract for servers that emit closure breadcrumbs in data",
        );

        let rpc_with_data = JsonRpcError {
            code: -32602,
            message: "invalid params".to_string(),
            data: Some(serde_json::json!({"field": "x"})),
        };
        let mapped: McpClientError = rpc_with_data.into();
        match mapped {
            McpClientError::Rpc { code, message } => {
                assert_eq!(code, -32602);
                assert_eq!(message, "invalid params");
            }
            other => panic!(
                "non-sentinel error with data must route to Rpc and drop data; got: {other:?}"
            ),
        }
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

    #[test]
    fn mcp_client_error_display_messages_pin_four_string_variant_format_strings() {
        // McpClientError (transport.rs lines 77-91) has six variants.
        // Two wrap external errors via #[from] (Io, Serde); the four
        // string-bearing variants emit operator-facing format strings
        // that no existing test inspects via Display. The
        // transport_closed_constants_pin (line 837) pins the
        // TRANSPORT_CLOSED_MESSAGE const but the McpClientError::Closed
        // #[error] attribute is a SEPARATE thiserror compilation — a
        // drift between the const and the Display attribute would
        // silently desync. The other three string variants (Rpc,
        // Timeout, ServerCrashed) have no existing Display pin.

        let rpc = McpClientError::Rpc {
            code: -32603,
            message: "internal".into(),
        };
        assert_eq!(
            format!("{rpc}"),
            "rpc error -32603: internal",
            "McpClientError::Rpc Display must remain 'rpc error \
             <code>: <message>' — the 'rpc error' prefix distinguishes \
             upstream MCP protocol failures from transport-level Closed \
             events in dashboards that group by prefix. A typo that \
             dropped 'error' ('rpc <code>: <message>') would break the \
             grep contract"
        );

        let closed = McpClientError::Closed;
        assert_eq!(
            format!("{closed}"),
            "transport closed",
            "McpClientError::Closed Display must remain 'transport \
             closed' — cross-binds with TRANSPORT_CLOSED_MESSAGE (const \
             at line 75) which is already pinned by \
             transport_closed_constants_pin_minus_32099_code_and_transport_closed_message. \
             The const pin asserts the CONST value, not the #[error] \
             attribute compiled into Display; a drift between Display \
             ('transport-closed' with hyphen) and the const ('transport \
             closed' with space) would silently desync operator-facing \
             log scraping from the wire-level boundary check"
        );

        let timeout = McpClientError::Timeout(Duration::from_secs(30));
        let timeout_message = format!("{timeout}");
        assert!(
            timeout_message.contains("timeout after"),
            "McpClientError::Timeout must keep the 'timeout after' \
             prefix — a refactor to 'timed out:' or 'request timeout' \
             would break dashboards that grep 'timeout after' to \
             correlate request-timeout incidents with the documented \
             DEFAULT_REQUEST_TIMEOUT (30s): {timeout_message}"
        );
        assert!(
            timeout_message.contains("30s"),
            "McpClientError::Timeout must surface Duration in Debug \
             format ('30s' for 30 seconds) — the #[error] attribute \
             uses {{0:?}} (Debug) because Duration does NOT impl \
             Display; a refactor that wrapped Duration in a manual \
             Display impl emitting decimals (e.g., '30.000000000') \
             would silently shift the operator-facing duration string. \
             The pin's '30s' substring catches the format change: \
             {timeout_message}"
        );

        let crashed = McpClientError::ServerCrashed;
        assert_eq!(
            format!("{crashed}"),
            "server crashed before responding",
            "McpClientError::ServerCrashed Display must keep the \
             'before responding' qualifier — distinguishes a mid-\
             request crash (no response coming) from a clean shutdown \
             after responding (Closed). Operators triaging request-\
             correlation failures need the qualifier to know whether \
             to retry the request or abandon it. A rewrite to 'server \
             crashed' would silently merge with hypothetical other \
             crash diagnostics"
        );
    }

    #[test]
    fn mcp_client_error_io_and_serde_display_messages_pin_prefix_and_external_source_display_delegation(
    ) {
        let io_err = McpClientError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "stdio pipe missing",
        ));
        let io_message = format!("{io_err}");
        assert!(
            io_message.starts_with("io: "),
            "McpClientError::Io must surface the literal 'io: ' bootstrap-stage prefix so audit-log filters can distinguish stdio-pipe disk faults from JSON-RPC frame parse faults during the MCP client's request/notify loop (dropped-prefix regression class): {io_message}"
        );
        assert!(
            io_message.contains("stdio pipe missing"),
            "McpClientError::Io must surface the inner std::io::Error Display rendering after the colon ({{0}}, not {{0:?}}); a Debug refactor would render 'Custom {{ kind: NotFound, error: ... }}' instead of the message payload (Debug-vs-Display formatting regression class on the {{0}} interpolation): {io_message}"
        );
        assert!(
            !io_message.contains("Custom {") && !io_message.contains("Os {"),
            "McpClientError::Io must NOT surface the std::io::Error Debug rendering; a Debug refactor on {{0}} would expose internal struct fields like 'Custom {{ kind: ..., error: ... }}' or 'Os {{ code: ..., kind: ..., message: ... }}' (Debug-vs-Display formatting regression class on the {{0}} interpolation): {io_message}"
        );

        let serde_source =
            serde_json::from_str::<serde_json::Value>("not json").expect_err("parse must fail");
        let serde_err = McpClientError::Serde(serde_source);
        let serde_message = format!("{serde_err}");
        assert!(
            serde_message.starts_with("serde: "),
            "McpClientError::Serde must surface the literal 'serde: ' bootstrap-stage prefix so audit-log filters can distinguish JSON-RPC frame parse faults from stdio-pipe disk faults (dropped-prefix regression class): {serde_message}"
        );
        assert!(
            serde_message.contains("expected"),
            "McpClientError::Serde must surface the inner serde_json::Error Display rendering after the colon (serde_json renders parse failures with 'expected ...' Display strings); a Debug refactor on {{0}} would render 'Error(\"...\", line: N, column: M)' instead (Debug-vs-Display formatting regression class on the {{0}} interpolation): {serde_message}"
        );
        assert!(
            !serde_message.contains("Error("),
            "McpClientError::Serde must NOT surface the serde_json::Error Debug rendering; a Debug refactor on {{0}} would expose 'Error(\"...\", line: N, column: M)' buffer-position structs (Debug-vs-Display formatting regression class on the {{0}} interpolation): {serde_message}"
        );

        assert_ne!(
            io_message, serde_message,
            "McpClientError::Io and McpClientError::Serde Display must not converge; merging the two prefixes would lose the stdio-pipe-fault vs JSON-RPC-frame-parse-fault discriminator (prefix-convergence regression class): io={io_message} serde={serde_message}"
        );
        assert!(
            !io_message.starts_with("serde:") && !serde_message.starts_with("io:"),
            "McpClientError::Io must not start with 'serde:' and McpClientError::Serde must not start with 'io:'; a sibling-prefix swap would silently mis-route incident triage (sibling-prefix-swap regression class): io={io_message} serde={serde_message}"
        );

        let string_variant_prefixes = [
            "rpc error ",
            "transport closed",
            "timeout after",
            "server crashed before responding",
        ];
        for prefix in string_variant_prefixes {
            assert!(
                !io_message.starts_with(prefix),
                "McpClientError::Io must not converge with the string-variant surface '{prefix}' pinned by mcp_client_error_display_messages_pin_four_string_variant_format_strings; a stdio-pipe fault must not be mis-routed as a structured protocol violation (string-surface-convergence regression class): {io_message}"
            );
            assert!(
                !serde_message.starts_with(prefix),
                "McpClientError::Serde must not converge with the string-variant surface '{prefix}' pinned by mcp_client_error_display_messages_pin_four_string_variant_format_strings; a JSON-RPC frame parse fault must not be mis-routed as a structured protocol violation (string-surface-convergence regression class): {serde_message}"
            );
        }
    }
}
