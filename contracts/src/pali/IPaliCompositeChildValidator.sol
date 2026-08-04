// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {PackedUserOperation} from "@openzeppelin/contracts/interfaces/draft-IERC4337.sol";
import {IERC7579Validator} from "@openzeppelin/contracts/interfaces/draft-IERC7579.sol";

/// @dev Opt-in module type for validators that can safely receive full UserOperation
/// context through Pali's composite validator.
uint256 constant PALI_MODULE_TYPE_COMPOSITE_CHILD = uint256(keccak256("pali.validator.composite-child.v1"));

interface IPaliCompositeChildValidator is IERC7579Validator {
    /// @notice Validate a child vote while preserving the account and complete UserOperation context.
    /// @dev The composite passes the child signature separately because userOp.signature contains the
    /// aggregate composite envelope. Implementations MUST return either VALIDATION_SUCCESS or
    /// VALIDATION_FAILED; time ranges and signature aggregators are not supported by composite v1.
    function validateUserOpWithSender(
        address account,
        PackedUserOperation calldata userOp,
        bytes32 userOpHash,
        bytes calldata signature
    ) external returns (uint256);
}
