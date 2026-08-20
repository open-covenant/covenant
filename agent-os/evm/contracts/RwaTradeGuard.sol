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

    address public owner;
    address public pendingOwner;

    /// The L2 sequencer uptime feed. Zero disables the check (for an L1 or a
    /// test); on Robinhood Chain it should be set.
    IAggregatorV3 public sequencerUptimeFeed;
    /// How long after the sequencer comes back a price is still distrusted.
    uint64 public sequencerGracePeriod = 3600;

    mapping(address => AssetConfig) private _assets;

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
