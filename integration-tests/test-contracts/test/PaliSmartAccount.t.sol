// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {
    MODULE_TYPE_EXECUTOR,
    MODULE_TYPE_HOOK,
    MODULE_TYPE_VALIDATOR
} from "@openzeppelin/contracts/interfaces/draft-IERC7579.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {Test} from "forge-std/Test.sol";
import {PaliECDSAValidatorModule} from "../src/passkey/PaliECDSAValidatorModule.sol";
import {PaliGuardianRecoveryModule} from "../src/passkey/PaliGuardianRecoveryModule.sol";
import {PaliSmartAccount} from "../src/passkey/PaliSmartAccount.sol";

contract MockHookModule {
    function isModuleType(uint256 moduleTypeId) external pure returns (bool) {
        return moduleTypeId == MODULE_TYPE_HOOK;
    }

    function onInstall(bytes calldata) external {}

    function onUninstall(bytes calldata) external {}
}

contract PaliSmartAccountTest is Test {
    bytes4 internal constant EIP1271_FAILED = 0xffffffff;

    PaliECDSAValidatorModule private ecdsa;
    PaliGuardianRecoveryModule private recovery;
    PaliSmartAccount private implementation;

    address private owner = address(0xA11CE);

    function setUp() public {
        ecdsa = new PaliECDSAValidatorModule();
        recovery = new PaliGuardianRecoveryModule();
        implementation = new PaliSmartAccount();
    }

    function testInitializeInstallsValidatorAndExecutorModules() public {
        PaliSmartAccount account = _deployProxy(_initCodeWithExecutor());

        assertEq(account.accountId(), "pali.smart-account.erc7579.1.0.0");
        assertTrue(account.isModuleInstalled(MODULE_TYPE_VALIDATOR, address(ecdsa), ""));
        assertTrue(account.isModuleInstalled(MODULE_TYPE_EXECUTOR, address(recovery), ""));

        address[] memory owners = ecdsa.owners(address(account));
        assertEq(owners.length, 1);
        assertEq(owners[0], owner);

        PaliGuardianRecoveryModule.RecoveryConfig memory config = recovery.config(address(account));
        assertTrue(config.installed);
        assertEq(config.threshold, 1);
    }

    function testInitializeRejectsMoreThanOneHook() public {
        MockHookModule firstHook = new MockHookModule();
        MockHookModule secondHook = new MockHookModule();

        PaliSmartAccount.ModuleInit[] memory validators = new PaliSmartAccount.ModuleInit[](1);
        validators[0] = PaliSmartAccount.ModuleInit({module: address(ecdsa), data: _ecdsaInitData(owner)});
        PaliSmartAccount.ModuleInit[] memory executors = new PaliSmartAccount.ModuleInit[](0);
        PaliSmartAccount.ModuleInit memory fallbackHandler;
        PaliSmartAccount.ModuleInit[] memory hooks = new PaliSmartAccount.ModuleInit[](2);
        hooks[0] = PaliSmartAccount.ModuleInit({module: address(firstHook), data: ""});
        hooks[1] = PaliSmartAccount.ModuleInit({module: address(secondHook), data: ""});

        bytes memory initCode = abi.encode(validators, executors, fallbackHandler, hooks);

        vm.expectRevert(PaliSmartAccount.TooManyInitialHooks.selector);
        _deployProxy(initCode);
    }

    function testProxyCannotBeInitializedTwice() public {
        PaliSmartAccount account = _deployProxy(_initCodeWithExecutor());

        vm.expectRevert(PaliSmartAccount.AlreadyInitialized.selector);
        account.initializeAccount(_initCodeWithExecutor());
    }

    function testRawEip1271SignatureValidationFailsWithoutValidatorModule() public {
        PaliSmartAccount account = _deployProxy(_initCodeWithExecutor());

        assertEq(account.isValidSignature(keccak256("pali"), hex"1234"), EIP1271_FAILED);
    }

    function _deployProxy(bytes memory initCode) private returns (PaliSmartAccount) {
        ERC1967Proxy proxy =
            new ERC1967Proxy(address(implementation), abi.encodeCall(PaliSmartAccount.initializeAccount, (initCode)));
        return PaliSmartAccount(payable(address(proxy)));
    }

    function _initCodeWithExecutor() private view returns (bytes memory) {
        PaliSmartAccount.ModuleInit[] memory validators = new PaliSmartAccount.ModuleInit[](1);
        validators[0] = PaliSmartAccount.ModuleInit({module: address(ecdsa), data: _ecdsaInitData(owner)});
        PaliSmartAccount.ModuleInit[] memory executors = new PaliSmartAccount.ModuleInit[](1);
        executors[0] = PaliSmartAccount.ModuleInit({module: address(recovery), data: _guardianInitData(owner)});
        PaliSmartAccount.ModuleInit memory fallbackHandler;
        PaliSmartAccount.ModuleInit[] memory hooks = new PaliSmartAccount.ModuleInit[](0);

        return abi.encode(validators, executors, fallbackHandler, hooks);
    }

    function _ecdsaInitData(address signer) private pure returns (bytes memory) {
        address[] memory owners = new address[](1);
        owners[0] = signer;
        return abi.encode(owners, uint64(1));
    }

    function _guardianInitData(address guardian) private pure returns (bytes memory) {
        address[] memory guardians = new address[](1);
        guardians[0] = guardian;
        return abi.encode(uint32(1 days), uint32(7 days), guardians, uint64(1));
    }
}
