// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import {PackedUserOperation} from "@openzeppelin/contracts/interfaces/draft-IERC4337.sol";
import {
    IERC7579Validator,
    MODULE_TYPE_VALIDATOR,
    VALIDATION_FAILED,
    VALIDATION_SUCCESS
} from "@openzeppelin/contracts/interfaces/draft-IERC7579.sol";

contract PaliECDSAValidatorModule is IERC7579Validator {
    bytes4 internal constant EIP1271_SUCCESS = 0x1626ba7e;
    bytes4 internal constant EIP1271_FAILED = 0xffffffff;

    error InvalidECDSAOwner(address owner);
    error InvalidECDSAThreshold(uint64 ownerCount, uint64 threshold);
    error DuplicateECDSAOwner(address owner);

    mapping(address account => address[]) private _owners;
    mapping(address account => mapping(address owner => bool)) private _isOwner;
    mapping(address account => uint64) private _threshold;

    function onInstall(bytes calldata initData) public override {
        (address[] memory owners_, uint64 threshold_) = abi.decode(initData, (address[], uint64));
        _setOwners(msg.sender, owners_, threshold_);
    }

    function onUninstall(bytes calldata) public override {
        address[] storage owners_ = _owners[msg.sender];
        for (uint256 i = 0; i < owners_.length; ++i) {
            delete _isOwner[msg.sender][owners_[i]];
        }
        delete _owners[msg.sender];
        delete _threshold[msg.sender];
    }

    function owners(address account) external view returns (address[] memory) {
        return _owners[account];
    }

    function threshold(address account) external view returns (uint64) {
        return _threshold[account];
    }

    function isOwner(address account, address owner) public view returns (bool) {
        return _isOwner[account][owner];
    }

    function isModuleType(uint256 moduleTypeId) external pure override returns (bool) {
        return moduleTypeId == MODULE_TYPE_VALIDATOR;
    }

    function isInitialized(address account) external view returns (bool) {
        return _threshold[account] != 0;
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
        return _validateSignature(sender, hash, signature) ? EIP1271_SUCCESS : EIP1271_FAILED;
    }

    function _validateSignature(address account, bytes32 hash, bytes calldata signature) internal view returns (bool) {
        uint64 threshold_ = _threshold[account];
        if (threshold_ == 0) {
            return false;
        }

        if (signature.length == 65) {
            (address signer, ECDSA.RecoverError err,) = ECDSA.tryRecoverCalldata(hash, signature);
            return err == ECDSA.RecoverError.NoError && threshold_ == 1 && _isOwner[account][signer];
        }

        bytes[] memory signatures;
        try this.decodeSignatures(signature) returns (bytes[] memory decodedSignatures) {
            signatures = decodedSignatures;
        } catch {
            return false;
        }

        if (signatures.length < threshold_) {
            return false;
        }

        address[] memory seen = new address[](signatures.length);
        uint64 validSignatures;
        for (uint256 i = 0; i < signatures.length; ++i) {
            (address signer, ECDSA.RecoverError err,) = ECDSA.tryRecover(hash, signatures[i]);
            if (err != ECDSA.RecoverError.NoError || !_isOwner[account][signer] || _contains(seen, i, signer)) {
                return false;
            }

            seen[i] = signer;
            unchecked {
                ++validSignatures;
            }
            if (validSignatures >= threshold_) {
                return true;
            }
        }

        return false;
    }

    function decodeSignatures(bytes calldata signature) external pure returns (bytes[] memory) {
        return abi.decode(signature, (bytes[]));
    }

    function _setOwners(address account, address[] memory owners_, uint64 threshold_) private {
        if (threshold_ == 0 || threshold_ > owners_.length) {
            revert InvalidECDSAThreshold(uint64(owners_.length), threshold_);
        }

        for (uint256 i = 0; i < owners_.length; ++i) {
            address owner = owners_[i];
            if (owner == address(0)) {
                revert InvalidECDSAOwner(owner);
            }
            if (_isOwner[account][owner]) {
                revert DuplicateECDSAOwner(owner);
            }
            _isOwner[account][owner] = true;
            _owners[account].push(owner);
        }
        _threshold[account] = threshold_;
    }

    function _contains(address[] memory values, uint256 length, address value) private pure returns (bool) {
        for (uint256 i = 0; i < length; ++i) {
            if (values[i] == value) {
                return true;
            }
        }
        return false;
    }
}
