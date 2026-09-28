//! The durable bracket around every on-chain push.
//!
//! `SidecarPayout`'s own idempotency ledger is in-memory and rebuilt
//! empty on restart, and the obligations' completion markers (a job
//! record's payout, a withdrawal's push, an unbond's push) are
//! journaled AFTER the irreversible transfer. That ordering leaves a
//! crash window in which money left but no durable line says so — and
//! the default-on retry sweeps would then submit a second, distinct
//! transaction (fresh blockhash, different signature, not
//! network-deduped).
//!
//! This book closes the window by inverting the ordering for the
//! *attempt*: an `Attempted` line is journaled and fsynced BEFORE the
//! signer can spawn, and resolved (or cleared) only after the outcome
//! is durably known. An open bracket therefore means exactly "a
//! transfer may be live for this obligation": every push path refuses
//! to touch such an obligation, boot either auto-resolves it from the
//! completion marker that did land or suspends it, and a suspended
//! obligation moves again only through the admin reconcile endpoint —
//! after a human (or a chain query) has checked the memo on-chain.
//! Fail-stuck, never double-paid.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use uuid::Uuid;

use crate::journal::{
    Journal, JournalError, TransferAttemptKind, TransferAttemptState, TransferAttemptStatus,
};

/// What [`TransferAttempts::begin`] decided.
pub enum BeginOutcome {
    /// The bracket is journaled; the caller may spawn the transfer.
    Proceed,
    /// An earlier bracket for this obligation is still open — either a
    /// push is in flight right now or a previous outcome was never
    /// resolved. Nothing may move until it is.
    Open(TransferAttemptState),
}

/// In-memory unless built with [`TransferAttempts::restore`], in which
/// case every transition journals before it commits — the same posture
/// as every other money book in this crate.
#[derive(Default)]
pub struct TransferAttempts {
    open: Mutex<HashMap<Uuid, TransferAttemptState>>,
    journal: Option<Arc<Journal>>,
}

impl TransferAttempts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seeds the book with the journal's still-`Attempted` brackets —
    /// the crash windows boot could not auto-resolve — and journals
    /// every transition from here on.
    pub fn restore(attempts: HashMap<Uuid, TransferAttemptState>, journal: Arc<Journal>) -> Self {
        let open = attempts
            .into_iter()
            .filter(|(_, s)| s.status == TransferAttemptStatus::Attempted)
            .collect();
        Self {
            open: Mutex::new(open),
            journal: Some(journal),
        }
    }

    /// Opens the bracket for one obligation, durably, before any
    /// transfer may be spawned. Refuses while a bracket for the same
    /// obligation is open — in flight or suspended, both mean "hands
    /// off".
    pub fn begin(
        &self,
        attempt_id: Uuid,
        kind: TransferAttemptKind,
        amount_micro_usdc: u64,
        recipient_address_b58: &str,
        memo: &str,
        now_ms: u64,
    ) -> Result<BeginOutcome, JournalError> {
        let mut open = self.open.lock();
        if let Some(existing) = open.get(&attempt_id) {
            return Ok(BeginOutcome::Open(existing.clone()));
        }
        let state = TransferAttemptState {
            attempt_id,
            kind,
            amount_micro_usdc,
            recipient_address_b58: recipient_address_b58.to_string(),
            memo: memo.to_string(),
            attempted_at_ms: now_ms,
            status: TransferAttemptStatus::Attempted,
            tx_signature: None,
            detail: String::new(),
        };
        if let Some(journal) = &self.journal {
            journal.record_transfer_attempt(&state)?;
        }
        open.insert(attempt_id, state);
        Ok(BeginOutcome::Proceed)
    }

    /// Closes the bracket as landed. Called only after the obligation's
    /// own completion marker is durable — resolving first would re-open
    /// the double-spend window this book exists to close.
    pub fn resolve(
        &self,
        attempt_id: Uuid,
        tx_signature: Option<&str>,
    ) -> Result<(), JournalError> {
        self.close(
            attempt_id,
            TransferAttemptStatus::Resolved,
            tx_signature,
            "",
        )
    }

    /// Closes the bracket as definitively-not-landed; the obligation is
    /// free for the sweeps to retry.
    pub fn clear(&self, attempt_id: Uuid, detail: &str) -> Result<(), JournalError> {
        self.close(attempt_id, TransferAttemptStatus::Cleared, None, detail)
    }

    /// Pins an unknown outcome onto the open bracket: the bracket stays
    /// open (suspending the obligation) with the failure and any known
    /// signature recorded for whoever reconciles it.
    pub fn suspend(
        &self,
        attempt_id: Uuid,
        detail: &str,
        tx_signature: Option<&str>,
    ) -> Result<(), JournalError> {
        let mut open = self.open.lock();
        let Some(state) = open.get_mut(&attempt_id) else {
            return Ok(());
        };
        state.detail = detail.to_string();
        state.tx_signature = tx_signature.map(str::to_string);
        if let Some(journal) = &self.journal {
            journal.record_transfer_attempt(state)?;
        }
        tracing::error!(
            %attempt_id,
            kind = state.kind.as_str(),
            amount_micro_usdc = state.amount_micro_usdc,
            memo = %state.memo,
            tx_signature = state.tx_signature.as_deref().unwrap_or(""),
            "transfer outcome unknown; obligation suspended until reconciled \
             (POST /admin/transfers/{{id}}/resolve)"
        );
        Ok(())
    }

    fn close(
        &self,
        attempt_id: Uuid,
        status: TransferAttemptStatus,
        tx_signature: Option<&str>,
        detail: &str,
    ) -> Result<(), JournalError> {
        let mut open = self.open.lock();
        let Some(mut state) = open.remove(&attempt_id) else {
            return Ok(());
        };
        state.status = status;
        state.tx_signature = tx_signature.map(str::to_string);
        state.detail = detail.to_string();
        if let Some(journal) = &self.journal {
            if let Err(e) = journal.record_transfer_attempt(&state) {
                // The bracket must not vanish from memory if its close
                // couldn't be made durable — put it back open.
                state.status = TransferAttemptStatus::Attempted;
                open.insert(attempt_id, state);
                return Err(e);
            }
        }
        Ok(())
    }

    /// Whether a bracket for this obligation is open — in flight or
    /// suspended. Push paths skip such obligations without spawning
    /// anything.
    pub fn is_open(&self, attempt_id: Uuid) -> bool {
        self.open.lock().contains_key(&attempt_id)
    }

    pub fn get(&self, attempt_id: Uuid) -> Option<TransferAttemptState> {
        self.open.lock().get(&attempt_id).cloned()
    }

    /// Every open bracket, oldest first — the admin's reconcile
    /// worklist and the `/metrics` gauge.
    pub fn open_attempts(&self) -> Vec<TransferAttemptState> {
        let mut rows: Vec<TransferAttemptState> = self.open.lock().values().cloned().collect();
        rows.sort_by(|a, b| {
            a.attempted_at_ms
                .cmp(&b.attempted_at_ms)
                .then(a.attempt_id.cmp(&b.attempt_id))
        });
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn begin(attempts: &TransferAttempts, id: Uuid) -> BeginOutcome {
        attempts
            .begin(id, TransferAttemptKind::JobPayout, 5_000, "addr", "memo", 1)
            .unwrap()
    }

    fn attempted(id: Uuid, at_ms: u64) -> TransferAttemptState {
        TransferAttemptState {
            attempt_id: id,
            kind: TransferAttemptKind::JobPayout,
            amount_micro_usdc: 5_000,
            recipient_address_b58: "addr".to_string(),
            memo: "memo".to_string(),
            attempted_at_ms: at_ms,
            status: TransferAttemptStatus::Attempted,
            tx_signature: None,
            detail: String::new(),
        }
    }

    #[test]
    fn a_begun_attempt_blocks_a_second_until_closed() {
        let attempts = TransferAttempts::new();
        let id = Uuid::new_v4();
        assert!(matches!(begin(&attempts, id), BeginOutcome::Proceed));
        assert!(matches!(begin(&attempts, id), BeginOutcome::Open(_)));
        assert!(attempts.is_open(id));

        attempts.clear(id, "signer refused").unwrap();
        assert!(!attempts.is_open(id));
        assert!(matches!(begin(&attempts, id), BeginOutcome::Proceed));
    }

    #[test]
    fn a_suspended_attempt_stays_open_with_its_evidence() {
        let attempts = TransferAttempts::new();
        let id = Uuid::new_v4();
        assert!(matches!(begin(&attempts, id), BeginOutcome::Proceed));
        attempts
            .suspend(id, "confirm timed out", Some("sig-1"))
            .unwrap();
        assert!(attempts.is_open(id));
        let state = attempts.get(id).unwrap();
        assert_eq!(state.detail, "confirm timed out");
        assert_eq!(state.tx_signature.as_deref(), Some("sig-1"));
        assert!(matches!(begin(&attempts, id), BeginOutcome::Open(_)));

        attempts.resolve(id, Some("sig-1")).unwrap();
        assert!(!attempts.is_open(id));
    }

    #[test]
    fn journaled_attempts_survive_a_restart_and_resolved_ones_do_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let journal = Arc::new(Journal::open(&path).unwrap());
        let attempts = TransferAttempts::restore(HashMap::new(), journal);

        let suspended = Uuid::new_v4();
        let resolved = Uuid::new_v4();
        attempts
            .begin(
                suspended,
                TransferAttemptKind::UnbondRefund,
                600,
                "addr-a",
                "memo-a",
                1,
            )
            .unwrap();
        attempts
            .begin(
                resolved,
                TransferAttemptKind::Withdrawal,
                700,
                "addr-b",
                "memo-b",
                2,
            )
            .unwrap();
        attempts.resolve(resolved, Some("sig-b")).unwrap();

        let restored = Journal::load(&path).unwrap();
        let book = TransferAttempts::restore(
            restored.transfer_attempts,
            Arc::new(Journal::open(&path).unwrap()),
        );
        assert!(book.is_open(suspended));
        assert!(!book.is_open(resolved));
        assert_eq!(book.open_attempts().len(), 1);
    }

    #[test]
    fn a_close_that_cannot_be_journaled_leaves_the_bracket_open() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Arc::new(Journal::read_only_for_test(&dir.path().join("j.jsonl")));
        let id = Uuid::new_v4();
        let book = TransferAttempts::restore(HashMap::from([(id, attempted(id, 1))]), journal);
        assert!(book.is_open(id));

        // The obligation's completion marker landed, but journaling the
        // close does not. The bracket must stay open — dropping it here
        // would un-fence a transfer that may be live and let a retry
        // sweep push a second one.
        assert!(book.resolve(id, Some("sig")).is_err());
        assert!(book.is_open(id));
        assert_eq!(
            book.get(id).unwrap().status,
            TransferAttemptStatus::Attempted
        );
    }

    #[test]
    fn closing_an_unknown_or_already_closed_bracket_is_a_no_op() {
        let attempts = TransferAttempts::new();
        let id = Uuid::new_v4();
        attempts.resolve(id, Some("sig")).unwrap();
        attempts.clear(id, "never begun").unwrap();
        assert!(!attempts.is_open(id));

        assert!(matches!(begin(&attempts, id), BeginOutcome::Proceed));
        attempts.resolve(id, Some("sig")).unwrap();
        // A redelivered completion or a retried sweep tick resolves it
        // again; the second call finds nothing and cannot re-open it.
        attempts.resolve(id, Some("sig")).unwrap();
        assert!(!attempts.is_open(id));
    }

    #[test]
    fn suspending_an_unknown_bracket_creates_nothing() {
        let attempts = TransferAttempts::new();
        let id = Uuid::new_v4();
        attempts
            .suspend(id, "confirm timed out", Some("sig"))
            .unwrap();
        assert!(!attempts.is_open(id));
        assert!(attempts.get(id).is_none());
    }

    #[test]
    fn open_attempts_lists_oldest_first_breaking_ties_by_id() {
        let attempts = TransferAttempts::new();
        let newer = Uuid::from_u128(1);
        let tie_low = Uuid::from_u128(2);
        let tie_high = Uuid::from_u128(3);
        for (id, at) in [(newer, 30), (tie_high, 10), (tie_low, 10)] {
            attempts
                .begin(id, TransferAttemptKind::JobPayout, 1, "a", "m", at)
                .unwrap();
        }
        let order: Vec<Uuid> = attempts
            .open_attempts()
            .iter()
            .map(|s| s.attempt_id)
            .collect();
        assert_eq!(order, vec![tie_low, tie_high, newer]);
    }
}
