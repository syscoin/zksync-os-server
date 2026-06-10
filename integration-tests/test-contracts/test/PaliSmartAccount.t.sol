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
    bytes4 internal constant EIP1271_SUCCESS = 0x1626ba7e;
    bytes4 internal constant EIP1271_FAILED = 0xffffffff;

    PaliECDSAValidatorModule private ecdsa;
    PaliECDSAValidatorModule private secondEcdsa;
    PaliGuardianRecoveryModule private recovery;
    PaliSmartAccount private implementation;

    uint256 private ownerPrivateKey = 0xA11CE;
    uint256 private secondOwnerPrivateKey = 0xB0B;
    address private owner;
    address private secondOwner;

    function setUp() public {
        ecdsa = new PaliECDSAValidatorModule();
        secondEcdsa = new PaliECDSAValidatorModule();
        recovery = new PaliGuardianRecoveryModule();
        implementation = new PaliSmartAccount();
        owner = vm.addr(ownerPrivateKey);
        secondOwner = vm.addr(secondOwnerPrivateKey);
    }

    function testInitializeInstallsValidatorAndExecutorModules() public {
        PaliSmartAccount account = _deployProxy(_initCodeWithExecutor());

        assertEq(account.accountId(), "pali.smart-account.erc7579.1.0.0");
        assertTrue(account.isModuleInstalled(MODULE_TYPE_VALIDATOR, address(ecdsa), ""));
        assertTrue(account.isModuleInstalled(MODULE_TYPE_EXECUTOR, address(recovery), ""));
        assertEq(account.activeValidator(), address(ecdsa));

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

    function testEip1271ValidationUsesAccountStateForInstalledValidator() public {
        PaliSmartAccount account = _deployProxy(_initCodeWithExecutor());
        bytes32 hash = keccak256("pali");
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(ownerPrivateKey, hash);
        bytes memory signature = abi.encodePacked(address(ecdsa), r, s, bytes1(v));

        vm.prank(address(0xB0B));
        assertEq(account.isValidSignature(hash, signature), EIP1271_SUCCESS);
    }

    function testInstallingValidatorSwitchesActiveValidator() public {
        PaliSmartAccount account = _deployProxy(_initCodeWithExecutor());
        bytes32 hash = keccak256("pali");

        vm.prank(address(account));
        account.installModule(MODULE_TYPE_VALIDATOR, address(secondEcdsa), _ecdsaInitData(secondOwner));

        assertTrue(account.isModuleInstalled(MODULE_TYPE_VALIDATOR, address(ecdsa), ""));
        assertTrue(account.isModuleInstalled(MODULE_TYPE_VALIDATOR, address(secondEcdsa), ""));
        assertEq(account.activeValidator(), address(secondEcdsa));

        (uint8 oldV, bytes32 oldR, bytes32 oldS) = vm.sign(ownerPrivateKey, hash);
        bytes memory oldSignature = abi.encodePacked(address(ecdsa), oldR, oldS, bytes1(oldV));
        assertEq(account.isValidSignature(hash, oldSignature), EIP1271_FAILED);

        (uint8 newV, bytes32 newR, bytes32 newS) = vm.sign(secondOwnerPrivateKey, hash);
        bytes memory newSignature = abi.encodePacked(address(secondEcdsa), newR, newS, bytes1(newV));
        assertEq(account.isValidSignature(hash, newSignature), EIP1271_SUCCESS);
    }

    function testCannotUninstallActiveValidator() public {
        PaliSmartAccount account = _deployProxy(_initCodeWithExecutor());

        vm.prank(address(account));
        vm.expectRevert(
            abi.encodeWithSelector(PaliSmartAccount.CannotUninstallActiveValidator.selector, address(ecdsa))
        );
        account.uninstallModule(MODULE_TYPE_VALIDATOR, address(ecdsa), "");

        assertEq(account.activeValidator(), address(ecdsa));
        assertTrue(account.isModuleInstalled(MODULE_TYPE_VALIDATOR, address(ecdsa), ""));
    }

    function testCanUninstallPreviousValidatorAfterReplacement() public {
        PaliSmartAccount account = _deployProxy(_initCodeWithExecutor());

        vm.prank(address(account));
        account.installModule(MODULE_TYPE_VALIDATOR, address(secondEcdsa), _ecdsaInitData(secondOwner));
        assertEq(account.activeValidator(), address(secondEcdsa));

        vm.prank(address(account));
        account.uninstallModule(MODULE_TYPE_VALIDATOR, address(ecdsa), "");

        assertFalse(account.isModuleInstalled(MODULE_TYPE_VALIDATOR, address(ecdsa), ""));
        assertEq(account.activeValidator(), address(secondEcdsa));
    }

    function testRotateValidatorRekeysActiveValidator() public {
        PaliSmartAccount account = _deployProxy(_initCodeWithExecutor());
        bytes32 hash = keccak256("pali");

        vm.prank(address(account));
        account.rotateValidator(address(ecdsa), "", _ecdsaInitData(secondOwner));

        assertEq(account.activeValidator(), address(ecdsa));
        assertTrue(account.isModuleInstalled(MODULE_TYPE_VALIDATOR, address(ecdsa), ""));

        address[] memory owners = ecdsa.owners(address(account));
        assertEq(owners.length, 1);
        assertEq(owners[0], secondOwner);

        (uint8 oldV, bytes32 oldR, bytes32 oldS) = vm.sign(ownerPrivateKey, hash);
        bytes memory oldSignature = abi.encodePacked(address(ecdsa), oldR, oldS, bytes1(oldV));
        assertEq(account.isValidSignature(hash, oldSignature), EIP1271_FAILED);

        (uint8 newV, bytes32 newR, bytes32 newS) = vm.sign(secondOwnerPrivateKey, hash);
        bytes memory newSignature = abi.encodePacked(address(ecdsa), newR, newS, bytes1(newV));
        assertEq(account.isValidSignature(hash, newSignature), EIP1271_SUCCESS);
    }

    function testRotateValidatorRejectsModuleThatIsNotInstalled() public {
        PaliSmartAccount account = _deployProxy(_initCodeWithExecutor());

        vm.prank(address(account));
        vm.expectRevert(
            abi.encodeWithSignature(
                "ERC7579UninstalledModule(uint256,address)", MODULE_TYPE_VALIDATOR, address(secondEcdsa)
            )
        );
        account.rotateValidator(address(secondEcdsa), "", _ecdsaInitData(secondOwner));
    }

    function testRotateValidatorRejectsUnauthorizedCaller() public {
        PaliSmartAccount account = _deployProxy(_initCodeWithExecutor());

        vm.prank(owner);
        vm.expectRevert(abi.encodeWithSignature("AccountUnauthorized(address)", owner));
        account.rotateValidator(address(ecdsa), "", _ecdsaInitData(secondOwner));
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
