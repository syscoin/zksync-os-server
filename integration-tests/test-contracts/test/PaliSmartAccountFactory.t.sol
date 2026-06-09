// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {MODULE_TYPE_VALIDATOR} from "@openzeppelin/contracts/interfaces/draft-IERC7579.sol";
import {Test} from "forge-std/Test.sol";
import {PaliECDSAValidatorModule} from "../src/passkey/PaliECDSAValidatorModule.sol";
import {PaliSmartAccount} from "../src/passkey/PaliSmartAccount.sol";
import {PaliSmartAccountFactory} from "../src/passkey/PaliSmartAccountFactory.sol";

contract PaliSmartAccountFactoryTest is Test {
    PaliECDSAValidatorModule private ecdsa;
    PaliSmartAccount private implementation;
    PaliSmartAccountFactory private factory;

    address private owner = address(0xA11CE);

    function setUp() public {
        ecdsa = new PaliECDSAValidatorModule();
        implementation = new PaliSmartAccount();
        factory = new PaliSmartAccountFactory(address(implementation));
    }

    function testCreateAccountDeploysDeterministicProxyAndInstallsInitialValidator() public {
        bytes32 salt = keccak256("pali.account.factory.test");
        bytes memory initData = _ecdsaInitData(owner);
        bytes memory initCode = factory.getInitData(address(ecdsa), initData);
        address predicted = factory.getAddress(salt, initCode);

        address account = factory.createAccount(salt, initCode);

        assertEq(account, predicted);
        assertGt(account.code.length, 0);
        assertEq(PaliSmartAccount(payable(account)).accountId(), "pali.smart-account.erc7579.1.0.0");
        assertTrue(PaliSmartAccount(payable(account)).isModuleInstalled(MODULE_TYPE_VALIDATOR, address(ecdsa), ""));

        address[] memory owners = ecdsa.owners(account);
        assertEq(owners.length, 1);
        assertEq(owners[0], owner);
        assertEq(ecdsa.threshold(account), 1);
    }

    function testCreateAccountIsIdempotentForSameSaltAndInitCode() public {
        bytes32 salt = keccak256("pali.account.factory.idempotent");
        bytes memory initCode = factory.getInitData(address(ecdsa), _ecdsaInitData(owner));

        address first = factory.createAccount(salt, initCode);
        address second = factory.createAccount(salt, initCode);

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
        factory.createAccount(salt, initCode);
    }

    function _ecdsaInitData(address signer) private pure returns (bytes memory) {
        address[] memory owners = new address[](1);
        owners[0] = signer;
        return abi.encode(owners, uint64(1));
    }
}
