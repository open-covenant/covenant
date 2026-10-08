//! Signing sidecar for the Covenant Compute on-chain lease meter.
//!
//! `covenant-compute-coordinator`'s `SidecarLeaseMeter` spawns this binary
//! once per step. The coordinator never links a Solana client and never
//! reads a key; everything that signs happens in this process.
//!
//! Protocol:
//! - argv[1]: `lease-open`, `lease-tick`, `lease-conclude`, `lease-void`, or
//!   `round` for an agent task's check-vote round (see `round.rs`), which
//!   answers with what the chain counted rather than a bare signature.
//! - stdin: one JSON object naming `program_id`, `mint`, `er_validator` and
//!   `job_id` (the uuid as 32 hex characters), plus the step's own fields:
//!   `operator`, `rate_micro_usdc_per_sec` and `max_duration_secs` to open,
//!   `metered_ms` and `receipt_hash_hex` to tick or conclude.
//! - stdout, exit 0: `{"signature": "..."}`.
//! - stdout, exit 1: `{"error": "...", "stage": "not_submitted" | "maybe_submitted"}`,
//!   with `signature` when a transaction was signed. `not_submitted` means
//!   the lease has not paid the operator for this step and no longer can, so
//!   the coordinator's own payout stands; `maybe_submitted` means something
//!   that changes what the vault pays may have landed.
//!
//! Environment (the coordinator clears everything else):
//! - `COVENANT_COMPUTE_LEASE_KEYPAIR`: the renter. Funds every vault from its
//!   token account, pays every fee and receives every refund.
//! - `COVENANT_COMPUTE_LEASE_COORDINATOR_KEYPAIR`: the only key the program
//!   accepts a tick, delegation, conclusion or void from. It never touches a
//!   vault and needs no SOL.
//! - `COVENANT_COMPUTE_LEASE_RPC_URL`: Solana L1.
//! - `COVENANT_COMPUTE_LEASE_ER_RPC_URL`: the rollup endpoint of the pinned
//!   validator. Checked against `er_validator` before anything is delegated.
//!
//! Drives either build of the meter, the standalone `compute-lease` program
//! or the lease meter inside the settlement program; which one is read off
//! the chain.

use std::process::ExitCode;

use covenant_compute_lease_signer::round::{RoundRequest, RoundSession};
use covenant_compute_lease_signer::steps::{refused, Failure, Request, Session};
use solana_sdk::signer::keypair::{read_keypair_file, Keypair};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const STEPS: [&str; 5] = [
    "lease-open",
    "lease-tick",
    "lease-conclude",
    "lease-void",
    "round",
];

#[tokio::main]
async fn main() -> ExitCode {
    let step = std::env::args().nth(1).unwrap_or_default();
    match run(&step).await {
        Ok(answer) => write_line(&answer).await,
        Err(failure) => {
            eprintln!("covenant-compute-lease-signer {step}: {}", failure.error);
            write_line(&failure).await;
            ExitCode::FAILURE
        }
    }
}

async fn run(step: &str) -> Result<serde_json::Value, Failure> {
    if !STEPS.contains(&step) {
        return Err(refused(format!(
            "unknown step {step:?}; expected one of {}",
            STEPS.join(", ")
        )));
    }
    let renter = keypair("COVENANT_COMPUTE_LEASE_KEYPAIR")?;
    let coordinator = keypair("COVENANT_COMPUTE_LEASE_COORDINATOR_KEYPAIR")?;
    let l1 = env("COVENANT_COMPUTE_LEASE_RPC_URL")?;
    let rollup = env("COVENANT_COMPUTE_LEASE_ER_RPC_URL")?;

    let mut input = String::new();
    tokio::io::stdin()
        .read_to_string(&mut input)
        .await
        .map_err(|e| refused(format!("read stdin: {e}")))?;
    let decode = |e: serde_json::Error| refused(format!("decode request: {e}"));

    if step == "round" {
        let request: RoundRequest = serde_json::from_str(input.trim()).map_err(decode)?;
        let outcome = RoundSession::new(renter, coordinator, &l1, &rollup, &request)?
            .run()
            .await?;
        return serde_json::to_value(outcome).map_err(|e| refused(format!("encode outcome: {e}")));
    }
    let request: Request = serde_json::from_str(input.trim()).map_err(decode)?;
    let session = Session::new(renter, coordinator, &l1, &rollup, &request)?;
    let signature = match step {
        "lease-open" => session.open(&request).await,
        "lease-tick" => session.tick(&request).await,
        "lease-conclude" => session.conclude(&request).await,
        _ => session.void().await,
    }?;
    Ok(serde_json::json!({ "signature": signature }))
}

fn env(name: &str) -> Result<String, Failure> {
    std::env::var(name).map_err(|_| refused(format!("{name} is not set")))
}

fn keypair(name: &str) -> Result<Keypair, Failure> {
    let path = env(name)?;
    read_keypair_file(&path).map_err(|e| refused(format!("read {name} at {path}: {e}")))
}

async fn write_line(value: &impl serde::Serialize) -> ExitCode {
    let line = match serde_json::to_string(value) {
        Ok(line) => line,
        Err(e) => {
            eprintln!("covenant-compute-lease-signer: encode response: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut stdout = tokio::io::stdout();
    if stdout.write_all(line.as_bytes()).await.is_err() || stdout.write_all(b"\n").await.is_err() {
        return ExitCode::FAILURE;
    }
    let _ = stdout.flush().await;
    ExitCode::SUCCESS
}
