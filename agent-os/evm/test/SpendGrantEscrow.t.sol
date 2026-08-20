// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Test} from "forge-std/Test.sol";
import {SpendGrantEscrow} from "../contracts/SpendGrantEscrow.sol";
import {IERC20} from "../contracts/interfaces/IERC20.sol";
import {MockUSDG, MockNoReturnUSDG} from "./mocks/MockUSDG.sol";

/// Every bound is a property of the contract that holds the money. These tests
/// pin the two guarantees that make "hand an agent a wallet and walk away"
/// safe: a rogue spender cannot exceed the grant (ceiling / total / provider /
/// expiry), and a gated call cannot pay out without a passing attestor verdict.
contract SpendGrantEscrowTest is Test {
    uint256 constant SECP256K1N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141;

    SpendGrantEscrow escrow;
    MockUSDG usd;

    uint256 attestorPk = 0xA11CE;
    address attestor;
    address admin = address(0xAD);
    address treasury = address(0x772E);
    address emergencyAdmin = address(0xE11E);
    address gateway = address(0x6A7E);
    address funder = address(0xF);
    address spender = address(0x59E);
    address provider1 = address(0x9401);
    address provider2 = address(0x9402);
    address stranger = address(0x57A);

    uint128 constant TOTAL_CAP = 10_000_000;
    uint128 constant PER_CALL = 1_000_000;
    uint128 constant AMOUNT = 500_000;
    bytes32 constant SCOPE = keccak256("scope-v1");
    bytes32 constant SPEC = keccak256("spec-1");
    bytes32 constant RESULT = keccak256("result-1");

    uint64 expiry;
    uint64 callDeadline;

    function setUp() public {
        vm.warp(1_700_000_000);
        attestor = vm.addr(attestorPk);
        usd = new MockUSDG();
        escrow = new SpendGrantEscrow(IERC20(address(usd)), admin, attestor, treasury, emergencyAdmin, gateway);
        vm.prank(admin);
        escrow.unpause();
        expiry = uint64(block.timestamp + 30 days);
        callDeadline = uint64(block.timestamp + 1 days);
        usd.mint(funder, 1_000_000_000);
    }

    function _providers() internal view returns (address[] memory p) {
        p = new address[](2);
        p[0] = provider1;
        p[1] = provider2;
    }

    function _createGrant(bool gate) internal returns (uint256 grantId) {
        vm.startPrank(funder);
        usd.approve(address(escrow), TOTAL_CAP);
        grantId = escrow.createGrant(spender, TOTAL_CAP, PER_CALL, expiry, _providers(), SCOPE, gate);
        vm.stopPrank();
    }

    function _charge(uint256 grantId, address provider, uint128 amount, uint256 callId) internal {
        vm.prank(spender);
        escrow.chargeCall(grantId, provider, amount, callId, callDeadline);
    }

    function _sign(uint256 pk, uint256 callId, bytes32 resultHash, bool passed, uint256 deadline)
        internal
        view
        returns (bytes memory)
    {
        bytes32 structHash =
            keccak256(abi.encode(escrow.QUALITY_TYPEHASH(), callId, resultHash, passed, SPEC, deadline));
        bytes32 digest = keccak256(abi.encodePacked("\x19\x01", escrow.domainSeparator(), structHash));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(pk, digest);
        return abi.encodePacked(r, s, v);
    }

    function test_CreateGrant_PullsFundsAndStores() public {
        uint256 g = _createGrant(false);
        assertEq(usd.balanceOf(address(escrow)), TOTAL_CAP);
        assertEq(usd.balanceOf(funder), 1_000_000_000 - TOTAL_CAP);
        SpendGrantEscrow.Grant memory grant = escrow.getGrant(g);
        assertEq(grant.funder, funder);
        assertEq(grant.spender, spender);
        assertEq(grant.totalCap, TOTAL_CAP);
        assertEq(grant.balance, TOTAL_CAP);
        assertEq(grant.perCallCeiling, PER_CALL);
        assertEq(grant.scopeHash, SCOPE);
        assertTrue(escrow.allowedProvider(g, provider1));
        assertTrue(escrow.allowedProvider(g, provider2));
        assertFalse(escrow.allowedProvider(g, stranger));
    }

    function test_CreateGrant_RevertOverMaxEscrow() public {
        uint128 over = uint128(escrow.MAX_ESCROW()) + 1;
        vm.startPrank(funder);
        usd.approve(address(escrow), type(uint256).max);
        vm.expectRevert(SpendGrantEscrow.LimitExceeded.selector);
        escrow.createGrant(spender, over, PER_CALL, expiry, _providers(), SCOPE, false);
        vm.stopPrank();
    }

    function test_CreateGrant_RevertPerCallOverTotal() public {
        vm.startPrank(funder);
        usd.approve(address(escrow), TOTAL_CAP);
        vm.expectRevert(SpendGrantEscrow.InvalidAmount.selector);
        escrow.createGrant(spender, TOTAL_CAP, TOTAL_CAP + 1, expiry, _providers(), SCOPE, false);
        vm.stopPrank();
    }

    function test_CreateGrant_RevertBadExpiry() public {
        vm.startPrank(funder);
        usd.approve(address(escrow), TOTAL_CAP);
        vm.expectRevert(SpendGrantEscrow.InvalidExpiry.selector);
        escrow.createGrant(spender, TOTAL_CAP, PER_CALL, uint64(block.timestamp), _providers(), SCOPE, false);
        vm.expectRevert(SpendGrantEscrow.InvalidExpiry.selector);
        escrow.createGrant(spender, TOTAL_CAP, PER_CALL, uint64(block.timestamp + 91 days), _providers(), SCOPE, false);
        vm.stopPrank();
    }

    function test_CreateGrant_RevertBadProviders() public {
        vm.startPrank(funder);
        usd.approve(address(escrow), TOTAL_CAP);
        address[] memory empty = new address[](0);
        vm.expectRevert(SpendGrantEscrow.InvalidProviders.selector);
        escrow.createGrant(spender, TOTAL_CAP, PER_CALL, expiry, empty, SCOPE, false);

        address[] memory dup = new address[](2);
        dup[0] = provider1;
        dup[1] = provider1;
        vm.expectRevert(SpendGrantEscrow.InvalidProviders.selector);
        escrow.createGrant(spender, TOTAL_CAP, PER_CALL, expiry, dup, SCOPE, false);

        address[] memory zero = new address[](1);
        zero[0] = address(0);
        vm.expectRevert(SpendGrantEscrow.InvalidProviders.selector);
        escrow.createGrant(spender, TOTAL_CAP, PER_CALL, expiry, zero, SCOPE, false);
        vm.stopPrank();
    }

    function test_CreateGrant_RevertZeroScope() public {
        vm.startPrank(funder);
        usd.approve(address(escrow), TOTAL_CAP);
        vm.expectRevert(SpendGrantEscrow.InvalidScope.selector);
        escrow.createGrant(spender, TOTAL_CAP, PER_CALL, expiry, _providers(), bytes32(0), false);
        vm.stopPrank();
    }

    function test_ChargeCall_LocksHold() public {
        uint256 g = _createGrant(false);
        _charge(g, provider1, AMOUNT, 1);
        assertEq(escrow.getGrant(g).balance, TOTAL_CAP - AMOUNT);
        SpendGrantEscrow.Call memory c = escrow.getCall(1);
        assertEq(uint8(c.status), uint8(SpendGrantEscrow.CallStatus.Held));
        assertEq(c.amount, AMOUNT);
        assertEq(c.provider, provider1);
        assertEq(usd.balanceOf(address(escrow)), TOTAL_CAP);
    }

    function test_ChargeCall_RevertNotSpender() public {
        uint256 g = _createGrant(false);
        vm.prank(stranger);
        vm.expectRevert(SpendGrantEscrow.NotSpender.selector);
        escrow.chargeCall(g, provider1, AMOUNT, 1, callDeadline);
    }

    function test_ChargeCall_RevertOverPerCallCeiling() public {
        uint256 g = _createGrant(false);
        vm.prank(spender);
        vm.expectRevert(SpendGrantEscrow.InvalidAmount.selector);
        escrow.chargeCall(g, provider1, PER_CALL + 1, 1, callDeadline);
    }

    function test_ChargeCall_RevertOverBalance() public {
        vm.startPrank(funder);
        usd.approve(address(escrow), 1_000_000);
        uint256 g = escrow.createGrant(spender, 1_000_000, 1_000_000, expiry, _providers(), SCOPE, false);
        vm.stopPrank();
        _charge(g, provider1, 700_000, 1);
        vm.prank(spender);
        vm.expectRevert(SpendGrantEscrow.InvalidAmount.selector);
        escrow.chargeCall(g, provider1, 400_000, 2, callDeadline);
    }

    function test_ChargeCall_RevertProviderNotAllowed() public {
        uint256 g = _createGrant(false);
        vm.prank(spender);
        vm.expectRevert(SpendGrantEscrow.ProviderNotAllowed.selector);
        escrow.chargeCall(g, stranger, AMOUNT, 1, callDeadline);
    }

    function test_ChargeCall_RevertExpired() public {
        uint256 g = _createGrant(false);
        vm.warp(expiry);
        vm.prank(spender);
        vm.expectRevert(SpendGrantEscrow.Expired.selector);
        escrow.chargeCall(g, provider1, AMOUNT, 1, uint64(expiry + 1 hours));
    }

    function test_ChargeCall_RevertDuplicateCallId() public {
        uint256 g = _createGrant(false);
        _charge(g, provider1, AMOUNT, 1);
        vm.prank(spender);
        vm.expectRevert(SpendGrantEscrow.CallExists.selector);
        escrow.chargeCall(g, provider1, AMOUNT, 1, callDeadline);
    }

    function test_ChargeCall_RevertBadDeadline() public {
        uint256 g = _createGrant(false);
        vm.startPrank(spender);
        vm.expectRevert(SpendGrantEscrow.InvalidDeadline.selector);
        escrow.chargeCall(g, provider1, AMOUNT, 1, uint64(block.timestamp));
        vm.expectRevert(SpendGrantEscrow.InvalidDeadline.selector);
        escrow.chargeCall(g, provider1, AMOUNT, 1, uint64(block.timestamp + 8 days));
        vm.stopPrank();
    }

    function test_ReleaseCall_V1_PaysProviderMinusFee() public {
        uint256 g = _createGrant(false);
        _charge(g, provider1, AMOUNT, 1);
        vm.prank(spender);
        escrow.releaseCall(1, RESULT);
        assertEq(usd.balanceOf(provider1), 450_000);
        assertEq(usd.balanceOf(treasury), 50_000);
        assertEq(usd.balanceOf(address(escrow)), TOTAL_CAP - AMOUNT);
        assertEq(uint8(escrow.getCall(1).status), uint8(SpendGrantEscrow.CallStatus.Released));
    }

    function test_ReleaseCall_V1_GatewayCanRelease() public {
        uint256 g = _createGrant(false);
        _charge(g, provider1, AMOUNT, 1);
        vm.prank(gateway);
        escrow.releaseCall(1, RESULT);
        assertEq(usd.balanceOf(provider1), 450_000);
    }

    function test_ReleaseCall_V1_RevertUnauthorized() public {
        uint256 g = _createGrant(false);
        _charge(g, provider1, AMOUNT, 1);
        vm.prank(stranger);
        vm.expectRevert(SpendGrantEscrow.Unauthorized.selector);
        escrow.releaseCall(1, RESULT);
    }

    function test_ReleaseCall_V1_RevertWhenGated() public {
        uint256 g = _createGrant(true);
        _charge(g, provider1, AMOUNT, 1);
        vm.prank(spender);
        vm.expectRevert(SpendGrantEscrow.QualityGateRequired.selector);
        escrow.releaseCall(1, RESULT);
    }

    function test_ReleaseCallAttested_V2_Pays() public {
        uint256 g = _createGrant(true);
        _charge(g, provider1, AMOUNT, 1);
        uint256 attDeadline = block.timestamp + 1 hours;
        bytes memory sig = _sign(attestorPk, 1, RESULT, true, attDeadline);
        vm.prank(stranger);
        escrow.releaseCallAttested(1, RESULT, SPEC, attDeadline, sig);
        assertEq(usd.balanceOf(provider1), 450_000);
        assertEq(usd.balanceOf(treasury), 50_000);
        assertEq(escrow.getCall(1).resultHash, RESULT);
    }

    function test_ReleaseCallAttested_RevertBadSigner() public {
        uint256 g = _createGrant(true);
        _charge(g, provider1, AMOUNT, 1);
        uint256 attDeadline = block.timestamp + 1 hours;
        bytes memory sig = _sign(0xBAD, 1, RESULT, true, attDeadline);
        vm.expectRevert(SpendGrantEscrow.InvalidSignature.selector);
        escrow.releaseCallAttested(1, RESULT, SPEC, attDeadline, sig);
    }

    function test_ReleaseCallAttested_RevertFailVerdictSig() public {
        uint256 g = _createGrant(true);
        _charge(g, provider1, AMOUNT, 1);
        uint256 attDeadline = block.timestamp + 1 hours;
        bytes memory sig = _sign(attestorPk, 1, RESULT, false, attDeadline);
        vm.expectRevert(SpendGrantEscrow.InvalidSignature.selector);
        escrow.releaseCallAttested(1, RESULT, SPEC, attDeadline, sig);
    }

    function test_ReleaseCallAttested_RevertExpiredAttestation() public {
        uint256 g = _createGrant(true);
        _charge(g, provider1, AMOUNT, 1);
        uint256 attDeadline = block.timestamp + 1 hours;
        bytes memory sig = _sign(attestorPk, 1, RESULT, true, attDeadline);
        vm.warp(attDeadline + 1);
        vm.expectRevert(SpendGrantEscrow.Expired.selector);
        escrow.releaseCallAttested(1, RESULT, SPEC, attDeadline, sig);
    }

    function test_ReleaseCallAttested_RevertPastCallDeadline() public {
        uint256 g = _createGrant(true);
        _charge(g, provider1, AMOUNT, 1);
        uint256 attDeadline = uint256(callDeadline) + 2 days;
        bytes memory sig = _sign(attestorPk, 1, RESULT, true, attDeadline);
        vm.warp(uint256(callDeadline) + 1);
        vm.expectRevert(SpendGrantEscrow.Expired.selector);
        escrow.releaseCallAttested(1, RESULT, SPEC, attDeadline, sig);
        vm.prank(stranger);
        escrow.refundCall(1, keccak256("timeout"));
        assertEq(escrow.getGrant(g).balance, TOTAL_CAP);
        assertEq(usd.balanceOf(provider1), 0);
    }

    function test_RefundCallAttested_JunkRefundsHold() public {
        uint256 g = _createGrant(true);
        _charge(g, provider1, AMOUNT, 1);
        uint256 attDeadline = block.timestamp + 1 hours;
        bytes memory sig = _sign(attestorPk, 1, RESULT, false, attDeadline);
        vm.prank(stranger);
        escrow.refundCallAttested(1, RESULT, SPEC, attDeadline, sig);
        assertEq(usd.balanceOf(provider1), 0);
        assertEq(escrow.getGrant(g).balance, TOTAL_CAP);
        assertEq(uint8(escrow.getCall(1).status), uint8(SpendGrantEscrow.CallStatus.Refunded));
    }

    function test_RefundCall_AfterDeadlinePermissionless() public {
        uint256 g = _createGrant(false);
        _charge(g, provider1, AMOUNT, 1);
        vm.warp(callDeadline + 1);
        vm.prank(stranger);
        escrow.refundCall(1, keccak256("timeout"));
        assertEq(escrow.getGrant(g).balance, TOTAL_CAP);
        assertEq(usd.balanceOf(provider1), 0);
        assertEq(uint8(escrow.getCall(1).status), uint8(SpendGrantEscrow.CallStatus.Refunded));
    }

    function test_RefundCall_RevertBeforeDeadline() public {
        uint256 g = _createGrant(false);
        _charge(g, provider1, AMOUNT, 1);
        vm.expectRevert(SpendGrantEscrow.NotDue.selector);
        escrow.refundCall(1, keccak256("timeout"));
    }

    function test_AdminClose_OnlyEmergencyAdmin() public {
        uint256 g = _createGrant(false);
        _charge(g, provider1, AMOUNT, 1);
        vm.prank(stranger);
        vm.expectRevert(SpendGrantEscrow.Unauthorized.selector);
        escrow.adminClose(1, keccak256("stuck"));
        vm.prank(emergencyAdmin);
        escrow.adminClose(1, keccak256("stuck"));
        assertEq(escrow.getGrant(g).balance, TOTAL_CAP);
        assertEq(uint8(escrow.getCall(1).status), uint8(SpendGrantEscrow.CallStatus.Refunded));
    }

    function test_WithdrawUnused_FunderOnly_UnheldOnly() public {
        uint256 g = _createGrant(false);
        _charge(g, provider1, AMOUNT, 1);
        vm.prank(stranger);
        vm.expectRevert(SpendGrantEscrow.NotFunder.selector);
        escrow.withdrawUnused(g, 1);

        vm.prank(funder);
        vm.expectRevert(SpendGrantEscrow.InvalidAmount.selector);
        escrow.withdrawUnused(g, TOTAL_CAP);

        vm.prank(funder);
        escrow.withdrawUnused(g, TOTAL_CAP - AMOUNT);
        assertEq(escrow.getGrant(g).balance, 0);
        assertEq(usd.balanceOf(address(escrow)), AMOUNT);

        vm.prank(spender);
        escrow.releaseCall(1, RESULT);
        assertEq(usd.balanceOf(provider1), 450_000);
    }

    function test_CloseGrant_StopsNewCharges() public {
        uint256 g = _createGrant(false);
        _charge(g, provider1, AMOUNT, 1);
        vm.prank(stranger);
        vm.expectRevert(SpendGrantEscrow.NotFunder.selector);
        escrow.closeGrant(g);

        vm.prank(funder);
        escrow.closeGrant(g);

        vm.prank(spender);
        vm.expectRevert(SpendGrantEscrow.InvalidState.selector);
        escrow.chargeCall(g, provider1, AMOUNT, 2, callDeadline);

        vm.prank(spender);
        escrow.releaseCall(1, RESULT);
        assertEq(usd.balanceOf(provider1), 450_000);
    }

    function test_Pause_BlocksChargeButNotRelease() public {
        uint256 g = _createGrant(false);
        _charge(g, provider1, AMOUNT, 1);
        vm.prank(admin);
        escrow.pause();
        vm.prank(spender);
        vm.expectRevert(SpendGrantEscrow.AlreadyPaused.selector);
        escrow.chargeCall(g, provider2, AMOUNT, 2, callDeadline);
        vm.prank(spender);
        escrow.releaseCall(1, RESULT);
        assertEq(usd.balanceOf(provider1), 450_000);
    }

    function test_Roles_SetAttestorGatewayAuth() public {
        vm.prank(stranger);
        vm.expectRevert(SpendGrantEscrow.Unauthorized.selector);
        escrow.setAttestor(stranger);

        vm.prank(admin);
        vm.expectRevert(SpendGrantEscrow.InvalidAddress.selector);
        escrow.setAttestor(address(0));

        vm.prank(admin);
        escrow.setAttestor(stranger);
        assertEq(escrow.attestor(), stranger);

        vm.prank(admin);
        escrow.setGateway(stranger);
        assertEq(escrow.gateway(), stranger);
    }

    function test_Signature_RejectsMalleableTwin() public {
        uint256 g = _createGrant(true);
        _charge(g, provider1, AMOUNT, 1);
        uint256 attDeadline = block.timestamp + 1 hours;
        bytes32 structHash =
            keccak256(abi.encode(escrow.QUALITY_TYPEHASH(), uint256(1), RESULT, true, SPEC, attDeadline));
        bytes32 digest = keccak256(abi.encodePacked("\x19\x01", escrow.domainSeparator(), structHash));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(attestorPk, digest);
        bytes32 highS = bytes32(SECP256K1N - uint256(s));
        uint8 flipped = v == 27 ? 28 : 27;
        bytes memory twin = abi.encodePacked(r, highS, flipped);
        vm.expectRevert(SpendGrantEscrow.InvalidSignature.selector);
        escrow.releaseCallAttested(1, RESULT, SPEC, attDeadline, twin);
    }

    /// Frozen cross-language vector. The Rust attestor in `covenantd` builds the
    /// same digest from these exact inputs; if either side drifts, one of these
    /// literals stops matching. chainId 42161 + verifyingContract are pinned so
    /// the domain separator is reproducible off a live deployment.
    function test_Golden_QualityDigest() public {
        assertEq(escrow.QUALITY_TYPEHASH(), 0x0cf929082ca7b2e597467a7baa8bdad7b2f11a50345061c502617546ee02c5f7);
        bytes32 resultHash = 0x1111111111111111111111111111111111111111111111111111111111111111;
        bytes32 specId = 0x2222222222222222222222222222222222222222222222222222222222222222;
        bytes32 structHash = keccak256(
            abi.encode(escrow.QUALITY_TYPEHASH(), uint256(42), resultHash, true, specId, uint256(1_700_003_600))
        );
        assertEq(structHash, 0x39fd9b7405f9e77c3bd56af5e08184f94c06e73c3947b3355d77f76a53993899);

        address fixedAddr = address(0xC0DE);
        vm.etch(fixedAddr, address(escrow).code);
        vm.chainId(42161);
        bytes32 dom = SpendGrantEscrow(fixedAddr).domainSeparator();
        assertEq(dom, 0xdfca76f14164933450f648cbe36072095dfe8b44f3dec7b405fb048035d7008b);
        bytes32 digest = keccak256(abi.encodePacked(hex"1901", dom, structHash));
        assertEq(digest, 0x8dcd9e33febda1edb51bfa3a8aa86fcc7a50d330f1cd9fd50cace7ceb37ece45);
    }

    /// Cross-language parity: a Quality verdict signed entirely by
    /// covenant-attestation's `quality.rs` releases a real held call through the
    /// production `releaseCallAttested` path — the same `_verifyQuality` →
    /// `_recover` → attestor gate a live release runs. Constants are the golden
    /// vector from `cargo run -p covenant-attestation --example quality_vector`
    /// (fixed key [7;32], chainId 42161, escrow 0x…C0DE). Regenerate if
    /// quality.rs changes; the domain binds both, so the escrow must verify from
    /// that address on that chain for the signature to recover.
    function test_Parity_RustSignedVerdictReleases() public {
        address rustAttestor = 0x4a62316623ad457F02cDC5D997deD67a383EC569;
        bytes32 r = 0x252e4c537d04e2a766aac8b52433b116557d32a4c6a52226a16301fac5695576;
        bytes32 s = 0x08d1ceb04a3412e069f9204430defb84232ffdfc882d5953a5f97e797e2f08c9;
        uint8 v = 27;
        bytes32 resultHash = 0x1111111111111111111111111111111111111111111111111111111111111111;
        bytes32 specId = 0x2222222222222222222222222222222222222222222222222222222222222222;
        uint256 attDeadline = 1_700_003_600;
        uint256 callId = 42;

        address fixedAddr = address(0xC0DE);
        vm.etch(fixedAddr, address(escrow).code);
        vm.chainId(42161);
        // etch copies code, not storage: re-arm the reentrancy guard (slot 3,
        // constructor-set to 1) that would otherwise read as locked.
        vm.store(fixedAddr, bytes32(uint256(3)), bytes32(uint256(1)));
        SpendGrantEscrow e = SpendGrantEscrow(fixedAddr);

        // Mutable roles do not travel with etched code; set the attestor to the
        // Rust key's address on the pinned deployment.
        vm.prank(admin);
        e.setAttestor(rustAttestor);

        vm.startPrank(funder);
        usd.approve(fixedAddr, TOTAL_CAP);
        uint256 g = e.createGrant(spender, TOTAL_CAP, PER_CALL, expiry, _providers(), SCOPE, true);
        vm.stopPrank();
        _chargeAt(e, g, callId);

        e.releaseCallAttested(callId, resultHash, specId, attDeadline, abi.encodePacked(r, s, v));

        uint256 fee = AMOUNT * 1_000 / 10_000;
        assertEq(usd.balanceOf(provider1), AMOUNT - fee);
        assertEq(usd.balanceOf(treasury), fee);
        assertEq(uint8(e.getCall(callId).status), uint8(SpendGrantEscrow.CallStatus.Released));
        assertEq(e.getCall(callId).resultHash, resultHash);
    }

    function _chargeAt(SpendGrantEscrow e, uint256 grantId, uint256 callId) internal {
        vm.prank(spender);
        e.chargeCall(grantId, provider1, AMOUNT, callId, callDeadline);
    }

    function test_NoReturnToken_StillSettles() public {
        MockNoReturnUSDG t = new MockNoReturnUSDG();
        SpendGrantEscrow e =
            new SpendGrantEscrow(IERC20(address(t)), admin, attestor, treasury, emergencyAdmin, gateway);
        vm.prank(admin);
        e.unpause();
        t.mint(funder, TOTAL_CAP);
        vm.startPrank(funder);
        t.approve(address(e), TOTAL_CAP);
        uint256 g = e.createGrant(spender, TOTAL_CAP, PER_CALL, expiry, _providers(), SCOPE, false);
        vm.stopPrank();
        vm.prank(spender);
        e.chargeCall(g, provider1, AMOUNT, 1, callDeadline);
        vm.prank(spender);
        e.releaseCall(1, RESULT);
        assertEq(t.balanceOf(provider1), 450_000);
        assertEq(t.balanceOf(treasury), 50_000);
    }
}
