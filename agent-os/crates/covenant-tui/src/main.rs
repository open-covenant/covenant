//! Covenant TUI binary. Owns terminal raw mode + alternate-screen
//! lifecycle, the async event loop, and the IPC client that connects
//! the editor's draft queue to a running covenantd.
//!
//! Restoration is panic-safe: a Drop guard puts the terminal back in
//! its original mode even if rendering panics. Without it a crash
//! leaves the user's shell in raw mode and no echo, which is a
//! miserable recovery.

use std::io::{self, Stdout};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_tui::{App, ExitReason, Mode, SubmissionOutcome};
use crossterm::event::{Event, EventStream};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Terminal;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

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

fn covenant_home() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("COVENANT_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".covenant"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).context("init terminal")?;

    let mut app = App::new();
    let exit = run(&mut terminal, &mut app).await?;
    drop(_guard);

    if matches!(exit, ExitReason::Interrupt) {
        std::process::exit(130);
    }
    Ok(())
}

async fn run(terminal: &mut Tui, app: &mut App) -> Result<ExitReason> {
    let mut events = EventStream::new();
    let (tx, mut rx) = mpsc::unbounded_channel::<SubmissionOutcome>();
    let home = covenant_home()?;

    loop {
        terminal
            .draw(|frame| render(frame, app))
            .context("draw frame")?;

        // Kick off an IPC worker exactly once per Submitting
        // transition. `take_pending_submission` consumes the
        // pending-flag on first call and returns `None` afterwards
        // for the same RPC, so a slow daemon does not cause the loop
        // to re-spawn the same task each frame.
        if let Some(text) = app.take_pending_submission() {
            let tx = tx.clone();
            let home = home.clone();
            tokio::spawn(async move {
                let outcome = submit_intent(&home, &text).await.unwrap_or_else(|e| {
                    SubmissionOutcome::Failed {
                        message: format!("{e:#}"),
                    }
                });
                let _ = tx.send(outcome);
            });
        }

        tokio::select! {
            Some(Ok(event)) = events.next() => {
                if let Event::Key(key) = event {
                    app.on_key(key);
                }
            }
            Some(outcome) = rx.recv() => {
                app.apply_submission_outcome(outcome);
            }
        }

        if let Some(reason) = app.exit_reason() {
            return Ok(reason);
        }
    }
}

async fn read_operator_token(home: &Path) -> Result<String> {
    let path = home.join("peers").join("operator.token");
    let raw = tokio::fs::read_to_string(&path).await.with_context(|| {
        format!(
            "read operator token at {} (is covenantd running?)",
            path.display()
        )
    })?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!(
            "operator token at {} is empty",
            path.display()
        ));
    }
    Ok(trimmed.to_string())
}

async fn submit_intent(home: &Path, text: &str) -> Result<SubmissionOutcome> {
    let sock = home.join("sock");
    let mut stream = UnixStream::connect(&sock).await.with_context(|| {
        format!(
            "connect to daemon at {} (is covenantd running?)",
            sock.display()
        )
    })?;
    let token_b58 = read_operator_token(home).await?;
    write_frame(&mut stream, &Request::Authenticate { token_b58 }).await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::Authenticated { .. } => {}
        Response::AuthenticationFailed { reason } => {
            return Ok(SubmissionOutcome::Failed {
                message: format!("authentication failed: {reason}"),
            });
        }
        other => {
            return Ok(SubmissionOutcome::Failed {
                message: format!("unexpected response to authenticate: {other:?}"),
            });
        }
    }
    write_frame(
        &mut stream,
        &Request::SubmitIntent {
            text: text.to_string(),
        },
    )
    .await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::IntentResult {
            intent_id,
            status,
            text,
            ..
        } => Ok(SubmissionOutcome::Accepted {
            intent_id,
            status,
            text,
        }),
        Response::Error { message } => Ok(SubmissionOutcome::Failed { message }),
        other => Ok(SubmissionOutcome::Failed {
            message: format!("unexpected response: {other:?}"),
        }),
    }
}

fn render(frame: &mut ratatui::Frame<'_>, app: &App) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(frame.area());

    match app.mode() {
        Mode::Browsing => {
            let header =
                Paragraph::new("covenant tui — i: draft · s: submit most-recent · q / Esc: quit")
                    .block(Block::default().borders(Borders::ALL).title("covenant"));
            frame.render_widget(header, layout[0]);

            let drafts = app.drafts();
            let body = if drafts.is_empty() {
                Paragraph::new("no drafted intents yet")
                    .style(Style::default().add_modifier(Modifier::DIM))
                    .alignment(Alignment::Center)
                    .block(Block::default().borders(Borders::ALL).title("drafts"))
            } else {
                let lines: Vec<Line<'_>> = drafts
                    .iter()
                    .enumerate()
                    .map(|(i, d)| Line::from(format!("{:>3}. {d}", i + 1)))
                    .collect();
                Paragraph::new(lines)
                    .block(Block::default().borders(Borders::ALL).title("drafts"))
            };
            frame.render_widget(body, layout[1]);
        }
        Mode::Editing { buffer } => {
            let header =
                Paragraph::new("editing intent — Enter to draft · Esc to cancel · Ctrl-C to quit")
                    .block(Block::default().borders(Borders::ALL).title("covenant"));
            frame.render_widget(header, layout[0]);

            let line = Line::from(vec![
                Span::raw(buffer.as_str()),
                Span::styled("|", Style::default().add_modifier(Modifier::SLOW_BLINK)),
            ]);
            let body = Paragraph::new(line)
                .block(Block::default().borders(Borders::ALL).title("intent"));
            frame.render_widget(body, layout[1]);
        }
        Mode::Submitting { text } => {
            let header = Paragraph::new("submitting — Esc to dismiss view (in-flight RPC continues)")
                .block(Block::default().borders(Borders::ALL).title("covenant"));
            frame.render_widget(header, layout[0]);

            let body = Paragraph::new(text.as_str())
                .style(Style::default().add_modifier(Modifier::DIM))
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL).title("submitting"));
            frame.render_widget(body, layout[1]);
        }
        Mode::Result {
            intent_id,
            status,
            text,
        } => {
            let header = Paragraph::new(format!("intent {intent_id} — status: {status}"))
                .block(Block::default().borders(Borders::ALL).title("covenant"));
            frame.render_widget(header, layout[0]);

            let body = Paragraph::new(text.as_str())
                .block(Block::default().borders(Borders::ALL).title("result"));
            frame.render_widget(body, layout[1]);
        }
        Mode::Error { message } => {
            let header = Paragraph::new("submission failed — any key returns to drafts")
                .block(Block::default().borders(Borders::ALL).title("covenant"));
            frame.render_widget(header, layout[0]);

            let body = Paragraph::new(message.as_str())
                .style(Style::default().add_modifier(Modifier::REVERSED))
                .block(Block::default().borders(Borders::ALL).title("error"));
            frame.render_widget(body, layout[1]);
        }
    }
}
