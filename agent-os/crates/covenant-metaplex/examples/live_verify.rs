//! Inspect configured-provider observations against the historical Covenant
//! record envelope. This does not authenticate Core accounts or validate claims.
//!   COVENANT_METAPLEX_DAS_URL=<helius mainnet> cargo run -p covenant-metaplex --example live_verify
use covenant_metaplex::{
    inspect_agent_records, inspect_record, verify::default_authority, DasClient, HttpDasClient,
};

#[tokio::main]
async fn main() {
    let das_url = std::env::var("COVENANT_METAPLEX_DAS_URL")
        .expect("set COVENANT_METAPLEX_DAS_URL to a mainnet DAS endpoint");
    let das = HttpDasClient::new(das_url);
    let agent = "4XtUrwvPWAzMGnsKenMpTMATXN3e2quJV11Jg2dab2dc";
    let attestation = "7PEd79CG1hFUU9qeBnAKmyA77YWzckd572qsYdq3W3GH";

    let att = das.get_asset(attestation).await.expect("getAsset");
    let av = inspect_record(&att, default_authority());
    println!("=== provider record observation {attestation} ===");
    println!("{}", serde_json::to_string_pretty(&av).unwrap());

    let agent_v = inspect_agent_records(&das, agent, default_authority())
        .await
        .expect("inspect_agent_records");
    println!("=== provider agent-record observation {agent} ===");
    println!("{}", serde_json::to_string_pretty(&agent_v).unwrap());
}
