//! Daemon-side accounting for outbound x402 payments.
//!
//! The `covenant-x402` crate runs the 402-then-pay loop but holds no
//! budget, writes no receipts, and records no audit events — by
//! design. This module is where those Covenant concerns attach. A
//! completed paid call produces three linked records, in this order:
//!
//! 1. a [`covenant_budget::BudgetDebit`] against the payer,
//! 2. a [`SettlementReceipt`] (resource [`ResourceKind::Tool`]),
//! 3. an [`AuditKind::ExternalPaymentSettled`] event.
//!
//! All three share the receipt id so the budget log, settlement log,
//! and audit log join cleanly — the same invariant the daemon's
//! per-dispatch path maintains.
//!
//! ## Scope
//!
//! This is the accounting layer only. It signs nothing: the
//! [`covenant_x402::Signer`] is supplied by the caller. The real
//! Solana funding-key signer, its `solana` feature wiring, and a
//! live dispatch endpoint land in a separate, separately-reviewed
//! change — anything that moves real USDC is kept out of this slice.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::{Method, Response};
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, warn};
use uuid::Uuid;

use covenant_audit::{AuditEvent, AuditKind, AuditLog};
use covenant_budget::{BudgetError, BudgetLedger};
use covenant_settlement::Settlement;
use covenant_types::{AgentId, ResourceKind, SettlementReceipt};
use covenant_x402::{Capability, Client, PaymentRequirements, Signer, X402Error};

/// Opt-in switch for the daemon's outbound x402 surface. `enabled`
/// defaults to `false` so a daemon with no operator opt-in never
/// makes paid calls. When `enabled` is true, `signer_binary` must
/// point at a runnable `covenant-x402-signer` and `signer_env`
/// should carry the funding-key path + RPC URL the sidecar reads
/// from its environment.
#[derive(Debug, Clone, Default)]
pub struct X402Config {
    pub enabled: bool,
    pub signer_binary: PathBuf,
    pub signer_env: Vec<(String, String)>,
}

/// A [`Signer`] that delegates to the standalone `covenant-x402-signer`
/// sidecar over a subprocess.
///
/// The daemon never links the Solana dep tree and never holds the
/// funding key: it spawns the sidecar, pipes the chosen
/// [`PaymentRequirements`] as JSON to its stdin, and reads the
/// `x-payment` header from its stdout. The funding key lives only in
/// the sidecar's address space, configured via the sidecar's own env
/// (e.g. `COVENANT_X402_FUNDING_KEYPAIR`).
pub struct SubprocessSigner {
    program: PathBuf,
    args: Vec<String>,
    env: Vec<(String, String)>,
}

impl SubprocessSigner {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Set an env var for the spawned sidecar (e.g. the funding
    /// keypair path or RPC URL). The daemon's own environment is not
    /// inherited beyond what the OS passes through.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
}

#[async_trait::async_trait]
impl Signer for SubprocessSigner {
    async fn build_payment(&self, requirements: &PaymentRequirements) -> Result<String, X402Error> {
        let payload = serde_json::to_vec(requirements)
            .map_err(|e| X402Error::Sign(format!("encode requirement: {e}")))?;

        let mut child = Command::new(&self.program)
            .args(&self.args)
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| X402Error::Sign(format!("spawn signer {:?}: {e}", self.program)))?;

        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| X402Error::Sign("signer stdin unavailable".into()))?;
            stdin
                .write_all(&payload)
                .await
                .map_err(|e| X402Error::Sign(format!("write to signer: {e}")))?;
            // Drop closes stdin so the one-shot sidecar sees EOF.
        }

        let output = child
            .wait_with_output()
            .await
            .map_err(|e| X402Error::Sign(format!("await signer: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(X402Error::Sign(format!(
                "signer exited {}: {}",
                output.status,
                stderr.trim()
            )));
        }

        let header = String::from_utf8(output.stdout)
            .map_err(|e| X402Error::Sign(format!("signer stdout not utf-8: {e}")))?;
        let header = header.trim().to_string();
        if header.is_empty() {
            return Err(X402Error::Sign("signer returned an empty header".into()));
        }
        Ok(header)
    }
}

/// Subsystem handles the accounting needs, borrowed for the duration
/// of one call. `issuer` is the actor recorded as the audit event's
/// issuer (the daemon/operator identity), distinct from the `payer`
/// whose budget is debited.
pub struct SettlementContext<'a> {
    pub settlement: &'a dyn Settlement,
    pub audit: &'a dyn AuditLog,
    pub budget: &'a dyn BudgetLedger,
    pub issuer: &'a AgentId,
}

/// A resolved paid call ready to execute. The caller resolves the
/// endpoint and pricing (e.g. from a [`covenant_x402::Catalog`]) and
/// fills this in; `amount`/`network`/`asset` are recorded on the
/// receipt and audit event.
pub struct PaidCall<'a> {
    pub provider: &'a str,
    pub endpoint: &'a str,
    pub method: Method,
    pub capability: Capability,
    pub body: Option<&'a Value>,
    /// Atomic amount advertised for this call.
    pub amount: String,
    /// CAIP-2 network the payment settles on.
    pub network: String,
    /// Asset (mint/contract) paid in.
    pub asset: String,
    /// USD-pegged credits to debit from the payer's budget.
    pub credits: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum X402DaemonError {
    #[error("x402 outbound surface is disabled")]
    Disabled,
    #[error("payer has no budget capacity; refusing to spend")]
    NoCapacity,
    #[error("payer budget would be exceeded by this call")]
    BudgetExceeded,
    #[error("payment: {0}")]
    Payment(#[from] X402Error),
    #[error("budget: {0}")]
    Budget(BudgetError),
    #[error("settlement: {0}")]
    Settlement(String),
    #[error("audit: {0}")]
    Audit(String),
    /// The paid endpoint's response body exceeded the in-memory read cap
    /// ([`MAX_RESPONSE_BYTES`]). Holds the offending byte count — the declared
    /// `Content-Length` or the count observed before the guard tripped.
    #[error("upstream response body of {0} bytes exceeds the in-memory cap")]
    ResponseTooLarge(u64),
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Records the budget debit, settlement receipt, and audit event for
/// a completed paid call. Returns the shared receipt id.
///
/// Order is debit → receipt → audit. A debit failure aborts before
/// any receipt is written, so the logs never carry a half-recorded
/// call.
pub async fn record_paid_call(
    ctx: &SettlementContext<'_>,
    payer: &AgentId,
    call: &PaidCall<'_>,
) -> Result<Uuid, X402DaemonError> {
    let receipt_id = Uuid::new_v4();
    let now = epoch_ms();

    ctx.budget
        .try_debit(payer, call.credits, receipt_id)
        .await
        .map_err(X402DaemonError::Budget)?;

    ctx.settlement
        .record(SettlementReceipt {
            id: receipt_id,
            payer: payer.clone(),
            resource: ResourceKind::Tool,
            memory_record_id: None,
            credits_consumed: call.credits,
            settled_at: now,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: None,
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
        })
        .await
        .map_err(|e| X402DaemonError::Settlement(e.to_string()))?;

    ctx.audit
        .record(AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: now,
            issuer: ctx.issuer.clone(),
            kind: AuditKind::ExternalPaymentSettled {
                provider: call.provider.to_string(),
                endpoint: call.endpoint.to_string(),
                network: call.network.clone(),
                asset: call.asset.clone(),
                amount: call.amount.clone(),
                receipt_id,
            },
        })
        .await
        .map_err(|e| X402DaemonError::Audit(e.to_string()))?;

    debug!(
        provider = call.provider,
        endpoint = call.endpoint,
        credits = call.credits,
        %receipt_id,
        "recorded x402 paid call"
    );
    Ok(receipt_id)
}

/// Outcome of a [`pay_and_record`] call.
///
/// `receipt_id` is `Some` when the call paid and the daemon recorded
/// the accounting; it is `None` only on a non-success upstream
/// response (no spend, no receipt). The caller joins this id against
/// the budget log, settlement log, and audit log.
#[derive(Debug)]
pub struct PaidOutcome {
    pub response: Response,
    pub receipt_id: Option<Uuid>,
}

/// Pre-checks budget, runs the 402-then-pay loop, and records the
/// accounting on a successful paid response.
///
/// The budget is checked read-only *before* paying so the daemon does
/// not spend USDC for an agent that cannot afford the credits. The
/// authoritative debit happens after a successful response. A free
/// (non-402) success is returned without any debit or receipt.
pub async fn pay_and_record(
    ctx: &SettlementContext<'_>,
    config: &X402Config,
    client: &Client,
    signer: &dyn Signer,
    payer: &AgentId,
    call: &PaidCall<'_>,
) -> Result<PaidOutcome, X402DaemonError> {
    if !config.enabled {
        return Err(X402DaemonError::Disabled);
    }

    match ctx.budget.would_exceed(payer, call.credits).await {
        Ok(false) => {}
        Ok(true) => return Err(X402DaemonError::BudgetExceeded),
        Err(BudgetError::NoCapacity(_)) => return Err(X402DaemonError::NoCapacity),
        Err(e) => return Err(X402DaemonError::Budget(e)),
    }

    let response = client
        .request_paid(
            call.method.clone(),
            call.endpoint,
            call.body,
            &call.capability,
            signer,
        )
        .await?;

    // A free 2xx (no 402 was issued) needs no settlement. We can't
    // distinguish "paid 2xx" from "free 2xx" from the response alone,
    // so the daemon treats any success here as a settled call when the
    // endpoint is in a paid catalog. Endpoints known to be free should
    // not be routed through this path.
    let receipt_id = if response.status().is_success() {
        match record_paid_call(ctx, payer, call).await {
            Ok(id) => Some(id),
            Err(e) => {
                // The payment already happened; failing to record it is a
                // serious accounting gap, so surface it loudly rather than
                // swallowing.
                warn!(error = %e, endpoint = call.endpoint, "x402 paid call succeeded but accounting failed");
                return Err(e);
            }
        }
    } else {
        None
    };
    Ok(PaidOutcome {
        response,
        receipt_id,
    })
}

/// Maximum paid-call response body the daemon will buffer into memory. The
/// endpoint that answered the paid request is untrusted, so a hostile or
/// compromised one must not be able to exhaust a worker with an unbounded
/// success body — the memory-axis ceiling mirroring the x402 client's bounded
/// 402-challenge read.
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Read a paid-call response body into a string, refusing anything past
/// [`MAX_RESPONSE_BYTES`]. [`PaidOutcome::response`] is handed back unread by
/// the x402 client, so the daemon — not the client — bounds it here.
pub(crate) async fn read_response_body(resp: Response) -> Result<String, X402DaemonError> {
    read_capped(resp, MAX_RESPONSE_BYTES).await
}

/// The `Content-Length` check rejects an oversized declared body before it is
/// streamed; the running accumulation check is the real guard, since the
/// header is optional and endpoint-controlled. The body is forwarded as an
/// opaque string, so the bounded bytes are decoded lossily.
async fn read_capped(mut resp: Response, max: usize) -> Result<String, X402DaemonError> {
    if let Some(len) = resp.content_length() {
        if len > max as u64 {
            return Err(X402DaemonError::ResponseTooLarge(len));
        }
    }
    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(X402Error::from)? {
        if buf.len() + chunk.len() > max {
            return Err(X402DaemonError::ResponseTooLarge(
                (buf.len() + chunk.len()) as u64,
            ));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_audit::InMemoryAuditLog;
    use covenant_budget::InMemoryLedger;
    use covenant_settlement::InMemorySettlement;

    fn agent(tag: u8) -> AgentId {
        AgentId::new("payer@local", [tag; 32])
    }

    fn sample_call<'a>() -> PaidCall<'a> {
        PaidCall {
            provider: "xona",
            endpoint: "https://api.xona-agent.com/img",
            method: Method::POST,
            capability: Capability {
                provider: "xona".into(),
                network: "solana:mainnet".into(),
                asset: "usdc-sol".into(),
                per_call_cap: 100_000,
            },
            body: None,
            amount: "80000".into(),
            network: "solana:mainnet".into(),
            asset: "usdc-sol".into(),
            credits: 8,
        }
    }

    #[tokio::test]
    async fn record_links_debit_receipt_and_audit() {
        let settlement = InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(1);
        budget.set_capacity(&payer, 1000).await.unwrap();

        let ctx = SettlementContext {
            settlement: &settlement,
            audit: &audit,
            budget: &budget,
            issuer: &issuer,
        };
        let call = sample_call();
        let receipt_id = record_paid_call(&ctx, &payer, &call).await.expect("record");

        let receipts = settlement.recent(10).await.unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].id, receipt_id);
        assert_eq!(receipts[0].credits_consumed, 8);
        assert_eq!(receipts[0].resource, ResourceKind::Tool);

        let events = audit.recent(10).await.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0].kind {
            AuditKind::ExternalPaymentSettled {
                receipt_id: rid,
                amount,
                provider,
                ..
            } => {
                assert_eq!(*rid, receipt_id);
                assert_eq!(amount, "80000");
                assert_eq!(provider, "xona");
            }
            other => panic!("unexpected audit kind: {other:?}"),
        }

        // 1000 capacity − 8 debited = 992 remaining.
        assert_eq!(budget.tokens_remaining(&payer).await.unwrap(), 992);
    }

    #[tokio::test]
    async fn record_aborts_when_budget_exhausted() {
        let settlement = InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(2);
        budget.set_capacity(&payer, 5).await.unwrap(); // less than 8 credits

        let ctx = SettlementContext {
            settlement: &settlement,
            audit: &audit,
            budget: &budget,
            issuer: &issuer,
        };
        let err = record_paid_call(&ctx, &payer, &sample_call())
            .await
            .expect_err("should abort");
        assert!(matches!(err, X402DaemonError::Budget(_)));

        // No partial records.
        assert!(settlement.recent(10).await.unwrap().is_empty());
        assert!(audit.recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn pay_and_record_refuses_when_disabled() {
        let settlement = InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(3);
        let ctx = SettlementContext {
            settlement: &settlement,
            audit: &audit,
            budget: &budget,
            issuer: &issuer,
        };
        let err = pay_and_record(
            &ctx,
            &X402Config::default(),
            &Client::new(reqwest::Client::new()),
            &covenant_x402::MockSigner,
            &payer,
            &sample_call(),
        )
        .await
        .expect_err("disabled");
        assert!(matches!(err, X402DaemonError::Disabled));
    }

    fn requirement() -> PaymentRequirements {
        PaymentRequirements {
            network: "solana:mainnet".into(),
            asset: "usdc-sol".into(),
            amount: "80000".into(),
            amount_usdc: 0.08,
            pay_to: "9VaDVp1Wb78G4Wm6VuTiMrpESjrUymXefQTHcJGRSTEA".into(),
            scheme: "exact".into(),
            extra: None,
        }
    }

    #[tokio::test]
    async fn subprocess_signer_returns_stdout_header() {
        let signer = SubprocessSigner::new("sh")
            .arg("-c")
            .arg("cat >/dev/null; printf 'mock-x-payment-header'");
        let header = signer.build_payment(&requirement()).await.expect("header");
        assert_eq!(header, "mock-x-payment-header");
    }

    #[tokio::test]
    async fn subprocess_signer_surfaces_nonzero_exit() {
        let signer = SubprocessSigner::new("sh")
            .arg("-c")
            .arg("cat >/dev/null; echo 'no funding key' >&2; exit 3");
        let err = signer
            .build_payment(&requirement())
            .await
            .expect_err("fail");
        assert!(matches!(err, X402Error::Sign(msg) if msg.contains("no funding key")));
    }

    #[tokio::test]
    async fn subprocess_signer_rejects_empty_header() {
        let signer = SubprocessSigner::new("sh").arg("-c").arg("cat >/dev/null");
        let err = signer
            .build_payment(&requirement())
            .await
            .expect_err("empty");
        assert!(matches!(err, X402Error::Sign(msg) if msg.contains("empty header")));
    }

    #[tokio::test]
    async fn pay_and_record_refuses_when_budget_would_exceed() {
        let settlement = InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(4);
        budget.set_capacity(&payer, 5).await.unwrap(); // 5 < 8 credits

        let ctx = SettlementContext {
            settlement: &settlement,
            audit: &audit,
            budget: &budget,
            issuer: &issuer,
        };
        let config = X402Config {
            enabled: true,
            ..Default::default()
        };
        let err = pay_and_record(
            &ctx,
            &config,
            &Client::new(reqwest::Client::new()),
            &covenant_x402::MockSigner,
            &payer,
            &sample_call(),
        )
        .await
        .expect_err("over budget");
        assert!(matches!(err, X402DaemonError::BudgetExceeded));

        // The pre-check is read-only: a refusal must not spend or record.
        assert!(settlement.recent(10).await.unwrap().is_empty());
        assert!(audit.recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn pay_and_record_refuses_when_payer_has_no_capacity() {
        let settlement = InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(5);
        // No set_capacity: would_exceed resolves to NoCapacity.

        let ctx = SettlementContext {
            settlement: &settlement,
            audit: &audit,
            budget: &budget,
            issuer: &issuer,
        };
        let config = X402Config {
            enabled: true,
            ..Default::default()
        };
        let err = pay_and_record(
            &ctx,
            &config,
            &Client::new(reqwest::Client::new()),
            &covenant_x402::MockSigner,
            &payer,
            &sample_call(),
        )
        .await
        .expect_err("no capacity");
        assert!(matches!(err, X402DaemonError::NoCapacity));

        assert!(settlement.recent(10).await.unwrap().is_empty());
        assert!(audit.recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn paid_response_body_read_is_bounded() {
        use wiremock::{
            matchers::{method, path},
            Mock, MockServer, ResponseTemplate,
        };
        // The paid endpoint is untrusted: a normal body reads back whole, but a
        // success body past the cap is refused, not buffered into a worker's
        // memory after the payment already settled.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ok"))
            .respond_with(ResponseTemplate::new(200).set_body_string("hello"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/big"))
            .respond_with(ResponseTemplate::new(200).set_body_string("a".repeat(4096)))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();

        let ok = http
            .get(format!("{}/ok", server.uri()))
            .send()
            .await
            .expect("request");
        assert_eq!(read_capped(ok, 64).await.expect("under cap"), "hello");

        // wiremock always emits Content-Length, so this trips the early
        // declared-length reject; the running accumulation guard for an absent
        // or understated header is inspection-verified (same as the x402
        // client's sibling test).
        let big = http
            .get(format!("{}/big", server.uri()))
            .send()
            .await
            .expect("request");
        let err = read_capped(big, 64).await.expect_err("over cap");
        assert!(
            matches!(err, X402DaemonError::ResponseTooLarge(_)),
            "got {err:?}"
        );
    }
}
