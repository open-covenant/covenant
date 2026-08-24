// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// Records what a caller granted, enough to assert the executor primes a venue
/// the way Permit2 expects.
contract MockPermit2 {
    struct Allowance {
        uint160 amount;
        uint48 expiration;
        uint48 nonce;
    }

    mapping(address => mapping(address => mapping(address => Allowance))) private _allowance;

    function approve(address token, address spender, uint160 amount, uint48 expiration) external {
        Allowance storage a = _allowance[msg.sender][token][spender];
        a.amount = amount;
        a.expiration = expiration;
    }

    function allowance(address user, address token, address spender)
        external
        view
        returns (uint160, uint48, uint48)
    {
        Allowance memory a = _allowance[user][token][spender];
        return (a.amount, a.expiration, a.nonce);
    }
}
