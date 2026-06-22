// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";
import {Base64Url} from "contracts/src/pali/Base64Url.sol";
import {PaliP256WebAuthnValidatorModule} from "contracts/src/pali/PaliP256WebAuthnValidatorModule.sol";

contract PaliP256WebAuthnValidatorModuleTest is Test {
    bytes4 internal constant EIP1271_SUCCESS = 0x1626ba7e;
    bytes4 internal constant EIP1271_FAILED = 0xffffffff;

    bytes32 private constant HASH = 0x1111111111111111111111111111111111111111111111111111111111111111;
    bytes32 private constant PUBLIC_KEY_X = bytes32(uint256(1));
    bytes32 private constant PUBLIC_KEY_Y = bytes32(uint256(2));
    bytes32 private constant RP_ID_HASH = bytes32(uint256(3));
    string private constant ORIGIN = "https://pali.test";

    PaliP256WebAuthnValidatorModule private module;

    function setUp() public {
        vm.etch(address(0x100), address(new MockP256WebAuthnPrecompile()).code);

        module = new PaliP256WebAuthnValidatorModule();
        module.onInstall(
            abi.encode(
                PaliP256WebAuthnValidatorModule.AuthData({
                    publicKeyX: PUBLIC_KEY_X,
                    publicKeyY: PUBLIC_KEY_Y,
                    rpIdHash: RP_ID_HASH,
                    originHash: keccak256(bytes(ORIGIN)),
                    originLength: bytes(ORIGIN).length
                })
            )
        );
    }

    function testValidWebAuthnEnvelopeUsesSuppliedOffsets() public view {
        bytes memory signature = _signature(_clientDataJSON("webauthn.get", Base64Url.encode32(HASH), ORIGIN));

        assertEq(module.isValidSignatureWithSender(address(0xB0B), HASH, signature), EIP1271_SUCCESS);
    }

    function testWrongChallengeOffsetFailsClosed() public view {
        bytes memory clientData = _clientDataJSON("webauthn.get", Base64Url.encode32(HASH), ORIGIN);
        PaliP256WebAuthnValidatorModule.WebAuthnProof memory proof = _proof(clientData);
        proof.challengeOffset = proof.challengeOffset + 1;

        assertEq(module.isValidSignatureWithSender(address(0xB0B), HASH, abi.encode(proof)), EIP1271_FAILED);
    }

    function testWrongOriginOffsetFailsClosedWithDuplicateOriginKey() public view {
        bytes memory clientData = bytes.concat(
            bytes('{"origin":"https://evil.test",'),
            bytes('"type":"webauthn.get",'),
            bytes('"challenge":"'),
            Base64Url.encode32(HASH),
            bytes('","origin":"'),
            bytes(ORIGIN),
            bytes('"}')
        );
        PaliP256WebAuthnValidatorModule.WebAuthnProof memory proof = _proof(clientData);
        proof.originOffset = _indexOf(clientData, bytes("https://evil.test"));

        assertEq(module.isValidSignatureWithSender(address(0xB0B), HASH, abi.encode(proof)), EIP1271_FAILED);
    }

    function testWrongTypeOffsetFailsClosedWithDuplicateTypeKey() public view {
        bytes memory clientData = bytes.concat(
            bytes('{"type":"not-webauthn",'),
            bytes('"type":"webauthn.get",'),
            bytes('"challenge":"'),
            Base64Url.encode32(HASH),
            bytes('","origin":"'),
            bytes(ORIGIN),
            bytes('"}')
        );
        PaliP256WebAuthnValidatorModule.WebAuthnProof memory proof = _proof(clientData);
        proof.typeOffset = _indexOf(clientData, bytes("not-webauthn"));

        assertEq(module.isValidSignatureWithSender(address(0xB0B), HASH, abi.encode(proof)), EIP1271_FAILED);
    }

    function _signature(bytes memory clientData) private pure returns (bytes memory) {
        return abi.encode(_proof(clientData));
    }

    function _proof(bytes memory clientData)
        private
        pure
        returns (PaliP256WebAuthnValidatorModule.WebAuthnProof memory)
    {
        return PaliP256WebAuthnValidatorModule.WebAuthnProof({
            authenticatorData: abi.encodePacked(RP_ID_HASH, bytes1(0x05), bytes4(0)),
            clientDataJSON: clientData,
            typeOffset: _indexOf(clientData, bytes("webauthn.get")),
            challengeOffset: _indexOf(clientData, Base64Url.encode32(HASH)),
            originOffset: _lastIndexOf(clientData, bytes(ORIGIN)),
            r: bytes32(uint256(1)),
            s: bytes32(uint256(1))
        });
    }

    function _clientDataJSON(string memory typeValue, bytes memory challenge, string memory origin)
        private
        pure
        returns (bytes memory)
    {
        return bytes.concat(
            bytes('{"type":"'),
            bytes(typeValue),
            bytes('","challenge":"'),
            challenge,
            bytes('","origin":"'),
            bytes(origin),
            bytes('"}')
        );
    }

    function _indexOf(bytes memory haystack, bytes memory needle) private pure returns (uint256) {
        for (uint256 i = 0; i <= haystack.length - needle.length; ++i) {
            if (_matchesAt(haystack, needle, i)) {
                return i;
            }
        }
        revert("needle not found");
    }

    function _lastIndexOf(bytes memory haystack, bytes memory needle) private pure returns (uint256 found) {
        bool didFind;
        for (uint256 i = 0; i <= haystack.length - needle.length; ++i) {
            if (_matchesAt(haystack, needle, i)) {
                found = i;
                didFind = true;
            }
        }
        require(didFind, "needle not found");
    }

    function _matchesAt(bytes memory haystack, bytes memory needle, uint256 offset) private pure returns (bool) {
        for (uint256 i = 0; i < needle.length; ++i) {
            if (haystack[offset + i] != needle[i]) {
                return false;
            }
        }
        return true;
    }
}

contract MockP256WebAuthnPrecompile {
    fallback() external {
        assembly {
            mstore(0x00, 1)
            return(0x00, 0x20)
        }
    }
}
