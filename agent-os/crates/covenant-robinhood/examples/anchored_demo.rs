//! Anchored governed-trading demo. Four orders run through the Covenant policy
//! gate in dry-run (no order reaches any venue) and every decision's receipt is
//! anchored on Solana through the covenant-metaplex-signer sidecar. The
//! governance half is fully live — real policy, real signed receipts, real
//! on-chain anchors; only the venue leg is simulated.
//!
//! Env:
//!   COVENANT_METAPLEX_SIGNER_BIN         path to covenant-metaplex-signer
//!   COVENANT_METAPLEX_KEYPAIR            minting keypair path (sidecar only)
//!   COVENANT_METAPLEX_RPC_URL            solana RPC
//!   COVENANT_METAPLEX_CLUSTER            devnet | mainnet-beta
//!   COVENANT_ROBINHOOD_ATTESTOR_KEYPAIR  base64 seed file for the receipt attestor
//!   COVENANT_DEMO_OUT                    optional path for a JSON summary

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use covenant_metaplex::{AttestationPayload, SignerRequest, SignerResponse};
use covenant_robinhood::governed::GovernedTrader;
use covenant_robinhood::policy::{Approvals, Caps, Rate, Risk, TradingPolicy, Universe};
use covenant_robinhood::{
    Anchor, MockTransport, Mode, OrderRequest, OrderType, Result, RobinhoodClient, RobinhoodSigner,
    Side, SignedReceipt,
};
use ed25519_dalek::SigningKey;
use serde_json::json;

struct MetaplexAnchor {
    program: PathBuf,
    cluster: String,
}

impl Anchor for MetaplexAnchor {
    fn anchor(&self, signed: &SignedReceipt) -> Result<Option<String>> {
        let payload = AttestationPayload::new(
            signed.root_hash_hex.clone(),
            "covenant",
            "robinhood-demo",
            "trade",
            signed.receipt.decided_at,
        );
        let request = SignerRequest::AttestAuditRoot {
            payload: Box::new(payload),
            asset: None,
            collection: None,
        };
        let body = serde_json::to_vec(&request)
            .map_err(|e| covenant_robinhood::Error::Decode(e.to_string()))?;

        let mut child = Command::new(&self.program)
            .env_clear()
            .envs(
                ["COVENANT_METAPLEX_KEYPAIR", "COVENANT_METAPLEX_RPC_URL"]
                    .iter()
                    .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_string(), v))),
            )
            .env("COVENANT_METAPLEX_CLUSTER", &self.cluster)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| covenant_robinhood::Error::Transport(format!("spawn signer: {e}")))?;
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(&body)
            .map_err(|e| covenant_robinhood::Error::Transport(format!("write signer: {e}")))?;
        let out = child
            .wait_with_output()
            .map_err(|e| covenant_robinhood::Error::Transport(format!("await signer: {e}")))?;
        if !out.status.success() {
            return Err(covenant_robinhood::Error::Transport(format!(
                "signer exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let resp: SignerResponse = serde_json::from_slice(&out.stdout)
            .map_err(|e| covenant_robinhood::Error::Decode(format!("signer response: {e}")))?;
        Ok(Some(format!("{}:{}", resp.signature, resp.asset)))
    }
}

fn policy() -> TradingPolicy {
    TradingPolicy {
        version: 1,
        venue: "robinhood-crypto".into(),
        mode: Mode::DryRun,
        caps: Caps {
            per_order_usd: Some(500.0),
            daily_notional_usd: Some(2_000.0),
        },
        risk: Risk {
            daily_loss_stop_usd: Some(300.0),
        },
        universe: Universe {
            allow: Some(vec!["BTC-USD".into(), "ETH-USD".into()]),
            deny: None,
            sides: Some(vec![Side::Buy]),
        },
        order_types: vec![OrderType::Market],
        rate: Rate {
            max_orders_per_min: Some(10),
            cooldown_secs: None,
        },
        approvals: Approvals {
            require_human_over_usd: Some(400.0),
        },
    }
}

fn attestor() -> SigningKey {
    use base64::Engine as _;
    let path = std::env::var("COVENANT_ROBINHOOD_ATTESTOR_KEYPAIR").expect("attestor keypair path");
    let raw = std::fs::read_to_string(path).expect("read attestor seed");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw.trim())
        .expect("base64 seed");
    let seed: [u8; 32] = bytes[..32].try_into().expect("32-byte seed");
    SigningKey::from_bytes(&seed)
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let signer_bin = std::env::var("COVENANT_METAPLEX_SIGNER_BIN").expect("signer bin path");
    let cluster =
        std::env::var("COVENANT_METAPLEX_CLUSTER").unwrap_or_else(|_| "devnet".to_string());

    let mock = MockTransport::new()
        .json(
            "GET",
            "symbol=BTC-USD",
            json!({"results":[{"symbol":"BTC-USD","price":"60000"}]}),
        )
        .json(
            "GET",
            "symbol=DOGE-USD",
            json!({"results":[{"symbol":"DOGE-USD","price":"0.12"}]}),
        );
    let client = RobinhoodClient::new(
        RobinhoodSigner::new("demo", SigningKey::from_bytes(&[1u8; 32])),
        mock,
    );
    let key = attestor();
    println!("attestor pubkey (pin to verify receipts): {}", {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(key.verifying_key().to_bytes())
    });
    let trader = GovernedTrader::new(client, policy(), key, "covenant-demo-agent").with_anchor(
        Box::new(MetaplexAnchor {
            program: PathBuf::from(signer_bin),
            cluster: cluster.clone(),
        }),
    );

    let cases = [
        (
            "within policy   (~$60)",
            OrderRequest::market("BTC-USD", Side::Buy, 0.001),
        ),
        (
            "over per-order  (~$1200)",
            OrderRequest::market("BTC-USD", Side::Buy, 0.02),
        ),
        (
            "not in universe (DOGE)",
            OrderRequest::market("DOGE-USD", Side::Buy, 1_000.0),
        ),
        (
            "needs approval  (~$450)",
            OrderRequest::market("BTC-USD", Side::Buy, 0.0075),
        ),
    ];

    let mut summary = Vec::new();
    for (label, order) in cases {
        match trader.submit(order).await {
            Ok(s) => {
                let (tx, asset) = s
                    .anchor
                    .as_deref()
                    .and_then(|a| a.split_once(':'))
                    .map(|(t, a)| (t.to_string(), a.to_string()))
                    .unwrap_or_default();
                println!(
                    "{label:<26} -> {:<16?} verified={}\n{:28}tx    https://solscan.io/tx/{tx}\n{:28}asset https://solscan.io/token/{asset}\n{:28}root  {}  {}",
                    s.receipt.decision,
                    s.verify(),
                    "", "", "",
                    s.root_hash_hex,
                    s.receipt.reason.clone().unwrap_or_default(),
                );
                summary.push(json!({
                    "case": label.trim(),
                    "decision": s.receipt.decision,
                    "reason": s.receipt.reason,
                    "symbol": s.receipt.order.symbol,
                    "root_hash_hex": s.root_hash_hex,
                    "signature_b64": s.signature_b64,
                    "attestor_pubkey_b64": s.attestor_pubkey_b64,
                    "tx": tx,
                    "asset": asset,
                    "cluster": cluster,
                }));
            }
            Err(e) => println!("{label:<26} -> error: {e}"),
        }
    }

    if let Ok(out) = std::env::var("COVENANT_DEMO_OUT") {
        std::fs::write(&out, serde_json::to_vec_pretty(&summary).unwrap()).expect("write summary");
        println!("\nsummary -> {out}");
    }
}
