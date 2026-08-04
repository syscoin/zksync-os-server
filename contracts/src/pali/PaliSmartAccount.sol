// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {IEntryPoint, PackedUserOperation} from "@openzeppelin/contracts/interfaces/draft-IERC4337.sol";
import {IERC1271} from "@openzeppelin/contracts/interfaces/IERC1271.sol";
import {
    IERC7579Validator,
    MODULE_TYPE_EXECUTOR,
    MODULE_TYPE_FALLBACK,
    MODULE_TYPE_HOOK,
    MODULE_TYPE_VALIDATOR,
    VALIDATION_FAILED
} from "@openzeppelin/contracts/interfaces/draft-IERC7579.sol";
import {AccountERC7579Hooked} from "@openzeppelin/contracts/account/extensions/draft-AccountERC7579Hooked.sol";
import {AccountERC7579} from "@openzeppelin/contracts/account/extensions/draft-AccountERC7579.sol";
import {Calldata} from "@openzeppelin/contracts/utils/Calldata.sol";
import {EIP712} from "@openzeppelin/contracts/utils/cryptography/EIP712.sol";
import {AbstractSigner} from "@openzeppelin/contracts/utils/cryptography/signers/AbstractSigner.sol";
import {ERC7739} from "@openzeppelin/contracts/utils/cryptography/signers/draft-ERC7739.sol";

contract PaliSmartAccount is AccountERC7579Hooked, ERC7739 {
    struct ModuleInit {
        address module;
        bytes data;
    }

    error AlreadyInitialized();
    error CannotUninstallActiveValidator(address validator);
    error InvalidEntryPoint();
    error InvalidInitialValidator();
    error TooManyInitialHooks();

    event ActiveValidatorChanged(address indexed validator);

    bool private _initialized;
    IEntryPoint private immutable _entryPoint;
    address public activeValidator;

    constructor(IEntryPoint entryPoint_) EIP712("pali.smart-account.erc1271", "1") {
        if (address(entryPoint_) == address(0)) {
            revert InvalidEntryPoint();
        }
        _initialized = true;
        _entryPoint = entryPoint_;
    }

    function entryPoint() public view override returns (IEntryPoint) {
        return _entryPoint;
    }

    function initializeAccount(bytes calldata initCode) external {
        if (_initialized) {
            revert AlreadyInitialized();
        }
        _initialized = true;

        (
            ModuleInit[] memory validators,
            ModuleInit[] memory executors,
            ModuleInit memory fallbackHandler,
            ModuleInit[] memory hooks
        ) = abi.decode(initCode, (ModuleInit[], ModuleInit[], ModuleInit, ModuleInit[]));

        if (validators.length == 0 || validators[0].module == address(0)) {
            revert InvalidInitialValidator();
        }

        for (uint256 i = 0; i < validators.length; ++i) {
            if (validators[i].module == address(0)) {
                revert InvalidInitialValidator();
            }
            _installModule(MODULE_TYPE_VALIDATOR, validators[i].module, validators[i].data);
        }

        for (uint256 i = 0; i < executors.length; ++i) {
            if (executors[i].module != address(0)) {
                _installModule(MODULE_TYPE_EXECUTOR, executors[i].module, executors[i].data);
            }
        }

        if (fallbackHandler.module != address(0)) {
            _installModule(MODULE_TYPE_FALLBACK, fallbackHandler.module, fallbackHandler.data);
        }

        uint256 installedHooks;
        for (uint256 i = 0; i < hooks.length; ++i) {
            if (hooks[i].module != address(0)) {
                ++installedHooks;
                if (installedHooks > 1) {
                    revert TooManyInitialHooks();
                }
                _installModule(MODULE_TYPE_HOOK, hooks[i].module, hooks[i].data);
            }
        }
    }

    function isValidSignature(bytes32 hash, bytes calldata signature)
        public
        view
        override(AccountERC7579, ERC7739)
        returns (bytes4)
    {
        return ERC7739.isValidSignature(hash, signature);
    }

    function domainSeparator() external view returns (bytes32) {
        return _domainSeparatorV4();
    }

    function accountId() public pure override returns (string memory) {
        return "pali.smart-account.erc7579.2.0.0";
    }

    function _installModule(uint256 moduleTypeId, address module, bytes memory initData) internal override {
        super._installModule(moduleTypeId, module, initData);
        if (moduleTypeId == MODULE_TYPE_VALIDATOR) {
            activeValidator = module;
            emit ActiveValidatorChanged(module);
        }
    }

    /// @notice Atomically re-keys an installed validator module by uninstalling and reinstalling it
    /// with fresh configuration, and makes it the active validator. This is the only way to change
    /// the configuration of the active validator, since uninstalling it directly is rejected to
    /// keep the account from ever being left without an active validator.
    function rotateValidator(address module, bytes calldata deInitData, bytes calldata initData)
        external
        onlyEntryPointOrSelf
    {
        // Intentionally bypasses the active-validator uninstall guard in this contract's
        // _uninstallModule override: the module is reinstalled in the same call.
        AccountERC7579Hooked._uninstallModule(MODULE_TYPE_VALIDATOR, module, deInitData);
        _installModule(MODULE_TYPE_VALIDATOR, module, initData);
    }

    function _uninstallModule(uint256 moduleTypeId, address module, bytes memory deInitData) internal override {
        if (moduleTypeId == MODULE_TYPE_VALIDATOR && activeValidator == module) {
            revert CannotUninstallActiveValidator(module);
        }
        super._uninstallModule(moduleTypeId, module, deInitData);
    }

    function _validateUserOp(PackedUserOperation calldata userOp, bytes32 userOpHash, bytes calldata)
        internal
        override
        returns (uint256)
    {
        address module = _extractUserOpValidator(userOp);
        return module == activeValidator && module != address(0)
            ? IERC7579Validator(module).validateUserOp(userOp, _signableUserOpHash(userOp, userOpHash))
            : VALIDATION_FAILED;
    }

    function _rawSignatureValidation(bytes32 hash, bytes calldata signature)
        internal
        view
        override(AbstractSigner, AccountERC7579)
        returns (bool)
    {
        if (signature.length >= 20) {
            (address module, bytes calldata innerSignature) = _extractSignatureValidator(signature);
            if (
                module == activeValidator && module != address(0)
                    && isModuleInstalled(MODULE_TYPE_VALIDATOR, module, Calldata.emptyBytes())
            ) {
                try IERC7579Validator(module).isValidSignatureWithSender(msg.sender, hash, innerSignature) returns (
                    bytes4 magic
                ) {
                    return magic == IERC1271.isValidSignature.selector;
                } catch {}
            }
        }
        return false;
    }
}
