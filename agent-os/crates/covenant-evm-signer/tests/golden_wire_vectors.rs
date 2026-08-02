//! Golden conformance vectors for the EVM-signer wire artifacts.
//!
//! Three fixtures freeze every byte an external consumer sees:
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
//! - `tests/fixtures/offchain-attestation.v1.json` — the signed EAS
//!   off-chain `Attest` digests (one reputation and one audit-root
//!   provenance record per Base network) that
//!   `agent-os/evm/contracts/OffchainAttestationVerifier.sol` re-derives
//!   on chain; `OffchainAttestationVerifier.t.sol` pins the base-sepolia
//!   records.
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
    attest_calldata, attest_selector, covenant_schema_uid, encode_solana_addr_request,
    offchain_uid, recover_address, reputation_schema_uid, solana_account_bytes, AttestMessage,
    EasAttestationSigner, EasDomain, ReputationProjection, ReputationScore, ResolverGateway,
    ATTEST_SIGNATURE, COVENANT_SCHEMA, REPUTATION_SCHEMA, SOLANA_MAINNET_CAIP2,
};
use covenant_identity::Secp256k1IssuerKey;
use k256::ecdsa::{RecoveryId, Signature as EcdsaSignature, VerifyingKey};
use serde_json::{json, Value};
use sha3::{Digest, Keccak256};

/// Set to (re)generate the committed fixtures instead of asserting.
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

const OFFCHAIN_DRIFT: &str = "offchain attestation wire drift: \
    tests/fixtures/offchain-attestation.v1.json freezes the signed EAS off-chain Attest \
    digests agent-os/evm/contracts/OffchainAttestationVerifier.sol re-derives on chain. \
    A mismatch means the EIP-712 domain, Attest struct layout, or schema data encoding \
    changed, and the deployed verifier would reject every legitimate record. Update the \
    fixture only as a deliberate, reviewed wire change (bump the .v<n> suffix for an \
    incompatible shape) — never blindly regenerate to silence this test.";

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

/// The committed offchain-attestation fixture's `description`, code-pinned
/// like the others.
const OFFCHAIN_DESCRIPTION: &str = "Frozen golden vectors for the EAS off-chain \
    attestations: one reputation and one audit-root provenance record per Base network \
    under the fixed [9; 32] issuer key, carrying the EIP-712 domain separator, signing \
    digest, (r, s, v) signature, offchain UID, and 128-byte ecrecover precompile input. \
    agent-os/evm/test/OffchainAttestationVerifier.t.sol pins the base-sepolia records, \
    making this a cross-language contract: update only as a deliberate, reviewed wire \
    change — never blindly regenerate to make a failing test pass.";

/// The live Solana audit-root attestation account the projection anchors to
/// (`docs/metaplex-integration.md`, live-accounts table).
const ANCHOR_BASE58: &str = "7PEd79CG1hFUU9qeBnAKmyA77YWzckd572qsYdq3W3GH";

/// The fixed DNS-encoded name in the resolver query: `agent.opencovenant.eth`.
const DNS_NAME: &[u8] = b"\x05agent\x0copencovenant\x03eth\x00";

/// A synthetic audit root / credential hash for the provenance records: the
/// digest math is what's pinned here — the VC→attest path that produces
/// these two words from a real credential is proven by the crate's lib
/// tests.
const PROVENANCE_ROOT: &str = "0xabcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
const PROVENANCE_HASH: &str = "0x00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

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

/// The EAS off-chain domain separator from retyped strings and independent
/// word packing — deliberately shares nothing with eip712.rs.
fn scratch_separator(version: &str, chain_id: u64) -> [u8; 32] {
    let mut buf = Vec::new();
    buf.extend_from_slice(&keccak(
        b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
    ));
    buf.extend_from_slice(&keccak(b"EAS Attestation"));
    buf.extend_from_slice(&keccak(version.as_bytes()));
    buf.extend_from_slice(&word(u128::from(chain_id)));
    let mut eas = [0u8; 32];
    eas[12..].copy_from_slice(&unhex("0x4200000000000000000000000000000000000021"));
    buf.extend_from_slice(&eas);
    keccak(&buf)
}

/// The EAS off-chain `Attest` v1 signing digest from scratch: retyped type
/// string, version 1, zero recipient/refUID, revocable true, `bytes data`
/// hashed as a dynamic member.
fn scratch_digest(
    separator: &[u8; 32],
    schema_uid: &[u8; 32],
    time: u64,
    expiration_time: u64,
    data: &[u8],
) -> [u8; 32] {
    let mut buf = Vec::new();
    buf.extend_from_slice(&keccak(
        b"Attest(uint16 version,bytes32 schema,address recipient,uint64 time,uint64 expirationTime,bool revocable,bytes32 refUID,bytes data)",
    ));
    buf.extend_from_slice(&word(1));
    buf.extend_from_slice(schema_uid);
    buf.extend_from_slice(&[0u8; 32]);
    buf.extend_from_slice(&word(u128::from(time)));
    buf.extend_from_slice(&word(u128::from(expiration_time)));
    buf.extend_from_slice(&word(1));
    buf.extend_from_slice(&[0u8; 32]);
    buf.extend_from_slice(&keccak(data));
    let struct_hash = keccak(&buf);

    let mut preimage = vec![0x19, 0x01];
    preimage.extend_from_slice(separator);
    preimage.extend_from_slice(&struct_hash);
    keccak(&preimage)
}

/// One fixture record: the attestation's message fields plus everything a
/// verifier re-derives — separator, digest, signature parts, offchain UID,
/// and the 128-byte ecrecover precompile input `digest ‖ v ‖ r ‖ s`.
#[allow(clippy::too_many_arguments)]
fn record_json(
    kind: &str,
    name: &str,
    chain_id: u64,
    eas_version: &str,
    schema: &str,
    message: &AttestMessage,
    separator: &[u8; 32],
    digest: &[u8; 32],
    signature: &[u8; 65],
    uid: &[u8; 32],
) -> Value {
    let mut ecrecover = Vec::with_capacity(128);
    ecrecover.extend_from_slice(digest);
    ecrecover.extend_from_slice(&word(u128::from(signature[64])));
    ecrecover.extend_from_slice(&signature[..32]);
    ecrecover.extend_from_slice(&signature[32..64]);
    json!({
        "kind": kind,
        "name": name,
        "chain_id": chain_id,
        "eas_version": eas_version,
        "schema": schema,
        "schema_uid": hex_0x(&message.schema),
        "time": message.time,
        "expiration_time": message.expiration_time,
        "data": hex_0x(&message.data),
        "domain_separator": hex_0x(separator),
        "digest": hex_0x(digest),
        "r": hex_0x(&signature[..32]),
        "s": hex_0x(&signature[32..64]),
        "v": signature[64],
        "uid": hex_0x(uid),
        "ecrecover_calldata": hex_0x(&ecrecover),
    })
}

fn offchain_doc() -> Value {
    let issuer = Secp256k1IssuerKey::from_secret_bytes(&[9u8; 32]).unwrap();
    let mut records = Vec::new();
    for (name, chain_id, eas_version, domain) in [
        (
            "base-sepolia",
            84_532u64,
            "1.2.0",
            EasDomain::base_sepolia(),
        ),
        ("base-mainnet", 8_453u64, "1.0.1", EasDomain::base_mainnet()),
    ] {
        let separator = scratch_separator(eas_version, chain_id);

        // The reputation record is signed by eip712.rs while its digest is
        // recomputed from scratch here: recover_address(scratch digest,
        // crate signature) returns the issuer only if the two EIP-712
        // implementations agree, so a bless run bakes crate parity in.
        let attestation = EasAttestationSigner::new(issuer.clone(), domain)
            .attest_reputation(&projection())
            .unwrap();
        let digest = scratch_digest(
            &separator,
            &attestation.message.schema,
            attestation.message.time,
            attestation.message.expiration_time,
            &attestation.message.data,
        );
        assert_eq!(
            recover_address(&digest, &attestation.signature).unwrap(),
            issuer.address(),
            "scratch EIP-712 math and eip712.rs disagree"
        );
        records.push(record_json(
            "reputation",
            name,
            chain_id,
            eas_version,
            REPUTATION_SCHEMA,
            &attestation.message,
            &separator,
            &digest,
            &attestation.signature,
            &attestation.uid,
        ));

        // The provenance record: the audit-root Attest message assembled
        // directly (no fixed-vector VC exists to feed attest()), signed
        // over the same scratch digest and identified by the crate's
        // offchain UID.
        let mut data = unhex(PROVENANCE_ROOT);
        data.extend_from_slice(&unhex(PROVENANCE_HASH));
        let message = AttestMessage {
            schema: covenant_schema_uid(),
            recipient: [0u8; 20],
            time: 1_700_000_000,
            expiration_time: 1_800_000_000,
            revocable: true,
            ref_uid: [0u8; 32],
            data,
        };
        let digest = scratch_digest(
            &separator,
            &message.schema,
            message.time,
            message.expiration_time,
            &message.data,
        );
        let signature = issuer.sign_eip712_digest(&digest);
        assert_eq!(
            recover_address(&digest, &signature).unwrap(),
            issuer.address(),
            "scratch EIP-712 math and sign_eip712_digest disagree"
        );
        let uid = offchain_uid(&message);
        records.push(record_json(
            "provenance",
            name,
            chain_id,
            eas_version,
            COVENANT_SCHEMA,
            &message,
            &separator,
            &digest,
            &signature,
            &uid,
        ));
    }

    json!({
        "description": OFFCHAIN_DESCRIPTION,
        "signer": hex_0x(&issuer.address()),
        "eas": "0x4200000000000000000000000000000000000021",
        "domain_name": "EAS Attestation",
        "records": records,
    })
}

#[test]
fn offchain_attestation_golden_vectors_are_frozen() {
    let built = offchain_doc();

    if std::env::var_os(BLESS_ENV).is_some() {
        write_fixture("offchain-attestation.v1.json", &built);
        return;
    }

    assert_eq!(
        built,
        read_fixture("offchain-attestation.v1.json"),
        "{OFFCHAIN_DRIFT}",
    );
}

/// The checks an on-chain consumer performs, run over fixture bytes alone —
/// no crate encoder: schema UIDs re-derive from the schema strings, the
/// reputation data cross-pins reputation-attest.v1.json's encoded_data, the
/// separator and digest re-derive from retyped constants, the committed
/// signature recovers the committed signer, the offchain UID re-derives
/// from its packed v1 preimage, and the ecrecover calldata reassembles as
/// digest ‖ v ‖ r ‖ s — so a fixture blessed from a broken signer cannot
/// pass.
#[test]
fn frozen_offchain_attestations_verify_from_committed_bytes_alone() {
    if std::env::var_os(BLESS_ENV).is_some() {
        return;
    }
    let fixture = read_fixture("offchain-attestation.v1.json");
    let signer = fixture["signer"].as_str().unwrap();
    let records = fixture["records"].as_array().expect("records array");
    assert_eq!(
        records.len(),
        4,
        "{OFFCHAIN_DRIFT} (two kinds x two networks)"
    );
    let reputation_data = read_fixture("reputation-attest.v1.json")["encoded_data"]
        .as_str()
        .unwrap()
        .to_string();

    for record in records {
        let schema = record["schema"].as_str().unwrap();
        let schema_uid: [u8; 32] = unhex(record["schema_uid"].as_str().unwrap())
            .try_into()
            .unwrap();
        // getUID(schema, no resolver, revocable): keccak(schema ‖ zero20 ‖ 0x01).
        let mut preimage = schema.as_bytes().to_vec();
        preimage.extend_from_slice(&[0u8; 20]);
        preimage.push(1);
        assert_eq!(
            keccak(&preimage),
            schema_uid,
            "{OFFCHAIN_DRIFT} (schema UID must re-derive from the schema string)",
        );

        if record["kind"] == "reputation" {
            assert_eq!(
                record["data"].as_str().unwrap(),
                reputation_data,
                "{OFFCHAIN_DRIFT} (reputation data must equal reputation-attest.v1.json's encoded_data)",
            );
        }

        let data = unhex(record["data"].as_str().unwrap());
        let time = record["time"].as_u64().unwrap();
        let expiry = record["expiration_time"].as_u64().unwrap();
        let separator = scratch_separator(
            record["eas_version"].as_str().unwrap(),
            record["chain_id"].as_u64().unwrap(),
        );
        assert_eq!(
            hex_0x(&separator),
            record["domain_separator"].as_str().unwrap(),
            "{OFFCHAIN_DRIFT} (domain separator must re-derive)",
        );
        let digest = scratch_digest(&separator, &schema_uid, time, expiry, &data);
        assert_eq!(
            hex_0x(&digest),
            record["digest"].as_str().unwrap(),
            "{OFFCHAIN_DRIFT} (digest must re-derive)",
        );

        let r: [u8; 32] = unhex(record["r"].as_str().unwrap()).try_into().unwrap();
        let s: [u8; 32] = unhex(record["s"].as_str().unwrap()).try_into().unwrap();
        let v = u8::try_from(record["v"].as_u64().unwrap()).unwrap();
        assert_eq!(
            hex_0x(&recover(&digest, &r, &s, v)),
            signer,
            "{OFFCHAIN_DRIFT} (committed signature does not recover the signer)",
        );

        // Offchain UID v1: keccak over u16-BE version ‖ the schema UID as
        // its UTF-8 `0x…` string ‖ zero recipient ‖ zero attester ‖
        // time BE8 ‖ expiry BE8 ‖ revocable ‖ zero refUID ‖ data ‖
        // u32-BE bump 0.
        let mut preimage = 1u16.to_be_bytes().to_vec();
        preimage.extend_from_slice(hex_0x(&schema_uid).as_bytes());
        preimage.extend_from_slice(&[0u8; 20]);
        preimage.extend_from_slice(&[0u8; 20]);
        preimage.extend_from_slice(&time.to_be_bytes());
        preimage.extend_from_slice(&expiry.to_be_bytes());
        preimage.push(1);
        preimage.extend_from_slice(&[0u8; 32]);
        preimage.extend_from_slice(&data);
        preimage.extend_from_slice(&0u32.to_be_bytes());
        assert_eq!(
            hex_0x(&keccak(&preimage)),
            record["uid"].as_str().unwrap(),
            "{OFFCHAIN_DRIFT} (offchain UID must re-derive from its packed preimage)",
        );

        let mut calldata = digest.to_vec();
        calldata.extend_from_slice(&word(u128::from(v)));
        calldata.extend_from_slice(&r);
        calldata.extend_from_slice(&s);
        assert_eq!(
            hex_0x(&calldata),
            record["ecrecover_calldata"].as_str().unwrap(),
            "{OFFCHAIN_DRIFT} (ecrecover calldata must be digest ‖ v ‖ r ‖ s)",
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
