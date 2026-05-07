//! Covenant daemon library — Phase 0/1/2 listener wired to router + runner +
//! memory + settlement + audit + capabilities. Per-dispatch we write a
//! working-tier memory record, a settlement receipt, an audit event, AND a
//! capability check (audit-only — Sprint 12 doesn't reject, Sprint 13 will).

#![deny(unsafe_code)]

pub mod http;

use anyhow::{Context, Result};
use covenant_a2a::Mailbox;
use covenant_audit::{hash_hex, AuditEvent, AuditKind, AuditLog};
use covenant_identity::LocalIdentity;
use covenant_ipc::{read_frame, write_frame, IpcError, Request, Response};
use covenant_llm::Embedder;
use covenant_mcp::ToolRegistry;
use covenant_memory::{IgnoreSet, MemoryStore};
use covenant_permissions::{sign as sign_capability, verify_with_clock, CapabilityStore};
use covenant_router::Router;
use covenant_runtime::Runner;
use covenant_settlement::{memory_write_credits, Settlement};
use covenant_types::{
    Capability, Intent, MemoryRecord, MemoryTier, Priority, ResourceKind, SettlementReceipt,
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
        }
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
        loop {
            let req: Request = match read_frame(&mut stream).await {
                Ok(r) => r,
                Err(IpcError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
            };
            let resp = self.respond(req).await;
            write_frame(&mut stream, &resp).await?;
        }
    }

    pub async fn respond(&self, req: Request) -> Response {
        match req {
            Request::Ping => Response::Pong,
            Request::SubmitIntent { text } => self.dispatch_intent(text).await,
            Request::RecentMemory { tier, limit } => self.recent_memory(tier, limit).await,
            Request::RecentReceipts { limit } => self.recent_receipts(limit).await,
            Request::RecentCapabilities { limit } => self.recent_capabilities(limit).await,
            Request::GrantCapability {
                action,
                scope,
                expires_at,
            } => self.grant_capability(action, scope, expires_at).await,
            Request::RevokeCapability { signature_b58 } => {
                self.revoke_capability(signature_b58).await
            }
            Request::SearchMemory { query, tier, limit } => {
                self.search_memory(query, tier, limit).await
            }
            Request::PurgeMemory { tier, before_ms } => self.purge_memory(tier, before_ms).await,
            Request::Verify { window } => self.verify_recent(window).await,
            Request::IgnoreCheck { text } => self.check_ignore(text),
            Request::ListTools => self.list_tools(),
            Request::CallTool { name, arguments } => self.call_tool(name, arguments).await,
            Request::RecentAudit { limit } => self.recent_audit(limit).await,
            Request::SendA2ATask { task } => self.send_a2a_task(task).await,
            Request::TryRecvA2ATask => self.try_recv_a2a_task().await,
            Request::PostA2AResult { result } => self.post_a2a_result(result).await,
            Request::TryRecvA2AResult => self.try_recv_a2a_result().await,
        }
    }

    async fn send_a2a_task(&self, task: covenant_a2a::A2ATask) -> Response {
        let task_id = task.id;
        match self.mailbox.send_task(task).await {
            Ok(()) => Response::A2ATaskQueued { task_id },
            Err(e) => Response::Error {
                message: format!("a2a: {e}"),
            },
        }
    }

    async fn try_recv_a2a_task(&self) -> Response {
        match self.mailbox.try_recv_task().await {
            Ok(task) => Response::A2ATaskOpt { task },
            Err(e) => Response::Error {
                message: format!("a2a: {e}"),
            },
        }
    }

    async fn post_a2a_result(&self, result: covenant_a2a::A2ATaskResult) -> Response {
        let task_id = result.task_id;
        match self.mailbox.send_result(result).await {
            Ok(()) => Response::A2AResultPosted { task_id },
            Err(e) => Response::Error {
                message: format!("a2a: {e}"),
            },
        }
    }

    async fn try_recv_a2a_result(&self) -> Response {
        match self.mailbox.try_recv_result().await {
            Ok(result) => Response::A2AResultOpt { result },
            Err(e) => Response::Error {
                message: format!("a2a: {e}"),
            },
        }
    }

    async fn recent_audit(&self, limit: usize) -> Response {
        match self.audit.recent(limit).await {
            Ok(events) => Response::AuditEvents { events },
            Err(e) => Response::Error {
                message: format!("audit: {e}"),
            },
        }
    }

    fn list_tools(&self) -> Response {
        Response::ToolList {
            tools: self.tools.list_specs(),
        }
    }

    async fn call_tool(&self, name: String, arguments: serde_json::Value) -> Response {
        let required = vec![format!("tool.call.{name}")];
        let check = self
            .check_capabilities(format!("tool:{name}"), required)
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

    async fn dispatch_intent(&self, text: String) -> Response {
        let intent_id = Uuid::new_v4();
        let issued_at = epoch_ms();

        let issuer = self.identity.agent_id();

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
            if let Err(e) = self.audit.record(event).await {
                warn!(error = %e, "audit record (intent ignored) failed");
            }
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
                .check_capabilities(card.id.clone(), card.manifest.capabilities.required.clone())
                .await;
            if !check.passed {
                return Response::Error {
                    message: format!(
                        "agent {} is missing capabilities: {:?}. Grant them with `covenant capabilities grant <action>`.",
                        card.id, check.missing
                    ),
                };
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
                id: Uuid::new_v4(),
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
        if let Err(e) = self.audit.record(audit_event).await {
            warn!(error = %e, "audit record failed");
        }

        Response::IntentResult {
            intent_id,
            status: "ok".into(),
            text: text_out,
            sources: sources_out,
            settlement: None,
        }
    }

    /// Capability check: returns required + missing + passed. Logs a
    /// `CapabilityCheck` audit event as a side-effect. Callers use `passed`
    /// to decide whether to reject the request. `scope_id` is the subject
    /// of the check (an agent id or `tool:<name>`); it lands in the audit
    /// row so operators can distinguish.
    async fn check_capabilities(
        &self,
        scope_id: String,
        required: Vec<String>,
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
            .list_for_subject(self.identity.pubkey_bytes())
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
            issuer: self.identity.agent_id(),
            kind: AuditKind::CapabilityCheck {
                agent_id: scope_id,
                required_actions: required.clone(),
                missing_actions: missing.clone(),
                passed,
            },
        };
        if let Err(e) = self.audit.record(event).await {
            warn!(error = %e, "audit record (capability check) failed");
        }
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
    ) -> Response {
        let issuer = self.identity.agent_id();
        let cap = Capability {
            subject: issuer.clone(),
            action: action.clone(),
            scope: scope.unwrap_or_else(|| serde_json::json!({})),
            granted_by: issuer.clone(),
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
            issuer: issuer.clone(),
            kind: AuditKind::CapabilityGranted {
                subject_display: issuer.display.clone(),
                action: action.clone(),
                granted_by_display: issuer.display.clone(),
                signature_b58: signature_b58.clone(),
            },
        };
        let _ = self.audit.record(event).await;

        Response::CapabilityGranted {
            signature_b58,
            subject_display: issuer.display,
            action,
        }
    }

    async fn recent_memory(&self, tier: Option<MemoryTier>, limit: usize) -> Response {
        match self.memory.recent(tier, limit).await {
            Ok(records) => Response::Memories { records },
            Err(e) => Response::Error {
                message: format!("memory: {e}"),
            },
        }
    }

    async fn recent_receipts(&self, limit: usize) -> Response {
        match self.settlement.recent(limit).await {
            Ok(receipts) => Response::Receipts { receipts },
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

    async fn recent_capabilities(&self, limit: usize) -> Response {
        match self.capabilities.recent(limit).await {
            Ok(capabilities) => Response::Capabilities { capabilities },
            Err(e) => Response::Error {
                message: format!("permissions: {e}"),
            },
        }
    }

    async fn revoke_capability(&self, signature_b58: String) -> Response {
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

struct CapabilityCheckOutcome {
    passed: bool,
    #[allow(dead_code)]
    required: Vec<String>,
    missing: Vec<String>,
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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
        )
    }

    #[tokio::test]
    async fn ping_returns_pong() {
        let s = server_with(vec![], "");
        assert_eq!(s.respond(Request::Ping).await, Response::Pong);
    }

    #[tokio::test]
    async fn submit_intent_writes_memory_and_settlement() {
        let s = server_with(
            vec![stub_card("research", vec!["tool.web_search"])],
            "mocked summary",
        );
        // Hard enforcement: grant the required cap up-front.
        s.respond(Request::GrantCapability {
            action: "tool.web_search".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s
            .respond(Request::SubmitIntent {
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
            .respond(Request::SubmitIntent {
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
            .respond(Request::GrantCapability {
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
            .respond(Request::SubmitIntent {
                text: "find papers".into(),
            })
            .await;
        assert!(matches!(r, Response::IntentResult { .. }));

        // Revoke; dispatch now fails.
        let revoked = s
            .respond(Request::RevokeCapability {
                signature_b58: sig_b58,
            })
            .await;
        match revoked {
            Response::CapabilityRevoked { removed, .. } => assert!(removed),
            other => panic!("unexpected: {other:?}"),
        }
        let r2 = s
            .respond(Request::SubmitIntent {
                text: "find papers".into(),
            })
            .await;
        assert!(matches!(r2, Response::Error { .. }));
    }

    #[tokio::test]
    async fn submit_intent_falls_back_to_echo_when_no_match() {
        let s = server_with(vec![stub_card("research", vec!["tool.web_search"])], "");
        let resp = s
            .respond(Request::SubmitIntent {
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
            .respond(Request::GrantCapability {
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
        let recent = s.respond(Request::RecentCapabilities { limit: 10 }).await;
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
        );
        // Dispatch will be rejected, but the capability check event is still recorded.
        s.respond(Request::SubmitIntent {
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
            .respond(Request::SubmitIntent {
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
        let resp = s.respond(Request::ListTools).await;
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
        s.respond(Request::GrantCapability {
            action: "tool.call.echo".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s
            .respond(Request::CallTool {
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
        s.respond(Request::GrantCapability {
            action: "tool.call.missing".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s
            .respond(Request::CallTool {
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
            .respond(Request::CallTool {
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
        );
        s.respond(Request::CallTool {
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

    #[tokio::test]
    async fn a2a_task_round_trips_through_server() {
        let s = server_with(vec![], "");
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: covenant_types::AgentId::new("orch@local", [0u8; 32]),
            recipient: covenant_types::AgentId::new("research@local", [0u8; 32]),
            intent_text: "find recent papers".into(),
            parent: None,
            deadline_ms: None,
        };
        let queued = s.respond(Request::SendA2ATask { task: task.clone() }).await;
        match queued {
            Response::A2ATaskQueued { task_id } => assert_eq!(task_id, task.id),
            other => panic!("unexpected: {other:?}"),
        }
        let recv = s.respond(Request::TryRecvA2ATask).await;
        match recv {
            Response::A2ATaskOpt { task: Some(t) } => assert_eq!(t.id, task.id),
            other => panic!("unexpected: {other:?}"),
        }
        // Empty after drain.
        let again = s.respond(Request::TryRecvA2ATask).await;
        match again {
            Response::A2ATaskOpt { task: None } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_result_round_trips_through_server() {
        let s = server_with(vec![], "");
        let task_id = Uuid::new_v4();
        let result =
            covenant_a2a::A2ATaskResult::ok(task_id, vec![covenant_mcp::Content::text("done")]);
        let posted = s
            .respond(Request::PostA2AResult {
                result: result.clone(),
            })
            .await;
        match posted {
            Response::A2AResultPosted { task_id: id } => assert_eq!(id, task_id),
            other => panic!("unexpected: {other:?}"),
        }
        let recv = s.respond(Request::TryRecvA2AResult).await;
        match recv {
            Response::A2AResultOpt {
                result: Some(got), ..
            } => {
                assert_eq!(got.task_id, task_id);
                assert_eq!(got.status, covenant_a2a::A2ATaskStatus::Ok);
            }
            other => panic!("unexpected: {other:?}"),
        }
        let again = s.respond(Request::TryRecvA2AResult).await;
        match again {
            Response::A2AResultOpt { result: None } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_audit_returns_events_in_order() {
        let s = server_with(vec![], "");
        s.respond(Request::GrantCapability {
            action: "tool.call.echo".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        s.respond(Request::CallTool {
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "hi" }),
        })
        .await;
        let resp = s.respond(Request::RecentAudit { limit: 10 }).await;
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
    async fn ignore_check_returns_matched_pattern() {
        let ignore = IgnoreSet::parse("**/*.pem\n");
        let s = server_with_ignore(vec![], "echo", ignore);
        let resp = s
            .respond(Request::IgnoreCheck {
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
}
