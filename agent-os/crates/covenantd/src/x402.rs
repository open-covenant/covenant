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
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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

    async fn build_payment_with_limits(
        &self,
        requirements: &PaymentRequirements,
        max_output_bytes: usize,
        deadline: std::time::Duration,
    ) -> Result<String, X402Error> {
        let payload = serde_json::to_vec(requirements)
            .map_err(|e| X402Error::Sign(format!("encode requirement: {e}")))?;

        let mut child = Command::new(&self.program)
            .args(&self.args)
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // An over-cap flood or an elapsed deadline returns early, dropping
            // the Child; kill_on_drop reaps the sidecar instead of leaving it
            // running detached.
            .kill_on_drop(true)
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

        let (stdout_bytes, stderr_bytes, status) =
            read_signer_output(&mut child, max_output_bytes, deadline)
                .await
                .map_err(|e| X402Error::Sign(e.message()))?;

        if !status.success() {
            let stderr = String::from_utf8_lossy(&stderr_bytes);
            return Err(X402Error::Sign(format!(
                "signer exited {}: {}",
                status,
                stderr.trim()
            )));
        }

        let header = String::from_utf8(stdout_bytes)
            .map_err(|e| X402Error::Sign(format!("signer stdout not utf-8: {e}")))?;
        let header = header.trim().to_string();
        if header.is_empty() {
            return Err(X402Error::Sign("signer returned an empty header".into()));
        }
        Ok(header)
    }
}

#[async_trait::async_trait]
impl Signer for SubprocessSigner {
    async fn build_payment(&self, requirements: &PaymentRequirements) -> Result<String, X402Error> {
        self.build_payment_with_limits(
            requirements,
            MAX_SIGNER_OUTPUT_BYTES,
            SIGNER_OUTPUT_DEADLINE,
        )
        .await
    }
}

/// Maximum bytes the daemon buffers from one signer sidecar stream. The x402
/// and metaplex signer sidecars talk to Solana RPC and DAS, so their stdout and
/// stderr reflect external responses; an unbounded read lets a runaway, buggy,
/// or hostile-RPC-fed sidecar exhaust the daemon's memory. The result is a
/// single line, so 16 MiB sits far above any legitimate payload while still
/// capping a flood — the memory-axis sibling of the bounded paid-call response
/// read below and of the runner's `read_agent_output_capped`.
pub(crate) const MAX_SIGNER_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

/// Wall-clock budget for one signer dispatch, applied independently to the read
/// phase and to the post-kill reap (the stdin write is not counted), so a
/// wedged sidecar costs at most ~2× this before it is killed and reaped — the
/// same two-stage bound the SAP bridge worker uses. `wait_with_output()` had no
/// deadline at all, letting a sidecar that stalls with a pipe held open hang the
/// dispatch forever; this also lets the concurrent capped read make progress
/// when one stream stalls after the other has already returned.
pub(crate) const SIGNER_OUTPUT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

/// Why a signer sidecar's bounded output could not be read. Callers map each
/// arm onto their own signer error type (the metaplex signer's `String`, the
/// x402 signer's [`X402Error::Sign`]) via [`SignerOutputError::message`], so the
/// wording stays identical across both dispatches.
#[derive(Debug)]
pub(crate) enum SignerOutputError {
    /// stdout exceeded the byte cap; the sidecar was killed before any
    /// truncated envelope could be parsed.
    StdoutTooLarge(usize),
    /// The read-and-reap deadline (seconds) elapsed; the sidecar was killed.
    Timeout(u64),
    /// An I/O error reading a stream or reaping the sidecar.
    Io(std::io::Error),
}

impl SignerOutputError {
    pub(crate) fn message(&self) -> String {
        match self {
            SignerOutputError::StdoutTooLarge(cap) => {
                format!("signer stdout exceeded the {cap}-byte cap")
            }
            SignerOutputError::Timeout(secs) => format!("signer did not finish within {secs}s"),
            SignerOutputError::Io(e) => format!("read signer output: {e}"),
        }
    }
}

/// Reads up to `max` bytes from a signer subprocess stream, reporting whether
/// the stream had more to give. `take` stops the read at the cap instead of
/// buffering an unbounded body; reading one extra byte lets the caller tell an
/// exact-fit payload from a truncated flood. The returned buffer is clamped to
/// `max`.
async fn read_stream_capped<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut buf = Vec::new();
    reader.take(max as u64 + 1).read_to_end(&mut buf).await?;
    let overflowed = buf.len() > max;
    buf.truncate(max);
    Ok((buf, overflowed))
}

/// Reads a one-shot signer sidecar's stdout and stderr — each bounded to `max`
/// bytes — then reaps it, all within `deadline`. This replaces
/// `child.wait_with_output()`, which buffered both streams to EOF with no size
/// cap and no deadline, letting a runaway, buggy, or hostile-RPC-fed sidecar
/// OOM the daemon or hang the dispatch. The streams are read concurrently (not
/// stdout-then-stderr) so a sidecar that fills its stderr pipe before closing
/// stdout cannot starve the stdout read. On overflow or timeout the child is
/// SIGKILL-reaped before the bounded wait so a sidecar blocked on a full pipe
/// cannot stall the reap. A stderr-only flood is truncated and tolerated — the
/// valid stdout envelope still decodes — but stdout overflow fails closed so no
/// caller parses a truncated envelope.
pub(crate) async fn read_signer_output(
    child: &mut tokio::process::Child,
    max: usize,
    deadline: std::time::Duration,
) -> Result<(Vec<u8>, Vec<u8>, std::process::ExitStatus), SignerOutputError> {
    let mut stdout = child.stdout.take().expect("signer stdout piped");
    let mut stderr = child.stderr.take().expect("signer stderr piped");

    let read_both = async {
        tokio::join!(
            read_stream_capped(&mut stdout, max),
            read_stream_capped(&mut stderr, max),
        )
    };
    let (out, err) = match tokio::time::timeout(deadline, read_both).await {
        Ok(pair) => pair,
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(SignerOutputError::Timeout(deadline.as_secs()));
        }
    };
    let (stdout_bytes, stdout_overflowed) = out.map_err(SignerOutputError::Io)?;
    let (stderr_bytes, _stderr_overflowed) = err.map_err(SignerOutputError::Io)?;

    if stdout_overflowed {
        let _ = child.start_kill();
    }
    let status = match tokio::time::timeout(deadline, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => return Err(SignerOutputError::Io(e)),
        Err(_) => {
            let _ = child.start_kill();
            return Err(SignerOutputError::Timeout(deadline.as_secs()));
        }
    };
    if stdout_overflowed {
        return Err(SignerOutputError::StdoutTooLarge(max));
    }
    Ok((stdout_bytes, stderr_bytes, status))
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
    async fn build_payment_surfaces_non_utf8_signer_stdout() {
        // The sidecar is external (it talks to Solana RPC); its stdout is
        // untrusted, so bytes that are not valid UTF-8 must surface the
        // from_utf8 Sign error rather than fall through to the trim /
        // empty-header check and return a garbage header. A single 0xFF byte
        // (printf '\377') is never valid UTF-8.
        let signer = SubprocessSigner::new("sh")
            .arg("-c")
            .arg("cat >/dev/null; printf '\\377'");
        let err = signer
            .build_payment(&requirement())
            .await
            .expect_err("non-utf8 stdout must surface, not fall through");
        assert!(
            matches!(&err, X402Error::Sign(m) if m.contains("signer stdout not utf-8")),
            "a non-UTF-8 signer stdout must surface the from_utf8 Sign error: {err:?}"
        );
    }

    #[tokio::test]
    async fn build_payment_with_limits_rejects_oversized_signer_stdout() {
        // The signer sidecar is external (it talks to Solana RPC); a stdout
        // flood past the cap must surface a Sign error naming the cap instead
        // of buffering the whole stream and OOMing the daemon. The fake signer
        // drains stdin first so the payload write does not race a broken pipe,
        // then writes 200 bytes; a 64-byte cap forces the overflow branch.
        let signer = SubprocessSigner::new("sh")
            .arg("-c")
            .arg("cat >/dev/null; head -c 200 /dev/zero");
        let err = signer
            .build_payment_with_limits(&requirement(), 64, SIGNER_OUTPUT_DEADLINE)
            .await
            .expect_err("over cap");
        assert!(
            matches!(&err, X402Error::Sign(m) if m.contains("exceeded") && m.contains("cap")),
            "an over-cap signer stdout must surface as a cap-breach Sign error: {err:?}"
        );
    }

    #[tokio::test]
    async fn build_payment_tolerates_a_bounded_stderr_flood() {
        // A stderr-only flood is truncated and tolerated: the valid stdout
        // header still returns. This proves stderr overflow stays bounded
        // without failing a dispatch whose result is well-formed.
        let signer = SubprocessSigner::new("sh")
            .arg("-c")
            .arg("cat >/dev/null; head -c 200 /dev/zero >&2; printf 'mock-x-payment-header'");
        let header = signer
            .build_payment_with_limits(&requirement(), 64, SIGNER_OUTPUT_DEADLINE)
            .await
            .expect("a bounded stderr flood is non-fatal");
        assert_eq!(header, "mock-x-payment-header");
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

    #[tokio::test]
    async fn read_capped_reads_a_body_at_the_exact_cap_and_rejects_one_byte_over() {
        use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};
        // read_capped guards memory with `> max` on both the Content-Length
        // pre-check (x402.rs:496) and the running accumulation check (x402.rs:502),
        // so a body sized exactly at the cap fits and must read back whole.
        // paid_response_body_read_is_bounded only brackets the boundary from far
        // away ("hello"/cap 64 under, 4096/cap 64 over) and its comment concedes
        // the accumulation guard is inspection-verified, so a `> max -> >= max`
        // slip on either guard survives it. Serve a body of known length N: at
        // cap N the original accepts (N is not > N) while the mutant rejects, and
        // at cap N-1 the body sits one byte over and is refused.
        const BODY: &str = "covenant daemon x402 paid-call at-cap inclusive boundary fixture";
        let n = BODY.len();

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(BODY))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();

        let resp = http.get(server.uri()).send().await.expect("request");
        let body = read_capped(resp, n)
            .await
            .expect("a body sized exactly at the cap fits and must read back whole");
        assert_eq!(body, BODY);

        let resp = http.get(server.uri()).send().await.expect("request");
        let err = read_capped(resp, n - 1)
            .await
            .expect_err("a body one byte over the cap must be rejected");
        assert!(
            matches!(err, X402DaemonError::ResponseTooLarge(_)),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn read_stream_capped_treats_an_exact_max_fill_as_an_exact_fit_not_overflow() {
        // read_stream_capped reads through take(max + 1) and sets
        // overflowed = buf.len() > max (x402.rs:218), so a signer stream of
        // exactly max bytes is an exact fit (overflowed false, buffer returned
        // whole) and only max + 1 is a truncated flood (overflowed true, buffer
        // clamped to max) — the discriminator the extra read byte exists to
        // enable. The signer cap tests bracket this from far away:
        // build_payment_with_limits_rejects_oversized_signer_stdout floods far
        // past the cap and subprocess_signer_returns_stdout_header sits far
        // under, so neither lands on buf.len() == max, where a `> max` ->
        // `>= max` slip flips an exact-max signer stdout to a spurious overflow
        // that read_signer_output fails closed on with StdoutTooLarge, rejecting
        // a legal worst-case envelope sized exactly at the cap. Pin the
        // inclusive endpoint directly.
        let max = 64;

        let exact = vec![b'x'; max];
        let mut reader = exact.as_slice();
        let (buf, overflowed) = read_stream_capped(&mut reader, max)
            .await
            .expect("reading an in-memory slice cannot fail");
        assert!(
            !overflowed,
            "a stream of exactly max bytes is an exact fit, not a flood"
        );
        assert_eq!(
            buf, exact,
            "an exact-fit body must be returned whole, not truncated"
        );

        let flood = vec![b'x'; max + 1];
        let mut reader = flood.as_slice();
        let (buf, overflowed) = read_stream_capped(&mut reader, max)
            .await
            .expect("reading an in-memory slice cannot fail");
        assert!(
            overflowed,
            "a stream of max + 1 bytes is a truncated flood and must report overflow"
        );
        assert_eq!(
            buf.len(),
            max,
            "an over-cap read must clamp the returned buffer to max"
        );
    }
}
