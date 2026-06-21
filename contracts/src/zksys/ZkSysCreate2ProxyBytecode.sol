// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {TransparentUpgradeableProxy} from "@openzeppelin/contracts-v4/proxy/transparent/TransparentUpgradeableProxy.sol";

// SYSCOIN: Local inspection target for deriving the canonical L2 zkSYS proxy
// CREATE2 address from the exact TransparentUpgradeableProxy v4 creation code.
contract ZkSysCreate2ProxyBytecode is TransparentUpgradeableProxy {
    constructor(address implementation, address admin, bytes memory data)
        TransparentUpgradeableProxy(implementation, admin, data)
    {}
}
