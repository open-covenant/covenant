// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// The subset of Permit2's allowance surface a router-routing contract needs.
/// Uniswap's UniversalRouter pulls a payer's ERC-20s through Permit2 rather than
/// a plain allowance, so a contract that routes through it holds its allowance
/// here.
interface IPermit2 {
    function approve(address token, address spender, uint160 amount, uint48 expiration) external;

    function allowance(address user, address token, address spender)
        external
        view
        returns (uint160 amount, uint48 expiration, uint48 nonce);
}
