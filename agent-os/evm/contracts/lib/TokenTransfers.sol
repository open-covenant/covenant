// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {IERC20} from "../interfaces/IERC20.sol";

/// @title TokenTransfers: non-reverting-token-safe ERC-20 moves.
/// @notice Treats a `false` return and a missing return alike, so USDG-style
///         tokens that return nothing on success still settle. Callers hold the
///         reentrancy guard; this library only moves value.
library TokenTransfers {
    error TokenTransferFailed();

    function pull(IERC20 token, address from, uint256 amount) internal {
        _call(token, abi.encodeCall(IERC20.transferFrom, (from, address(this), amount)));
    }

    function push(IERC20 token, address to, uint256 amount) internal {
        _call(token, abi.encodeCall(IERC20.transfer, (to, amount)));
    }

    function safeApprove(IERC20 token, address spender, uint256 amount) internal {
        _call(token, abi.encodeCall(IERC20.approve, (spender, amount)));
    }

    function _call(IERC20 token, bytes memory data) private {
        (bool success, bytes memory result) = address(token).call(data);
        if (!success || (result.length != 0 && !abi.decode(result, (bool)))) {
            revert TokenTransferFailed();
        }
    }
}
