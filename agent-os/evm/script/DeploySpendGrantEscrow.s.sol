// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Script, console2} from "forge-std/Script.sol";
import {SpendGrantEscrow} from "../contracts/SpendGrantEscrow.sol";
import {IERC20} from "../contracts/interfaces/IERC20.sol";

/// Deploys SpendGrantEscrow to Robinhood Chain. Reads roles + the USDG address
/// from the environment so the same script targets testnet and mainnet by env
/// alone. The contract ships paused; `admin` unpauses only after the role set
/// is verified onchain and a pilot grant funds within MAX_ESCROW.
///
///   USDG   canonical Robinhood-Chain USDG (6dp), 0x5fc5360D…d168 on mainnet
///   ADMIN  timelock/Safe that unpauses and rotates attestor/gateway
///   ATTESTOR         covenantd quality-verdict signer (the spec gate)
///   TREASURY         fee sink
///   EMERGENCY_ADMIN  can pause + adminClose a single stuck hold
///   GATEWAY          may release delivered v1 calls alongside the spender
contract DeploySpendGrantEscrow is Script {
    function run() external returns (SpendGrantEscrow escrow) {
        IERC20 usd = IERC20(vm.envAddress("USDG"));
        address admin = vm.envAddress("ADMIN");
        address attestor = vm.envAddress("ATTESTOR");
        address treasury = vm.envAddress("TREASURY");
        address emergencyAdmin = vm.envAddress("EMERGENCY_ADMIN");
        address gateway = vm.envAddress("GATEWAY");

        vm.startBroadcast();
        escrow = new SpendGrantEscrow(usd, admin, attestor, treasury, emergencyAdmin, gateway);
        vm.stopBroadcast();

        console2.log("SpendGrantEscrow", address(escrow));
        console2.log("chainId", block.chainid);
        console2.log("paused", escrow.paused());
    }
}
