//! Covenant daemon library — Phase 0/1/2 listener wired to router + runner +
//! memory + settlement + audit + capabilities. Per-dispatch we write a
//! working-tier memory record, a settlement receipt, an audit event, AND a
//! capability check (audit-only — Sprint 12 doesn't reject, Sprint 13 will).

#![deny(unsafe_code)]

pub mod http;

use anyhow::{Context, Result};
use covenant_a2a::Mailbox;
use covenant_audit::{hash_hex, AuditEvent, AuditKind, AuditLog};
use covenant_budget::{BudgetError, BudgetLedger};
use covenant_identity::LocalIdentity;
use covenant_ipc::{read_frame, write_frame, IpcError, Request, Response};
use covenant_llm::Embedder;
use covenant_mcp::ToolRegistry;
use covenant_memory::{IgnoreSet, MemoryStore};
use covenant_peer_auth::{PeerEntry, PeerRegistry, PeerToken};
use covenant_permissions::{sign as sign_capability, verify_with_clock, CapabilityStore};
use covenant_router::{AgentCard, Router};
use covenant_runtime::Runner;
use covenant_settlement::{intent_dispatch_credits, memory_write_credits, Settlement};
use covenant_types::{
    AgentId, Capability, Intent, MemoryRecord, MemoryTier, Priority, ResourceKind,
    SettlementReceipt,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::{UnixListener, UnixStream};
use tracing::{debug, warn};
use uuid::Uuid;

pub fn covenant_home() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("COVENANT_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".covenant"))
}

#[derive(Clone)]
pub struct Server {
    router: Arc<Router>,
    runner: Arc<dyn Runner>,
    memory: Arc<dyn MemoryStore>,
    settlement: Arc<dyn Settlement>,
    audit: Arc<dyn AuditLog>,
    capabilities: Arc<dyn CapabilityStore>,
    embedder: Arc<dyn Embedder>,
    identity: Arc<LocalIdentity>,
    ignore: Arc<IgnoreSet>,
    tools: Arc<ToolRegistry>,
    mailbox: Arc<dyn Mailbox>,
    pub peers: Arc<dyn PeerRegistry>,
    budget: Arc<dyn BudgetLedger>,
    /// `$COVENANT_HOME` for this daemon — set via [`Server::with_home`]
    /// in the binary's `main`. Required by [`Server::rotate_operator_token`]
    /// (which needs to read the current operator token from
    /// `<home>/peers/operator.token` and write the rotated one back to
    /// the same path with mode 0600). All other handlers are home-agnostic
    /// — they go through the storage traits — so unit tests that don't
    /// exercise rotation leave this `None`.
    home: Option<PathBuf>,
}

#[allow(clippy::too_many_arguments)]
impl Server {
    pub fn new(
        router: Arc<Router>,
        runner: Arc<dyn Runner>,
        memory: Arc<dyn MemoryStore>,
        settlement: Arc<dyn Settlement>,
        audit: Arc<dyn AuditLog>,
        capabilities: Arc<dyn CapabilityStore>,
        embedder: Arc<dyn Embedder>,
        identity: Arc<LocalIdentity>,
        ignore: Arc<IgnoreSet>,
        tools: Arc<ToolRegistry>,
        mailbox: Arc<dyn Mailbox>,
        peers: Arc<dyn PeerRegistry>,
        budget: Arc<dyn BudgetLedger>,
    ) -> Self {
        Self {
            router,
            runner,
            memory,
            settlement,
            audit,
            capabilities,
            embedder,
            identity,
            ignore,
            tools,
            mailbox,
            peers,
            budget,
            home: None,
        }
    }

    /// Bind a `$COVENANT_HOME` path so [`Server::rotate_operator_token`]
    /// knows where to read the current token and where to rewrite it.
    /// Daemon `main` calls this once after [`Server::new`]. Without it,
    /// `RotateOperatorToken` returns `Response::Error`.
    pub fn with_home(mut self, home: PathBuf) -> Self {
        self.home = Some(home);
        self
    }

    /// Walk the router's registered agents and seed each one's budget
    /// bucket from its manifest's `Settlement.budget_credits_per_hour`.
    /// Cards with `budget_credits_per_hour == 0` are skipped — the spec's
    /// `Settlement` rustdoc says "Phase 0 tolerates 0; enforced from
    /// Phase 1," and `dispatch_intent` mirrors the predicate. Idempotent:
    /// calling twice is fine because [`BudgetLedger::set_capacity`]
    /// re-stamps the bucket without resetting `tokens_remaining` further
    /// than the (possibly shrunk) capacity. Daemon main calls this once
    /// after `Server::new`; tests that need budget enforcement call it
    /// explicitly.
    pub async fn register_agent_budgets(&self) -> Result<(), BudgetSeedError> {
        for card in self.router.agents() {
            let cap = card.manifest.settlement.budget_credits_per_hour;
            if cap == 0 {
                continue;
            }
            let agent = agent_id_for_card(card);
            self.budget
                .set_capacity(&agent, cap)
                .await
                .map_err(|source| BudgetSeedError {
                    agent_id: card.id.clone(),
                    source,
                })?;
        }
        Ok(())
    }

    pub async fn serve(&self, listener: UnixListener) -> Result<()> {
        loop {
            let (stream, _peer) = listener.accept().await?;
            debug!("accepted connection");
            let me = self.clone();
            tokio::spawn(async move {
                if let Err(e) = me.handle(stream).await {
                    warn!(error = %e, "connection failed");
                }
            });
        }
    }

    async fn handle(&self, mut stream: UnixStream) -> Result<()> {
        // First frame must be `Authenticate`. Anything else terminates the
        // connection after a single `AuthenticationFailed` reply. The
        // authenticated peer is bound to the connection for its lifetime;
        // a new connection requires a new handshake.
        let first: Request = match read_frame(&mut stream).await {
            Ok(r) => r,
            Err(IpcError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };
        let peer = match first {
            Request::Authenticate { token_b58 } => match self.authenticate(&token_b58).await {
                Some(agent_id) => {
                    write_frame(
                        &mut stream,
                        &Response::Authenticated {
                            display: agent_id.display.clone(),
                        },
                    )
                    .await?;
                    agent_id
                }
                None => {
                    let reason = "unknown or revoked token";
                    self.record_auth_failure("ipc", reason).await;
                    write_frame(
                        &mut stream,
                        &Response::AuthenticationFailed {
                            reason: reason.into(),
                        },
                    )
                    .await?;
                    return Ok(());
                }
            },
            _ => {
                let reason = "first frame must be Authenticate";
                self.record_auth_failure("ipc", reason).await;
                write_frame(
                    &mut stream,
                    &Response::AuthenticationFailed {
                        reason: reason.into(),
                    },
                )
                .await?;
                return Ok(());
            }
        };

        loop {
            let req: Request = match read_frame(&mut stream).await {
                Ok(r) => r,
                Err(IpcError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
            };
            let resp = self.respond(req, &peer).await;
            write_frame(&mut stream, &resp).await?;
        }
    }

    async fn authenticate(&self, token_b58: &str) -> Option<AgentId> {
        let token = PeerToken::from_b58(token_b58).ok()?;
        self.peers.resolve(&token).await.ok().flatten()
    }

    pub async fn record_auth_failure(&self, transport: &str, reason: &str) {
        let event = AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: epoch_ms(),
            issuer: self.identity.agent_id(),
            kind: AuditKind::AuthenticationFailed {
                transport: transport.to_string(),
                reason: reason.to_string(),
            },
        };
        self.record_daemon_event(event).await;
    }

    /// Record an audit event that represents an action by the
    /// authenticated `peer`. Asserts `event.issuer.pubkey == peer.pubkey`
    /// in debug builds and warns in release builds; the row is recorded
    /// either way (dropping it would hide the very regression the
    /// invariant is here to surface). Compare on the 32-byte pubkey, not
    /// the wire-supplied `display`. Sprint 58f.
    async fn record_peer_event(&self, peer: &AgentId, event: AuditEvent) {
        debug_assert_eq!(
            event.issuer.pubkey, peer.pubkey,
            "audit invariant: peer-action event.issuer.pubkey must equal authenticated peer.pubkey"
        );
        if event.issuer.pubkey != peer.pubkey {
            warn!(
                expected = %peer.display,
                got = %event.issuer.display,
                "audit invariant violated: peer-action event.issuer != peer"
            );
        }
        if let Err(e) = self.audit.record(event).await {
            warn!(error = %e, "audit record failed");
        }
    }

    /// Record an audit event the daemon emits on its own behalf — i.e.
    /// when no peer is authenticated (currently only
    /// `AuthenticationFailed`). Asserts the issuer matches
    /// `self.identity` to catch a future regression that routes a
    /// peer-action through this path. Same release-mode posture as
    /// [`Self::record_peer_event`]. Sprint 58f.
    async fn record_daemon_event(&self, event: AuditEvent) {
        let identity_pubkey = self.identity.agent_id().pubkey;
        debug_assert_eq!(
            event.issuer.pubkey, identity_pubkey,
            "audit invariant: daemon-internal event.issuer.pubkey must equal self.identity.pubkey"
        );
        if event.issuer.pubkey != identity_pubkey {
            warn!(
                got = %event.issuer.display,
                "audit invariant violated: daemon event.issuer != daemon identity"
            );
        }
        if let Err(e) = self.audit.record(event).await {
            warn!(error = %e, "audit record failed");
        }
    }

    pub async fn respond(&self, req: Request, peer: &AgentId) -> Response {
        match req {
            Request::Ping => Response::Pong,
            Request::Authenticate { token_b58 } => match self.authenticate(&token_b58).await {
                Some(agent_id) => Response::Authenticated {
                    display: agent_id.display,
                },
                None => Response::AuthenticationFailed {
                    reason: "unknown or revoked token".into(),
                },
            },
            Request::SubmitIntent { text } => self.dispatch_intent(text, peer).await,
            Request::RecentMemory { tier, limit } => self.recent_memory(tier, limit, peer).await,
            Request::RecentReceipts { limit } => self.recent_receipts(limit, peer).await,
            Request::RecentCapabilities { limit } => self.recent_capabilities(limit, peer).await,
            Request::GrantCapability {
                action,
                scope,
                expires_at,
            } => self.grant_capability(action, scope, expires_at, peer).await,
            Request::RevokeCapability { signature_b58 } => {
                self.revoke_capability(signature_b58, peer).await
            }
            Request::SearchMemory { query, tier, limit } => {
                self.search_memory(query, tier, limit).await
            }
            Request::PurgeMemory { tier, before_ms } => self.purge_memory(tier, before_ms).await,
            Request::Verify { window } => self.verify_recent(window).await,
            Request::IgnoreCheck { text } => self.check_ignore(text),
            Request::ListTools => self.list_tools(),
            Request::CallTool { name, arguments } => self.call_tool(name, arguments, peer).await,
            Request::RecentAudit { limit } => self.recent_audit(limit, peer).await,
            Request::PurgeAudit { before_ms } => self.purge_audit(before_ms, peer).await,
            Request::PurgeCapabilities { before_ms } => {
                self.purge_capabilities(before_ms, peer).await
            }
            Request::SendA2ATask { task } => self.send_a2a_task(task, peer).await,
            Request::TryRecvA2ATask => self.try_recv_a2a_task(peer).await,
            Request::PostA2AResult { result } => self.post_a2a_result(result, peer).await,
            Request::TryRecvA2AResult => self.try_recv_a2a_result(peer).await,
            Request::RecentA2ATasks { limit } => self.recent_a2a_tasks(limit, peer).await,
            Request::RecentA2AResults { limit } => self.recent_a2a_results(limit, peer).await,
            Request::CompactA2A => self.compact_a2a(peer).await,
            Request::PurgePeers { before_ms } => self.purge_peers(before_ms, peer).await,
            Request::ResumeIntent { intent_id } => self.resume_intent(intent_id, peer).await,
            Request::RecentDebits { limit } => self.recent_debits(limit).await,
            Request::RotateOperatorToken => self.rotate_operator_token(peer).await,
        }
    }

    async fn send_a2a_task(&self, task: covenant_a2a::A2ATask, peer: &AgentId) -> Response {
        if task.sender != *peer {
            let event = AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: peer.clone(),
                kind: AuditKind::A2ASenderMismatch {
                    peer_display: peer.display.clone(),
                    claimed_sender_display: task.sender.display.clone(),
                },
            };
            self.record_peer_event(peer, event).await;
            return Response::Error {
                message: format!(
                    "a2a send rejected: task.sender {:?} does not match \
                     authenticated peer {:?}",
                    task.sender.display, peer.display
                ),
            };
        }
        let task_id = task.id;
        let recipient = task.recipient.display.clone();
        let action = format!("a2a.send.{recipient}");
        let check = self
            .check_capabilities(format!("a2a-send:{recipient}"), vec![action.clone()], peer)
            .await;
        if !check.passed {
            return Response::Error {
                message: format!(
                    "a2a send to {recipient} requires capability {action:?}. \
                     Grant it with `covenant capabilities grant {action}`."
                ),
            };
        }
        // Sprint 59 recipient admission gate: when sender ≠ recipient
        // (cross-peer send), the recipient peer must have granted
        // `a2a.recv.<sender>` to themselves. v0 single-peer is loopback
        // (peer == recipient), so the gate is a no-op there. The
        // pubkey-byte compare defeats display spoofing.
        if peer.pubkey != task.recipient.pubkey {
            let recv_action = format!("a2a.recv.{}", peer.display);
            if !self
                .recipient_has_recv_action(&task.recipient, &recv_action)
                .await
            {
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: peer.clone(),
                    kind: AuditKind::A2ARecipientRejected {
                        sender_display: peer.display.clone(),
                        recipient_display: task.recipient.display.clone(),
                        action: recv_action.clone(),
                    },
                };
                self.record_peer_event(peer, event).await;
                return Response::Error {
                    message: format!(
                        "a2a send to {} rejected: recipient has not granted \
                         capability {recv_action:?}",
                        task.recipient.display
                    ),
                };
            }
        }
        match self.mailbox.send_task(task).await {
            Ok(()) => Response::A2ATaskQueued { task_id },
            Err(e) => Response::Error {
                message: format!("a2a: {e}"),
            },
        }
    }

    /// Returns true iff the capability store has a non-revoked,
    /// non-expired grant for `action` with `subject = recipient.pubkey`.
    /// Used by Sprint 59's recipient admission gate. The lookup keys on
    /// the 32-byte pubkey, not the wire-supplied display.
    async fn recipient_has_recv_action(&self, recipient: &AgentId, action: &str) -> bool {
        let now = epoch_ms();
        let caps = self
            .capabilities
            .list_for_subject(recipient.pubkey)
            .await
            .unwrap_or_default();
        caps.iter()
            .filter(|c| verify_with_clock(c, now).is_ok())
            .any(|c| c.capability.action == action)
    }

    async fn try_recv_a2a_task(&self, peer: &AgentId) -> Response {
        match self.mailbox.try_recv_task_for(peer).await {
            Ok(task) => Response::A2ATaskOpt { task },
            Err(e) => Response::Error {
                message: format!("a2a: {e}"),
            },
        }
    }

    async fn post_a2a_result(
        &self,
        result: covenant_a2a::A2ATaskResult,
        peer: &AgentId,
    ) -> Response {
        let task_id = result.task_id;
        let sender = match self.mailbox.lookup_task_sender(task_id).await {
            Ok(Some(s)) => s,
            Ok(None) => {
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: peer.clone(),
                    kind: AuditKind::A2AResultRejected {
                        task_id,
                        reason: "unknown_task".into(),
                    },
                };
                self.record_peer_event(peer, event).await;
                return Response::Error {
                    message: format!(
                        "a2a respond rejected: task_id {task_id} was never \
                         dispatched through this daemon"
                    ),
                };
            }
            Err(e) => {
                return Response::Error {
                    message: format!("a2a: {e}"),
                };
            }
        };
        let action = format!("a2a.respond.{}", sender.display);
        let check = self
            .check_capabilities(format!("a2a-respond:{task_id}"), vec![action.clone()], peer)
            .await;
        if !check.passed {
            return Response::Error {
                message: format!(
                    "a2a respond to {} requires capability {action:?}. \
                     Grant it with `covenant capabilities grant {action}`.",
                    sender.display
                ),
            };
        }
        match self.mailbox.send_result(result).await {
            Ok(()) => Response::A2AResultPosted { task_id },
            Err(e) => Response::Error {
                message: format!("a2a: {e}"),
            },
        }
    }

    async fn try_recv_a2a_result(&self, peer: &AgentId) -> Response {
        match self.mailbox.try_recv_result_for(peer).await {
            Ok(result) => Response::A2AResultOpt { result },
            Err(e) => Response::Error {
                message: format!("a2a: {e}"),
            },
        }
    }

    /// Returns A2A tasks where `peer` is either the sender or recipient.
    /// Bidirectional filter — a peer's natural view spans tasks they sent
    /// (their outbound queue) and tasks addressed to them (their inbox).
    /// Sprint 49's hard `task.sender == peer` send-time invariant means
    /// the sender direction is forge-resistant; recipient is wire-supplied
    /// at send time so an adversarial peer cannot craft a recipient match
    /// that wasn't already routed to them at send. Compared on the 32-byte
    /// pubkey, not the display string. Sprint 58g per-peer filter.
    async fn recent_a2a_tasks(&self, limit: usize, peer: &AgentId) -> Response {
        match self.mailbox.recent_tasks(limit).await {
            Ok(tasks) => {
                let tasks = tasks
                    .into_iter()
                    .filter(|t| t.sender.pubkey == peer.pubkey || t.recipient.pubkey == peer.pubkey)
                    .collect();
                Response::A2ATasks { tasks }
            }
            Err(e) => Response::Error {
                message: format!("a2a: {e}"),
            },
        }
    }

    /// Returns A2A results whose original task sender matches `peer`.
    /// Joins each `result.task_id` against `Mailbox::lookup_task_sender`
    /// (the post-Sprint-49 senders-map invariant: `senders[task_id] ==
    /// authenticated_peer_at_send`); rows whose lookup returns `None` (the
    /// task pre-dates the senders map, or was compacted) drop, matching
    /// Sprint 50's `try_recv_a2a_result_for` posture. Lookup errors drop
    /// the row and warn — the operator dashboard prefers a missing row
    /// over a leaked one. Compared on the 32-byte pubkey. Sprint 58g.
    async fn recent_a2a_results(&self, limit: usize, peer: &AgentId) -> Response {
        let results = match self.mailbox.recent_results(limit).await {
            Ok(r) => r,
            Err(e) => {
                return Response::Error {
                    message: format!("a2a: {e}"),
                };
            }
        };
        let mut filtered = Vec::with_capacity(results.len());
        for result in results {
            match self.mailbox.lookup_task_sender(result.task_id).await {
                Ok(Some(sender)) if sender.pubkey == peer.pubkey => filtered.push(result),
                Ok(_) => {}
                Err(e) => {
                    warn!(error = %e, task_id = %result.task_id, "a2a: lookup_task_sender failed; dropping row");
                }
            }
        }
        Response::A2AResults { results: filtered }
    }

    /// Daemon-side fan-out across `router.agents()` for the operator
    /// budget dashboard. Per-agent buckets live in [`BudgetLedger`] and
    /// the trait method takes a single agent — for the flat operator
    /// view we walk the registered cards (skipping zero-budget Phase-0
    /// manifests, same predicate as [`Self::register_agent_budgets`]),
    /// pull `limit` debits per agent, merge, sort newest-first, and
    /// truncate. Read-only; no capability gate, same posture as
    /// `RecentMemory` / `RecentReceipts` / `RecentAudit`.
    ///
    /// **No per-peer filter (Sprint 58g punt):** `BudgetDebit.agent` is
    /// the rate-limited *agent* (e.g. `research@agent`), not the
    /// dispatcher peer. The budget belongs to the agent and is shared
    /// across every peer that dispatches through it. Per-peer attribution
    /// requires extending `BudgetDebit` with `dispatched_by:
    /// Option<AgentId>` and threading it through `try_debit`; that lands
    /// when the budget itself becomes per-peer (Phase-1 multi-tenant
    /// migration). v0 single-peer makes the leak surface non-existent.
    async fn recent_debits(&self, limit: usize) -> Response {
        let mut all: Vec<covenant_budget::BudgetDebit> = Vec::new();
        for card in self.router.agents() {
            if card.manifest.settlement.budget_credits_per_hour == 0 {
                continue;
            }
            let agent = agent_id_for_card(card);
            match self.budget.recent_debits(&agent, limit).await {
                Ok(debits) => all.extend(debits),
                Err(e) => {
                    return Response::Error {
                        message: format!("budget: {e}"),
                    };
                }
            }
        }
        all.sort_by(|a, b| b.at_ms.cmp(&a.at_ms));
        all.truncate(limit);
        Response::Debits { debits: all }
    }

    async fn compact_a2a(&self, peer: &AgentId) -> Response {
        let required = vec!["a2a.compact".to_string()];
        let check = self
            .check_capabilities("a2a:compact".into(), required, peer)
            .await;
        if !check.passed {
            return Response::Error {
                message: "a2a compact requires capability \"a2a.compact\". \
                     Grant it with `covenant capabilities grant a2a.compact`."
                    .into(),
            };
        }
        match self.mailbox.compact().await {
            Ok(dropped) => Response::A2ACompacted { dropped },
            Err(e) => Response::Error {
                message: format!("a2a: {e}"),
            },
        }
    }

    /// Rotate the operator's bootstrap token. Sprint 60.
    ///
    /// Order is load-bearing — see Plan-gate A1 in SPRINT_LOG Sprint 60.
    /// 1. Mint a fresh `PeerToken`.
    /// 2. Read the current token off disk so we know which one to revoke.
    /// 3. Register the new entry under the operator's `AgentId`.
    /// 4. Write the new token to `<home>/peers/operator.token` (mode 0600).
    /// 5. Revoke the old token in the registry.
    /// 6. Record an `OperatorTokenRotated` audit event.
    ///
    /// Crashing between (3) and (4) leaves the new token registered but
    /// not on disk; the next daemon boot reads the old (still-valid)
    /// token from disk and the orphan registry entry is harmless. The
    /// inverse — write-disk-before-register — would expose a window where
    /// the on-disk token resolves to nothing, locking the operator out.
    ///
    /// Gated to the operator's own identity — `peer.pubkey ==
    /// self.identity.pubkey`. v0 has only one peer (the operator), so any
    /// authenticated caller would pass a `peers.rotate` capability check
    /// anyway; the identity gate is the right invariant going into Phase-1
    /// multi-peer where a guest peer must not rotate the operator's token.
    async fn rotate_operator_token(&self, peer: &AgentId) -> Response {
        let identity_pubkey = self.identity.agent_id().pubkey;
        if peer.pubkey != identity_pubkey {
            return Response::Error {
                message: "operator token rotation requires the operator identity".into(),
            };
        }
        let Some(home) = self.home.clone() else {
            return Response::Error {
                message:
                    "operator token rotation unavailable: server has no home directory configured"
                        .into(),
            };
        };
        let token_path = home.join("peers").join("operator.token");

        let old_token = match read_operator_token_b58(&token_path) {
            Ok(t) => t,
            Err(e) => {
                return Response::Error {
                    message: format!("read operator token at {}: {e}", token_path.display()),
                };
            }
        };

        let new_token = PeerToken::generate();
        let new_entry = PeerEntry {
            token: new_token,
            agent_id: peer.clone(),
            registered_at: epoch_ms(),
        };
        if let Err(e) = self.peers.register(new_entry).await {
            return Response::Error {
                message: format!("register new operator token: {e}"),
            };
        }
        if let Err(e) = write_operator_token_0600(&token_path, &new_token.to_b58()) {
            // Best-effort rollback: the new token is registered but the
            // on-disk write failed, so the old token still resolves and is
            // still on disk. Leave the new entry in the registry — it
            // costs one row and a future rotation (or peer purge) cleans
            // it up. Surfacing the error preserves operator agency.
            return Response::Error {
                message: format!("write new operator token to {}: {e}", token_path.display()),
            };
        }
        match self.peers.revoke(&old_token).await {
            Ok(_) => {}
            Err(e) => {
                // The new token is on disk and registered; the old token
                // failed to revoke. The operator's next boot reads the new
                // token from disk and authenticates fine; the old one is
                // still live as a registry record but the operator no
                // longer has its bytes. Audit + warn rather than error
                // out — the rotation succeeded for every observable use.
                warn!(error = %e, "rotate: revoke old token failed; new token is live");
            }
        }

        let event = AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: epoch_ms(),
            issuer: peer.clone(),
            kind: AuditKind::OperatorTokenRotated {
                peer_display: peer.display.clone(),
                old_token_prefix: token_b58_prefix(&old_token),
                new_token_prefix: token_b58_prefix(&new_token),
            },
        };
        self.record_peer_event(peer, event).await;

        Response::OperatorTokenRotated {
            token_b58: new_token.to_b58(),
        }
    }

    async fn purge_peers(&self, before_ms: u64, peer: &AgentId) -> Response {
        let required = vec!["peers.purge".to_string()];
        let check = self
            .check_capabilities("peers:purge".into(), required, peer)
            .await;
        if !check.passed {
            return Response::Error {
                message: "peers purge requires capability \"peers.purge\". \
                     Grant it with `covenant capabilities grant peers.purge`."
                    .into(),
            };
        }
        match self.peers.purge_revoked_older_than(before_ms).await {
            Ok(purged) => Response::PeersPurged { purged },
            Err(e) => Response::Error {
                message: format!("peers: {e}"),
            },
        }
    }

    /// Returns audit rows whose `issuer.pubkey` matches `peer.pubkey`.
    /// Filtering at the Server boundary (not in the storage trait) keeps
    /// `AuditLog` peer-agnostic and lets every read surface re-use the
    /// same predicate. Compared on the 32-byte pubkey, not the display
    /// string, because the display can be re-used across pubkeys at the
    /// wire boundary even with `validate_agent_id_display` (Sprint 45).
    /// In v0 every authenticated caller is the operator and `peer.pubkey
    /// == identity.pubkey`, so the filter degenerates to a no-op — the
    /// behaviour change matters only once a second peer authenticates.
    /// `AuthenticationFailed` rows have `issuer == identity` (no
    /// authenticated peer at the moment of rejection) and so naturally
    /// remain visible only to the operator.
    async fn recent_audit(&self, limit: usize, peer: &AgentId) -> Response {
        match self.audit.recent(limit).await {
            Ok(events) => {
                let events = events
                    .into_iter()
                    .filter(|e| e.issuer.pubkey == peer.pubkey)
                    .collect();
                Response::AuditEvents { events }
            }
            Err(e) => Response::Error {
                message: format!("audit: {e}"),
            },
        }
    }

    async fn purge_audit(&self, before_ms: u64, peer: &AgentId) -> Response {
        let required = vec!["audit.purge".to_string()];
        let check = self
            .check_capabilities("audit:purge".into(), required, peer)
            .await;
        if !check.passed {
            return Response::Error {
                message: "audit purge requires capability \"audit.purge\". \
                     Grant it with `covenant capabilities grant audit.purge`."
                    .into(),
            };
        }
        match self.audit.purge_older_than(before_ms).await {
            Ok(purged) => Response::AuditPurged { purged },
            Err(e) => Response::Error {
                message: format!("audit: {e}"),
            },
        }
    }

    async fn purge_capabilities(&self, before_ms: u64, peer: &AgentId) -> Response {
        let required = vec!["capabilities.purge".to_string()];
        let check = self
            .check_capabilities("capabilities:purge".into(), required, peer)
            .await;
        if !check.passed {
            return Response::Error {
                message: "capabilities purge requires capability \"capabilities.purge\". \
                     Grant it with `covenant capabilities grant capabilities.purge`."
                    .into(),
            };
        }
        match self.capabilities.purge_revoked_older_than(before_ms).await {
            Ok(purged) => Response::CapabilitiesPurged { purged },
            Err(e) => Response::Error {
                message: format!("permissions: {e}"),
            },
        }
    }

    fn list_tools(&self) -> Response {
        Response::ToolList {
            tools: self.tools.list_specs(),
        }
    }

    async fn call_tool(
        &self,
        name: String,
        arguments: serde_json::Value,
        peer: &AgentId,
    ) -> Response {
        let required = vec![format!("tool.call.{name}")];
        let check = self
            .check_capabilities(format!("tool:{name}"), required, peer)
            .await;
        if !check.passed {
            return Response::Error {
                message: format!(
                    "tool {name} requires capability {:?}. Grant it with `covenant capabilities grant {}`.",
                    check.missing,
                    check.missing.first().cloned().unwrap_or_default()
                ),
            };
        }
        match self.tools.call(&name, arguments).await {
            Ok(r) => Response::ToolResult {
                content: r.content,
                is_error: r.is_error,
            },
            Err(e) => Response::Error {
                message: format!("tool: {e}"),
            },
        }
    }

    fn check_ignore(&self, text: String) -> Response {
        let v = self.ignore.check(&text);
        Response::IgnoreReport {
            ignored: v.ignored,
            matched_pattern: v.matched.map(|p| p.raw().trim().to_string()),
            rules_loaded: self.ignore.len(),
        }
    }

    async fn dispatch_intent(&self, text: String, peer: &AgentId) -> Response {
        let intent_id = Uuid::new_v4();
        // Pre-allocated so the budget debit's `paired_receipt` and the
        // settlement receipt's `id` agree — joining the budget log to
        // the receipt log on this UUID matches 1:1 instead of producing
        // zero matches (security-review L1 closure).
        let receipt_id = Uuid::new_v4();
        let issued_at = epoch_ms();

        let issuer = peer.clone();

        let ignore_check = self.ignore.check(&text);
        if ignore_check.ignored {
            let matched_pattern = ignore_check
                .matched
                .map(|p| p.raw().trim().to_string())
                .unwrap_or_default();
            let event = AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: issuer.clone(),
                kind: AuditKind::IntentIgnored {
                    intent_id,
                    intent_text: text.clone(),
                    matched_pattern: matched_pattern.clone(),
                },
            };
            self.record_peer_event(peer, event).await;
            return Response::IntentResult {
                intent_id,
                status: "ignored".into(),
                text: format!("ignored by .covenantignore rule: {matched_pattern}"),
                sources: Vec::new(),
                settlement: None,
            };
        }

        let matched = self.router.route(&text);
        let card = matched
            .as_ref()
            .and_then(|m| self.router.find_by_id(&m.agent_id));

        let (text_out, sources_out) = if let Some(card) = card {
            let check = self
                .check_capabilities(
                    card.id.clone(),
                    card.manifest.capabilities.required.clone(),
                    peer,
                )
                .await;
            if !check.passed {
                return Response::Error {
                    message: format!(
                        "agent {} is missing capabilities: {:?}. Grant them with `covenant capabilities grant <action>`.",
                        card.id, check.missing
                    ),
                };
            }
            // Phase-0 manifests still default `budget_credits_per_hour = 0`.
            // The `Settlement` rustdoc says "Phase 0 tolerates 0; enforced
            // from Phase 1." So skip the debit path entirely when the
            // manifest opts out of budget enforcement. Non-zero capacity
            // gets the full token-bucket gate.
            if card.manifest.settlement.budget_credits_per_hour > 0 {
                let agent = agent_id_for_card(card);
                let requested = intent_dispatch_credits();
                match self.budget.try_debit(&agent, requested, receipt_id).await {
                    Ok(()) => {}
                    Err(BudgetError::NoCapacity(_)) => {
                        // Manifest opts in to budget but the bucket was never
                        // seeded — operator forgot to call
                        // `register_agent_budgets`, or a hot-reload added the
                        // manifest without re-seeding. v0 still passes
                        // (don't block dispatch on a misconfigured daemon)
                        // but the bypass now lands in /audit/recent so the
                        // operator sees it. Sprint 58c M2 closure.
                        warn!(
                            agent = %card.id,
                            "no budget capacity registered for agent; \
                             dispatching without debit (call register_agent_budgets at startup)"
                        );
                        let event = AuditEvent {
                            id: Uuid::new_v4(),
                            timestamp_ms: epoch_ms(),
                            issuer: issuer.clone(),
                            kind: AuditKind::BudgetUnseeded {
                                agent_display: agent.display.clone(),
                                intent_id,
                                requested,
                            },
                        };
                        self.record_peer_event(peer, event).await;
                    }
                    Err(BudgetError::Exhausted {
                        tokens_remaining,
                        refill_eta_ms,
                    }) => {
                        let event = AuditEvent {
                            id: Uuid::new_v4(),
                            timestamp_ms: epoch_ms(),
                            issuer: issuer.clone(),
                            kind: AuditKind::BudgetExhausted {
                                agent_display: agent.display.clone(),
                                intent_id,
                                intent_text: text.clone(),
                                requested,
                                tokens_remaining,
                                refill_eta_ms,
                            },
                        };
                        self.record_peer_event(peer, event).await;
                        // Wire response rounds tokens_remaining to a coarse
                        // bucket; the audit row above keeps the precise u64.
                        // Sprint 58c L3 closure (multi-peer prep).
                        let coarse = round_tokens_remaining(tokens_remaining);
                        return Response::Error {
                            message: format!(
                                "agent {} budget exhausted: requested {requested} credit(s), \
                                 ≥{coarse} remaining, refill eta {refill_eta_ms}ms",
                                card.id
                            ),
                        };
                    }
                    Err(e) => {
                        warn!(agent = %card.id, error = %e, "budget debit failed");
                        return Response::Error {
                            message: format!("budget debit failed for {}: {e}", card.id),
                        };
                    }
                }
            }
            let intent = Intent {
                id: intent_id,
                text: text.clone(),
                issuer: issuer.clone(),
                issued_at,
                priority: Priority::Normal,
                parent: None,
            };
            match self.runner.run(card, &intent).await {
                Ok(result) => (result.text, result.sources),
                Err(e) => {
                    warn!(agent = %card.id, error = %e, "agent run failed");
                    return Response::Error {
                        message: format!("agent {} failed: {e}", card.id),
                    };
                }
            }
        } else {
            (
                format!("phase 0 echo (no agent matched): {text}"),
                Vec::new(),
            )
        };

        let embedding = match self.embedder.embed(&text_out).await {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, embedder = %self.embedder.name(), "embed failed; storing without vector");
                Vec::new()
            }
        };
        let record = MemoryRecord {
            id: intent_id,
            tier: MemoryTier::Working,
            owner: issuer.clone(),
            text: text_out.clone(),
            embedding,
            metadata: serde_json::json!({
                "intent_text": text,
                "agent_id": card.map(|c| c.id.clone()),
                "status": "ok",
            }),
            created_at: epoch_ms(),
            parent: None,
        };
        let bytes_written = record.text.len();
        if let Err(e) = self.memory.put(record).await {
            warn!(error = %e, "memory write failed");
        } else {
            let receipt = SettlementReceipt {
                id: receipt_id,
                payer: issuer.clone(),
                resource: ResourceKind::Memory,
                credits_consumed: memory_write_credits(bytes_written),
                settled_at: epoch_ms(),
                onchain_sig: None,
            };
            if let Err(e) = self.settlement.record(receipt).await {
                warn!(error = %e, "settlement record failed");
            }
        }

        let audit_event = AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: epoch_ms(),
            issuer: issuer.clone(),
            kind: AuditKind::IntentDispatched {
                intent_id,
                intent_text: text.clone(),
                matched_agent: card.map(|c| c.id.clone()),
                result_hash_hex: hash_hex(text_out.as_bytes()),
                status: "ok".into(),
            },
        };
        self.record_peer_event(peer, audit_event).await;

        Response::IntentResult {
            intent_id,
            status: "ok".into(),
            text: text_out,
            sources: sources_out,
            settlement: None,
        }
    }

    /// Re-dispatch a previously budget-rejected intent. The audit log's
    /// `BudgetExhausted` row carries the original `intent_text`, so the
    /// resume verb scans recent audit, finds the matching `intent_id`,
    /// and runs the text through `dispatch_intent` like any fresh
    /// `SubmitIntent` (capability check, ignore rules, and budget gate
    /// all re-run — the bucket may have refilled). Returns
    /// `Response::Error` if no `BudgetExhausted` row matches the supplied
    /// `intent_id`. Sprint 58c — closes the §11 pin's "queue a resume"
    /// half for Phase-0 single-shot agents (Phase-1 multi-step agents
    /// will need an actual checkpoint/restart mechanism on top of this).
    async fn resume_intent(&self, intent_id: Uuid, peer: &AgentId) -> Response {
        // Recent-audit window: 1024 events covers the typical operator
        // turnaround (a few minutes of feed) without scanning the whole
        // log. If the row has aged out, the operator must re-submit
        // with the original text via SubmitIntent.
        let window = 1024usize;
        let events = match self.audit.recent(window).await {
            Ok(es) => es,
            Err(e) => {
                warn!(error = %e, "audit recent failed during resume");
                return Response::Error {
                    message: format!("resume: audit read failed: {e}"),
                };
            }
        };
        // Filter to the resuming peer's own rows BEFORE the find_map.
        // Resuming someone else's `BudgetExhausted` would otherwise leak
        // their `intent_text` through `dispatch_intent`'s code path. Same
        // pubkey-equality predicate as `recent_audit`. Sprint 58d.
        let text = events
            .iter()
            .filter(|e| e.issuer.pubkey == peer.pubkey)
            .rev()
            .find_map(|e| match &e.kind {
                AuditKind::BudgetExhausted {
                    intent_id: row_id,
                    intent_text,
                    ..
                } if *row_id == intent_id => Some(intent_text.clone()),
                _ => None,
            });
        match text {
            Some(t) => self.dispatch_intent(t, peer).await,
            None => Response::Error {
                message: format!(
                    "resume: no BudgetExhausted audit row for intent {intent_id} \
                     within last {window} events"
                ),
            },
        }
    }

    /// Capability check: returns required + missing + passed. Logs a
    /// `CapabilityCheck` audit event attributed to `peer`. `scope_id` is
    /// the subject of the check (an agent id or `tool:<name>`); it lands
    /// in the audit row so operators can distinguish. The capability set
    /// consulted is the one keyed on `peer.pubkey` — in v0 every authenticated
    /// caller is the operator (peer.pubkey == identity.pubkey), but the
    /// per-peer keying lays groundwork for multi-peer.
    async fn check_capabilities(
        &self,
        scope_id: String,
        required: Vec<String>,
        peer: &AgentId,
    ) -> CapabilityCheckOutcome {
        let now = epoch_ms();
        if required.is_empty() {
            return CapabilityCheckOutcome {
                passed: true,
                required,
                missing: Vec::new(),
            };
        }
        let user_caps = self
            .capabilities
            .list_for_subject(peer.pubkey)
            .await
            .unwrap_or_default();
        let valid_actions: Vec<String> = user_caps
            .iter()
            .filter(|c| verify_with_clock(c, now).is_ok())
            .map(|c| c.capability.action.clone())
            .collect();
        let missing: Vec<String> = required
            .iter()
            .filter(|a| !valid_actions.iter().any(|v| v == *a))
            .cloned()
            .collect();
        let passed = missing.is_empty();
        let event = AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: now,
            issuer: peer.clone(),
            kind: AuditKind::CapabilityCheck {
                agent_id: scope_id,
                required_actions: required.clone(),
                missing_actions: missing.clone(),
                passed,
            },
        };
        self.record_peer_event(peer, event).await;
        CapabilityCheckOutcome {
            passed,
            required,
            missing,
        }
    }

    async fn grant_capability(
        &self,
        action: String,
        scope: Option<serde_json::Value>,
        expires_at: Option<u64>,
        peer: &AgentId,
    ) -> Response {
        let granted_by = self.identity.agent_id();
        let cap = Capability {
            subject: peer.clone(),
            action: action.clone(),
            scope: scope.unwrap_or_else(|| serde_json::json!({})),
            granted_by: granted_by.clone(),
            expires_at,
        };
        let signed = sign_capability(cap, self.identity.signing_key());
        let signature_b58 = bs58::encode(signed.signature).into_string();

        if let Err(e) = self.capabilities.record(signed.clone()).await {
            return Response::Error {
                message: format!("permissions: failed to record capability: {e}"),
            };
        }

        let event = AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: epoch_ms(),
            issuer: peer.clone(),
            kind: AuditKind::CapabilityGranted {
                subject_display: peer.display.clone(),
                action: action.clone(),
                granted_by_display: granted_by.display.clone(),
                signature_b58: signature_b58.clone(),
            },
        };
        self.record_peer_event(peer, event).await;

        Response::CapabilityGranted {
            signature_b58,
            subject_display: peer.display.clone(),
            action,
        }
    }

    /// Returns memory records owned by `peer`. `MemoryRecord.owner` is
    /// set to the authenticated peer in `dispatch_intent`, so the filter
    /// keys directly off the dispatch attribution. Compared on the
    /// 32-byte pubkey. Sprint 58g per-peer filter.
    async fn recent_memory(
        &self,
        tier: Option<MemoryTier>,
        limit: usize,
        peer: &AgentId,
    ) -> Response {
        match self.memory.recent(tier, limit).await {
            Ok(records) => {
                let records = records
                    .into_iter()
                    .filter(|r| r.owner.pubkey == peer.pubkey)
                    .collect();
                Response::Memories { records }
            }
            Err(e) => Response::Error {
                message: format!("memory: {e}"),
            },
        }
    }

    /// Returns settlement receipts where `peer` is the payer.
    /// `SettlementReceipt.payer` is set to the authenticated peer in
    /// `dispatch_intent`, so the filter keys directly off the dispatch
    /// attribution. Compared on the 32-byte pubkey. Sprint 58g.
    async fn recent_receipts(&self, limit: usize, peer: &AgentId) -> Response {
        match self.settlement.recent(limit).await {
            Ok(receipts) => {
                let receipts = receipts
                    .into_iter()
                    .filter(|r| r.payer.pubkey == peer.pubkey)
                    .collect();
                Response::Receipts { receipts }
            }
            Err(e) => Response::Error {
                message: format!("settlement: {e}"),
            },
        }
    }

    async fn search_memory(
        &self,
        query: String,
        tier: Option<MemoryTier>,
        limit: usize,
    ) -> Response {
        let q_emb = match self.embedder.embed(&query).await {
            Ok(v) => v,
            Err(e) => {
                return Response::Error {
                    message: format!("embed: {e}"),
                };
            }
        };
        match self.memory.search_similar(q_emb, tier, limit).await {
            Ok(records) => Response::Memories { records },
            Err(e) => Response::Error {
                message: format!("memory: {e}"),
            },
        }
    }

    /// Cross-check the last `window` records of memory / audit / receipts /
    /// capabilities for drift. Returns a report with per-check pass/fail and
    /// a total orphan count. Useful for paranoid operator inspection.
    async fn verify_recent(&self, window: usize) -> Response {
        use covenant_audit::AuditKind;
        use covenant_ipc::VerifyCheck;
        use std::collections::HashSet;

        let mut checks: Vec<VerifyCheck> = Vec::new();
        let mut orphans_total: u64 = 0;

        let memories = self.memory.recent(None, window).await.unwrap_or_default();
        let audits = self.audit.recent(window).await.unwrap_or_default();
        let receipts = self.settlement.recent(window).await.unwrap_or_default();
        let caps = self.capabilities.recent(window).await.unwrap_or_default();

        // Check 1: every memory record's id appears as an IntentDispatched
        // audit event's intent_id. The other direction (audit without memory)
        // is also drift but rarer in practice; report both.
        let memory_ids: HashSet<Uuid> = memories.iter().map(|m| m.id).collect();
        let dispatched_intent_ids: HashSet<Uuid> = audits
            .iter()
            .filter_map(|e| match &e.kind {
                AuditKind::IntentDispatched { intent_id, .. } => Some(*intent_id),
                _ => None,
            })
            .collect();
        let memory_orphans: u64 = memory_ids
            .iter()
            .filter(|id| !dispatched_intent_ids.contains(id))
            .count() as u64;
        let audit_orphans: u64 = dispatched_intent_ids
            .iter()
            .filter(|id| !memory_ids.contains(id))
            .count() as u64;
        orphans_total += memory_orphans + audit_orphans;
        checks.push(VerifyCheck {
            name: "memory ↔ audit".into(),
            passed: memory_orphans == 0 && audit_orphans == 0,
            message: format!(
                "{} memory orphan(s), {} audit orphan(s)",
                memory_orphans, audit_orphans
            ),
        });

        // Check 2: every capability in the granted set has a matching
        // CapabilityGranted audit event. Mismatch means an out-of-band write
        // to granted.jsonl that didn't go through the daemon.
        let audited_grant_sigs: HashSet<String> = audits
            .iter()
            .filter_map(|e| match &e.kind {
                AuditKind::CapabilityGranted { signature_b58, .. } => Some(signature_b58.clone()),
                _ => None,
            })
            .collect();
        let cap_orphans: u64 = caps
            .iter()
            .filter(|c| {
                let s = bs58::encode(c.signature).into_string();
                !audited_grant_sigs.contains(&s)
            })
            .count() as u64;
        orphans_total += cap_orphans;
        checks.push(VerifyCheck {
            name: "capability ↔ audit".into(),
            passed: cap_orphans == 0,
            message: format!(
                "{} capabilit(ies) without matching grant audit event",
                cap_orphans
            ),
        });

        // Check 3: memory writes and settlement receipts should be 1:1.
        // Mismatch means a memory write succeeded but the settlement record
        // failed (or vice versa) — Phase 0 is fail-soft on settlement.
        let mem = memories.len();
        let rec = receipts.len();
        let pair_diff = mem.abs_diff(rec) as u64;
        orphans_total += pair_diff;
        checks.push(VerifyCheck {
            name: "memory ↔ receipts".into(),
            passed: pair_diff == 0,
            message: format!(
                "{} memory record(s) vs {} receipt(s); diff = {}",
                mem, rec, pair_diff
            ),
        });

        Response::VerifyReport {
            window,
            checks,
            orphans_total,
        }
    }

    async fn purge_memory(&self, tier: Option<MemoryTier>, before_ms: u64) -> Response {
        match self.memory.purge_older_than(tier, before_ms).await {
            Ok(purged) => Response::MemoryPurged { purged },
            Err(e) => Response::Error {
                message: format!("memory: {e}"),
            },
        }
    }

    /// Returns capabilities where `peer` is either the subject (caps held
    /// against them) or `granted_by` (caps they granted). Bidirectional
    /// because both ends of a delegation are natural privacy boundaries —
    /// a peer wants to see what they're authorised for AND what they
    /// delegated out. v0 single-peer collapses to operator's own grants
    /// (subject == granted_by). Compared on the 32-byte pubkey. Sprint
    /// 58g per-peer filter.
    async fn recent_capabilities(&self, limit: usize, peer: &AgentId) -> Response {
        match self.capabilities.recent(limit).await {
            Ok(capabilities) => {
                let capabilities = capabilities
                    .into_iter()
                    .filter(|c| {
                        c.capability.subject.pubkey == peer.pubkey
                            || c.capability.granted_by.pubkey == peer.pubkey
                    })
                    .collect();
                Response::Capabilities { capabilities }
            }
            Err(e) => Response::Error {
                message: format!("permissions: {e}"),
            },
        }
    }

    async fn revoke_capability(&self, signature_b58: String, peer: &AgentId) -> Response {
        let bytes = match bs58::decode(&signature_b58).into_vec() {
            Ok(b) if b.len() == 64 => {
                let mut arr = [0u8; 64];
                arr.copy_from_slice(&b);
                arr
            }
            Ok(_) => {
                return Response::Error {
                    message: "signature must decode to 64 bytes".into(),
                };
            }
            Err(e) => {
                return Response::Error {
                    message: format!("invalid base58 signature: {e}"),
                };
            }
        };
        // Peer can only revoke caps whose subject is peer. The daemon
        // is the trust root and signs every cap, but the cap is *for*
        // the subject — a different peer must not be able to revoke it
        // by replaying a signature visible on `/capabilities/recent`.
        let owned = self
            .capabilities
            .list_for_subject(peer.pubkey)
            .await
            .unwrap_or_default();
        if !owned.iter().any(|c| c.signature == bytes) {
            let event = AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: peer.clone(),
                kind: AuditKind::CapabilityRevokeRejected {
                    signature_b58: signature_b58.clone(),
                    reason: "peer is not the subject of this capability".into(),
                },
            };
            self.record_peer_event(peer, event).await;
            return Response::Error {
                message: "revoke rejected: capability subject does not match authenticated peer"
                    .into(),
            };
        }
        match self.capabilities.revoke(bytes).await {
            Ok(removed) => Response::CapabilityRevoked {
                signature_b58,
                removed,
            },
            Err(e) => Response::Error {
                message: format!("permissions: {e}"),
            },
        }
    }
}

#[cfg(test)]
impl Server {
    /// Test convenience: respond with the daemon's own identity acting as
    /// the authenticated peer. In production v0 the operator peer is the
    /// daemon's own identity, so this matches what `handle` does after
    /// the auth handshake.
    async fn op_respond(&self, req: Request) -> Response {
        let peer = self.identity.agent_id();
        self.respond(req, &peer).await
    }
}

struct CapabilityCheckOutcome {
    passed: bool,
    #[allow(dead_code)]
    required: Vec<String>,
    missing: Vec<String>,
}

/// Wraps a [`BudgetError`] from [`Server::register_agent_budgets`] with
/// the manifest id that failed, so startup error messages name the
/// offending agent instead of dropping a bare `serde:` line on the
/// operator (security-review L5 closure).
#[derive(Debug)]
pub struct BudgetSeedError {
    pub agent_id: String,
    pub source: BudgetError,
}

impl std::fmt::Display for BudgetSeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "seed budget for agent {:?}: {}",
            self.agent_id, self.source
        )
    }
}

impl std::error::Error for BudgetSeedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Read a base58-encoded operator token off disk and decode it. The file
/// is expected at `<home>/peers/operator.token` and must be mode 0600 —
/// any group/world bit is treated as a credential leak and the read
/// fails. Used by daemon boot to reuse an existing token and by
/// [`Server::rotate_operator_token`] to identify which token to revoke.
fn read_operator_token_b58(path: &std::path::Path) -> std::io::Result<PeerToken> {
    require_operator_token_mode_0600(path)?;
    let s = std::fs::read_to_string(path)?;
    PeerToken::from_b58(s.trim()).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("decode token: {e}"),
        )
    })
}

/// Atomically write `token_b58` to `path` with mode 0600. Reused by
/// daemon boot ([`crate::main`]'s `bootstrap_operator_token`) and
/// [`Server::rotate_operator_token`].
///
/// `OpenOptionsExt::mode` is honoured only on file creation. If the
/// file already exists with a permissive mode, `O_CREAT|O_TRUNC` reuses
/// the inode and our `0o600` is silently ignored. We `remove_file`
/// first to force a fresh inode, then `set_permissions` after writing
/// to defend against any umask-overlay surprises (Sprint 47 lesson).
pub fn write_operator_token_0600(path: &std::path::Path, token_b58: &str) -> std::io::Result<()> {
    use std::fs::Permissions;
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(token_b58.as_bytes())?;
    f.write_all(b"\n")?;
    f.flush()?;
    std::fs::set_permissions(path, Permissions::from_mode(0o600))?;
    Ok(())
}

/// Refuse to read a token whose mode is anything but `0o600`. Loud
/// failure forces operator action — silently regenerating would still
/// leak the prior token to whoever could read the permissive file.
pub fn require_operator_token_mode_0600(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path)?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} mode is {:#o}; expected 0o600 (any group/world bit is a credential leak)",
                path.display(),
                mode
            ),
        ));
    }
    Ok(())
}

/// 6-char base58 prefix of `token`. Matches `PeerToken::Debug`'s
/// redaction posture so audit rows stay grep-correlatable with debug
/// logs without ever recording the full secret.
fn token_b58_prefix(token: &PeerToken) -> String {
    let s = token.to_b58();
    s.chars().take(6).collect()
}

/// Coarse-bucket rounding for the `tokens_remaining` value embedded in
/// the wire `Response::Error` message of an exhausted dispatch.
/// Powers-of-5 sequence — operator-readable and defeats fine-grained
/// inference of peer state in the multi-peer build (post-58c). The
/// audit row keeps the precise `u64`. Sprint 58c L3 closure.
fn round_tokens_remaining(n: u64) -> u64 {
    const BUCKETS: &[u64] = &[
        0,
        1,
        5,
        10,
        50,
        100,
        500,
        1_000,
        5_000,
        10_000,
        50_000,
        100_000,
        500_000,
        1_000_000,
        5_000_000,
        10_000_000,
        50_000_000,
        100_000_000,
        500_000_000,
        1_000_000_000,
        5_000_000_000,
        10_000_000_000,
    ];
    BUCKETS.iter().rev().copied().find(|&b| b <= n).unwrap_or(0)
}

/// Map an `AgentCard` to a stable `AgentId` for budget keying. v0 agents
/// don't yet have their own ed25519 keypair (the daemon's identity is
/// reused across every agent at dispatch time), so we synthesise a
/// deterministic one from the manifest id: zero-padded bytes for the
/// pubkey, `<id>@agent` for the display. The pubkey is opaque to the
/// budget ledger — used only as a hash-map key — so collision risk is
/// bounded by manifest-id uniqueness, which the operator already
/// enforces. The `@agent` host segment satisfies `validate_agent_id_display`'s
/// `<local>@<host>` shape so the synthesised id round-trips through
/// `JsonlLedger`'s serde without rejection.
fn agent_id_for_card(card: &AgentCard) -> AgentId {
    let mut pk = [0u8; 32];
    for (i, b) in card.id.bytes().take(32).enumerate() {
        pk[i] = b;
    }
    AgentId::new(format!("{}@agent", card.id), pk)
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_manifest::Manifest;
    use covenant_memory::InMemoryStore;
    use covenant_router::AgentCard;
    use covenant_runtime::MockRunner;
    use covenant_settlement::InMemorySettlement;

    fn stub_card(id: &str, capabilities: Vec<&str>) -> AgentCard {
        let toml = format!(
            r#"
[agent]
id = "{id}"
name = "{id}"
version = "0.0.1"
runtime = "rust-bin"
entry = "./fake"

[capabilities]
required = {caps:?}
"#,
            caps = capabilities
        );
        let m = Manifest::parse(&toml).unwrap();
        AgentCard::from_manifest_and_dir(m, PathBuf::from("/tmp/nope"))
    }

    fn server_with(cards: Vec<AgentCard>, runner_text: &str) -> Server {
        server_with_ignore(cards, runner_text, IgnoreSet::default())
    }

    fn server_with_ignore(cards: Vec<AgentCard>, runner_text: &str, ignore: IgnoreSet) -> Server {
        Server::new(
            Arc::new(Router::from_cards(cards)),
            Arc::new(MockRunner::new(runner_text)),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            Arc::new(covenant_audit::InMemoryAuditLog::new()),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(ignore),
            Arc::new(ToolRegistry::from_tools(vec![
                Arc::new(covenant_mcp::native::EchoTool),
                Arc::new(covenant_mcp::native::ClockTool),
            ])),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        )
    }

    #[tokio::test]
    async fn ping_returns_pong() {
        let s = server_with(vec![], "");
        assert_eq!(s.op_respond(Request::Ping).await, Response::Pong);
    }

    #[tokio::test]
    async fn submit_intent_writes_memory_and_settlement() {
        let s = server_with(
            vec![stub_card("research", vec!["tool.web_search"])],
            "mocked summary",
        );
        // Hard enforcement: grant the required cap up-front.
        s.op_respond(Request::GrantCapability {
            action: "tool.web_search".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s
            .op_respond(Request::SubmitIntent {
                text: "find recent papers on agent memory".into(),
            })
            .await;
        match resp {
            Response::IntentResult { text, .. } => assert_eq!(text, "mocked summary"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn submit_intent_rejects_when_capabilities_missing() {
        let s = server_with(
            vec![stub_card("research", vec!["tool.web_search"])],
            "mocked summary",
        );
        let resp = s
            .op_respond(Request::SubmitIntent {
                text: "find recent papers on agent memory".into(),
            })
            .await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("missing capabilities"));
                assert!(message.contains("tool.web_search"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn revoke_capability_takes_it_out_of_circulation() {
        let s = server_with(
            vec![stub_card("research", vec!["tool.web_search"])],
            "mocked summary",
        );
        let grant = s
            .op_respond(Request::GrantCapability {
                action: "tool.web_search".into(),
                scope: None,
                expires_at: None,
            })
            .await;
        let sig_b58 = match grant {
            Response::CapabilityGranted { signature_b58, .. } => signature_b58,
            other => panic!("unexpected: {other:?}"),
        };
        // Dispatch passes after grant.
        let r = s
            .op_respond(Request::SubmitIntent {
                text: "find papers".into(),
            })
            .await;
        assert!(matches!(r, Response::IntentResult { .. }));

        // Revoke; dispatch now fails.
        let revoked = s
            .op_respond(Request::RevokeCapability {
                signature_b58: sig_b58,
            })
            .await;
        match revoked {
            Response::CapabilityRevoked { removed, .. } => assert!(removed),
            other => panic!("unexpected: {other:?}"),
        }
        let r2 = s
            .op_respond(Request::SubmitIntent {
                text: "find papers".into(),
            })
            .await;
        assert!(matches!(r2, Response::Error { .. }));
    }

    #[tokio::test]
    async fn submit_intent_falls_back_to_echo_when_no_match() {
        let s = server_with(vec![stub_card("research", vec!["tool.web_search"])], "");
        let resp = s
            .op_respond(Request::SubmitIntent {
                text: "zzz no keywords".into(),
            })
            .await;
        match resp {
            Response::IntentResult { text, .. } => assert!(text.contains("no agent matched")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn grant_capability_signs_and_persists() {
        let s = server_with(vec![], "");
        let resp = s
            .op_respond(Request::GrantCapability {
                action: "tool.web_search".into(),
                scope: None,
                expires_at: None,
            })
            .await;
        let sig_b58 = match resp {
            Response::CapabilityGranted { signature_b58, .. } => signature_b58,
            other => panic!("unexpected: {other:?}"),
        };
        // Recently-granted cap should round-trip through `recent_capabilities`.
        let recent = s
            .op_respond(Request::RecentCapabilities { limit: 10 })
            .await;
        match recent {
            Response::Capabilities { capabilities } => {
                assert_eq!(capabilities.len(), 1);
                let got = bs58::encode(capabilities[0].signature).into_string();
                assert_eq!(got, sig_b58);
                assert_eq!(capabilities[0].capability.action, "tool.web_search");
                assert!(verify_with_clock(&capabilities[0], epoch_ms()).is_ok());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_audits_capability_check_with_missing_actions() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![stub_card(
                "research",
                vec!["tool.web_search", "memory.write"],
            )])),
            Arc::new(MockRunner::new("ok")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );
        // Dispatch will be rejected, but the capability check event is still recorded.
        s.op_respond(Request::SubmitIntent {
            text: "find papers".into(),
        })
        .await;
        let events = audit.recent(10).await.unwrap();
        let cap_check = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::CapabilityCheck { .. }))
            .expect("capability check audit event present");
        match &cap_check.kind {
            AuditKind::CapabilityCheck {
                missing_actions,
                passed,
                required_actions,
                ..
            } => {
                assert_eq!(required_actions.len(), 2);
                assert_eq!(missing_actions.len(), 2);
                assert!(!passed);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_skips_when_intent_matches_ignore_rule() {
        let ignore = IgnoreSet::parse("id_rsa\n");
        let s = server_with_ignore(vec![], "echo", ignore);
        let resp = s
            .op_respond(Request::SubmitIntent {
                text: "summarise ~/.ssh/id_rsa".into(),
            })
            .await;
        match resp {
            Response::IntentResult { status, text, .. } => {
                assert_eq!(status, "ignored");
                assert!(text.contains("id_rsa"));
            }
            other => panic!("unexpected: {other:?}"),
        }
        let recents = s.memory.recent(None, 10).await.unwrap();
        assert!(recents.is_empty(), "ignored intents must not write memory");
        let recpts = s.settlement.recent(10).await.unwrap();
        assert!(
            recpts.is_empty(),
            "ignored intents must not write a receipt"
        );
    }

    #[tokio::test]
    async fn list_tools_returns_registered_tool_specs() {
        let s = server_with(vec![], "");
        let resp = s.op_respond(Request::ListTools).await;
        match resp {
            Response::ToolList { tools } => {
                let names: Vec<String> = tools.into_iter().map(|t| t.name).collect();
                assert_eq!(names, vec!["clock".to_string(), "echo".to_string()]);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn call_tool_dispatches_through_registry() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "tool.call.echo".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s
            .op_respond(Request::CallTool {
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "hi" }),
            })
            .await;
        match resp {
            Response::ToolResult { content, is_error } => {
                assert!(!is_error);
                let txt = match &content[0] {
                    covenant_mcp::Content::Text { text } => text.clone(),
                    other => panic!("unexpected: {other:?}"),
                };
                assert_eq!(txt, "hi");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn call_tool_returns_error_for_unknown_name() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "tool.call.missing".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s
            .op_respond(Request::CallTool {
                name: "missing".into(),
                arguments: serde_json::Value::Null,
            })
            .await;
        match resp {
            Response::Error { message } => assert!(message.contains("not found")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn call_tool_rejects_when_capability_missing() {
        let s = server_with(vec![], "");
        let resp = s
            .op_respond(Request::CallTool {
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "hi" }),
            })
            .await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("requires capability"));
                assert!(message.contains("tool.call.echo"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn call_tool_audits_capability_check() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::from_tools(vec![Arc::new(
                covenant_mcp::native::EchoTool,
            )])),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );
        s.op_respond(Request::CallTool {
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "hi" }),
        })
        .await;
        let events = audit.recent(10).await.unwrap();
        let cap = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::CapabilityCheck { .. }))
            .expect("capability check audit event present");
        match &cap.kind {
            AuditKind::CapabilityCheck {
                agent_id,
                required_actions,
                passed,
                ..
            } => {
                assert_eq!(agent_id, "tool:echo");
                assert_eq!(required_actions, &vec!["tool.call.echo".to_string()]);
                assert!(!passed);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// Builds a task whose `sender` matches `s.identity.agent_id()` so it
    /// passes the Sprint 49 spoof check. Tests that need a mismatched
    /// sender construct the task inline.
    fn dummy_a2a_task_for(s: &Server) -> covenant_a2a::A2ATask {
        // Sprint 59 recv gate: loopback recipient (operator's pubkey)
        // skips the gate (D2). The display stays "research@local" so
        // pre-Sprint-59 assertions keying on the recipient display
        // (e.g. `a2a.send.research@local`) still hold.
        covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: s.identity.agent_id(),
            recipient: covenant_types::AgentId::new("research@local", s.identity.pubkey_bytes()),
            intent_text: "find recent papers".into(),
            parent: None,
            deadline_ms: None,
        }
    }

    #[tokio::test]
    async fn a2a_task_round_trips_through_server() {
        let s = server_with(vec![], "");
        // Sprint 50: `try_recv` filters by recipient, so the round-trip
        // test queues a task addressed *to* the operator peer and drains
        // it from the same peer's perspective.
        let peer = s.identity.agent_id();
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: peer.clone(),
            recipient: peer.clone(),
            intent_text: "loopback".into(),
            parent: None,
            deadline_ms: None,
        };
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", task.recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;

        let queued = s
            .op_respond(Request::SendA2ATask { task: task.clone() })
            .await;
        match queued {
            Response::A2ATaskQueued { task_id } => assert_eq!(task_id, task.id),
            other => panic!("unexpected: {other:?}"),
        }
        let recv = s.op_respond(Request::TryRecvA2ATask).await;
        match recv {
            Response::A2ATaskOpt { task: Some(t) } => assert_eq!(t.id, task.id),
            other => panic!("unexpected: {other:?}"),
        }
        let again = s.op_respond(Request::TryRecvA2ATask).await;
        match again {
            Response::A2ATaskOpt { task: None } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_send_rejects_when_capability_missing() {
        let s = server_with(vec![], "");
        let task = dummy_a2a_task_for(&s);
        let resp = s.op_respond(Request::SendA2ATask { task }).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("requires capability"));
                assert!(message.contains("a2a.send.research@local"));
            }
            other => panic!("unexpected: {other:?}"),
        }
        let drained = s.op_respond(Request::TryRecvA2ATask).await;
        assert!(
            matches!(drained, Response::A2ATaskOpt { task: None }),
            "rejected task must not enqueue: {drained:?}"
        );
    }

    #[tokio::test]
    async fn a2a_send_audits_capability_check() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );
        s.op_respond(Request::SendA2ATask {
            task: dummy_a2a_task_for(&s),
        })
        .await;

        let events = audit.recent(10).await.unwrap();
        let cap = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::CapabilityCheck { .. }))
            .expect("capability check audit event present");
        match &cap.kind {
            AuditKind::CapabilityCheck {
                agent_id,
                required_actions,
                passed,
                ..
            } => {
                assert_eq!(agent_id, "a2a-send:research@local");
                assert_eq!(
                    required_actions,
                    &vec!["a2a.send.research@local".to_string()]
                );
                assert!(!passed);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_result_round_trips_through_server() {
        let s = server_with(vec![], "");
        let task = dummy_a2a_task_for(&s);
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", task.recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::SendA2ATask { task: task.clone() })
            .await;

        s.op_respond(Request::GrantCapability {
            action: format!("a2a.respond.{}", task.sender.display),
            scope: None,
            expires_at: None,
        })
        .await;

        let result =
            covenant_a2a::A2ATaskResult::ok(task.id, vec![covenant_mcp::Content::text("done")]);
        let posted = s
            .op_respond(Request::PostA2AResult {
                result: result.clone(),
            })
            .await;
        match posted {
            Response::A2AResultPosted { task_id: id } => assert_eq!(id, task.id),
            other => panic!("unexpected: {other:?}"),
        }
        let recv = s.op_respond(Request::TryRecvA2AResult).await;
        match recv {
            Response::A2AResultOpt {
                result: Some(got), ..
            } => {
                assert_eq!(got.task_id, task.id);
                assert_eq!(got.status, covenant_a2a::A2ATaskStatus::Ok);
            }
            other => panic!("unexpected: {other:?}"),
        }
        let again = s.op_respond(Request::TryRecvA2AResult).await;
        match again {
            Response::A2AResultOpt { result: None } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_recent_returns_queued_tasks_without_consuming() {
        let s = server_with(vec![], "");
        // Sprint 50: per-peer recv requires the queued tasks to be
        // addressed to the peer doing the drain. Loopback fits the v0
        // single-peer test surface.
        let peer = s.identity.agent_id();
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: peer.clone(),
            recipient: peer.clone(),
            intent_text: "loopback".into(),
            parent: None,
            deadline_ms: None,
        };
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", task.recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::SendA2ATask { task: task.clone() })
            .await;
        s.op_respond(Request::SendA2ATask {
            task: covenant_a2a::A2ATask {
                id: Uuid::new_v4(),
                ..task.clone()
            },
        })
        .await;

        let recent = s.op_respond(Request::RecentA2ATasks { limit: 10 }).await;
        match recent {
            Response::A2ATasks { tasks } => {
                assert_eq!(tasks.len(), 2);
                assert_eq!(tasks[0].id, task.id, "oldest first");
            }
            other => panic!("unexpected: {other:?}"),
        }

        // recent must not consume — try_recv still finds tasks.
        let drained = s.op_respond(Request::TryRecvA2ATask).await;
        assert!(matches!(drained, Response::A2ATaskOpt { task: Some(_) }));
    }

    #[tokio::test]
    async fn a2a_respond_rejects_when_task_id_unknown() {
        let s = server_with(vec![], "");
        // No task has been sent; any task_id is unknown to the mailbox.
        let unknown_id = Uuid::new_v4();
        let result =
            covenant_a2a::A2ATaskResult::ok(unknown_id, vec![covenant_mcp::Content::text("done")]);
        let resp = s.op_respond(Request::PostA2AResult { result }).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("never dispatched"));
            }
            other => panic!("unexpected: {other:?}"),
        }
        let drained = s.op_respond(Request::TryRecvA2AResult).await;
        assert!(
            matches!(drained, Response::A2AResultOpt { result: None }),
            "rejected result must not enqueue: {drained:?}"
        );

        // Defender-visible: the rejection lands in the audit log even
        // though no capability check happened upstream of the lookup.
        match s.op_respond(Request::RecentAudit { limit: 10 }).await {
            Response::AuditEvents { events } => {
                let logged = events.iter().find_map(|e| match &e.kind {
                    AuditKind::A2AResultRejected { task_id, reason } => {
                        Some((*task_id, reason.clone()))
                    }
                    _ => None,
                });
                let (task_id, reason) = logged.expect("expected an A2AResultRejected audit event");
                assert_eq!(task_id, unknown_id);
                assert_eq!(reason, "unknown_task");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_respond_rejects_when_sender_capability_missing() {
        let s = server_with(vec![], "");
        let task = dummy_a2a_task_for(&s);
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", task.recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::SendA2ATask { task: task.clone() })
            .await;

        // Task is now known; respond cap is still missing.
        let result =
            covenant_a2a::A2ATaskResult::ok(task.id, vec![covenant_mcp::Content::text("done")]);
        let resp = s.op_respond(Request::PostA2AResult { result }).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("requires capability"));
                assert!(message.contains(&format!("a2a.respond.{}", task.sender.display)));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_send_rejects_when_sender_does_not_match_peer() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );
        // Authenticated peer is `user@local`, but the task claims to be from
        // `evil@local`. Even with the send cap granted, the spoof check fires
        // first and the task never reaches the mailbox.
        s.op_respond(Request::GrantCapability {
            action: "a2a.send.research@local".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: covenant_types::AgentId::new("evil@local", [9u8; 32]),
            recipient: covenant_types::AgentId::new("research@local", [0u8; 32]),
            intent_text: "stolen identity".into(),
            parent: None,
            deadline_ms: None,
        };
        let resp = s.op_respond(Request::SendA2ATask { task }).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("does not match"));
                assert!(message.contains("evil@local"));
                assert!(message.contains("user@local"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
        let drained = s.op_respond(Request::TryRecvA2ATask).await;
        assert!(
            matches!(drained, Response::A2ATaskOpt { task: None }),
            "spoofed task must not enqueue: {drained:?}"
        );
        let events = audit.recent(20).await.unwrap();
        let mismatch = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::A2ASenderMismatch { .. }))
            .expect("expected an A2ASenderMismatch audit event");
        match &mismatch.kind {
            AuditKind::A2ASenderMismatch {
                peer_display,
                claimed_sender_display,
            } => {
                assert_eq!(peer_display, "user@local");
                assert_eq!(claimed_sender_display, "evil@local");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    // Sprint 59 — recipient admission gate tests.

    #[tokio::test]
    async fn recv_gate_skipped_when_peer_equals_recipient_loopback() {
        let s = server_with(vec![], "");
        let peer = s.identity.agent_id();
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: peer.clone(),
            recipient: peer.clone(),
            intent_text: "loopback".into(),
            parent: None,
            deadline_ms: None,
        };
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", task.recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s
            .op_respond(Request::SendA2ATask { task: task.clone() })
            .await;
        match resp {
            Response::A2ATaskQueued { task_id } => assert_eq!(task_id, task.id),
            other => panic!("v0 loopback must skip recv gate, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn recv_gate_rejects_when_recipient_lacks_cap() {
        let s = server_with(vec![], "");
        let peer = s.identity.agent_id();
        let foreign_recipient = AgentId::new("victim@local", [7u8; 32]);
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: peer.clone(),
            recipient: foreign_recipient.clone(),
            intent_text: "spam".into(),
            parent: None,
            deadline_ms: None,
        };
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", foreign_recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s.op_respond(Request::SendA2ATask { task }).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("recipient has not granted"));
                assert!(message.contains(&format!("a2a.recv.{}", peer.display)));
            }
            other => panic!("expected Error, got {other:?}"),
        }
        let drained = s.op_respond(Request::TryRecvA2ATask).await;
        assert!(
            matches!(drained, Response::A2ATaskOpt { task: None }),
            "rejected task must not enqueue: {drained:?}"
        );
    }

    #[tokio::test]
    async fn recv_gate_passes_when_recipient_has_cap() {
        let s = server_with(vec![], "");
        let peer = s.identity.agent_id();
        let foreign_recipient = AgentId::new("victim@local", [7u8; 32]);
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", foreign_recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        // Inject the recv cap directly with subject = recipient.pubkey.
        // v0 has no IPC verb to grant on a foreign subject; tests bypass
        // via the store API to exercise the gate's pass path. Sign with
        // an alien identity — verification keys on subject pubkey, not
        // signature provenance.
        let alien_grantor = LocalIdentity::generate("granter@local");
        let recv_cap = covenant_types::Capability {
            subject: foreign_recipient.clone(),
            action: format!("a2a.recv.{}", peer.display),
            scope: serde_json::json!({}),
            granted_by: alien_grantor.agent_id(),
            expires_at: None,
        };
        let signed = sign_capability(recv_cap, alien_grantor.signing_key());
        s.capabilities.record(signed).await.unwrap();

        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: peer.clone(),
            recipient: foreign_recipient,
            intent_text: "authorised".into(),
            parent: None,
            deadline_ms: None,
        };
        let resp = s
            .op_respond(Request::SendA2ATask { task: task.clone() })
            .await;
        match resp {
            Response::A2ATaskQueued { task_id } => assert_eq!(task_id, task.id),
            other => panic!("recv gate must pass with cap granted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn recv_gate_audits_recipient_rejected_with_attribution() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );
        let peer = s.identity.agent_id();
        let foreign_recipient = AgentId::new("victim@local", [7u8; 32]);
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", foreign_recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: peer.clone(),
            recipient: foreign_recipient.clone(),
            intent_text: "spam".into(),
            parent: None,
            deadline_ms: None,
        };
        s.op_respond(Request::SendA2ATask { task }).await;

        let events = audit.recent(20).await.unwrap();
        let row = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::A2ARecipientRejected { .. }))
            .expect("expected an A2ARecipientRejected audit event");
        assert_eq!(
            row.issuer.pubkey, peer.pubkey,
            "issuer must be the sender peer (record_peer_event invariant)"
        );
        match &row.kind {
            AuditKind::A2ARecipientRejected {
                sender_display,
                recipient_display,
                action,
            } => {
                assert_eq!(sender_display, &peer.display);
                assert_eq!(recipient_display, &foreign_recipient.display);
                assert_eq!(action, &format!("a2a.recv.{}", peer.display));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recv_gate_does_not_short_circuit_send_cap_check() {
        let s = server_with(vec![], "");
        let peer = s.identity.agent_id();
        let foreign_recipient = AgentId::new("victim@local", [7u8; 32]);
        // No `a2a.send.<recipient>` granted. The send-cap check fires
        // first and short-circuits before the recv gate runs.
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: peer.clone(),
            recipient: foreign_recipient.clone(),
            intent_text: "no send cap".into(),
            parent: None,
            deadline_ms: None,
        };
        let resp = s.op_respond(Request::SendA2ATask { task }).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains(&format!("a2a.send.{}", foreign_recipient.display)));
                assert!(
                    !message.contains("recipient has not granted"),
                    "send-cap check must short-circuit before recv gate: {message}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn recv_gate_keys_on_pubkey_not_display() {
        let s = server_with(vec![], "");
        let peer = s.identity.agent_id();
        // Recipient with the operator's *display* but a different
        // pubkey: the gate must still trip because subject keying is on
        // the 32-byte pubkey.
        let spoofed_recipient = AgentId::new(peer.display.clone(), [9u8; 32]);
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", spoofed_recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: peer.clone(),
            recipient: spoofed_recipient,
            intent_text: "display spoof".into(),
            parent: None,
            deadline_ms: None,
        };
        let resp = s.op_respond(Request::SendA2ATask { task }).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("recipient has not granted"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn purge_audit_rejects_without_capability() {
        let s = server_with(vec![], "");
        // No `audit.purge` cap granted — purge attempt is rejected even
        // though the peer is authenticated.
        let resp = s.op_respond(Request::PurgeAudit { before_ms: 1 }).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("audit.purge"));
                assert!(message.contains("requires capability"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn purge_audit_passes_after_grant() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "audit.purge".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s.op_respond(Request::PurgeAudit { before_ms: 0 }).await;
        match resp {
            Response::AuditPurged { .. } => {}
            other => panic!("expected AuditPurged, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn purge_capabilities_rejects_without_capability() {
        let s = server_with(vec![], "");
        let resp = s
            .op_respond(Request::PurgeCapabilities { before_ms: 1 })
            .await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("capabilities.purge"));
                assert!(message.contains("requires capability"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn purge_capabilities_passes_after_grant() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "capabilities.purge".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s
            .op_respond(Request::PurgeCapabilities { before_ms: 0 })
            .await;
        match resp {
            Response::CapabilitiesPurged { .. } => {}
            other => panic!("expected CapabilitiesPurged, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn compact_a2a_rejects_without_capability() {
        let s = server_with(vec![], "");
        let resp = s.op_respond(Request::CompactA2A).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("a2a.compact"));
                assert!(message.contains("requires capability"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn compact_a2a_passes_after_grant() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "a2a.compact".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s.op_respond(Request::CompactA2A).await;
        match resp {
            Response::A2ACompacted { dropped } => {
                // No tasks to compact in a fresh in-memory mailbox.
                assert_eq!(dropped, 0);
            }
            other => panic!("expected A2ACompacted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn purge_peers_rejects_without_capability() {
        let s = server_with(vec![], "");
        let resp = s.op_respond(Request::PurgePeers { before_ms: 1 }).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("peers.purge"));
                assert!(message.contains("requires capability"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn purge_peers_passes_after_grant() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "peers.purge".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s.op_respond(Request::PurgePeers { before_ms: 0 }).await;
        match resp {
            Response::PeersPurged { purged } => {
                assert_eq!(purged, 0, "no revocations to purge in a fresh registry");
            }
            other => panic!("expected PeersPurged, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn revoke_rejects_when_peer_is_not_subject() {
        let s = server_with(vec![], "");
        // Operator (= s.identity) grants themselves a cap. The capability's
        // subject pubkey is the operator's. A different peer asking to
        // revoke that signature must be rejected even though the signature
        // is publicly visible via `/capabilities/recent`.
        let granted = s
            .op_respond(Request::GrantCapability {
                action: "tool.web_search".into(),
                scope: None,
                expires_at: None,
            })
            .await;
        let sig_b58 = match granted {
            Response::CapabilityGranted { signature_b58, .. } => signature_b58,
            other => panic!("unexpected: {other:?}"),
        };

        let stranger = AgentId::new("stranger@local", [7u8; 32]);
        let resp = s
            .respond(
                Request::RevokeCapability {
                    signature_b58: sig_b58.clone(),
                },
                &stranger,
            )
            .await;
        match resp {
            Response::Error { message } => assert!(message.contains("does not match")),
            other => panic!("expected Error, got {other:?}"),
        }

        // Cap is still live: operator can still revoke it.
        let owner = s
            .op_respond(Request::RevokeCapability {
                signature_b58: sig_b58,
            })
            .await;
        match owner {
            Response::CapabilityRevoked { removed, .. } => assert!(removed),
            other => panic!("expected CapabilityRevoked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn auth_failure_records_audit_event() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );
        s.record_auth_failure("ipc", "first frame must be Authenticate")
            .await;
        s.record_auth_failure("http", "missing Authorization header")
            .await;
        let events = audit.recent(10).await.unwrap();
        let mut transports: Vec<&str> = events
            .iter()
            .filter_map(|e| match &e.kind {
                AuditKind::AuthenticationFailed { transport, .. } => Some(transport.as_str()),
                _ => None,
            })
            .collect();
        transports.sort();
        assert_eq!(transports, vec!["http", "ipc"]);
    }

    #[tokio::test]
    async fn recent_audit_returns_events_in_order() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "tool.call.echo".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::CallTool {
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "hi" }),
        })
        .await;
        let resp = s.op_respond(Request::RecentAudit { limit: 10 }).await;
        match resp {
            Response::AuditEvents { events } => {
                assert!(
                    events
                        .iter()
                        .any(|e| matches!(e.kind, AuditKind::CapabilityGranted { .. })),
                    "expected a CapabilityGranted event"
                );
                assert!(
                    events
                        .iter()
                        .any(|e| matches!(e.kind, AuditKind::CapabilityCheck { .. })),
                    "expected a CapabilityCheck event from the tool dispatch"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_audit_scrubs_other_peers_rows() {
        let s = server_with(vec![], "");
        let alien = AgentId::new("alice@local", [9u8; 32]);
        s.audit
            .record(AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: alien.clone(),
                kind: AuditKind::BudgetExhausted {
                    agent_display: "research@agent".into(),
                    intent_id: Uuid::new_v4(),
                    intent_text: "alice's secret intent".into(),
                    requested: 1,
                    tokens_remaining: 0,
                    refill_eta_ms: u64::MAX,
                },
            })
            .await
            .unwrap();
        let mine = s.identity.agent_id();
        s.audit
            .record(AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: mine.clone(),
                kind: AuditKind::IntentDispatched {
                    intent_id: Uuid::new_v4(),
                    intent_text: "operator's own intent".into(),
                    matched_agent: None,
                    result_hash_hex: hash_hex(b""),
                    status: "ok".into(),
                },
            })
            .await
            .unwrap();
        let resp = s.op_respond(Request::RecentAudit { limit: 100 }).await;
        match resp {
            Response::AuditEvents { events } => {
                assert!(
                    events.iter().all(|e| e.issuer.pubkey == mine.pubkey),
                    "every returned row must belong to the requesting peer"
                );
                assert!(
                    !events.iter().any(|e| matches!(
                        &e.kind,
                        AuditKind::BudgetExhausted { intent_text, .. }
                            if intent_text == "alice's secret intent"
                    )),
                    "alien BudgetExhausted row leaked through filter"
                );
                assert!(
                    events.iter().any(|e| matches!(
                        &e.kind,
                        AuditKind::IntentDispatched { intent_text, .. }
                            if intent_text == "operator's own intent"
                    )),
                    "operator's own row should still be visible"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn resume_intent_rejects_other_peers_intent() {
        let s = server_with(vec![], "");
        let alien = AgentId::new("alice@local", [9u8; 32]);
        let alien_intent_id = Uuid::new_v4();
        s.audit
            .record(AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: alien,
                kind: AuditKind::BudgetExhausted {
                    agent_display: "research@agent".into(),
                    intent_id: alien_intent_id,
                    intent_text: "leaked".into(),
                    requested: 1,
                    tokens_remaining: 0,
                    refill_eta_ms: u64::MAX,
                },
            })
            .await
            .unwrap();
        let resp = s
            .op_respond(Request::ResumeIntent {
                intent_id: alien_intent_id,
            })
            .await;
        match resp {
            Response::Error { message } => {
                assert!(
                    message.contains("no BudgetExhausted audit row"),
                    "expected the not-found error, got: {message}"
                );
                assert!(
                    !message.contains("leaked"),
                    "intent_text must not appear in the error"
                );
            }
            other => panic!("expected Response::Error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_audit_v0_operator_sees_own_events() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "tool.call.echo".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::CallTool {
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "hi" }),
        })
        .await;
        let me = s.identity.agent_id();
        let resp = s.op_respond(Request::RecentAudit { limit: 10 }).await;
        match resp {
            Response::AuditEvents { events } => {
                assert!(!events.is_empty(), "operator should see their own rows");
                assert!(
                    events.iter().all(|e| e.issuer.pubkey == me.pubkey),
                    "filter must keep operator's rows"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    // -------- Sprint 58g: per-peer filter on the other RecentX surfaces --------

    #[tokio::test]
    async fn recent_memory_scrubs_other_peers_records() {
        let s = server_with(vec![], "");
        let alien = AgentId::new("alice@local", [9u8; 32]);
        let alien_record = MemoryRecord {
            id: Uuid::new_v4(),
            tier: MemoryTier::Working,
            owner: alien,
            text: "alice's secret memory".into(),
            embedding: Vec::new(),
            metadata: serde_json::json!({}),
            created_at: epoch_ms(),
            parent: None,
        };
        s.memory.put(alien_record).await.unwrap();
        let me = s.identity.agent_id();
        let mine = MemoryRecord {
            id: Uuid::new_v4(),
            tier: MemoryTier::Working,
            owner: me.clone(),
            text: "operator's own memory".into(),
            embedding: Vec::new(),
            metadata: serde_json::json!({}),
            created_at: epoch_ms(),
            parent: None,
        };
        s.memory.put(mine).await.unwrap();
        let resp = s
            .op_respond(Request::RecentMemory {
                tier: None,
                limit: 100,
            })
            .await;
        match resp {
            Response::Memories { records } => {
                assert!(
                    records.iter().all(|r| r.owner.pubkey == me.pubkey),
                    "every returned row must be owned by the requesting peer"
                );
                assert!(
                    !records.iter().any(|r| r.text == "alice's secret memory"),
                    "alien memory record leaked through filter"
                );
                assert!(
                    records.iter().any(|r| r.text == "operator's own memory"),
                    "operator's own row should still be visible"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_memory_v0_operator_sees_own_records() {
        let s = server_with(
            vec![stub_card("research", vec!["tool.web_search"])],
            "summary",
        );
        s.op_respond(Request::GrantCapability {
            action: "tool.web_search".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::SubmitIntent {
            text: "find recent papers on agent memory".into(),
        })
        .await;
        let me = s.identity.agent_id();
        let resp = s
            .op_respond(Request::RecentMemory {
                tier: None,
                limit: 10,
            })
            .await;
        match resp {
            Response::Memories { records } => {
                assert!(!records.is_empty(), "operator should see their own rows");
                assert!(
                    records.iter().all(|r| r.owner.pubkey == me.pubkey),
                    "filter must keep operator's rows"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_receipts_scrubs_other_peers_receipts() {
        let s = server_with(vec![], "");
        let alien = AgentId::new("alice@local", [9u8; 32]);
        s.settlement
            .record(SettlementReceipt {
                id: Uuid::new_v4(),
                payer: alien,
                resource: ResourceKind::Memory,
                credits_consumed: 7,
                settled_at: epoch_ms(),
                onchain_sig: None,
            })
            .await
            .unwrap();
        let me = s.identity.agent_id();
        s.settlement
            .record(SettlementReceipt {
                id: Uuid::new_v4(),
                payer: me.clone(),
                resource: ResourceKind::Memory,
                credits_consumed: 3,
                settled_at: epoch_ms(),
                onchain_sig: None,
            })
            .await
            .unwrap();
        let resp = s.op_respond(Request::RecentReceipts { limit: 100 }).await;
        match resp {
            Response::Receipts { receipts } => {
                assert!(
                    receipts.iter().all(|r| r.payer.pubkey == me.pubkey),
                    "every returned receipt must have the requesting peer as payer"
                );
                assert!(
                    !receipts.iter().any(|r| r.credits_consumed == 7),
                    "alien receipt amount leaked through filter"
                );
                assert!(
                    receipts.iter().any(|r| r.credits_consumed == 3),
                    "operator's own receipt should still be visible"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_receipts_v0_operator_sees_own() {
        let s = server_with(
            vec![stub_card("research", vec!["tool.web_search"])],
            "summary",
        );
        s.op_respond(Request::GrantCapability {
            action: "tool.web_search".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::SubmitIntent {
            text: "find recent papers on agent memory".into(),
        })
        .await;
        let me = s.identity.agent_id();
        let resp = s.op_respond(Request::RecentReceipts { limit: 10 }).await;
        match resp {
            Response::Receipts { receipts } => {
                assert!(!receipts.is_empty(), "operator should see their own rows");
                assert!(
                    receipts.iter().all(|r| r.payer.pubkey == me.pubkey),
                    "filter must keep operator's receipts"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_capabilities_scrubs_when_neither_subject_nor_granted_by() {
        let s = server_with(vec![], "");
        // Alien grants alien — peer is on neither side. Sign with a separate
        // identity so the wire-shape is realistic; the filter is on pubkeys,
        // not signature validity, so a hand-built capability is enough.
        let alien_grantor = LocalIdentity::generate("alice@local");
        let alien_subject = AgentId::new("bob@local", [8u8; 32]);
        let cap = covenant_types::Capability {
            subject: alien_subject,
            action: "tool.call.echo".into(),
            scope: serde_json::json!({}),
            granted_by: alien_grantor.agent_id(),
            expires_at: None,
        };
        let signed = sign_capability(cap, alien_grantor.signing_key());
        s.capabilities.record(signed).await.unwrap();

        let resp = s
            .op_respond(Request::RecentCapabilities { limit: 100 })
            .await;
        match resp {
            Response::Capabilities { capabilities } => {
                assert!(
                    capabilities.is_empty(),
                    "alien-to-alien capability leaked through filter: {capabilities:?}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_capabilities_visible_when_peer_is_subject() {
        let s = server_with(vec![], "");
        // Alien grants the operator: peer is the subject.
        let alien_grantor = LocalIdentity::generate("alice@local");
        let me = s.identity.agent_id();
        let cap = covenant_types::Capability {
            subject: me.clone(),
            action: "tool.call.echo".into(),
            scope: serde_json::json!({}),
            granted_by: alien_grantor.agent_id(),
            expires_at: None,
        };
        let signed = sign_capability(cap, alien_grantor.signing_key());
        s.capabilities.record(signed).await.unwrap();

        let resp = s
            .op_respond(Request::RecentCapabilities { limit: 100 })
            .await;
        match resp {
            Response::Capabilities { capabilities } => {
                assert_eq!(capabilities.len(), 1);
                assert_eq!(capabilities[0].capability.subject.pubkey, me.pubkey);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_capabilities_visible_when_peer_is_granted_by() {
        let s = server_with(vec![], "");
        // Operator grants an alien: peer is `granted_by`. Sign with the
        // operator's own key so the on-disk shape is the real grant path.
        let alien_subject = AgentId::new("bob@local", [8u8; 32]);
        let me = s.identity.agent_id();
        let cap = covenant_types::Capability {
            subject: alien_subject,
            action: "tool.call.echo".into(),
            scope: serde_json::json!({}),
            granted_by: me.clone(),
            expires_at: None,
        };
        let signed = sign_capability(cap, s.identity.signing_key());
        s.capabilities.record(signed).await.unwrap();

        let resp = s
            .op_respond(Request::RecentCapabilities { limit: 100 })
            .await;
        match resp {
            Response::Capabilities { capabilities } => {
                assert_eq!(capabilities.len(), 1);
                assert_eq!(capabilities[0].capability.granted_by.pubkey, me.pubkey);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_capabilities_v0_operator_sees_own_grants() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "tool.call.echo".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let me = s.identity.agent_id();
        let resp = s
            .op_respond(Request::RecentCapabilities { limit: 10 })
            .await;
        match resp {
            Response::Capabilities { capabilities } => {
                assert_eq!(capabilities.len(), 1);
                let c = &capabilities[0].capability;
                assert_eq!(c.subject.pubkey, me.pubkey);
                assert_eq!(c.granted_by.pubkey, me.pubkey);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_a2a_tasks_scrubs_when_peer_is_neither_side() {
        let s = server_with(vec![], "");
        let alien_a = AgentId::new("alice@local", [9u8; 32]);
        let alien_b = AgentId::new("bob@local", [8u8; 32]);
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: alien_a,
            recipient: alien_b,
            intent_text: "alien-to-alien".into(),
            parent: None,
            deadline_ms: None,
        };
        s.mailbox.send_task(task).await.unwrap();
        let resp = s.op_respond(Request::RecentA2ATasks { limit: 100 }).await;
        match resp {
            Response::A2ATasks { tasks } => {
                assert!(
                    tasks.is_empty(),
                    "task with neither sender nor recipient match leaked: {tasks:?}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_a2a_tasks_visible_when_peer_is_sender() {
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let alien_recipient = AgentId::new("bob@local", [8u8; 32]);
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: me.clone(),
            recipient: alien_recipient,
            intent_text: "outbound".into(),
            parent: None,
            deadline_ms: None,
        };
        s.mailbox.send_task(task.clone()).await.unwrap();
        let resp = s.op_respond(Request::RecentA2ATasks { limit: 100 }).await;
        match resp {
            Response::A2ATasks { tasks } => {
                assert_eq!(tasks.len(), 1);
                assert_eq!(tasks[0].id, task.id);
                assert_eq!(tasks[0].sender.pubkey, me.pubkey);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_a2a_tasks_visible_when_peer_is_recipient() {
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let alien_sender = AgentId::new("alice@local", [9u8; 32]);
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: alien_sender,
            recipient: me.clone(),
            intent_text: "inbound".into(),
            parent: None,
            deadline_ms: None,
        };
        s.mailbox.send_task(task.clone()).await.unwrap();
        let resp = s.op_respond(Request::RecentA2ATasks { limit: 100 }).await;
        match resp {
            Response::A2ATasks { tasks } => {
                assert_eq!(tasks.len(), 1);
                assert_eq!(tasks[0].id, task.id);
                assert_eq!(tasks[0].recipient.pubkey, me.pubkey);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_a2a_results_scrubs_when_lookup_returns_other_peer() {
        let s = server_with(vec![], "");
        // Alien-sent task; result posted against it. Operator must not see
        // the result because `lookup_task_sender` returns the alien.
        let alien = AgentId::new("alice@local", [9u8; 32]);
        let alien_recipient = AgentId::new("bob@local", [8u8; 32]);
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: alien,
            recipient: alien_recipient,
            intent_text: "alien-task".into(),
            parent: None,
            deadline_ms: None,
        };
        s.mailbox.send_task(task.clone()).await.unwrap();
        let result = covenant_a2a::A2ATaskResult::ok(
            task.id,
            vec![covenant_mcp::Content::text("alien-result")],
        );
        s.mailbox.send_result(result).await.unwrap();

        let resp = s.op_respond(Request::RecentA2AResults { limit: 100 }).await;
        match resp {
            Response::A2AResults { results } => {
                assert!(
                    results.is_empty(),
                    "result for alien-sent task leaked: {results:?}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_a2a_results_visible_when_lookup_returns_peer() {
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let alien_recipient = AgentId::new("bob@local", [8u8; 32]);
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: me.clone(),
            recipient: alien_recipient,
            intent_text: "operator-task".into(),
            parent: None,
            deadline_ms: None,
        };
        s.mailbox.send_task(task.clone()).await.unwrap();
        let result = covenant_a2a::A2ATaskResult::ok(
            task.id,
            vec![covenant_mcp::Content::text("operator-result")],
        );
        s.mailbox.send_result(result.clone()).await.unwrap();

        let resp = s.op_respond(Request::RecentA2AResults { limit: 100 }).await;
        match resp {
            Response::A2AResults { results } => {
                assert_eq!(results.len(), 1);
                assert_eq!(results[0].task_id, task.id);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn ignore_check_returns_matched_pattern() {
        let ignore = IgnoreSet::parse("**/*.pem\n");
        let s = server_with_ignore(vec![], "echo", ignore);
        let resp = s
            .op_respond(Request::IgnoreCheck {
                text: "load /etc/ssl/server.pem please".into(),
            })
            .await;
        match resp {
            Response::IgnoreReport {
                ignored,
                matched_pattern,
                rules_loaded,
            } => {
                assert!(ignored);
                assert_eq!(matched_pattern.as_deref(), Some("**/*.pem"));
                assert_eq!(rules_loaded, 1);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    fn stub_card_with_budget(id: &str, capabilities: Vec<&str>, credits: u64) -> AgentCard {
        let toml = format!(
            r#"
[agent]
id = "{id}"
name = "{id}"
version = "0.0.1"
runtime = "rust-bin"
entry = "./fake"

[capabilities]
required = {caps:?}

[settlement]
budget_credits_per_hour = {credits}
"#,
            caps = capabilities
        );
        let m = Manifest::parse(&toml).unwrap();
        AgentCard::from_manifest_and_dir(m, PathBuf::from("/tmp/nope"))
    }

    /// Register-then-dispatch path: capacity = 10, one debit lands, token
    /// count drops to 9, dispatch returns the runner's mocked output.
    #[tokio::test]
    async fn dispatch_passes_when_budget_healthy() {
        let s = server_with(
            vec![stub_card_with_budget(
                "research",
                vec!["tool.web_search"],
                10,
            )],
            "mocked summary",
        );
        s.register_agent_budgets().await.unwrap();
        s.op_respond(Request::GrantCapability {
            action: "tool.web_search".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s
            .op_respond(Request::SubmitIntent {
                text: "find recent papers on agent memory".into(),
            })
            .await;
        match resp {
            Response::IntentResult { text, .. } => assert_eq!(text, "mocked summary"),
            other => panic!("unexpected: {other:?}"),
        }
        let synth = agent_id_for_card(&stub_card_with_budget(
            "research",
            vec!["tool.web_search"],
            10,
        ));
        assert_eq!(s.budget.tokens_remaining(&synth).await.unwrap(), 9);
    }

    /// Capacity = 1, two dispatches: the second is rejected with
    /// `Response::Error`, leaves a `BudgetExhausted` row in the audit log,
    /// and writes no memory record / receipt for the rejected attempt.
    #[tokio::test]
    async fn dispatch_rejects_and_audits_when_budget_exhausted() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let memory = Arc::new(InMemoryStore::new());
        let settlement = Arc::new(InMemorySettlement::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![stub_card_with_budget(
                "research",
                vec!["tool.web_search"],
                1,
            )])),
            Arc::new(MockRunner::new("mocked summary")),
            memory.clone(),
            settlement.clone(),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );
        s.register_agent_budgets().await.unwrap();
        s.op_respond(Request::GrantCapability {
            action: "tool.web_search".into(),
            scope: None,
            expires_at: None,
        })
        .await;

        let first = s
            .op_respond(Request::SubmitIntent {
                text: "find recent papers".into(),
            })
            .await;
        assert!(matches!(first, Response::IntentResult { .. }));
        let memory_after_first = memory.recent(None, 10).await.unwrap().len();
        let receipts_after_first = settlement.recent(10).await.unwrap().len();

        let second = s
            .op_respond(Request::SubmitIntent {
                text: "find more recent papers".into(),
            })
            .await;
        match second {
            Response::Error { message } => {
                assert!(
                    message.contains("budget exhausted"),
                    "expected budget exhaustion message, got {message:?}"
                );
                assert!(message.contains("research"));
            }
            other => panic!("expected Error, got {other:?}"),
        }

        // Rejected dispatch must not have advanced memory or receipts.
        assert_eq!(
            memory.recent(None, 10).await.unwrap().len(),
            memory_after_first
        );
        assert_eq!(
            settlement.recent(10).await.unwrap().len(),
            receipts_after_first
        );

        let events = audit.recent(50).await.unwrap();
        let exhausted = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::BudgetExhausted { .. }))
            .expect("expected a BudgetExhausted audit event");
        // Pin the issuer attribution: the audit row's issuer is the
        // rejected caller (peer), not the synthesised agent. A future
        // refactor that flips it would silently re-key audit feeds.
        assert_eq!(exhausted.issuer.display, "user@local");
        match &exhausted.kind {
            AuditKind::BudgetExhausted {
                agent_display,
                intent_text,
                requested,
                tokens_remaining,
                ..
            } => {
                assert_eq!(agent_display, "research@agent");
                assert_eq!(*requested, 1);
                assert_eq!(*tokens_remaining, 0);
                // Sprint 58c: audit row carries the rejected text so
                // `intents resume <id>` can re-dispatch from this row alone.
                assert_eq!(intent_text, "find more recent papers");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// Phase-0 manifests have `budget_credits_per_hour = 0`. The daemon
    /// must keep dispatching them — register_agent_budgets seeds capacity
    /// 0, the bucket has no tokens, and try_debit returns Exhausted; the
    /// dispatch path treats credit-0 agents as "no enforcement requested"
    /// and skips the debit. (Equivalent test: card with budget = 0 plus
    /// no register_agent_budgets call, exercising the NoCapacity warn-
    /// and-pass branch.)
    /// Sprint 58c M2 closure — when the manifest opts in to budget but
    /// `register_agent_budgets` was never called, dispatch falls into
    /// the NoCapacity arm. v0 still passes the dispatch through but
    /// records a `BudgetUnseeded` audit row so the bypass is visible.
    #[tokio::test]
    async fn dispatch_audits_unseeded_when_manifest_opts_in_but_bucket_missing() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![stub_card_with_budget(
                "research",
                vec!["tool.web_search"],
                10,
            )])),
            Arc::new(MockRunner::new("mocked summary")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );
        // Skip register_agent_budgets — the bucket is absent.
        s.op_respond(Request::GrantCapability {
            action: "tool.web_search".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s
            .op_respond(Request::SubmitIntent {
                text: "find recent papers".into(),
            })
            .await;
        // Dispatch passes (v0 fail-open).
        assert!(matches!(resp, Response::IntentResult { .. }));
        let events = audit.recent(50).await.unwrap();
        let unseeded = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::BudgetUnseeded { .. }))
            .expect("expected a BudgetUnseeded audit event");
        match &unseeded.kind {
            AuditKind::BudgetUnseeded {
                agent_display,
                requested,
                ..
            } => {
                assert_eq!(agent_display, "research@agent");
                assert_eq!(*requested, 1);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// Sprint 58c L3 closure — wire response rounds tokens_remaining to
    /// a powers-of-5 bucket. Sanity covers the bucket boundaries.
    #[test]
    fn round_tokens_remaining_collapses_to_powers_of_five() {
        assert_eq!(round_tokens_remaining(0), 0);
        assert_eq!(round_tokens_remaining(1), 1);
        assert_eq!(round_tokens_remaining(4), 1);
        assert_eq!(round_tokens_remaining(5), 5);
        assert_eq!(round_tokens_remaining(9), 5);
        assert_eq!(round_tokens_remaining(10), 10);
        assert_eq!(round_tokens_remaining(49), 10);
        assert_eq!(round_tokens_remaining(50), 50);
        assert_eq!(round_tokens_remaining(99), 50);
        assert_eq!(round_tokens_remaining(100), 100);
        assert_eq!(round_tokens_remaining(499), 100);
        assert_eq!(round_tokens_remaining(500), 500);
        assert_eq!(round_tokens_remaining(999), 500);
        assert_eq!(round_tokens_remaining(1000), 1_000);
        assert_eq!(round_tokens_remaining(u64::MAX), 10_000_000_000);
    }

    /// Sprint 58c — resume verb plumbing. A `BudgetExhausted` audit
    /// row recorded for a given `intent_id` is the only state the
    /// resume verb needs: it scans the audit, extracts `intent_text`,
    /// and runs it through `dispatch_intent`. Synthesised audit row
    /// here so the test doesn't have to actually exhaust then refill
    /// (no clock-injection at the InMemoryLedger layer).
    #[tokio::test]
    async fn resume_intent_re_dispatches_from_budget_exhausted_audit_row() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![stub_card_with_budget(
                "research",
                vec!["tool.web_search"],
                10,
            )])),
            Arc::new(MockRunner::new("resumed result")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );
        s.register_agent_budgets().await.unwrap();
        s.op_respond(Request::GrantCapability {
            action: "tool.web_search".into(),
            scope: None,
            expires_at: None,
        })
        .await;

        // Synthesise a BudgetExhausted row as if a previous dispatch had
        // been rejected. The resume verb scans recent audit, finds this
        // row by intent_id, and re-dispatches the captured text. Tag the
        // synthesised row with the daemon's real pubkey so the Sprint 58d
        // per-peer filter passes it through to the find_map.
        let exhausted_intent = Uuid::new_v4();
        audit
            .record(AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: s.identity.agent_id(),
                kind: AuditKind::BudgetExhausted {
                    agent_display: "research@agent".into(),
                    intent_id: exhausted_intent,
                    intent_text: "find recent papers".into(),
                    requested: 1,
                    tokens_remaining: 0,
                    refill_eta_ms: 0,
                },
            })
            .await
            .unwrap();

        let resp = s
            .op_respond(Request::ResumeIntent {
                intent_id: exhausted_intent,
            })
            .await;
        match resp {
            Response::IntentResult { text, .. } => assert_eq!(text, "resumed result"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// Sprint 58c — resume with no matching audit row returns Error,
    /// not a fresh dispatch on an empty intent.
    #[tokio::test]
    async fn resume_intent_returns_error_when_no_audit_row_matches() {
        let s = server_with(
            vec![stub_card_with_budget(
                "research",
                vec!["tool.web_search"],
                10,
            )],
            "should not run",
        );
        let resp = s
            .op_respond(Request::ResumeIntent {
                intent_id: Uuid::new_v4(),
            })
            .await;
        match resp {
            Response::Error { message } => {
                assert!(
                    message.contains("no BudgetExhausted audit row"),
                    "expected resume-not-found message, got {message:?}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_skips_budget_for_zero_credit_agents() {
        let s = server_with(
            vec![stub_card_with_budget(
                "research",
                vec!["tool.web_search"],
                0,
            )],
            "mocked summary",
        );
        // Don't call register_agent_budgets — the agent has no bucket,
        // so try_debit will return NoCapacity, which dispatch treats as
        // a warn-and-pass for v0.
        s.op_respond(Request::GrantCapability {
            action: "tool.web_search".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s
            .op_respond(Request::SubmitIntent {
                text: "find recent papers on agent memory".into(),
            })
            .await;
        match resp {
            Response::IntentResult { text, .. } => assert_eq!(text, "mocked summary"),
            other => panic!("expected IntentResult for zero-budget agent, got {other:?}"),
        }
    }

    // ---- Sprint 58f: write-side audit invariant ----
    //
    // The peer-action sites in `Server::respond` build `AuditEvent`s with
    // `issuer = peer.clone()` and pass them to `record_peer_event`, which
    // asserts the issuer matches the authenticated peer. The forward-
    // looking goal is to catch the next regression that builds an event
    // for one peer's action and accidentally signs it as another (or as
    // the daemon). `cargo test` runs with `debug_assertions = on`, so
    // `debug_assert_eq!` panics — `#[should_panic]` is the natural test
    // shape.

    fn forged_event_with_pubkey(pubkey: [u8; 32], display: &str) -> AuditEvent {
        AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: epoch_ms(),
            issuer: AgentId::new(display.to_string(), pubkey),
            kind: AuditKind::CapabilityCheck {
                agent_id: "tool:test".into(),
                required_actions: vec!["tool.call.test".into()],
                missing_actions: vec![],
                passed: true,
            },
        }
    }

    #[tokio::test]
    #[should_panic(expected = "audit invariant")]
    async fn record_peer_event_panics_when_issuer_does_not_match_peer() {
        let s = server_with(vec![], "");
        let honest_peer = s.identity.agent_id();
        let forged = forged_event_with_pubkey([9u8; 32], "evil@local");
        s.record_peer_event(&honest_peer, forged).await;
    }

    #[tokio::test]
    #[should_panic(expected = "audit invariant")]
    async fn record_daemon_event_panics_when_issuer_is_not_self_identity() {
        let s = server_with(vec![], "");
        let forged = forged_event_with_pubkey([9u8; 32], "evil@local");
        s.record_daemon_event(forged).await;
    }

    #[tokio::test]
    async fn record_peer_event_records_when_issuer_matches_peer() {
        let s = server_with(vec![], "");
        let peer = s.identity.agent_id();
        let event = AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: epoch_ms(),
            issuer: peer.clone(),
            kind: AuditKind::CapabilityCheck {
                agent_id: "tool:positive".into(),
                required_actions: vec!["tool.call.positive".into()],
                missing_actions: vec![],
                passed: true,
            },
        };
        let event_id = event.id;
        s.record_peer_event(&peer, event).await;
        let recent = s.audit.recent(16).await.expect("audit.recent");
        assert!(
            recent.iter().any(|e| e.id == event_id),
            "expected event {event_id} to land in audit.recent"
        );
    }

    #[tokio::test]
    async fn record_daemon_event_records_when_issuer_is_self_identity() {
        // Sanity-pin the existing `record_auth_failure` path through the
        // helper. The test exercises the helper directly so the assertion
        // covers any future daemon-internal call site that doesn't go
        // through `record_auth_failure`.
        let s = server_with(vec![], "");
        let event = AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: epoch_ms(),
            issuer: s.identity.agent_id(),
            kind: AuditKind::AuthenticationFailed {
                transport: "test".into(),
                reason: "synthetic".into(),
            },
        };
        let event_id = event.id;
        s.record_daemon_event(event).await;
        let recent = s.audit.recent(16).await.expect("audit.recent");
        assert!(
            recent.iter().any(|e| e.id == event_id),
            "expected daemon event {event_id} to land in audit.recent"
        );
    }

    #[tokio::test]
    async fn recent_debits_returns_empty_when_router_has_no_agents() {
        let s = server_with(vec![], "");
        match s.op_respond(Request::RecentDebits { limit: 10 }).await {
            Response::Debits { debits } => assert!(debits.is_empty()),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_debits_skips_zero_budget_agents() {
        // A `budget_credits_per_hour = 0` manifest never has its bucket
        // seeded by `register_agent_budgets`, so a `recent_debits` query
        // for it would return `NoCapacity`. The fan-out must skip those
        // cards before calling into the ledger.
        let s = server_with(
            vec![stub_card("zero", vec!["tool.web_search"])],
            "mocked summary",
        );
        s.register_agent_budgets().await.unwrap();
        match s.op_respond(Request::RecentDebits { limit: 10 }).await {
            Response::Debits { debits } => assert!(debits.is_empty()),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_debits_returns_debit_after_dispatch() {
        let s = server_with(
            vec![stub_card_with_budget(
                "research",
                vec!["tool.web_search"],
                10,
            )],
            "mocked summary",
        );
        s.register_agent_budgets().await.unwrap();
        s.op_respond(Request::GrantCapability {
            action: "tool.web_search".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s
            .op_respond(Request::SubmitIntent {
                text: "find recent papers on agent memory".into(),
            })
            .await;
        assert!(matches!(resp, Response::IntentResult { .. }));
        match s.op_respond(Request::RecentDebits { limit: 10 }).await {
            Response::Debits { debits } => {
                assert_eq!(debits.len(), 1);
                assert_eq!(debits[0].agent.display, "research@agent");
                assert_eq!(debits[0].credits, 1);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_debits_sorts_newest_first_across_agents() {
        // Two agents, two debits each. The flat list must be newest-first
        // by `at_ms` regardless of which agent the debits came from.
        let s = server_with(
            vec![
                stub_card_with_budget("alpha", vec![], 100),
                stub_card_with_budget("beta", vec![], 100),
            ],
            "ignored",
        );
        s.register_agent_budgets().await.unwrap();
        let alpha = agent_id_for_card(&stub_card_with_budget("alpha", vec![], 100));
        let beta = agent_id_for_card(&stub_card_with_budget("beta", vec![], 100));
        // Hand-debit so the test controls timing without depending on
        // dispatch wall-clock granularity.
        s.budget.try_debit(&alpha, 1, Uuid::new_v4()).await.unwrap();
        // Wait one ms so the second debit is strictly later.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        s.budget.try_debit(&beta, 1, Uuid::new_v4()).await.unwrap();
        match s.op_respond(Request::RecentDebits { limit: 10 }).await {
            Response::Debits { debits } => {
                assert_eq!(debits.len(), 2);
                assert!(
                    debits[0].at_ms >= debits[1].at_ms,
                    "expected newest-first; got {} then {}",
                    debits[0].at_ms,
                    debits[1].at_ms
                );
                assert_eq!(debits[0].agent.display, "beta@agent");
                assert_eq!(debits[1].agent.display, "alpha@agent");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// Sprint 60. Constructs a Server with a tempdir-bound home and a
    /// pre-seeded operator token (b58 written to `<home>/peers/operator.token`
    /// at mode 0600 + registered to the daemon identity in the peer
    /// registry). Returns the server, the tempdir handle (drop = teardown),
    /// the old token, and the operator's `AgentId` for assertions.
    async fn server_with_operator_token() -> (Server, tempfile::TempDir, PeerToken, AgentId) {
        let dir = tempfile::tempdir().expect("tempdir");
        let identity = Arc::new(LocalIdentity::generate("user@local"));
        let peers = Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new());

        let old_token = PeerToken::generate();
        let operator = identity.agent_id();
        peers
            .register(PeerEntry {
                token: old_token,
                agent_id: operator.clone(),
                registered_at: epoch_ms(),
            })
            .await
            .expect("register old token");

        let token_path = dir.path().join("peers").join("operator.token");
        write_operator_token_0600(&token_path, &old_token.to_b58()).expect("seed operator.token");

        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("ignored")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            Arc::new(covenant_audit::InMemoryAuditLog::new()),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            identity,
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            peers,
            Arc::new(covenant_budget::InMemoryLedger::new()),
        )
        .with_home(dir.path().to_path_buf());
        (s, dir, old_token, operator)
    }

    /// Sprint 60 happy path: rotation under the operator identity returns
    /// the new token, the registry resolves it to the operator, the old
    /// token no longer resolves, and the on-disk file holds the new b58.
    #[tokio::test]
    async fn rotate_token_succeeds_under_operator_identity() {
        let (s, dir, old_token, operator) = server_with_operator_token().await;

        let new_b58 = match s.op_respond(Request::RotateOperatorToken).await {
            Response::OperatorTokenRotated { token_b58 } => token_b58,
            other => panic!("unexpected: {other:?}"),
        };
        let new_token = PeerToken::from_b58(&new_b58).expect("decode new b58");

        // Registry: new resolves to operator; old returns None.
        assert_eq!(
            s.peers.resolve(&new_token).await.unwrap(),
            Some(operator.clone()),
            "new token must resolve to the operator identity"
        );
        assert_eq!(
            s.peers.resolve(&old_token).await.unwrap(),
            None,
            "old token must be revoked after rotation"
        );

        // Disk: file holds the new b58, mode 0600.
        let token_path = dir.path().join("peers").join("operator.token");
        let on_disk = std::fs::read_to_string(&token_path).expect("read operator.token");
        assert_eq!(on_disk.trim(), new_b58);
        require_operator_token_mode_0600(&token_path).expect("0600 enforced post-rotate");
    }

    /// Sprint 60 — Plan-gate C3 enforcement. A non-operator peer (whose
    /// pubkey doesn't match `self.identity.pubkey`) must be rejected
    /// regardless of authentication state. The C2 "any authenticated peer
    /// can rotate" alternative was rejected for exactly this reason — in
    /// Phase-1 multi-peer a guest peer would inherit operator-rotation
    /// capability via authentication alone.
    #[tokio::test]
    async fn rotate_token_rejects_when_peer_is_not_operator_identity() {
        let (s, _dir, old_token, _operator) = server_with_operator_token().await;
        // A foreign peer authenticated by a different ed25519 keypair —
        // valid registration on the wire, but pubkey ≠ identity.pubkey.
        let foreign = AgentId::new("guest@local", [9u8; 32]);
        let resp = s.respond(Request::RotateOperatorToken, &foreign).await;
        match resp {
            Response::Error { message } => {
                assert!(
                    message.contains("operator identity"),
                    "rejection message must name the gate; got {message:?}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
        // Old token must still resolve — the rotation didn't run.
        assert!(
            s.peers.resolve(&old_token).await.unwrap().is_some(),
            "rejected rotation must leave the old token alive"
        );
    }

    /// Sprint 60 — verifies the audit row layout: issuer is the operator
    /// peer (Sprint 58f invariant), kind is `OperatorTokenRotated`, and
    /// the embedded prefixes are the 6-char base58 prefixes of the
    /// before/after tokens (no full token bytes in the audit log).
    #[tokio::test]
    async fn rotate_token_records_audit_event_with_token_prefixes() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let identity = Arc::new(LocalIdentity::generate("user@local"));
        let peers = Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new());
        let old_token = PeerToken::generate();
        let operator = identity.agent_id();
        peers
            .register(PeerEntry {
                token: old_token,
                agent_id: operator.clone(),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        write_operator_token_0600(
            &dir.path().join("peers").join("operator.token"),
            &old_token.to_b58(),
        )
        .unwrap();

        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("ignored")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            identity,
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            peers,
            Arc::new(covenant_budget::InMemoryLedger::new()),
        )
        .with_home(dir.path().to_path_buf());

        let new_b58 = match s.op_respond(Request::RotateOperatorToken).await {
            Response::OperatorTokenRotated { token_b58 } => token_b58,
            other => panic!("unexpected: {other:?}"),
        };

        let events = audit.recent(50).await.unwrap();
        let row = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::OperatorTokenRotated { .. }))
            .expect("OperatorTokenRotated row");
        assert_eq!(row.issuer.pubkey, operator.pubkey, "Sprint 58f invariant");
        match &row.kind {
            AuditKind::OperatorTokenRotated {
                peer_display,
                old_token_prefix,
                new_token_prefix,
            } => {
                assert_eq!(peer_display, &operator.display);
                let expected_old: String = old_token.to_b58().chars().take(6).collect();
                let expected_new: String = new_b58.chars().take(6).collect();
                assert_eq!(old_token_prefix, &expected_old);
                assert_eq!(new_token_prefix, &expected_new);
                assert_eq!(old_token_prefix.len(), 6);
                assert_eq!(new_token_prefix.len(), 6);
                assert_ne!(
                    old_token_prefix, new_token_prefix,
                    "the rotation must produce a fresh token"
                );
            }
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    /// Sprint 60 — guard against the `with_home` builder being skipped.
    /// Without a configured home, the rotation can't read or write the
    /// on-disk token, so the verb returns `Error` with a message naming
    /// the missing home. Tests that don't construct a tempdir-bound
    /// server (most of them) shouldn't accidentally rotate either.
    #[tokio::test]
    async fn rotate_token_errors_when_server_has_no_home() {
        let s = server_with(vec![], "ignored");
        match s.op_respond(Request::RotateOperatorToken).await {
            Response::Error { message } => {
                assert!(
                    message.contains("home"),
                    "message must mention the missing home; got {message:?}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// Sprint 60 — the rotation must short-circuit on the C3 gate before
    /// touching the registry. A foreign peer with no on-disk token
    /// available should still be rejected on the identity check, not on
    /// a downstream "read operator token" io-error. Tests that the gate
    /// orderings match the docstring's enumerated steps.
    #[tokio::test]
    async fn rotate_token_identity_gate_runs_before_disk_read() {
        // Server with a configured home but NO on-disk token — if the
        // identity gate ran second, the response would be a `read operator
        // token` io-error, not the identity-check rejection.
        let dir = tempfile::tempdir().expect("tempdir");
        let s = server_with(vec![], "ignored").with_home(dir.path().to_path_buf());
        let foreign = AgentId::new("guest@local", [9u8; 32]);
        match s.respond(Request::RotateOperatorToken, &foreign).await {
            Response::Error { message } => {
                assert!(
                    message.contains("operator identity"),
                    "must rejected on identity gate; got {message:?}"
                );
                assert!(
                    !message.contains("read operator token"),
                    "must not have reached the disk read; got {message:?}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }
}
