// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {PackedUserOperation} from "@openzeppelin/contracts/interfaces/draft-IERC4337.sol";
import {
    MODULE_TYPE_VALIDATOR,
    VALIDATION_FAILED,
    VALIDATION_SUCCESS
} from "@openzeppelin/contracts/interfaces/draft-IERC7579.sol";
import {Test} from "forge-std/Test.sol";
import {PaliCompositeValidatorModule} from "contracts/src/pali/PaliCompositeValidatorModule.sol";
import {PaliECDSAValidatorModule} from "contracts/src/pali/PaliECDSAValidatorModule.sol";
import {PaliP256WebAuthnValidatorModule} from "contracts/src/pali/PaliP256WebAuthnValidatorModule.sol";

contract MockP256Precompile {
    fallback() external {
        assembly {
            mstore(0x00, 1)
            return(0x00, 0x20)
        }
    }
}

contract RevertingValidatorModule {
    uint256 internal constant PALI_MODULE_TYPE_COMPOSITE_CHILD =
        uint256(keccak256("pali.validator.composite-child.v1"));

    function isModuleType(uint256 moduleTypeId) external pure returns (bool) {
        return moduleTypeId == MODULE_TYPE_VALIDATOR || moduleTypeId == PALI_MODULE_TYPE_COMPOSITE_CHILD;
    }

    function onInstall(bytes calldata) external {}

    function onUninstall(bytes calldata) external {}

    function validateUserOpWithSender(address, PackedUserOperation calldata, bytes32, bytes calldata)
        external
        pure
        returns (uint256)
    {
        revert("broken child validator");
    }

    function isValidSignatureWithSender(address, bytes32, bytes calldata) external pure returns (bytes4) {
        revert("broken child validator");
    }
}

contract LegacyValidatorModule {
    function isModuleType(uint256 moduleTypeId) external pure returns (bool) {
        return moduleTypeId == MODULE_TYPE_VALIDATOR;
    }
}

contract PolicyRestrictedValidatorModule {
    bytes4 internal constant EIP1271_SUCCESS = 0x1626ba7e;
    uint256 internal constant PALI_MODULE_TYPE_COMPOSITE_CHILD =
        uint256(keccak256("pali.validator.composite-child.v1"));

    function isModuleType(uint256 moduleTypeId) external pure returns (bool) {
        return moduleTypeId == MODULE_TYPE_VALIDATOR || moduleTypeId == PALI_MODULE_TYPE_COMPOSITE_CHILD;
    }

    function onInstall(bytes calldata) external {}

    function onUninstall(bytes calldata) external {}

    function validateUserOp(PackedUserOperation calldata, bytes32) external pure returns (uint256) {
        return VALIDATION_FAILED;
    }

    function validateUserOpWithSender(address, PackedUserOperation calldata, bytes32, bytes calldata)
        external
        pure
        returns (uint256)
    {
        // A valid signature with a non-zero validAfter restriction. Composite
        // v1 deliberately rejects richer validationData it cannot propagate.
        return uint256(1) << 208;
    }

    function isValidSignatureWithSender(address, bytes32, bytes calldata) external pure returns (bytes4) {
        return EIP1271_SUCCESS;
    }
}

contract PaliValidatorMalformedSignatureTest is Test {
    bytes4 internal constant EIP1271_FAILED = 0xffffffff;

    PaliCompositeValidatorModule private composite;
    PaliECDSAValidatorModule private ecdsa;
    PaliP256WebAuthnValidatorModule private p256;

    function setUp() public {
        MockP256Precompile mockP256 = new MockP256Precompile();
        vm.etch(address(0x100), address(mockP256).code);

        ecdsa = new PaliECDSAValidatorModule();
        address[] memory owners = new address[](1);
        owners[0] = address(0xA11CE);
        ecdsa.onInstall(abi.encode(owners, uint64(1)));

        composite = new PaliCompositeValidatorModule();
        address[] memory children = new address[](1);
        children[0] = address(ecdsa);
        composite.onInstall(abi.encode(children, uint64(1)));

        p256 = new PaliP256WebAuthnValidatorModule();
        p256.onInstall(
            abi.encode(
                PaliP256WebAuthnValidatorModule.AuthData({
                    publicKeyX: bytes32(uint256(1)),
                    publicKeyY: bytes32(uint256(2)),
                    rpIdHash: bytes32(uint256(4)),
                    originHash: bytes32(uint256(5)),
                    originLength: 1
                })
            )
        );
    }

    function testEcdsaMalformedThresholdSignatureFailsClosed() public view {
        assertEq(ecdsa.isValidSignatureWithSender(address(this), keccak256("pali"), hex"1234"), EIP1271_FAILED);
    }

    function testCompositeMalformedSignatureFailsClosed() public view {
        assertEq(composite.isValidSignatureWithSender(address(this), keccak256("pali"), hex"1234"), EIP1271_FAILED);
    }

    function testCompositeAllowsStricterThresholds() public {
        PaliCompositeValidatorModule strictComposite = new PaliCompositeValidatorModule();
        address[] memory children = new address[](2);
        children[0] = address(ecdsa);
        children[1] = address(p256);

        strictComposite.onInstall(abi.encode(children, uint64(2)));

        assertEq(strictComposite.threshold(address(this)), 2);
    }

    function testP256MalformedSignatureFailsClosed() public view {
        assertEq(p256.isValidSignatureWithSender(address(this), keccak256("pali"), hex"1234"), EIP1271_FAILED);
    }

    function testCompositeRejectsLegacyChildWithoutFullContextMarker() public {
        LegacyValidatorModule legacy = new LegacyValidatorModule();
        PaliCompositeValidatorModule candidate = new PaliCompositeValidatorModule();
        address[] memory children = new address[](1);
        children[0] = address(legacy);

        vm.expectRevert(
            abi.encodeWithSelector(PaliCompositeValidatorModule.InvalidChildValidator.selector, address(legacy))
        );
        candidate.onInstall(abi.encode(children, uint64(1)));
    }

    function isModuleInstalled(uint256, address, bytes calldata) external pure returns (bool) {
        return true;
    }
}

contract PaliCompositeRevertingChildTest is Test {
    bytes4 internal constant EIP1271_SUCCESS = 0x1626ba7e;
    bytes4 internal constant EIP1271_FAILED = 0xffffffff;

    PaliCompositeValidatorModule private composite;
    PaliECDSAValidatorModule private ecdsa;
    RevertingValidatorModule private revertingChild;

    uint256 private ownerPrivateKey = 0xA11CE;
    address private owner;

    function setUp() public {
        owner = vm.addr(ownerPrivateKey);

        ecdsa = new PaliECDSAValidatorModule();
        address[] memory owners = new address[](1);
        owners[0] = owner;
        ecdsa.onInstall(abi.encode(owners, uint64(1)));

        revertingChild = new RevertingValidatorModule();

        composite = new PaliCompositeValidatorModule();
        address[] memory children = new address[](2);
        children[0] = address(revertingChild);
        children[1] = address(ecdsa);
        composite.onInstall(abi.encode(children, uint64(1)));
    }

    function testRevertingChildDoesNotBlockOtherChildrenFromMeetingThreshold() public view {
        bytes32 hash = keccak256("pali");
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(ownerPrivateKey, hash);

        bytes[] memory childSignatures = new bytes[](2);
        childSignatures[0] = hex"deadbeef";
        childSignatures[1] = abi.encodePacked(r, s, bytes1(v));

        assertEq(
            composite.isValidSignatureWithSender(address(this), hash, abi.encode(childSignatures)), EIP1271_SUCCESS
        );
    }

    function testRevertingChildCountsAsInvalidWhenThresholdCannotBeMet() public view {
        bytes32 hash = keccak256("pali");

        bytes[] memory childSignatures = new bytes[](2);
        childSignatures[0] = hex"deadbeef";

        assertEq(composite.isValidSignatureWithSender(address(this), hash, abi.encode(childSignatures)), EIP1271_FAILED);
    }

    function testEcdsaAcceptsCanonicalUserOperationTypedDataSignature() public view {
        bytes32 userOpHash = keccak256("canonical ERC-4337 typed-data hash");
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(ownerPrivateKey, userOpHash);
        PackedUserOperation memory userOp;
        userOp.sender = address(this);
        userOp.signature = abi.encodePacked(r, s, bytes1(v));

        assertEq(ecdsa.validateUserOp(userOp, userOpHash), VALIDATION_SUCCESS);
    }

    function testEcdsaAcceptsCanonicalErc1271Hash() public view {
        bytes32 hash = keccak256("ERC-7739 transformed ERC-1271 hash");
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(ownerPrivateKey, hash);
        assertEq(
            ecdsa.isValidSignatureWithSender(address(this), hash, abi.encodePacked(r, s, bytes1(v))), EIP1271_SUCCESS
        );
    }

    function testCompositeUserOperationUsesCompatibleChildValidation() public {
        bytes32 hash = keccak256("pali user operation");
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(ownerPrivateKey, hash);
        bytes[] memory childSignatures = new bytes[](2);
        childSignatures[1] = abi.encodePacked(r, s, bytes1(v));
        PackedUserOperation memory userOp;
        userOp.sender = address(this);
        userOp.signature = abi.encode(childSignatures);

        assertEq(composite.validateUserOp(userOp, hash), VALIDATION_SUCCESS);
    }

    function testCompositeRejectsDirectUserOperationValidationFromNonAccount() public {
        PackedUserOperation memory userOp;
        userOp.sender = address(this);
        bytes[] memory childSignatures = new bytes[](2);
        childSignatures[1] = hex"01";
        userOp.signature = abi.encode(childSignatures);

        vm.prank(address(0xB0B));
        assertEq(composite.validateUserOp(userOp, keccak256("unauthorized direct call")), VALIDATION_FAILED);
    }

    function isModuleInstalled(uint256, address, bytes calldata) external pure returns (bool) {
        return true;
    }
}

contract PaliCompositePolicyEnforcementTest is Test {
    bytes4 internal constant EIP1271_SUCCESS = 0x1626ba7e;
    bytes4 internal constant EIP1271_FAILED = 0xffffffff;

    PaliCompositeValidatorModule private composite;
    PolicyRestrictedValidatorModule private restrictedChild;
    mapping(address module => bool installed) private _installed;

    function setUp() public {
        restrictedChild = new PolicyRestrictedValidatorModule();
        _installed[address(restrictedChild)] = true;

        composite = new PaliCompositeValidatorModule();
        address[] memory children = new address[](1);
        children[0] = address(restrictedChild);
        composite.onInstall(abi.encode(children, uint64(1)));
    }

    function testCompositeEnforcesChildUserOperationPolicy() public {
        PackedUserOperation memory userOp;
        userOp.sender = address(this);
        bytes[] memory childSignatures = new bytes[](1);
        childSignatures[0] = hex"01";
        userOp.signature = abi.encode(childSignatures);

        assertEq(composite.validateUserOp(userOp, keccak256("restricted user operation")), VALIDATION_FAILED);
    }

    function testUninstalledChildDoesNotCountTowardCompositeThreshold() public {
        _installed[address(restrictedChild)] = false;
        bytes[] memory childSignatures = new bytes[](1);
        childSignatures[0] = hex"01";

        assertEq(
            composite.isValidSignatureWithSender(
                address(this), keccak256("stale child signature"), abi.encode(childSignatures)
            ),
            EIP1271_FAILED
        );
    }

    function testInstalledChildStillCountsTowardCompositeThreshold() public view {
        bytes[] memory childSignatures = new bytes[](1);
        childSignatures[0] = hex"01";

        assertEq(
            composite.isValidSignatureWithSender(
                address(this), keccak256("installed child signature"), abi.encode(childSignatures)
            ),
            EIP1271_SUCCESS
        );
    }

    function isModuleInstalled(uint256 moduleTypeId, address module, bytes calldata) external view returns (bool) {
        return moduleTypeId == MODULE_TYPE_VALIDATOR && _installed[module];
    }
}
