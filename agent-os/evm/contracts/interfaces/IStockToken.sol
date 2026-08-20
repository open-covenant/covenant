// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// @notice The Robinhood Stock Token accessors the RWA guard reads: the ERC-8056
///         `uiMultiplier` (share ratio, 1e18-scaled) and the token's own
///         oracle-paused flag. A zero multiplier means the token is not
///         initialized and nothing should trade against it.
interface IStockToken {
    function uiMultiplier() external view returns (uint256);

    function oraclePaused() external view returns (bool);
}
