use covenant_erc8004_register::*;

/// Golden vector whose provenance is a live Base Sepolia `eth_call`: this exact calldata
/// simulated cleanly against IdentityRegistry `0x8004A818..` and returned a `uint256` agentId
/// (re-run by the ignored test in `live_dry_run.rs`). This offline test locks the encoder to
/// that on-chain-accepted vector; it does not itself reach the network.
const DRY_RUN_URI: &str = "ipfs://covenant-agent-card-dry-run";
const DRY_RUN_CALLDATA: &str = "f2c298be\
0000000000000000000000000000000000000000000000000000000000000020\
0000000000000000000000000000000000000000000000000000000000000022\
697066733a2f2f636f76656e616e742d6167656e742d636172642d6472792d72756e\
000000000000000000000000000000000000000000000000000000000000";

#[test]
fn register_selector_matches_pinned_value() {
    assert_eq!(register_string_selector(), [0xf2, 0xc2, 0x98, 0xbe]);
    assert_eq!(selector("register()"), [0x1a, 0xa3, 0xa0, 0x08]);
}

#[test]
fn register_calldata_matches_live_dry_run_vector() {
    assert_eq!(
        hex::encode(encode_register_call(DRY_RUN_URI)),
        DRY_RUN_CALLDATA
    );
}

#[test]
fn string_encoding_pads_to_word_boundary() {
    // 4-byte selector + offset word + length word + n data words.
    assert_eq!(encode_register_call("").len(), 4 + 32 + 32); // empty: zero data words
    assert_eq!(
        encode_register_call(&"a".repeat(32)).len(),
        4 + 32 + 32 + 32
    ); // exact word
    assert_eq!(
        encode_register_call(&"a".repeat(33)).len(),
        4 + 32 + 32 + 64
    ); // spills to two words
}

#[test]
fn pinned_addresses_are_checksummed_and_distinct_per_environment() {
    for d in DEPLOYMENTS {
        assert!(
            is_eip55_checksummed(d.identity_registry),
            "{} not EIP-55",
            d.network
        );
    }
    // Vanity CREATE2 gives one address per environment across chains.
    assert_eq!(
        BASE_SEPOLIA.identity_registry,
        ETHEREUM_SEPOLIA.identity_registry
    );
    assert_ne!(
        BASE_MAINNET.identity_registry,
        BASE_SEPOLIA.identity_registry
    );
}

#[test]
fn bad_checksum_is_rejected() {
    assert!(!is_eip55_checksummed(
        "0x8004a169fb4a3325136eb29fa0ceb6d2e539a432"
    )); // all-lower
    assert!(!is_eip55_checksummed(
        "8004A169FB4a3325136EB29fA0ceB6D2e539a432"
    )); // no 0x
    assert!(!is_eip55_checksummed("0x1234")); // wrong length
}

#[test]
fn deployment_lookup() {
    assert_eq!(deployment_for_chain(8453), Some(BASE_MAINNET));
    assert_eq!(deployment_for_chain(84532), Some(BASE_SEPOLIA));
    assert_eq!(deployment_for_chain(1), None);
}

#[test]
fn agent_uri_immutability_gate() {
    assert_eq!(
        classify_agent_uri("ipfs://bafy..."),
        AgentUriClass::ContentAddressed
    );
    assert_eq!(
        classify_agent_uri("ar://tx"),
        AgentUriClass::ContentAddressed
    );
    assert_eq!(
        classify_agent_uri("data:application/json,{}"),
        AgentUriClass::ContentAddressed
    );
    assert_eq!(
        classify_agent_uri("https://covenant/agent.json"),
        AgentUriClass::MutableLocation
    );

    assert!(ensure_immutable_agent_uri("ipfs://bafy...").is_ok());
    assert!(ensure_immutable_agent_uri("https://covenant/agent.json").is_err());
    assert!(ensure_immutable_agent_uri("   ").is_err());
}

#[test]
fn staged_call_is_unsigned_zero_value_and_content_addressed() {
    let call = stage_register_call(BASE_MAINNET, DRY_RUN_URI).unwrap();
    assert_eq!(call.chain_id, 8453);
    assert_eq!(call.to, BASE_MAINNET.identity_registry);
    assert_eq!(call.value, "0x0");
    assert_eq!(call.function, "register(string)");
    assert_eq!(call.data, format!("0x{DRY_RUN_CALLDATA}"));
    assert_eq!(call.source_commit, DEPLOYMENTS_SOURCE_COMMIT);

    // A mutable agentURI must never reach a staged registration.
    assert!(stage_register_call(BASE_MAINNET, "https://covenant/agent.json").is_err());
}

/// The Rust constants and the machine-readable `deployments.json` must not drift apart.
#[test]
fn deployments_json_matches_constants() {
    let doc: serde_json::Value = serde_json::from_str(include_str!("../deployments.json")).unwrap();

    assert_eq!(doc["source"]["commit"], DEPLOYMENTS_SOURCE_COMMIT);
    assert_eq!(doc["register"]["signature"], REGISTER_STRING_SIGNATURE);
    assert_eq!(doc["register"]["selector"], "0xf2c298be");

    let registry = &doc["identityRegistry"];
    for d in DEPLOYMENTS {
        let entry = &registry[d.chain_id.to_string()];
        assert_eq!(
            entry["network"], d.network,
            "network mismatch for {}",
            d.chain_id
        );
        assert_eq!(
            entry["address"], d.identity_registry,
            "address mismatch for {}",
            d.chain_id
        );
    }
}
