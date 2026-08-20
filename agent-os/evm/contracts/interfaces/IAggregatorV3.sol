// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// @notice The Chainlink price-feed and L2 sequencer-uptime interface the RWA
///         guard reads. Stock Token feeds return the per-token price already
///         folded with the corporate-action multiplier; the sequencer feed
///         returns 0 for up and 1 for down, with `startedAt` marking the last
///         status change for the grace window.
interface IAggregatorV3 {
    function decimals() external view returns (uint8);

    function latestRoundData()
        external
        view
        returns (
            uint80 roundId,
            int256 answer,
            uint256 startedAt,
            uint256 updatedAt,
            uint80 answeredInRound
        );
}
