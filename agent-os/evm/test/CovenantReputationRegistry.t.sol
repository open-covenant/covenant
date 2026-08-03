// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Test} from "forge-std/Test.sol";
import {CovenantReputationRegistry} from "../contracts/CovenantReputationRegistry.sol";

/// Cross-language parity: a reputation score signed by covenant-attestation's
/// `reputation.rs` verifies here, its malleable twins revert, and the thin
/// store keeps the freshest score. Constants are the golden vector from
/// `cargo run -p covenant-attestation --example reputation_vector` (fixed key
/// [7;32]). Regenerate them if reputation.rs changes.
///
/// Note the absence of `vm.chainId`: the domain is chain-agnostic (constant
/// salt), so the same signature verifies regardless of `block.chainid` — the
/// property that lets one score be posted on 4663, Base, and anywhere else.
contract CovenantReputationRegistryTest is Test {
    uint256 constant N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141;

    address constant ATTESTOR = 0x4a62316623ad457F02cDC5D997deD67a383EC569;

    bytes32 constant SUBJECT = 0xabababababababababababababababababababababababababababababababab;
    uint32 constant SCORE = 9_500;
    uint8 constant SCORE_DECIMALS = 4;
    uint64 constant VALID_UNTIL = 1_700_003_600;
    bytes32 constant SOLANA_ATTESTATION =
        0x2222222222222222222222222222222222222222222222222222222222222222;

    bytes32 constant DOMAIN_SEPARATOR =
        0xa1810486e59f4b39150c8c9cf9944cf3cf07150d1371650d7eb96d1b71e562fb;
    bytes32 constant DIGEST = 0xe9a0fe2c860337d88659e7a68324eb92f8942f0ca36a0742aa3c45ee4dcccef5;

    uint8 constant V = 27;
    bytes32 constant R = 0x4f5e4d2e809c9ff7ee74cce4887e688eba6adaaab0230e5f3705fecc651d76e6;
    bytes32 constant S = 0x7a4ab58833001e88195ac082ab63b63d2c5035c8ee9be9bf4e0c19b04626439e;

    event ReputationPosted(
        bytes32 indexed subject,
        uint32 score,
        uint8 scoreDecimals,
        uint64 validUntil,
        bytes32 solanaAttestation
    );

    function _rep() internal pure returns (CovenantReputationRegistry.Reputation memory) {
        return CovenantReputationRegistry.Reputation({
            subject: SUBJECT,
            score: SCORE,
            scoreDecimals: SCORE_DECIMALS,
            validUntil: VALID_UNTIL,
            sourceChain: "solana",
            solanaAttestation: SOLANA_ATTESTATION
        });
    }

    function setUp() public {
        vm.warp(1_700_000_000); // before validUntil
    }

    function test_Golden_ReputationDigest() public {
        CovenantReputationRegistry reg = new CovenantReputationRegistry(ATTESTOR);
        assertEq(reg.domainSeparator(), DOMAIN_SEPARATOR);
        assertEq(reg.digest(_rep()), DIGEST);
    }

    function test_verify_accepts_and_returns_score() public {
        CovenantReputationRegistry reg = new CovenantReputationRegistry(ATTESTOR);
        (uint32 score, uint8 decimals) = reg.verify(_rep(), V, R, S);
        assertEq(score, SCORE);
        assertEq(decimals, SCORE_DECIMALS);
    }

    function test_verify_rejects_high_s() public {
        CovenantReputationRegistry reg = new CovenantReputationRegistry(ATTESTOR);
        bytes32 highS = bytes32(N - uint256(S));
        uint8 flipped = V == 27 ? 28 : 27;
        vm.expectRevert(CovenantReputationRegistry.MalleableSignature.selector);
        reg.verify(_rep(), flipped, R, highS);
    }

    function test_verify_rejects_bad_v() public {
        CovenantReputationRegistry reg = new CovenantReputationRegistry(ATTESTOR);
        vm.expectRevert(CovenantReputationRegistry.MalleableSignature.selector);
        reg.verify(_rep(), 29, R, S);
    }

    function test_verify_rejects_untrusted_signer() public {
        CovenantReputationRegistry reg = new CovenantReputationRegistry(address(0xBEEF));
        vm.expectRevert(CovenantReputationRegistry.UntrustedSigner.selector);
        reg.verify(_rep(), V, R, S);
    }

    function test_verify_rejects_expired() public {
        CovenantReputationRegistry reg = new CovenantReputationRegistry(ATTESTOR);
        vm.warp(uint256(VALID_UNTIL) + 1);
        vm.expectRevert(CovenantReputationRegistry.Expired.selector);
        reg.verify(_rep(), V, R, S);
    }

    function test_postReputation_stores_and_emits() public {
        CovenantReputationRegistry reg = new CovenantReputationRegistry(ATTESTOR);
        vm.expectEmit(true, false, false, true);
        emit ReputationPosted(SUBJECT, SCORE, SCORE_DECIMALS, VALID_UNTIL, SOLANA_ATTESTATION);
        reg.postReputation(_rep(), V, R, S);

        (uint32 score, uint8 decimals, uint64 validUntil, bytes32 anchor, uint64 postedAt) =
            reg.latest(SUBJECT);
        assertEq(score, SCORE);
        assertEq(decimals, SCORE_DECIMALS);
        assertEq(validUntil, VALID_UNTIL);
        assertEq(anchor, SOLANA_ATTESTATION);
        assertEq(postedAt, 1_700_000_000);
    }

    function test_postReputation_rejects_stale() public {
        CovenantReputationRegistry reg = new CovenantReputationRegistry(ATTESTOR);
        reg.postReputation(_rep(), V, R, S);
        // Re-posting the same score (equal validUntil) is not fresher.
        vm.expectRevert(CovenantReputationRegistry.StaleUpdate.selector);
        reg.postReputation(_rep(), V, R, S);
    }
}
