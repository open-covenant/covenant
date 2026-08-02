//! Golden parity for `contracts/OffchainAttestationVerifier.sol` from pure
//! keccak — a third, independent implementation of the EAS off-chain
//! `Attest` v1 digest, sharing no code with covenant-evm-signer (k256) or
//! the @noble generator that cross-checked the frozen fixture
//! (covenant-evm-signer/tests/fixtures/offchain-attestation.v1.json).
//! Everything here re-derives from retyped type strings, literal constants,
//! and by-hand word packing.
//!
//! The two `verify*` blobs are the bytes an operator submits as post-deploy
//! `eth_call` checks. The fixture signer is the `[9; 32]` TEST key, so
//! against a live-attestor deployment these exact blobs must revert
//! `UntrustedSigner` — that revert still proves the digest math and ABI
//! decoding executed — and only a deployment constructed with the test
//! attestor returns true; `test/OffchainAttestationVerifier.t.sol` proves
//! that leg.

use sha3::{Digest, Keccak256};

const REPUTATION_SCHEMA: &str =
    "uint32 score,uint8 score_decimals,uint64 expiry,string source_chain,bytes32 solana_attestation_pda";
const PROVENANCE_SCHEMA: &str = "bytes32 auditRoot,bytes32 credentialHash";

/// The EAS OP-Stack predeploy, identical on Base and Base Sepolia.
const EAS: &str = "4200000000000000000000000000000000000021";

/// The live Solana audit-root attestation account (32 bytes, base58
/// `7PEd79CG1hFUU9qeBnAKmyA77YWzckd572qsYdq3W3GH`).
const ANCHOR: &str = "5ed84d69180c43cbb5a3fbc022dddb666b30155ecc0acad29a2e8941d522c8e6";
const SOURCE: &[u8] = b"solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";

const AUDIT_ROOT: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
const CREDENTIAL_HASH: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

const REP_R: &str = "6e495fe62db01116963287f4fbd1cbbc77c217ccdb881fbbb9944d46bf971a10";
const REP_S: &str = "36739fe4236c8c411a644bb6917dd11741de7cbcc8beebb4f3d35b945a27f79f";
const PROV_R: &str = "e8ca693cc6afc2ad091f315e7cf9a93b9796d69c48faecfd6ffa6a85ccf6388a";
const PROV_S: &str = "4dffd56072688111cf54ff547c5abe19a58e7b7c9a86633619a003bec8330295";

/// fixture verify_reputation_calldata: [offset 0x80, v, r, s] head, then the
/// 6-word tuple head (score, decimals, expiry, string offset 0xc0, pda,
/// issuedAt), length 39, and the padded source chain.
const REP_BLOB: &str = "3c5d3a87\
0000000000000000000000000000000000000000000000000000000000000080\
000000000000000000000000000000000000000000000000000000000000001b\
6e495fe62db01116963287f4fbd1cbbc77c217ccdb881fbbb9944d46bf971a10\
36739fe4236c8c411a644bb6917dd11741de7cbcc8beebb4f3d35b945a27f79f\
000000000000000000000000000000000000000000000000000000000000251c\
0000000000000000000000000000000000000000000000000000000000000004\
000000000000000000000000000000000000000000000000000000006b49d200\
00000000000000000000000000000000000000000000000000000000000000c0\
5ed84d69180c43cbb5a3fbc022dddb666b30155ecc0acad29a2e8941d522c8e6\
000000000000000000000000000000000000000000000000000000006553f100\
0000000000000000000000000000000000000000000000000000000000000027\
736f6c616e613a3565796b7434557346763850384e4a64545245705931767a71\
4b715a4b76647000000000000000000000000000000000000000000000000000";

/// fixture verify_provenance_calldata: the static tuple inlined — root,
/// hash, validFrom, validUntil — then v, r, s.
const PROV_BLOB: &str = "4aaf42a6\
abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789\
00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff\
000000000000000000000000000000000000000000000000000000006553f100\
000000000000000000000000000000000000000000000000000000006b49d200\
000000000000000000000000000000000000000000000000000000000000001b\
e8ca693cc6afc2ad091f315e7cf9a93b9796d69c48faecfd6ffa6a85ccf6388a\
4dffd56072688111cf54ff547c5abe19a58e7b7c9a86633619a003bec8330295";

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

fn unhex(hex: &str) -> Vec<u8> {
    hex::decode(hex).expect("hex literal")
}

/// getUID(schema, no resolver, revocable): keccak256(schema ‖ zero address ‖ 0x01).
fn schema_uid(schema: &str) -> [u8; 32] {
    let mut buf = schema.as_bytes().to_vec();
    buf.extend_from_slice(&[0u8; 20]);
    buf.push(1);
    keccak(&buf)
}

/// The EAS off-chain EIP-712 domain separator, from the retyped domain type.
fn separator(version: &str, chain_id: u128) -> [u8; 32] {
    let mut buf = Vec::new();
    buf.extend_from_slice(&keccak(
        b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
    ));
    buf.extend_from_slice(&keccak(b"EAS Attestation"));
    buf.extend_from_slice(&keccak(version.as_bytes()));
    buf.extend_from_slice(&word(chain_id));
    let mut eas = [0u8; 32];
    eas[12..].copy_from_slice(&unhex(EAS));
    buf.extend_from_slice(&eas);
    keccak(&buf)
}

/// The `Attest` v1 signing digest: version 1, zero recipient/refUID,
/// revocable true, `bytes data` hashed as a dynamic member.
fn digest(
    separator: &[u8; 32],
    schema_uid: &[u8; 32],
    time: u128,
    expiry: u128,
    data: &[u8],
) -> [u8; 32] {
    let mut buf = Vec::new();
    buf.extend_from_slice(&keccak(
        b"Attest(uint16 version,bytes32 schema,address recipient,uint64 time,uint64 expirationTime,bool revocable,bytes32 refUID,bytes data)",
    ));
    buf.extend_from_slice(&word(1));
    buf.extend_from_slice(schema_uid);
    buf.extend_from_slice(&[0u8; 32]);
    buf.extend_from_slice(&word(time));
    buf.extend_from_slice(&word(expiry));
    buf.extend_from_slice(&word(1));
    buf.extend_from_slice(&[0u8; 32]);
    buf.extend_from_slice(&keccak(data));
    let struct_hash = keccak(&buf);

    let mut preimage = vec![0x19, 0x01];
    preimage.extend_from_slice(separator);
    preimage.extend_from_slice(&struct_hash);
    keccak(&preimage)
}

/// The reputation schema data for the fixed projection (score 9500, 4
/// decimals, expiry 1.8e9, Solana-mainnet source chain, live anchor).
fn reputation_data() -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&word(9_500));
    data.extend_from_slice(&word(4));
    data.extend_from_slice(&word(1_800_000_000));
    data.extend_from_slice(&word(160));
    data.extend_from_slice(&unhex(ANCHOR));
    data.extend_from_slice(&word(39));
    data.extend_from_slice(SOURCE);
    data.extend_from_slice(&[0u8; 25]);
    data
}

fn provenance_data() -> Vec<u8> {
    let mut data = unhex(AUDIT_ROOT);
    data.extend_from_slice(&unhex(CREDENTIAL_HASH));
    data
}

#[test]
fn schema_uids_re_derive_from_the_schema_strings() {
    assert_eq!(
        hex::encode(schema_uid(REPUTATION_SCHEMA)),
        "84738ec346cd136dddd5b09e8df18a3c5cfb2603aaf5a68758c0149aa406cc39",
        "registered reputation schema UID"
    );
    assert_eq!(
        hex::encode(schema_uid(PROVENANCE_SCHEMA)),
        "841835e486a461ee145aee188a06343cdf499e2ee41ca668e3e3044e9516ea9f",
        "covenant provenance schema UID"
    );
}

#[test]
fn sepolia_domain_and_digests_re_derive() {
    let sep = separator("1.2.0", 84_532);
    assert_eq!(
        hex::encode(sep),
        "624018bb2371d6c455c856a60753986a8faa78de7968185e82df1ebe1b4494be",
        "base-sepolia domain separator"
    );
    assert_eq!(
        hex::encode(digest(
            &sep,
            &schema_uid(REPUTATION_SCHEMA),
            1_700_000_000,
            1_800_000_000,
            &reputation_data(),
        )),
        "46aa3e8f9d35d91fb6df8188f1b223cff6252da161717f1778885ca1eefcb706",
        "base-sepolia reputation digest"
    );
    assert_eq!(
        hex::encode(digest(
            &sep,
            &schema_uid(PROVENANCE_SCHEMA),
            1_700_000_000,
            1_800_000_000,
            &provenance_data(),
        )),
        "c5ca6d01c842cceb70b89f5566d6d01b1108a2e4b09b3ccf4842fa4e5c93e3d1",
        "base-sepolia provenance digest"
    );
}

#[test]
fn mainnet_domain_and_digests_re_derive() {
    let sep = separator("1.0.1", 8_453);
    assert_eq!(
        hex::encode(sep),
        "21736c2bca21ab458153a66ee6272df1c06c3764c0de323927a4fdf80c4b89ec",
        "base-mainnet domain separator"
    );
    assert_eq!(
        hex::encode(digest(
            &sep,
            &schema_uid(REPUTATION_SCHEMA),
            1_700_000_000,
            1_800_000_000,
            &reputation_data(),
        )),
        "47a6823c49e1a66f080da5dd242da1741600e8346098eb1c90a7be5113ae47a4",
        "base-mainnet reputation digest"
    );
    assert_eq!(
        hex::encode(digest(
            &sep,
            &schema_uid(PROVENANCE_SCHEMA),
            1_700_000_000,
            1_800_000_000,
            &provenance_data(),
        )),
        "e5ee3eed958eae3b8f6bde57fb1393f5c16e6e699483a7e8cad64fa9bc2eff47",
        "base-mainnet provenance digest"
    );
}

#[test]
fn verify_calldata_blobs_are_pinned() {
    let rep_selector = &keccak(
        b"verifyReputation((uint32,uint8,uint64,string,bytes32,uint64),uint8,bytes32,bytes32)",
    )[..4];
    assert_eq!(
        hex::encode(rep_selector),
        "3c5d3a87",
        "verifyReputation selector"
    );
    let mut call = rep_selector.to_vec();
    call.extend_from_slice(&word(0x80));
    call.extend_from_slice(&word(27));
    call.extend_from_slice(&unhex(REP_R));
    call.extend_from_slice(&unhex(REP_S));
    call.extend_from_slice(&word(9_500));
    call.extend_from_slice(&word(4));
    call.extend_from_slice(&word(1_800_000_000));
    call.extend_from_slice(&word(0xc0));
    call.extend_from_slice(&unhex(ANCHOR));
    call.extend_from_slice(&word(1_700_000_000));
    call.extend_from_slice(&word(39));
    call.extend_from_slice(SOURCE);
    call.extend_from_slice(&[0u8; 25]);
    assert_eq!(
        hex::encode(&call),
        REP_BLOB,
        "verifyReputation eth_call blob"
    );

    let prov_selector =
        &keccak(b"verifyProvenance((bytes32,bytes32,uint64,uint64),uint8,bytes32,bytes32)")[..4];
    assert_eq!(
        hex::encode(prov_selector),
        "4aaf42a6",
        "verifyProvenance selector"
    );
    let mut call = prov_selector.to_vec();
    call.extend_from_slice(&unhex(AUDIT_ROOT));
    call.extend_from_slice(&unhex(CREDENTIAL_HASH));
    call.extend_from_slice(&word(1_700_000_000));
    call.extend_from_slice(&word(1_800_000_000));
    call.extend_from_slice(&word(27));
    call.extend_from_slice(&unhex(PROV_R));
    call.extend_from_slice(&unhex(PROV_S));
    assert_eq!(
        hex::encode(&call),
        PROV_BLOB,
        "verifyProvenance eth_call blob"
    );
}
