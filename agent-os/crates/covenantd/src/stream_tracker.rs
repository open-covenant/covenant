//! In-flight IPC v2 streaming-response tracker (ADR 0010 slice 2).
//!
//! ADR 0010 introduces a v2 streaming envelope: a `stream_begin` /
//! `stream_chunk` / `stream_end | stream_error` sequence over a single
//! IPC connection. The daemon needs a single place to record which
//! streams are currently open so an operator-facing snapshot can read
//! "what is in flight" and so connection close can deterministically
//! purge entries the daemon-side dispatch left behind.
//!
//! `Server::serve` allocates a `connection_id` per accepted connection
//! and `Server::handle`'s `PurgeOnDrop` guard calls `purge_connection`
//! on connection close. The three streaming dispatch forks (ADR 0010
//! slices 3.d–5.d) register an entry when they open a stream and
//! unregister it on stream end or error, so a client that disconnects
//! mid-stream is cleaned up by the guard rather than leaking an entry.
//!
//! Keys are intentionally tuples `(connection_id, stream_id)`. ADR 0010
//! is explicit that stream_id is connection-scoped, not globally unique;
//! treating it as global would let one client's connection-restart-collision
//! clobber a sibling's in-flight stream.

use std::collections::HashMap;
use std::sync::RwLock;
use uuid::Uuid;

/// One in-flight stream's metadata. Fields are immutable after
/// construction — the tracker is the single point of in-place state
/// changes, so consumers cannot accidentally diverge a registered entry
/// from a separately-held clone. To "update" an entry, unregister and
/// re-register; the tuple lookup makes the operation O(1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamEntry {
    /// IPC verb name (e.g. `"RecentMemory"`, `"RecentAudit"`,
    /// `"SubmitIntent"`). Free-form snake-case-or-camel-case; the
    /// tracker does not validate.
    pub verb: String,
    /// The per-chunk payload schema string from ADR 0010
    /// (`covenant.ipc.v2.chunk.memory-record.v1`,
    /// `covenant.ipc.v2.chunk.audit-event.v1`,
    /// `covenant.ipc.v2.chunk.agent-result.v1`). Used by future
    /// operator-facing snapshot endpoints to label in-flight streams.
    pub schema: String,
    /// UNIX-epoch milliseconds at register time.
    pub started_at_ms: u64,
}

/// Shared in-memory tracker keyed by `(connection_id, stream_id)`.
///
/// All methods take `&self` and use an internal `RwLock`. Callers wrap
/// it in `Arc<StreamTracker>` and clone the Arc across connection
/// handlers. Lock guards are released before any method returns; no
/// method exposes a guard across a yield point.
///
/// Complexity note: `purge_connection` scans every entry (O(N) over
/// total tracked streams). v0 daemons run single-digit concurrent
/// connections, so the cost is acceptable. A secondary
/// `HashMap<Uuid, Vec<Uuid>>` index keyed by connection_id would lift
/// this to O(K) over per-connection streams; that is a future
/// optimization, not a v0 requirement. A refactor under load must keep
/// the doc-comment honest if it changes the complexity assumption.
#[derive(Debug, Default)]
pub struct StreamTracker {
    entries: RwLock<HashMap<(Uuid, Uuid), StreamEntry>>,
}

impl StreamTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert an entry. If `(connection_id, stream_id)` is already
    /// present the existing entry is overwritten — this should not
    /// happen under correct dispatch flow and likely indicates a race
    /// in the per-verb dispatch integration.
    pub fn register(&self, connection_id: Uuid, stream_id: Uuid, entry: StreamEntry) {
        let mut guard = self
            .entries
            .write()
            .expect("stream tracker rwlock poisoned");
        guard.insert((connection_id, stream_id), entry);
    }

    pub fn unregister(&self, connection_id: Uuid, stream_id: Uuid) -> Option<StreamEntry> {
        let mut guard = self
            .entries
            .write()
            .expect("stream tracker rwlock poisoned");
        guard.remove(&(connection_id, stream_id))
    }

    pub fn get(&self, connection_id: Uuid, stream_id: Uuid) -> Option<StreamEntry> {
        let guard = self.entries.read().expect("stream tracker rwlock poisoned");
        guard.get(&(connection_id, stream_id)).cloned()
    }

    /// Drops every entry whose key has `connection_id` and returns the
    /// number of entries removed. Called from the connection handler's
    /// `PurgeOnDrop` guard so a client disconnect cleans up every stream
    /// that connection opened.
    pub fn purge_connection(&self, connection_id: Uuid) -> usize {
        let mut guard = self
            .entries
            .write()
            .expect("stream tracker rwlock poisoned");
        let before = guard.len();
        guard.retain(|(conn, _), _| *conn != connection_id);
        before - guard.len()
    }

    pub fn len(&self) -> usize {
        let guard = self.entries.read().expect("stream tracker rwlock poisoned");
        guard.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns a copy of every entry. The read guard is dropped before
    /// the Vec is returned so a long-running snapshot caller cannot
    /// stall register/unregister. Order is unspecified — callers must
    /// not rely on insertion order.
    pub fn snapshot(&self) -> Vec<((Uuid, Uuid), StreamEntry)> {
        let guard = self.entries.read().expect("stream tracker rwlock poisoned");
        guard.iter().map(|(k, v)| (*k, v.clone())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::task::JoinSet;

    fn fixture_entry(verb: &str) -> StreamEntry {
        StreamEntry {
            verb: verb.into(),
            schema: "covenant.ipc.v2.chunk.memory-record.v1".into(),
            started_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        }
    }

    #[test]
    fn register_get_unregister_round_trip_pins_lifecycle() {
        let t = StreamTracker::new();
        assert!(t.is_empty());

        let conn = Uuid::new_v4();
        let stream = Uuid::new_v4();
        let entry = fixture_entry("RecentMemory");
        t.register(conn, stream, entry.clone());
        assert_eq!(t.len(), 1);
        assert_eq!(t.get(conn, stream), Some(entry.clone()));

        let removed = t.unregister(conn, stream);
        assert_eq!(removed, Some(entry));
        assert!(t.is_empty());
        assert_eq!(t.get(conn, stream), None);
        assert_eq!(t.unregister(conn, stream), None);
    }

    #[test]
    fn purge_connection_removes_only_matching_connection_entries_and_returns_count() {
        let t = StreamTracker::new();
        let conn_a = Uuid::new_v4();
        let conn_b = Uuid::new_v4();

        let s1 = Uuid::new_v4();
        let s2 = Uuid::new_v4();
        let s3 = Uuid::new_v4();
        let s4 = Uuid::new_v4();

        t.register(conn_a, s1, fixture_entry("v1"));
        t.register(conn_a, s2, fixture_entry("v2"));
        t.register(conn_b, s3, fixture_entry("v3"));
        t.register(conn_b, s4, fixture_entry("v4"));
        assert_eq!(t.len(), 4);

        let dropped = t.purge_connection(conn_a);
        assert_eq!(
            dropped, 2,
            "purge_connection must return the exact count of entries removed; a return value off by one would mask a missed drop or a double-count"
        );
        assert_eq!(t.len(), 2);
        assert_eq!(t.get(conn_a, s1), None);
        assert_eq!(t.get(conn_a, s2), None);
        assert!(t.get(conn_b, s3).is_some());
        assert!(t.get(conn_b, s4).is_some());

        let dropped = t.purge_connection(conn_b);
        assert_eq!(dropped, 2);
        assert!(t.is_empty());

        let dropped = t.purge_connection(Uuid::new_v4());
        assert_eq!(
            dropped, 0,
            "purging a connection_id with no entries must return 0 cleanly (not panic, not negative-arithmetic underflow)"
        );
    }

    #[test]
    fn same_stream_id_under_different_connections_is_independent() {
        // ADR 0010: stream_id is connection-scoped, not globally unique.
        // The tracker's tuple key must preserve that. If a refactor
        // dropped the connection_id from the HashMap key (under a
        // "stream_id is already a Uuid, that's unique enough" rationale),
        // this test surfaces it: two connections that allocate the same
        // stream_id would overwrite each other under a single-key map.
        let t = StreamTracker::new();
        let conn_a = Uuid::new_v4();
        let conn_b = Uuid::new_v4();
        let shared_stream_id = Uuid::new_v4();

        let entry_a = StreamEntry {
            verb: "from_conn_a".into(),
            schema: "covenant.ipc.v2.chunk.memory-record.v1".into(),
            started_at_ms: 1,
        };
        let entry_b = StreamEntry {
            verb: "from_conn_b".into(),
            schema: "covenant.ipc.v2.chunk.audit-event.v1".into(),
            started_at_ms: 2,
        };
        t.register(conn_a, shared_stream_id, entry_a.clone());
        t.register(conn_b, shared_stream_id, entry_b.clone());

        assert_eq!(t.len(), 2);
        assert_eq!(t.get(conn_a, shared_stream_id), Some(entry_a));
        assert_eq!(t.get(conn_b, shared_stream_id), Some(entry_b));
    }

    #[test]
    fn snapshot_returns_all_entries_with_dropped_read_guard() {
        let t = StreamTracker::new();
        let conn = Uuid::new_v4();
        let s1 = Uuid::new_v4();
        let s2 = Uuid::new_v4();
        t.register(conn, s1, fixture_entry("RecentMemory"));
        t.register(conn, s2, fixture_entry("RecentAudit"));

        let mut snap = t.snapshot();
        snap.sort_by_key(|((_, s), _)| *s);
        assert_eq!(snap.len(), 2);

        // Asserting that the guard is dropped before snapshot returns
        // — register a fresh entry from the same thread after snapshot
        // returns. If snapshot held the read guard the write would
        // deadlock; we let the test runner's wall-clock supervision
        // catch a deadlock, but the absence of a deadlock here is the
        // pin.
        t.register(conn, Uuid::new_v4(), fixture_entry("SubmitIntent"));
        assert_eq!(t.len(), 3);
    }

    #[tokio::test]
    async fn concurrent_register_and_unregister_converges_to_empty() {
        // Stress the RwLock under N concurrent writers + corresponding
        // unregisters. The tracker must end empty and len must converge
        // monotonically. A bug that double-registered an entry or
        // dropped a write under contention would surface as len != 0
        // after join.
        let t = Arc::new(StreamTracker::new());
        let conn = Uuid::new_v4();
        let mut set = JoinSet::new();

        const N: usize = 32;
        for _ in 0..N {
            let t = t.clone();
            set.spawn(async move {
                let stream = Uuid::new_v4();
                t.register(conn, stream, fixture_entry("concurrent"));
                assert!(t.get(conn, stream).is_some());
                assert!(t.unregister(conn, stream).is_some());
            });
        }
        while let Some(res) = set.join_next().await {
            res.expect("spawned task panicked");
        }
        assert!(
            t.is_empty(),
            "concurrent register+unregister must converge to len=0; got {}",
            t.len()
        );
    }
}
