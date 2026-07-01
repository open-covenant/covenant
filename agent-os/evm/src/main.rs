//! Emit a staged ERC-8004 `register(string)` call as JSON. The output is the exact,
//! reproducible artifact an operator reviews and submits; this binary never signs or sends.
//!
//!   stage-register --agent-uri ipfs://<cid> [--chain-id 8453]

use std::process::ExitCode;

use covenant_erc8004_register::{deployment_for_chain, stage_register_call};

fn main() -> ExitCode {
    let mut agent_uri = None;
    let mut chain_id = 8453u64;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--agent-uri" => agent_uri = args.next(),
            "--chain-id" => match args.next().and_then(|v| v.parse().ok()) {
                Some(id) => chain_id = id,
                None => return fail("--chain-id needs a numeric value"),
            },
            "-h" | "--help" => {
                eprintln!("usage: stage-register --agent-uri <uri> [--chain-id <id>]");
                return ExitCode::SUCCESS;
            }
            other => return fail(&format!("unexpected argument {other:?}")),
        }
    }

    let Some(agent_uri) = agent_uri else {
        return fail("--agent-uri is required");
    };
    let Some(deployment) = deployment_for_chain(chain_id) else {
        return fail(&format!(
            "no pinned ERC-8004 deployment for chain {chain_id}"
        ));
    };

    match stage_register_call(deployment, &agent_uri) {
        Ok(call) => {
            println!("{}", serde_json::to_string_pretty(&call).unwrap());
            ExitCode::SUCCESS
        }
        Err(err) => fail(&err),
    }
}

fn fail(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::FAILURE
}
