//! Covenant TUI binary. Owns terminal raw mode + alternate-screen
//! lifecycle and the event loop; delegates state to [`covenant_tui::App`].
//!
//! Restoration is panic-safe: a Drop guard puts the terminal back in
//! its original mode even if rendering panics. Without it a crash
//! leaves the user's shell in raw mode and no echo, which is a
//! miserable recovery.

use std::io::{self, Stdout};
use std::time::Duration;

use anyhow::{Context, Result};
use covenant_tui::{App, ExitReason};
use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Alignment;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Terminal;

type Tui = Terminal<CrosstermBackend<Stdout>>;

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("enable raw mode")?;
        execute!(io::stdout(), EnterAlternateScreen).context("enter alternate screen")?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

fn main() -> Result<()> {
    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).context("init terminal")?;

    let mut app = App::new();
    let exit = run(&mut terminal, &mut app)?;
    drop(_guard);

    if matches!(exit, ExitReason::Interrupt) {
        std::process::exit(130);
    }
    Ok(())
}

fn run(terminal: &mut Tui, app: &mut App) -> Result<ExitReason> {
    loop {
        terminal
            .draw(|frame| render(frame, app))
            .context("draw frame")?;
        if event::poll(Duration::from_millis(250)).context("poll events")? {
            if let Event::Key(key) = event::read().context("read event")? {
                app.on_key(key);
            }
        }
        if let Some(reason) = app.exit_reason() {
            return Ok(reason);
        }
    }
}

fn render(frame: &mut ratatui::Frame<'_>, _app: &App) {
    let block = Block::default()
        .title("covenant — press q or Esc to quit")
        .borders(Borders::ALL);
    let body = Paragraph::new("connect to daemon: not yet wired in this slice")
        .style(Style::default().add_modifier(Modifier::DIM))
        .alignment(Alignment::Center)
        .block(block);
    frame.render_widget(body, frame.area());
}
