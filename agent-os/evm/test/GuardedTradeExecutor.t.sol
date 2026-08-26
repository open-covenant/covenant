// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Test} from "forge-std/Test.sol";
import {GuardedTradeExecutor} from "../contracts/GuardedTradeExecutor.sol";
import {RwaTradeGuard} from "../contracts/RwaTradeGuard.sol";
import {MockAggregator} from "./mocks/MockAggregator.sol";
import {MockTradableStock} from "./mocks/MockTradableStock.sol";
import {MockUSDG} from "./mocks/MockUSDG.sol";
import {MockSwapRouter} from "./mocks/MockSwapRouter.sol";
import {MockPermit2} from "./mocks/MockPermit2.sol";

contract GuardedTradeExecutorTest is Test {
    GuardedTradeExecutor exec;
    RwaTradeGuard guard;
    MockAggregator feed;
    MockTradableStock stock;
    MockUSDG usdg;
    MockSwapRouter router;
    MockPermit2 permit2;

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
        permit2 = new MockPermit2();

        vm.startPrank(owner);
        guard.setAsset(address(stock), address(feed), CAP, BAND, STALE);
        exec = new GuardedTradeExecutor(address(guard), address(usdg), address(permit2), owner);
        exec.setRouter(address(router), true);
        guard.setExecutor(address(exec), true);
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
        new GuardedTradeExecutor(address(0), address(usdg), address(permit2), owner);
    }

    // Priming: a UniversalRouter-style venue pulls through Permit2, so the
    // executor has to hold its allowance there before it can route.
    function test_PrimeRouterGrantsPermit2Allowance() public {
        uint48 expiry = uint48(NOW + 30 days);
        vm.prank(owner);
        exec.primeRouter(address(router), address(usdg), type(uint160).max, expiry);

        assertEq(usdg.allowance(address(exec), address(permit2)), type(uint256).max, "permit2 pulls from the executor");
        (uint160 amount, uint48 expiration,) = permit2.allowance(address(exec), address(usdg), address(router));
        assertEq(amount, type(uint160).max);
        assertEq(expiration, expiry);
    }

    function test_PrimeRouterRefusesUnallowlistedVenue() public {
        vm.prank(owner);
        vm.expectRevert(abi.encodeWithSelector(GuardedTradeExecutor.RouterNotAllowed.selector, stranger));
        exec.primeRouter(stranger, address(usdg), type(uint160).max, uint48(NOW + 1 days));
    }

    function test_OnlyOwnerPrimes() public {
        vm.prank(agent);
        vm.expectRevert(GuardedTradeExecutor.NotOwner.selector);
        exec.primeRouter(address(router), address(usdg), type(uint160).max, uint48(NOW + 1 days));
    }

    // The exit: an agent that can enter a position can leave it, under the same
    // bounds, and the executor keeps nothing on the way through.
    function test_ExecutesBoundedSell() public {
        uint256 shares = 2 * WAD;
        uint256 proceeds = 400 * USDG_ONE; // 2 @ $200
        stock.mint(agent, shares);
        usdg.mint(address(router), 10_000 * USDG_ONE);
        vm.prank(agent);
        stock.approve(address(exec), type(uint256).max);

        uint256 usdgBefore = usdg.balanceOf(agent);
        vm.prank(agent);
        uint256 got = exec.sell(
            address(stock),
            shares,
            proceeds,
            PRICE,
            address(router),
            abi.encodeCall(MockSwapRouter.swap, (address(stock), shares, address(usdg), proceeds, address(exec)))
        );

        assertEq(got, proceeds, "delivered the proceeds");
        assertEq(stock.balanceOf(agent), 0, "position closed");
        assertEq(usdg.balanceOf(agent), usdgBefore + proceeds, "agent holds the proceeds");
        assertEq(stock.balanceOf(address(exec)), 0, "executor holds no stock between calls");
        assertEq(usdg.balanceOf(address(exec)), 0, "executor holds no USDG between calls");
    }

    function test_RecklessSellRevertsBeforeAnyPull() public {
        uint256 reckless = 100 * WAD; // $20,000 > $10,000 cap
        stock.mint(agent, reckless);
        vm.prank(agent);
        stock.approve(address(exec), type(uint256).max);

        vm.prank(agent);
        vm.expectRevert(abi.encodeWithSelector(RwaTradeGuard.NotionalOverCap.selector, 20_000 * 1e8, CAP));
        exec.sell(address(stock), reckless, 0, PRICE, address(router), "");

        assertEq(stock.balanceOf(agent), reckless, "no stock left the agent");
    }

    function test_ShortProceedsRevertTheSell() public {
        uint256 shares = WAD;
        stock.mint(agent, shares);
        usdg.mint(address(router), 10_000 * USDG_ONE);
        vm.prank(agent);
        stock.approve(address(exec), type(uint256).max);

        vm.prank(agent);
        vm.expectRevert(
            abi.encodeWithSelector(GuardedTradeExecutor.InsufficientOut.selector, 100 * USDG_ONE, 200 * USDG_ONE)
        );
        exec.sell(
            address(stock),
            shares,
            200 * USDG_ONE,
            PRICE,
            address(router),
            abi.encodeCall(MockSwapRouter.swap, (address(stock), shares, address(usdg), 100 * USDG_ONE, address(exec)))
        );
    }

    // The bound the per-trade cap cannot express: a hundred in-cap trades.
    function test_WindowStopsRepeatedInCapTrades() public {
        vm.prank(owner);
        guard.setSpendWindow(1 days, 1_500 * 1e8); // room for one $1,000 buy, not two

        uint256 assetOut = 5 * WAD; // $1,000
        uint256 spend = 1_000 * USDG_ONE;
        vm.prank(agent);
        exec.buy(address(stock), spend, assetOut, PRICE, address(router), _swap(spend, assetOut));
        assertEq(guard.spentInWindow(agent), 1_000 * 1e8, "the fill is charged to the window");

        vm.prank(agent);
        vm.expectRevert(abi.encodeWithSelector(RwaTradeGuard.WindowCapExceeded.selector, 2_000 * 1e8, 1_500 * 1e8));
        exec.buy(address(stock), spend, assetOut, PRICE, address(router), _swap(spend, assetOut));
    }

    function test_WindowRefillsAsItDecays() public {
        vm.prank(owner);
        guard.setSpendWindow(1 days, 1_000 * 1e8);

        uint256 assetOut = 5 * WAD;
        uint256 spend = 1_000 * USDG_ONE;
        vm.prank(agent);
        exec.buy(address(stock), spend, assetOut, PRICE, address(router), _swap(spend, assetOut));
        assertEq(guard.spentInWindow(agent), 1_000 * 1e8);

        vm.warp(NOW + 12 hours);
        assertEq(guard.spentInWindow(agent), 500 * 1e8, "half the window has passed, half the budget is back");

        vm.warp(NOW + 1 days);
        assertEq(guard.spentInWindow(agent), 0, "a full window clears it");
        feed.set(int256(PRICE), NOW + 1 days);
        vm.prank(agent);
        exec.buy(address(stock), spend, assetOut, PRICE, address(router), _swap(spend, assetOut));
    }

    function test_OnlyRegisteredExecutorCommits() public {
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(RwaTradeGuard.NotExecutor.selector, stranger));
        guard.commitTrade(agent, address(stock), WAD, PRICE);
    }

    function test_PrimeRouterRefusedWithoutPermit2() public {
        vm.startPrank(owner);
        GuardedTradeExecutor bare = new GuardedTradeExecutor(address(guard), address(usdg), address(0), owner);
        bare.setRouter(address(router), true);
        vm.expectRevert(GuardedTradeExecutor.Permit2Unset.selector);
        bare.primeRouter(address(router), address(usdg), type(uint160).max, uint48(NOW + 1 days));
        vm.stopPrank();
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
