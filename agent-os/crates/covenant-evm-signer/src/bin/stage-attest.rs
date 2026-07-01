//! `stage-attest` — emit an unsigned EAS `attest` transaction for a reputation
//! projection, as reproducible JSON an operator reviews and submits.
//!
//!   stage-attest [--chain base-sepolia|base] < projection.json
//!
//! Reads a reputation projection on stdin (the shape the signer sidecar's
//! `reputation` mode reads) and writes the staged transaction to stdout. Unlike
//! the signer sidecar, it loads no key: it never signs and never submits. Base
//! Sepolia is the only autonomous target; `--chain base` returns the mainnet
//! gate error.

use std::io::Read;
use std::process::ExitCode;

use covenant_evm_signer::{
    parse_reputation_projection, stage_reputation_attestation, RelayPolicy, BASE_MAINNET,
    BASE_SEPOLIA,
};

fn main() -> ExitCode {
    let mut chain = BASE_SEPOLIA;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--chain" => {
                chain = match args.next().as_deref() {
                    Some("base-sepolia") => BASE_SEPOLIA,
                    Some("base") | Some("base-mainnet") => BASE_MAINNET,
                    Some(other) => return fail(&format!("unknown --chain {other:?}")),
                    None => return fail("--chain needs a value"),
                };
            }
            "-h" | "--help" => {
                eprintln!("usage: stage-attest [--chain base-sepolia|base] < projection.json");
                return ExitCode::SUCCESS;
            }
            other => return fail(&format!("unexpected argument {other:?}")),
        }
    }

    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return fail("failed to read stdin");
    }

    let projection = match serde_json::from_str(input.trim())
        .map_err(|e| e.to_string())
        .and_then(|v| parse_reputation_projection(&v).map_err(|e| e.to_string()))
    {
        Ok(p) => p,
        Err(e) => return fail(&e),
    };

    match stage_reputation_attestation(&projection, chain, RelayPolicy::default()) {
        Ok(tx) => {
            println!("{}", serde_json::to_string_pretty(&tx.to_json()).unwrap());
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e.to_string()),
    }
}

fn fail(message: &str) -> ExitCode {
    eprintln!("stage-attest: {message}");
    ExitCode::FAILURE
}
