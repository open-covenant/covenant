//! Covenant TUI core. Holds the testable state and key-handler surface
//! that the binary's event loop drives. The terminal I/O lives in
//! `main.rs`; everything in this module is `#[cfg(test)]`-friendly and
//! does not touch stdout, stdin, raw mode, or the alternate screen.
//!
//! Future slices extend [`App`] with screens (memory tail, audit tail,
//! capabilities, etc.); each screen adds state here and a match arm
//! in [`App::on_key`]. The I/O layer in `main.rs` does not change shape.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Reasons the event loop may exit. `None` while the app keeps running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// The user pressed `q` or `Esc` from the base view.
    UserQuit,
    /// The user pressed `Ctrl-C` from any view.
    Interrupt,
}

/// Active screen / interaction mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Base view. `i` enters the intent editor; `q` / `Esc` quit.
    Browsing,
    /// Intent editor: typing fills the buffer; Backspace removes the
    /// last char; `Enter` accepts the buffer into [`App::drafts`] and
    /// returns to [`Mode::Browsing`]; `Esc` discards the buffer and
    /// returns to [`Mode::Browsing`]. `q` is a literal character here,
    /// not a quit.
    Editing { buffer: String },
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
        };
    }

    fn handle_browsing(&mut self, event: KeyEvent) -> Mode {
        match event.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.exit = Some(ExitReason::UserQuit);
                Mode::Browsing
            }
            KeyCode::Char('i') => Mode::Editing {
                buffer: String::new(),
            },
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
