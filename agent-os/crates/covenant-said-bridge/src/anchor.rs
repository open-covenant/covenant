//! Anchor pipeline.
//!
//! `Fixture` mode appends the payload to `anchor_pending.jsonl` and
//! never spends SOL. `Live` mode invokes the worker's `submit-anchor`
//! command and requires `COVENANT_SAID_ALLOW_PAID_ANCHOR=1`.
//!
//! `start_seq` and `end_seq` are audit-chain indices, not settlement
//! batch IDs.

use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::client::SaidBridge;
use crate::cursor::{AnchorCursor, PendingAnchor};
use crate::{worker, BridgeError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnchorRequest {
    pub start_audit_index: u64,
    pub end_audit_index: u64,
    pub merkle_root_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnchorSubmission {
    pub anchor_index: u64,
    pub start_seq: u64,
    pub end_seq: u64,
    pub merkle_root_hex: String,
    pub tx_sig: String,
    pub slot: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorMode {
    /// Write the would-be payload to `anchor_pending.jsonl`. No SOL spent.
    Fixture,
    /// Drive the TS worker's `submit-anchor` command on devnet or mainnet.
    Live,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn validate_root_hex(hex: &str) -> Result<()> {
    if hex.len() != 64 {
        return Err(BridgeError::Invalid(format!(
            "merkle_root_hex must be 64 chars, got {}",
            hex.len()
        )));
    }
    if !hex
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(BridgeError::Invalid(
            "merkle_root_hex must be lowercase ASCII hex".into(),
        ));
    }
    Ok(())
}

fn validate_range(req: &AnchorRequest) -> Result<()> {
    if req.end_audit_index < req.start_audit_index {
        return Err(BridgeError::Invalid(format!(
            "end_audit_index {} < start_audit_index {}",
            req.end_audit_index, req.start_audit_index
        )));
    }
    validate_root_hex(&req.merkle_root_hex)
}

impl SaidBridge {
    pub async fn anchor(
        &self,
        cursor: &AnchorCursor,
        fixture_path: &PathBuf,
        mode: AnchorMode,
        request: AnchorRequest,
    ) -> Result<AnchorSubmission> {
        validate_range(&request)?;
        match mode {
            AnchorMode::Fixture => {
                self.require_enabled()?;
                self.anchor_fixture(cursor, fixture_path, request).await
            }
            AnchorMode::Live => {
                self.require_paid("submit_anchor", self.config().paid.anchor)?;
                self.anchor_live(cursor, request).await
            }
        }
    }

    async fn anchor_fixture(
        &self,
        cursor: &AnchorCursor,
        fixture_path: &PathBuf,
        request: AnchorRequest,
    ) -> Result<AnchorSubmission> {
        let anchor_index = cursor.next_index()?;
        let pending = PendingAnchor {
            anchor_index,
            start_audit_index: request.start_audit_index,
            end_audit_index: request.end_audit_index,
            merkle_root_hex: request.merkle_root_hex.clone(),
        };
        cursor.claim(&pending)?;

        if let Some(parent) = fixture_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    BridgeError::Invalid(format!("mkdir {}: {e}", parent.display()))
                })?;
            }
        }

        let line = serde_json::to_string(&serde_json::json!({
            "anchor_index": anchor_index,
            "start_seq": request.start_audit_index,
            "end_seq": request.end_audit_index,
            "merkle_root_hex": request.merkle_root_hex,
            "stamped_at_ms": now_ms(),
        }))
        .map_err(|e| BridgeError::Invalid(e.to_string()))?;

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(fixture_path)
            .await
            .map_err(|e| BridgeError::Invalid(format!("open {}: {e}", fixture_path.display())))?;
        file.write_all(line.as_bytes())
            .await
            .map_err(|e| BridgeError::Invalid(format!("write {}: {e}", fixture_path.display())))?;
        file.write_all(b"\n")
            .await
            .map_err(|e| BridgeError::Invalid(format!("write {}: {e}", fixture_path.display())))?;
        file.flush()
            .await
            .map_err(|e| BridgeError::Invalid(format!("flush {}: {e}", fixture_path.display())))?;

        let tx_sig = format!("fixture:{anchor_index}");
        cursor.confirm(anchor_index, &tx_sig, 0, now_ms())?;

        Ok(AnchorSubmission {
            anchor_index,
            start_seq: request.start_audit_index,
            end_seq: request.end_audit_index,
            merkle_root_hex: request.merkle_root_hex,
            tx_sig,
            slot: 0,
        })
    }

    async fn anchor_live(
        &self,
        cursor: &AnchorCursor,
        request: AnchorRequest,
    ) -> Result<AnchorSubmission> {
        let anchor_index = cursor.next_index()?;
        let pending = PendingAnchor {
            anchor_index,
            start_audit_index: request.start_audit_index,
            end_audit_index: request.end_audit_index,
            merkle_root_hex: request.merkle_root_hex.clone(),
        };
        cursor.claim(&pending)?;

        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct WorkerPayload<'a> {
            anchor_index: u64,
            start_seq: u64,
            end_seq: u64,
            merkle_root_hex: &'a str,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WorkerResult {
            tx_sig: String,
            #[serde(default)]
            slot: u64,
        }

        let payload = WorkerPayload {
            anchor_index,
            start_seq: request.start_audit_index,
            end_seq: request.end_audit_index,
            merkle_root_hex: &request.merkle_root_hex,
        };
        let result: WorkerResult = worker::invoke(self.config(), "submit-anchor", &payload).await?;

        cursor.confirm(anchor_index, &result.tx_sig, result.slot, now_ms())?;

        Ok(AnchorSubmission {
            anchor_index,
            start_seq: request.start_audit_index,
            end_seq: request.end_audit_index,
            merkle_root_hex: request.merkle_root_hex,
            tx_sig: result.tx_sig,
            slot: result.slot,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Cluster, Config};
    use tempfile::tempdir;

    fn bridge_enabled() -> SaidBridge {
        let mut cfg = Config::disabled(Cluster::Devnet);
        cfg.enabled = true;
        SaidBridge::new(cfg).unwrap()
    }

    /// Enabled bridge with the anchor paid gate open and a shell-stub
    /// worker that drains stdin and prints `envelope_json` verbatim.
    fn bridge_with_anchor_worker(envelope_json: &str) -> SaidBridge {
        let mut cfg = Config::disabled(Cluster::Devnet);
        cfg.enabled = true;
        cfg.paid.anchor = true;
        let script = format!("cat >/dev/null; printf '%s' '{envelope_json}'");
        cfg.worker_command = vec!["sh".into(), "-c".into(), script];
        SaidBridge::new(cfg).unwrap()
    }

    #[tokio::test]
    async fn fixture_anchor_advances_cursor_and_writes_file() {
        let dir = tempdir().unwrap();
        let fixture_path = dir.path().join("anchor_pending.jsonl");
        let cursor = AnchorCursor::open(dir.path().join("cursor.db")).unwrap();
        let bridge = bridge_enabled();

        let request = AnchorRequest {
            start_audit_index: 0,
            end_audit_index: 7,
            merkle_root_hex: "ab".repeat(32),
        };

        let result = bridge
            .anchor(&cursor, &fixture_path, AnchorMode::Fixture, request.clone())
            .await
            .unwrap();
        assert_eq!(result.anchor_index, 0);
        assert!(result.tx_sig.starts_with("fixture:"));

        let next = bridge
            .anchor(&cursor, &fixture_path, AnchorMode::Fixture, request)
            .await
            .unwrap();
        assert_eq!(next.anchor_index, 1);

        let contents = tokio::fs::read_to_string(&fixture_path).await.unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
    }

    #[tokio::test]
    async fn anchor_live_requires_paid_gate() {
        let dir = tempdir().unwrap();
        let fixture_path = dir.path().join("ignored.jsonl");
        let cursor = AnchorCursor::in_memory().unwrap();
        let bridge = bridge_enabled();
        let request = AnchorRequest {
            start_audit_index: 0,
            end_audit_index: 7,
            merkle_root_hex: "ab".repeat(32),
        };
        let err = bridge
            .anchor(&cursor, &fixture_path, AnchorMode::Live, request)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            BridgeError::PaidGateClosed {
                instruction: "submit_anchor"
            }
        ));
    }

    #[tokio::test]
    async fn anchor_live_maps_worker_success_and_confirms_cursor() {
        let dir = tempdir().unwrap();
        let fixture_path = dir.path().join("ignored.jsonl");
        let cursor = AnchorCursor::in_memory().unwrap();
        let bridge =
            bridge_with_anchor_worker(r#"{"ok":true,"data":{"txSig":"livesig","slot":777}}"#);
        let request = AnchorRequest {
            start_audit_index: 3,
            end_audit_index: 9,
            merkle_root_hex: "ab".repeat(32),
        };

        let result = bridge
            .anchor(&cursor, &fixture_path, AnchorMode::Live, request)
            .await
            .unwrap();
        assert_eq!(result.anchor_index, 0);
        assert_eq!(result.start_seq, 3);
        assert_eq!(result.end_seq, 9);
        assert_eq!(result.tx_sig, "livesig");
        assert_eq!(result.slot, 777);

        // The worker-success path must persist the confirmation, not just
        // return it: the slot leaves pending and the cursor advances.
        assert_eq!(cursor.last_confirmed_index().unwrap(), Some(0));
        assert!(cursor.pending().unwrap().is_empty());
        assert_eq!(cursor.next_index().unwrap(), 1);
        let recent = cursor.recent(10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].tx_sig, "livesig");
        assert_eq!(recent[0].slot, 777);
    }

    #[tokio::test]
    async fn anchor_live_worker_failure_leaves_claim_pending() {
        let dir = tempdir().unwrap();
        let fixture_path = dir.path().join("ignored.jsonl");
        let cursor = AnchorCursor::in_memory().unwrap();
        let bridge =
            bridge_with_anchor_worker(r#"{"ok":false,"error":"rpc down","name":"SendError"}"#);
        let request = AnchorRequest {
            start_audit_index: 0,
            end_audit_index: 4,
            merkle_root_hex: "ab".repeat(32),
        };

        let err = bridge
            .anchor(&cursor, &fixture_path, AnchorMode::Live, request)
            .await
            .unwrap_err();
        assert!(matches!(err, BridgeError::Upstream { .. }));

        // Claim-before-submit means a failed submit keeps the index
        // reserved (pending, unconfirmed) so a retry cannot silently
        // reuse it against a stale on-chain last_anchor_index.
        assert_eq!(cursor.last_confirmed_index().unwrap(), None);
        let pending = cursor.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].anchor_index, 0);
    }

    #[test]
    fn validate_range_rejects_inverted_or_bad_hex() {
        let bad = AnchorRequest {
            start_audit_index: 10,
            end_audit_index: 5,
            merkle_root_hex: "ab".repeat(32),
        };
        assert!(matches!(validate_range(&bad), Err(BridgeError::Invalid(_))));

        let bad_hex = AnchorRequest {
            start_audit_index: 0,
            end_audit_index: 5,
            merkle_root_hex: "zz".repeat(32),
        };
        assert!(matches!(
            validate_range(&bad_hex),
            Err(BridgeError::Invalid(_))
        ));
    }
}
