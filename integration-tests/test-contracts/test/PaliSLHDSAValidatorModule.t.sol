// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {
    PackedUserOperation,
    MODULE_TYPE_VALIDATOR,
    VALIDATION_FAILED,
    VALIDATION_SUCCESS
} from "@openzeppelin/contracts/interfaces/draft-IERC7579.sol";
import {Test} from "forge-std/Test.sol";
import {
    ISLHDSAVerifier,
    PaliSLHDSAValidatorModule
} from "../src/passkey/PaliSLHDSAValidatorModule.sol";
import {SLHDSASHA212824Verifier} from "../src/passkey/SLHDSASHA212824Verifier.sol";

contract MockSLHDSAVerifier is ISLHDSAVerifier {
    bool public shouldRevert;
    bool public valid;

    function setValid(bool valid_) external {
        valid = valid_;
    }

    function setShouldRevert(bool shouldRevert_) external {
        shouldRevert = shouldRevert_;
    }

    function verify(bytes32 pkSeed, bytes32 pkRoot, bytes32 message, bytes calldata sig)
        external
        view
        override
        returns (bool)
    {
        pkSeed;
        pkRoot;
        message;
        sig;
        if (shouldRevert) {
            revert("mock verifier revert");
        }
        return valid;
    }
}

contract PaliSLHDSAValidatorModuleTest is Test {
    bytes4 internal constant EIP1271_SUCCESS = 0x1626ba7e;
    bytes4 internal constant EIP1271_FAILED = 0xffffffff;
    bytes32 internal constant PK_SEED = 0x1111111111111111111111111111111100000000000000000000000000000000;
    bytes32 internal constant PK_ROOT = 0x2222222222222222222222222222222200000000000000000000000000000000;
    bytes32 internal constant HASH = 0x3333333333333333333333333333333333333333333333333333333333333333;

    MockSLHDSAVerifier private verifier;
    PaliSLHDSAValidatorModule private module;

    function setUp() public {
        verifier = new MockSLHDSAVerifier();
        module = new PaliSLHDSAValidatorModule(verifier);
        module.onInstall(abi.encode(PaliSLHDSAValidatorModule.AuthData({pkSeed: PK_SEED, pkRoot: PK_ROOT})));
    }

    function testInstallStoresAuthDataAndReportsModuleType() public view {
        PaliSLHDSAValidatorModule.AuthData memory authData = module.authData(address(this));

        assertTrue(module.isModuleType(MODULE_TYPE_VALIDATOR));
        assertTrue(module.isInitialized(address(this)));
        assertEq(authData.pkSeed, PK_SEED);
        assertEq(authData.pkRoot, PK_ROOT);
    }

    function testRejectsInvalidAuthData() public {
        PaliSLHDSAValidatorModule fresh = new PaliSLHDSAValidatorModule(verifier);

        vm.expectRevert(PaliSLHDSAValidatorModule.InvalidSLHDSAAuthConfig.selector);
        fresh.onInstall(abi.encode(PaliSLHDSAValidatorModule.AuthData({pkSeed: bytes32(0), pkRoot: PK_ROOT})));

        vm.expectRevert(PaliSLHDSAValidatorModule.InvalidSLHDSAAuthConfig.selector);
        fresh.onInstall(abi.encode(PaliSLHDSAValidatorModule.AuthData({pkSeed: PK_SEED, pkRoot: bytes32(uint256(1))})));
    }

    function testEip1271ValidationUsesVerifier() public {
        bytes memory signature = _signature();
        verifier.setValid(true);

        assertEq(module.isValidSignatureWithSender(address(0xB0B), HASH, signature), EIP1271_SUCCESS);
    }

    function testInvalidSignatureFailsClosed() public {
        verifier.setValid(false);

        assertEq(module.isValidSignatureWithSender(address(0xB0B), HASH, _signature()), EIP1271_FAILED);
    }

    function testMalformedSignatureFailsClosedWithoutCallingVerifier() public view {
        assertEq(module.isValidSignatureWithSender(address(0xB0B), HASH, hex"1234"), EIP1271_FAILED);
    }

    function testRevertingVerifierFailsClosed() public {
        verifier.setShouldRevert(true);

        assertEq(module.isValidSignatureWithSender(address(0xB0B), HASH, _signature()), EIP1271_FAILED);
    }

    function testValidateUserOpReturnsValidationData() public {
        PackedUserOperation memory userOp;
        userOp.sender = address(this);
        userOp.signature = _signature();

        verifier.setValid(true);
        assertEq(module.validateUserOp(userOp, HASH), VALIDATION_SUCCESS);

        verifier.setValid(false);
        assertEq(module.validateUserOp(userOp, HASH), VALIDATION_FAILED);
    }

    function testRealVerifierRejectsZeroSignature() public {
        SLHDSASHA212824Verifier realVerifier = new SLHDSASHA212824Verifier();
        PaliSLHDSAValidatorModule realModule = new PaliSLHDSAValidatorModule(ISLHDSAVerifier(address(realVerifier)));
        realModule.onInstall(abi.encode(PaliSLHDSAValidatorModule.AuthData({pkSeed: PK_SEED, pkRoot: PK_ROOT})));

        assertEq(realModule.isValidSignatureWithSender(address(0xB0B), HASH, new bytes(3856)), EIP1271_FAILED);
    }

    function testRealVerifierRejectsMalformedSignatureLength() public {
        SLHDSASHA212824Verifier realVerifier = new SLHDSASHA212824Verifier();

        vm.expectRevert(bytes("Invalid sig length"));
        realVerifier.verify(PK_SEED, PK_ROOT, HASH, hex"1234");
    }

    function testRealVerifierRejectsNonCanonicalPublicKey() public {
        SLHDSASHA212824Verifier realVerifier = new SLHDSASHA212824Verifier();

        vm.expectRevert(bytes("Invalid public key"));
        realVerifier.verify(bytes32(uint256(1)), PK_ROOT, HASH, new bytes(3856));
    }

    function testUninstallClearsAuthData() public {
        module.onUninstall("");

        assertFalse(module.isInitialized(address(this)));
        assertEq(module.isValidSignatureWithSender(address(0xB0B), HASH, _signature()), EIP1271_FAILED);
    }

    function _signature() private pure returns (bytes memory signature) {
        signature = new bytes(3856);
        signature[0] = 0x01;
    }
}
