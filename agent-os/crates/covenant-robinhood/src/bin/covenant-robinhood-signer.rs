//! covenant-robinhood-signer — one-shot stdin to stdout Ed25519 request signer.
//!
//! covenantd spawns this per signed request with `env_clear()` plus only
//! `COVENANT_ROBINHOOD_API_KEY` and `COVENANT_ROBINHOOD_KEYPAIR`, so the
//! private key never enters the daemon's address space.
//!
//! - stdin:  a JSON `SignRequest`, e.g.
//!   `{"method":"POST","path":"/api/v1/crypto/trading/orders/","body":"{...}"}`
//! - stdout: a JSON `SignedHeaders` (`x-api-key` / `x-timestamp` / `x-signature`).
//! - exit 0 on success; non-zero with a message on stderr otherwise.
//!
//! Configuration (env):
//! - `COVENANT_ROBINHOOD_API_KEY` — the API key from the credentials portal.
//! - `COVENANT_ROBINHOOD_KEYPAIR` — path to a file holding the base64 private
//!   key (32-byte seed or 64-byte libsodium secret key).

use std::io::{Read, Write};
use std::process::ExitCode;

use covenant_robinhood::signer::{RobinhoodSigner, SignRequest};

fn main() -> ExitCode {
    match run() {
        Ok(out) => {
            let mut stdout = std::io::stdout();
            if stdout
                .write_all(out.as_bytes())
                .and_then(|_| stdout.write_all(b"\n"))
                .is_err()
            {
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("covenant-robinhood-signer: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<String, Box<dyn std::error::Error>> {
    let api_key = std::env::var("COVENANT_ROBINHOOD_API_KEY")
        .map_err(|_| "COVENANT_ROBINHOOD_API_KEY is not set")?;
    let keypair_path = std::env::var("COVENANT_ROBINHOOD_KEYPAIR")
        .map_err(|_| "COVENANT_ROBINHOOD_KEYPAIR is not set")?;
    let private_key_b64 =
        std::fs::read_to_string(&keypair_path).map_err(|e| format!("read {keypair_path}: {e}"))?;
    let signer = RobinhoodSigner::from_base64_key(api_key, &private_key_b64)?;

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let req: SignRequest = serde_json::from_str(input.trim())
        .map_err(|e| format!("decode SignRequest from stdin: {e}"))?;

    Ok(serde_json::to_string(&signer.sign_request(&req))?)
}
