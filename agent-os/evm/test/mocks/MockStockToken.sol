// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {IStockToken} from "../../contracts/interfaces/IStockToken.sol";

/// A settable Stock Token exposing the ERC-8056 multiplier and the oracle-paused
/// flag the guard reads.
contract MockStockToken is IStockToken {
    uint256 public multiplier;
    bool public paused;

    constructor(uint256 multiplier_) {
        multiplier = multiplier_;
    }

    function uiMultiplier() external view returns (uint256) {
        return multiplier;
    }

    function oraclePaused() external view returns (bool) {
        return paused;
    }

    function setMultiplier(uint256 multiplier_) external {
        multiplier = multiplier_;
    }

    function setPaused(bool paused_) external {
        paused = paused_;
    }
}
