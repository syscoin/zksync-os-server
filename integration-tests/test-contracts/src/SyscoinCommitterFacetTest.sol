// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {CommitterFacet} from "@zksync-era/l1-contracts/contracts/state-transition/chain-deps/facets/Committer.sol";

struct SyscoinGatewayDAValidatorOutput {
    bytes32 stateDiffHash;
    bytes32[] blobsLinearHashes;
    bytes32[] blobsOpeningCommitments;
}

/// @dev Test-only composition of the pinned Syscoin committer and a local DA validator.
/// The fixture's authorized admin path points the child chain at this same implementation,
/// avoiding Syscoin's 0x63 precompile in the test VM.
contract SyscoinCommitterFacetTest is CommitterFacet {
    error InvalidBlobsDAInputLength(uint256 inputLength);
    error InvalidBlobsPublished(bytes32 publishedHash, bytes32 expectedHash);

    constructor(uint256 l1ChainId) CommitterFacet(l1ChainId) {}

    function checkDA(
        uint256,
        uint256,
        bytes32 l2DAValidatorOutputHash,
        bytes calldata operatorDAInput,
        uint256
    ) external pure returns (SyscoinGatewayDAValidatorOutput memory output) {
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
