// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {
    ITransactionFilterer
} from "@zksync-era/l1-contracts/contracts/state-transition/chain-interfaces/ITransactionFilterer.sol";

/// @dev Direct-L1 v31 fixture shim for its mixed asset-router / transaction-filter versions.
/// The fixture's L1 router emits the legacy finalize selector, which its L2 router still supports,
/// but the newer filter rejects. Gateway fixtures retain their full institutional filter.
contract LegacyDepositTransactionFiltererTest is ITransactionFilterer {
    address private constant L2_TO_L1_MESSENGER = address(0x8008);
    address private constant L2_ASSET_ROUTER = address(0x10003);
    address private constant MIN_ALLOWED_ADDRESS = address(0x10000);

    function isTransactionAllowed(address, address contractL2, uint256, uint256, bytes memory, address)
        external
        pure
        returns (bool)
    {
        // SYSCOIN: The direct-L1 fixture also exercises priority-tx pubdata sealing through
        // the canonical L1 messenger; production transaction filters are not replaced by this shim.
        return contractL2 > MIN_ALLOWED_ADDRESS || contractL2 == L2_ASSET_ROUTER || contractL2 == L2_TO_L1_MESSENGER;
    }
}
