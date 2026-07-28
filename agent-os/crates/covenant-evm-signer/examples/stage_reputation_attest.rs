//! Regenerate the staged Base Sepolia EAS reputation `attest` artifact
//! (`agent-os/autonomy/multichain/staging/reputation-attest-base-sepolia.json`)
//! from `attest_calldata`, so every byte of the staged transaction comes from
//! the crate's encoder rather than hand assembly.
//!
//!   cargo run -p covenant-evm-signer --example stage_reputation_attest
//!   cargo run -p covenant-evm-signer --example stage_reputation_attest -- --out <path>
//!
//! Deterministic by construction: no key, no RPC, no clock. The projection is
//! the reviewed staging fixture — score 0.95 at 4 decimals, the pinned
//! validity window, the Solana mainnet CAIP-2 chain id, and the `0xab…ab`
//! placeholder PDA that keeps the artifact visibly not-submission-ready until
//! a real Solana anchor is staged.

use covenant_evm_signer::{
    attest_calldata, reputation_schema_uid, ReputationProjection, ReputationScore,
    ATTEST_SIGNATURE, RELAY_MAX_DATA_BYTES, SOLANA_MAINNET_CAIP2,
};
use serde_json::json;

fn hex_0x(bytes: &[u8]) -> String {
    let mut s = String::from("0x");
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn main() {
    let projection = ReputationProjection::new(
        ReputationScore::from_ratio(95, 100, 4).expect("staging score"),
        SOLANA_MAINNET_CAIP2,
        [0xAB; 32],
        1_700_000_000,
        1_800_000_000,
    );
    let calldata = attest_calldata(&projection).expect("staging projection encodes");

    let artifact = json!({
        "abiSourceCommit": "0c51c77cccd68e19ddbfeb832f153e75fac1af19",
        "chainId": 84532,
        "data": hex_0x(&calldata),
        "expirationTime": projection.expiry_unix,
        "function": ATTEST_SIGNATURE,
        "network": "Base Sepolia",
        "notes": [
            "Unsigned: no signature, nonce, gas, or from. Submission needs an operator-custodied funded key; this crate never holds one.",
            "EAS is an OP-Stack predeploy at 0x4200000000000000000000000000000000000021; re-verify the live contract's attest ABI against this calldata immediately before submission.",
            "recipient is zero: the identity binding is the Solana PDA inside the payload, not an EVM token, so the attestation is non-transferable and cannot be laundered onto a sellable NFT.",
            "revocable=true, expirationTime=1800000000: the score expires on-chain and can be revoked, so a stale attestation does not stay trusted forever.",
            "Cost: at most 24 writes/day. The builder cannot rate-limit; the operator submission layer must enforce this.",
            "Precondition: the reputation schema (0x84738ec346cd136dddd5b09e8df18a3c5cfb2603aaf5a68758c0149aa406cc39) must be registered in the EAS SchemaRegistry on Base Sepolia first, or attest reverts InvalidSchema. That registration is itself a gated on-chain write.",
            "solanaAttestationPda is the 0xab..ab placeholder, not a real Solana anchor: this artifact is NOT submission-ready until the real attestation PDA is staged (multichain-attestation-pda-producer). scripts/validate-reputation-staging.mjs --submission refuses it.",
            "Regenerated 2026-07-28 by examples/stage_reputation_attest.rs: the prior artifact's hand-assembled calldata froze a corrupt 42-byte source_chain; every byte of data now comes from attest_calldata()/encode_data()."
        ],
        "policy": {
            "maxDataBytes": RELAY_MAX_DATA_BYTES,
            "maxWritesPerDay": 24
        },
        "recipient": "0x0000000000000000000000000000000000000000",
        "revocable": true,
        "schemaUID": hex_0x(&reputation_schema_uid()),
        "to": "0x4200000000000000000000000000000000000021",
        "value": "0x0"
    });

    let rendered = serde_json::to_string_pretty(&artifact).expect("artifact serializes") + "\n";

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => print!("{rendered}"),
        [flag, path] if flag == "--out" => {
            std::fs::write(path, &rendered).unwrap_or_else(|e| panic!("write {path}: {e}"));
            eprintln!("wrote {path}");
        }
        _ => {
            eprintln!("usage: stage_reputation_attest [--out <path>]");
            std::process::exit(2);
        }
    }
}
