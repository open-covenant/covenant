// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {IERC20} from "./interfaces/IERC20.sol";
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
///         The executor holds no funds between calls and resets its approval to
///         zero, so an allowlisted venue that misbehaves can take at most this
///         trade's `maxUsdgIn` and still cannot deliver less than `minAssetOut`
///         without reverting the whole trade.
///
/// @dev    Deployed alongside `RwaTradeGuard` on Robinhood Chain. USDG (6dp) is
///         the settlement asset. `swapData` is swap calldata built off-chain for
///         the allowlisted `router` (Uniswap, 1inch, any AMM); the allowlist and
///         the balance-delta check are what make an opaque call safe to route.
contract GuardedTradeExecutor {
    using TokenTransfers for IERC20;

    RwaTradeGuard public immutable guard;
    IERC20 public immutable usdg;
    address public owner;
    address public pendingOwner;

    mapping(address => bool) public routerAllowed;

    bool private _entered;

    event OwnershipTransferStarted(address indexed from, address indexed to);
    event OwnershipTransferred(address indexed from, address indexed to);
    event RouterSet(address indexed router, bool allowed);
    event Bought(
        address indexed agent, address indexed asset, address indexed router, uint256 usdgSpent, uint256 assetOut
    );

    error NotOwner();
    error ZeroAddress();
    error RouterNotAllowed(address router);
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

    constructor(address guard_, address usdg_, address initialOwner) {
        if (guard_ == address(0) || usdg_ == address(0) || initialOwner == address(0)) {
            revert ZeroAddress();
        }
        guard = RwaTradeGuard(guard_);
        usdg = IERC20(usdg_);
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
        guard.checkTrade(asset, minAssetOut, quotedPriceUsdE8);

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
}
