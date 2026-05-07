//! Length-prefixed JSON IPC for the covenant daemon and CLI.
//!
//! Wire format: 4-byte big-endian length, then that many bytes of JSON.
//! Frames over [`MAX_FRAME`] bytes are rejected on the read side.

#![deny(unsafe_code)]

use covenant_a2a::{A2ATask, A2ATaskResult};
use covenant_audit::AuditEvent;
use covenant_mcp::{Content, ToolSpec};
use covenant_permissions::SignedCapability;
use covenant_types::{MemoryRecord, MemoryTier, SettlementReceipt};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifyCheck {
    pub name: String,
    pub passed: bool,
    pub message: String,
}
// `Receipts` mirrors `Memories`: a list of `SettlementReceipt`. Kept as a
// distinct response variant so the CLI can format them differently.
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

pub const MAX_FRAME: u32 = 8 * 1024 * 1024;

// `PartialEq` only — `A2ATaskResult` (carried in `PostA2AResult`) holds a
// `serde_json::Value` which isn't `Eq`. Symmetric with `Response`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    Ping,
    SubmitIntent {
        text: String,
    },
    RecentMemory {
        #[serde(default)]
        tier: Option<MemoryTier>,
        #[serde(default = "default_recent_limit")]
        limit: usize,
    },
    RecentReceipts {
        #[serde(default = "default_recent_limit")]
        limit: usize,
    },
    SearchMemory {
        query: String,
        #[serde(default)]
        tier: Option<MemoryTier>,
        #[serde(default = "default_recent_limit")]
        limit: usize,
    },
    PurgeMemory {
        #[serde(default)]
        tier: Option<MemoryTier>,
        before_ms: u64,
    },
    RecentCapabilities {
        #[serde(default = "default_recent_limit")]
        limit: usize,
    },
    GrantCapability {
        action: String,
        #[serde(default)]
        scope: Option<serde_json::Value>,
        #[serde(default)]
        expires_at: Option<u64>,
    },
    RevokeCapability {
        signature_b58: String,
    },
    Verify {
        #[serde(default = "default_verify_window")]
        window: usize,
    },
    IgnoreCheck {
        text: String,
    },
    ListTools,
    CallTool {
        name: String,
        #[serde(default)]
        arguments: serde_json::Value,
    },
    RecentAudit {
        #[serde(default = "default_recent_limit")]
        limit: usize,
    },
    SendA2ATask {
        task: A2ATask,
    },
    TryRecvA2ATask,
    PostA2AResult {
        result: A2ATaskResult,
    },
    TryRecvA2AResult,
    RecentA2ATasks {
        #[serde(default = "default_recent_limit")]
        limit: usize,
    },
    RecentA2AResults {
        #[serde(default = "default_recent_limit")]
        limit: usize,
    },
}

fn default_recent_limit() -> usize {
    10
}

fn default_verify_window() -> usize {
    100
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Pong,
    IntentResult {
        intent_id: Uuid,
        status: String,
        text: String,
        sources: Vec<String>,
        settlement: Option<SettlementReceipt>,
    },
    Memories {
        records: Vec<MemoryRecord>,
    },
    MemoryPurged {
        purged: u64,
    },
    Receipts {
        receipts: Vec<SettlementReceipt>,
    },
    VerifyReport {
        window: usize,
        checks: Vec<VerifyCheck>,
        orphans_total: u64,
    },
    Capabilities {
        capabilities: Vec<SignedCapability>,
    },
    CapabilityGranted {
        signature_b58: String,
        subject_display: String,
        action: String,
    },
    CapabilityRevoked {
        signature_b58: String,
        removed: bool,
    },
    IgnoreReport {
        ignored: bool,
        matched_pattern: Option<String>,
        rules_loaded: usize,
    },
    ToolList {
        tools: Vec<ToolSpec>,
    },
    ToolResult {
        content: Vec<Content>,
        is_error: bool,
    },
    AuditEvents {
        events: Vec<AuditEvent>,
    },
    A2ATaskQueued {
        task_id: Uuid,
    },
    A2ATaskOpt {
        task: Option<A2ATask>,
    },
    A2AResultPosted {
        task_id: Uuid,
    },
    A2AResultOpt {
        result: Option<A2ATaskResult>,
    },
    A2ATasks {
        tasks: Vec<A2ATask>,
    },
    A2AResults {
        results: Vec<A2ATaskResult>,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("frame too large: {got} bytes (max {MAX_FRAME})")]
    FrameTooLarge { got: u32 },
}

pub async fn read_frame<R, T>(reader: &mut R) -> Result<T, IpcError>
where
    R: AsyncReadExt + Unpin,
    T: serde::de::DeserializeOwned,
{
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME {
        return Err(IpcError::FrameTooLarge { got: len });
    }
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await?;
    Ok(serde_json::from_slice(&buf)?)
}

pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<(), IpcError>
where
    W: AsyncWriteExt + Unpin,
    T: serde::Serialize,
{
    let payload = serde_json::to_vec(value)?;
    let len =
        u32::try_from(payload.len()).map_err(|_| IpcError::FrameTooLarge { got: u32::MAX })?;
    if len > MAX_FRAME {
        return Err(IpcError::FrameTooLarge { got: len });
    }
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn request_roundtrip_via_pipe() {
        let (mut a, mut b) = tokio::io::duplex(8192);
        let req = Request::SubmitIntent {
            text: "hello".into(),
        };
        write_frame(&mut a, &req).await.unwrap();
        let got: Request = read_frame(&mut b).await.unwrap();
        assert_eq!(got, req);
    }

    #[tokio::test]
    async fn ping_pong_via_pipe() {
        let (mut a, mut b) = tokio::io::duplex(64);
        write_frame(&mut a, &Request::Ping).await.unwrap();
        let got: Request = read_frame(&mut b).await.unwrap();
        assert_eq!(got, Request::Ping);
    }

    #[tokio::test]
    async fn recent_memory_request_uses_default_limit() {
        let json = r#"{"kind":"recent_memory"}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        match req {
            Request::RecentMemory { tier, limit } => {
                assert!(tier.is_none());
                assert_eq!(limit, 10);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn intent_result_serialises_settlement_null() {
        let r = Response::IntentResult {
            intent_id: Uuid::nil(),
            status: "ok".into(),
            text: "echo".into(),
            sources: vec![],
            settlement: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"settlement\":null"));
        assert!(json.contains("\"kind\":\"intent_result\""));
    }

    #[tokio::test]
    async fn rejects_oversized_frame_header() {
        let (mut a, mut b) = tokio::io::duplex(64);
        let bad_len = (MAX_FRAME + 1).to_be_bytes();
        a.write_all(&bad_len).await.unwrap();
        let r: Result<Request, _> = read_frame(&mut b).await;
        assert!(matches!(r, Err(IpcError::FrameTooLarge { .. })));
    }
}
