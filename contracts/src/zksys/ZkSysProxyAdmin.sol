// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {ProxyAdmin} from "@openzeppelin/contracts-v4/proxy/transparent/ProxyAdmin.sol";

// SYSCOIN: ProxyAdmin variant that can be safely deployed through the universal
// CREATE2 deployer while assigning upgrade authority to the intended admin.
contract ZkSysProxyAdmin is ProxyAdmin {
    constructor(address owner_) {
        transferOwnership(owner_);
    }
}
