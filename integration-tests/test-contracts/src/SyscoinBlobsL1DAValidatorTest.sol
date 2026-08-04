// SPDX-License-Identifier: MIT

pragma solidity 0.8.28;

struct SyscoinL1DAValidatorOutput {
    bytes32 stateDiffHash;
    bytes32[] blobsLinearHashes;
    bytes32[] blobsOpeningCommitments;
}

/// @dev Anvil test replacement for the pinned v31 snapshot's stock EIP-4844 validator.
/// Bitcoin DA publication and finality are checked by the integration harness; this contract
/// preserves the on-chain length and commitment checks without requiring Syscoin's 0x63 precompile.
contract SyscoinBlobsL1DAValidatorTest {
    error InvalidBlobsDAInputLength(uint256 inputLength);
    error InvalidBlobsPublished(bytes32 publishedHash, bytes32 expectedHash);

    function checkDA(
        uint256,
        uint256,
        bytes32 l2DAValidatorOutputHash,
        bytes calldata operatorDAInput,
        uint256
    ) external pure returns (SyscoinL1DAValidatorOutput memory output) {
        if (operatorDAInput.length == 0 || operatorDAInput.length % 32 != 0 || operatorDAInput.length / 32 > 32) {
            revert InvalidBlobsDAInputLength(operatorDAInput.length);
        }

        bytes32 publishedHash = keccak256(operatorDAInput);
        if (publishedHash != l2DAValidatorOutputHash) {
            revert InvalidBlobsPublished(publishedHash, l2DAValidatorOutputHash);
        }

        output.blobsLinearHashes = new bytes32[](0);
        output.blobsOpeningCommitments = new bytes32[](0);
    }
}
