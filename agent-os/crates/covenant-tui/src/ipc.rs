//! IPC client used by the TUI binary and exercised by the live test
//! suite. Connects to a running `covenantd` over the local Unix
//! socket, authenticates with the operator token, and submits an
//! intent.
//!
//! Kept separate from the App state machine so the state machine
//! stays terminal-and-socket free for unit tests, and the IPC path
//! is in turn reachable from `tests/` for live coverage.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use covenant_audit::AuditEvent;
use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_types::{MemoryRecord, MemoryTier};
use tokio::net::UnixStream;

use crate::SubmissionOutcome;

/// Hard cap on `Request::RecentMemory::limit` to prevent a runaway
/// request from asking the daemon to enumerate the entire memory
/// table into a single IPC frame.
pub const RECENT_MEMORY_LIMIT_CAP: usize = 50;

/// Hard cap on `Request::RecentAudit::limit`. Audit volume can grow
/// faster than memory, so the cap is set higher.
pub const RECENT_AUDIT_LIMIT_CAP: usize = 100;

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

/// Resolves `$COVENANT_HOME` with the same fallback shape as the
/// existing `covenant` CLI: explicit env var first, then
/// `$HOME/.covenant`.
pub fn covenant_home() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("COVENANT_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".covenant"))
}

/// Reads the operator bootstrap token from `$COVENANT_HOME/peers/operator.token`.
/// The daemon mints the token on first boot at mode 0600.
pub async fn read_operator_token(home: &Path) -> Result<String> {
    let path = home.join("peers").join("operator.token");
    let raw = tokio::fs::read_to_string(&path).await.with_context(|| {
        format!(
            "read operator token at {} (is covenantd running?)",
            path.display()
        )
    })?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!(
            "operator token at {} is empty",
            path.display()
        ));
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
pub async fn submit_intent(home: &Path, text: &str) -> Result<SubmissionOutcome> {
    let sock = home.join("sock");
    let mut stream = UnixStream::connect(&sock).await.with_context(|| {
        format!(
            "connect to daemon at {} (is covenantd running?)",
            sock.display()
        )
    })?;
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
) -> Result<GrantOutcome> {
    let sock = home.join("sock");
    let mut stream = UnixStream::connect(&sock).await.with_context(|| {
        format!(
            "connect to daemon at {} (is covenantd running?)",
            sock.display()
        )
    })?;
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
) -> Result<MemoryFetchOutcome> {
    let sock = home.join("sock");
    let mut stream = UnixStream::connect(&sock).await.with_context(|| {
        format!(
            "connect to daemon at {} (is covenantd running?)",
            sock.display()
        )
    })?;
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
pub async fn recent_audit(home: &Path, limit: usize) -> Result<AuditFetchOutcome> {
    let sock = home.join("sock");
    let mut stream = UnixStream::connect(&sock).await.with_context(|| {
        format!(
            "connect to daemon at {} (is covenantd running?)",
            sock.display()
        )
    })?;
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
