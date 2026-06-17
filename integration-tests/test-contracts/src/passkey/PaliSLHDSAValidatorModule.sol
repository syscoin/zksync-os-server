// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {PackedUserOperation} from "@openzeppelin/contracts/interfaces/draft-IERC4337.sol";
import {
    IERC7579Validator,
    MODULE_TYPE_VALIDATOR,
    VALIDATION_FAILED,
    VALIDATION_SUCCESS
} from "@openzeppelin/contracts/interfaces/draft-IERC7579.sol";

interface ISLHDSAVerifier {
    function verify(bytes32 pkSeed, bytes32 pkRoot, bytes32 message, bytes calldata sig) external view returns (bool);
}

contract PaliSLHDSAValidatorModule is IERC7579Validator {
    bytes4 internal constant EIP1271_SUCCESS = 0x1626ba7e;
    bytes4 internal constant EIP1271_FAILED = 0xffffffff;
    bytes32 internal constant N_MASK = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF00000000000000000000000000000000;
    uint256 internal constant SLH_DSA_SHA2_128_24_SIGNATURE_LENGTH = 3856;

    struct AuthData {
        bytes32 pkSeed;
        bytes32 pkRoot;
    }

    error InvalidSLHDSAAuthConfig();
    error InvalidSLHDSAVerifier();

    ISLHDSAVerifier public immutable verifier;

    mapping(address account => AuthData) private _authData;

    constructor(ISLHDSAVerifier verifier_) {
        if (address(verifier_) == address(0)) {
            revert InvalidSLHDSAVerifier();
        }
        verifier = verifier_;
    }

    function onInstall(bytes calldata initData) public override {
        AuthData memory authData_ = abi.decode(initData, (AuthData));
        if (!_isCanonicalKeyPart(authData_.pkSeed) || !_isCanonicalKeyPart(authData_.pkRoot)) {
            revert InvalidSLHDSAAuthConfig();
        }
        _authData[msg.sender] = authData_;
    }

    function onUninstall(bytes calldata) public override {
        delete _authData[msg.sender];
    }

    function authData(address account) external view returns (AuthData memory) {
        return _authData[account];
    }

    function isModuleType(uint256 moduleTypeId) external pure override returns (bool) {
        return moduleTypeId == MODULE_TYPE_VALIDATOR;
    }

    function isInitialized(address account) external view returns (bool) {
        return _authData[account].pkSeed != bytes32(0);
    }

    function validateUserOp(PackedUserOperation calldata userOp, bytes32 userOpHash)
        external
        view
        override
        returns (uint256)
    {
        return _validateSignature(userOp.sender, userOpHash, userOp.signature) ? VALIDATION_SUCCESS : VALIDATION_FAILED;
    }

    function isValidSignatureWithSender(address sender, bytes32 hash, bytes calldata signature)
        external
        view
        override
        returns (bytes4)
    {
        return _validateSignature(_validationAccount(sender), hash, signature) ? EIP1271_SUCCESS : EIP1271_FAILED;
    }

    function _validateSignature(address account, bytes32 hash, bytes calldata signature) internal view returns (bool) {
        AuthData memory authData_ = _authData[account];
        if (authData_.pkSeed == bytes32(0) || signature.length != SLH_DSA_SHA2_128_24_SIGNATURE_LENGTH) {
            return false;
        }

        try verifier.verify(authData_.pkSeed, authData_.pkRoot, hash, signature) returns (bool valid) {
            return valid;
        } catch {
            return false;
        }
    }

    function _validationAccount(address sender) private view returns (address) {
        return _authData[msg.sender].pkSeed != bytes32(0) ? msg.sender : sender;
    }

    function _isCanonicalKeyPart(bytes32 value) private pure returns (bool) {
        return value != bytes32(0) && value == (value & N_MASK);
    }
}
