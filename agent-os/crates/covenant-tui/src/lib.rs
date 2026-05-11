//! Covenant TUI core. Holds the testable state and key-handler surface
//! that the binary's event loop drives. The terminal I/O lives in
//! `main.rs`; everything in this module is `#[cfg(test)]`-friendly and
//! does not touch stdout, stdin, raw mode, or the alternate screen.
//!
//! Future slices extend [`App`] with screens (memory tail, audit tail,
//! capabilities, etc.); each screen adds state here and a match arm
//! in [`App::on_key`]. The I/O layer in `main.rs` does not change shape.

use covenant_audit::AuditEvent;
use covenant_permissions::SignedCapability;
use covenant_types::MemoryRecord;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use uuid::Uuid;

pub mod ipc;

pub use ipc::{AuditFetchOutcome, CapabilitiesFetchOutcome, MemoryFetchOutcome};

/// Reasons the event loop may exit. `None` while the app keeps running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// The user pressed `q` or `Esc` from the base view.
    UserQuit,
    /// The user pressed `Ctrl-C` from any view.
    Interrupt,
}

/// Active screen / interaction mode.
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    /// Base view. `i` enters the intent editor; `s` submits the most
    /// recent draft to the daemon; `q` / `Esc` quit.
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
}

impl Default for Mode {
    fn default() -> Self {
        Mode::Browsing
    }
}

/// Drafted intents are capped so a stress-test (or an unattended
/// keyboard) cannot grow the buffer without bound. The oldest entry
/// drops when the cap is exceeded.
const MAX_DRAFTS: usize = 64;

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
            _ => Mode::Browsing,
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
        assert_eq!(app.mode(), &Mode::Editing { buffer: String::new() });
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
        assert_eq!(app.mode(), &Mode::Editing { buffer: "ab".into() });
        app.on_key(press(KeyCode::Backspace));
        app.on_key(press(KeyCode::Backspace));
        app.on_key(press(KeyCode::Backspace));
        assert_eq!(app.mode(), &Mode::Editing { buffer: String::new() });
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
            &Mode::Editing { buffer: String::new() },
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
