//! Daemon-driven on-chain settlement flush (devnet).
//!
//! `covenant-settlement` records receipts to a JSONL log and owns the
//! batch/flush/`mark_batch_confirmed` primitives, but nothing inside the
//! daemon autonomously anchors those batches on-chain. This module is that
//! driver: on an interval it reads unsettled receipts, forms a batch, and
//! delegates the `anchor_receipt_batch` submission to the standalone
//! `covenant-settlement-signer` sidecar — the same subprocess isolation the
//! x402 and Metaplex signers use. The anchoring key and the anchor-lang /
//! solana-sdk dep tree stay out of the daemon's address space and build.
//!
//! Devnet only. [`SubprocessSettlementSigner::from_config`] refuses to build
//! a signer unless the cluster is `devnet` and the RPC endpoint looks like
//! devnet, and the sidecar enforces the same gate again before it signs.
//! Mainnet promotion is a separate, human-approved step.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use covenant_settlement::{build_receipt_batch, ChainConfirmation, Settlement, SettlementError};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::x402::{read_signer_output, MAX_SIGNER_OUTPUT_BYTES, SIGNER_OUTPUT_DEADLINE};

/// One flush request to the settlement signer sidecar. JSON on the sidecar's
/// stdin.
///
/// This is intentionally duplicated with the sidecar's own `AnchorRequest`
/// (`covenant-settlement-signer`): keeping the daemon free of the sidecar's
/// solana dependency tree is the whole point of the process split, so the two
/// ends own their own copy of the tiny wire contract. The two must stay in
/// sync — three fields, all named identically.
#[derive(Debug, Clone, Serialize)]
pub struct SettlementAnchorRequest {
    pub batch_id: String,
    pub merkle_root: String,
    pub receipt_count: u32,
}

/// The sidecar's reply. JSON on its stdout. Duplicated with the sidecar's
/// `AnchorResponse`; see [`SettlementAnchorRequest`].
#[derive(Debug, Clone, Deserialize)]
pub struct SettlementAnchorResponse {
    pub confirmed: bool,
    pub signature: String,
    pub slot: u64,
    pub confirmed_at: u64,
    pub already_anchored: bool,
    #[allow(dead_code)]
    pub cluster: String,
}

/// Delegates one `anchor_receipt_batch` submission to a signer. The daemon
/// uses [`SubprocessSettlementSigner`]; tests use in-process stubs.
#[async_trait::async_trait]
pub trait SettlementSigner: Send + Sync {
    async fn anchor(
        &self,
        request: SettlementAnchorRequest,
    ) -> Result<SettlementAnchorResponse, String>;
}

/// Config for the autonomous settlement flush driver, resolved from env.
/// Disabled by default and hard-scoped to devnet.
#[derive(Debug, Clone)]
pub struct SettlementFlushConfig {
    pub enabled: bool,
    pub interval: Duration,
    pub cluster: String,
    pub rpc_url: String,
    pub program_id: String,
    pub keypair_path: String,
    pub signer_binary: String,
    /// Maximum receipts anchored per flush. The driver drains the oldest
    /// unsettled receipts up to this cap each tick, so a larger backlog is
    /// worked across successive flushes rather than in one oversized batch.
    pub limit: usize,
}

impl Default for SettlementFlushConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval: Duration::from_secs(900),
            cluster: "devnet".to_string(),
            rpc_url: String::new(),
            program_id: String::new(),
            keypair_path: String::new(),
            signer_binary: String::new(),
            limit: 256,
        }
    }
}

/// Resolve the settlement flush driver config from env. Infallible: bad
/// numeric values fall back to defaults rather than refusing to boot. The
/// devnet safety gate lives in [`SubprocessSettlementSigner::from_config`],
/// which runs before any transaction.
pub fn settlement_flush_config_from_env() -> SettlementFlushConfig {
    settlement_flush_config_from_values(
        std::env::var("COVENANT_SETTLEMENT_AUTO_FLUSH")
            .ok()
            .as_deref(),
        std::env::var("COVENANT_SETTLEMENT_FLUSH_INTERVAL_SECS")
            .ok()
            .as_deref(),
        std::env::var("COVENANT_SETTLEMENT_CLUSTER").ok().as_deref(),
        std::env::var("COVENANT_SETTLEMENT_RPC_URL").ok().as_deref(),
        std::env::var("COVENANT_SETTLEMENT_PROGRAM_ID")
            .ok()
            .as_deref(),
        std::env::var("COVENANT_SETTLEMENT_KEYPAIR").ok().as_deref(),
        std::env::var("COVENANT_SETTLEMENT_SIGNER_BIN")
            .ok()
            .as_deref(),
        std::env::var("COVENANT_SETTLEMENT_FLUSH_LIMIT")
            .ok()
            .as_deref(),
    )
}

/// Pure resolver over the raw env values — no process-global reads, so tests
/// drive every branch deterministically.
#[allow(clippy::too_many_arguments)]
pub fn settlement_flush_config_from_values(
    enabled: Option<&str>,
    interval_secs: Option<&str>,
    cluster: Option<&str>,
    rpc_url: Option<&str>,
    program_id: Option<&str>,
    keypair_path: Option<&str>,
    signer_binary: Option<&str>,
    limit: Option<&str>,
) -> SettlementFlushConfig {
    let mut config = SettlementFlushConfig {
        enabled: matches!(
            enabled.map(str::trim),
            Some("1") | Some("true") | Some("yes")
        ),
        ..Default::default()
    };
    if let Some(secs) = interval_secs
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
    {
        config.interval = Duration::from_secs(secs);
    }
    if let Some(cluster) = cluster.map(str::trim).filter(|s| !s.is_empty()) {
        config.cluster = cluster.to_string();
    }
    config.rpc_url = rpc_url.unwrap_or("").trim().to_string();
    config.program_id = program_id.unwrap_or("").trim().to_string();
    config.keypair_path = keypair_path.unwrap_or("").trim().to_string();
    config.signer_binary = signer_binary.unwrap_or("").trim().to_string();
    if let Some(limit) = limit
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
    {
        config.limit = limit;
    }
    config
}

/// A [`SettlementSigner`] that delegates to the `covenant-settlement-signer`
/// sidecar. The daemon spawns it per flush, pipes the request as JSON to
/// stdin, and reads the response from stdout under the shared signer output
/// cap and deadline. The anchoring key never enters the daemon: the sidecar
/// reads it from its own environment (`COVENANT_SETTLEMENT_KEYPAIR`).
pub struct SubprocessSettlementSigner {
    program: PathBuf,
    env: Vec<(String, String)>,
}

impl SubprocessSettlementSigner {
    /// Build a signer, or refuse when the target is not an explicit devnet
    /// deployment. This is the daemon-side half of the mainnet gate: it
    /// mirrors the sidecar's `assert_devnet` so a misconfigured driver fails
    /// before it ever spawns a signing subprocess.
    pub fn from_config(config: &SettlementFlushConfig) -> Result<Self, String> {
        if config.cluster != "devnet" {
            return Err(format!(
                "settlement flush is devnet-only; refusing cluster {:?}",
                config.cluster
            ));
        }
        if !config.rpc_url.contains("devnet") {
            return Err(format!(
                "settlement flush RPC {:?} is not a recognizable devnet endpoint",
                config.rpc_url
            ));
        }
        // Two-sided with the check above: reject any mainnet mention even when
        // the devnet substring is present, so a "devnet" proxy in front of
        // mainnet cannot slip through. Fail closed.
        if config.rpc_url.contains("mainnet") {
            return Err(format!(
                "settlement flush RPC {:?} names mainnet; refusing (devnet-only)",
                config.rpc_url
            ));
        }
        if config.program_id.is_empty() {
            return Err("settlement flush requires COVENANT_SETTLEMENT_PROGRAM_ID".to_string());
        }
        if config.keypair_path.is_empty() {
            return Err("settlement flush requires COVENANT_SETTLEMENT_KEYPAIR".to_string());
        }
        if config.signer_binary.is_empty() {
            return Err("settlement flush requires COVENANT_SETTLEMENT_SIGNER_BIN".to_string());
        }
        let env = vec![
            (
                "COVENANT_SETTLEMENT_KEYPAIR".to_string(),
                config.keypair_path.clone(),
            ),
            (
                "COVENANT_SETTLEMENT_RPC_URL".to_string(),
                config.rpc_url.clone(),
            ),
            (
                "COVENANT_SETTLEMENT_CLUSTER".to_string(),
                config.cluster.clone(),
            ),
            (
                "COVENANT_SETTLEMENT_PROGRAM_ID".to_string(),
                config.program_id.clone(),
            ),
        ];
        Ok(Self {
            program: PathBuf::from(&config.signer_binary),
            env,
        })
    }
}

#[async_trait::async_trait]
impl SettlementSigner for SubprocessSettlementSigner {
    async fn anchor(
        &self,
        request: SettlementAnchorRequest,
    ) -> Result<SettlementAnchorResponse, String> {
        let payload = serde_json::to_vec(&request).map_err(|e| format!("encode request: {e}"))?;

        let mut child = Command::new(&self.program)
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // An elapsed deadline or over-cap flood returns early, dropping the
            // Child; kill_on_drop reaps the sidecar instead of leaving it
            // running detached.
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("spawn signer {:?}: {e}", self.program))?;

        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| "signer stdin unavailable".to_string())?;
            stdin
                .write_all(&payload)
                .await
                .map_err(|e| format!("write to signer: {e}"))?;
            // Drop closes stdin so the one-shot sidecar sees EOF.
        }

        let (stdout_bytes, stderr_bytes, status) =
            read_signer_output(&mut child, MAX_SIGNER_OUTPUT_BYTES, SIGNER_OUTPUT_DEADLINE)
                .await
                .map_err(|e| e.message())?;

        if !status.success() {
            let stderr = String::from_utf8_lossy(&stderr_bytes);
            return Err(format!("signer exited {}: {}", status, stderr.trim()));
        }

        let stdout =
            String::from_utf8(stdout_bytes).map_err(|e| format!("signer stdout not utf-8: {e}"))?;
        serde_json::from_str::<SettlementAnchorResponse>(stdout.trim())
            .map_err(|e| format!("decode signer response: {e}"))
    }
}

/// The result of one flush iteration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlushOutcome {
    /// No unsettled receipts to anchor.
    NoReceipts,
    /// A batch was anchored (or found already anchored) and its receipts
    /// were marked confirmed.
    Anchored {
        batch_id: String,
        signature: String,
        slot: u64,
        confirmed_at: u64,
        receipts_updated: u64,
        already_anchored: bool,
    },
}

/// One flush cycle: take the oldest unsettled receipts, form a batch, anchor
/// it through `signer`, and — only once the signer reports on-chain
/// confirmation — mark the batch confirmed with the real transaction
/// signature, slot, and block timestamp.
///
/// `limit` caps the batch size. Draining oldest first
/// ([`Settlement::unsettled`], not `recent`) is load-bearing: a backlog larger
/// than one batch is worked FIFO across successive flushes, so no receipt is
/// ever stranded outside a newest-`limit` window.
///
/// The confirmation gate is load-bearing too: a submission that is not
/// confirmed leaves the receipts unsettled and retryable rather than stranding
/// them as "settled" against a transaction that never landed. Idempotency
/// comes from the batch id being derived from the receipts' merkle root, so a
/// re-flush after a crash rebuilds the identical batch, which the sidecar
/// recognizes as already anchored.
pub async fn flush_once(
    settlement: &dyn Settlement,
    signer: &dyn SettlementSigner,
    cluster: &str,
    limit: usize,
) -> Result<FlushOutcome, String> {
    let receipts = settlement
        .unsettled(limit)
        .await
        .map_err(|e| format!("read receipts: {e}"))?;

    let batch = match build_receipt_batch(&receipts) {
        Ok(batch) => batch,
        Err(SettlementError::EmptyBatch) => return Ok(FlushOutcome::NoReceipts),
        Err(e) => return Err(format!("build batch: {e}")),
    };

    let response = signer
        .anchor(SettlementAnchorRequest {
            batch_id: batch.batch_id.clone(),
            merkle_root: batch.merkle_root.clone(),
            receipt_count: batch.receipt_count,
        })
        .await?;

    if !response.confirmed {
        return Err(format!(
            "signer did not confirm batch {}; leaving receipts unsettled",
            batch.batch_id
        ));
    }

    let confirmation = ChainConfirmation {
        chain: "solana".to_string(),
        cluster: cluster.to_string(),
        batch_id: batch.batch_id.clone(),
        merkle_root: batch.merkle_root.clone(),
        tx_sig: Some(response.signature.clone()),
        slot: Some(response.slot),
        confirmed_at: Some(response.confirmed_at),
    };
    let receipts_updated = settlement
        .mark_batch_confirmed(&batch.receipt_ids, confirmation)
        .await
        .map_err(|e| format!("mark batch confirmed: {e}"))?;

    Ok(FlushOutcome::Anchored {
        batch_id: batch.batch_id,
        signature: response.signature,
        slot: response.slot,
        confirmed_at: response.confirmed_at,
        receipts_updated,
        already_anchored: response.already_anchored,
    })
}

/// Spawn the autonomous settlement flush driver. Strictly opt-in
/// (`COVENANT_SETTLEMENT_AUTO_FLUSH`) and devnet-only. Builds the signer once
/// and, if the target is not a valid devnet deployment, logs the refusal and
/// exits without ticking. Every per-tick failure is logged and swallowed so
/// the driver keeps running.
pub fn spawn_settlement_flush_driver(
    settlement: Arc<dyn Settlement>,
    config: SettlementFlushConfig,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let signer = match SubprocessSettlementSigner::from_config(&config) {
            Ok(signer) => signer,
            Err(e) => {
                tracing::warn!(error = %e, "settlement flush driver refused to start");
                return;
            }
        };
        let mut interval = tokio::time::interval(config.interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            match flush_once(settlement.as_ref(), &signer, &config.cluster, config.limit).await {
                Ok(FlushOutcome::NoReceipts) => {}
                Ok(FlushOutcome::Anchored {
                    batch_id,
                    signature,
                    slot,
                    receipts_updated,
                    already_anchored,
                    ..
                }) => {
                    tracing::info!(
                        batch_id = %batch_id,
                        signature = %signature,
                        slot,
                        receipts_updated,
                        already_anchored,
                        "settlement batch anchored on devnet"
                    );
                }
                Err(e) => {
                    tracing::warn!(error = %e, "settlement flush failed; will retry next tick");
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_settlement::{InMemorySettlement, JsonlReceiptStore};
    use covenant_types::{AgentId, ResourceKind, SettlementReceipt};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use uuid::Uuid;

    fn receipt() -> SettlementReceipt {
        SettlementReceipt {
            id: Uuid::new_v4(),
            payer: AgentId::new("user@local", [0u8; 32]),
            resource: ResourceKind::Compute,
            memory_record_id: None,
            credits_consumed: 1,
            settled_at: 1_700_000_000_000,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: None,
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
        }
    }

    /// A signer stub with a scripted response and a call counter, so tests
    /// can assert both what flush_once does with a reply and whether it
    /// dispatched at all.
    struct StubSigner {
        response: Result<SettlementAnchorResponse, String>,
        calls: AtomicUsize,
    }

    impl StubSigner {
        fn confirming(already_anchored: bool) -> Self {
            Self {
                response: Ok(SettlementAnchorResponse {
                    confirmed: true,
                    signature: "s".repeat(88),
                    slot: 321,
                    confirmed_at: 1_700_000_123,
                    already_anchored,
                    cluster: "devnet".to_string(),
                }),
                calls: AtomicUsize::new(0),
            }
        }

        fn unconfirmed() -> Self {
            Self {
                response: Ok(SettlementAnchorResponse {
                    confirmed: false,
                    signature: String::new(),
                    slot: 0,
                    confirmed_at: 0,
                    already_anchored: false,
                    cluster: "devnet".to_string(),
                }),
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl SettlementSigner for StubSigner {
        async fn anchor(
            &self,
            _request: SettlementAnchorRequest,
        ) -> Result<SettlementAnchorResponse, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.response.clone()
        }
    }

    fn devnet_config() -> SettlementFlushConfig {
        SettlementFlushConfig {
            cluster: "devnet".to_string(),
            rpc_url: "https://api.devnet.solana.com".to_string(),
            program_id: "cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y".to_string(),
            keypair_path: "/tmp/does-not-need-to-exist.json".to_string(),
            signer_binary: "/tmp/signer".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn from_config_refuses_non_devnet_cluster() {
        let mut config = devnet_config();
        config.cluster = "mainnet-beta".to_string();
        let err = SubprocessSettlementSigner::from_config(&config)
            .err()
            .unwrap();
        assert!(err.contains("devnet-only"), "unexpected: {err}");
    }

    #[test]
    fn from_config_refuses_mainnet_rpc() {
        let mut config = devnet_config();
        config.rpc_url = "https://api.mainnet-beta.solana.com".to_string();
        let err = SubprocessSettlementSigner::from_config(&config)
            .err()
            .unwrap();
        assert!(err.contains("devnet endpoint"), "unexpected: {err}");
    }

    #[test]
    fn from_config_refuses_rpc_naming_mainnet_even_with_devnet_substring() {
        let mut config = devnet_config();
        config.rpc_url = "https://devnet.mainnet-beta.example.com".to_string();
        let err = SubprocessSettlementSigner::from_config(&config)
            .err()
            .unwrap();
        assert!(err.contains("names mainnet"), "unexpected: {err}");
    }

    #[test]
    fn from_config_requires_keypair_program_and_binary() {
        for mutate in [
            (|c: &mut SettlementFlushConfig| c.program_id.clear())
                as fn(&mut SettlementFlushConfig),
            |c: &mut SettlementFlushConfig| c.keypair_path.clear(),
            |c: &mut SettlementFlushConfig| c.signer_binary.clear(),
        ] {
            let mut config = devnet_config();
            mutate(&mut config);
            assert!(SubprocessSettlementSigner::from_config(&config).is_err());
        }
    }

    #[test]
    fn config_from_values_gates_enable_and_devnet_default() {
        let off =
            settlement_flush_config_from_values(None, None, None, None, None, None, None, None);
        assert!(!off.enabled);
        assert_eq!(off.cluster, "devnet");

        let on = settlement_flush_config_from_values(
            Some("true"),
            Some("30"),
            Some("devnet"),
            Some("https://api.devnet.solana.com"),
            Some("cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y"),
            Some("/keys/devnet.json"),
            Some("/bin/signer"),
            Some("64"),
        );
        assert!(on.enabled);
        assert_eq!(on.interval, Duration::from_secs(30));
        assert_eq!(on.limit, 64);
        assert_eq!(on.rpc_url, "https://api.devnet.solana.com");
    }

    #[tokio::test]
    async fn flush_once_with_no_receipts_is_a_noop() {
        let settlement = InMemorySettlement::new();
        let signer = StubSigner::confirming(false);
        let outcome = flush_once(&settlement, &signer, "devnet", 256)
            .await
            .unwrap();
        assert_eq!(outcome, FlushOutcome::NoReceipts);
        assert_eq!(signer.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn flush_once_marks_confirmed_only_on_finality() {
        let settlement = InMemorySettlement::new();
        settlement.record(receipt()).await.unwrap();

        // An unconfirmed submission must not settle anything.
        let unconfirmed = StubSigner::unconfirmed();
        let err = flush_once(&settlement, &unconfirmed, "devnet", 256)
            .await
            .unwrap_err();
        assert!(err.contains("did not confirm"), "unexpected: {err}");
        let after = settlement.recent(256).await.unwrap();
        assert!(
            after
                .iter()
                .all(|r| r.batch_id.is_none() && r.confirmed_at.is_none()),
            "unconfirmed flush must leave receipts unsettled"
        );

        // A confirmed submission settles with the real signature/slot/timestamp.
        let confirmed = StubSigner::confirming(false);
        let outcome = flush_once(&settlement, &confirmed, "devnet", 256)
            .await
            .unwrap();
        match outcome {
            FlushOutcome::Anchored {
                receipts_updated,
                slot,
                ..
            } => {
                assert_eq!(receipts_updated, 1);
                assert_eq!(slot, 321);
            }
            other => panic!("unexpected: {other:?}"),
        }
        let settled = settlement.recent(256).await.unwrap();
        assert!(settled.iter().all(|r| {
            r.batch_id.is_some()
                && r.tx_sig.as_deref() == Some(&"s".repeat(88))
                && r.slot == Some(321)
                && r.confirmed_at == Some(1_700_000_123)
        }));
    }

    #[tokio::test]
    async fn reflush_after_settled_is_noreceipts_and_does_not_call_signer() {
        let settlement = InMemorySettlement::new();
        settlement.record(receipt()).await.unwrap();

        let first = StubSigner::confirming(false);
        flush_once(&settlement, &first, "devnet", 256)
            .await
            .unwrap();

        // Every receipt now carries a batch_id, so the batch is empty and the
        // signer is never dispatched — no risk of a double-anchor.
        let second = StubSigner::confirming(false);
        let outcome = flush_once(&settlement, &second, "devnet", 256)
            .await
            .unwrap();
        assert_eq!(outcome, FlushOutcome::NoReceipts);
        assert_eq!(second.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn crash_between_anchor_and_mark_recovers_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("receipts").join("working.jsonl");

        // Record receipts and capture the batch id the daemon would derive.
        let store = JsonlReceiptStore::open(path.clone()).await.unwrap();
        store.record(receipt()).await.unwrap();
        store.record(receipt()).await.unwrap();
        let pre = store.recent(256).await.unwrap();
        let expected = build_receipt_batch(&pre).unwrap();
        drop(store);

        // Simulate the crash window: the sidecar anchored on-chain but the
        // daemon died before mark_batch_confirmed. Reopen the store; the
        // receipts are still unsettled, so a recovery flush rebuilds the same
        // batch id and the sidecar reports it already anchored.
        let store = JsonlReceiptStore::open(path.clone()).await.unwrap();
        let signer = StubSigner::confirming(true);
        let outcome = flush_once(&store, &signer, "devnet", 256).await.unwrap();
        match outcome {
            FlushOutcome::Anchored {
                batch_id,
                already_anchored,
                receipts_updated,
                ..
            } => {
                assert_eq!(
                    batch_id, expected.batch_id,
                    "recovery must reuse the derived batch id"
                );
                assert!(
                    already_anchored,
                    "recovery flush should hit the idempotent path"
                );
                assert_eq!(receipts_updated, 2);
            }
            other => panic!("unexpected: {other:?}"),
        }

        // A third flush has nothing left to do.
        let again = StubSigner::confirming(true);
        assert_eq!(
            flush_once(&store, &again, "devnet", 256).await.unwrap(),
            FlushOutcome::NoReceipts
        );
        assert_eq!(again.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn backlog_larger_than_limit_drains_oldest_first_without_stranding() {
        // The receipt-stranding regression: a backlog bigger than one batch
        // must be worked oldest-first across flushes. Reading the *newest*
        // `limit` receipts would settle only the tail and permanently strand
        // the oldest once the log grew past `limit`.
        let settlement = InMemorySettlement::new();
        let mut ids = Vec::new();
        for _ in 0..7 {
            let r = receipt();
            ids.push(r.id);
            settlement.record(r).await.unwrap();
        }

        let limit = 3;
        let first = StubSigner::confirming(false);
        match flush_once(&settlement, &first, "devnet", limit)
            .await
            .unwrap()
        {
            FlushOutcome::Anchored {
                receipts_updated, ..
            } => assert_eq!(receipts_updated, 3),
            other => panic!("unexpected: {other:?}"),
        }

        // The OLDEST three settled; the newer four wait for later flushes.
        let settled_ids = |receipts: &[SettlementReceipt]| {
            receipts
                .iter()
                .filter(|r| r.batch_id.is_some())
                .map(|r| r.id)
                .collect::<std::collections::HashSet<_>>()
        };
        let after_first = settled_ids(&settlement.recent(256).await.unwrap());
        assert!(
            ids[..3].iter().all(|id| after_first.contains(id)),
            "oldest three settle first"
        );
        assert!(
            ids[3..].iter().all(|id| !after_first.contains(id)),
            "newer receipts must wait"
        );

        // Keep flushing until the store is drained; the bound catches a
        // regression that stops making progress.
        let mut anchoring_flushes = 1;
        loop {
            let signer = StubSigner::confirming(false);
            match flush_once(&settlement, &signer, "devnet", limit)
                .await
                .unwrap()
            {
                FlushOutcome::NoReceipts => break,
                FlushOutcome::Anchored { .. } => {
                    anchoring_flushes += 1;
                    assert!(
                        anchoring_flushes <= 3,
                        "7 receipts at batch 3 drains in 3 flushes"
                    );
                }
            }
        }

        // Every receipt — including the oldest, which a newest-`limit` read
        // would have stranded — is now settled.
        let settled = settlement.recent(256).await.unwrap();
        assert_eq!(settled.len(), 7);
        assert!(
            settled
                .iter()
                .all(|r| r.batch_id.is_some() && r.confirmed_at.is_some()),
            "no receipt may be left unsettled after the backlog drains"
        );
    }
}
