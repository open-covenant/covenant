//! Live "Covenant Verified" check against mainnet DAS. No key, no Covenant
//! infra — exactly what Metaplex's directory (or anyone) would run.
//!   COVENANT_METAPLEX_DAS_URL=<helius mainnet> cargo run -p covenant-metaplex --example live_verify
use covenant_metaplex::{
    verify::default_authority, verify_agent, verify_attestation, DasClient, HttpDasClient,
};

#[tokio::main]
async fn main() {
    let das_url = std::env::var("COVENANT_METAPLEX_DAS_URL")
        .expect("set COVENANT_METAPLEX_DAS_URL to a mainnet DAS endpoint");
    let das = HttpDasClient::new(das_url);
    let agent = "4XtUrwvPWAzMGnsKenMpTMATXN3e2quJV11Jg2dab2dc";
    let attestation = "7PEd79CG1hFUU9qeBnAKmyA77YWzckd572qsYdq3W3GH";

    let att = das.get_asset(attestation).await.expect("getAsset");
    let av = verify_attestation(&att, default_authority());
    println!("=== attestation {attestation} ===");
    println!("{}", serde_json::to_string_pretty(&av).unwrap());

    let agent_v = verify_agent(&das, agent, default_authority())
        .await
        .expect("verify_agent");
    println!("=== agent {agent} ===");
    println!("{}", serde_json::to_string_pretty(&agent_v).unwrap());
}
