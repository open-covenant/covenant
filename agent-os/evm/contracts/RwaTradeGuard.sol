// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {IAggregatorV3} from "./interfaces/IAggregatorV3.sol";
import {IStockToken} from "./interfaces/IStockToken.sol";

/// @title RwaTradeGuard: on-chain risk bounds for tokenized-equity trading.
/// @notice The on-chain half of `covenant-rwa-firewall`. A session key or a
///         4337 validator calls `checkTrade` before it executes a Stock Token
///         trade; a trade that breaks the asset's bounds reverts and never
///         reaches the venue. Where the daemon's copy is advisory, this one is
///         enforced by the chain: an agent cannot route around a `view` that its
///         own execution path calls.
///
///         The bounds are the ones a spend cap cannot express. The fill must sit
///         within a band of the asset's Chainlink price; the price must be fresh
///         (staleness bounds how long a frozen last print is trusted past a
///         close, but it is not itself a market-hours signal, and the tightness
///         of that bound is only `maxStalenessSecs`; an explicit market-hours
///         gate lives in the off-chain policy); the token must be initialized
///         (`uiMultiplier != 0`) and not oracle-paused; the notional at the live
///         price must be under the per-asset cap; and, on an L2, the sequencer
///         must be up. Notional is computed from the raw amount and the
///         multiplier-folded per-token feed, so the guard is multiplier-correct
///         without trusting a caller's per-share math.
///
///         Those bounds judge one trade. `commitTrade` adds the bound on the
///         hundred after it: a registered execution path charges each trade to
///         the trader's spend window, which decays back to zero over its own
///         duration, and refuses the trade that would push the running figure
///         past the cap.
///
/// @dev    Feeds are pinned to 8 decimals at registration so the answer is
///         directly USD*1e8; the multiplier is 1e18-scaled (ERC-8056). Only the
///         owner registers assets, so a look-alike token with a matching ticker
///         but a different contract is never tradeable through this guard.
contract RwaTradeGuard {
    uint256 private constant WAD = 1e18;
    uint256 private constant BPS = 10_000;

    struct AssetConfig {
        IAggregatorV3 feed;
        uint256 notionalCapUsdE8;
        uint32 bandBps;
        uint64 maxStalenessSecs;
    }

    /// A trader's running spend against the window, and when it was last moved.
    /// The recorded figure decays to zero over `spendWindowSecs`, so the pair is
    /// all the state a window needs.
    struct Spend {
        uint256 usdE8;
        uint64 at;
    }

    address public owner;
    address public pendingOwner;

    /// The L2 sequencer uptime feed. Zero disables the check (for an L1 or a
    /// test); on Robinhood Chain it should be set.
    IAggregatorV3 public sequencerUptimeFeed;
    /// How long after the sequencer comes back a price is still distrusted.
    uint64 public sequencerGracePeriod = 3600;

    /// How long a trader's spend takes to decay back to zero, and the most that
    /// may stand against them at once. A zero duration disables the window, which
    /// is the right setting only where something else bounds cumulative size.
    uint64 public spendWindowSecs;
    uint256 public spendWindowCapUsdE8;

    mapping(address => AssetConfig) private _assets;
    /// Contracts allowed to commit a trade against a trader's window. An
    /// execution path registers here; nothing else can spend someone's budget.
    mapping(address => bool) public executorAllowed;
    mapping(address => Spend) private _spent;

    event OwnershipTransferStarted(address indexed from, address indexed to);
    event OwnershipTransferred(address indexed from, address indexed to);
    event SequencerFeedSet(address indexed feed, uint64 gracePeriod);
    event AssetSet(
        address indexed asset,
        address indexed feed,
        uint256 notionalCapUsdE8,
        uint32 bandBps,
        uint64 maxStalenessSecs
    );
    event AssetRemoved(address indexed asset);
    event ExecutorSet(address indexed executor, bool allowed);
    event SpendWindowSet(uint64 windowSecs, uint256 capUsdE8);
    event TradeCommitted(address indexed trader, address indexed asset, uint256 notionalUsdE8, uint256 windowSpentUsdE8);

    error NotOwner();
    error ZeroAddress();
    error FeedNot8Decimals(uint8 decimals);
    error AssetNotEnabled(address asset);
    error SequencerDown();
    error PriceUnavailable();
    error OraclePaused();
    error MultiplierUnset();
    error StalePriceFeed(uint256 ageSecs, uint64 maxSecs);
    error PriceOutsideBand(uint256 quotedUsdE8, uint256 oracleUsdE8, uint256 diffBps, uint32 bandBps);
    error NotionalOverCap(uint256 notionalUsdE8, uint256 capUsdE8);
    error NotExecutor(address caller);
    error WindowCapExceeded(uint256 wouldBeUsdE8, uint256 capUsdE8);

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    constructor(address initialOwner) {
        if (initialOwner == address(0)) revert ZeroAddress();
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

    // --- configuration ---

    function setSequencerFeed(address feed, uint64 gracePeriod) external onlyOwner {
        sequencerUptimeFeed = IAggregatorV3(feed);
        sequencerGracePeriod = gracePeriod;
        emit SequencerFeedSet(feed, gracePeriod);
    }

    /// Register or update a tradeable asset. Pins the feed to 8 decimals so the
    /// answer is USD*1e8; reverts otherwise rather than silently misprice.
    function setAsset(
        address asset,
        address feed,
        uint256 notionalCapUsdE8,
        uint32 bandBps,
        uint64 maxStalenessSecs
    ) external onlyOwner {
        if (asset == address(0) || feed == address(0)) revert ZeroAddress();
        uint8 dec = IAggregatorV3(feed).decimals();
        if (dec != 8) revert FeedNot8Decimals(dec);
        _assets[asset] = AssetConfig({
            feed: IAggregatorV3(feed),
            notionalCapUsdE8: notionalCapUsdE8,
            bandBps: bandBps,
            maxStalenessSecs: maxStalenessSecs
        });
        emit AssetSet(asset, feed, notionalCapUsdE8, bandBps, maxStalenessSecs);
    }

    /// Register an execution path. Only a registered one can commit a trade, so
    /// a trader's window cannot be drained by anyone who feels like calling.
    function setExecutor(address executor, bool allowed) external onlyOwner {
        if (executor == address(0)) revert ZeroAddress();
        executorAllowed[executor] = allowed;
        emit ExecutorSet(executor, allowed);
    }

    /// Bound how much a single trader can move through the guard over time. The
    /// per-asset cap bounds one trade; this bounds the hundred trades after it.
    function setSpendWindow(uint64 windowSecs, uint256 capUsdE8) external onlyOwner {
        spendWindowSecs = windowSecs;
        spendWindowCapUsdE8 = capUsdE8;
        emit SpendWindowSet(windowSecs, capUsdE8);
    }

    function removeAsset(address asset) external onlyOwner {
        delete _assets[asset];
        emit AssetRemoved(asset);
    }

    function assetConfig(address asset) external view returns (AssetConfig memory) {
        return _assets[asset];
    }

    // --- the guard ---

    /// Judge a proposed trade. Reverts with a named reason if the trade breaks a
    /// bound; on success returns the notional it was sized at (USD*1e8) and the
    /// multiplier-adjusted share amount (1e18). A caller wires this in front of
    /// the swap it is about to sign.
    function checkTrade(address asset, uint256 rawAmount, uint256 quotedPriceUsdE8)
        external
        view
        returns (uint256 notionalUsdE8, uint256 sharesE18)
    {
        return _evaluate(asset, rawAmount, quotedPriceUsdE8);
    }

    /// Judge a trade and charge it to `trader`'s window. The execution path calls
    /// this instead of `checkTrade` so a trade that clears every per-trade bound
    /// still cannot be repeated past the trader's budget. Registered executors
    /// only, since this writes.
    function commitTrade(address trader, address asset, uint256 rawAmount, uint256 quotedPriceUsdE8)
        external
        returns (uint256 notionalUsdE8, uint256 sharesE18)
    {
        if (!executorAllowed[msg.sender]) revert NotExecutor(msg.sender);
        (notionalUsdE8, sharesE18) = _evaluate(asset, rawAmount, quotedPriceUsdE8);

        if (spendWindowSecs != 0) {
            uint256 running = _decayed(trader) + notionalUsdE8;
            if (running > spendWindowCapUsdE8) revert WindowCapExceeded(running, spendWindowCapUsdE8);
            _spent[trader] = Spend({usdE8: running, at: uint64(block.timestamp)});
            emit TradeCommitted(trader, asset, notionalUsdE8, running);
        } else {
            emit TradeCommitted(trader, asset, notionalUsdE8, 0);
        }
    }

    /// What still stands against a trader's window right now.
    function spentInWindow(address trader) external view returns (uint256) {
        return _decayed(trader);
    }

    /// A trader's recorded spend, faded by how much of the window has passed. The
    /// budget refills smoothly rather than resetting on a boundary, so a trader
    /// cannot wait for a clock tick and spend the cap twice back to back. What it
    /// bounds is how much can stand against a trader at any instant; over a full
    /// window the refill lets a little more than the cap through in total.
    function _decayed(address trader) private view returns (uint256) {
        Spend memory prior = _spent[trader];
        if (prior.usdE8 == 0 || spendWindowSecs == 0) return 0;
        uint256 elapsed = block.timestamp - prior.at;
        if (elapsed >= spendWindowSecs) return 0;
        return prior.usdE8 - (prior.usdE8 * elapsed) / spendWindowSecs;
    }

    function _evaluate(address asset, uint256 rawAmount, uint256 quotedPriceUsdE8)
        private
        view
        returns (uint256 notionalUsdE8, uint256 sharesE18)
    {
        AssetConfig memory cfg = _assets[asset];
        if (address(cfg.feed) == address(0)) revert AssetNotEnabled(asset);

        _requireSequencerUp();

        IStockToken token = IStockToken(asset);
        if (token.oraclePaused()) revert OraclePaused();
        uint256 multiplier = token.uiMultiplier();
        if (multiplier == 0) revert MultiplierUnset();

        (, int256 answer,, uint256 updatedAt,) = cfg.feed.latestRoundData();
        if (answer <= 0) revert PriceUnavailable();
        uint256 oracle = uint256(answer);

        // A feed timestamped in the future is not a usable price; refuse it
        // rather than let the age subtraction underflow.
        if (updatedAt > block.timestamp) revert PriceUnavailable();
        uint256 age = block.timestamp - updatedAt;
        if (age > cfg.maxStalenessSecs) revert StalePriceFeed(age, cfg.maxStalenessSecs);

        uint256 diff = quotedPriceUsdE8 > oracle ? quotedPriceUsdE8 - oracle : oracle - quotedPriceUsdE8;
        // Exact comparison so a quote a fraction of a bp beyond the band cannot
        // floor back in.
        if (diff * BPS > oracle * cfg.bandBps) {
            revert PriceOutsideBand(quotedPriceUsdE8, oracle, (diff * BPS) / oracle, cfg.bandBps);
        }

        notionalUsdE8 = (rawAmount * oracle) / WAD;
        if (notionalUsdE8 > cfg.notionalCapUsdE8) {
            revert NotionalOverCap(notionalUsdE8, cfg.notionalCapUsdE8);
        }

        sharesE18 = (rawAmount * multiplier) / WAD;
    }

    function _requireSequencerUp() private view {
        if (address(sequencerUptimeFeed) == address(0)) return;
        (, int256 up, uint256 startedAt,,) = sequencerUptimeFeed.latestRoundData();
        // 0 = up, 1 = down; startedAt 0 is an invalid round. Distrust a price
        // while down, on an invalid or future round, or inside the grace window
        // after a restart.
        if (
            up != 0 || startedAt == 0 || startedAt > block.timestamp
                || block.timestamp - startedAt <= sequencerGracePeriod
        ) {
            revert SequencerDown();
        }
    }
}
