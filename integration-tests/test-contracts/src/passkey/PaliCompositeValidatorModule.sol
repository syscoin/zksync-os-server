// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {PackedUserOperation} from "@openzeppelin/contracts/interfaces/draft-IERC4337.sol";
import {
    IERC7579Module,
    IERC7579ModuleConfig,
    IERC7579Validator,
    MODULE_TYPE_VALIDATOR,
    VALIDATION_FAILED,
    VALIDATION_SUCCESS
} from "@openzeppelin/contracts/interfaces/draft-IERC7579.sol";

contract PaliCompositeValidatorModule is IERC7579Validator {
    bytes4 internal constant EIP1271_SUCCESS = 0x1626ba7e;
    bytes4 internal constant EIP1271_FAILED = 0xffffffff;

    error DuplicateChildValidator(address validator);
    error InvalidChildValidator(address validator);
    error InvalidCompositeThreshold(uint64 childCount, uint64 threshold);

    mapping(address account => address[]) private _children;
    mapping(address account => uint64) private _threshold;

    function onInstall(bytes calldata initData) public override {
        (address[] memory children, uint64 threshold_) = abi.decode(initData, (address[], uint64));
        _setPolicy(msg.sender, children, threshold_);
    }

    function onUninstall(bytes calldata) public override {
        delete _children[msg.sender];
        delete _threshold[msg.sender];
    }

    function childValidators(address account) external view returns (address[] memory) {
        return _children[account];
    }

    function threshold(address account) external view returns (uint64) {
        return _threshold[account];
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
        address[] storage children = _children[account];
        uint64 threshold_ = _threshold[account];
        if (threshold_ == 0 || children.length == 0) {
            return false;
        }

        bytes[] memory childSignatures;
        try this.decodeChildSignatures(signature) returns (bytes[] memory decodedSignatures) {
            childSignatures = decodedSignatures;
        } catch {
            return false;
        }

        if (childSignatures.length != children.length) {
            return false;
        }

        uint64 validChildren;
        for (uint256 i = 0; i < children.length; ++i) {
            if (childSignatures[i].length == 0) {
                continue;
            }

            try IERC7579Validator(children[i]).isValidSignatureWithSender(account, hash, childSignatures[i]) returns (
                bytes4 magic
            ) {
                if (magic == EIP1271_SUCCESS) {
                    unchecked {
                        ++validChildren;
                    }
                    if (validChildren >= threshold_) {
                        return true;
                    }
                }
            } catch {
                return false;
            }
        }

        return false;
    }

    function decodeChildSignatures(bytes calldata signature) external pure returns (bytes[] memory) {
        return abi.decode(signature, (bytes[]));
    }

    function _setPolicy(address account, address[] memory children, uint64 threshold_) private {
        if (threshold_ == 0 || threshold_ > children.length) {
            revert InvalidCompositeThreshold(uint64(children.length), threshold_);
        }

        for (uint256 i = 0; i < children.length; ++i) {
            address child = children[i];
            if (
                child == address(0) || !IERC7579Module(child).isModuleType(MODULE_TYPE_VALIDATOR)
                    || !IERC7579ModuleConfig(account).isModuleInstalled(MODULE_TYPE_VALIDATOR, child, "")
            ) {
                revert InvalidChildValidator(child);
            }

            for (uint256 j = 0; j < i; ++j) {
                if (children[j] == child) {
                    revert DuplicateChildValidator(child);
                }
            }
            _children[account].push(child);
        }
        _threshold[account] = threshold_;
    }
}
