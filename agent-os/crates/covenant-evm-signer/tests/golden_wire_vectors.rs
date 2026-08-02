//! Golden conformance vectors for the EVM-signer wire artifacts.
//!
//! Two fixtures freeze every byte an external consumer sees:
//!
//! - `tests/fixtures/reputation-attest.v1.json` — the EAS `attest` calldata
//!   (and its ABI-encoded schema data) for one fixed reputation projection
//!   anchored to the live Solana audit-root attestation account. The relay
//!   submits these bytes verbatim to the EAS contract, so a change to the
//!   schema string, tuple layout, or offset arithmetic fails here instead of
//!   producing attestations no indexer can decode.
//! - `tests/fixtures/resolver-ccip.v1.json` — one signed CCIP-Read gateway
//!   response for a fixed `addr(node, 501)` query. The Solidity parity test
//!   (`agent-os/evm/test/OffchainResolver.t.sol`) embeds this vector's
//!   request, result, response, and signer, making it a cross-language
//!   contract with the deployed resolver's `resolveWithProof` callback.
//!
//! Blessing cannot freeze a broken encoder unnoticed: the attest calldata is
//! re-derived from retyped constants and independent word packing, and the
//! CCIP leg re-verifies digest, signature, and ABI envelope from fixture
//! bytes alone.
//!
//! Regenerate deliberately, only after reviewing the resulting diff:
//!
//! ```text
//! COVENANT_BLESS_EVM_SIGNER_GOLDEN=1 cargo test -p covenant-evm-signer \
//!   --test golden_wire_vectors
//! ```

use std::path::PathBuf;

use covenant_evm_signer::{
    attest_calldata, attest_selector, encode_solana_addr_request, reputation_schema_uid,
    solana_account_bytes, ReputationProjection, ReputationScore, ResolverGateway, ATTEST_SIGNATURE,
    REPUTATION_SCHEMA, SOLANA_MAINNET_CAIP2,
};
use covenant_identity::Secp256k1IssuerKey;
use k256::ecdsa::{RecoveryId, Signature as EcdsaSignature, VerifyingKey};
use serde_json::{json, Value};
use sha3::{Digest, Keccak256};

/// Set to (re)generate both committed fixtures instead of asserting.
const BLESS_ENV: &str = "COVENANT_BLESS_EVM_SIGNER_GOLDEN";

const REPUTATION_DRIFT: &str = "reputation attest wire drift: \
    tests/fixtures/reputation-attest.v1.json freezes the exact EAS calldata the relay \
    submits on chain. A mismatch means the schema string, tuple layout, offsets, or \
    anchor encoding changed, and existing attestations would no longer decode under the \
    registered schema. Update the fixture only as a deliberate, reviewed wire change \
    (bump the .v<n> suffix for an incompatible shape) — never blindly regenerate to \
    silence this test.";

const RESOLVER_DRIFT: &str = "resolver CCIP wire drift: \
    tests/fixtures/resolver-ccip.v1.json freezes a signed gateway response whose bytes \
    agent-os/evm/test/OffchainResolver.t.sol verifies with the deployed resolver's \
    proof check. A mismatch means the 0x1900 digest preimage, signature form, or ABI \
    envelope changed, and live resolvers would reject every gateway response. Update \
    the fixture only as a deliberate, reviewed wire change (bump the .v<n> suffix for \
    an incompatible shape) — never blindly regenerate to silence this test.";

/// The committed reputation fixture's `description`, code-pinned so a
/// hand-edit to the fixture's prose fails the suite and a reword here
/// forces a re-bless.
const REPUTATION_DESCRIPTION: &str = "Frozen golden vector for the EAS reputation attest \
    calldata: one fixed projection (score 9500, 4 decimals, Solana-mainnet source chain, \
    the live audit-root attestation account as anchor) encoded to the exact bytes the \
    relay submits. encoded_data is the ABI tuple the registered schema decodes; \
    attest_calldata wraps it in the attest((bytes32,(address,uint64,bool,bytes32,bytes,\
    uint256))) request. Update only as a deliberate, reviewed wire change — never \
    blindly regenerate to make a failing test pass.";

/// The committed resolver fixture's `description`, code-pinned like the
/// reputation one.
const RESOLVER_DESCRIPTION: &str = "Frozen golden vector for the CCIP-Read gateway: one \
    signed response for addr(node, 501) under the fixed [7; 32] gateway key, carrying \
    the request, ABI-encoded result, 0x1900 signature digest, 65-byte signature, and \
    the full response envelope resolveWithProof decodes. agent-os/evm/test/\
    OffchainResolver.t.sol pins the same bytes, making this a cross-language contract: \
    update only as a deliberate, reviewed wire change — never blindly regenerate to \
    make a failing test pass.";

/// The live Solana audit-root attestation account the projection anchors to
/// (`docs/metaplex-integration.md`, live-accounts table).
const ANCHOR_BASE58: &str = "7PEd79CG1hFUU9qeBnAKmyA77YWzckd572qsYdq3W3GH";

/// The fixed DNS-encoded name in the resolver query: `agent.opencovenant.eth`.
const DNS_NAME: &[u8] = b"\x05agent\x0copencovenant\x03eth\x00";

fn fixture_path(file: &str) -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(file)
}

fn read_fixture(file: &str) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(fixture_path(file)).unwrap_or_else(|error| {
            panic!(
                "missing golden fixture {} ({error}); regenerate with {BLESS_ENV}=1",
                fixture_path(file).display()
            )
        }),
    )
    .expect("golden fixture is valid JSON")
}

fn write_fixture(file: &str, doc: &Value) {
    let mut text = serde_json::to_string_pretty(doc).expect("fixture serializes");
    text.push('\n');
    std::fs::create_dir_all(fixture_path(file).parent().unwrap()).expect("fixtures dir");
    std::fs::write(fixture_path(file), text)
        .unwrap_or_else(|error| panic!("write {}: {error}", fixture_path(file).display()));
}

/// The fixed projection every reputation vector derives from.
fn projection() -> ReputationProjection {
    ReputationProjection::new(
        ReputationScore::from_ratio(95, 100, 4).unwrap(),
        SOLANA_MAINNET_CAIP2,
        solana_account_bytes(ANCHOR_BASE58).unwrap(),
        1_700_000_000,
        1_800_000_000,
    )
}

fn reputation_doc() -> Value {
    let projection = projection();
    let call = attest_calldata(&projection).unwrap();
    let data = &call[4 + 10 * 32..];
    json!({
        "description": REPUTATION_DESCRIPTION,
        "schema": REPUTATION_SCHEMA,
        "schema_uid": hex_0x(&reputation_schema_uid()),
        "attest_signature": ATTEST_SIGNATURE,
        "attest_selector": hex_0x(&attest_selector()),
        "anchor_base58": ANCHOR_BASE58,
        "anchor_hex": hex_0x(&projection.solana_attestation_pda),
        "source_chain": projection.source_chain,
        "score": projection.score.score,
        "score_decimals": projection.score.decimals,
        "issued_at": projection.issued_at_unix,
        "expiry": projection.expiry_unix,
        "encoded_data": hex_0x(data),
        "attest_calldata": hex_0x(&call),
    })
}

#[test]
fn reputation_attest_golden_vectors_are_frozen() {
    let built = reputation_doc();

    if std::env::var_os(BLESS_ENV).is_some() {
        write_fixture("reputation-attest.v1.json", &built);
        return;
    }

    let fixture = read_fixture("reputation-attest.v1.json");
    assert_eq!(built, fixture, "{REPUTATION_DRIFT}");

    // Structural invariants over the committed bytes themselves: the last
    // head word of the calldata is the byte length of the schema data, and
    // the tail is that data verbatim — so the two fixture fields can never
    // drift apart without failing here.
    let call = unhex(fixture["attest_calldata"].as_str().unwrap());
    let data = unhex(fixture["encoded_data"].as_str().unwrap());
    let len_word: [u8; 32] = call[4 + 9 * 32..4 + 10 * 32].try_into().unwrap();
    assert_eq!(
        len_word,
        word(data.len() as u128),
        "{REPUTATION_DRIFT} (calldata length word must equal encoded_data length)",
    );
    assert_eq!(
        &call[4 + 10 * 32..],
        data.as_slice(),
        "{REPUTATION_DRIFT} (calldata tail must be encoded_data verbatim)",
    );
}

/// Rebuild the full attest calldata from scratch — retyped signature string,
/// literal schema UID and anchor bytes, independent word packing — so
/// blessing with a drifted encoder fails even before the fixture is
/// compared. Deliberately shares no constants or helpers with the crate.
#[test]
fn reputation_encoding_re_derives_from_pinned_constants() {
    let selector = &keccak(b"attest((bytes32,(address,uint64,bool,bytes32,bytes,uint256)))")[..4];
    assert_eq!(selector, unhex("0xf17325e7"), "attest selector drifted");

    let uid = unhex("0x84738ec346cd136dddd5b09e8df18a3c5cfb2603aaf5a68758c0149aa406cc39");
    // The anchor account bytes, cross-generated from the base58 form with
    // @solana/web3.js — independent of this workspace's bs58 decoding.
    let anchor = unhex("0x5ed84d69180c43cbb5a3fbc022dddb666b30155ecc0acad29a2e8941d522c8e6");
    let source = b"solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";
    assert_eq!(source.len(), 39, "source chain literal length");

    let mut data = Vec::new();
    data.extend_from_slice(&word(9_500));
    data.extend_from_slice(&word(4));
    data.extend_from_slice(&word(1_800_000_000));
    data.extend_from_slice(&word(160));
    data.extend_from_slice(&anchor);
    data.extend_from_slice(&word(39));
    data.extend_from_slice(source);
    data.extend_from_slice(&[0u8; 25]);

    let mut call = Vec::new();
    call.extend_from_slice(selector);
    call.extend_from_slice(&word(0x20));
    call.extend_from_slice(&uid);
    call.extend_from_slice(&word(0x40));
    call.extend_from_slice(&[0u8; 32]);
    call.extend_from_slice(&word(1_800_000_000));
    call.extend_from_slice(&word(1));
    call.extend_from_slice(&[0u8; 32]);
    call.extend_from_slice(&word(0xc0));
    call.extend_from_slice(&word(0));
    call.extend_from_slice(&word(data.len() as u128));
    call.extend_from_slice(&data);

    assert_eq!(
        attest_calldata(&projection()).unwrap(),
        call,
        "attest calldata no longer matches the from-scratch ABI assembly; a re-bless \
         would freeze the drift as the new contract",
    );
}

/// The fixed gateway every resolver vector derives from: key `[7; 32]`
/// (signer `0x4a62…c569`, the same key the Solidity parity test trusts),
/// resolver `0x11…11`.
fn gateway() -> ResolverGateway {
    ResolverGateway::new(
        Secp256k1IssuerKey::from_secret_bytes(&[7u8; 32]).unwrap(),
        [0x11; 20],
    )
}

fn resolver_doc() -> Value {
    let gateway = gateway();
    let request = encode_solana_addr_request(DNS_NAME, &[0x22; 32]);
    let response = gateway
        .resolve_solana(&request, &[0x33; 32], 1_800_000_000)
        .unwrap();
    json!({
        "description": RESOLVER_DESCRIPTION,
        "signer": hex_0x(&gateway.signer_address()),
        "resolver": hex_0x(&gateway.resolver_address()),
        "node": hex_0x(&[0x22u8; 32]),
        "solana_address": hex_0x(&[0x33u8; 32]),
        "expires": response.expires,
        "dns_name": hex_0x(DNS_NAME),
        "request": hex_0x(&response.request),
        "result": hex_0x(&response.result),
        "digest": hex_0x(&response.digest()),
        "signature": hex_0x(&response.signature),
        "response": hex_0x(&response.abi_encode()),
    })
}

#[test]
fn resolver_ccip_golden_vectors_are_frozen() {
    let built = resolver_doc();

    if std::env::var_os(BLESS_ENV).is_some() {
        write_fixture("resolver-ccip.v1.json", &built);
        return;
    }

    assert_eq!(
        built,
        read_fixture("resolver-ccip.v1.json"),
        "{RESOLVER_DRIFT}",
    );
}

/// The verification `resolveWithProof` performs, run over fixture bytes
/// alone — no gateway, no crate encoder. The committed digest must re-derive
/// from the 0x1900 preimage, the committed signature must recover the
/// committed signer, and the committed response envelope must reassemble
/// from its parts with independent word packing, so a fixture blessed from a
/// broken gateway cannot pass.
#[test]
fn frozen_ccip_response_verifies_from_committed_bytes_alone() {
    if std::env::var_os(BLESS_ENV).is_some() {
        return;
    }
    let fixture = read_fixture("resolver-ccip.v1.json");
    let resolver: [u8; 20] = unhex(fixture["resolver"].as_str().unwrap())
        .try_into()
        .unwrap();
    let request = unhex(fixture["request"].as_str().unwrap());
    let result = unhex(fixture["result"].as_str().unwrap());
    let expires = fixture["expires"].as_u64().expect("expires is an integer");
    let signature = unhex(fixture["signature"].as_str().unwrap());
    assert_eq!(signature.len(), 65, "{RESOLVER_DRIFT} (signature length)");

    let mut preimage = vec![0x19, 0x00];
    preimage.extend_from_slice(&resolver);
    preimage.extend_from_slice(&expires.to_be_bytes());
    preimage.extend_from_slice(&keccak(&request));
    preimage.extend_from_slice(&keccak(&result));
    let digest = keccak(&preimage);
    assert_eq!(
        hex_0x(&digest),
        fixture["digest"].as_str().unwrap(),
        "{RESOLVER_DRIFT} (digest must re-derive from the 0x1900 preimage)",
    );

    let recovered = recover(
        &digest,
        signature[..32].try_into().unwrap(),
        signature[32..64].try_into().unwrap(),
        signature[64],
    );
    assert_eq!(
        hex_0x(&recovered),
        fixture["signer"].as_str().unwrap(),
        "{RESOLVER_DRIFT} (committed signature does not recover the signer)",
    );

    // The result is `abi.encode(bytes)` of the 32-byte Solana address:
    // offset word, length word, then the address — exactly 96 bytes.
    let solana = unhex(fixture["solana_address"].as_str().unwrap());
    let mut expected_result = Vec::new();
    expected_result.extend_from_slice(&word(0x20));
    expected_result.extend_from_slice(&word(32));
    expected_result.extend_from_slice(&solana);
    assert_eq!(
        result, expected_result,
        "{RESOLVER_DRIFT} (result must be abi.encode(bytes) of the Solana address)",
    );

    let mut response = Vec::new();
    response.extend_from_slice(&word(0x60));
    response.extend_from_slice(&word(u128::from(expires)));
    response.extend_from_slice(&word(0x60 + 32 + 96));
    response.extend_from_slice(&word(96));
    response.extend_from_slice(&result);
    response.extend_from_slice(&word(65));
    response.extend_from_slice(&signature);
    response.extend_from_slice(&[0u8; 31]);
    assert_eq!(
        hex_0x(&response),
        fixture["response"].as_str().unwrap(),
        "{RESOLVER_DRIFT} (response envelope must reassemble from its parts)",
    );
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
