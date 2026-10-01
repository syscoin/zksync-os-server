// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// AnvilComponentOnly assumption, NOT Bitcoin availability or finality.
/// Matches the compact native precompile's raw ABI, without cold storage reads.
contract ComponentBitcoinDAAvailabilityMock {
    fallback() external {
        require(msg.data.length == 32);
        assembly ("memory-safe") {
            calldatacopy(0, 0, 32)
            return(0, 32)
        }
    }
}

/// Exercises the actual caller-side 1400-gas allowance (not intrinsic tx gas).
contract ComponentBitcoinDAGasProbe {
    function check(bytes calldata input, bool expectedSuccess) external returns (bool) {
        (bool success, bytes memory output) = address(0x63).call{gas: 1400}(input);
        return success == expectedSuccess
            && (!success || (output.length == 32 && keccak256(output) == keccak256(input)));
    }
}
