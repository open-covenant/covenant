// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Test} from "forge-std/Test.sol";
import {RwaTradeGuard} from "../contracts/RwaTradeGuard.sol";
import {MockAggregator} from "./mocks/MockAggregator.sol";
import {MockStockToken} from "./mocks/MockStockToken.sol";

contract RwaTradeGuardTest is Test {
    RwaTradeGuard guard;
    MockAggregator feed;
    MockStockToken token;

    address owner = address(0xA11CE);
    address stranger = address(0xB0B);

    uint256 constant NOW = 1_000_000;
    uint256 constant WAD = 1e18;
    uint256 constant PRICE = 200 * 1e8; // $200.00
    uint256 constant CAP = 10_000 * 1e8; // $10,000
    uint32 constant BAND = 50; // 0.5%
    uint64 constant STALE = 3600;

    function setUp() public {
        vm.warp(NOW);
        vm.prank(owner);
        guard = new RwaTradeGuard(owner);

        feed = new MockAggregator(8);
        feed.set(int256(PRICE), NOW);
        token = new MockStockToken(WAD); // multiplier 1.0

        vm.prank(owner);
        guard.setAsset(address(token), address(feed), CAP, BAND, STALE);
    }

    function _check(uint256 rawAmount, uint256 quoted) internal view returns (uint256, uint256) {
        return guard.checkTrade(address(token), rawAmount, quoted);
    }

    function test_AllowsFairInCapTrade() public view {
        (uint256 notional, uint256 shares) = _check(5 * WAD, PRICE);
        assertEq(notional, 1_000 * 1e8);
        assertEq(shares, 5 * WAD);
    }

    function test_RefusesQuoteOutsideBand() public {
        vm.expectRevert(
            abi.encodeWithSelector(RwaTradeGuard.PriceOutsideBand.selector, 202 * 1e8, PRICE, 100, BAND)
        );
        _check(WAD, 202 * 1e8); // 1% over, band is 0.5%
    }

    function test_RefusesNotionalOverCap() public {
        vm.expectRevert(
            abi.encodeWithSelector(RwaTradeGuard.NotionalOverCap.selector, 12_000 * 1e8, CAP)
        );
        _check(60 * WAD, PRICE); // 60 * $200 = $12,000 > $10,000
    }

    function test_RefusesStaleFeed() public {
        feed.set(int256(PRICE), NOW - (STALE + 1));
        vm.expectRevert(
            abi.encodeWithSelector(RwaTradeGuard.StalePriceFeed.selector, STALE + 1, STALE)
        );
        _check(WAD, PRICE);
    }

    function test_RefusesOraclePaused() public {
        token.setPaused(true);
        vm.expectRevert(RwaTradeGuard.OraclePaused.selector);
        _check(WAD, PRICE);
    }

    function test_RefusesFutureDatedFeed() public {
        feed.set(int256(PRICE), NOW + 1);
        vm.expectRevert(RwaTradeGuard.PriceUnavailable.selector);
        _check(WAD, PRICE);
    }

    function test_BandIsExactAtBoundary() public view {
        // 50bps of $200 is exactly $1.00. A quote $1.00 off is inside.
        uint256 atBand = PRICE + 1 * 1e8;
        guard.checkTrade(address(token), WAD, atBand);
    }

    function test_BandRefusesOneUnitBeyond() public {
        uint256 oneOver = PRICE + 1 * 1e8 + 1;
        vm.expectRevert();
        _check(WAD, oneOver);
    }

    function test_SequencerInvalidRoundIsRefused() public {
        MockAggregator seq = new MockAggregator(0);
        vm.prank(owner);
        guard.setSequencerFeed(address(seq), 3600);
        seq.set(0, 0); // up, but startedAt 0 = invalid round
        seq.setStartedAt(0);
        vm.expectRevert(RwaTradeGuard.SequencerDown.selector);
        _check(WAD, PRICE);
    }

    function test_RefusesUninitializedMultiplier() public {
        token.setMultiplier(0);
        vm.expectRevert(RwaTradeGuard.MultiplierUnset.selector);
        _check(WAD, PRICE);
    }

    function test_RefusesNonPositivePrice() public {
        feed.set(0, NOW);
        vm.expectRevert(RwaTradeGuard.PriceUnavailable.selector);
        _check(WAD, PRICE);
        feed.set(-1, NOW);
        vm.expectRevert(RwaTradeGuard.PriceUnavailable.selector);
        _check(WAD, PRICE);
    }

    function test_RefusesUnregisteredAsset() public {
        vm.expectRevert(
            abi.encodeWithSelector(RwaTradeGuard.AssetNotEnabled.selector, address(0xDEAD))
        );
        guard.checkTrade(address(0xDEAD), WAD, PRICE);
    }

    function test_MultiplierScalesShares() public {
        token.setMultiplier(2 * WAD); // after a 2:1 split
        (uint256 notional, uint256 shares) = _check(10 * WAD, PRICE);
        assertEq(shares, 20 * WAD, "shares double");
        assertEq(notional, 2_000 * 1e8, "notional unchanged");
    }

    function test_SetAssetRejectsNon8DecimalFeed() public {
        MockAggregator six = new MockAggregator(6);
        vm.prank(owner);
        vm.expectRevert(abi.encodeWithSelector(RwaTradeGuard.FeedNot8Decimals.selector, 6));
        guard.setAsset(address(token), address(six), CAP, BAND, STALE);
    }

    function test_OnlyOwnerConfigures() public {
        vm.prank(stranger);
        vm.expectRevert(RwaTradeGuard.NotOwner.selector);
        guard.setAsset(address(token), address(feed), CAP, BAND, STALE);
    }

    function test_SequencerDownIsRefused() public {
        MockAggregator seq = new MockAggregator(0);
        vm.prank(owner);
        guard.setSequencerFeed(address(seq), 3600);

        // Down (answer 1) is refused.
        seq.set(1, 0);
        seq.setStartedAt(NOW - 10_000);
        vm.expectRevert(RwaTradeGuard.SequencerDown.selector);
        _check(WAD, PRICE);

        // Up but inside the grace window is refused.
        seq.set(0, 0);
        seq.setStartedAt(NOW - 100);
        vm.expectRevert(RwaTradeGuard.SequencerDown.selector);
        _check(WAD, PRICE);

        // Up and past the grace window is allowed.
        seq.setStartedAt(NOW - 10_000);
        (uint256 notional,) = _check(5 * WAD, PRICE);
        assertEq(notional, 1_000 * 1e8);
    }

    function test_TwoStepOwnership() public {
        vm.prank(owner);
        guard.transferOwnership(stranger);
        assertEq(guard.owner(), owner, "not transferred until accepted");
        vm.prank(stranger);
        guard.acceptOwnership();
        assertEq(guard.owner(), stranger);
    }
}
