//! IPC client used by the TUI binary and exercised by the live test
//! suite. Connects to a running `covenantd` over the local Unix
//! socket, authenticates with the operator token, and submits an
//! intent.
//!
//! Kept separate from the App state machine so the state machine
//! stays terminal-and-socket free for unit tests, and the IPC path
//! is in turn reachable from `tests/` for live coverage.

use std::io;
use std::path::{Path, PathBuf};

use covenant_a2a::A2ATask;
use covenant_audit::AuditEvent;
use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_permissions::SignedCapability;
use covenant_types::{MemoryRecord, MemoryTier, SettlementReceipt};
use thiserror::Error;
use tokio::net::UnixStream;

use crate::SubmissionOutcome;

/// Errors produced by the TUI IPC client.
///
/// `DaemonNotRunning` is split out so the renderer can show a
/// targeted hint (`run covenantd start`) instead of a raw "no such
/// file" or "connection refused" message. The classifier matches on
/// [`std::io::ErrorKind`] rather than the platform-specific message
/// text so a translated locale doesn't bypass it.
#[derive(Debug, Error)]
pub enum IpcError {
    #[error("covenantd is not running at {}; run `covenantd start`", sock_path.display())]
    DaemonNotRunning { sock_path: PathBuf },
    #[error("HOME is not set; set $COVENANT_HOME or $HOME")]
    HomeNotSet,
    #[error("operator token at {} is empty; rotate or rebootstrap the daemon", path.display())]
    OperatorTokenEmpty { path: PathBuf },
    #[error("read operator token at {path}: {source}", path = path.display())]
    OperatorTokenRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("IPC wire error: {0}")]
    Wire(#[from] io::Error),
    #[error("IPC framing error: {0}")]
    Frame(#[from] covenant_ipc::IpcError),
    #[error("{0}")]
    Other(String),
}

/// Maps a [`UnixStream::connect`] error to the right [`IpcError`]
/// variant. `NotFound` (socket file absent) and `ConnectionRefused`
/// (socket exists but no one is listening — e.g. daemon crashed mid-
/// run) are both surfaced as `DaemonNotRunning`. Other I/O errors
/// pass through unchanged so genuine wire failures aren't masked.
pub fn map_socket_error(err: io::Error, sock_path: &Path) -> IpcError {
    match err.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => IpcError::DaemonNotRunning {
            sock_path: sock_path.to_path_buf(),
        },
        _ => IpcError::Wire(err),
    }
}

async fn connect_socket(home: &Path) -> Result<(UnixStream, PathBuf), IpcError> {
    let sock = home.join("sock");
    match UnixStream::connect(&sock).await {
        Ok(stream) => Ok((stream, sock)),
        Err(err) => Err(map_socket_error(err, &sock)),
    }
}

/// Hard cap on `Request::RecentMemory::limit` to prevent a runaway
/// request from asking the daemon to enumerate the entire memory
/// table into a single IPC frame.
pub const RECENT_MEMORY_LIMIT_CAP: usize = 50;

/// Hard cap on `Request::RecentAudit::limit`. Audit volume can grow
/// faster than memory, so the cap is set higher.
pub const RECENT_AUDIT_LIMIT_CAP: usize = 100;

/// Hard cap on `Request::RecentCapabilities::limit`. Capabilities
/// grow slowly (one per grant) so the cap matches recent_memory.
pub const RECENT_CAPABILITIES_LIMIT_CAP: usize = 50;

/// Hard cap on `Request::RecentA2ATasks::limit`. The mailbox is
/// server-side filtered to tasks the operator sent or received, so
/// it grows linearly with A2A traffic; the cap is set higher than
/// memory but below audit.
pub const RECENT_A2A_LIMIT_CAP: usize = 50;

/// Hard cap on `Request::RecentReceipts::limit`. Receipts are
/// server-side filtered to rows where the payer matches the calling
/// peer; one row per resource consumption event.
pub const RECENT_RECEIPTS_LIMIT_CAP: usize = 50;

/// Outcome of a [`grant_capability`] call. Same shape as
/// [`SubmissionOutcome`]: wire-level errors bubble up as `Err`, and
/// daemon-side rejections collapse into `Failed { message }` so a
/// caller can render the daemon's reason verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantOutcome {
    Granted {
        signature_b58: String,
        subject_display: String,
        action: String,
    },
    Failed {
        message: String,
    },
}

/// Outcome of a [`recent_memory`] call. Wire-level errors bubble up
/// as `Err`; daemon-side rejections (e.g. missing `memory.read`)
/// collapse into `Failed { message }` so the TUI can render the
/// reason inside the memory tail screen.
#[derive(Debug, Clone, PartialEq)]
pub enum MemoryFetchOutcome {
    Fetched { records: Vec<MemoryRecord> },
    Failed { message: String },
}

/// Outcome of a [`recent_audit`] call. Same shape as
/// [`MemoryFetchOutcome`]. `recent_audit` is filtered server-side to
/// the calling peer's own audit rows, so a `Failed` here always
/// reflects a wire or auth issue, never a capability gate.
#[derive(Debug, Clone, PartialEq)]
pub enum AuditFetchOutcome {
    Fetched { events: Vec<AuditEvent> },
    Failed { message: String },
}

/// Outcome of a [`recent_capabilities`] call. Same shape as the
/// other fetch outcomes; daemon filters server-side to caps where
/// `subject` or `granted_by` matches the calling peer.
#[derive(Debug, Clone, PartialEq)]
pub enum CapabilitiesFetchOutcome {
    Fetched { capabilities: Vec<SignedCapability> },
    Failed { message: String },
}

/// Outcome of a [`recent_a2a_tasks`] call. Daemon filters tasks
/// server-side to ones where the operator is sender or recipient,
/// so a `Failed` here is always a wire or auth issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum A2aFetchOutcome {
    Fetched { tasks: Vec<A2ATask> },
    Failed { message: String },
}

/// Outcome of a [`recent_receipts`] call. Daemon gates the read
/// behind the `chain.receipts` capability and filters server-side to
/// rows where the payer matches the calling peer; a missing grant
/// surfaces as `Failed { message }` so the TUI can render the
/// daemon's reason inside the receipts view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptsFetchOutcome {
    Fetched { receipts: Vec<SettlementReceipt> },
    Failed { message: String },
}

/// Resolves `$COVENANT_HOME` with the same fallback shape as the
/// existing `covenant` CLI: explicit env var first, then
/// `$HOME/.covenant`.
pub fn covenant_home() -> Result<PathBuf, IpcError> {
    if let Ok(p) = std::env::var("COVENANT_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var("HOME").map_err(|_| IpcError::HomeNotSet)?;
    Ok(PathBuf::from(home).join(".covenant"))
}

/// Reads the operator bootstrap token from `$COVENANT_HOME/peers/operator.token`.
/// The daemon mints the token on first boot at mode 0600.
pub async fn read_operator_token(home: &Path) -> Result<String, IpcError> {
    let path = home.join("peers").join("operator.token");
    let raw =
        tokio::fs::read_to_string(&path)
            .await
            .map_err(|source| IpcError::OperatorTokenRead {
                path: path.clone(),
                source,
            })?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(IpcError::OperatorTokenEmpty { path });
    }
    Ok(trimmed.to_string())
}

/// Connects to `$COVENANT_HOME/sock`, authenticates with the operator
/// token, sends `Request::SubmitIntent`, and maps the daemon response
/// into a [`SubmissionOutcome`].
///
/// Wire-level errors (no socket, broken pipe) bubble up as
/// `Err(anyhow::Error)`; daemon-level errors (auth failed, capability
/// missing, unexpected response shape) collapse into
/// `Ok(SubmissionOutcome::Failed { message })` so the App can render
/// the message without distinguishing "the connection broke" from
/// "the daemon refused" — both are user-visible failures from the
/// TUI's perspective.
pub async fn submit_intent(home: &Path, text: &str) -> Result<SubmissionOutcome, IpcError> {
    let (mut stream, _sock) = connect_socket(home).await?;
    let token_b58 = read_operator_token(home).await?;
    write_frame(&mut stream, &Request::Authenticate { token_b58 }).await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::Authenticated { .. } => {}
        Response::AuthenticationFailed { reason } => {
            return Ok(SubmissionOutcome::Failed {
                message: format!("authentication failed: {reason}"),
            });
        }
        other => {
            return Ok(SubmissionOutcome::Failed {
                message: format!("unexpected response to authenticate: {other:?}"),
            });
        }
    }
    write_frame(
        &mut stream,
        &Request::SubmitIntent {
            text: text.to_string(),
        },
    )
    .await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::IntentResult {
            intent_id,
            status,
            text,
            ..
        } => Ok(SubmissionOutcome::Accepted {
            intent_id,
            status,
            text,
        }),
        Response::Error { message } => Ok(SubmissionOutcome::Failed { message }),
        other => Ok(SubmissionOutcome::Failed {
            message: format!("unexpected response: {other:?}"),
        }),
    }
}

/// Connects to `$COVENANT_HOME/sock`, authenticates with the operator
/// token, sends `Request::GrantCapability`, and maps the daemon
/// response into a [`GrantOutcome`].
///
/// `scope = None` produces an unscoped grant. `expires_at` is a
/// best-effort epoch-ms hint; the daemon enforces.
pub async fn grant_capability(
    home: &Path,
    action: &str,
    scope: Option<serde_json::Value>,
    expires_at: Option<u64>,
) -> Result<GrantOutcome, IpcError> {
    let (mut stream, _sock) = connect_socket(home).await?;
    let token_b58 = read_operator_token(home).await?;
    write_frame(&mut stream, &Request::Authenticate { token_b58 }).await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::Authenticated { .. } => {}
        Response::AuthenticationFailed { reason } => {
            return Ok(GrantOutcome::Failed {
                message: format!("authentication failed: {reason}"),
            });
        }
        other => {
            return Ok(GrantOutcome::Failed {
                message: format!("unexpected response to authenticate: {other:?}"),
            });
        }
    }
    write_frame(
        &mut stream,
        &Request::GrantCapability {
            action: action.to_string(),
            scope,
            expires_at,
        },
    )
    .await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::CapabilityGranted {
            signature_b58,
            subject_display,
            action,
        } => Ok(GrantOutcome::Granted {
            signature_b58,
            subject_display,
            action,
        }),
        Response::Error { message } => Ok(GrantOutcome::Failed { message }),
        other => Ok(GrantOutcome::Failed {
            message: format!("unexpected response: {other:?}"),
        }),
    }
}

/// Connects to `$COVENANT_HOME/sock`, authenticates with the operator
/// token, sends `Request::RecentMemory`, and maps the daemon
/// response into a [`MemoryFetchOutcome`].
///
/// `tier = None` fetches across every tier the operator can read.
/// `limit` is clamped to [`RECENT_MEMORY_LIMIT_CAP`] so a runaway
/// request cannot ask the daemon for an unbounded scan.
pub async fn recent_memory(
    home: &Path,
    tier: Option<MemoryTier>,
    limit: usize,
) -> Result<MemoryFetchOutcome, IpcError> {
    let (mut stream, _sock) = connect_socket(home).await?;
    let token_b58 = read_operator_token(home).await?;
    write_frame(&mut stream, &Request::Authenticate { token_b58 }).await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::Authenticated { .. } => {}
        Response::AuthenticationFailed { reason } => {
            return Ok(MemoryFetchOutcome::Failed {
                message: format!("authentication failed: {reason}"),
            });
        }
        other => {
            return Ok(MemoryFetchOutcome::Failed {
                message: format!("unexpected response to authenticate: {other:?}"),
            });
        }
    }
    let limit = limit.min(RECENT_MEMORY_LIMIT_CAP);
    write_frame(&mut stream, &Request::RecentMemory { tier, limit }).await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::Memories { records } => Ok(MemoryFetchOutcome::Fetched { records }),
        Response::Error { message } => Ok(MemoryFetchOutcome::Failed { message }),
        other => Ok(MemoryFetchOutcome::Failed {
            message: format!("unexpected response: {other:?}"),
        }),
    }
}

/// Connects to `$COVENANT_HOME/sock`, authenticates with the operator
/// token, sends `Request::RecentAudit`, and maps the daemon response
/// into an [`AuditFetchOutcome`]. The daemon filters audit rows to
/// `issuer.pubkey == peer.pubkey` so the operator only sees their
/// own activity.
///
/// `limit` is clamped to [`RECENT_AUDIT_LIMIT_CAP`].
pub async fn recent_audit(home: &Path, limit: usize) -> Result<AuditFetchOutcome, IpcError> {
    let (mut stream, _sock) = connect_socket(home).await?;
    let token_b58 = read_operator_token(home).await?;
    write_frame(&mut stream, &Request::Authenticate { token_b58 }).await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::Authenticated { .. } => {}
        Response::AuthenticationFailed { reason } => {
            return Ok(AuditFetchOutcome::Failed {
                message: format!("authentication failed: {reason}"),
            });
        }
        other => {
            return Ok(AuditFetchOutcome::Failed {
                message: format!("unexpected response to authenticate: {other:?}"),
            });
        }
    }
    let limit = limit.min(RECENT_AUDIT_LIMIT_CAP);
    write_frame(&mut stream, &Request::RecentAudit { limit }).await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::AuditEvents { events } => Ok(AuditFetchOutcome::Fetched { events }),
        Response::Error { message } => Ok(AuditFetchOutcome::Failed { message }),
        other => Ok(AuditFetchOutcome::Failed {
            message: format!("unexpected response: {other:?}"),
        }),
    }
}

/// Connects to `$COVENANT_HOME/sock`, authenticates with the operator
/// token, sends `Request::RecentCapabilities`, and maps the daemon
/// response into a [`CapabilitiesFetchOutcome`].
///
/// `limit` is clamped to [`RECENT_CAPABILITIES_LIMIT_CAP`].
pub async fn recent_capabilities(
    home: &Path,
    limit: usize,
) -> Result<CapabilitiesFetchOutcome, IpcError> {
    let (mut stream, _sock) = connect_socket(home).await?;
    let token_b58 = read_operator_token(home).await?;
    write_frame(&mut stream, &Request::Authenticate { token_b58 }).await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::Authenticated { .. } => {}
        Response::AuthenticationFailed { reason } => {
            return Ok(CapabilitiesFetchOutcome::Failed {
                message: format!("authentication failed: {reason}"),
            });
        }
        other => {
            return Ok(CapabilitiesFetchOutcome::Failed {
                message: format!("unexpected response to authenticate: {other:?}"),
            });
        }
    }
    let limit = limit.min(RECENT_CAPABILITIES_LIMIT_CAP);
    write_frame(&mut stream, &Request::RecentCapabilities { limit }).await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::Capabilities { capabilities } => {
            Ok(CapabilitiesFetchOutcome::Fetched { capabilities })
        }
        Response::Error { message } => Ok(CapabilitiesFetchOutcome::Failed { message }),
        other => Ok(CapabilitiesFetchOutcome::Failed {
            message: format!("unexpected response: {other:?}"),
        }),
    }
}

/// Connects to `$COVENANT_HOME/sock`, authenticates with the operator
/// token, sends `Request::RecentA2ATasks`, and maps the daemon
/// response into an [`A2aFetchOutcome`]. The daemon filters tasks to
/// rows where the operator is sender or recipient.
///
/// `limit` is clamped to [`RECENT_A2A_LIMIT_CAP`].
pub async fn recent_a2a_tasks(home: &Path, limit: usize) -> Result<A2aFetchOutcome, IpcError> {
    let (mut stream, _sock) = connect_socket(home).await?;
    let token_b58 = read_operator_token(home).await?;
    write_frame(&mut stream, &Request::Authenticate { token_b58 }).await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::Authenticated { .. } => {}
        Response::AuthenticationFailed { reason } => {
            return Ok(A2aFetchOutcome::Failed {
                message: format!("authentication failed: {reason}"),
            });
        }
        other => {
            return Ok(A2aFetchOutcome::Failed {
                message: format!("unexpected response to authenticate: {other:?}"),
            });
        }
    }
    let limit = limit.min(RECENT_A2A_LIMIT_CAP);
    write_frame(&mut stream, &Request::RecentA2ATasks { limit }).await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::A2ATasks { tasks } => Ok(A2aFetchOutcome::Fetched { tasks }),
        Response::Error { message } => Ok(A2aFetchOutcome::Failed { message }),
        other => Ok(A2aFetchOutcome::Failed {
            message: format!("unexpected response: {other:?}"),
        }),
    }
}

/// Connects to `$COVENANT_HOME/sock`, authenticates with the operator
/// token, sends `Request::RecentReceipts`, and maps the daemon
/// response into a [`ReceiptsFetchOutcome`]. The daemon gates this on
/// the `chain.receipts` capability and filters server-side to rows
/// where the payer matches the calling peer.
///
/// `limit` is clamped to [`RECENT_RECEIPTS_LIMIT_CAP`].
pub async fn recent_receipts(home: &Path, limit: usize) -> Result<ReceiptsFetchOutcome, IpcError> {
    let (mut stream, _sock) = connect_socket(home).await?;
    let token_b58 = read_operator_token(home).await?;
    write_frame(&mut stream, &Request::Authenticate { token_b58 }).await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::Authenticated { .. } => {}
        Response::AuthenticationFailed { reason } => {
            return Ok(ReceiptsFetchOutcome::Failed {
                message: format!("authentication failed: {reason}"),
            });
        }
        other => {
            return Ok(ReceiptsFetchOutcome::Failed {
                message: format!("unexpected response to authenticate: {other:?}"),
            });
        }
    }
    let limit = limit.min(RECENT_RECEIPTS_LIMIT_CAP);
    write_frame(&mut stream, &Request::RecentReceipts { limit }).await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::Receipts { receipts } => Ok(ReceiptsFetchOutcome::Fetched { receipts }),
        Response::Error { message } => Ok(ReceiptsFetchOutcome::Failed { message }),
        other => Ok(ReceiptsFetchOutcome::Failed {
            message: format!("unexpected response: {other:?}"),
        }),
    }
}
