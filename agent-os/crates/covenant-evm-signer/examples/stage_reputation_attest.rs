//! Regenerate the staged Base Sepolia EAS reputation `attest` artifact
//! (`agent-os/autonomy/multichain/staging/reputation-attest-base-sepolia.json`)
//! from `attest_calldata`, so every byte of the staged transaction comes from
//! the crate's encoder rather than hand assembly.
//!
//!   cargo run -p covenant-evm-signer --example stage_reputation_attest
//!   cargo run -p covenant-evm-signer --example stage_reputation_attest -- --out <path>
//!   cargo run -p covenant-evm-signer --example stage_reputation_attest -- --pda <base58>
//!
//! Deterministic by construction: no key, no RPC, no clock. The projection is
//! the reviewed staging fixture — score 0.95 at 4 decimals, the pinned
//! validity window, the Solana mainnet CAIP-2 chain id — anchored to the live
//! audit-root attestation asset recorded in `docs/metaplex-integration.md`.
//! `--pda` re-anchors to a different base58 account (a newer attestation
//! asset, or a SAP attestation PDA) without touching this source.

use covenant_evm_signer::{
    attest_calldata, reputation_schema_uid, solana_account_bytes, ReputationProjection,
    ReputationScore, ATTEST_SIGNATURE, RELAY_MAX_DATA_BYTES, SOLANA_MAINNET_CAIP2,
};
use serde_json::json;

/// The production audit-root attestation asset (MPL Core AppData) the
/// reputation score's provenance traces to — the "Audit-root attestation
/// (ERC-8004 v2)" row of `docs/metaplex-integration.md`, live on Solana
/// mainnet and verifiable through any DAS endpoint.
const LIVE_ATTESTATION_ASSET: &str = "7PEd79CG1hFUU9qeBnAKmyA77YWzckd572qsYdq3W3GH";

fn hex_0x(bytes: &[u8]) -> String {
    let mut s = String::from("0x");
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out: Option<&str> = None;
    let mut pda: &str = LIVE_ATTESTATION_ASSET;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match (flag.as_str(), it.next()) {
            ("--out", Some(path)) => out = Some(path),
            ("--pda", Some(base58)) => pda = base58,
            _ => {
                eprintln!("usage: stage_reputation_attest [--out <path>] [--pda <base58>]");
                std::process::exit(2);
            }
        }
    }

    let anchor = solana_account_bytes(pda)
        .unwrap_or_else(|e| panic!("--pda must be a 32-byte base58 Solana account: {e}"));
    let projection = ReputationProjection::new(
        ReputationScore::from_ratio(95, 100, 4).expect("staging score"),
        SOLANA_MAINNET_CAIP2,
        anchor,
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
            "recipient is zero: the identity binding is the Solana account inside the payload, not an EVM token, so the attestation is non-transferable and cannot be laundered onto a sellable NFT.",
            "revocable=true, expirationTime=1800000000: the score expires on-chain and can be revoked, so a stale attestation does not stay trusted forever.",
            "Cost: at most 24 writes/day. The builder cannot rate-limit; the operator submission layer must enforce this.",
            "Precondition: the reputation schema (0x84738ec346cd136dddd5b09e8df18a3c5cfb2603aaf5a68758c0149aa406cc39) must be registered in the EAS SchemaRegistry on Base Sepolia first, or attest reverts InvalidSchema. That registration is itself a gated on-chain write.",
            format!("solanaAttestationPda is {pda} — the live audit-root attestation asset (MPL Core AppData) recorded in docs/metaplex-integration.md, base58-decoded to the schema's bytes32 by solana_account_bytes(). Re-anchor with --pda. Projection validation refuses all-zero and repeated-byte placeholder patterns."),
            "Regenerated 2026-08-02 by examples/stage_reputation_attest.rs: the prior artifact carried the 0xab..ab placeholder anchor (multichain-attestation-pda-producer)."
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
    match out {
        None => print!("{rendered}"),
        Some(path) => {
            std::fs::write(path, &rendered).unwrap_or_else(|e| panic!("write {path}: {e}"));
            eprintln!("wrote {path}");
        }
    }
}
