// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Test} from "forge-std/Test.sol";
import {GuardedTradeExecutor} from "../contracts/GuardedTradeExecutor.sol";
import {RwaTradeGuard} from "../contracts/RwaTradeGuard.sol";
import {MockAggregator} from "./mocks/MockAggregator.sol";
import {MockTradableStock} from "./mocks/MockTradableStock.sol";
import {MockUSDG} from "./mocks/MockUSDG.sol";
import {MockSwapRouter} from "./mocks/MockSwapRouter.sol";

contract GuardedTradeExecutorTest is Test {
    GuardedTradeExecutor exec;
    RwaTradeGuard guard;
    MockAggregator feed;
    MockTradableStock stock;
    MockUSDG usdg;
    MockSwapRouter router;

    address owner = address(0xA11CE);
    address agent = address(0xA6E27);
    address stranger = address(0xB0B);

    uint256 constant NOW = 1_000_000;
    uint256 constant WAD = 1e18;
    uint256 constant PRICE = 200 * 1e8; // $200.00
    uint256 constant CAP = 10_000 * 1e8; // $10,000
    uint32 constant BAND = 50; // 0.5%
    uint64 constant STALE = 3600;
    uint256 constant USDG_ONE = 1e6; // 6 decimals

    function setUp() public {
        vm.warp(NOW);
        guard = new RwaTradeGuard(owner);
        feed = new MockAggregator(8);
        feed.set(int256(PRICE), NOW);
        stock = new MockTradableStock(WAD); // multiplier 1.0
        usdg = new MockUSDG();
        router = new MockSwapRouter();

        vm.startPrank(owner);
        guard.setAsset(address(stock), address(feed), CAP, BAND, STALE);
        exec = new GuardedTradeExecutor(address(guard), address(usdg), owner);
        exec.setRouter(address(router), true);
        vm.stopPrank();

        // A funded agent with a session key, and a venue holding inventory.
        usdg.mint(agent, 5_000 * USDG_ONE);
        stock.mint(address(router), 1_000 * WAD);
        vm.prank(agent);
        usdg.approve(address(exec), type(uint256).max);
    }

    function _swap(uint256 usdgIn, uint256 assetOut) internal view returns (bytes memory) {
        return abi.encodeCall(
            MockSwapRouter.swap, (address(usdg), usdgIn, address(stock), assetOut, address(exec))
        );
    }

    // The capability: an agent executes a real bounded buy that settles.
    function test_ExecutesBoundedBuy() public {
        uint256 assetOut = 5 * WAD; // 5 shares @ $200 = $1,000, under the cap
        uint256 spend = 1_000 * USDG_ONE;
        uint256 maxIn = 1_010 * USDG_ONE; // a slippage headroom over the fill

        vm.prank(agent);
        uint256 got = exec.buy(address(stock), maxIn, assetOut, PRICE, address(router), _swap(spend, assetOut));

        assertEq(got, assetOut, "delivered the fill");
        assertEq(stock.balanceOf(agent), assetOut, "agent holds the stock");
        assertEq(usdg.balanceOf(agent), 5_000 * USDG_ONE - spend, "agent spent exactly the fill, headroom refunded");
        assertEq(usdg.balanceOf(address(exec)), 0, "executor holds no USDG between calls");
        assertEq(stock.balanceOf(address(exec)), 0, "executor holds no stock between calls");
    }

    // The safety that makes the capability handable: a reckless size reverts in
    // the guard before a single unit of USDG is pulled.
    function test_RecklessSizeRevertsBeforeAnyPull() public {
        uint256 reckless = 100 * WAD; // 100 @ $200 = $20,000 > $10,000 cap
        uint256 before = usdg.balanceOf(agent);

        vm.prank(agent);
        vm.expectRevert(abi.encodeWithSelector(RwaTradeGuard.NotionalOverCap.selector, 20_000 * 1e8, CAP));
        exec.buy(address(stock), 5_000 * USDG_ONE, reckless, PRICE, address(router), _swap(5_000 * USDG_ONE, reckless));

        assertEq(usdg.balanceOf(agent), before, "no USDG left the agent");
        assertEq(stock.balanceOf(agent), 0, "no stock delivered");
    }

    function test_OffBandQuoteRevertsBeforeAnyPull() public {
        uint256 before = usdg.balanceOf(agent);
        vm.prank(agent);
        vm.expectRevert(); // PriceOutsideBand
        exec.buy(address(stock), 2_000 * USDG_ONE, WAD, 202 * 1e8, address(router), _swap(1_000 * USDG_ONE, WAD));
        assertEq(usdg.balanceOf(agent), before, "no USDG left the agent");
    }

    function test_StaleFeedRevertsBeforeAnyPull() public {
        feed.set(int256(PRICE), NOW - (STALE + 1));
        uint256 before = usdg.balanceOf(agent);
        vm.prank(agent);
        vm.expectRevert(abi.encodeWithSelector(RwaTradeGuard.StalePriceFeed.selector, STALE + 1, STALE));
        exec.buy(address(stock), 2_000 * USDG_ONE, WAD, PRICE, address(router), _swap(1_000 * USDG_ONE, WAD));
        assertEq(usdg.balanceOf(agent), before, "no USDG left the agent");
    }

    // A venue that under-delivers cannot settle: the whole trade reverts and the
    // agent keeps its USDG.
    function test_ShortFillReverts() public {
        uint256 want = 5 * WAD;
        uint256 short = 4 * WAD;
        uint256 before = usdg.balanceOf(agent);

        vm.prank(agent);
        vm.expectRevert(abi.encodeWithSelector(GuardedTradeExecutor.InsufficientOut.selector, short, want));
        exec.buy(address(stock), 1_010 * USDG_ONE, want, PRICE, address(router), _swap(1_000 * USDG_ONE, short));

        assertEq(usdg.balanceOf(agent), before, "agent keeps its USDG on a short fill");
    }

    function test_SwapFailureReverts() public {
        bytes memory bad = abi.encodeCall(MockSwapRouter.revertingSwap, ());
        uint256 before = usdg.balanceOf(agent);
        vm.prank(agent);
        vm.expectRevert(GuardedTradeExecutor.SwapFailed.selector);
        exec.buy(address(stock), 1_010 * USDG_ONE, 5 * WAD, PRICE, address(router), bad);
        assertEq(usdg.balanceOf(agent), before, "agent keeps its USDG when the venue reverts");
    }

    function test_RejectsUnallowlistedRouter() public {
        MockSwapRouter rogue = new MockSwapRouter();
        vm.prank(agent);
        vm.expectRevert(abi.encodeWithSelector(GuardedTradeExecutor.RouterNotAllowed.selector, address(rogue)));
        exec.buy(address(stock), 1_010 * USDG_ONE, 5 * WAD, PRICE, address(rogue), _swap(1_000 * USDG_ONE, 5 * WAD));
    }

    function test_ReentrantRouterIsRefused() public {
        ReentrantRouter eve = new ReentrantRouter(exec, address(stock), usdg);
        vm.prank(owner);
        exec.setRouter(address(eve), true);
        stock.mint(address(eve), 100 * WAD);

        vm.prank(agent);
        vm.expectRevert(); // the reenter trips nonReentrant; the outer call reverts
        exec.buy(address(stock), 1_010 * USDG_ONE, 5 * WAD, PRICE, address(eve), abi.encodeCall(ReentrantRouter.swap, ()));
        assertEq(stock.balanceOf(agent), 0, "no settlement under reentrancy");
    }

    function test_OnlyOwnerSetsRouter() public {
        vm.prank(stranger);
        vm.expectRevert(GuardedTradeExecutor.NotOwner.selector);
        exec.setRouter(address(router), false);
    }

    function test_TwoStepOwnership() public {
        vm.prank(owner);
        exec.transferOwnership(stranger);
        assertEq(exec.owner(), owner, "not transferred until accepted");
        vm.prank(stranger);
        exec.acceptOwnership();
        assertEq(exec.owner(), stranger);
    }

    function test_ConstructorRejectsZero() public {
        vm.expectRevert(GuardedTradeExecutor.ZeroAddress.selector);
        new GuardedTradeExecutor(address(0), address(usdg), owner);
    }
}

/// A venue that tries to reenter the executor mid-swap.
contract ReentrantRouter {
    GuardedTradeExecutor exec;
    address stock;
    MockUSDG usdg;

    constructor(GuardedTradeExecutor exec_, address stock_, MockUSDG usdg_) {
        exec = exec_;
        stock = stock_;
        usdg = usdg_;
    }

    function swap() external {
        exec.buy(stock, 1e6, 1e18, 200 * 1e8, address(this), abi.encodeCall(ReentrantRouter.swap, ()));
    }
}
