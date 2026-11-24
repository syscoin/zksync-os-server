// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {IEntryPoint} from "@openzeppelin/contracts/interfaces/draft-IERC4337.sol";
import {MODULE_TYPE_VALIDATOR} from "@openzeppelin/contracts/interfaces/draft-IERC7579.sol";
import {Test} from "forge-std/Test.sol";
import {PaliCompositeValidatorModule} from "contracts/src/pali/PaliCompositeValidatorModule.sol";
import {PaliECDSAValidatorModule} from "contracts/src/pali/PaliECDSAValidatorModule.sol";
import {PaliP256WebAuthnValidatorModule} from "contracts/src/pali/PaliP256WebAuthnValidatorModule.sol";
import {ISLHDSAVerifier, PaliSLHDSAValidatorModule} from "contracts/src/pali/PaliSLHDSAValidatorModule.sol";
import {PaliSmartAccount} from "contracts/src/pali/PaliSmartAccount.sol";
import {PaliSmartAccountFactory} from "contracts/src/pali/PaliSmartAccountFactory.sol";
import {SLHDSASHA212824Verifier} from "contracts/src/pali/SLHDSASHA212824Verifier.sol";
import {MockP256WebAuthnPrecompile} from "./PaliP256WebAuthnValidatorModule.t.sol";

contract MockEntryPoint {
    address public immutable senderCreator;

    constructor(address senderCreator_) {
        senderCreator = senderCreator_;
    }
}

contract PaliSmartAccountFactoryTest is Test {
    PaliECDSAValidatorModule private ecdsa;
    PaliP256WebAuthnValidatorModule private p256;
    PaliSLHDSAValidatorModule private slhDsa;
    PaliSmartAccount private implementation;
    PaliSmartAccountFactory private factory;
    MockEntryPoint private entryPoint;

    address private owner = address(0xA11CE);
    address private senderCreator = address(0x4337);

    function setUp() public {
        ecdsa = new PaliECDSAValidatorModule();
        p256 = new PaliP256WebAuthnValidatorModule();
        SLHDSASHA212824Verifier verifier = new SLHDSASHA212824Verifier();
        slhDsa = new PaliSLHDSAValidatorModule(ISLHDSAVerifier(address(verifier)), address(verifier).codehash);
        entryPoint = new MockEntryPoint(senderCreator);
        implementation = new PaliSmartAccount(IEntryPoint(address(entryPoint)));
        factory = new PaliSmartAccountFactory(
            address(implementation), address(entryPoint), address(ecdsa), address(p256), address(slhDsa)
        );
    }

    function testCreateAccountDeploysDeterministicProxyAndInstallsInitialValidator() public {
        bytes32 salt = keccak256("pali.account.factory.test");
        bytes memory initData = _ecdsaInitData(owner);
        bytes memory initCode = factory.getInitData(address(ecdsa), initData);
        address predicted = factory.getAddress(salt, initCode);

        vm.prank(senderCreator);
        address account = factory.createAccount(salt, initCode);

        assertEq(account, predicted);
        assertGt(account.code.length, 0);
        assertEq(PaliSmartAccount(payable(account)).accountId(), "pali.smart-account.erc7579.2.0.0");
        assertTrue(PaliSmartAccount(payable(account)).isModuleInstalled(MODULE_TYPE_VALIDATOR, address(ecdsa), ""));

        address[] memory owners = ecdsa.owners(account);
        assertEq(owners.length, 1);
        assertEq(owners[0], owner);
        assertEq(ecdsa.threshold(account), 1);
    }

    function testFactoryAcceptsP256AndSLHDSABootstrap() public {
        vm.etch(address(0x100), address(new MockP256WebAuthnPrecompile()).code);
        bytes memory p256Data = abi.encode(
            PaliP256WebAuthnValidatorModule.AuthData({
                publicKeyX: bytes32(uint256(1)),
                publicKeyY: bytes32(uint256(2)),
                rpIdHash: keccak256("pali.test"),
                originHash: keccak256("https://pali.test"),
                originLength: bytes("https://pali.test").length
            })
        );
        bytes memory slhData = abi.encode(bytes32(bytes16(uint128(1))), bytes32(bytes16(uint128(2))));
        address p256Account = _deployWithValidator(address(p256), p256Data);
        address slhAccount = _deployWithValidator(address(slhDsa), slhData);
        assertTrue(p256.isInitialized(p256Account));
        assertTrue(slhDsa.isInitialized(slhAccount));
    }

    function testFactoryRejectsUnsupportedValidatorsDuringBootstrap() public {
        address[3] memory unsupported =
            [address(new PaliCompositeValidatorModule()), address(new PaliECDSAValidatorModule()), address(0)];
        PaliSmartAccountFactory.ModuleInit[] memory validators = new PaliSmartAccountFactory.ModuleInit[](2);
        validators[0] = PaliSmartAccountFactory.ModuleInit(address(ecdsa), _ecdsaInitData(owner));
        PaliSmartAccountFactory.ModuleInit[] memory executors = new PaliSmartAccountFactory.ModuleInit[](0);
        PaliSmartAccountFactory.ModuleInit memory fallbackHandler;
        PaliSmartAccountFactory.ModuleInit[] memory hooks = new PaliSmartAccountFactory.ModuleInit[](0);
        bytes32 salt = keccak256("pali.account.factory.composite");

        for (uint256 i = 0; i < unsupported.length; ++i) {
            validators[1] = PaliSmartAccountFactory.ModuleInit(unsupported[i], "");
            bytes memory initCode = factory.getInitData(validators, executors, fallbackHandler, hooks);
            bytes memory expected =
                abi.encodeWithSelector(PaliSmartAccountFactory.UnsupportedBootstrapValidator.selector, unsupported[i]);

            vm.expectRevert(expected);
            factory.getAddress(salt, initCode);
            vm.prank(senderCreator);
            vm.expectRevert(expected);
            factory.createAccount(salt, initCode);
            vm.prank(senderCreator);
            vm.expectRevert(expected);
            factory.createAccountWithModules(salt, validators, executors, fallbackHandler, hooks);
        }
    }

    function testConstructorRejectsNonContractImplementation() public {
        MockEntryPoint otherEntryPoint = new MockEntryPoint(senderCreator);

        vm.expectRevert(
            abi.encodeWithSelector(PaliSmartAccountFactory.InvalidImplementation.selector, address(0xBADC0DE))
        );
        new PaliSmartAccountFactory(
            address(0xBADC0DE), address(otherEntryPoint), address(ecdsa), address(p256), address(slhDsa)
        );
    }

    function testConstructorRejectsMismatchedAccountEntryPoint() public {
        MockEntryPoint otherEntryPoint = new MockEntryPoint(senderCreator);

        vm.expectRevert(
            abi.encodeWithSelector(
                PaliSmartAccountFactory.InvalidAccountEntryPoint.selector, address(entryPoint), address(otherEntryPoint)
            )
        );
        new PaliSmartAccountFactory(
            address(implementation), address(otherEntryPoint), address(ecdsa), address(p256), address(slhDsa)
        );
    }

    function testCreateAccountOnlyAllowsEntryPointSenderCreator() public {
        bytes32 salt = keccak256("pali.account.factory.sender-creator");
        bytes memory initCode = factory.getInitData(address(ecdsa), _ecdsaInitData(owner));

        vm.expectRevert(abi.encodeWithSelector(PaliSmartAccountFactory.OnlySenderCreator.selector, address(this)));
        factory.createAccount(salt, initCode);
    }

    function testCreateAccountIsIdempotentForSameSaltAndInitCode() public {
        bytes32 salt = keccak256("pali.account.factory.idempotent");
        bytes memory initCode = factory.getInitData(address(ecdsa), _ecdsaInitData(owner));

        vm.startPrank(senderCreator);
        address first = factory.createAccount(salt, initCode);
        address second = factory.createAccount(salt, initCode);
        vm.stopPrank();

        assertEq(second, first);
        assertEq(second, factory.getAddress(salt, initCode));
    }

    function testCreate2AddressBindsInitCode() public view {
        bytes32 salt = keccak256("pali.account.factory.init-code-binding");
        bytes memory firstInitCode = factory.getInitData(address(ecdsa), _ecdsaInitData(address(0xA11CE)));
        bytes memory secondInitCode = factory.getInitData(address(ecdsa), _ecdsaInitData(address(0xB0B)));

        assertNotEq(factory.getAddress(salt, firstInitCode), factory.getAddress(salt, secondInitCode));
    }

    function testImplementationCannotBeInitializedDirectly() public {
        bytes memory initCode = factory.getInitData(address(ecdsa), _ecdsaInitData(owner));

        vm.expectRevert(PaliSmartAccount.AlreadyInitialized.selector);
        implementation.initializeAccount(initCode);
    }

    function testCreateAccountRequiresInitialValidator() public {
        bytes32 salt = keccak256("pali.account.factory.invalid-init");
        PaliSmartAccountFactory.ModuleInit[] memory validators = new PaliSmartAccountFactory.ModuleInit[](0);
        PaliSmartAccountFactory.ModuleInit[] memory executors = new PaliSmartAccountFactory.ModuleInit[](0);
        PaliSmartAccountFactory.ModuleInit memory fallbackHandler;
        PaliSmartAccountFactory.ModuleInit[] memory hooks = new PaliSmartAccountFactory.ModuleInit[](0);
        bytes memory initCode = factory.getInitData(validators, executors, fallbackHandler, hooks);

        vm.expectRevert(PaliSmartAccount.InvalidInitialValidator.selector);
        vm.prank(senderCreator);
        factory.createAccount(salt, initCode);
    }

    function _ecdsaInitData(address signer) private pure returns (bytes memory) {
        address[] memory owners = new address[](1);
        owners[0] = signer;
        return abi.encode(owners, uint64(1));
    }

    function _deployWithValidator(address validator, bytes memory data) private returns (address account) {
        bytes32 salt = keccak256("pali.account.factory.approved-validator");
        bytes memory initCode = factory.getInitData(validator, data);
        address predicted = factory.getAddress(salt, initCode);
        vm.prank(senderCreator);
        account = factory.createAccount(salt, initCode);
        assertEq(account, predicted);
        assertTrue(PaliSmartAccount(payable(account)).isModuleInstalled(MODULE_TYPE_VALIDATOR, validator, ""));
    }
}
