// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import {Test} from "forge-std/Test.sol";
import {OffchainResolver} from "../contracts/OffchainResolver.sol";

/// Cross-language parity: a CCIP response signed by covenant-evm-signer's
/// `resolver.rs` verifies in the deployed contract. The constants are the golden
/// vector from `cargo run -p covenant-evm-signer --example resolver_vector`
/// (fixed key [7;32], resolver 0x11..11, node 0x22..22, solana 0x33..33,
/// expires 1_800_000_000). Regenerate them if resolver.rs changes.
contract OffchainResolverTest is Test {
    address constant RESOLVER = 0x1111111111111111111111111111111111111111;
    address constant SIGNER = 0x4a62316623ad457F02cDC5D997deD67a383EC569;

    bytes constant REQUEST =
        hex"9061b923000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000000000000800000000000000000000000000000000000000000000000000000000000000018056167656e740c6f70656e636f76656e616e74036574680000000000000000000000000000000000000000000000000000000000000000000000000000000044f1cb7e06222222222222222222222222222222222222222222222222222222222222222200000000000000000000000000000000000000000000000000000000000001f500000000000000000000000000000000000000000000000000000000";

    bytes constant RESPONSE =
        hex"0000000000000000000000000000000000000000000000000000000000000060000000000000000000000000000000000000000000000000000000006b49d20000000000000000000000000000000000000000000000000000000000000000e0000000000000000000000000000000000000000000000000000000000000006000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000020333333333333333333333333333333333333333333333333333333333333333300000000000000000000000000000000000000000000000000000000000000415fbc28245be60c978c0aa824cc99f998dddeb41d7339eeefb4b1c0db7a8bb65463a77226d7f1f7b9c3cae693f16b644af990fabead9a5fd843264e4cc19c564e1c00000000000000000000000000000000000000000000000000000000000000";

    bytes constant RESULT =
        hex"000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000203333333333333333333333333333333333333333333333333333333333333333";

    string constant URL = "https://ens-gateway.opencovenant.org/{sender}/{data}.json";

    function _deploy(address signer) internal returns (OffchainResolver) {
        address[] memory signers = new address[](1);
        signers[0] = signer;
        deployCodeTo("OffchainResolver.sol:OffchainResolver", abi.encode(URL, signers), RESOLVER);
        return OffchainResolver(RESOLVER);
    }

    function test_resolveWithProof_accepts_the_gateway_golden_vector() public {
        OffchainResolver r = _deploy(SIGNER);
        vm.warp(1_000_000_000); // before expires
        assertEq(r.resolveWithProof(RESPONSE, REQUEST), RESULT);
    }

    function test_resolveWithProof_rejects_an_unlisted_signer() public {
        OffchainResolver r = _deploy(address(0xDEAD));
        vm.warp(1_000_000_000);
        vm.expectRevert(OffchainResolver.UntrustedSigner.selector);
        r.resolveWithProof(RESPONSE, REQUEST);
    }

    function test_resolveWithProof_rejects_an_expired_response() public {
        OffchainResolver r = _deploy(SIGNER);
        vm.warp(1_900_000_000); // after expires
        vm.expectRevert(OffchainResolver.SignatureExpired.selector);
        r.resolveWithProof(RESPONSE, REQUEST);
    }

    function test_resolve_reverts_offchain_lookup() public {
        OffchainResolver r = _deploy(SIGNER);
        vm.expectRevert(); // OffchainLookup(address,string[],bytes,bytes4,bytes)
        r.resolve(hex"00", hex"00");
    }
}
