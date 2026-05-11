//! Covenant TUI core. Holds the testable state and key-handler surface
//! that the binary's event loop drives. The terminal I/O lives in
//! `main.rs`; everything in this module is `#[cfg(test)]`-friendly and
//! does not touch stdout, stdin, raw mode, or the alternate screen.
//!
//! Future slices extend [`App`] with screens (intent submission, memory
//! tail, audit tail, capabilities); each screen adds state here and a
//! match arm in [`App::on_key`]. The I/O layer in `main.rs` does not
//! change shape.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Reasons the event loop may exit. `None` while the app keeps running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// The user pressed `q` or `Esc`.
    UserQuit,
    /// The user pressed `Ctrl-C`.
    Interrupt,
}

/// Top-level TUI state. Screens are added by future slices.
#[derive(Debug, Default)]
pub struct App {
    exit: Option<ExitReason>,
}

impl App {
    pub fn new() -> Self {
        Self { exit: None }
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
        if event.modifiers.contains(KeyModifiers::CONTROL) && event.code == KeyCode::Char('c') {
            self.exit = Some(ExitReason::Interrupt);
            return;
        }
        match event.code {
            KeyCode::Char('q') | KeyCode::Esc => self.exit = Some(ExitReason::UserQuit),
            _ => {}
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

    #[test]
    fn new_app_is_running() {
        let app = App::new();
        assert!(!app.should_quit());
        assert_eq!(app.exit_reason(), None);
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
}
