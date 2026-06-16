//! Ergonomic async client for building agents against a running `covenantd`
//! daemon.
//!
//! Covenant agents talk to the daemon over a length-prefixed JSON IPC protocol
//! on a Unix socket (`$COVENANT_HOME/sock`). [`Client`] wraps the connect,
//! authenticate, and request/response round-trips so an author never hand-rolls
//! a frame:
//!
//! ```no_run
//! # async fn run() -> Result<(), covenant_sdk::SdkError> {
//! use covenant_sdk::{Client, MemoryTier};
//!
//! let mut client = Client::connect_default().await?;
//! let tools = client.list_tools().await?;
//! let result = client.call_tool("echo", serde_json::json!({ "text": "hi" })).await?;
//! let recent = client.recent_memory(Some(MemoryTier::Working), 10).await?;
//! # let _ = (tools, result, recent);
//! # Ok(())
//! # }
//! ```
//!
//! The client borrows the daemon's own protocol types (`covenant-ipc`'s
//! `Request`/`Response`), so the typed surface here cannot drift from the wire
//! shapes the daemon accepts. The frame codec, the authentication handshake, and
//! response demultiplexing stay encapsulated; authors see typed methods and
//! domain types only.
//!
//! ## Not exposed here
//!
//! Memory is **read-only** over IPC today ([`Client::recent_memory`],
//! [`Client::search_memory`]). The daemon writes memory as a side effect of
//! intent execution; there is no client-facing memory-write verb to wrap.
//! Streaming responses (ADR 0010 v2) and operator-only verbs (purge, repair,
//! peer registry, settlement backfill) are intentionally outside this
//! author-facing surface.

use std::path::{Path, PathBuf};

use covenant_ipc::{read_frame, write_frame, Request, Response};
use serde_json::Value;
use tokio::net::UnixStream;

pub use covenant_ipc::{IpcError, ProtocolInfo};
pub use covenant_mcp::{Content, ToolSpec};
pub use covenant_permissions::SignedCapability;
pub use covenant_types::{AgentId, Capability, MemoryRecord, MemoryTier, SettlementReceipt};
pub use uuid::Uuid;

/// Everything that can go wrong talking to the daemon.
#[derive(Debug, thiserror::Error)]
pub enum SdkError {
    /// Neither `COVENANT_HOME` nor `HOME` is set, so the daemon home cannot be
    /// resolved from the environment.
    #[error("cannot resolve daemon home: set COVENANT_HOME or HOME")]
    HomeUnresolved,

    /// The Unix socket could not be reached — usually the daemon is not running.
    #[error("connect to daemon socket {path:?}: {source} (is covenantd running?)")]
    Connect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The operator token file could not be read.
    #[error("read operator token at {path:?}: {source} (start covenantd once to mint it)")]
    Token {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The daemon rejected the token; the connection is closed.
    #[error("daemon rejected authentication: {0}")]
    Authentication(String),

    /// The daemon answered with [`Response::Error`] — e.g. a denied capability.
    /// The connection remains usable for further requests.
    #[error("daemon returned an error: {0}")]
    Daemon(String),

    /// The daemon answered a request with a response variant the SDK does not
    /// expect for that verb.
    #[error("unexpected daemon response to {request}: {got}")]
    Unexpected { request: &'static str, got: String },

    /// A frame-level transport failure (I/O, malformed JSON, oversized frame).
    #[error(transparent)]
    Wire(#[from] IpcError),
}

/// Resolve the daemon home from the environment: `$COVENANT_HOME` if set, else
/// `$HOME/.covenant`. Mirrors the resolution the `covenant` CLI uses.
pub fn home_dir() -> Result<PathBuf, SdkError> {
    resolve_home(
        std::env::var("COVENANT_HOME").ok(),
        std::env::var("HOME").ok(),
    )
}

fn resolve_home(covenant_home: Option<String>, home: Option<String>) -> Result<PathBuf, SdkError> {
    if let Some(explicit) = covenant_home {
        return Ok(PathBuf::from(explicit));
    }
    match home {
        Some(home) => Ok(PathBuf::from(home).join(".covenant")),
        None => Err(SdkError::HomeUnresolved),
    }
}

/// Path to the daemon's IPC socket inside `home`.
pub fn socket_path(home: &Path) -> PathBuf {
    home.join("sock")
}

/// Path to the operator bootstrap token inside `home`.
pub fn operator_token_path(home: &Path) -> PathBuf {
    home.join("peers").join("operator.token")
}

/// An authenticated connection to a running `covenantd` daemon.
///
/// Construct with [`Client::connect_default`] (home and token resolved from the
/// environment) or the lower-level [`Client::connect_authenticated`]. Each
/// method is a single request/response round-trip on the underlying socket; the
/// connection stays open and bound to the authenticated identity so it can be
/// reused across calls.
#[derive(Debug)]
pub struct Client {
    stream: UnixStream,
    identity: String,
}

impl Client {
    /// Connect using the environment-resolved home and the operator token file.
    /// The one-liner authors reach for.
    pub async fn connect_default() -> Result<Client, SdkError> {
        let home = home_dir()?;
        Client::connect_with_token_file(&home).await
    }

    /// Connect to the socket under `home`, reading the bootstrap token from
    /// `<home>/peers/operator.token`.
    pub async fn connect_with_token_file(home: &Path) -> Result<Client, SdkError> {
        let token = read_operator_token(home)?;
        Client::connect_authenticated(home, &token).await
    }

    /// Connect to the socket under `home` and authenticate with `token_b58`.
    pub async fn connect_authenticated(home: &Path, token_b58: &str) -> Result<Client, SdkError> {
        let mut stream = connect_socket(home).await?;
        let identity = authenticate(&mut stream, token_b58).await?;
        Ok(Client { stream, identity })
    }

    /// The daemon-confirmed identity bound to this connection (`AgentId.display`).
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// Liveness check.
    pub async fn ping(&mut self) -> Result<(), SdkError> {
        match self.roundtrip(&Request::Ping).await? {
            Response::Pong => Ok(()),
            Response::Error { message } => Err(SdkError::Daemon(message)),
            other => Err(unexpected("ping", &other)),
        }
    }

    /// The daemon's advertised protocol version range.
    pub async fn protocol_info(&mut self) -> Result<ProtocolInfo, SdkError> {
        match self.roundtrip(&Request::ProtocolInfo).await? {
            Response::ProtocolInfo { info } => Ok(info),
            Response::Error { message } => Err(SdkError::Daemon(message)),
            other => Err(unexpected("protocol_info", &other)),
        }
    }

    /// Submit an intent for the daemon to dispatch and return the result.
    pub async fn submit_intent(
        &mut self,
        text: impl Into<String>,
    ) -> Result<IntentOutcome, SdkError> {
        let request = Request::SubmitIntent {
            text: text.into(),
            prefer_stream: None,
        };
        match self.roundtrip(&request).await? {
            Response::IntentResult {
                intent_id,
                status,
                text,
                sources,
                settlement,
            } => Ok(IntentOutcome {
                intent_id,
                status,
                text,
                sources,
                settlement,
            }),
            Response::Error { message } => Err(SdkError::Daemon(message)),
            other => Err(unexpected("submit_intent", &other)),
        }
    }

    /// List the tools the daemon's router advertises.
    pub async fn list_tools(&mut self) -> Result<Vec<ToolSpec>, SdkError> {
        match self.roundtrip(&Request::ListTools).await? {
            Response::ToolList { tools } => Ok(tools),
            Response::Error { message } => Err(SdkError::Daemon(message)),
            other => Err(unexpected("list_tools", &other)),
        }
    }

    /// Call a tool by name with JSON `arguments` and return its output blocks.
    /// `is_error` on the result reflects a tool-level failure, distinct from a
    /// daemon-level [`SdkError::Daemon`].
    pub async fn call_tool(
        &mut self,
        name: impl Into<String>,
        arguments: Value,
    ) -> Result<ToolOutcome, SdkError> {
        let request = Request::CallTool {
            name: name.into(),
            arguments,
        };
        match self.roundtrip(&request).await? {
            Response::ToolResult { content, is_error } => Ok(ToolOutcome { content, is_error }),
            Response::Error { message } => Err(SdkError::Daemon(message)),
            other => Err(unexpected("call_tool", &other)),
        }
    }

    /// Fetch the caller's most recent memory records, newest first. `tier`
    /// narrows to one tier; `None` spans all tiers.
    pub async fn recent_memory(
        &mut self,
        tier: Option<MemoryTier>,
        limit: usize,
    ) -> Result<Vec<MemoryRecord>, SdkError> {
        let request = Request::RecentMemory {
            tier,
            limit,
            prefer_stream: None,
        };
        match self.roundtrip(&request).await? {
            Response::Memories { records } => Ok(records),
            Response::Error { message } => Err(SdkError::Daemon(message)),
            other => Err(unexpected("recent_memory", &other)),
        }
    }

    /// Semantic-search the caller's memory. `min_relevance` is an optional
    /// cosine-similarity floor in `[0.0, 1.0]`.
    pub async fn search_memory(
        &mut self,
        query: impl Into<String>,
        tier: Option<MemoryTier>,
        limit: usize,
        min_relevance: Option<f32>,
    ) -> Result<Vec<MemoryRecord>, SdkError> {
        let request = Request::SearchMemory {
            query: query.into(),
            tier,
            limit,
            min_relevance,
        };
        match self.roundtrip(&request).await? {
            Response::Memories { records } => Ok(records),
            Response::Error { message } => Err(SdkError::Daemon(message)),
            other => Err(unexpected("search_memory", &other)),
        }
    }

    /// List the caller's most recently granted capabilities.
    pub async fn recent_capabilities(
        &mut self,
        limit: usize,
    ) -> Result<Vec<SignedCapability>, SdkError> {
        match self
            .roundtrip(&Request::RecentCapabilities { limit })
            .await?
        {
            Response::Capabilities { capabilities } => Ok(capabilities),
            Response::Error { message } => Err(SdkError::Daemon(message)),
            other => Err(unexpected("recent_capabilities", &other)),
        }
    }

    /// Request a capability grant for `action` with an optional `scope` payload
    /// and expiry (epoch ms). Daemon policy decides whether to sign it.
    pub async fn grant_capability(
        &mut self,
        action: impl Into<String>,
        scope: Option<Value>,
        expires_at: Option<u64>,
    ) -> Result<GrantedCapability, SdkError> {
        let request = Request::GrantCapability {
            action: action.into(),
            scope,
            expires_at,
        };
        match self.roundtrip(&request).await? {
            Response::CapabilityGranted {
                signature_b58,
                subject_display,
                action,
            } => Ok(GrantedCapability {
                signature_b58,
                subject_display,
                action,
            }),
            Response::Error { message } => Err(SdkError::Daemon(message)),
            other => Err(unexpected("grant_capability", &other)),
        }
    }

    async fn roundtrip(&mut self, request: &Request) -> Result<Response, SdkError> {
        write_frame(&mut self.stream, request).await?;
        Ok(read_frame::<_, Response>(&mut self.stream).await?)
    }
}

/// Result of [`Client::submit_intent`].
#[derive(Debug, Clone)]
pub struct IntentOutcome {
    pub intent_id: Uuid,
    pub status: String,
    pub text: String,
    pub sources: Vec<String>,
    pub settlement: Option<SettlementReceipt>,
}

/// Result of [`Client::call_tool`].
#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub content: Vec<Content>,
    pub is_error: bool,
}

/// Result of [`Client::grant_capability`].
#[derive(Debug, Clone)]
pub struct GrantedCapability {
    pub signature_b58: String,
    pub subject_display: String,
    pub action: String,
}

async fn connect_socket(home: &Path) -> Result<UnixStream, SdkError> {
    let path = socket_path(home);
    UnixStream::connect(&path)
        .await
        .map_err(|source| SdkError::Connect { path, source })
}

fn read_operator_token(home: &Path) -> Result<String, SdkError> {
    let path = operator_token_path(home);
    match std::fs::read_to_string(&path) {
        Ok(raw) => Ok(raw.trim().to_string()),
        Err(source) => Err(SdkError::Token { path, source }),
    }
}

async fn authenticate(stream: &mut UnixStream, token_b58: &str) -> Result<String, SdkError> {
    write_frame(
        stream,
        &Request::Authenticate {
            token_b58: token_b58.to_string(),
        },
    )
    .await?;
    match read_frame::<_, Response>(stream).await? {
        Response::Authenticated { display } => Ok(display),
        Response::AuthenticationFailed { reason } => Err(SdkError::Authentication(reason)),
        other => Err(unexpected("authenticate", &other)),
    }
}

fn unexpected(request: &'static str, got: &Response) -> SdkError {
    SdkError::Unexpected {
        request,
        got: response_kind(got),
    }
}

/// The serde `kind` tag of a response with its payload stripped, so an
/// unexpected secret-bearing response can never spill its value into an error
/// string a caller might log.
fn response_kind(response: &Response) -> String {
    serde_json::to_value(response)
        .ok()
        .and_then(|value| value.get("kind")?.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use tokio::net::UnixListener;
    use tokio::task::JoinHandle;

    /// 32 zero bytes, base58 — a wire-valid `AgentId.pubkey`.
    const ZERO_PUBKEY_B58: &str = "11111111111111111111111111111111";

    /// A fake daemon over a real Unix socket. It speaks the genuine frame codec
    /// and the real `Request`/`Response` enums, so every test exercises the wire
    /// boundary the SDK actually rides. `responder` maps each decoded request to
    /// the response JSON to send back; received requests are recorded as wire
    /// JSON for both-direction assertions.
    struct Harness {
        dir: TempDir,
        received: Arc<Mutex<Vec<Value>>>,
        task: JoinHandle<()>,
    }

    impl Harness {
        fn start<F>(responder: F) -> Harness
        where
            F: Fn(&Request) -> Value + Send + Sync + 'static,
        {
            let dir = TempDir::new().unwrap();
            std::fs::create_dir_all(dir.path().join("peers")).unwrap();
            std::fs::write(operator_token_path(dir.path()), "  operator-token-xyz  \n").unwrap();
            let listener = UnixListener::bind(socket_path(dir.path())).unwrap();
            let received = Arc::new(Mutex::new(Vec::new()));
            let received_task = received.clone();
            let responder = Arc::new(responder);
            let task = tokio::spawn(async move {
                while let Ok((mut stream, _)) = listener.accept().await {
                    loop {
                        let request: Request = match read_frame(&mut stream).await {
                            Ok(r) => r,
                            Err(_) => break,
                        };
                        received_task
                            .lock()
                            .unwrap()
                            .push(serde_json::to_value(&request).unwrap());
                        let response = responder(&request);
                        if write_frame(&mut stream, &response).await.is_err() {
                            break;
                        }
                    }
                }
            });
            Harness {
                dir,
                received,
                task,
            }
        }

        fn home(&self) -> &Path {
            self.dir.path()
        }

        fn received(&self) -> Vec<Value> {
            self.received.lock().unwrap().clone()
        }

        async fn client(&self) -> Client {
            Client::connect_with_token_file(self.home()).await.unwrap()
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn ok_auth() -> Value {
        json!({ "kind": "authenticated", "display": "agent@local" })
    }

    fn err_resp(message: &str) -> Value {
        json!({ "kind": "error", "message": message })
    }

    #[tokio::test]
    async fn connect_authenticates_and_binds_identity() {
        let harness = Harness::start(|req| match req {
            Request::Authenticate { .. } => ok_auth(),
            _ => err_resp("unexpected"),
        });
        let client = harness.client().await;
        assert_eq!(client.identity(), "agent@local");
        let received = harness.received();
        assert_eq!(received[0]["kind"], "authenticate");
        // The file's surrounding whitespace was trimmed before sending.
        assert_eq!(received[0]["token_b58"], "operator-token-xyz");
    }

    #[tokio::test]
    async fn authentication_failure_surfaces_reason() {
        let harness = Harness::start(
            |_| json!({ "kind": "authentication_failed", "reason": "token revoked" }),
        );
        let err = Client::connect_with_token_file(harness.home())
            .await
            .unwrap_err();
        assert!(matches!(err, SdkError::Authentication(reason) if reason == "token revoked"));
    }

    #[tokio::test]
    async fn connect_to_missing_socket_reports_path() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("peers")).unwrap();
        std::fs::write(operator_token_path(dir.path()), "token").unwrap();
        let err = Client::connect_with_token_file(dir.path())
            .await
            .unwrap_err();
        match err {
            SdkError::Connect { path, .. } => assert_eq!(path, socket_path(dir.path())),
            other => panic!("expected Connect, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_token_file_reports_path() {
        let dir = TempDir::new().unwrap();
        let err = Client::connect_with_token_file(dir.path())
            .await
            .unwrap_err();
        match err {
            SdkError::Token { path, .. } => assert_eq!(path, operator_token_path(dir.path())),
            other => panic!("expected Token, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn submit_intent_roundtrips_and_sends_text() {
        let harness = Harness::start(|req| match req {
            Request::Authenticate { .. } => ok_auth(),
            Request::SubmitIntent { text, .. } => json!({
                "kind": "intent_result",
                "intent_id": "00000000-0000-0000-0000-000000000001",
                "status": "ok",
                "text": format!("echo: {text}"),
                "sources": ["a", "b"],
                "settlement": null,
            }),
            _ => err_resp("nope"),
        });
        let mut client = harness.client().await;
        let out = client.submit_intent("do the thing").await.unwrap();
        assert_eq!(out.status, "ok");
        assert_eq!(out.text, "echo: do the thing");
        assert_eq!(out.sources, vec!["a".to_string(), "b".to_string()]);
        assert!(out.settlement.is_none());
        let received = harness.received();
        assert_eq!(received[1]["kind"], "submit_intent");
        assert_eq!(received[1]["text"], "do the thing");
        // v1 terminal-frame behaviour: prefer_stream is dropped on the wire.
        assert!(received[1].get("prefer_stream").is_none());
    }

    #[tokio::test]
    async fn call_tool_roundtrips_with_arguments_and_error_flag() {
        let harness = Harness::start(|req| match req {
            Request::Authenticate { .. } => ok_auth(),
            Request::CallTool { .. } => json!({
                "kind": "tool_result",
                "content": [{ "type": "text", "text": "boom" }],
                "is_error": true,
            }),
            _ => err_resp("nope"),
        });
        let mut client = harness.client().await;
        let out = client
            .call_tool("search", json!({ "q": "rust" }))
            .await
            .unwrap();
        assert!(out.is_error);
        assert_eq!(out.content, vec![Content::text("boom")]);
        let received = harness.received();
        assert_eq!(received[1]["kind"], "call_tool");
        assert_eq!(received[1]["name"], "search");
        assert_eq!(received[1]["arguments"]["q"], "rust");
    }

    #[tokio::test]
    async fn list_tools_parses_specs() {
        let harness = Harness::start(|req| match req {
            Request::Authenticate { .. } => ok_auth(),
            Request::ListTools => json!({
                "kind": "tool_list",
                "tools": [{
                    "name": "echo",
                    "description": "echoes input",
                    "inputSchema": { "type": "object" },
                }],
            }),
            _ => err_resp("nope"),
        });
        let mut client = harness.client().await;
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
    }

    #[tokio::test]
    async fn recent_memory_parses_records_and_sends_filters() {
        let harness = Harness::start(|req| match req {
            Request::Authenticate { .. } => ok_auth(),
            Request::RecentMemory { .. } => json!({
                "kind": "memories",
                "records": [{
                    "id": "00000000-0000-0000-0000-0000000000aa",
                    "tier": "working",
                    "owner": { "display": "agent@local", "pubkey": ZERO_PUBKEY_B58 },
                    "text": "a note",
                    "embedding": [],
                    "metadata": null,
                    "created_at": 123,
                }],
            }),
            _ => err_resp("nope"),
        });
        let mut client = harness.client().await;
        let records = client
            .recent_memory(Some(MemoryTier::Working), 7)
            .await
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].text, "a note");
        assert_eq!(records[0].tier, MemoryTier::Working);
        let received = harness.received();
        assert_eq!(received[1]["kind"], "recent_memory");
        assert_eq!(received[1]["tier"], "working");
        assert_eq!(received[1]["limit"], 7);
    }

    #[tokio::test]
    async fn search_memory_sends_query_and_threshold() {
        let harness = Harness::start(|req| match req {
            Request::Authenticate { .. } => ok_auth(),
            Request::SearchMemory { .. } => json!({ "kind": "memories", "records": [] }),
            _ => err_resp("nope"),
        });
        let mut client = harness.client().await;
        let records = client
            .search_memory("needle", None, 5, Some(0.25))
            .await
            .unwrap();
        assert!(records.is_empty());
        let received = harness.received();
        assert_eq!(received[1]["kind"], "search_memory");
        assert_eq!(received[1]["query"], "needle");
        assert_eq!(received[1]["limit"], 5);
        assert_eq!(received[1]["min_relevance"], 0.25);
    }

    #[tokio::test]
    async fn grant_capability_returns_signature_and_sends_scope() {
        let harness = Harness::start(|req| match req {
            Request::Authenticate { .. } => ok_auth(),
            Request::GrantCapability { .. } => json!({
                "kind": "capability_granted",
                "signature_b58": "SiGN",
                "subject_display": "agent@local",
                "action": "memory.read",
            }),
            _ => err_resp("nope"),
        });
        let mut client = harness.client().await;
        let granted = client
            .grant_capability("memory.read", Some(json!({ "tier": "working" })), Some(999))
            .await
            .unwrap();
        assert_eq!(granted.signature_b58, "SiGN");
        assert_eq!(granted.action, "memory.read");
        let received = harness.received();
        assert_eq!(received[1]["kind"], "grant_capability");
        assert_eq!(received[1]["action"], "memory.read");
        assert_eq!(received[1]["scope"]["tier"], "working");
        assert_eq!(received[1]["expires_at"], 999);
    }

    #[tokio::test]
    async fn recent_capabilities_parses_signed_grants() {
        let harness = Harness::start(|req| match req {
            Request::Authenticate { .. } => ok_auth(),
            Request::RecentCapabilities { .. } => {
                let agent = json!({ "display": "agent@local", "pubkey": ZERO_PUBKEY_B58 });
                let signature = "1".repeat(64);
                json!({
                    "kind": "capabilities",
                    "capabilities": [{
                        "capability": {
                            "subject": agent.clone(),
                            "action": "memory.read",
                            "scope": {},
                            "granted_by": agent,
                            "expires_at": null,
                        },
                        "signature": signature,
                    }],
                })
            }
            _ => err_resp("nope"),
        });
        let mut client = harness.client().await;
        let caps = client.recent_capabilities(20).await.unwrap();
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].capability.action, "memory.read");
    }

    #[tokio::test]
    async fn daemon_error_maps_and_connection_survives() {
        let harness = Harness::start(|req| match req {
            Request::Authenticate { .. } => ok_auth(),
            Request::ListTools => err_resp("capability denied"),
            Request::Ping => json!({ "kind": "pong" }),
            _ => err_resp("nope"),
        });
        let mut client = harness.client().await;
        let err = client.list_tools().await.unwrap_err();
        assert!(matches!(err, SdkError::Daemon(message) if message == "capability denied"));
        // The connection is still usable for the next request after a daemon error.
        client.ping().await.unwrap();
    }

    #[tokio::test]
    async fn unexpected_response_is_reported() {
        let harness = Harness::start(|req| match req {
            Request::Authenticate { .. } => ok_auth(),
            _ => json!({ "kind": "pong" }),
        });
        let mut client = harness.client().await;
        let err = client.list_tools().await.unwrap_err();
        match err {
            SdkError::Unexpected { request, .. } => assert_eq!(request, "list_tools"),
            other => panic!("expected Unexpected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unexpected_secret_response_keeps_value_out_of_the_error() {
        let harness = Harness::start(|req| match req {
            Request::Authenticate { .. } => ok_auth(),
            _ => json!({ "kind": "secret", "name": "API_KEY", "value": "super-secret-material" }),
        });
        let mut client = harness.client().await;
        let err = client.list_tools().await.unwrap_err();
        let rendered = err.to_string();
        assert!(
            !rendered.contains("super-secret-material"),
            "secret value leaked into error: {rendered}"
        );
        match err {
            SdkError::Unexpected { request, got } => {
                assert_eq!(request, "list_tools");
                assert_eq!(got, "secret");
            }
            other => panic!("expected Unexpected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn protocol_info_reports_versions() {
        let harness = Harness::start(|req| match req {
            Request::Authenticate { .. } => ok_auth(),
            Request::ProtocolInfo => json!({
                "kind": "protocol_info",
                "info": {
                    "protocol": "covenant.ipc",
                    "version": 1,
                    "min_supported": 1,
                    "max_supported": 2,
                },
            }),
            _ => err_resp("nope"),
        });
        let mut client = harness.client().await;
        let info = client.protocol_info().await.unwrap();
        assert_eq!(info.protocol, "covenant.ipc");
        assert_eq!(info.max_supported, 2);
    }

    #[test]
    fn resolve_home_prefers_covenant_home() {
        let home = resolve_home(Some("/explicit".into()), Some("/home/u".into())).unwrap();
        assert_eq!(home, PathBuf::from("/explicit"));
    }

    #[test]
    fn resolve_home_falls_back_to_home_dotdir() {
        let home = resolve_home(None, Some("/home/u".into())).unwrap();
        assert_eq!(home, PathBuf::from("/home/u/.covenant"));
    }

    #[test]
    fn resolve_home_errors_without_env() {
        assert!(matches!(
            resolve_home(None, None),
            Err(SdkError::HomeUnresolved)
        ));
    }

    #[test]
    fn path_helpers_match_daemon_layout() {
        let home = Path::new("/var/covenant");
        assert_eq!(socket_path(home), PathBuf::from("/var/covenant/sock"));
        assert_eq!(
            operator_token_path(home),
            PathBuf::from("/var/covenant/peers/operator.token")
        );
    }
}
