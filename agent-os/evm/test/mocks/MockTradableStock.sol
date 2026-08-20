// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {IStockToken} from "../../contracts/interfaces/IStockToken.sol";

/// A Stock Token that is both guard-readable (ERC-8056 multiplier + oracle-paused
/// flag) and transferable, so the executor can receive it from a venue and
/// deliver it to the agent. 18 decimals, like a real Robinhood Stock Token.
contract MockTradableStock is IStockToken {
    string public constant name = "Mock Stock";
    string public constant symbol = "MSTK";
    uint8 public constant decimals = 18;

    uint256 public multiplier;
    bool public paused;

    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

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

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        _move(msg.sender, to, amount);
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        uint256 allowed = allowance[from][msg.sender];
        if (allowed != type(uint256).max) allowance[from][msg.sender] = allowed - amount;
        _move(from, to, amount);
        return true;
    }

    function _move(address from, address to, uint256 amount) private {
        require(balanceOf[from] >= amount, "balance");
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
    }
}
