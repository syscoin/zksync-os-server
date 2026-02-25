// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

/// @dev Helper that accesses BALANCE of a target address and then reverts.
contract TouchAndRevert {
    function touchAndRevert(address target) external {
        assembly {
            // mstore prevents the optimizer from removing the balance() call
            mstore(0, balance(target))
        }
        revert("intentional revert");
    }
}

/// @dev Tests EIP-2929 warm/cold behavior with access lists and reverting calls.
///
/// Storage layout:
///   slot 0: helper (address)
///   slot 1: callSuccess (bool)
///   slot 2: balanceGasWithAccessList (uint256)
///   slot 3: balanceGasWithoutAccessList (uint256)
contract WarmAfterRevertTest {
    TouchAndRevert public helper;                  // slot 0
    bool public callSuccess;                       // slot 1
    uint256 public balanceGasWithAccessList;        // slot 2
    uint256 public balanceGasWithoutAccessList;     // slot 3

    constructor() {
        helper = new TouchAndRevert();
    }

    /// @dev Test warm status for two addresses after a reverting call:
    ///   - `accessListTarget`: included in the tx access list (EIP-2930)
    ///   - `coldTarget`: NOT in the access list, only warmed inside reverting call
    ///
    /// The helper touches coldTarget via BALANCE, then reverts.
    ///
    /// After the revert:
    ///   - accessListTarget should stay warm if the EVM preserves access list
    ///     entries across reverts (REVM does via WarmAddresses)
    ///   - coldTarget's warm status depends on whether the EVM reverts
    ///     execution-level warming (both REVM and zksync-os do)
    function testWarmAfterRevert(
        address accessListTarget,
        address coldTarget
    ) external {
        // CALL helper: touches BALANCE(coldTarget), then reverts.
        (bool success,) = address(helper).call(
            abi.encodeWithSelector(
                TouchAndRevert.touchAndRevert.selector,
                coldTarget
            )
        );
        callSuccess = success;

        // Measure BALANCE gas for the access-list-warmed address.
        // mstore(0, ...) prevents the Yul optimizer from removing balance().
        {
            uint256 gasBefore = gasleft();
            assembly {
                mstore(0, balance(accessListTarget))
            }
            balanceGasWithAccessList = gasBefore - gasleft();
        }

        // Measure BALANCE gas for the cold (non-access-list) address.
        {
            uint256 gasBefore = gasleft();
            assembly {
                mstore(0, balance(coldTarget))
            }
            balanceGasWithoutAccessList = gasBefore - gasleft();
        }
    }
}
