//! Standalone x402 funding-key signer (sidecar to covenantd and to the
//! compute coordinator).
//!
//! One-shot, stdin→stdout. The daemon spawns this process per paid
//! call, pipes the chosen [`PaymentRequirements`] as JSON to stdin,
//! and reads the resulting `x-payment` header from stdout. The funding
//! key never enters the daemon's address space, and the Solana dep
//! tree never enters the daemon's build.
//!
//! Protocol (default mode, no args — the x402 payer flow):
//! - stdin:  a single JSON [`PaymentRequirements`] object.
//! - stdout: the `x-payment` header value (one line) on success.
//! - exit 0 on success; non-zero with a message on stderr otherwise.
//!
//! Dispatch is by inspecting `requirements.extra.feePayer`:
//! - present → sponsored flow (`PayaiSolanaSigner`): builds a v0
//!   `VersionedTransaction` whose payer slot is the sponsor's pubkey
//!   and partial-signs as funder; the facilitator co-signs at settle.
//! - absent → self-paid flow (`SolanaSigner`): builds a legacy
//!   `Transaction` and full-signs, with the funder paying SOL gas.
//!
//! Protocol (`payout` mode — first CLI arg is `payout`; used by
//! `covenant-compute-coordinator`'s `SidecarPayout`): a direct,
//! no-facilitator SPL transfer. There is no 402 challenge and no
//! sponsor here — this process itself is the payer, so it submits and
//! confirms the transfer instead of just building it.
//! - stdin:  a single JSON [`PayoutRequest`] object.
//! - stdout: a single JSON [`PayoutResponse`] object (the confirmed
//!   tx signature) on success.
//! - exit 0 on success; non-zero with a message on stderr otherwise.
//!
//! Configuration (env, both modes):
//! - `COVENANT_X402_FUNDING_KEYPAIR` — path to the Solana keypair JSON
//!   that funds payments. Required.
//! - `COVENANT_X402_RPC_URL` — Solana RPC for the blockhash + mint
//!   decimals lookup. Defaults to mainnet-beta in payment mode; payout
//!   mode requires the caller set this explicitly (a coordinator
//!   moving real funds should never rely on an implicit network).

use std::process::ExitCode;

use covenant_x402::{PayaiSolanaSigner, PaymentRequirements, Signer, SolanaSigner};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    if std::env::args().nth(1).as_deref() == Some("payout") {
        return match run_payout().await {
            Ok(response) => match serde_json::to_string(&response) {
                Ok(line) => write_stdout_line(&line).await,
                Err(e) => {
                    eprintln!("covenant-x402-signer: encode payout response: {e}");
                    ExitCode::FAILURE
                }
            },
            // A payout failure names its stage on stdout so the caller
            // can tell a safe-to-retry failure from one whose transfer
            // may be live on-chain; stderr keeps the human-readable
            // line either way.
            Err(failure) => {
                eprintln!("covenant-x402-signer: {}", failure.error);
                if let Ok(line) = serde_json::to_string(&failure) {
                    let _ = write_stdout_line(&line).await;
                }
                ExitCode::FAILURE
            }
        };
    }

    match run().await {
        Ok(header) => write_stdout_line(&header).await,
        Err(e) => {
            eprintln!("covenant-x402-signer: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn write_stdout_line(line: &str) -> ExitCode {
    let mut stdout = tokio::io::stdout();
    if let Err(e) = stdout.write_all(line.as_bytes()).await {
        eprintln!("covenant-x402-signer: write stdout: {e}");
        return ExitCode::FAILURE;
    }
    let _ = stdout.write_all(b"\n").await;
    let _ = stdout.flush().await;
    ExitCode::SUCCESS
}

async fn run() -> Result<String, Box<dyn std::error::Error>> {
    let keypair_path = std::env::var("COVENANT_X402_FUNDING_KEYPAIR")
        .map_err(|_| "COVENANT_X402_FUNDING_KEYPAIR is not set")?;
    let rpc_url =
        std::env::var("COVENANT_X402_RPC_URL").unwrap_or_else(|_| DEFAULT_RPC_URL.to_string());

    let mut input = String::new();
    tokio::io::stdin().read_to_string(&mut input).await?;
    let requirement: PaymentRequirements = serde_json::from_str(input.trim())
        .map_err(|e| format!("decode PaymentRequirements from stdin: {e}"))?;

    let sponsored = requirement
        .extra
        .as_ref()
        .and_then(|e| e.fee_payer.as_ref())
        .is_some();

    if sponsored {
        let signer = PayaiSolanaSigner::from_keypair_file(&keypair_path, rpc_url)?;
        Ok(signer.build_payment(&requirement).await?)
    } else {
        let signer = SolanaSigner::from_keypair_file(&keypair_path, rpc_url)?;
        Ok(signer.build_payment(&requirement).await?)
    }
}

/// A plain outbound-transfer request — the compute coordinator's
/// payout wire shape. `destination_owner` is the recipient's wallet
/// pubkey (not their ATA); this process derives + idempotently
/// creates the ATA itself, same as [`covenant_x402::solana::build_transfer_transaction`]
/// always has. `job_id` is opaque here — echoed into this process's
/// own tracing only, never interpreted. `memo` is likewise opaque: it
/// rides the transfer as an SPL Memo instruction verbatim, so the
/// caller can bind the on-chain transaction to whatever it is paying
/// for (the coordinator sends its receipt-derived payout memo).
#[derive(Debug, Deserialize)]
struct PayoutRequest {
    mint: String,
    destination_owner: String,
    amount: u64,
    #[serde(default)]
    job_id: Option<String>,
    #[serde(default)]
    memo: Option<String>,
}

#[derive(Debug, Serialize)]
struct PayoutResponse {
    signature: String,
}

/// The payout-mode failure wire shape. `stage` is
/// [`covenant_x402::TransferStage`]: `not_submitted` means the caller
/// may safely retry, `maybe_submitted` means the transfer could be live
/// on-chain and a blind retry risks paying twice. `signature` is
/// present whenever the transaction was signed, for reconciliation.
#[derive(Debug, Serialize)]
struct PayoutFailure {
    error: String,
    stage: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<String>,
}

impl PayoutFailure {
    fn not_submitted(error: impl std::fmt::Display) -> Self {
        Self {
            error: error.to_string(),
            stage: covenant_x402::TransferStage::NotSubmitted.as_str(),
            signature: None,
        }
    }
}

async fn run_payout() -> Result<PayoutResponse, PayoutFailure> {
    let keypair_path = std::env::var("COVENANT_X402_FUNDING_KEYPAIR")
        .map_err(|_| PayoutFailure::not_submitted("COVENANT_X402_FUNDING_KEYPAIR is not set"))?;
    // Unlike payment mode, payout has no safe implicit default: a
    // coordinator pushing a real transfer must say which network it
    // means every time.
    let rpc_url = std::env::var("COVENANT_X402_RPC_URL").map_err(|_| {
        PayoutFailure::not_submitted("COVENANT_X402_RPC_URL is not set (required for payout mode)")
    })?;

    let mut input = String::new();
    tokio::io::stdin()
        .read_to_string(&mut input)
        .await
        .map_err(|e| PayoutFailure::not_submitted(format!("read stdin: {e}")))?;
    let request: PayoutRequest = serde_json::from_str(input.trim())
        .map_err(|e| PayoutFailure::not_submitted(format!("decode PayoutRequest from stdin: {e}")))?;

    let signer = SolanaSigner::from_keypair_file(&keypair_path, rpc_url)
        .map_err(PayoutFailure::not_submitted)?;
    tracing::info!(
        job_id = request.job_id.as_deref().unwrap_or("unknown"),
        mint = %request.mint,
        destination_owner = %request.destination_owner,
        amount = request.amount,
        memo = request.memo.as_deref().unwrap_or(""),
        "submitting payout transfer"
    );
    let signature = signer
        .submit_transfer_staged(
            &request.mint,
            &request.destination_owner,
            request.amount,
            request.memo.as_deref(),
        )
        .await
        .map_err(|e| PayoutFailure {
            error: e.message,
            stage: e.stage.as_str(),
            signature: e.signature,
        })?;
    tracing::info!(
        job_id = request.job_id.as_deref().unwrap_or("unknown"),
        %signature,
        "payout transfer confirmed"
    );
    Ok(PayoutResponse { signature })
}
