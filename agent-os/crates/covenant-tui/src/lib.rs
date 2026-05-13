//! Covenant TUI core. Holds the testable state and key-handler surface
//! that the binary's event loop drives. The terminal I/O lives in
//! `main.rs`; everything in this module is `#[cfg(test)]`-friendly and
//! does not touch stdout, stdin, raw mode, or the alternate screen.
//!
//! Future slices extend [`App`] with screens (memory tail, audit tail,
//! capabilities, etc.); each screen adds state here and a match arm
//! in [`App::on_key`]. The I/O layer in `main.rs` does not change shape.

use covenant_a2a::A2ATask;
use covenant_audit::AuditEvent;
use covenant_peer_auth::PeerSummary;
use covenant_permissions::SignedCapability;
use covenant_types::{MemoryRecord, SettlementReceipt};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use uuid::Uuid;

pub mod ipc;

pub use ipc::{
    A2aFetchOutcome, AuditFetchOutcome, CapabilitiesFetchOutcome, GrantOutcome, MemoryFetchOutcome,
    PeersFetchOutcome, ReceiptsFetchOutcome,
};

/// Reasons the event loop may exit. `None` while the app keeps running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// The user pressed `q` or `Esc` from the base view.
    UserQuit,
    /// The user pressed `Ctrl-C` from any view.
    Interrupt,
}

/// Active screen / interaction mode.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Mode {
    /// Base view. `i` enters the intent editor; `s` submits the most
    /// recent draft to the daemon; `q` / `Esc` quit.
    #[default]
    Browsing,
    /// Intent editor: typing fills the buffer; Backspace removes the
    /// last char; `Enter` accepts the buffer into [`App::drafts`] and
    /// returns to [`Mode::Browsing`]; `Esc` discards the buffer and
    /// returns to [`Mode::Browsing`]. `q` is a literal character here,
    /// not a quit.
    Editing { buffer: String },
    /// In-flight submission. Renderer shows a spinner-style hint.
    /// `Esc` here does NOT cancel the in-flight RPC; it just hides
    /// this view and returns to Browsing (the response, when it
    /// lands, is dropped).
    Submitting { text: String },
    /// Daemon returned an intent result. Any key returns to Browsing.
    Result {
        intent_id: Uuid,
        status: String,
        text: String,
    },
    /// Submission failed (IPC, auth, or daemon-side error). Any key
    /// returns to Browsing. Message is rendered as-is so a CLI
    /// caller's reasoning carries through.
    Error { message: String },
    /// Memory tail view. Press `m` from Browsing to enter; the
    /// renderer shows a "fetching…" hint while `loading` is true,
    /// then the records or the embedded error once the fetch
    /// resolves. Press `q` / `Esc` to dismiss.
    MemoryTail {
        loading: bool,
        records: Vec<MemoryRecord>,
        error: Option<String>,
    },
    /// Audit tail view. Press `a` from Browsing to enter. The audit
    /// log has no read-side capability gate (rows are server-side
    /// filtered to the calling peer's own activity), so the fetch
    /// only fails on wire-level or auth issues. Press `q` / `Esc`
    /// to dismiss.
    AuditTail {
        loading: bool,
        events: Vec<AuditEvent>,
        error: Option<String>,
    },
    /// Capabilities view. Press `c` from Browsing to enter. Lists
    /// active signed capabilities where the operator is either the
    /// subject or the granter; server-side filtered, no read-side
    /// gate. Press `q` / `Esc` to dismiss.
    CapabilitiesTail {
        loading: bool,
        capabilities: Vec<SignedCapability>,
        error: Option<String>,
    },
    /// A2A inbox view. Press `A` from Browsing to enter. Lists
    /// recent A2A tasks where the operator is either sender or
    /// recipient; server-side filtered, no read-side gate. Press
    /// `q` / `Esc` to dismiss.
    A2aTail {
        loading: bool,
        tasks: Vec<A2ATask>,
        error: Option<String>,
    },
    /// Chain receipts view. Press `r` from Browsing to enter. Lists
    /// recent settlement receipts where the operator is the payer;
    /// gated by the `chain.receipts` capability and server-side
    /// filtered. Press `q` / `Esc` to dismiss.
    ReceiptsTail {
        loading: bool,
        receipts: Vec<SettlementReceipt>,
        error: Option<String>,
    },
    /// Peer registry view. Press `p` from Browsing to enter. Lists
    /// bootstrapped peers newest-first, surfacing both live and
    /// revoked rows; operator-only on the daemon side, so a non-
    /// operator caller collapses into `error`. `truncated` reports
    /// whether the daemon held back rows past the request's limit.
    /// Press `q` / `Esc` to dismiss.
    PeersTail {
        loading: bool,
        peers: Vec<PeerSummary>,
        truncated: bool,
        error: Option<String>,
    },
    /// Grant editor. Press `g` from Browsing to enter. Chars
    /// accumulate as the capability action name (e.g. `memory.read`);
    /// Enter on a non-empty trimmed buffer transitions to
    /// `GrantSubmitting`; Esc returns to Browsing.
    GrantEditor { buffer: String },
    /// In-flight unscoped capability grant. Distinct from
    /// [`Mode::Submitting`] so the renderer can show the right
    /// label and the dispatch path stays clear of intent submission.
    GrantSubmitting { action: String },
    /// Grant succeeded. Any key returns to Browsing; `q` quits.
    GrantResult {
        action: String,
        subject_display: String,
        signature_b58: String,
    },
    /// Grant failed (daemon-side or wire-level). Any key returns to
    /// Browsing; `q` quits.
    GrantError { message: String },
    /// Keybinding help overlay. Press `?` from Browsing to enter.
    /// Any key (including `?`) returns to Browsing.
    Help,
}

impl Mode {
    /// Stable, render-safe discriminant string. The status bar reads
    /// this so a `{:?}` Debug refactor can't silently leak internal
    /// fields (buffer contents, error messages) into the bar. The
    /// match is exhaustive so any new variant must add an arm.
    pub fn name(&self) -> &'static str {
        match self {
            Mode::Browsing => "browsing",
            Mode::Editing { .. } => "editing",
            Mode::Submitting { .. } => "submitting",
            Mode::Result { .. } => "result",
            Mode::Error { .. } => "error",
            Mode::MemoryTail { .. } => "memory-tail",
            Mode::AuditTail { .. } => "audit-tail",
            Mode::CapabilitiesTail { .. } => "capabilities-tail",
            Mode::A2aTail { .. } => "a2a-tail",
            Mode::ReceiptsTail { .. } => "receipts-tail",
            Mode::PeersTail { .. } => "peers-tail",
            Mode::GrantEditor { .. } => "grant-editor",
            Mode::GrantSubmitting { .. } => "grant-submitting",
            Mode::GrantResult { .. } => "grant-result",
            Mode::GrantError { .. } => "grant-error",
            Mode::Help => "help",
        }
    }
}

/// Drafted intents are capped so a stress-test (or an unattended
/// keyboard) cannot grow the buffer without bound. The oldest entry
/// drops when the cap is exceeded.
const MAX_DRAFTS: usize = 64;

/// Keybindings available from `Mode::Browsing`, paired with the
/// short description rendered in the help overlay. The slice is the
/// single source of truth — `handle_browsing` and the help renderer
/// both read it so a new binding cannot land in the handler without
/// showing up in the overlay (or vice versa).
pub const HELP_BINDINGS: &[(&str, &str)] = &[
    ("i", "draft intent"),
    ("s", "submit most recent draft"),
    ("g", "grant capability"),
    ("m", "memory tail"),
    ("a", "audit tail"),
    ("c", "capabilities tail"),
    ("A", "a2a inbox"),
    ("r", "chain receipts"),
    ("p", "peers"),
    ("?", "this help"),
    ("q", "quit"),
];

/// Title rendered above the peers-tail panel. The truncated branch
/// suffixes "(truncated)" so an operator inspecting a short-result
/// list can tell the daemon dropped rows past `limit` apart from a
/// genuinely small peer set. Kept as a `&'static str` so a regression
/// that drops the truncated marker fails the corresponding unit test
/// instead of only surfacing under live screenshot review.
pub fn peers_tail_title(truncated: bool) -> &'static str {
    if truncated {
        "peers — q / Esc to dismiss (truncated)"
    } else {
        "peers — q / Esc to dismiss"
    }
}

/// Top-level TUI state. Screens are added by future slices.
#[derive(Debug, Default)]
pub struct App {
    mode: Mode,
    drafts: Vec<String>,
    exit: Option<ExitReason>,
    /// Set when transitioning to [`Mode::Submitting`] and cleared by
    /// the first call to [`App::take_pending_submission`]. The flag
    /// ensures the binary's event loop only spawns one IPC task per
    /// submission, even though the App may remain in `Submitting`
    /// for the lifetime of the in-flight RPC.
    pending_submission: bool,
    /// Set when transitioning to [`Mode::MemoryTail`] and cleared by
    /// the first call to [`App::take_pending_memory_fetch`]. Same
    /// one-shot-flag pattern as `pending_submission`.
    pending_memory_fetch: bool,
    /// Set when transitioning to [`Mode::AuditTail`] and cleared by
    /// the first call to [`App::take_pending_audit_fetch`].
    pending_audit_fetch: bool,
    /// One-shot for the capabilities-view fetch.
    pending_capabilities_fetch: bool,
    /// One-shot for the A2A inbox fetch.
    pending_a2a_fetch: bool,
    /// One-shot for the chain receipts fetch.
    pending_receipts_fetch: bool,
    /// One-shot for the peer registry fetch.
    pending_peers_fetch: bool,
    /// One-shot for an in-flight grant submission. Set on the
    /// transition into `GrantSubmitting`.
    pending_grant_submission: bool,
}

impl App {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current mode. Renderer in `main.rs` reads this to decide which
    /// screen to draw.
    pub fn mode(&self) -> &Mode {
        &self.mode
    }

    /// Drafted intents in submission order (oldest first). Slice 3
    /// will pull entries off the front and send them to the daemon.
    pub fn drafts(&self) -> &[String] {
        &self.drafts
    }

    /// `Some(reason)` once the event loop should stop. The binary's
    /// loop reads this after every key event and breaks when it
    /// transitions to `Some`.
    pub fn exit_reason(&self) -> Option<ExitReason> {
        self.exit
    }

    pub fn should_quit(&self) -> bool {
        self.exit.is_some()
    }

    /// Key handler. Only reacts to `KeyEventKind::Press` so a held key
    /// on Windows (which emits `Press` + `Repeat` + `Release`) does
    /// not flip state multiple times per physical press.
    pub fn on_key(&mut self, event: KeyEvent) {
        if event.kind != KeyEventKind::Press {
            return;
        }
        // Ctrl-C interrupts from every mode. Check before mode dispatch
        // so a runaway editor session can't trap the user.
        if event.modifiers.contains(KeyModifiers::CONTROL) && event.code == KeyCode::Char('c') {
            self.exit = Some(ExitReason::Interrupt);
            return;
        }
        // Take ownership of the current mode so handlers can both mutate
        // `self` (drafts, exit) and return the next mode without
        // borrow-checker conflicts. Restored on the next line.
        let prev = std::mem::take(&mut self.mode);
        self.mode = match prev {
            Mode::Browsing => self.handle_browsing(event),
            Mode::Editing { buffer } => self.handle_editing(buffer, event),
            // Esc / any key dismisses Submitting (without cancelling
            // the in-flight RPC) or a terminal result/error view.
            Mode::Submitting { text } => match event.code {
                KeyCode::Esc => Mode::Browsing,
                _ => Mode::Submitting { text },
            },
            Mode::Result { .. } | Mode::Error { .. } => self.handle_terminal_view(event),
            Mode::MemoryTail {
                loading,
                records,
                error,
            } => match event.code {
                KeyCode::Char('q') | KeyCode::Esc => Mode::Browsing,
                _ => Mode::MemoryTail {
                    loading,
                    records,
                    error,
                },
            },
            Mode::AuditTail {
                loading,
                events,
                error,
            } => match event.code {
                KeyCode::Char('q') | KeyCode::Esc => Mode::Browsing,
                _ => Mode::AuditTail {
                    loading,
                    events,
                    error,
                },
            },
            Mode::CapabilitiesTail {
                loading,
                capabilities,
                error,
            } => match event.code {
                KeyCode::Char('q') | KeyCode::Esc => Mode::Browsing,
                _ => Mode::CapabilitiesTail {
                    loading,
                    capabilities,
                    error,
                },
            },
            Mode::A2aTail {
                loading,
                tasks,
                error,
            } => match event.code {
                KeyCode::Char('q') | KeyCode::Esc => Mode::Browsing,
                _ => Mode::A2aTail {
                    loading,
                    tasks,
                    error,
                },
            },
            Mode::ReceiptsTail {
                loading,
                receipts,
                error,
            } => match event.code {
                KeyCode::Char('q') | KeyCode::Esc => Mode::Browsing,
                _ => Mode::ReceiptsTail {
                    loading,
                    receipts,
                    error,
                },
            },
            Mode::PeersTail {
                loading,
                peers,
                truncated,
                error,
            } => match event.code {
                KeyCode::Char('q') | KeyCode::Esc => Mode::Browsing,
                _ => Mode::PeersTail {
                    loading,
                    peers,
                    truncated,
                    error,
                },
            },
            Mode::GrantEditor { buffer } => self.handle_grant_editor(buffer, event),
            Mode::GrantSubmitting { action } => match event.code {
                KeyCode::Esc => Mode::Browsing,
                _ => Mode::GrantSubmitting { action },
            },
            Mode::GrantResult { .. } | Mode::GrantError { .. } => self.handle_terminal_view(event),
            Mode::Help => Mode::Browsing,
        };
    }

    /// Apply the daemon's response (or a wire-level error) to the App
    /// state. No-op if the user has already navigated away from
    /// Submitting (e.g. they pressed Esc before the response landed).
    pub fn apply_submission_outcome(&mut self, outcome: SubmissionOutcome) {
        if !matches!(self.mode, Mode::Submitting { .. }) {
            return;
        }
        self.mode = match outcome {
            SubmissionOutcome::Accepted {
                intent_id,
                status,
                text,
            } => Mode::Result {
                intent_id,
                status,
                text,
            },
            SubmissionOutcome::Failed { message } => Mode::Error { message },
        };
    }

    /// In-flight submission text, exposed so the renderer can show
    /// what is being submitted. Always returns `Some` while in
    /// [`Mode::Submitting`]; see [`App::take_pending_submission`] for
    /// the kickoff variant.
    pub fn in_flight(&self) -> Option<&str> {
        match &self.mode {
            Mode::Submitting { text } => Some(text.as_str()),
            _ => None,
        }
    }

    /// Returns the text of a freshly-entered submission exactly once,
    /// consuming the `pending_submission` flag set on the transition
    /// into [`Mode::Submitting`]. The binary's event loop calls this
    /// each iteration: a `Some` value means "spawn an IPC task for
    /// this text"; a `None` value means "the in-flight RPC has
    /// already been spawned, do nothing this tick".
    pub fn take_pending_submission(&mut self) -> Option<String> {
        if !self.pending_submission {
            return None;
        }
        let Mode::Submitting { text } = &self.mode else {
            return None;
        };
        let text = text.clone();
        self.pending_submission = false;
        Some(text)
    }

    /// One-shot flag for the memory-tail fetch. Same kickoff
    /// semantics as [`App::take_pending_submission`].
    pub fn take_pending_memory_fetch(&mut self) -> bool {
        if !self.pending_memory_fetch {
            return false;
        }
        self.pending_memory_fetch = false;
        true
    }

    /// Apply a memory-tail fetch result. No-op if the user has
    /// already navigated away from `Mode::MemoryTail`.
    pub fn apply_memory_fetch_outcome(&mut self, outcome: MemoryFetchOutcome) {
        let Mode::MemoryTail { .. } = &self.mode else {
            return;
        };
        self.mode = match outcome {
            MemoryFetchOutcome::Fetched { records } => Mode::MemoryTail {
                loading: false,
                records,
                error: None,
            },
            MemoryFetchOutcome::Failed { message } => Mode::MemoryTail {
                loading: false,
                records: Vec::new(),
                error: Some(message),
            },
        };
    }

    /// One-shot flag for the audit-tail fetch. Same kickoff
    /// semantics as [`App::take_pending_memory_fetch`].
    pub fn take_pending_audit_fetch(&mut self) -> bool {
        if !self.pending_audit_fetch {
            return false;
        }
        self.pending_audit_fetch = false;
        true
    }

    /// Apply an audit-tail fetch result. No-op if the user has
    /// already navigated away from `Mode::AuditTail`.
    pub fn apply_audit_fetch_outcome(&mut self, outcome: AuditFetchOutcome) {
        let Mode::AuditTail { .. } = &self.mode else {
            return;
        };
        self.mode = match outcome {
            AuditFetchOutcome::Fetched { events } => Mode::AuditTail {
                loading: false,
                events,
                error: None,
            },
            AuditFetchOutcome::Failed { message } => Mode::AuditTail {
                loading: false,
                events: Vec::new(),
                error: Some(message),
            },
        };
    }

    /// One-shot flag for the capabilities-view fetch.
    pub fn take_pending_capabilities_fetch(&mut self) -> bool {
        if !self.pending_capabilities_fetch {
            return false;
        }
        self.pending_capabilities_fetch = false;
        true
    }

    /// Apply a capabilities-view fetch result. No-op if the user
    /// has already navigated away from `Mode::CapabilitiesTail`.
    pub fn apply_capabilities_fetch_outcome(&mut self, outcome: CapabilitiesFetchOutcome) {
        let Mode::CapabilitiesTail { .. } = &self.mode else {
            return;
        };
        self.mode = match outcome {
            CapabilitiesFetchOutcome::Fetched { capabilities } => Mode::CapabilitiesTail {
                loading: false,
                capabilities,
                error: None,
            },
            CapabilitiesFetchOutcome::Failed { message } => Mode::CapabilitiesTail {
                loading: false,
                capabilities: Vec::new(),
                error: Some(message),
            },
        };
    }

    /// One-shot flag for the A2A inbox fetch.
    pub fn take_pending_a2a_fetch(&mut self) -> bool {
        if !self.pending_a2a_fetch {
            return false;
        }
        self.pending_a2a_fetch = false;
        true
    }

    /// Apply an A2A inbox fetch result. No-op if the user has
    /// already navigated away from `Mode::A2aTail`.
    pub fn apply_a2a_fetch_outcome(&mut self, outcome: A2aFetchOutcome) {
        let Mode::A2aTail { .. } = &self.mode else {
            return;
        };
        self.mode = match outcome {
            A2aFetchOutcome::Fetched { tasks } => Mode::A2aTail {
                loading: false,
                tasks,
                error: None,
            },
            A2aFetchOutcome::Failed { message } => Mode::A2aTail {
                loading: false,
                tasks: Vec::new(),
                error: Some(message),
            },
        };
    }

    /// One-shot flag for the chain receipts fetch.
    pub fn take_pending_receipts_fetch(&mut self) -> bool {
        if !self.pending_receipts_fetch {
            return false;
        }
        self.pending_receipts_fetch = false;
        true
    }

    /// Apply a chain receipts fetch result. No-op if the user has
    /// already navigated away from `Mode::ReceiptsTail`.
    pub fn apply_receipts_fetch_outcome(&mut self, outcome: ReceiptsFetchOutcome) {
        let Mode::ReceiptsTail { .. } = &self.mode else {
            return;
        };
        self.mode = match outcome {
            ReceiptsFetchOutcome::Fetched { receipts } => Mode::ReceiptsTail {
                loading: false,
                receipts,
                error: None,
            },
            ReceiptsFetchOutcome::Failed { message } => Mode::ReceiptsTail {
                loading: false,
                receipts: Vec::new(),
                error: Some(message),
            },
        };
    }

    /// One-shot flag for the peer registry fetch.
    pub fn take_pending_peers_fetch(&mut self) -> bool {
        if !self.pending_peers_fetch {
            return false;
        }
        self.pending_peers_fetch = false;
        true
    }

    /// Apply a peer registry fetch result. No-op if the user has
    /// already navigated away from `Mode::PeersTail`.
    pub fn apply_peers_fetch_outcome(&mut self, outcome: PeersFetchOutcome) {
        let Mode::PeersTail { .. } = &self.mode else {
            return;
        };
        self.mode = match outcome {
            PeersFetchOutcome::Fetched { peers, truncated } => Mode::PeersTail {
                loading: false,
                peers,
                truncated,
                error: None,
            },
            PeersFetchOutcome::Failed { message } => Mode::PeersTail {
                loading: false,
                peers: Vec::new(),
                truncated: false,
                error: Some(message),
            },
        };
    }

    /// Returns the action of a freshly-entered grant exactly once,
    /// same kickoff semantics as [`App::take_pending_submission`].
    pub fn take_pending_grant_submission(&mut self) -> Option<String> {
        if !self.pending_grant_submission {
            return None;
        }
        let Mode::GrantSubmitting { action } = &self.mode else {
            return None;
        };
        let action = action.clone();
        self.pending_grant_submission = false;
        Some(action)
    }

    /// Apply a grant outcome. No-op if the user has dismissed the
    /// Submitting view.
    pub fn apply_grant_outcome(&mut self, outcome: GrantOutcome) {
        let Mode::GrantSubmitting { .. } = &self.mode else {
            return;
        };
        self.mode = match outcome {
            GrantOutcome::Granted {
                signature_b58,
                subject_display,
                action,
            } => Mode::GrantResult {
                action,
                subject_display,
                signature_b58,
            },
            GrantOutcome::Failed { message } => Mode::GrantError { message },
        };
    }
}

/// The two outcomes a submission can have. The daemon's
/// `Response::IntentResult` and any wire-level / auth / capability
/// failure both collapse into this enum so the App's state machine
/// stays simple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmissionOutcome {
    Accepted {
        intent_id: Uuid,
        status: String,
        text: String,
    },
    Failed {
        message: String,
    },
}

impl App {
    fn handle_browsing(&mut self, event: KeyEvent) -> Mode {
        match event.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.exit = Some(ExitReason::UserQuit);
                Mode::Browsing
            }
            KeyCode::Char('i') => Mode::Editing {
                buffer: String::new(),
            },
            KeyCode::Char('s') => {
                // Pop the most recent draft (LIFO). Slice 3 ships
                // newest-first because the user just typed it and
                // expects it to be the one that submits; a queue
                // FIFO can land in a later slice.
                if let Some(text) = self.drafts.pop() {
                    self.pending_submission = true;
                    Mode::Submitting { text }
                } else {
                    Mode::Browsing
                }
            }
            KeyCode::Char('m') => {
                self.pending_memory_fetch = true;
                Mode::MemoryTail {
                    loading: true,
                    records: Vec::new(),
                    error: None,
                }
            }
            KeyCode::Char('a') => {
                self.pending_audit_fetch = true;
                Mode::AuditTail {
                    loading: true,
                    events: Vec::new(),
                    error: None,
                }
            }
            KeyCode::Char('c') => {
                self.pending_capabilities_fetch = true;
                Mode::CapabilitiesTail {
                    loading: true,
                    capabilities: Vec::new(),
                    error: None,
                }
            }
            KeyCode::Char('A') => {
                self.pending_a2a_fetch = true;
                Mode::A2aTail {
                    loading: true,
                    tasks: Vec::new(),
                    error: None,
                }
            }
            KeyCode::Char('r') => {
                self.pending_receipts_fetch = true;
                Mode::ReceiptsTail {
                    loading: true,
                    receipts: Vec::new(),
                    error: None,
                }
            }
            KeyCode::Char('p') => {
                self.pending_peers_fetch = true;
                Mode::PeersTail {
                    loading: true,
                    peers: Vec::new(),
                    truncated: false,
                    error: None,
                }
            }
            KeyCode::Char('g') => Mode::GrantEditor {
                buffer: String::new(),
            },
            KeyCode::Char('?') => Mode::Help,
            _ => Mode::Browsing,
        }
    }

    fn handle_grant_editor(&mut self, mut buffer: String, event: KeyEvent) -> Mode {
        match event.code {
            KeyCode::Esc => Mode::Browsing,
            KeyCode::Enter => {
                let trimmed = buffer.trim().to_string();
                if trimmed.is_empty() {
                    Mode::GrantEditor {
                        buffer: String::new(),
                    }
                } else {
                    self.pending_grant_submission = true;
                    Mode::GrantSubmitting { action: trimmed }
                }
            }
            KeyCode::Backspace => {
                buffer.pop();
                Mode::GrantEditor { buffer }
            }
            KeyCode::Char(c) => {
                buffer.push(c);
                Mode::GrantEditor { buffer }
            }
            _ => Mode::GrantEditor { buffer },
        }
    }

    /// Result/Error modes share the same dismissal shape: any key
    /// returns to Browsing.
    fn handle_terminal_view(&mut self, event: KeyEvent) -> Mode {
        match event.code {
            KeyCode::Char('q') => {
                self.exit = Some(ExitReason::UserQuit);
                Mode::Browsing
            }
            _ => Mode::Browsing,
        }
    }

    fn handle_editing(&mut self, mut buffer: String, event: KeyEvent) -> Mode {
        match event.code {
            KeyCode::Esc => Mode::Browsing,
            KeyCode::Enter => {
                let trimmed = buffer.trim();
                if !trimmed.is_empty() {
                    if self.drafts.len() >= MAX_DRAFTS {
                        self.drafts.remove(0);
                    }
                    self.drafts.push(trimmed.to_string());
                }
                Mode::Browsing
            }
            KeyCode::Backspace => {
                buffer.pop();
                Mode::Editing { buffer }
            }
            KeyCode::Char(c) => {
                buffer.push(c);
                Mode::Editing { buffer }
            }
            _ => Mode::Editing { buffer },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn type_chars(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(press(KeyCode::Char(c)));
        }
    }

    #[test]
    fn new_app_is_running_in_browsing_mode() {
        let app = App::new();
        assert!(!app.should_quit());
        assert_eq!(app.exit_reason(), None);
        assert_eq!(app.mode(), &Mode::Browsing);
        assert!(app.drafts().is_empty());
    }

    #[test]
    fn peers_tail_title_truncated_true_renders_truncated_suffix() {
        assert_eq!(
            peers_tail_title(true),
            "peers — q / Esc to dismiss (truncated)",
            "the truncated branch must suffix '(truncated)' so an operator can distinguish a dropped-row result from a genuinely short peer set",
        );
    }

    #[test]
    fn peers_tail_title_truncated_false_omits_truncated_suffix() {
        assert_eq!(
            peers_tail_title(false),
            "peers — q / Esc to dismiss",
            "the non-truncated branch must omit '(truncated)' so the operator does not misread a complete result as a partial one",
        );
    }

    #[test]
    fn pressing_q_quits_with_user_reason() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('q')));
        assert!(app.should_quit());
        assert_eq!(app.exit_reason(), Some(ExitReason::UserQuit));
    }

    #[test]
    fn pressing_escape_quits_with_user_reason() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.exit_reason(), Some(ExitReason::UserQuit));
    }

    #[test]
    fn ctrl_c_quits_with_interrupt_reason() {
        let mut app = App::new();
        app.on_key(ctrl(KeyCode::Char('c')));
        assert_eq!(app.exit_reason(), Some(ExitReason::Interrupt));
    }

    #[test]
    fn unrelated_keys_leave_state_alone() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('x')));
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Tab));
        assert!(!app.should_quit());
        assert_eq!(app.mode(), &Mode::Browsing);
    }

    #[test]
    fn release_events_do_not_trigger_quit() {
        let mut app = App::new();
        let release = KeyEvent::new_with_kind(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        app.on_key(release);
        assert!(!app.should_quit());
    }

    #[test]
    fn pressing_i_enters_editing_mode_with_empty_buffer() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('i')));
        assert_eq!(
            app.mode(),
            &Mode::Editing {
                buffer: String::new()
            }
        );
        assert!(!app.should_quit());
    }

    #[test]
    fn editor_accumulates_chars_including_q() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('i')));
        type_chars(&mut app, "query q");
        assert_eq!(
            app.mode(),
            &Mode::Editing {
                buffer: "query q".into()
            }
        );
        assert!(!app.should_quit(), "q in editor must be a literal char");
    }

    #[test]
    fn backspace_removes_last_char_in_editor() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('i')));
        type_chars(&mut app, "abc");
        app.on_key(press(KeyCode::Backspace));
        assert_eq!(
            app.mode(),
            &Mode::Editing {
                buffer: "ab".into()
            }
        );
        app.on_key(press(KeyCode::Backspace));
        app.on_key(press(KeyCode::Backspace));
        app.on_key(press(KeyCode::Backspace));
        assert_eq!(
            app.mode(),
            &Mode::Editing {
                buffer: String::new()
            }
        );
    }

    #[test]
    fn esc_in_editor_discards_buffer_and_returns_to_browsing() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('i')));
        type_chars(&mut app, "draft to discard");
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.mode(), &Mode::Browsing);
        assert!(app.drafts().is_empty(), "Esc must not record a draft");
        app.on_key(press(KeyCode::Char('i')));
        assert_eq!(
            app.mode(),
            &Mode::Editing {
                buffer: String::new()
            },
            "next editor session must start empty"
        );
    }

    #[test]
    fn enter_in_editor_accepts_buffer_and_returns_to_browsing() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('i')));
        type_chars(&mut app, "summarise CHANGELOG");
        app.on_key(press(KeyCode::Enter));
        assert_eq!(app.mode(), &Mode::Browsing);
        assert_eq!(app.drafts(), &["summarise CHANGELOG".to_string()]);
    }

    #[test]
    fn enter_on_empty_or_whitespace_buffer_does_not_record_draft() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('i')));
        app.on_key(press(KeyCode::Enter));
        assert_eq!(app.mode(), &Mode::Browsing);
        assert!(app.drafts().is_empty());
        app.on_key(press(KeyCode::Char('i')));
        type_chars(&mut app, "   ");
        app.on_key(press(KeyCode::Enter));
        assert!(app.drafts().is_empty());
    }

    #[test]
    fn ctrl_c_interrupts_from_editor_mode() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('i')));
        type_chars(&mut app, "in flight");
        app.on_key(ctrl(KeyCode::Char('c')));
        assert_eq!(app.exit_reason(), Some(ExitReason::Interrupt));
    }

    #[test]
    fn pressing_s_in_browsing_with_empty_drafts_is_a_noop() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('s')));
        assert_eq!(app.mode(), &Mode::Browsing);
        assert!(app.take_pending_submission().is_none());
    }

    #[test]
    fn pressing_s_pops_most_recent_draft_and_enters_submitting() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('i')));
        type_chars(&mut app, "first");
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Char('i')));
        type_chars(&mut app, "most recent");
        app.on_key(press(KeyCode::Enter));
        assert_eq!(app.drafts().len(), 2);

        app.on_key(press(KeyCode::Char('s')));
        assert_eq!(
            app.mode(),
            &Mode::Submitting {
                text: "most recent".into()
            }
        );
        assert_eq!(app.drafts(), &["first".to_string()]);
        assert_eq!(app.in_flight(), Some("most recent"));
    }

    #[test]
    fn take_pending_submission_returns_text_once_then_none() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('i')));
        type_chars(&mut app, "submit me");
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Char('s')));

        assert_eq!(app.take_pending_submission(), Some("submit me".into()));
        assert_eq!(app.take_pending_submission(), None);
        // App is still in Submitting so the renderer can show the
        // in-flight text; only the kickoff flag was consumed.
        assert_eq!(app.in_flight(), Some("submit me"));
    }

    #[test]
    fn apply_submission_outcome_accepted_transitions_to_result() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('i')));
        type_chars(&mut app, "intent text");
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Char('s')));
        let _ = app.take_pending_submission();

        let intent_id = Uuid::new_v4();
        app.apply_submission_outcome(SubmissionOutcome::Accepted {
            intent_id,
            status: "ok".into(),
            text: "daemon reply".into(),
        });
        assert_eq!(
            app.mode(),
            &Mode::Result {
                intent_id,
                status: "ok".into(),
                text: "daemon reply".into()
            }
        );
    }

    #[test]
    fn apply_submission_outcome_failed_transitions_to_error() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('i')));
        type_chars(&mut app, "intent text");
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Char('s')));
        let _ = app.take_pending_submission();

        app.apply_submission_outcome(SubmissionOutcome::Failed {
            message: "capability missing".into(),
        });
        assert_eq!(
            app.mode(),
            &Mode::Error {
                message: "capability missing".into()
            }
        );
    }

    #[test]
    fn apply_submission_outcome_after_user_dismissed_is_noop() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('i')));
        type_chars(&mut app, "intent text");
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Char('s')));
        let _ = app.take_pending_submission();
        // User pressed Esc before the response landed.
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.mode(), &Mode::Browsing);

        app.apply_submission_outcome(SubmissionOutcome::Accepted {
            intent_id: Uuid::new_v4(),
            status: "ok".into(),
            text: "late reply".into(),
        });
        assert_eq!(
            app.mode(),
            &Mode::Browsing,
            "late response must not clobber the user's view"
        );
    }

    #[test]
    fn result_view_any_key_returns_to_browsing() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('i')));
        type_chars(&mut app, "x");
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Char('s')));
        app.apply_submission_outcome(SubmissionOutcome::Accepted {
            intent_id: Uuid::new_v4(),
            status: "ok".into(),
            text: "reply".into(),
        });
        app.on_key(press(KeyCode::Enter));
        assert_eq!(app.mode(), &Mode::Browsing);
    }

    #[test]
    fn error_view_q_returns_to_browsing_and_quits() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('i')));
        type_chars(&mut app, "x");
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Char('s')));
        app.apply_submission_outcome(SubmissionOutcome::Failed {
            message: "boom".into(),
        });
        app.on_key(press(KeyCode::Char('q')));
        assert_eq!(app.exit_reason(), Some(ExitReason::UserQuit));
    }

    #[test]
    fn handle_terminal_view_pins_q_quits_with_browsing_return_while_esc_and_other_keys_dismiss_without_quit_across_four_modes(
    ) {
        // App::handle_terminal_view (lib.rs line 765-773) is the shared
        // dismissal handler for the four terminal-view modes: Result and
        // Error (routed at line 294) and GrantResult and GrantError
        // (routed at line 374). The body has exactly two arms:
        //
        //   KeyCode::Char('q') => { self.exit = Some(UserQuit); Mode::Browsing }
        //   _ => Mode::Browsing
        //
        // The doc-comment above the fn says "any key returns to
        // Browsing" but does not document the asymmetry with
        // handle_browsing (line 659-735), where `KeyCode::Char('q') |
        // KeyCode::Esc` share the quit arm. Tests in this module pin
        // each half separately — result_view_any_key_returns_to_browsing
        // uses Enter and asserts mode (not exit_reason),
        // error_view_q_returns_to_browsing_and_quits uses 'q' and asserts
        // exit_reason (not mode). The 'q' arm is the only place
        // self.exit is set inside handle_terminal_view; the wildcard
        // arm must NOT set exit even though it returns the same Mode.
        //
        // This pin anchors three contracts across all four terminal-view
        // modes simultaneously:
        //   (a) 'q' both sets exit AND returns Mode::Browsing — a
        //       refactor that dropped the Mode::Browsing return would
        //       leave the renderer drawing the stale terminal view for
        //       one frame before the event loop saw exit_reason.
        //   (b) Esc returns to Browsing WITHOUT setting exit — pins
        //       the asymmetry with handle_browsing; a refactor that
        //       consolidated Char('q') | Esc into the quit arm would
        //       silently quit on every Esc-dismissal of a Result, Error,
        //       GrantResult, or GrantError view.
        //   (c) Arbitrary non-q non-Esc chars return to Browsing WITHOUT
        //       setting exit — a refactor that moved self.exit out of
        //       the q-arm would silently quit on any key.

        fn arrange_result() -> App {
            let mut app = App::new();
            app.on_key(press(KeyCode::Char('i')));
            type_chars(&mut app, "x");
            app.on_key(press(KeyCode::Enter));
            app.on_key(press(KeyCode::Char('s')));
            app.apply_submission_outcome(SubmissionOutcome::Accepted {
                intent_id: Uuid::new_v4(),
                status: "ok".into(),
                text: "reply".into(),
            });
            assert!(matches!(app.mode(), Mode::Result { .. }));
            assert_eq!(app.exit_reason(), None);
            app
        }

        fn arrange_error() -> App {
            let mut app = App::new();
            app.on_key(press(KeyCode::Char('i')));
            type_chars(&mut app, "x");
            app.on_key(press(KeyCode::Enter));
            app.on_key(press(KeyCode::Char('s')));
            app.apply_submission_outcome(SubmissionOutcome::Failed {
                message: "boom".into(),
            });
            assert!(matches!(app.mode(), Mode::Error { .. }));
            assert_eq!(app.exit_reason(), None);
            app
        }

        fn arrange_grant_result() -> App {
            let mut app = App::new();
            app.on_key(press(KeyCode::Char('g')));
            type_chars(&mut app, "memory.read");
            app.on_key(press(KeyCode::Enter));
            let _ = app.take_pending_grant_submission();
            app.apply_grant_outcome(GrantOutcome::Granted {
                signature_b58: "sig".into(),
                subject_display: "user@local".into(),
                action: "memory.read".into(),
            });
            assert!(matches!(app.mode(), Mode::GrantResult { .. }));
            assert_eq!(app.exit_reason(), None);
            app
        }

        fn arrange_grant_error() -> App {
            let mut app = App::new();
            app.on_key(press(KeyCode::Char('g')));
            type_chars(&mut app, "bogus.action");
            app.on_key(press(KeyCode::Enter));
            let _ = app.take_pending_grant_submission();
            app.apply_grant_outcome(GrantOutcome::Failed {
                message: "unknown action namespace".into(),
            });
            assert!(matches!(app.mode(), Mode::GrantError { .. }));
            assert_eq!(app.exit_reason(), None);
            app
        }

        let arrangers: [(&str, fn() -> App); 4] = [
            ("Result", arrange_result),
            ("Error", arrange_error),
            ("GrantResult", arrange_grant_result),
            ("GrantError", arrange_grant_error),
        ];

        for (mode_label, build) in arrangers {
            // (a) 'q' must BOTH set exit AND return Mode::Browsing.
            //     error_view_q_returns_to_browsing_and_quits only
            //     asserts exit_reason for Error; this anchors both
            //     halves of the quit arm across all four modes.
            let mut app = build();
            app.on_key(press(KeyCode::Char('q')));
            assert_eq!(
                app.mode(),
                &Mode::Browsing,
                "'q' in Mode::{mode_label} must return Mode::Browsing — \
                 a refactor that omitted the Mode::Browsing return on \
                 the quit arm (e.g., early-return after setting exit) \
                 would leave the renderer drawing the stale terminal \
                 view for one frame before the event loop noticed \
                 exit_reason; error_view_q_returns_to_browsing_and_quits \
                 only asserts exit_reason and would still pass"
            );
            assert_eq!(
                app.exit_reason(),
                Some(ExitReason::UserQuit),
                "'q' in Mode::{mode_label} must set exit_reason to \
                 UserQuit — the 'q' arm is the ONLY place \
                 handle_terminal_view sets self.exit; a refactor that \
                 moved or removed the assignment would silently break \
                 the documented terminal-view dismissal contract"
            );

            // (b) Esc must dismiss to Browsing WITHOUT setting exit.
            //     This is the load-bearing asymmetry: handle_browsing
            //     (line 659-735) treats Char('q') | Esc as a single
            //     quit arm. handle_terminal_view does not. A refactor
            //     that consolidated the two handlers' quit semantics
            //     "for consistency" would silently discard the
            //     operator's TUI session on every Esc-dismissal of a
            //     terminal view.
            let mut app = build();
            app.on_key(press(KeyCode::Esc));
            assert_eq!(
                app.mode(),
                &Mode::Browsing,
                "Esc in Mode::{mode_label} must return Mode::Browsing — \
                 wildcard arm of handle_terminal_view"
            );
            assert_eq!(
                app.exit_reason(),
                None,
                "Esc in Mode::{mode_label} must NOT quit — pins the \
                 asymmetry with handle_browsing where Esc DOES quit; \
                 a refactor that consolidated Char('q') | Esc into the \
                 quit arm under an 'Esc-is-quit for consistency' \
                 rationale would silently discard the operator's TUI \
                 session on every Esc-dismissal of a terminal view. \
                 result_view_any_key_returns_to_browsing uses Enter and \
                 would still pass; the existing tests would not catch \
                 the regression"
            );

            // (c) Arbitrary non-q non-Esc char must dismiss to Browsing
            //     WITHOUT setting exit. Anchors that the wildcard arm
            //     does not quit; a refactor that moved
            //     `self.exit = Some(UserQuit)` outside the q-arm under
            //     an 'any key dismisses and quits because terminal
            //     views are terminal' rationale would silently quit on
            //     every keystroke while inspecting a result or error.
            let mut app = build();
            app.on_key(press(KeyCode::Char('x')));
            assert_eq!(
                app.mode(),
                &Mode::Browsing,
                "non-q non-Esc char 'x' in Mode::{mode_label} must \
                 return Mode::Browsing — wildcard arm"
            );
            assert_eq!(
                app.exit_reason(),
                None,
                "non-q non-Esc char 'x' in Mode::{mode_label} must NOT \
                 quit — a refactor that moved self.exit out of the \
                 'q' arm to the wildcard or to a pre-match block would \
                 silently quit on every keystroke; \
                 result_view_any_key_returns_to_browsing only asserts \
                 the mode is Browsing (which still holds) and would \
                 not catch the regression"
            );
        }
    }

    #[test]
    fn pressing_m_enters_memory_tail_loading_and_arms_fetch() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('m')));
        assert!(
            matches!(
                app.mode(),
                Mode::MemoryTail {
                    loading: true,
                    error: None,
                    ..
                }
            ),
            "mode is {:?}",
            app.mode()
        );
        assert!(
            app.take_pending_memory_fetch(),
            "first take returns true to trigger the kickoff"
        );
        assert!(
            !app.take_pending_memory_fetch(),
            "subsequent takes return false until next 'm' press"
        );
    }

    #[test]
    fn memory_tail_records_fetched_transitions_to_loaded() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('m')));
        let _ = app.take_pending_memory_fetch();
        app.apply_memory_fetch_outcome(MemoryFetchOutcome::Fetched {
            records: Vec::new(),
        });
        assert!(
            matches!(
                app.mode(),
                Mode::MemoryTail {
                    loading: false,
                    records,
                    error: None,
                } if records.is_empty()
            ),
            "mode is {:?}",
            app.mode()
        );
    }

    #[test]
    fn memory_tail_fetch_failure_surfaces_in_embedded_error() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('m')));
        let _ = app.take_pending_memory_fetch();
        app.apply_memory_fetch_outcome(MemoryFetchOutcome::Failed {
            message: "memory read requires capability \"memory.read\"".into(),
        });
        assert!(
            matches!(
                app.mode(),
                Mode::MemoryTail {
                    loading: false,
                    error: Some(_),
                    ..
                }
            ),
            "mode is {:?}",
            app.mode()
        );
    }

    #[test]
    fn memory_tail_q_returns_to_browsing() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('m')));
        let _ = app.take_pending_memory_fetch();
        app.on_key(press(KeyCode::Char('q')));
        assert_eq!(app.mode(), &Mode::Browsing);
    }

    #[test]
    fn memory_tail_esc_returns_to_browsing() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('m')));
        let _ = app.take_pending_memory_fetch();
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.mode(), &Mode::Browsing);
    }

    #[test]
    fn memory_tail_late_response_after_dismissal_is_noop() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('m')));
        let _ = app.take_pending_memory_fetch();
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.mode(), &Mode::Browsing);
        app.apply_memory_fetch_outcome(MemoryFetchOutcome::Fetched {
            records: Vec::new(),
        });
        assert_eq!(
            app.mode(),
            &Mode::Browsing,
            "late memory response must not clobber the user's view"
        );
    }

    #[test]
    fn pressing_a_enters_audit_tail_loading_and_arms_fetch() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('a')));
        assert!(
            matches!(
                app.mode(),
                Mode::AuditTail {
                    loading: true,
                    error: None,
                    ..
                }
            ),
            "mode is {:?}",
            app.mode()
        );
        assert!(app.take_pending_audit_fetch());
        assert!(!app.take_pending_audit_fetch());
    }

    #[test]
    fn audit_tail_q_returns_to_browsing() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('a')));
        let _ = app.take_pending_audit_fetch();
        app.on_key(press(KeyCode::Char('q')));
        assert_eq!(app.mode(), &Mode::Browsing);
    }

    #[test]
    fn audit_tail_fetch_failure_surfaces_in_embedded_error() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('a')));
        let _ = app.take_pending_audit_fetch();
        app.apply_audit_fetch_outcome(AuditFetchOutcome::Failed {
            message: "wire error".into(),
        });
        assert!(
            matches!(
                app.mode(),
                Mode::AuditTail {
                    loading: false,
                    error: Some(_),
                    ..
                }
            ),
            "mode is {:?}",
            app.mode()
        );
    }

    #[test]
    fn audit_tail_late_response_after_dismissal_is_noop() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('a')));
        let _ = app.take_pending_audit_fetch();
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.mode(), &Mode::Browsing);
        app.apply_audit_fetch_outcome(AuditFetchOutcome::Fetched { events: Vec::new() });
        assert_eq!(app.mode(), &Mode::Browsing);
    }

    #[test]
    fn pressing_c_enters_capabilities_tail_and_arms_fetch() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('c')));
        assert!(
            matches!(
                app.mode(),
                Mode::CapabilitiesTail {
                    loading: true,
                    error: None,
                    ..
                }
            ),
            "mode is {:?}",
            app.mode()
        );
        assert!(app.take_pending_capabilities_fetch());
        assert!(!app.take_pending_capabilities_fetch());
    }

    #[test]
    fn capabilities_tail_q_returns_to_browsing() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('c')));
        let _ = app.take_pending_capabilities_fetch();
        app.on_key(press(KeyCode::Char('q')));
        assert_eq!(app.mode(), &Mode::Browsing);
    }

    #[test]
    fn capabilities_tail_fetch_failure_surfaces_in_embedded_error() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('c')));
        let _ = app.take_pending_capabilities_fetch();
        app.apply_capabilities_fetch_outcome(CapabilitiesFetchOutcome::Failed {
            message: "wire boom".into(),
        });
        assert!(
            matches!(
                app.mode(),
                Mode::CapabilitiesTail {
                    loading: false,
                    error: Some(_),
                    ..
                }
            ),
            "mode is {:?}",
            app.mode()
        );
    }

    #[test]
    fn capabilities_tail_late_response_after_dismissal_is_noop() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('c')));
        let _ = app.take_pending_capabilities_fetch();
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.mode(), &Mode::Browsing);
        app.apply_capabilities_fetch_outcome(CapabilitiesFetchOutcome::Fetched {
            capabilities: Vec::new(),
        });
        assert_eq!(app.mode(), &Mode::Browsing);
    }

    #[test]
    fn pressing_g_in_browsing_enters_grant_editor_with_empty_buffer() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('g')));
        assert_eq!(
            app.mode(),
            &Mode::GrantEditor {
                buffer: String::new()
            }
        );
    }

    #[test]
    fn grant_editor_accumulates_chars() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('g')));
        type_chars(&mut app, "memory.read");
        assert_eq!(
            app.mode(),
            &Mode::GrantEditor {
                buffer: "memory.read".into()
            }
        );
    }

    #[test]
    fn grant_editor_esc_returns_to_browsing_without_submitting() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('g')));
        type_chars(&mut app, "memory.read");
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.mode(), &Mode::Browsing);
        assert!(app.take_pending_grant_submission().is_none());
    }

    #[test]
    fn grant_editor_enter_on_non_empty_buffer_transitions_to_submitting() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('g')));
        type_chars(&mut app, "memory.read");
        app.on_key(press(KeyCode::Enter));
        assert_eq!(
            app.mode(),
            &Mode::GrantSubmitting {
                action: "memory.read".into()
            }
        );
        assert_eq!(
            app.take_pending_grant_submission(),
            Some("memory.read".into())
        );
        assert_eq!(app.take_pending_grant_submission(), None);
    }

    #[test]
    fn grant_editor_enter_on_empty_or_whitespace_stays_in_editor() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('g')));
        app.on_key(press(KeyCode::Enter));
        assert!(
            matches!(app.mode(), Mode::GrantEditor { buffer } if buffer.is_empty()),
            "Enter on empty buffer must not submit; mode is {:?}",
            app.mode()
        );

        type_chars(&mut app, "   ");
        app.on_key(press(KeyCode::Enter));
        assert!(
            matches!(app.mode(), Mode::GrantEditor { buffer } if buffer.is_empty()),
            "Enter on whitespace-only buffer must reset and stay in editor; mode is {:?}",
            app.mode()
        );
    }

    #[test]
    fn grant_outcome_granted_transitions_to_grant_result() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('g')));
        type_chars(&mut app, "memory.read");
        app.on_key(press(KeyCode::Enter));
        let _ = app.take_pending_grant_submission();
        app.apply_grant_outcome(GrantOutcome::Granted {
            signature_b58: "sig123".into(),
            subject_display: "user@local".into(),
            action: "memory.read".into(),
        });
        assert_eq!(
            app.mode(),
            &Mode::GrantResult {
                action: "memory.read".into(),
                subject_display: "user@local".into(),
                signature_b58: "sig123".into(),
            }
        );
    }

    #[test]
    fn grant_outcome_failed_transitions_to_grant_error() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('g')));
        type_chars(&mut app, "bogus.action"); // Invalid namespace
        app.on_key(press(KeyCode::Enter));
        let _ = app.take_pending_grant_submission();
        app.apply_grant_outcome(GrantOutcome::Failed {
            message: "unknown action namespace".into(),
        });
        assert_eq!(
            app.mode(),
            &Mode::GrantError {
                message: "unknown action namespace".into()
            }
        );
    }

    #[test]
    fn grant_late_response_after_dismissal_is_noop() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('g')));
        type_chars(&mut app, "memory.read");
        app.on_key(press(KeyCode::Enter));
        let _ = app.take_pending_grant_submission();
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.mode(), &Mode::Browsing);

        app.apply_grant_outcome(GrantOutcome::Granted {
            signature_b58: "late".into(),
            subject_display: "user@local".into(),
            action: "memory.read".into(),
        });
        assert_eq!(
            app.mode(),
            &Mode::Browsing,
            "late grant response must not clobber the user's view"
        );
    }

    fn sample_a2a_task() -> A2ATask {
        use covenant_types::AgentId;
        A2ATask {
            id: Uuid::new_v4(),
            sender: AgentId::new("user@local", [1u8; 32]),
            recipient: AgentId::new("user@local", [1u8; 32]),
            intent_text: "ping".into(),
            task_kind: Some("ping".into()),
            parent: None,
            deadline_ms: None,
            idempotency: None,
        }
    }

    #[test]
    fn pressing_shift_a_enters_a2a_tail_loading_and_arms_fetch() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('A')));
        assert!(
            matches!(
                app.mode(),
                Mode::A2aTail {
                    loading: true,
                    error: None,
                    ..
                }
            ),
            "mode is {:?}",
            app.mode()
        );
        assert!(app.take_pending_a2a_fetch());
        assert!(!app.take_pending_a2a_fetch());
    }

    #[test]
    fn lowercase_a_still_enters_audit_tail_not_a2a_tail() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('a')));
        assert!(
            matches!(app.mode(), Mode::AuditTail { .. }),
            "lowercase 'a' must keep its existing AuditTail binding; mode is {:?}",
            app.mode()
        );
    }

    #[test]
    fn a2a_tail_fetched_transitions_to_loaded() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('A')));
        let _ = app.take_pending_a2a_fetch();
        let task = sample_a2a_task();
        app.apply_a2a_fetch_outcome(A2aFetchOutcome::Fetched {
            tasks: vec![task.clone()],
        });
        assert!(
            matches!(
                app.mode(),
                Mode::A2aTail {
                    loading: false,
                    error: None,
                    tasks,
                } if tasks.len() == 1 && tasks[0].id == task.id
            ),
            "mode is {:?}",
            app.mode()
        );
    }

    #[test]
    fn a2a_tail_fetch_failure_surfaces_in_embedded_error() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('A')));
        let _ = app.take_pending_a2a_fetch();
        app.apply_a2a_fetch_outcome(A2aFetchOutcome::Failed {
            message: "wire boom".into(),
        });
        assert!(
            matches!(
                app.mode(),
                Mode::A2aTail {
                    loading: false,
                    error: Some(_),
                    ..
                }
            ),
            "mode is {:?}",
            app.mode()
        );
    }

    #[test]
    fn a2a_tail_q_returns_to_browsing() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('A')));
        let _ = app.take_pending_a2a_fetch();
        app.on_key(press(KeyCode::Char('q')));
        assert_eq!(app.mode(), &Mode::Browsing);
    }

    #[test]
    fn a2a_tail_esc_returns_to_browsing() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('A')));
        let _ = app.take_pending_a2a_fetch();
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.mode(), &Mode::Browsing);
    }

    #[test]
    fn a2a_tail_late_response_after_dismissal_is_noop() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('A')));
        let _ = app.take_pending_a2a_fetch();
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.mode(), &Mode::Browsing);
        app.apply_a2a_fetch_outcome(A2aFetchOutcome::Fetched { tasks: Vec::new() });
        assert_eq!(app.mode(), &Mode::Browsing);
    }

    fn sample_receipt() -> SettlementReceipt {
        use covenant_types::{AgentId, ResourceKind};
        SettlementReceipt {
            id: Uuid::new_v4(),
            payer: AgentId::new("user@local", [2u8; 32]),
            resource: ResourceKind::Memory,
            memory_record_id: Some(Uuid::new_v4()),
            credits_consumed: 1,
            settled_at: 1_700_000_000,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: None,
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
        }
    }

    #[test]
    fn pressing_r_enters_receipts_tail_loading_and_arms_fetch() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('r')));
        assert!(
            matches!(
                app.mode(),
                Mode::ReceiptsTail {
                    loading: true,
                    error: None,
                    ..
                }
            ),
            "mode is {:?}",
            app.mode()
        );
        assert!(app.take_pending_receipts_fetch());
        assert!(!app.take_pending_receipts_fetch());
    }

    #[test]
    fn receipts_tail_fetched_transitions_to_loaded() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('r')));
        let _ = app.take_pending_receipts_fetch();
        let receipt = sample_receipt();
        app.apply_receipts_fetch_outcome(ReceiptsFetchOutcome::Fetched {
            receipts: vec![receipt.clone()],
        });
        assert!(
            matches!(
                app.mode(),
                Mode::ReceiptsTail {
                    loading: false,
                    error: None,
                    receipts,
                } if receipts.len() == 1 && receipts[0].id == receipt.id
            ),
            "mode is {:?}",
            app.mode()
        );
    }

    #[test]
    fn receipts_tail_fetch_failure_surfaces_in_embedded_error() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('r')));
        let _ = app.take_pending_receipts_fetch();
        app.apply_receipts_fetch_outcome(ReceiptsFetchOutcome::Failed {
            message: "receipt reads require capability \"chain.receipts\"".into(),
        });
        assert!(
            matches!(
                app.mode(),
                Mode::ReceiptsTail {
                    loading: false,
                    error: Some(_),
                    ..
                }
            ),
            "mode is {:?}",
            app.mode()
        );
    }

    #[test]
    fn receipts_tail_q_returns_to_browsing() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('r')));
        let _ = app.take_pending_receipts_fetch();
        app.on_key(press(KeyCode::Char('q')));
        assert_eq!(app.mode(), &Mode::Browsing);
    }

    #[test]
    fn receipts_tail_esc_returns_to_browsing() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('r')));
        let _ = app.take_pending_receipts_fetch();
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.mode(), &Mode::Browsing);
    }

    #[test]
    fn receipts_tail_late_response_after_dismissal_is_noop() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('r')));
        let _ = app.take_pending_receipts_fetch();
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.mode(), &Mode::Browsing);
        app.apply_receipts_fetch_outcome(ReceiptsFetchOutcome::Fetched {
            receipts: Vec::new(),
        });
        assert_eq!(app.mode(), &Mode::Browsing);
    }

    fn sample_peer() -> PeerSummary {
        use covenant_types::AgentId;
        PeerSummary {
            agent_id: AgentId::new("user@local", [3u8; 32]),
            token_prefix: "abcdef".into(),
            registered_at: 1_700_000_000,
            revoked_at: None,
        }
    }

    #[test]
    fn pressing_p_enters_peers_tail_loading_and_arms_fetch() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('p')));
        assert!(
            matches!(
                app.mode(),
                Mode::PeersTail {
                    loading: true,
                    error: None,
                    truncated: false,
                    ..
                }
            ),
            "mode is {:?}",
            app.mode()
        );
        assert!(app.take_pending_peers_fetch());
        assert!(!app.take_pending_peers_fetch());
    }

    #[test]
    fn peers_tail_fetched_transitions_to_loaded() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('p')));
        let _ = app.take_pending_peers_fetch();
        let peer = sample_peer();
        app.apply_peers_fetch_outcome(PeersFetchOutcome::Fetched {
            peers: vec![peer.clone()],
            truncated: true,
        });
        assert!(
            matches!(
                app.mode(),
                Mode::PeersTail {
                    loading: false,
                    error: None,
                    truncated: true,
                    peers,
                } if peers.len() == 1 && peers[0].agent_id.pubkey == peer.agent_id.pubkey
            ),
            "mode is {:?}",
            app.mode()
        );
    }

    #[test]
    fn peers_tail_fetched_truncated_false_clears_initial_false() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('p')));
        let _ = app.take_pending_peers_fetch();
        match app.mode() {
            Mode::PeersTail {
                loading: true,
                truncated: false,
                ..
            } => {}
            other => {
                panic!("initial peers-tail state must be loading=true, truncated=false: {other:?}")
            }
        }
        let peer = sample_peer();
        app.apply_peers_fetch_outcome(PeersFetchOutcome::Fetched {
            peers: vec![peer.clone()],
            truncated: false,
        });
        match app.mode() {
            Mode::PeersTail {
                loading: false,
                truncated,
                peers,
                error: None,
            } => {
                assert!(
                    !truncated,
                    "truncated=false outcome must propagate verbatim"
                );
                assert_eq!(peers.len(), 1);
                assert_eq!(peers[0].agent_id.pubkey, peer.agent_id.pubkey);
            }
            other => panic!("expected loaded peers-tail with truncated=false: {other:?}"),
        }
        assert_eq!(
            app.mode().name(),
            "peers-tail",
            "Mode::name() must report the stable 'peers-tail' discriminant string the status bar reads — a rename here is the audit signal for downstream UI"
        );
    }

    #[test]
    fn peers_tail_fetched_truncated_true_flips_initial_false() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('p')));
        let _ = app.take_pending_peers_fetch();
        match app.mode() {
            Mode::PeersTail {
                loading: true,
                truncated: false,
                ..
            } => {}
            other => {
                panic!("initial peers-tail state must be loading=true, truncated=false: {other:?}")
            }
        }
        let peer = sample_peer();
        app.apply_peers_fetch_outcome(PeersFetchOutcome::Fetched {
            peers: vec![peer.clone()],
            truncated: true,
        });
        match app.mode() {
            Mode::PeersTail {
                loading: false,
                truncated,
                peers,
                error: None,
            } => {
                assert!(
                    *truncated,
                    "truncated=true outcome must propagate verbatim — the renderer's banner toggle reads this flag",
                );
                assert_eq!(peers.len(), 1);
                assert_eq!(peers[0].agent_id.pubkey, peer.agent_id.pubkey);
            }
            other => panic!("expected loaded peers-tail with truncated=true: {other:?}"),
        }
        assert_eq!(
            app.mode().name(),
            "peers-tail",
            "Mode::name() must remain 'peers-tail' when truncated=true so the status bar discriminant does not depend on payload state",
        );
    }

    #[test]
    fn peers_tail_fetch_failure_surfaces_in_embedded_error() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('p')));
        let _ = app.take_pending_peers_fetch();
        app.apply_peers_fetch_outcome(PeersFetchOutcome::Failed {
            message: "peers.list is operator-only".into(),
        });
        assert!(
            matches!(
                app.mode(),
                Mode::PeersTail {
                    loading: false,
                    error: Some(_),
                    truncated: false,
                    ..
                }
            ),
            "mode is {:?}",
            app.mode()
        );
    }

    #[test]
    fn peers_tail_q_returns_to_browsing() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('p')));
        let _ = app.take_pending_peers_fetch();
        app.on_key(press(KeyCode::Char('q')));
        assert_eq!(app.mode(), &Mode::Browsing);
    }

    #[test]
    fn peers_tail_esc_returns_to_browsing() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('p')));
        let _ = app.take_pending_peers_fetch();
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.mode(), &Mode::Browsing);
    }

    #[test]
    fn peers_tail_late_response_after_dismissal_is_noop() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('p')));
        let _ = app.take_pending_peers_fetch();
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.mode(), &Mode::Browsing);
        app.apply_peers_fetch_outcome(PeersFetchOutcome::Fetched {
            peers: Vec::new(),
            truncated: false,
        });
        assert_eq!(app.mode(), &Mode::Browsing);
    }

    #[test]
    fn pressing_question_mark_in_browsing_enters_help() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('?')));
        assert_eq!(app.mode(), &Mode::Help);
    }

    #[test]
    fn any_key_in_help_returns_to_browsing() {
        for key in [
            KeyCode::Char('?'),
            KeyCode::Char('q'),
            KeyCode::Char('x'),
            KeyCode::Esc,
            KeyCode::Enter,
            KeyCode::Tab,
        ] {
            let mut app = App::new();
            app.on_key(press(KeyCode::Char('?')));
            assert_eq!(app.mode(), &Mode::Help);
            app.on_key(press(key));
            assert_eq!(
                app.mode(),
                &Mode::Browsing,
                "key {key:?} did not return to Browsing"
            );
        }
    }

    #[test]
    fn question_mark_inside_intent_editor_is_a_literal_char() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('i')));
        app.on_key(press(KeyCode::Char('?')));
        assert_eq!(app.mode(), &Mode::Editing { buffer: "?".into() });
    }

    #[test]
    fn question_mark_inside_grant_editor_is_a_literal_char() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('g')));
        app.on_key(press(KeyCode::Char('?')));
        assert_eq!(app.mode(), &Mode::GrantEditor { buffer: "?".into() });
    }

    #[test]
    fn help_bindings_are_single_char_keys() {
        // The renderer formats each binding with a width-4 key column;
        // a multi-char key (e.g. "Ctrl-X") would silently overflow.
        // Hard-coding single chars also matches the KeyCode::Char arms
        // in handle_browsing. The reverse direction — every Browsing
        // key has an entry — is enforced by review during the diff
        // that adds the binding.
        for (key, desc) in HELP_BINDINGS {
            assert_eq!(
                key.chars().count(),
                1,
                "help key must be a single char: {key:?}"
            );
            assert!(!desc.is_empty(), "help description must not be empty");
        }
    }

    #[test]
    fn help_bindings_pins_eleven_exact_key_description_pairs_in_declaration_order() {
        // HELP_BINDINGS (line 180-192) is the pub const both
        // handle_browsing (line 659+) and the help renderer in main.rs
        // (line 493-499) consume. The renderer formats each entry as
        // `format!("  {key:<4}  {desc}")` and surfaces the overlay
        // operators press `?` to view; declaration order IS the visual
        // order operators learn.
        //
        // help_bindings_are_single_char_keys (above) iterates the slice
        // and asserts each key is a single char and each description
        // is non-empty, but does NOT pin the count, the exact (key,
        // description) pairs, or the order. A refactor that removed
        // an entry, renamed a binding (e.g., 'i' draft intent → 'I'
        // for capitalization-consistency with 'A' a2a inbox),
        // reordered the slice under a 'group by category' rationale,
        // or rewrote a description (e.g., 'draft intent' → 'compose
        // intent') would silently shift the operator-facing UI while
        // the existing structural pin continued to pass.

        assert_eq!(
            HELP_BINDINGS.len(),
            11,
            "HELP_BINDINGS must contain exactly 11 entries — the \
             documented operator-facing keybinding set (i, s, g, m, \
             a, c, A, r, p, ?, q). A refactor that removed an entry \
             would silently hide the documented action; a refactor \
             that added one without coordinating with handle_browsing \
             would silently advertise an inert binding. The count \
             arm catches both regression classes before the exact-pair \
             arm runs",
        );

        assert_eq!(
            HELP_BINDINGS,
            &[
                ("i", "draft intent"),
                ("s", "submit most recent draft"),
                ("g", "grant capability"),
                ("m", "memory tail"),
                ("a", "audit tail"),
                ("c", "capabilities tail"),
                ("A", "a2a inbox"),
                ("r", "chain receipts"),
                ("p", "peers"),
                ("?", "this help"),
                ("q", "quit"),
            ],
            "HELP_BINDINGS must match the documented (key, description) \
             pairs in declaration order. A refactor that reordered the \
             slice (e.g., alphabetized by key) would silently shift \
             the visual order operators learn from the overlay; a \
             refactor that rewrote a description (e.g., 'draft intent' \
             → 'compose intent') would silently shift user-facing \
             terminology without coordinating with operator training \
             material; a refactor that renamed a key (e.g., 'i' → 'I') \
             without updating handle_browsing would silently break \
             muscle memory while the existing structural pin still \
             passes",
        );

        // Cross-bind: every key must be unique so handle_browsing's
        // KeyCode::Char arms don't shadow each other and the rendered
        // overlay doesn't show the same key twice with different
        // actions.
        let keys: std::collections::BTreeSet<&str> =
            HELP_BINDINGS.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            keys.len(),
            HELP_BINDINGS.len(),
            "HELP_BINDINGS keys must be unique — a refactor that \
             added a duplicate key (e.g., a second 'a' for 'audit \
             archive') would silently shadow the original binding in \
             handle_browsing's match cascade, surfacing one action \
             twice in the overlay while the other action becomes \
             inert",
        );
    }

    #[test]
    fn map_socket_error_classifies_not_found_as_daemon_not_running() {
        use crate::ipc::{map_socket_error, IpcError};
        use std::io;
        use std::path::PathBuf;
        let err = io::Error::new(io::ErrorKind::NotFound, "no such file");
        let path = PathBuf::from("/tmp/sock");
        match map_socket_error(err, &path) {
            IpcError::DaemonNotRunning { sock_path } => assert_eq!(sock_path, path),
            other => panic!("expected DaemonNotRunning, got {other:?}"),
        }
    }

    #[test]
    fn map_socket_error_classifies_connection_refused_as_daemon_not_running() {
        use crate::ipc::{map_socket_error, IpcError};
        use std::io;
        use std::path::PathBuf;
        let err = io::Error::new(io::ErrorKind::ConnectionRefused, "refused");
        let path = PathBuf::from("/tmp/sock");
        match map_socket_error(err, &path) {
            IpcError::DaemonNotRunning { sock_path } => assert_eq!(sock_path, path),
            other => panic!("expected DaemonNotRunning, got {other:?}"),
        }
    }

    #[test]
    fn map_socket_error_passes_other_io_kinds_through() {
        use crate::ipc::{map_socket_error, IpcError};
        use std::io;
        use std::path::PathBuf;
        let err = io::Error::new(io::ErrorKind::PermissionDenied, "denied");
        let path = PathBuf::from("/tmp/sock");
        match map_socket_error(err, &path) {
            IpcError::Wire(inner) => {
                assert_eq!(inner.kind(), io::ErrorKind::PermissionDenied);
            }
            other => panic!("expected Wire, got {other:?}"),
        }
    }

    #[test]
    fn recent_limit_caps_pin_six_documented_values_and_audit_is_strictly_highest() {
        // covenant_tui::ipc declares six pub const RECENT_*_LIMIT_CAP at
        // ipc.rs lines 78-103, each .min()'d in the corresponding
        // recent_X async fn (line 350 memory, 385 audit, 428 caps,
        // 464 a2a, 499 receipts, 544 peers). These are the operator-
        // side hard ceiling on IPC request limit fields, so a runaway
        // TUI request cannot ask the daemon to enumerate the entire
        // memory/audit/etc table into one IPC frame. The doc-comments
        // name the relative ordering: audit is set higher than memory
        // because audit grows faster; capabilities/peers match memory
        // because they grow slowly. No prior test in this crate
        // references any of the six caps. A refactor that lowered any
        // cap to 0 (callers-must-always-pass-limit) would collapse
        // every TUI fetch to an empty page; a refactor that raised
        // one to e.g. 10000 (we-have-headroom) would push unbounded
        // payloads through IPC; a refactor that swapped two constants
        // (alphabetize the block) would silently route memory traffic
        // through the audit cap. This pin anchors each value AND the
        // audit-strictly-highest ordering across the constants block.
        use crate::ipc::{
            RECENT_A2A_LIMIT_CAP, RECENT_AUDIT_LIMIT_CAP, RECENT_CAPABILITIES_LIMIT_CAP,
            RECENT_MEMORY_LIMIT_CAP, RECENT_PEERS_LIMIT_CAP, RECENT_RECEIPTS_LIMIT_CAP,
        };

        assert_eq!(
            RECENT_MEMORY_LIMIT_CAP, 50,
            "RECENT_MEMORY_LIMIT_CAP must remain 50 — caps \
             Request::RecentMemory::limit so a runaway TUI memory-tail \
             fetch cannot ask the daemon to enumerate the entire \
             memory table; a refactor that lowered this to 0 would \
             collapse memory-tail to an empty view, raising it would \
             let unbounded payloads through IPC"
        );
        assert_eq!(
            RECENT_AUDIT_LIMIT_CAP, 100,
            "RECENT_AUDIT_LIMIT_CAP must remain 100 — caps \
             Request::RecentAudit::limit; doc-comment notes audit \
             volume grows faster than memory, so the cap is set higher. \
             A refactor that pulled this down to memory's 50 under an \
             'unify caps' pass would silently halve operator audit \
             visibility"
        );
        assert_eq!(
            RECENT_CAPABILITIES_LIMIT_CAP, 50,
            "RECENT_CAPABILITIES_LIMIT_CAP must remain 50 — caps \
             Request::RecentCapabilities::limit; doc-comment says it \
             matches recent_memory because capabilities grow slowly \
             (one row per grant)"
        );
        assert_eq!(
            RECENT_A2A_LIMIT_CAP, 50,
            "RECENT_A2A_LIMIT_CAP must remain 50 — caps \
             Request::RecentA2ATasks::limit; the mailbox is server-\
             side filtered to tasks the operator sent or received, so \
             it grows linearly with A2A traffic"
        );
        assert_eq!(
            RECENT_RECEIPTS_LIMIT_CAP, 50,
            "RECENT_RECEIPTS_LIMIT_CAP must remain 50 — caps \
             Request::RecentReceipts::limit; receipts are server-side \
             filtered to rows where the payer matches the calling \
             peer, one row per resource consumption event"
        );
        assert_eq!(
            RECENT_PEERS_LIMIT_CAP, 50,
            "RECENT_PEERS_LIMIT_CAP must remain 50 — caps \
             Request::ListPeers::limit; doc-comment says it matches \
             receipts so the TUI never asks the daemon to enumerate \
             an unbounded registry in one frame"
        );

        let non_audit = [
            ("RECENT_MEMORY_LIMIT_CAP", RECENT_MEMORY_LIMIT_CAP),
            (
                "RECENT_CAPABILITIES_LIMIT_CAP",
                RECENT_CAPABILITIES_LIMIT_CAP,
            ),
            ("RECENT_A2A_LIMIT_CAP", RECENT_A2A_LIMIT_CAP),
            ("RECENT_RECEIPTS_LIMIT_CAP", RECENT_RECEIPTS_LIMIT_CAP),
            ("RECENT_PEERS_LIMIT_CAP", RECENT_PEERS_LIMIT_CAP),
        ];

        for (name, cap) in non_audit {
            assert!(
                RECENT_AUDIT_LIMIT_CAP > cap,
                "RECENT_AUDIT_LIMIT_CAP ({audit}) must be strictly \
                 greater than {name} ({cap}) — pins the doc-comment \
                 ordering that audit grows faster than memory and \
                 every other request, so its cap is the unique \
                 maximum across the six. A refactor that swapped \
                 RECENT_AUDIT_LIMIT_CAP with one of the others under \
                 an 'alphabetize the constants block' pass would \
                 silently route audit requests through the lower cap \
                 and the other request type through the audit cap",
                audit = RECENT_AUDIT_LIMIT_CAP,
            );
        }

        let equal_pairs = [
            (
                "RECENT_CAPABILITIES_LIMIT_CAP",
                RECENT_CAPABILITIES_LIMIT_CAP,
            ),
            ("RECENT_A2A_LIMIT_CAP", RECENT_A2A_LIMIT_CAP),
            ("RECENT_RECEIPTS_LIMIT_CAP", RECENT_RECEIPTS_LIMIT_CAP),
            ("RECENT_PEERS_LIMIT_CAP", RECENT_PEERS_LIMIT_CAP),
        ];
        for (name, cap) in equal_pairs {
            assert_eq!(
                cap,
                RECENT_MEMORY_LIMIT_CAP,
                "{name} ({cap}) must equal RECENT_MEMORY_LIMIT_CAP \
                 ({memory}) — the five non-audit caps share a single \
                 documented page-size baseline (capabilities matches \
                 memory; peers matches receipts; the doc-comments \
                 anchor the equality). A refactor that nudged one \
                 cap independently under a 'tune this one type' pass \
                 would silently break the documented uniformity",
                memory = RECENT_MEMORY_LIMIT_CAP,
            );
        }
    }

    #[test]
    fn ipc_error_daemon_not_running_message_includes_path_and_hint() {
        use crate::ipc::IpcError;
        use std::path::PathBuf;
        let err = IpcError::DaemonNotRunning {
            sock_path: PathBuf::from("/tmp/covenant/sock"),
        };
        let message = format!("{err}");
        assert!(
            message.contains("/tmp/covenant/sock"),
            "message must surface the sock path: {message}"
        );
        assert!(
            message.contains("covenantd start"),
            "message must hint at the fix: {message}"
        );
    }

    #[test]
    fn ipc_error_display_messages_pin_three_remaining_variant_hints_and_paths() {
        // IpcError (ipc.rs lines 32-51) has six variants. The existing
        // ipc_error_daemon_not_running_message_includes_path_and_hint
        // pins DaemonNotRunning; this pin covers the three remaining
        // string-bearing variants whose #[error] format strings carry
        // operator-facing recovery hints that the doc-comment at
        // ipc.rs lines 22-30 explains but no test anchors. A thiserror
        // format-string typo, a dropped hint, or a swapped field
        // binding would all degrade operator diagnostics silently —
        // each surfaces as the same generic Display string until an
        // operator hits the error in production.
        use crate::ipc::IpcError;
        use std::io;
        use std::path::PathBuf;

        let err = IpcError::HomeNotSet;
        let message = format!("{err}");
        assert_eq!(
            message, "HOME is not set; set $COVENANT_HOME or $HOME",
            "HomeNotSet Display message must remain literal — the dual \
             env-var hint is load-bearing because covenant_home() (ipc.rs \
             line 189-195) tries COVENANT_HOME first then falls back to \
             HOME, and an operator hitting this error needs both names \
             surfaced verbatim. A typo that dropped the 'COVENANT_' \
             prefix or that reordered the two env vars under a 'sort \
             alphabetically' pass would silently send operators down the \
             wrong recovery path"
        );

        let token_path = PathBuf::from("/tmp/covenant/peers/operator.token");
        let err = IpcError::OperatorTokenEmpty {
            path: token_path.clone(),
        };
        let message = format!("{err}");
        assert!(
            message.contains("/tmp/covenant/peers/operator.token"),
            "OperatorTokenEmpty must surface the path: {message}"
        );
        assert!(
            message.contains("empty"),
            "OperatorTokenEmpty must call out the empty state — the \
             variant exists specifically because a zero-byte token file \
             is a corruption signal distinct from a missing or unreadable \
             file: {message}"
        );
        assert!(
            message.contains("rotate"),
            "OperatorTokenEmpty must mention the 'rotate' recovery verb \
             — the bootstrap recovery contract (rotate the token OR \
             rebootstrap the daemon) lives only in this error string; \
             a thiserror format rewrite that simplified to 'operator \
             token is empty' under a 'less verbose error messages' pass \
             would silently leave operators with no actionable hint: \
             {message}"
        );
        assert!(
            message.contains("rebootstrap"),
            "OperatorTokenEmpty must also mention 'rebootstrap' — the \
             alternative recovery path when rotation isn't possible \
             (e.g., the daemon's identity key is also corrupted). Both \
             verbs must remain because they branch to different \
             recovery flows: {message}"
        );

        let err = IpcError::OperatorTokenRead {
            path: token_path.clone(),
            source: io::Error::new(io::ErrorKind::PermissionDenied, "denied"),
        };
        let message = format!("{err}");
        assert!(
            message.contains("/tmp/covenant/peers/operator.token"),
            "OperatorTokenRead must surface the path in the path slot, \
             not the source slot — a #[error] format swap that bound \
             {{source}} to the path position would emit messages like \
             'read operator token at denied: /tmp/...' instead of \
             'read operator token at /tmp/...: denied', sending \
             operators investigating filesystem state instead of the \
             io::Error cause: {message}"
        );
        assert!(
            message.contains("denied"),
            "OperatorTokenRead must surface the wrapped io::Error \
             message — the variant exists specifically to propagate \
             the underlying cause (PermissionDenied, NotADirectory, \
             InvalidData, etc.) to the operator with path context. A \
             format-string refactor that dropped the {{source}} slot \
             under a 'paths are enough context' pass would silently \
             strip the io::Error reason: {message}"
        );
        assert!(
            message.contains("read operator token"),
            "OperatorTokenRead must keep the 'read operator token' \
             prefix — distinguishes this variant from the other \
             token-related variants (OperatorTokenEmpty) in operator \
             dashboards that group errors by message prefix: {message}"
        );
    }

    #[test]
    fn mode_name_is_kebab_case_and_nonempty_for_every_variant() {
        use covenant_types::AgentId;
        // Constructing every variant locally guarantees a compile error
        // if a new variant lands without a Mode::name() arm — the
        // exhaustive match inside Mode::name itself fails first, and
        // this test fails second if someone returns an empty string.
        let variants: Vec<Mode> = vec![
            Mode::Browsing,
            Mode::Editing {
                buffer: String::new(),
            },
            Mode::Submitting { text: "x".into() },
            Mode::Result {
                intent_id: Uuid::nil(),
                status: "ok".into(),
                text: "x".into(),
            },
            Mode::Error {
                message: "x".into(),
            },
            Mode::MemoryTail {
                loading: false,
                records: Vec::new(),
                error: None,
            },
            Mode::AuditTail {
                loading: false,
                events: Vec::new(),
                error: None,
            },
            Mode::CapabilitiesTail {
                loading: false,
                capabilities: Vec::new(),
                error: None,
            },
            Mode::A2aTail {
                loading: false,
                tasks: Vec::new(),
                error: None,
            },
            Mode::ReceiptsTail {
                loading: false,
                receipts: Vec::new(),
                error: None,
            },
            Mode::GrantEditor {
                buffer: String::new(),
            },
            Mode::GrantSubmitting { action: "x".into() },
            Mode::GrantResult {
                action: "x".into(),
                subject_display: AgentId::new("user@local", [0u8; 32]).display,
                signature_b58: "x".into(),
            },
            Mode::GrantError {
                message: "x".into(),
            },
            Mode::Help,
        ];

        let mut seen = std::collections::HashSet::new();
        for m in &variants {
            let name = m.name();
            assert!(!name.is_empty(), "mode {m:?} has empty name");
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "mode name must be kebab-case [a-z0-9-]: {name}"
            );
            assert!(
                seen.insert(name),
                "duplicate mode name: {name} (two variants share it)"
            );
        }
    }

    #[test]
    fn mode_name_pins_exact_discriminant_string_for_each_of_sixteen_variants() {
        use covenant_types::AgentId;
        // Mode::name (lib.rs line 148-167) maps every Mode variant to a
        // stable kebab-case discriminant string the binary's status bar
        // reads. The kebab-case test above pins SHAPE (charset,
        // non-empty, uniqueness across its variants vec) and silently
        // omits Mode::PeersTail from that vec, so PeersTail never
        // participates in the shape gate. The only direct
        // exact-string anchors elsewhere are two indirect assertions
        // for "peers-tail" (lines 1828, 1871); the other fifteen
        // variants have no per-variant exact-string pin.
        //
        // A rename like "audit-tail" -> "audittail" passes the
        // kebab-case shape gate; a "consolidate Result and Error into
        // outcome" refactor collapses two distinct discriminants until
        // the uniqueness arm fires. The renames are precisely the
        // silent-status-bar-drift regressions the shape gate was
        // designed for — the missing piece is the exact-string anchor
        // that prevents either arm from drifting independently.
        let cases: Vec<(Mode, &'static str)> = vec![
            (Mode::Browsing, "browsing"),
            (
                Mode::Editing {
                    buffer: String::new(),
                },
                "editing",
            ),
            (Mode::Submitting { text: "x".into() }, "submitting"),
            (
                Mode::Result {
                    intent_id: Uuid::nil(),
                    status: "ok".into(),
                    text: "x".into(),
                },
                "result",
            ),
            (
                Mode::Error {
                    message: "x".into(),
                },
                "error",
            ),
            (
                Mode::MemoryTail {
                    loading: false,
                    records: Vec::new(),
                    error: None,
                },
                "memory-tail",
            ),
            (
                Mode::AuditTail {
                    loading: false,
                    events: Vec::new(),
                    error: None,
                },
                "audit-tail",
            ),
            (
                Mode::CapabilitiesTail {
                    loading: false,
                    capabilities: Vec::new(),
                    error: None,
                },
                "capabilities-tail",
            ),
            (
                Mode::A2aTail {
                    loading: false,
                    tasks: Vec::new(),
                    error: None,
                },
                "a2a-tail",
            ),
            (
                Mode::ReceiptsTail {
                    loading: false,
                    receipts: Vec::new(),
                    error: None,
                },
                "receipts-tail",
            ),
            (
                Mode::PeersTail {
                    loading: false,
                    peers: Vec::new(),
                    truncated: false,
                    error: None,
                },
                "peers-tail",
            ),
            (
                Mode::GrantEditor {
                    buffer: String::new(),
                },
                "grant-editor",
            ),
            (
                Mode::GrantSubmitting { action: "x".into() },
                "grant-submitting",
            ),
            (
                Mode::GrantResult {
                    action: "x".into(),
                    subject_display: AgentId::new("user@local", [0u8; 32]).display,
                    signature_b58: "x".into(),
                },
                "grant-result",
            ),
            (
                Mode::GrantError {
                    message: "x".into(),
                },
                "grant-error",
            ),
            (Mode::Help, "help"),
        ];

        assert_eq!(
            cases.len(),
            16,
            "Mode has 16 declared variants — a new variant added without \
             a corresponding (variant, expected_name) pair in this test \
             would silently let its Mode::name arm drift without an \
             exact-string anchor. The exhaustive match inside Mode::name \
             produces a compile error first; this count arm produces a \
             test failure second so an author who hand-wrote the new arm \
             without updating this pin gets a clear signal here",
        );

        for (variant, expected) in &cases {
            assert_eq!(
                variant.name(),
                *expected,
                "Mode::{variant:?} must surface name() == {expected:?} — \
                 the kebab-case shape gate above would still accept a \
                 rename like \"audit-tail\" -> \"audittail\" (still \
                 kebab-allowed charset) or a \"consolidate Result and \
                 Error into outcome\" collapse (until the uniqueness \
                 check fires). The exact-string contract is the \
                 operator-facing status-bar identifier; downstream \
                 dashboards, screenshots, and operator scripts anchor \
                 on these literals",
            );
        }

        let expected_unique: std::collections::BTreeSet<&str> =
            cases.iter().map(|(_, n)| *n).collect();
        assert_eq!(
            expected_unique.len(),
            cases.len(),
            "the (variant, expected_name) cases above must list 16 \
             distinct expected strings — a duplicate would indicate a \
             test-side typo that masks a real Mode::name collision \
             (e.g., two arms expected to map to \"result\")",
        );
    }

    #[test]
    fn drafts_are_capped_at_max_drafts_oldest_drops_first() {
        let mut app = App::new();
        for n in 0..MAX_DRAFTS + 5 {
            app.on_key(press(KeyCode::Char('i')));
            type_chars(&mut app, &format!("draft-{n}"));
            app.on_key(press(KeyCode::Enter));
        }
        assert_eq!(app.drafts().len(), MAX_DRAFTS);
        assert_eq!(app.drafts().first().unwrap(), &format!("draft-{}", 5));
        assert_eq!(
            app.drafts().last().unwrap(),
            &format!("draft-{}", MAX_DRAFTS + 4)
        );
    }
}
