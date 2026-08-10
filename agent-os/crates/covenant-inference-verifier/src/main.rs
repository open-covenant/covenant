//! `covenant-inference-verifier <tee_url> <challenge_base64>`
//!
//! Fetches and DCAP-verifies a MagicBlock ER quote for the given challenge and
//! prints the [`DcapResult`](covenant_inference_verifier::DcapResult) as JSON on
//! stdout — the exact camelCase shape `covenantd` reads back off the wire.

use std::process::ExitCode;

use covenant_inference_verifier::verify_quote;

#[tokio::main]
async fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(tee_url), Some(challenge_base64)) = (args.next(), args.next()) else {
        eprintln!("usage: covenant-inference-verifier <tee_url> <challenge_base64>");
        return ExitCode::from(2);
    };

    match verify_quote(&tee_url, &challenge_base64).await {
        Ok(dcap) => {
            // to_string_pretty over a struct that can always serialize.
            println!("{}", serde_json::to_string_pretty(&dcap).unwrap());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("verification failed: {e}");
            ExitCode::FAILURE
        }
    }
}
