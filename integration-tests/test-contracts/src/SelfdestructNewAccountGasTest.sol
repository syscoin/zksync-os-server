// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

/// @dev Helper that selfdestructs to a given beneficiary.
///      Deployed in the constructor (previous tx), so EIP-6780
///      does NOT destroy it — only balance is transferred.
contract SdTarget {
    function destroy(address payable beneficiary) external {
        assembly {
            selfdestruct(beneficiary)
        }
    }

    receive() external payable {}
}

/// @dev Test for found_1.md: SELFDESTRUCT charges 25,000 NEWACCOUNT
///      gas post-Cancun when sending to an empty beneficiary.
///
/// Per the bug report, go-ethereum skips the 25,000 charge post-Cancun:
///   if evm.chainRules.IsCancun { return gas, nil }
///
/// This test measures gas for two scenarios:
///   1. SELFDESTRUCT to an EMPTY beneficiary (nonce=0, no code, balance=0)
///   2. SELFDESTRUCT to a NON-EMPTY beneficiary (has balance)
///
/// If the 25,000 NEWACCOUNT is charged, scenario 1 costs ~25,000 more than
/// scenario 2. If skipped (as go-ethereum does), they cost approximately
/// the same (both cold-access 2,600 + overhead).
///
/// The gas measurements are stored in storage. Any difference between
/// zksync-os and REVM will surface via the consistency checker.
contract SelfdestructNewAccountGasTest {
    SdTarget public target1;
    SdTarget public target2;

    uint256 public gasToEmpty;
    uint256 public gasToNonEmpty;
    uint256 public gasDifference;

    constructor() {
        // Deploy targets in constructor (previous tx from the test call).
        target1 = new SdTarget();
        target2 = new SdTarget();
    }

    receive() external payable {}

    /// @dev Fund both targets, then selfdestruct each to different beneficiaries.
    ///      emptyBeneficiary must be a fresh address with zero balance/nonce/code.
    ///      nonEmptyBeneficiary should already have some balance.
    function measure(
        address payable emptyBeneficiary,
        address payable nonEmptyBeneficiary
    ) external payable {
        require(msg.value >= 0.2 ether, "need >= 0.2 ETH");

        // Fund target1
        (bool ok1,) = address(target1).call{value: 0.1 ether}("");
        require(ok1, "fund1 failed");

        // Fund target2
        (bool ok2,) = address(target2).call{value: 0.1 ether}("");
        require(ok2, "fund2 failed");

        // Scenario 1: SELFDESTRUCT to EMPTY beneficiary
        // Should include 25,000 NEWACCOUNT if the charge is applied
        uint256 gasBefore1 = gasleft();
        address(target1).call(
            abi.encodeWithSelector(SdTarget.destroy.selector, emptyBeneficiary)
        );
        gasToEmpty = gasBefore1 - gasleft();

        // Scenario 2: SELFDESTRUCT to NON-EMPTY beneficiary
        // No NEWACCOUNT charge (beneficiary is not empty)
        uint256 gasBefore2 = gasleft();
        address(target2).call(
            abi.encodeWithSelector(SdTarget.destroy.selector, nonEmptyBeneficiary)
        );
        gasToNonEmpty = gasBefore2 - gasleft();

        // Store the difference — should be ~25,000 if NEWACCOUNT is charged
        gasDifference = gasToEmpty > gasToNonEmpty
            ? gasToEmpty - gasToNonEmpty
            : gasToNonEmpty - gasToEmpty;
    }
}
