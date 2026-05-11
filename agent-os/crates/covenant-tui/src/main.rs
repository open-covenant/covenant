//! Covenant TUI binary. Owns terminal raw mode + alternate-screen
//! lifecycle, the async event loop, and the IPC client that connects
//! the editor's draft queue to a running covenantd.
//!
//! Restoration is panic-safe: a Drop guard puts the terminal back in
//! its original mode even if rendering panics. Without it a crash
//! leaves the user's shell in raw mode and no echo, which is a
//! miserable recovery.

use std::io::{self, Stdout};

use anyhow::{Context, Result};
use covenant_tui::ipc::{
    covenant_home, grant_capability, recent_a2a_tasks, recent_audit, recent_capabilities,
    recent_memory, recent_receipts, submit_intent, A2aFetchOutcome, AuditFetchOutcome,
    CapabilitiesFetchOutcome, GrantOutcome, MemoryFetchOutcome, ReceiptsFetchOutcome,
};
use covenant_tui::{App, ExitReason, Mode, SubmissionOutcome, HELP_BINDINGS};
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
use tokio::sync::mpsc;

type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Renders the bottom status bar. Shows the current mode discriminant
/// (via `Mode::name()`, never `{:?}`) and the resolved COVENANT_HOME
/// truncated from the left with a `…` prefix so the trailing path
/// segment stays visible on narrow terminals.
fn render_status_bar(
    frame: &mut ratatui::Frame<'_>,
    app: &App,
    home: &std::path::Path,
    rect: ratatui::layout::Rect,
) {
    let mode = app.mode().name();
    let home_str = home.display().to_string();
    let suffix = "  ? for help";
    let width = rect.width as usize;
    let fixed_overhead = mode.len() + 2 + suffix.len() + 2; // "<mode>  <home><suffix>"
    let home_display = if width > fixed_overhead && home_str.len() > width - fixed_overhead {
        let keep = width.saturating_sub(fixed_overhead + 1);
        let start = home_str.len().saturating_sub(keep);
        format!("…{}", &home_str[start..])
    } else {
        home_str
    };
    let text = format!("{mode}  {home_display}{suffix}");
    let bar = Paragraph::new(text).style(Style::default().add_modifier(Modifier::DIM));
    frame.render_widget(bar, rect);
}

/// Stable, render-safe label for a `ResourceKind`. Same reason as
/// `audit_kind_label`: avoid `{:?}` so a future variant reorder does
/// not silently change column widths in the rendered table.
fn receipt_resource_label(kind: &covenant_types::ResourceKind) -> &'static str {
    use covenant_types::ResourceKind::*;
    match kind {
        Compute => "compute",
        Memory => "memory",
        Tool => "tool",
        Message => "message",
        Registration => "register",
    }
}

/// Stable, render-safe discriminant label for an `AuditKind`. The
/// renderer must NOT use `{:?}` because that would leak internal
/// variant ordering and break on a refactor; the explicit match
/// pins the names that show up in the TUI.
fn audit_kind_label(kind: &covenant_audit::AuditKind) -> &'static str {
    use covenant_audit::AuditKind::*;
    match kind {
        IntentDispatched { .. } => "intent.dispatched",
        IntentIgnored { .. } => "intent.ignored",
        AuthenticationFailed { .. } => "authentication.failed",
        CapabilityCheck { .. } => "capability.check",
        CapabilityGranted { .. } => "capability.granted",
        CapabilityGrantRejected { .. } => "capability.grant_rejected",
        CapabilityScopeRejected { .. } => "capability.scope_rejected",
        CapabilityRevokeRejected { .. } => "capability.revoke_rejected",
        MemoryRepairApplied { .. } => "memory.repair_applied",
        MemoryCompactionApplied { .. } => "memory.compaction_applied",
        BudgetExhausted { .. } => "budget.exhausted",
        BudgetUnseeded { .. } => "budget.unseeded",
        OperatorTokenRotated { .. } => "operator.token_rotated",
        OperatorTokenRotationRejected { .. } => "operator.token_rotation_rejected",
        OperatorPeersListRejected { .. } => "operator.peers_list_rejected",
        PeerRevoked { .. } => "peer.revoked",
        OperatorPeerRevokeRejected { .. } => "operator.peer_revoke_rejected",
        PeerSelfRevokeBlocked { .. } => "peer.self_revoke_blocked",
        _ => "(other)",
    }
}

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
    let (submit_tx, mut submit_rx) = mpsc::unbounded_channel::<SubmissionOutcome>();
    let (memory_tx, mut memory_rx) = mpsc::unbounded_channel::<MemoryFetchOutcome>();
    let (audit_tx, mut audit_rx) = mpsc::unbounded_channel::<AuditFetchOutcome>();
    let (caps_tx, mut caps_rx) = mpsc::unbounded_channel::<CapabilitiesFetchOutcome>();
    let (a2a_tx, mut a2a_rx) = mpsc::unbounded_channel::<A2aFetchOutcome>();
    let (receipts_tx, mut receipts_rx) = mpsc::unbounded_channel::<ReceiptsFetchOutcome>();
    let (grant_tx, mut grant_rx) = mpsc::unbounded_channel::<GrantOutcome>();
    let home = covenant_home()?;

    loop {
        terminal
            .draw(|frame| render(frame, app, &home))
            .context("draw frame")?;

        // Kick off an IPC worker exactly once per Submitting
        // transition. `take_pending_submission` consumes the
        // pending-flag on first call and returns `None` afterwards
        // for the same RPC, so a slow daemon does not cause the loop
        // to re-spawn the same task each frame.
        if let Some(text) = app.take_pending_submission() {
            let tx = submit_tx.clone();
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

        if app.take_pending_memory_fetch() {
            let tx = memory_tx.clone();
            let home = home.clone();
            tokio::spawn(async move {
                let outcome = recent_memory(&home, None, 20).await.unwrap_or_else(|e| {
                    MemoryFetchOutcome::Failed {
                        message: format!("{e:#}"),
                    }
                });
                let _ = tx.send(outcome);
            });
        }

        if app.take_pending_audit_fetch() {
            let tx = audit_tx.clone();
            let home = home.clone();
            tokio::spawn(async move {
                let outcome =
                    recent_audit(&home, 30)
                        .await
                        .unwrap_or_else(|e| AuditFetchOutcome::Failed {
                            message: format!("{e:#}"),
                        });
                let _ = tx.send(outcome);
            });
        }

        if app.take_pending_capabilities_fetch() {
            let tx = caps_tx.clone();
            let home = home.clone();
            tokio::spawn(async move {
                let outcome = recent_capabilities(&home, 20).await.unwrap_or_else(|e| {
                    CapabilitiesFetchOutcome::Failed {
                        message: format!("{e:#}"),
                    }
                });
                let _ = tx.send(outcome);
            });
        }

        if app.take_pending_a2a_fetch() {
            let tx = a2a_tx.clone();
            let home = home.clone();
            tokio::spawn(async move {
                let outcome =
                    recent_a2a_tasks(&home, 20)
                        .await
                        .unwrap_or_else(|e| A2aFetchOutcome::Failed {
                            message: format!("{e:#}"),
                        });
                let _ = tx.send(outcome);
            });
        }

        if app.take_pending_receipts_fetch() {
            let tx = receipts_tx.clone();
            let home = home.clone();
            tokio::spawn(async move {
                let outcome = recent_receipts(&home, 20).await.unwrap_or_else(|e| {
                    ReceiptsFetchOutcome::Failed {
                        message: format!("{e:#}"),
                    }
                });
                let _ = tx.send(outcome);
            });
        }

        if let Some(action) = app.take_pending_grant_submission() {
            let tx = grant_tx.clone();
            let home = home.clone();
            tokio::spawn(async move {
                let outcome = grant_capability(&home, &action, None, None)
                    .await
                    .unwrap_or_else(|e| GrantOutcome::Failed {
                        message: format!("{e:#}"),
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
            Some(outcome) = submit_rx.recv() => {
                app.apply_submission_outcome(outcome);
            }
            Some(outcome) = memory_rx.recv() => {
                app.apply_memory_fetch_outcome(outcome);
            }
            Some(outcome) = audit_rx.recv() => {
                app.apply_audit_fetch_outcome(outcome);
            }
            Some(outcome) = caps_rx.recv() => {
                app.apply_capabilities_fetch_outcome(outcome);
            }
            Some(outcome) = a2a_rx.recv() => {
                app.apply_a2a_fetch_outcome(outcome);
            }
            Some(outcome) = receipts_rx.recv() => {
                app.apply_receipts_fetch_outcome(outcome);
            }
            Some(outcome) = grant_rx.recv() => {
                app.apply_grant_outcome(outcome);
            }
        }

        if let Some(reason) = app.exit_reason() {
            return Ok(reason);
        }
    }
}

fn render(frame: &mut ratatui::Frame<'_>, app: &App, home: &std::path::Path) {
    // Three-row layout: bordered header (3), content (min 0), status
    // bar (1). The bottom row is reserved so each mode arm only needs
    // to draw into layout[0]/layout[1] like before.
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(frame.area());

    render_status_bar(frame, app, home, layout[2]);

    match app.mode() {
        Mode::Browsing => {
            let header = Paragraph::new("covenant tui — ? for help · q: quit")
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
                Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("drafts"))
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
            let body =
                Paragraph::new(line).block(Block::default().borders(Borders::ALL).title("intent"));
            frame.render_widget(body, layout[1]);
        }
        Mode::Submitting { text } => {
            let header =
                Paragraph::new("submitting — Esc to dismiss view (in-flight RPC continues)")
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
        Mode::AuditTail {
            loading,
            events,
            error,
        } => {
            let header = Paragraph::new("audit tail — q / Esc to dismiss")
                .block(Block::default().borders(Borders::ALL).title("covenant"));
            frame.render_widget(header, layout[0]);

            let body = if let Some(message) = error {
                Paragraph::new(message.as_str())
                    .style(Style::default().add_modifier(Modifier::REVERSED))
                    .block(Block::default().borders(Borders::ALL).title("audit error"))
            } else if *loading {
                Paragraph::new("fetching…")
                    .style(Style::default().add_modifier(Modifier::DIM))
                    .alignment(Alignment::Center)
                    .block(Block::default().borders(Borders::ALL).title("audit"))
            } else if events.is_empty() {
                Paragraph::new("no audit events")
                    .style(Style::default().add_modifier(Modifier::DIM))
                    .alignment(Alignment::Center)
                    .block(Block::default().borders(Borders::ALL).title("audit"))
            } else {
                let lines: Vec<Line<'_>> = events
                    .iter()
                    .map(|e| {
                        let kind = audit_kind_label(&e.kind);
                        Line::from(format!(
                            "{ts:>14}  {kind:<28}  {issuer}",
                            ts = e.timestamp_ms,
                            kind = kind,
                            issuer = e.issuer.display,
                        ))
                    })
                    .collect();
                Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("audit"))
            };
            frame.render_widget(body, layout[1]);
        }
        Mode::GrantEditor { buffer } => {
            let header = Paragraph::new(
                "granting capability — Enter to submit · Esc to cancel · Ctrl-C to quit",
            )
            .block(Block::default().borders(Borders::ALL).title("covenant"));
            frame.render_widget(header, layout[0]);

            let line = Line::from(vec![
                Span::raw(buffer.as_str()),
                Span::styled("|", Style::default().add_modifier(Modifier::SLOW_BLINK)),
            ]);
            let body =
                Paragraph::new(line).block(Block::default().borders(Borders::ALL).title("action"));
            frame.render_widget(body, layout[1]);
        }
        Mode::GrantSubmitting { action } => {
            let header = Paragraph::new("granting — Esc to dismiss (RPC continues)")
                .block(Block::default().borders(Borders::ALL).title("covenant"));
            frame.render_widget(header, layout[0]);

            let body = Paragraph::new(action.as_str())
                .style(Style::default().add_modifier(Modifier::DIM))
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("submitting grant"),
                );
            frame.render_widget(body, layout[1]);
        }
        Mode::GrantResult {
            action,
            subject_display,
            signature_b58,
        } => {
            let header = Paragraph::new(format!(
                "granted {action} to {subject_display} — any key returns"
            ))
            .block(Block::default().borders(Borders::ALL).title("covenant"));
            frame.render_widget(header, layout[0]);

            let sig_prefix: String = signature_b58.chars().take(16).collect();
            let body = Paragraph::new(format!("signature: {sig_prefix}…"))
                .block(Block::default().borders(Borders::ALL).title("grant"));
            frame.render_widget(body, layout[1]);
        }
        Mode::GrantError { message } => {
            let header = Paragraph::new("grant failed — any key returns")
                .block(Block::default().borders(Borders::ALL).title("covenant"));
            frame.render_widget(header, layout[0]);

            let body = Paragraph::new(message.as_str())
                .style(Style::default().add_modifier(Modifier::REVERSED))
                .block(Block::default().borders(Borders::ALL).title("grant error"));
            frame.render_widget(body, layout[1]);
        }
        Mode::Help => {
            let header = Paragraph::new("help — any key returns to drafts")
                .block(Block::default().borders(Borders::ALL).title("covenant"));
            frame.render_widget(header, layout[0]);

            let lines: Vec<Line<'_>> = HELP_BINDINGS
                .iter()
                .map(|(key, desc)| Line::from(format!("  {key:<4}  {desc}")))
                .collect();
            let body = Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL).title("keybindings"));
            frame.render_widget(body, layout[1]);
        }
        Mode::CapabilitiesTail {
            loading,
            capabilities,
            error,
        } => {
            let header = Paragraph::new("capabilities — q / Esc to dismiss")
                .block(Block::default().borders(Borders::ALL).title("covenant"));
            frame.render_widget(header, layout[0]);

            let body = if let Some(message) = error {
                Paragraph::new(message.as_str())
                    .style(Style::default().add_modifier(Modifier::REVERSED))
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("capabilities error"),
                    )
            } else if *loading {
                Paragraph::new("fetching…")
                    .style(Style::default().add_modifier(Modifier::DIM))
                    .alignment(Alignment::Center)
                    .block(Block::default().borders(Borders::ALL).title("capabilities"))
            } else if capabilities.is_empty() {
                Paragraph::new("no active capabilities")
                    .style(Style::default().add_modifier(Modifier::DIM))
                    .alignment(Alignment::Center)
                    .block(Block::default().borders(Borders::ALL).title("capabilities"))
            } else {
                let lines: Vec<Line<'_>> = capabilities
                    .iter()
                    .map(|c| {
                        let cap = &c.capability;
                        let expires = cap
                            .expires_at
                            .map(|m| format!("{m}"))
                            .unwrap_or_else(|| "never".to_string());
                        let sig_prefix: String = bs58::encode(&c.signature[..])
                            .into_string()
                            .chars()
                            .take(8)
                            .collect();
                        Line::from(format!(
                            "{action:<32}  {subject:<24}  expires: {expires:<14}  sig: {sig_prefix}",
                            action = cap.action,
                            subject = cap.subject.display,
                            expires = expires,
                            sig_prefix = sig_prefix,
                        ))
                    })
                    .collect();
                Paragraph::new(lines)
                    .block(Block::default().borders(Borders::ALL).title("capabilities"))
            };
            frame.render_widget(body, layout[1]);
        }
        Mode::A2aTail {
            loading,
            tasks,
            error,
        } => {
            let header = Paragraph::new("a2a inbox — q / Esc to dismiss")
                .block(Block::default().borders(Borders::ALL).title("covenant"));
            frame.render_widget(header, layout[0]);

            let body = if let Some(message) = error {
                Paragraph::new(message.as_str())
                    .style(Style::default().add_modifier(Modifier::REVERSED))
                    .block(Block::default().borders(Borders::ALL).title("a2a error"))
            } else if *loading {
                Paragraph::new("fetching…")
                    .style(Style::default().add_modifier(Modifier::DIM))
                    .alignment(Alignment::Center)
                    .block(Block::default().borders(Borders::ALL).title("a2a"))
            } else if tasks.is_empty() {
                Paragraph::new("no a2a tasks")
                    .style(Style::default().add_modifier(Modifier::DIM))
                    .alignment(Alignment::Center)
                    .block(Block::default().borders(Borders::ALL).title("a2a"))
            } else {
                let lines: Vec<Line<'_>> = tasks
                    .iter()
                    .map(|t| {
                        let id_prefix: String = t.id.to_string().chars().take(8).collect();
                        let kind = t.task_kind.as_deref().unwrap_or("(none)");
                        Line::from(format!(
                            "{id_prefix}  {kind:<20}  {sender} -> {recipient}",
                            kind = kind,
                            sender = t.sender.display,
                            recipient = t.recipient.display,
                        ))
                    })
                    .collect();
                Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("a2a"))
            };
            frame.render_widget(body, layout[1]);
        }
        Mode::ReceiptsTail {
            loading,
            receipts,
            error,
        } => {
            let header = Paragraph::new("chain receipts — q / Esc to dismiss")
                .block(Block::default().borders(Borders::ALL).title("covenant"));
            frame.render_widget(header, layout[0]);

            let body = if let Some(message) = error {
                Paragraph::new(message.as_str())
                    .style(Style::default().add_modifier(Modifier::REVERSED))
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("receipts error"),
                    )
            } else if *loading {
                Paragraph::new("fetching…")
                    .style(Style::default().add_modifier(Modifier::DIM))
                    .alignment(Alignment::Center)
                    .block(Block::default().borders(Borders::ALL).title("receipts"))
            } else if receipts.is_empty() {
                Paragraph::new("no chain receipts")
                    .style(Style::default().add_modifier(Modifier::DIM))
                    .alignment(Alignment::Center)
                    .block(Block::default().borders(Borders::ALL).title("receipts"))
            } else {
                let lines: Vec<Line<'_>> = receipts
                    .iter()
                    .map(|r| {
                        let id_prefix: String = r.id.to_string().chars().take(8).collect();
                        let payer_prefix: String =
                            r.payer.pubkey_base58().chars().take(8).collect();
                        let resource = receipt_resource_label(&r.resource);
                        Line::from(format!(
                            "{id_prefix}  {resource:<10}  {credits:>6} credits  payer: {payer_prefix}",
                            credits = r.credits_consumed,
                        ))
                    })
                    .collect();
                Paragraph::new(lines)
                    .block(Block::default().borders(Borders::ALL).title("receipts"))
            };
            frame.render_widget(body, layout[1]);
        }
        Mode::MemoryTail {
            loading,
            records,
            error,
        } => {
            let header = Paragraph::new("memory tail — q / Esc to dismiss")
                .block(Block::default().borders(Borders::ALL).title("covenant"));
            frame.render_widget(header, layout[0]);

            let body = if let Some(message) = error {
                Paragraph::new(message.as_str())
                    .style(Style::default().add_modifier(Modifier::REVERSED))
                    .block(Block::default().borders(Borders::ALL).title("memory error"))
            } else if *loading {
                Paragraph::new("fetching…")
                    .style(Style::default().add_modifier(Modifier::DIM))
                    .alignment(Alignment::Center)
                    .block(Block::default().borders(Borders::ALL).title("memory"))
            } else if records.is_empty() {
                Paragraph::new("no records in memory")
                    .style(Style::default().add_modifier(Modifier::DIM))
                    .alignment(Alignment::Center)
                    .block(Block::default().borders(Borders::ALL).title("memory"))
            } else {
                let lines: Vec<Line<'_>> = records
                    .iter()
                    .map(|r| {
                        let id_prefix: String = r.id.to_string().chars().take(8).collect();
                        let text_preview: String = r.text.chars().take(80).collect();
                        Line::from(format!(
                            "{id_prefix}  {tier:?}  {owner}  {text}",
                            tier = r.tier,
                            owner = r.owner.display,
                            text = text_preview
                        ))
                    })
                    .collect();
                Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("memory"))
            };
            frame.render_widget(body, layout[1]);
        }
    }
}
