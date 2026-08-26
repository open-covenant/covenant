// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {IERC20} from "./interfaces/IERC20.sol";
import {IPermit2} from "./interfaces/IPermit2.sol";
import {TokenTransfers} from "./lib/TokenTransfers.sol";
import {RwaTradeGuard} from "./RwaTradeGuard.sol";

/// @title GuardedTradeExecutor: an agent trades tokenized equities, bounded.
/// @notice The executing half of the RWA firewall. `RwaTradeGuard` judges a
///         trade; this carries one out. An agent hands it a session key and a
///         trade, and the trade goes through only if it clears the guard,
///         spending at most
///         the USDG the agent lets it pull. A reckless trade, over the cap or
///         off the oracle band, reverts in the guard before any USDG moves, so
///         an agent can be handed a wallet of tokenized equities and left to
///         trade it unattended.
///
///         `buy` runs the guard on the intended size and quoted price first,
///         pulls up to `maxUsdgIn`, routes the swap through an owner-allowlisted
///         venue, requires at least `minAssetOut` of the registered Stock Token
///         back, refunds any unspent USDG, and delivers the token to the caller.
///         `sell` is the same trade in reverse, under the same bounds, so an
///         agent handed a position can also leave it.
///         The executor holds no funds between calls and resets its approval to
///         zero, so an allowlisted venue that misbehaves can take at most this
///         trade's `maxUsdgIn` and still cannot deliver less than `minAssetOut`
///         without reverting the whole trade.
///
/// @dev    Deployed alongside `RwaTradeGuard` on Robinhood Chain. USDG (6dp) is
///         the settlement asset. `swapData` is swap calldata built off-chain for
///         the allowlisted `router` (Uniswap, 1inch, any AMM); the allowlist and
///         the balance-delta check are what make an opaque call safe to route.
///         A venue that settles through Permit2 rather than a plain allowance is
///         primed once with `primeRouter`.
contract GuardedTradeExecutor {
    using TokenTransfers for IERC20;

    RwaTradeGuard public immutable guard;
    IERC20 public immutable usdg;
    /// Permit2, where a UniversalRouter-style venue looks for the payer's
    /// allowance. Zero on a deployment whose venues pull on a plain allowance.
    IPermit2 public immutable permit2;
    address public owner;
    address public pendingOwner;

    mapping(address => bool) public routerAllowed;

    bool private _entered;

    event OwnershipTransferStarted(address indexed from, address indexed to);
    event OwnershipTransferred(address indexed from, address indexed to);
    event RouterSet(address indexed router, bool allowed);
    event RouterPrimed(address indexed router, address indexed token, uint160 amount, uint48 expiration);
    event Bought(
        address indexed agent, address indexed asset, address indexed router, uint256 usdgSpent, uint256 assetOut
    );
    event Sold(
        address indexed agent, address indexed asset, address indexed router, uint256 assetSold, uint256 usdgOut
    );

    error NotOwner();
    error ZeroAddress();
    error RouterNotAllowed(address router);
    error Permit2Unset();
    error Reentrant();
    error SwapFailed();
    error InsufficientOut(uint256 got, uint256 min);

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    modifier nonReentrant() {
        if (_entered) revert Reentrant();
        _entered = true;
        _;
        _entered = false;
    }

    constructor(address guard_, address usdg_, address permit2_, address initialOwner) {
        if (guard_ == address(0) || usdg_ == address(0) || initialOwner == address(0)) {
            revert ZeroAddress();
        }
        guard = RwaTradeGuard(guard_);
        usdg = IERC20(usdg_);
        permit2 = IPermit2(permit2_);
        owner = initialOwner;
        emit OwnershipTransferred(address(0), initialOwner);
    }

    // --- ownership (two-step) ---

    function transferOwnership(address to) external onlyOwner {
        pendingOwner = to;
        emit OwnershipTransferStarted(owner, to);
    }

    function acceptOwnership() external {
        if (msg.sender != pendingOwner) revert NotOwner();
        address from = owner;
        owner = pendingOwner;
        pendingOwner = address(0);
        emit OwnershipTransferred(from, owner);
    }

    function setRouter(address router, bool allowed) external onlyOwner {
        if (router == address(0)) revert ZeroAddress();
        routerAllowed[router] = allowed;
        emit RouterSet(router, allowed);
    }

    /// Give an allowlisted venue the Permit2 allowance it pulls USDG through.
    /// Uniswap's UniversalRouter settles a swap by pulling the payer through
    /// Permit2, not through the plain allowance `buy` sets, so a deployment that
    /// routes there is primed once per venue. Owner-gated and allowlist-gated:
    /// the executor holds no funds between trades, so the standing allowance can
    /// only ever reach the USDG in flight for the trade being executed.
    function primeRouter(address router, address token, uint160 amount, uint48 expiration) external onlyOwner {
        if (address(permit2) == address(0)) revert Permit2Unset();
        if (token == address(0)) revert ZeroAddress();
        if (!routerAllowed[router]) revert RouterNotAllowed(router);
        IERC20(token).safeApprove(address(permit2), type(uint256).max);
        permit2.approve(token, router, amount, expiration);
        emit RouterPrimed(router, token, amount, expiration);
    }

    // --- the capability ---

    /// Buy `minAssetOut`-or-more of `asset` for up to `maxUsdgIn`, quoted at
    /// `quotedPriceUsdE8`, through `router`. Reverts in the guard first if the
    /// intended trade breaks a bound; otherwise swaps, enforces the minimum out,
    /// refunds unspent USDG, and sends the token to the caller. Returns the token
    /// amount delivered.
    function buy(
        address asset,
        uint256 maxUsdgIn,
        uint256 minAssetOut,
        uint256 quotedPriceUsdE8,
        address router,
        bytes calldata swapData
    ) external nonReentrant returns (uint256 assetOut) {
        if (!routerAllowed[router]) revert RouterNotAllowed(router);

        // The bound comes first: the guard judges the intended size and price
        // before any USDG is pulled, so a reckless trade never reaches the venue.
        // Committing rather than checking also charges the trade to the caller's
        // window, so a hundred in-cap trades in a row is itself bounded.
        guard.commitTrade(msg.sender, asset, minAssetOut, quotedPriceUsdE8);

        usdg.pull(msg.sender, maxUsdgIn);
        usdg.safeApprove(router, maxUsdgIn);

        IERC20 token = IERC20(asset);
        uint256 balBefore = token.balanceOf(address(this));
        (bool ok,) = router.call(swapData);
        if (!ok) revert SwapFailed();
        assetOut = token.balanceOf(address(this)) - balBefore;
        if (assetOut < minAssetOut) revert InsufficientOut(assetOut, minAssetOut);

        usdg.safeApprove(router, 0);
        uint256 leftover = usdg.balanceOf(address(this));
        if (leftover > 0) usdg.push(msg.sender, leftover);
        token.push(msg.sender, assetOut);

        emit Bought(msg.sender, asset, router, maxUsdgIn - leftover, assetOut);
    }

    /// Sell `assetIn` of `asset` for at least `minUsdgOut`, quoted at
    /// `quotedPriceUsdE8`, through `router`. The mirror of `buy`: the guard
    /// judges the size and price before the token is pulled, the swap runs
    /// against the balance delta, anything the venue left unsold goes back, and
    /// the USDG lands with the caller. A position an agent can enter and cannot
    /// leave is not a bounded position, so the exit runs under the same bounds.
    function sell(
        address asset,
        uint256 assetIn,
        uint256 minUsdgOut,
        uint256 quotedPriceUsdE8,
        address router,
        bytes calldata swapData
    ) external nonReentrant returns (uint256 usdgOut) {
        if (!routerAllowed[router]) revert RouterNotAllowed(router);

        guard.commitTrade(msg.sender, asset, assetIn, quotedPriceUsdE8);

        IERC20 token = IERC20(asset);
        token.pull(msg.sender, assetIn);
        token.safeApprove(router, assetIn);

        uint256 balBefore = usdg.balanceOf(address(this));
        (bool ok,) = router.call(swapData);
        if (!ok) revert SwapFailed();
        usdgOut = usdg.balanceOf(address(this)) - balBefore;
        if (usdgOut < minUsdgOut) revert InsufficientOut(usdgOut, minUsdgOut);

        token.safeApprove(router, 0);
        uint256 leftover = token.balanceOf(address(this));
        if (leftover > 0) token.push(msg.sender, leftover);
        usdg.push(msg.sender, usdgOut);

        emit Sold(msg.sender, asset, router, assetIn - leftover, usdgOut);
    }
}
