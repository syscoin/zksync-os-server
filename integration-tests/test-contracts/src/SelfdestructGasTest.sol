// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

/// @dev Contract that self-destructs to a given beneficiary.
contract SelfdestructTarget {
    function destroyTo(address payable beneficiary) external {
        assembly {
            selfdestruct(beneficiary)
        }
    }

    receive() external payable {}
}

/// @dev Regression test for post-Cancun SELFDESTRUCT gas behavior.
///
/// Post-Cancun (EIP-6780), SELFDESTRUCT does NOT destroy the contract unless
/// it was created in the same transaction. However, the gas rules are unchanged:
/// NEWACCOUNT (25,000) IS still charged when sending to an empty beneficiary.
/// Both zksync-os and REVM agree on this charge.
///
/// The test measures gas consumed by the selfdestruct call and stores it.
/// Both implementations should produce the same gasUsed value (~32,700).
contract SelfdestructGasTest {
    SelfdestructTarget public target;

    /// Stored gas measurement — differs between correct/buggy EVM.
    uint256 public gasUsed;

    constructor() {
        target = new SelfdestructTarget();
    }

    /// @dev Fund target, then trigger selfdestruct to an empty beneficiary
    /// and record the gas consumed. No gas limit — both EVMs succeed,
    /// but the stored gasUsed value will differ if NEWACCOUNT is charged.
    function testSelfdestructToEmpty(address payable beneficiary) external payable {
        // Fund the target so it has non-zero balance for selfdestruct transfer
        (bool fundOk,) = address(target).call{value: msg.value}("");
        require(fundOk, "funding failed");

        uint256 gasBefore = gasleft();
        // Low-level call because SELFDESTRUCT halts execution (no ABI return data)
        address(target).call(
            abi.encodeWithSelector(SelfdestructTarget.destroyTo.selector, beneficiary)
        );
        gasUsed = gasBefore - gasleft();
    }

    receive() external payable {}
}
