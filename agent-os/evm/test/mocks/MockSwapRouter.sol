// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {IERC20} from "../../contracts/interfaces/IERC20.sol";

/// A constant-fill swap venue for exercising the executor. `swap` pulls
/// `amountIn` of `tokenIn` from the caller (the executor, which approved it) and
/// sends `amountOut` of `tokenOut` from its own inventory to `to`. The caller
/// chooses `amountOut`, so a short fill (`amountOut < minAssetOut`) and a
/// reverting venue are both reachable from tests.
contract MockSwapRouter {
    error Reverting();

    function swap(address tokenIn, uint256 amountIn, address tokenOut, uint256 amountOut, address to) external {
        IERC20(tokenIn).transferFrom(msg.sender, address(this), amountIn);
        IERC20(tokenOut).transfer(to, amountOut);
    }

    /// A venue that takes nothing and always reverts, to prove the executor
    /// surfaces a failed swap rather than settling a phantom fill.
    function revertingSwap() external pure {
        revert Reverting();
    }
}
