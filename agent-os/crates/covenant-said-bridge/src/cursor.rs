//! SQLite-backed anchor cursor. `submit_anchor` requires
//! `anchor_index = AgentIdentity.last_anchor_index + 1`, so the cursor
//! persists every claim before submit and the confirmation after.

use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use rusqlite::{params, Connection};

use crate::{BridgeError, Result};

pub struct AnchorCursor {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAnchor {
    pub anchor_index: u64,
    pub start_audit_index: u64,
    pub end_audit_index: u64,
    pub merkle_root_hex: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedAnchor {
    pub anchor_index: u64,
    pub start_audit_index: u64,
    pub end_audit_index: u64,
    pub merkle_root_hex: String,
    pub tx_sig: String,
    pub slot: u64,
    pub submitted_at_ms: i64,
}

impl AnchorCursor {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| BridgeError::Invalid(format!("mkdir {}: {e}", parent.display())))?;
            }
        }
        let conn = Connection::open(path.as_ref())
            .map_err(|e| BridgeError::Invalid(format!("open cursor db: {e}")))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS said_anchor_cursor (
                anchor_index INTEGER PRIMARY KEY,
                start_audit_index INTEGER NOT NULL,
                end_audit_index INTEGER NOT NULL,
                merkle_root TEXT NOT NULL,
                tx_sig TEXT,
                slot INTEGER,
                submitted_at_ms INTEGER
            );",
        )
        .map_err(|e| BridgeError::Invalid(format!("init schema: {e}")))?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn in_memory() -> Result<Self> {
        Self::open(PathBuf::from(":memory:"))
    }

    /// The next anchor index to claim. Returns 0 when the cursor is empty.
    pub fn next_index(&self) -> Result<u64> {
        let max: Option<i64> = self
            .conn
            .lock()
            .query_row(
                "SELECT MAX(anchor_index) FROM said_anchor_cursor",
                [],
                |row| row.get(0),
            )
            .map_err(|e| BridgeError::Invalid(format!("read max: {e}")))?;
        Ok(max.map(|v| (v as u64) + 1).unwrap_or(0))
    }

    /// The highest confirmed anchor index, or None if no anchors are
    /// confirmed yet (regardless of pending rows).
    pub fn last_confirmed_index(&self) -> Result<Option<u64>> {
        let max: Option<i64> = self
            .conn
            .lock()
            .query_row(
                "SELECT MAX(anchor_index) FROM said_anchor_cursor WHERE tx_sig IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .map_err(|e| BridgeError::Invalid(format!("read confirmed: {e}")))?;
        Ok(max.map(|v| v as u64))
    }

    /// Insert a claimed slot before submitting. Conflicts on the
    /// `anchor_index` PK so a duplicate claim is impossible.
    pub fn claim(&self, anchor: &PendingAnchor) -> Result<()> {
        self.conn
            .lock()
            .execute(
                "INSERT INTO said_anchor_cursor (anchor_index, start_audit_index, end_audit_index, merkle_root) VALUES (?1, ?2, ?3, ?4)",
                params![anchor.anchor_index as i64, anchor.start_audit_index as i64, anchor.end_audit_index as i64, anchor.merkle_root_hex],
            )
            .map(|_| ())
            .map_err(|e| BridgeError::Invalid(format!("claim slot {}: {e}", anchor.anchor_index)))
    }

    pub fn confirm(&self, anchor_index: u64, tx_sig: &str, slot: u64, submitted_at_ms: i64) -> Result<()> {
        let rows = self
            .conn
            .lock()
            .execute(
                "UPDATE said_anchor_cursor SET tx_sig = ?1, slot = ?2, submitted_at_ms = ?3 WHERE anchor_index = ?4",
                params![tx_sig, slot as i64, submitted_at_ms, anchor_index as i64],
            )
            .map_err(|e| BridgeError::Invalid(format!("confirm {anchor_index}: {e}")))?;
        if rows == 0 {
            return Err(BridgeError::Invalid(format!(
                "confirm: no row at anchor_index {anchor_index}"
            )));
        }
        Ok(())
    }

    pub fn pending(&self) -> Result<Vec<PendingAnchor>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT anchor_index, start_audit_index, end_audit_index, merkle_root FROM said_anchor_cursor WHERE tx_sig IS NULL ORDER BY anchor_index",
            )
            .map_err(|e| BridgeError::Invalid(format!("prepare pending: {e}")))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(PendingAnchor {
                    anchor_index: row.get::<_, i64>(0)? as u64,
                    start_audit_index: row.get::<_, i64>(1)? as u64,
                    end_audit_index: row.get::<_, i64>(2)? as u64,
                    merkle_root_hex: row.get(3)?,
                })
            })
            .map_err(|e| BridgeError::Invalid(format!("query pending: {e}")))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| BridgeError::Invalid(format!("collect pending: {e}")))
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<ConfirmedAnchor>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT anchor_index, start_audit_index, end_audit_index, merkle_root, tx_sig, slot, submitted_at_ms FROM said_anchor_cursor WHERE tx_sig IS NOT NULL ORDER BY anchor_index DESC LIMIT ?1",
            )
            .map_err(|e| BridgeError::Invalid(format!("prepare recent: {e}")))?;
        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(ConfirmedAnchor {
                    anchor_index: row.get::<_, i64>(0)? as u64,
                    start_audit_index: row.get::<_, i64>(1)? as u64,
                    end_audit_index: row.get::<_, i64>(2)? as u64,
                    merkle_root_hex: row.get(3)?,
                    tx_sig: row.get(4)?,
                    slot: row.get::<_, i64>(5)? as u64,
                    submitted_at_ms: row.get(6)?,
                })
            })
            .map_err(|e| BridgeError::Invalid(format!("query recent: {e}")))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| BridgeError::Invalid(format!("collect recent: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_cursor_starts_at_zero() {
        let c = AnchorCursor::in_memory().unwrap();
        assert_eq!(c.next_index().unwrap(), 0);
        assert_eq!(c.last_confirmed_index().unwrap(), None);
        assert!(c.pending().unwrap().is_empty());
    }

    #[test]
    fn claim_then_confirm_advances_cursor() {
        let c = AnchorCursor::in_memory().unwrap();
        let a = PendingAnchor {
            anchor_index: 0,
            start_audit_index: 0,
            end_audit_index: 10,
            merkle_root_hex: "ab".repeat(32),
        };
        c.claim(&a).unwrap();
        assert_eq!(c.next_index().unwrap(), 1);
        assert_eq!(c.last_confirmed_index().unwrap(), None);
        assert_eq!(c.pending().unwrap().len(), 1);

        c.confirm(0, "SIG", 12345, 1_700_000_000_000).unwrap();
        assert_eq!(c.last_confirmed_index().unwrap(), Some(0));
        assert!(c.pending().unwrap().is_empty());
        let recent = c.recent(10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].tx_sig, "SIG");
        assert_eq!(recent[0].slot, 12345);
    }

    #[test]
    fn duplicate_claim_rejected() {
        let c = AnchorCursor::in_memory().unwrap();
        let a = PendingAnchor {
            anchor_index: 0,
            start_audit_index: 0,
            end_audit_index: 10,
            merkle_root_hex: "00".repeat(32),
        };
        c.claim(&a).unwrap();
        let err = c.claim(&a).unwrap_err();
        assert!(matches!(err, BridgeError::Invalid(_)));
    }

    #[test]
    fn confirm_missing_slot_errors() {
        let c = AnchorCursor::in_memory().unwrap();
        let err = c.confirm(42, "SIG", 1, 1).unwrap_err();
        assert!(matches!(err, BridgeError::Invalid(m) if m.contains("no row")));
    }

    #[test]
    fn pending_only_lists_unconfirmed() {
        let c = AnchorCursor::in_memory().unwrap();
        for i in 0..3 {
            c.claim(&PendingAnchor {
                anchor_index: i,
                start_audit_index: i * 10,
                end_audit_index: i * 10 + 9,
                merkle_root_hex: "cd".repeat(32),
            })
            .unwrap();
        }
        c.confirm(0, "S0", 1, 1).unwrap();
        c.confirm(2, "S2", 3, 3).unwrap();
        let pending = c.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].anchor_index, 1);
    }
}
