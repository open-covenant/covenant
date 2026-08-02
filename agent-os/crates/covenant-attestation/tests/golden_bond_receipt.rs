//! Golden conformance vectors for the signed bond-receipt wire artifacts.
//!
//! `tests/fixtures/bond-receipt.v1.json` freezes, for one fixed receipt on
//! each Base network under the fixed `[7; 32]` attestor key, every byte a
//! verifier consumes: the EIP-712 domain separator and signing digest, the
//! attestor's signature as `(r, s, v)`, and the 128-byte `ecrecover`
//! precompile input. These bytes are the compatibility contract with
//! deployed, unupgradeable-by-us consumers — the Solidity parity test
//! (`agent-os/evm/test/BondReceiptVerifier.t.sol`) embeds the Base Sepolia
//! vector's constants — so a change to a type string, field order, or word
//! packing fails here instead of stranding already-issued receipts on chain.
//!
//! Blessing cannot freeze a broken encoder unnoticed: the digest is
//! re-derived from retyped type strings and independent word packing, and
//! the recovery leg runs over fixture bytes alone.
//!
//! Regenerate deliberately, only after reviewing the resulting diff:
//!
//! ```text
//! COVENANT_BLESS_BOND_RECEIPT_GOLDEN=1 cargo test -p covenant-attestation \
//!   --test golden_bond_receipt
//! ```

use std::path::PathBuf;

use covenant_attestation::{BaseNetwork, BondReceipt};
use covenant_identity::Secp256k1IssuerKey;
use k256::ecdsa::{RecoveryId, Signature as EcdsaSignature, VerifyingKey};
use serde_json::{json, Value};
use sha3::{Digest, Keccak256};

/// Set to (re)generate the committed fixture instead of asserting against it.
const BLESS_ENV: &str = "COVENANT_BLESS_BOND_RECEIPT_GOLDEN";

const DRIFT: &str = "bond receipt wire drift: tests/fixtures/bond-receipt.v1.json is a frozen \
    external conformance contract — BondReceiptVerifier.sol's Foundry test embeds these exact \
    bytes. A mismatch means the EIP-712 domain, type string, field order, word packing, or \
    signing changed, and every already-issued receipt would stop verifying. Update the fixture \
    only as a deliberate, reviewed wire change (bump the .v<n> suffix for an incompatible \
    shape) — never blindly regenerate to silence this test.";

/// The committed fixture's `description`, code-pinned so a hand-edit to the
/// fixture's prose fails the suite and a reword here forces a re-bless.
const DESCRIPTION: &str = "Frozen golden vectors for the signed bond receipt: one fixed \
    receipt per Base network under the fixed [7; 32] attestor key, carrying the EIP-712 \
    domain separator, signing digest, (r, s, v) signature, and 128-byte ecrecover precompile \
    input a verifier consumes. agent-os/evm/test/BondReceiptVerifier.t.sol pins the Base \
    Sepolia vector's constants, making these bytes a cross-language contract: update only as \
    a deliberate, reviewed wire change — never blindly regenerate to make a failing test pass.";

/// The fixed attestor key — the same `[7; 32]` key whose vector the Solidity
/// parity test verifies (attestor `0x4a62…c569`).
fn attestor() -> Secp256k1IssuerKey {
    Secp256k1IssuerKey::from_secret_bytes(&[7u8; 32]).unwrap()
}

/// The fixed receipt, identical to the Solidity test's `_receipt()` literal.
fn receipt(network: BaseNetwork) -> BondReceipt {
    BondReceipt {
        network,
        subject: [0xAB; 32],
        bond_token: network.usdc(),
        bond_amount: 1_000_000,
        agent_return: [0x11; 20],
        slash_beneficiary: [0x22; 20],
        slash_beneficiary_bps: 8_000,
        nonce: [0xCD; 32],
        issued_at: 1_700_000_000,
        expiry: 1_800_000_000,
    }
}

const NETWORKS: [(&str, BaseNetwork); 2] = [
    ("base-sepolia", BaseNetwork::Sepolia),
    ("base-mainnet", BaseNetwork::Mainnet),
];

fn golden_records() -> Vec<Value> {
    NETWORKS
        .iter()
        .map(|(name, network)| {
            let receipt = receipt(*network);
            let signed = receipt.sign(&attestor()).unwrap();
            let sig = signed.signature();
            json!({
                "name": name,
                "chain_id": network.chain_id(),
                "attestor": hex_0x(&attestor().address()),
                "subject": hex_0x(&receipt.subject),
                "bond_token": hex_0x(&receipt.bond_token),
                "bond_amount": receipt.bond_amount.to_string(),
                "agent_return": hex_0x(&receipt.agent_return),
                "slash_beneficiary": hex_0x(&receipt.slash_beneficiary),
                "slash_beneficiary_bps": receipt.slash_beneficiary_bps,
                "nonce": hex_0x(&receipt.nonce),
                "issued_at": receipt.issued_at,
                "expiry": receipt.expiry,
                "domain_separator": hex_0x(&receipt.domain_separator()),
                "digest": hex_0x(&signed.digest()),
                "r": hex_0x(&sig[..32]),
                "s": hex_0x(&sig[32..64]),
                "v": sig[64],
                "ecrecover_calldata": hex_0x(&signed.ecrecover_precompile_calldata()),
            })
        })
        .collect()
}

fn fixture_path() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("bond-receipt.v1.json")
}

fn read_fixture() -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(fixture_path()).unwrap_or_else(|error| {
            panic!(
                "missing golden fixture {} ({error}); regenerate with {BLESS_ENV}=1",
                fixture_path().display()
            )
        }),
    )
    .expect("golden fixture is valid JSON")
}

#[test]
fn bond_receipt_golden_vectors_are_frozen() {
    let records = golden_records();

    if std::env::var_os(BLESS_ENV).is_some() {
        let doc = json!({ "description": DESCRIPTION, "records": records });
        let mut text = serde_json::to_string_pretty(&doc).expect("fixture serializes");
        text.push('\n');
        std::fs::create_dir_all(fixture_path().parent().unwrap()).expect("fixtures dir");
        std::fs::write(fixture_path(), text)
            .unwrap_or_else(|error| panic!("write {}: {error}", fixture_path().display()));
        return;
    }

    let fixture = read_fixture();
    assert_eq!(
        fixture["description"].as_str(),
        Some(DESCRIPTION),
        "{DRIFT} (fixture description must match the code-pinned contract note)",
    );
    let golden = fixture["records"]
        .as_array()
        .expect("fixture.records is an array");
    assert_eq!(records.len(), golden.len(), "{DRIFT} (record count)");
    for (built, committed) in records.iter().zip(golden) {
        assert_eq!(built, committed, "{DRIFT} (record {})", built["name"]);
    }
}

/// The verification a Base contract performs, run over fixture bytes alone —
/// no re-signing, no crate encoder. The committed `(digest, v, r, s)` must
/// recover the committed attestor and the committed precompile input must be
/// exactly `digest ‖ v ‖ r ‖ s`, so a fixture blessed from a broken signer
/// cannot pass.
#[test]
fn frozen_vectors_recover_the_attestor_from_committed_bytes_alone() {
    if std::env::var_os(BLESS_ENV).is_some() {
        return;
    }
    let fixture = read_fixture();
    for record in fixture["records"].as_array().expect("records") {
        let name = record["name"].as_str().unwrap();
        let digest = unhex32(&record["digest"]);
        let r = unhex32(&record["r"]);
        let s = unhex32(&record["s"]);
        let v = u8::try_from(record["v"].as_u64().expect("v is an integer")).unwrap();

        let recovered = recover(&digest, &r, &s, v);
        assert_eq!(
            hex_0x(&recovered),
            record["attestor"].as_str().unwrap(),
            "{DRIFT} (record {name}: committed signature does not recover the attestor)",
        );

        let calldata = unhex(record["ecrecover_calldata"].as_str().unwrap());
        assert_eq!(
            calldata.len(),
            128,
            "{DRIFT} (record {name}: calldata length)"
        );
        assert_eq!(
            &calldata[..32],
            &digest,
            "{DRIFT} (record {name}: calldata digest word)"
        );
        assert!(
            calldata[32..63].iter().all(|b| *b == 0),
            "{DRIFT} (record {name}: v word must be right-aligned)",
        );
        assert_eq!(calldata[63], v, "{DRIFT} (record {name}: calldata v)");
        assert_eq!(&calldata[64..96], &r, "{DRIFT} (record {name}: calldata r)");
        assert_eq!(
            &calldata[96..128],
            &s,
            "{DRIFT} (record {name}: calldata s)"
        );
    }
}

/// Rebuild each digest from scratch — retyped type strings, independent word
/// packing — so blessing with a drifted encoder fails even before the fixture
/// is compared. Deliberately shares no constants or helpers with `bond.rs`.
#[test]
fn digest_re_derives_from_the_pinned_type_strings() {
    for (name, network) in NETWORKS {
        let receipt = receipt(network);

        let mut domain = Vec::new();
        domain.extend_from_slice(&keccak(
            b"EIP712Domain(string name,string version,uint256 chainId)",
        ));
        domain.extend_from_slice(&keccak(b"Covenant Bond Receipt"));
        domain.extend_from_slice(&keccak(b"1"));
        domain.extend_from_slice(&word(receipt.network.chain_id() as u128));

        let mut message = Vec::new();
        message.extend_from_slice(&keccak(
            b"BondReceipt(bytes32 subject,address bondToken,uint256 bondAmount,\
              address agentReturn,address slashBeneficiary,uint256 slashBeneficiaryBps,\
              bytes32 nonce,uint256 issuedAt,uint256 expiry)",
        ));
        message.extend_from_slice(&receipt.subject);
        message.extend_from_slice(&address_word(&receipt.bond_token));
        message.extend_from_slice(&word(receipt.bond_amount));
        message.extend_from_slice(&address_word(&receipt.agent_return));
        message.extend_from_slice(&address_word(&receipt.slash_beneficiary));
        message.extend_from_slice(&word(u128::from(receipt.slash_beneficiary_bps)));
        message.extend_from_slice(&receipt.nonce);
        message.extend_from_slice(&word(u128::from(receipt.issued_at)));
        message.extend_from_slice(&word(u128::from(receipt.expiry)));

        let mut preimage = vec![0x19, 0x01];
        preimage.extend_from_slice(&keccak(&domain));
        preimage.extend_from_slice(&keccak(&message));

        assert_eq!(
            receipt.digest(),
            keccak(&preimage),
            "bond receipt digest for {name} no longer matches the pinned EIP-712 \
             encoding; a re-bless would freeze the drift as the new contract",
        );
    }
}

fn keccak(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Keccak256::digest(bytes));
    out
}

fn word(value: u128) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[16..].copy_from_slice(&value.to_be_bytes());
    out
}

fn address_word(address: &[u8; 20]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[12..].copy_from_slice(address);
    out
}

fn recover(digest: &[u8; 32], r: &[u8; 32], s: &[u8; 32], v: u8) -> [u8; 20] {
    let recid =
        RecoveryId::from_byte(v.checked_sub(27).expect("v is 27 or 28")).expect("recovery id");
    let mut compact = [0u8; 64];
    compact[..32].copy_from_slice(r);
    compact[32..].copy_from_slice(s);
    let sig = EcdsaSignature::from_slice(&compact).expect("signature");
    let key = VerifyingKey::recover_from_prehash(digest, &sig, recid).expect("recover");
    let hash = keccak(&key.to_encoded_point(false).as_bytes()[1..]);
    let mut address = [0u8; 20];
    address.copy_from_slice(&hash[12..]);
    address
}

fn hex_0x(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(2 + bytes.len() * 2);
    out.push_str("0x");
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn unhex(hex: &str) -> Vec<u8> {
    let body = hex.strip_prefix("0x").expect("0x-prefixed hex");
    (0..body.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&body[i..i + 2], 16).expect("hex byte"))
        .collect()
}

fn unhex32(value: &Value) -> [u8; 32] {
    unhex(value.as_str().expect("hex string"))
        .try_into()
        .expect("32 bytes")
}
