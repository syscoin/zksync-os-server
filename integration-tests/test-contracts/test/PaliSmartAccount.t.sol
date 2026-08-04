// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {IEntryPoint, PackedUserOperation} from "@openzeppelin/contracts/interfaces/draft-IERC4337.sol";
import {
    IERC7579Validator,
    MODULE_TYPE_EXECUTOR,
    MODULE_TYPE_HOOK,
    MODULE_TYPE_VALIDATOR,
    VALIDATION_FAILED
} from "@openzeppelin/contracts/interfaces/draft-IERC7579.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {Test} from "forge-std/Test.sol";
import {PaliECDSAValidatorModule} from "contracts/src/pali/PaliECDSAValidatorModule.sol";
import {PaliGuardianRecoveryModule} from "contracts/src/pali/PaliGuardianRecoveryModule.sol";
import {PaliSmartAccount} from "contracts/src/pali/PaliSmartAccount.sol";

contract MockHookModule {
    function isModuleType(uint256 moduleTypeId) external pure returns (bool) {
        return moduleTypeId == MODULE_TYPE_HOOK;
    }

    function onInstall(bytes calldata) external {}

    function onUninstall(bytes calldata) external {}
}

contract FullLengthPersonalValidator is IERC7579Validator {
    bytes4 private constant EIP1271_SUCCESS = 0x1626ba7e;
    bytes4 private constant EIP1271_FAILED = 0xffffffff;

    bytes32 private expectedHash;
    uint256 private expectedLength;

    function setExpected(bytes32 hash, uint256 length) external {
        expectedHash = hash;
        expectedLength = length;
    }

    function onInstall(bytes calldata) external override {}

    function onUninstall(bytes calldata) external override {}

    function isModuleType(uint256 moduleTypeId) external pure override returns (bool) {
        return moduleTypeId == MODULE_TYPE_VALIDATOR;
    }

    function validateUserOp(PackedUserOperation calldata, bytes32) external pure override returns (uint256) {
        return VALIDATION_FAILED;
    }

    function isValidSignatureWithSender(address, bytes32 hash, bytes calldata signature)
        external
        view
        override
        returns (bytes4)
    {
        return hash == expectedHash && signature.length == expectedLength ? EIP1271_SUCCESS : EIP1271_FAILED;
    }
}

contract PaliSmartAccountTest is Test {
    bytes4 internal constant EIP1271_SUCCESS = 0x1626ba7e;
    bytes4 internal constant EIP1271_FAILED = 0xffffffff;
    bytes32 internal constant EIP712_DOMAIN_TYPEHASH =
        keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)");
    bytes32 internal constant PERSONAL_SIGN_TYPEHASH = keccak256("PersonalSign(bytes prefixed)");
    bytes32 internal constant PALI_ERC1271_NAME_HASH = keccak256("pali.smart-account.erc1271");
    bytes32 internal constant PALI_ERC1271_VERSION_HASH = keccak256("1");

    PaliECDSAValidatorModule private ecdsa;
    PaliECDSAValidatorModule private secondEcdsa;
    PaliGuardianRecoveryModule private recovery;
    PaliSmartAccount private implementation;

    uint256 private ownerPrivateKey = 0xA11CE;
    uint256 private secondOwnerPrivateKey = 0xB0B;
    IEntryPoint private entryPoint = IEntryPoint(address(0x4337));
    address private owner;
    address private secondOwner;

    function setUp() public {
        ecdsa = new PaliECDSAValidatorModule();
        secondEcdsa = new PaliECDSAValidatorModule();
        recovery = new PaliGuardianRecoveryModule();
        implementation = new PaliSmartAccount(entryPoint);
        owner = vm.addr(ownerPrivateKey);
        secondOwner = vm.addr(secondOwnerPrivateKey);
    }

    function testInitializeInstallsValidatorAndExecutorModules() public {
        PaliSmartAccount account = _deployProxy(_initCodeWithExecutor());

        assertEq(account.accountId(), "pali.smart-account.erc7579.2.0.0");
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

    function testEip1271PersonalSignUsesAccountStateForInstalledValidator() public {
        PaliSmartAccount account = _deployProxy(_initCodeWithExecutor());
        bytes32 hash = keccak256("pali");
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(ownerPrivateKey, _personalSignHash(account, hash));
        bytes memory signature = abi.encodePacked(address(ecdsa), r, s, bytes1(v));

        vm.prank(address(0xB0B));
        assertEq(account.isValidSignature(hash, signature), EIP1271_SUCCESS);
    }

    function testEip1271SignatureIsBoundToSmartAccount() public {
        PaliSmartAccount firstAccount = _deployProxy(_initCodeWithExecutor());
        PaliSmartAccount secondAccount = _deployProxy(_initCodeWithExecutor());
        bytes32 hash = keccak256("shared application challenge");
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(ownerPrivateKey, _personalSignHash(firstAccount, hash));
        bytes memory signature = abi.encodePacked(address(ecdsa), r, s, bytes1(v));

        assertEq(firstAccount.isValidSignature(hash, signature), EIP1271_SUCCESS);
        assertEq(secondAccount.isValidSignature(hash, signature), EIP1271_FAILED);
    }

    function testLegacyRawEip1271SignatureIsRejected() public {
        PaliSmartAccount account = _deployProxy(_initCodeWithExecutor());
        bytes32 hash = keccak256("legacy application challenge");
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(ownerPrivateKey, hash);
        bytes memory signature = abi.encodePacked(address(ecdsa), r, s, bytes1(v));

        assertEq(account.isValidSignature(hash, signature), EIP1271_FAILED);
    }

    function testEip1271TypedDataIsBoundToSmartAccountAndApplication() public {
        PaliSmartAccount firstAccount = _deployProxy(_initCodeWithExecutor());
        PaliSmartAccount secondAccount = _deployProxy(_initCodeWithExecutor());
        string memory contentsDescription = "MockMessage(string message,uint256 value)";
        bytes32 contentsHash =
            keccak256(abi.encode(keccak256(bytes(contentsDescription)), keccak256(bytes("Hello, Pali!")), uint256(42)));
        bytes32 appSeparator = keccak256(
            abi.encode(
                EIP712_DOMAIN_TYPEHASH, keccak256("Pali Test App"), keccak256("1"), block.chainid, address(0xCA11)
            )
        );
        bytes32 originalHash = keccak256(abi.encodePacked(hex"1901", appSeparator, contentsHash));
        bytes32 nestedTypeHash = keccak256(
            abi.encodePacked(
                "TypedDataSign(MockMessage contents,string name,string version,uint256 chainId,address verifyingContract,bytes32 salt)",
                contentsDescription
            )
        );
        bytes32 nestedStructHash = keccak256(
            abi.encode(
                nestedTypeHash,
                contentsHash,
                PALI_ERC1271_NAME_HASH,
                PALI_ERC1271_VERSION_HASH,
                block.chainid,
                address(firstAccount),
                bytes32(0)
            )
        );
        bytes32 nestedHash = keccak256(abi.encodePacked(hex"1901", appSeparator, nestedStructHash));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(ownerPrivateKey, nestedHash);
        bytes memory signature = abi.encodePacked(
            address(ecdsa),
            r,
            s,
            bytes1(v),
            appSeparator,
            contentsHash,
            contentsDescription,
            uint16(bytes(contentsDescription).length)
        );

        assertEq(firstAccount.isValidSignature(originalHash, signature), EIP1271_SUCCESS);
        assertEq(secondAccount.isValidSignature(originalHash, signature), EIP1271_FAILED);
    }

    function testPersonalSignatureFallsBackAfterTypedLookingSuffix() public {
        FullLengthPersonalValidator validator = new FullLengthPersonalValidator();
        PaliSmartAccount.ModuleInit[] memory validators = new PaliSmartAccount.ModuleInit[](1);
        validators[0] = PaliSmartAccount.ModuleInit({module: address(validator), data: ""});
        PaliSmartAccount.ModuleInit[] memory executors = new PaliSmartAccount.ModuleInit[](0);
        PaliSmartAccount.ModuleInit memory fallbackHandler;
        PaliSmartAccount.ModuleInit[] memory hooks = new PaliSmartAccount.ModuleInit[](0);
        PaliSmartAccount account = _deployProxy(abi.encode(validators, executors, fallbackHandler, hooks));

        bytes32 appSeparator = keccak256("adversarial app separator");
        bytes32 contentsHash = keccak256("adversarial contents");
        bytes memory contentsDescription = bytes("X()");
        bytes32 hash = keccak256(abi.encodePacked(hex"1901", appSeparator, contentsHash));
        validator.setExpected(_personalSignHash(account, hash), 3856);

        uint256 suffixLength = 32 + 32 + contentsDescription.length + 2;
        bytes memory filler = new bytes(3856 - suffixLength);
        bytes memory innerSignature = abi.encodePacked(
            filler, appSeparator, contentsHash, contentsDescription, uint16(contentsDescription.length)
        );

        assertEq(innerSignature.length, 3856);
        bytes memory accountSignature = abi.encodePacked(address(validator), innerSignature);

        // The typed-data attempt receives a shortened inner signature and fails;
        // the personal-sign fallback then validates the original full calldata.
        assertEq(account.isValidSignature(hash, accountSignature), EIP1271_SUCCESS);
    }

    function testInstallingValidatorSwitchesActiveValidator() public {
        PaliSmartAccount account = _deployProxy(_initCodeWithExecutor());
        bytes32 hash = keccak256("pali");

        vm.prank(address(account));
        account.installModule(MODULE_TYPE_VALIDATOR, address(secondEcdsa), _ecdsaInitData(secondOwner));

        assertTrue(account.isModuleInstalled(MODULE_TYPE_VALIDATOR, address(ecdsa), ""));
        assertTrue(account.isModuleInstalled(MODULE_TYPE_VALIDATOR, address(secondEcdsa), ""));
        assertEq(account.activeValidator(), address(secondEcdsa));

        bytes32 signedHash = _personalSignHash(account, hash);
        (uint8 oldV, bytes32 oldR, bytes32 oldS) = vm.sign(ownerPrivateKey, signedHash);
        bytes memory oldSignature = abi.encodePacked(address(ecdsa), oldR, oldS, bytes1(oldV));
        assertEq(account.isValidSignature(hash, oldSignature), EIP1271_FAILED);

        (uint8 newV, bytes32 newR, bytes32 newS) = vm.sign(secondOwnerPrivateKey, signedHash);
        bytes memory newSignature = abi.encodePacked(address(secondEcdsa), newR, newS, bytes1(newV));
        assertEq(account.isValidSignature(hash, newSignature), EIP1271_SUCCESS);
    }

    function testMultipleInitialValidatorsInstallAndLastValidatorBecomesActive() public {
        PaliSmartAccount.ModuleInit[] memory validators = new PaliSmartAccount.ModuleInit[](2);
        validators[0] = PaliSmartAccount.ModuleInit({module: address(ecdsa), data: _ecdsaInitData(owner)});
        validators[1] = PaliSmartAccount.ModuleInit({module: address(secondEcdsa), data: _ecdsaInitData(secondOwner)});
        PaliSmartAccount.ModuleInit[] memory executors = new PaliSmartAccount.ModuleInit[](0);
        PaliSmartAccount.ModuleInit memory fallbackHandler;
        PaliSmartAccount.ModuleInit[] memory hooks = new PaliSmartAccount.ModuleInit[](0);
        PaliSmartAccount account = _deployProxy(abi.encode(validators, executors, fallbackHandler, hooks));
        bytes32 hash = keccak256("pali");

        assertTrue(account.isModuleInstalled(MODULE_TYPE_VALIDATOR, address(ecdsa), ""));
        assertTrue(account.isModuleInstalled(MODULE_TYPE_VALIDATOR, address(secondEcdsa), ""));
        assertEq(account.activeValidator(), address(secondEcdsa));

        bytes32 signedHash = _personalSignHash(account, hash);
        (uint8 oldV, bytes32 oldR, bytes32 oldS) = vm.sign(ownerPrivateKey, signedHash);
        bytes memory oldSignature = abi.encodePacked(address(ecdsa), oldR, oldS, bytes1(oldV));
        assertEq(account.isValidSignature(hash, oldSignature), EIP1271_FAILED);

        (uint8 newV, bytes32 newR, bytes32 newS) = vm.sign(secondOwnerPrivateKey, signedHash);
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

        bytes32 signedHash = _personalSignHash(account, hash);
        (uint8 oldV, bytes32 oldR, bytes32 oldS) = vm.sign(ownerPrivateKey, signedHash);
        bytes memory oldSignature = abi.encodePacked(address(ecdsa), oldR, oldS, bytes1(oldV));
        assertEq(account.isValidSignature(hash, oldSignature), EIP1271_FAILED);

        (uint8 newV, bytes32 newR, bytes32 newS) = vm.sign(secondOwnerPrivateKey, signedHash);
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

    function _personalSignHash(PaliSmartAccount account, bytes32 hash) private view returns (bytes32) {
        return keccak256(
            abi.encodePacked(hex"1901", account.domainSeparator(), keccak256(abi.encode(PERSONAL_SIGN_TYPEHASH, hash)))
        );
    }
}
