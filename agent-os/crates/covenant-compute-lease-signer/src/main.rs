//! Signing sidecar for the Covenant Compute on-chain lease meter.
//!
//! `covenant-compute-coordinator`'s `SidecarLeaseMeter` spawns this binary
//! once per step. The coordinator never links a Solana client and never
//! reads a key; everything that signs happens in this process.
//!
//! Protocol:
//! - argv[1]: `lease-open`, `lease-tick`, `lease-conclude` or `lease-void`.
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

mod lease;
mod rpc;
mod steps;

use std::process::ExitCode;

use solana_sdk::signer::keypair::{read_keypair_file, Keypair};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use steps::{refused, Failure, Request, Session};

const STEPS: [&str; 4] = ["lease-open", "lease-tick", "lease-conclude", "lease-void"];

#[tokio::main]
async fn main() -> ExitCode {
    let step = std::env::args().nth(1).unwrap_or_default();
    match run(&step).await {
        Ok(signature) => write_line(&serde_json::json!({ "signature": signature })).await,
        Err(failure) => {
            eprintln!("covenant-compute-lease-signer {step}: {}", failure.error);
            write_line(&failure).await;
            ExitCode::FAILURE
        }
    }
}

async fn run(step: &str) -> Result<String, Failure> {
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
    let request: Request =
        serde_json::from_str(input.trim()).map_err(|e| refused(format!("decode request: {e}")))?;

    let session = Session::new(renter, coordinator, &l1, &rollup, &request)?;
    match step {
        "lease-open" => session.open(&request).await,
        "lease-tick" => session.tick(&request).await,
        "lease-conclude" => session.conclude(&request).await,
        _ => session.void().await,
    }
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
