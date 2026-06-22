// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {MODULE_TYPE_VALIDATOR} from "@openzeppelin/contracts/interfaces/draft-IERC7579.sol";
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
    function isModuleType(uint256 moduleTypeId) external pure returns (bool) {
        return moduleTypeId == MODULE_TYPE_VALIDATOR;
    }

    function onInstall(bytes calldata) external {}

    function onUninstall(bytes calldata) external {}

    function isValidSignatureWithSender(address, bytes32, bytes calldata) external pure returns (bytes4) {
        revert("broken child validator");
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

    function isModuleInstalled(uint256, address, bytes calldata) external pure returns (bool) {
        return true;
    }
}
