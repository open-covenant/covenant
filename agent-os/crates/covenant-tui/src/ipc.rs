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
use covenant_ipc::{read_frame, write_frame, Request, Response};
use tokio::net::UnixStream;

use crate::SubmissionOutcome;

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
