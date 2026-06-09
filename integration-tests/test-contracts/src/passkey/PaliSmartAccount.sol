// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {
    MODULE_TYPE_EXECUTOR,
    MODULE_TYPE_FALLBACK,
    MODULE_TYPE_HOOK,
    MODULE_TYPE_VALIDATOR
} from "@openzeppelin/contracts/interfaces/draft-IERC7579.sol";
import {AccountERC7579Hooked} from
    "@openzeppelin/contracts/account/extensions/draft-AccountERC7579Hooked.sol";

contract PaliSmartAccount is AccountERC7579Hooked {
    struct ModuleInit {
        address module;
        bytes data;
    }

    error AlreadyInitialized();
    error InvalidInitialValidator();
    error TooManyInitialHooks();

    bool private _initialized;

    constructor() {
        _initialized = true;
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

    function accountId() public pure override returns (string memory) {
        return "pali.smart-account.erc7579.1.0.0";
    }

    function _rawSignatureValidation(bytes32, bytes calldata) internal pure override returns (bool) {
        return false;
    }
}
