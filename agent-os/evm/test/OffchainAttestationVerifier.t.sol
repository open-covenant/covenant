// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Test} from "forge-std/Test.sol";
import {OffchainAttestationVerifier} from "../contracts/OffchainAttestationVerifier.sol";

/// Cross-language parity: EAS off-chain reputation and provenance
/// attestations signed by covenant-evm-signer's `eip712.rs` verify here, and
/// their malleable twins revert. Constants are the frozen base-sepolia golden
/// vectors in covenant-evm-signer/tests/fixtures/offchain-attestation.v1.json
/// (fixed key [9;32]), asserted by tests/golden_wire_vectors.rs; re-bless
/// that fixture with COVENANT_BLESS_EVM_SIGNER_GOLDEN=1 and mirror the diff
/// here if the off-chain wire deliberately changes.
contract OffchainAttestationVerifierTest is Test {
    // secp256k1 group order, for building the high-S twin.
    uint256 constant N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141;

    address constant ATTESTOR = 0x58DA990A8F4A3a6ca7cb6315d68a140105917352;
    uint256 constant CHAIN_ID = 84532;
    string constant EAS_VERSION = "1.2.0";

    bytes32 constant ANCHOR = 0x5ed84d69180c43cbb5a3fbc022dddb666b30155ecc0acad29a2e8941d522c8e6;
    string constant SOURCE_CHAIN = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";

    uint8 constant REP_V = 27;
    bytes32 constant REP_R = 0x6e495fe62db01116963287f4fbd1cbbc77c217ccdb881fbbb9944d46bf971a10;
    bytes32 constant REP_S = 0x36739fe4236c8c411a644bb6917dd11741de7cbcc8beebb4f3d35b945a27f79f;
    bytes32 constant REP_DIGEST = 0x46aa3e8f9d35d91fb6df8188f1b223cff6252da161717f1778885ca1eefcb706;

    bytes32 constant AUDIT_ROOT = 0xabcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789;
    bytes32 constant CREDENTIAL_HASH = 0x00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff;

    uint8 constant PROV_V = 27;
    bytes32 constant PROV_R = 0xe8ca693cc6afc2ad091f315e7cf9a93b9796d69c48faecfd6ffa6a85ccf6388a;
    bytes32 constant PROV_S = 0x4dffd56072688111cf54ff547c5abe19a58e7b7c9a86633619a003bec8330295;
    bytes32 constant PROV_DIGEST = 0xc5ca6d01c842cceb70b89f5566d6d01b1108a2e4b09b3ccf4842fa4e5c93e3d1;

    // The fixture's verify_reputation_calldata / verify_provenance_calldata:
    // the exact eth_call blobs an operator submits post-deploy, dispatched
    // through solc's generated ABI decoder rather than re-encoded here.
    bytes constant VERIFY_REPUTATION_CALLDATA =
        hex"3c5d3a87"
        hex"0000000000000000000000000000000000000000000000000000000000000080"
        hex"000000000000000000000000000000000000000000000000000000000000001b"
        hex"6e495fe62db01116963287f4fbd1cbbc77c217ccdb881fbbb9944d46bf971a10"
        hex"36739fe4236c8c411a644bb6917dd11741de7cbcc8beebb4f3d35b945a27f79f"
        hex"000000000000000000000000000000000000000000000000000000000000251c"
        hex"0000000000000000000000000000000000000000000000000000000000000004"
        hex"000000000000000000000000000000000000000000000000000000006b49d200"
        hex"00000000000000000000000000000000000000000000000000000000000000c0"
        hex"5ed84d69180c43cbb5a3fbc022dddb666b30155ecc0acad29a2e8941d522c8e6"
        hex"000000000000000000000000000000000000000000000000000000006553f100"
        hex"0000000000000000000000000000000000000000000000000000000000000027"
        hex"736f6c616e613a3565796b7434557346763850384e4a64545245705931767a71"
        hex"4b715a4b76647000000000000000000000000000000000000000000000000000";
    bytes constant VERIFY_PROVENANCE_CALLDATA =
        hex"4aaf42a6"
        hex"abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
        hex"00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
        hex"000000000000000000000000000000000000000000000000000000006553f100"
        hex"000000000000000000000000000000000000000000000000000000006b49d200"
        hex"000000000000000000000000000000000000000000000000000000000000001b"
        hex"e8ca693cc6afc2ad091f315e7cf9a93b9796d69c48faecfd6ffa6a85ccf6388a"
        hex"4dffd56072688111cf54ff547c5abe19a58e7b7c9a86633619a003bec8330295";

    OffchainAttestationVerifier verifier;

    function setUp() public {
        vm.chainId(CHAIN_ID); // so domainSeparator's block.chainid matches the vectors
        vm.warp(1_750_000_000); // inside [issuedAt/validFrom, expiry/validUntil]
        verifier = new OffchainAttestationVerifier(ATTESTOR, EAS_VERSION);
    }

    function _reputation() internal pure returns (OffchainAttestationVerifier.ReputationRecord memory) {
        return OffchainAttestationVerifier.ReputationRecord({
            score: 9500,
            scoreDecimals: 4,
            expiry: 1_800_000_000,
            sourceChain: SOURCE_CHAIN,
            solanaAttestationPda: ANCHOR,
            issuedAt: 1_700_000_000
        });
    }

    function _provenance() internal pure returns (OffchainAttestationVerifier.ProvenanceRecord memory) {
        return OffchainAttestationVerifier.ProvenanceRecord({
            auditRoot: AUDIT_ROOT,
            credentialHash: CREDENTIAL_HASH,
            validFrom: 1_700_000_000,
            validUntil: 1_800_000_000
        });
    }

    function test_schema_uids_match_the_registered_values() public view {
        assertEq(
            verifier.REPUTATION_SCHEMA_UID(),
            0x84738ec346cd136dddd5b09e8df18a3c5cfb2603aaf5a68758c0149aa406cc39
        );
        assertEq(
            verifier.PROVENANCE_SCHEMA_UID(),
            0x841835e486a461ee145aee188a06343cdf499e2ee41ca668e3e3044e9516ea9f
        );
    }

    function test_domain_separator_matches_the_vector() public view {
        assertEq(verifier.domainSeparator(), 0x624018bb2371d6c455c856a60753986a8faa78de7968185e82df1ebe1b4494be);
    }

    function test_reputation_digest_matches_the_vector() public view {
        assertEq(verifier.reputationDigest(_reputation()), REP_DIGEST);
    }

    function test_provenance_digest_matches_the_vector() public view {
        assertEq(verifier.provenanceDigest(_provenance()), PROV_DIGEST);
    }

    function test_verify_reputation_accepts_the_signed_record() public view {
        assertTrue(verifier.verifyReputation(_reputation(), REP_V, REP_R, REP_S));
    }

    function test_verify_provenance_accepts_the_signed_record() public view {
        assertTrue(verifier.verifyProvenance(_provenance(), PROV_V, PROV_R, PROV_S));
    }

    function test_pinned_reputation_calldata_verifies_via_staticcall() public view {
        (bool ok, bytes memory ret) = address(verifier).staticcall(VERIFY_REPUTATION_CALLDATA);
        assertTrue(ok, "reputation staticcall reverted");
        assertTrue(abi.decode(ret, (bool)));
    }

    function test_pinned_provenance_calldata_verifies_via_staticcall() public view {
        (bool ok, bytes memory ret) = address(verifier).staticcall(VERIFY_PROVENANCE_CALLDATA);
        assertTrue(ok, "provenance staticcall reverted");
        assertTrue(abi.decode(ret, (bool)));
    }

    function test_high_s_twin_is_rejected() public {
        bytes32 highS = bytes32(N - uint256(REP_S));
        uint8 flipped = REP_V == 27 ? 28 : 27;
        vm.expectRevert(OffchainAttestationVerifier.MalleableSignature.selector);
        verifier.verifyReputation(_reputation(), flipped, REP_R, highS);
    }

    function test_bad_v_is_rejected() public {
        vm.expectRevert(OffchainAttestationVerifier.MalleableSignature.selector);
        verifier.verifyReputation(_reputation(), 29, REP_R, REP_S);
    }

    function test_untrusted_attestor_is_rejected() public {
        OffchainAttestationVerifier other = new OffchainAttestationVerifier(address(0xBEEF), EAS_VERSION);
        vm.expectRevert(OffchainAttestationVerifier.UntrustedSigner.selector);
        other.verifyReputation(_reputation(), REP_V, REP_R, REP_S);
    }

    function test_tampered_score_recovers_an_untrusted_signer() public {
        OffchainAttestationVerifier.ReputationRecord memory r = _reputation();
        r.score = 9501;
        vm.expectRevert(OffchainAttestationVerifier.UntrustedSigner.selector);
        verifier.verifyReputation(r, REP_V, REP_R, REP_S);
    }

    function test_expired_reputation_is_rejected() public {
        vm.warp(1_800_000_001);
        vm.expectRevert(OffchainAttestationVerifier.Expired.selector);
        verifier.verifyReputation(_reputation(), REP_V, REP_R, REP_S);
    }

    function test_zero_expiry_is_rejected() public {
        OffchainAttestationVerifier.ReputationRecord memory r = _reputation();
        r.expiry = 0;
        vm.expectRevert(OffchainAttestationVerifier.NeverExpires.selector);
        verifier.verifyReputation(r, REP_V, REP_R, REP_S);
    }

    function test_inverted_reputation_window_is_rejected() public {
        OffchainAttestationVerifier.ReputationRecord memory r = _reputation();
        r.issuedAt = r.expiry;
        vm.expectRevert(OffchainAttestationVerifier.WindowInverted.selector);
        verifier.verifyReputation(r, REP_V, REP_R, REP_S);
    }

    function test_zero_anchor_is_rejected() public {
        OffchainAttestationVerifier.ReputationRecord memory r = _reputation();
        r.solanaAttestationPda = bytes32(0);
        vm.expectRevert(OffchainAttestationVerifier.MissingAnchor.selector);
        verifier.verifyReputation(r, REP_V, REP_R, REP_S);
    }

    function test_placeholder_anchor_is_rejected() public {
        OffchainAttestationVerifier.ReputationRecord memory r = _reputation();
        r.solanaAttestationPda = 0xabababababababababababababababababababababababababababababababab;
        vm.expectRevert(OffchainAttestationVerifier.PlaceholderAnchor.selector);
        verifier.verifyReputation(r, REP_V, REP_R, REP_S);
    }

    function test_empty_source_chain_is_rejected() public {
        OffchainAttestationVerifier.ReputationRecord memory r = _reputation();
        r.sourceChain = "";
        vm.expectRevert(OffchainAttestationVerifier.EmptySourceChain.selector);
        verifier.verifyReputation(r, REP_V, REP_R, REP_S);
    }

    function test_oversized_score_decimals_are_rejected() public {
        OffchainAttestationVerifier.ReputationRecord memory r = _reputation();
        r.scoreDecimals = 19;
        vm.expectRevert(OffchainAttestationVerifier.ScoreScaleOverflow.selector);
        verifier.verifyReputation(r, REP_V, REP_R, REP_S);
    }

    function test_zero_audit_root_is_rejected() public {
        OffchainAttestationVerifier.ProvenanceRecord memory p = _provenance();
        p.auditRoot = bytes32(0);
        vm.expectRevert(OffchainAttestationVerifier.ZeroField.selector);
        verifier.verifyProvenance(p, PROV_V, PROV_R, PROV_S);
    }

    function test_zero_credential_hash_is_rejected() public {
        OffchainAttestationVerifier.ProvenanceRecord memory p = _provenance();
        p.credentialHash = bytes32(0);
        vm.expectRevert(OffchainAttestationVerifier.ZeroField.selector);
        verifier.verifyProvenance(p, PROV_V, PROV_R, PROV_S);
    }

    function test_expired_provenance_is_rejected() public {
        vm.warp(1_800_000_001);
        vm.expectRevert(OffchainAttestationVerifier.Expired.selector);
        verifier.verifyProvenance(_provenance(), PROV_V, PROV_R, PROV_S);
    }

    function test_never_expiring_provenance_is_rejected() public {
        OffchainAttestationVerifier.ProvenanceRecord memory p = _provenance();
        p.validUntil = 0;
        vm.expectRevert(OffchainAttestationVerifier.NeverExpires.selector);
        verifier.verifyProvenance(p, PROV_V, PROV_R, PROV_S);
    }

    function test_inverted_provenance_window_is_rejected() public {
        OffchainAttestationVerifier.ProvenanceRecord memory p = _provenance();
        p.validFrom = p.validUntil;
        vm.expectRevert(OffchainAttestationVerifier.WindowInverted.selector);
        verifier.verifyProvenance(p, PROV_V, PROV_R, PROV_S);
    }

    function test_tampered_audit_root_recovers_an_untrusted_signer() public {
        OffchainAttestationVerifier.ProvenanceRecord memory p = _provenance();
        p.auditRoot = bytes32(uint256(AUDIT_ROOT) ^ 1);
        vm.expectRevert(OffchainAttestationVerifier.UntrustedSigner.selector);
        verifier.verifyProvenance(p, PROV_V, PROV_R, PROV_S);
    }
}
