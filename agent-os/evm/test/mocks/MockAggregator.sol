// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {IAggregatorV3} from "../../contracts/interfaces/IAggregatorV3.sol";

/// A settable AggregatorV3, used both as a price feed (answer = price,
/// updatedAt) and as a sequencer uptime feed (answer = 0 up / 1 down,
/// startedAt = last restart).
contract MockAggregator is IAggregatorV3 {
    uint8 private immutable _decimals;
    int256 public answer;
    uint256 public updatedAt;
    uint256 public startedAt;

    constructor(uint8 decimals_) {
        _decimals = decimals_;
    }

    function decimals() external view returns (uint8) {
        return _decimals;
    }

    function set(int256 answer_, uint256 updatedAt_) external {
        answer = answer_;
        updatedAt = updatedAt_;
    }

    function setStartedAt(uint256 startedAt_) external {
        startedAt = startedAt_;
    }

    function latestRoundData()
        external
        view
        returns (uint80, int256, uint256, uint256, uint80)
    {
        return (1, answer, startedAt, updatedAt, 1);
    }
}
