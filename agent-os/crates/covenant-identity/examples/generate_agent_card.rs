//! Emit the unsigned Covenant Foundation agent card — the canonical content
//! for `https://opencovenant.org/agents/covenant-foundation.json` (the
//! `agentURI` of Base mainnet ERC-8004 agentId 58403).
//!
//!   cargo run -p covenant-identity --example generate_agent_card
//!   cargo run -p covenant-identity --example generate_agent_card -- --out <path>
//!
//! Deterministic: no key, no network, no clock. Signing the card
//! (`signatures`) is a separate step with the operator-held Foundation
//! identity key, and replacing the served file is a deliberate release step:
//! the registered agentURI serves whatever sits at that path.

use covenant_identity::covenant_foundation_card;

fn main() {
    let card = covenant_foundation_card();
    let rendered = serde_json::to_string_pretty(&card).expect("card serializes") + "\n";

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => print!("{rendered}"),
        [flag, path] if flag == "--out" => {
            std::fs::write(path, &rendered).unwrap_or_else(|e| panic!("write {path}: {e}"));
            eprintln!("wrote {path}");
        }
        _ => {
            eprintln!("usage: generate_agent_card [--out <path>]");
            std::process::exit(2);
        }
    }
}
