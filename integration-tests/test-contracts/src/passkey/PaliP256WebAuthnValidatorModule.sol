// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {PackedUserOperation} from "@openzeppelin/contracts/interfaces/draft-IERC4337.sol";
import {
    IERC7579Validator,
    MODULE_TYPE_VALIDATOR,
    VALIDATION_FAILED,
    VALIDATION_SUCCESS
} from "@openzeppelin/contracts/interfaces/draft-IERC7579.sol";
import {Base64Url} from "./Base64Url.sol";
import {P256Verifier} from "./P256Verifier.sol";

contract PaliP256WebAuthnValidatorModule is IERC7579Validator {
    bytes4 internal constant EIP1271_SUCCESS = 0x1626ba7e;
    bytes4 internal constant EIP1271_FAILED = 0xffffffff;

    uint256 internal constant WEBAUTHN_AUTH_DATA_MIN_LENGTH = 37;
    bytes1 internal constant WEBAUTHN_FLAG_USER_PRESENT = 0x01;
    bytes1 internal constant WEBAUTHN_FLAG_USER_VERIFIED = 0x04;
    bytes13 internal constant WEBAUTHN_CHALLENGE_PREFIX = '"challenge":"';
    bytes8 internal constant WEBAUTHN_TYPE_PREFIX = '"type":"';
    bytes12 internal constant WEBAUTHN_TYPE_VALUE = "webauthn.get";
    bytes10 internal constant WEBAUTHN_ORIGIN_PREFIX = '"origin":"';

    address internal constant P256_VERIFY_PRECOMPILE = address(0x100);
    bytes32 internal constant P256_SELFTEST_DIGEST = 0x1111111111111111111111111111111111111111111111111111111111111111;
    bytes32 internal constant P256_SELFTEST_R = 0xe3beddb9ba4659cb78bc5473b9cbf30ec64c559fa20c84d691e02eb958d1f349;
    bytes32 internal constant P256_SELFTEST_S = 0x0fd599b0671624232916d74d0045f0bf6b88762159572dfb49850eda48417839;
    bytes32 internal constant P256_SELFTEST_X = 0x6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296;
    bytes32 internal constant P256_SELFTEST_Y = 0x4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5;

    struct AuthData {
        bytes32 publicKeyX;
        bytes32 publicKeyY;
        bytes32 credentialIdHash;
        bytes32 rpIdHash;
        bytes32 originHash;
        uint256 originLength;
    }

    struct WebAuthnProof {
        bytes authenticatorData;
        bytes clientDataJSON;
        uint256 typeOffset;
        uint256 challengeOffset;
        uint256 originOffset;
        bytes32 r;
        bytes32 s;
    }

    error InvalidP256AuthConfig();
    error P256VerifierUnavailable();

    mapping(address account => AuthData) private _authData;

    function onInstall(bytes calldata initData) public override {
        _assertP256VerifierAvailable();

        AuthData memory authData_ = abi.decode(initData, (AuthData));
        if (
            authData_.publicKeyX == bytes32(0) || authData_.publicKeyY == bytes32(0)
                || authData_.credentialIdHash == bytes32(0) || authData_.rpIdHash == bytes32(0)
                || authData_.originHash == bytes32(0) || authData_.originLength == 0
        ) {
            revert InvalidP256AuthConfig();
        }
        _authData[msg.sender] = authData_;
    }

    function onUninstall(bytes calldata) public override {
        delete _authData[msg.sender];
    }

    function authData(address account) external view returns (AuthData memory) {
        return _authData[account];
    }

    function isModuleType(uint256 moduleTypeId) external pure override returns (bool) {
        return moduleTypeId == MODULE_TYPE_VALIDATOR;
    }

    function isInitialized(address account) external view returns (bool) {
        return _authData[account].publicKeyX != bytes32(0);
    }

    function validateUserOp(PackedUserOperation calldata userOp, bytes32 userOpHash)
        external
        view
        override
        returns (uint256)
    {
        return _validateSignature(userOp.sender, userOpHash, userOp.signature) ? VALIDATION_SUCCESS : VALIDATION_FAILED;
    }

    function isValidSignatureWithSender(address sender, bytes32 hash, bytes calldata signature)
        external
        view
        override
        returns (bytes4)
    {
        return _validateSignature(_validationAccount(sender), hash, signature) ? EIP1271_SUCCESS : EIP1271_FAILED;
    }

    function _validateSignature(address account, bytes32 hash, bytes calldata signature) internal view returns (bool) {
        AuthData memory authData_ = _authData[account];
        if (authData_.publicKeyX == bytes32(0)) {
            return false;
        }

        WebAuthnProof memory proof;
        try this.decodeWebAuthnProof(signature) returns (WebAuthnProof memory decodedProof) {
            proof = decodedProof;
        } catch {
            return false;
        }

        if (!_validateWebAuthnEnvelope(authData_, hash, proof)) {
            return false;
        }

        bytes32 digest = sha256(abi.encodePacked(proof.authenticatorData, sha256(proof.clientDataJSON)));
        return P256Verifier.isValid(digest, proof.r, proof.s, authData_.publicKeyX, authData_.publicKeyY);
    }

    function _validationAccount(address sender) private view returns (address) {
        return _authData[msg.sender].publicKeyX != bytes32(0) ? msg.sender : sender;
    }

    function decodeWebAuthnProof(bytes calldata signature) external pure returns (WebAuthnProof memory) {
        return abi.decode(signature, (WebAuthnProof));
    }

    function _validateWebAuthnEnvelope(AuthData memory authData_, bytes32 hash, WebAuthnProof memory proof)
        private
        pure
        returns (bool)
    {
        bytes memory expectedChallenge = Base64Url.encode32(hash);
        return _hasExpectedType(proof.clientDataJSON, proof.typeOffset)
            && _hasExpectedChallenge(proof.clientDataJSON, proof.challengeOffset, expectedChallenge)
            && _hasExpectedOrigin(authData_, proof.clientDataJSON, proof.originOffset)
            && _hasRequiredWebAuthnFlags(proof.authenticatorData)
            && _hasExpectedRpIdHash(authData_, proof.authenticatorData);
    }

    function _hasExpectedType(bytes memory clientData, uint256 typeOffset) private pure returns (bool) {
        if (
            typeOffset < WEBAUTHN_TYPE_PREFIX.length || typeOffset + WEBAUTHN_TYPE_VALUE.length >= clientData.length
                || clientData[typeOffset + WEBAUTHN_TYPE_VALUE.length] != '"'
        ) {
            return false;
        }

        for (uint256 i = 0; i < WEBAUTHN_TYPE_PREFIX.length; ++i) {
            if (clientData[typeOffset - WEBAUTHN_TYPE_PREFIX.length + i] != WEBAUTHN_TYPE_PREFIX[i]) {
                return false;
            }
        }

        for (uint256 i = 0; i < WEBAUTHN_TYPE_VALUE.length; ++i) {
            if (clientData[typeOffset + i] != WEBAUTHN_TYPE_VALUE[i]) {
                return false;
            }
        }

        return true;
    }

    function _hasExpectedChallenge(bytes memory clientData, uint256 challengeOffset, bytes memory expectedChallenge)
        private
        pure
        returns (bool)
    {
        if (
            challengeOffset < WEBAUTHN_CHALLENGE_PREFIX.length
                || challengeOffset + expectedChallenge.length >= clientData.length
                || clientData[challengeOffset + expectedChallenge.length] != '"'
        ) {
            return false;
        }

        for (uint256 i = 0; i < WEBAUTHN_CHALLENGE_PREFIX.length; ++i) {
            if (clientData[challengeOffset - WEBAUTHN_CHALLENGE_PREFIX.length + i] != WEBAUTHN_CHALLENGE_PREFIX[i]) {
                return false;
            }
        }

        for (uint256 i = 0; i < expectedChallenge.length; ++i) {
            if (clientData[challengeOffset + i] != expectedChallenge[i]) {
                return false;
            }
        }

        return true;
    }

    function _hasExpectedOrigin(AuthData memory authData_, bytes memory clientData, uint256 originOffset)
        private
        pure
        returns (bool)
    {
        if (
            originOffset < WEBAUTHN_ORIGIN_PREFIX.length || originOffset + authData_.originLength >= clientData.length
                || clientData[originOffset + authData_.originLength] != '"'
        ) {
            return false;
        }

        for (uint256 i = 0; i < WEBAUTHN_ORIGIN_PREFIX.length; ++i) {
            if (clientData[originOffset - WEBAUTHN_ORIGIN_PREFIX.length + i] != WEBAUTHN_ORIGIN_PREFIX[i]) {
                return false;
            }
        }

        bytes32 actualOriginHash;
        uint256 originLength = authData_.originLength;
        assembly ("memory-safe") {
            actualOriginHash := keccak256(add(add(clientData, 0x20), originOffset), originLength)
        }
        return actualOriginHash == authData_.originHash;
    }

    function _hasRequiredWebAuthnFlags(bytes memory authenticatorData) private pure returns (bool) {
        if (authenticatorData.length < WEBAUTHN_AUTH_DATA_MIN_LENGTH) {
            return false;
        }

        bytes1 flags = authenticatorData[32];
        return (flags & WEBAUTHN_FLAG_USER_PRESENT) == WEBAUTHN_FLAG_USER_PRESENT
            && (flags & WEBAUTHN_FLAG_USER_VERIFIED) == WEBAUTHN_FLAG_USER_VERIFIED;
    }

    function _hasExpectedRpIdHash(AuthData memory authData_, bytes memory authenticatorData)
        private
        pure
        returns (bool)
    {
        if (authenticatorData.length < WEBAUTHN_AUTH_DATA_MIN_LENGTH) {
            return false;
        }

        bytes32 actualRpIdHash;
        assembly ("memory-safe") {
            actualRpIdHash := mload(add(authenticatorData, 0x20))
        }
        return actualRpIdHash == authData_.rpIdHash;
    }

    function _assertP256VerifierAvailable() private view {
        (bool success, bytes memory result) = P256_VERIFY_PRECOMPILE.staticcall(
            abi.encodePacked(P256_SELFTEST_DIGEST, P256_SELFTEST_R, P256_SELFTEST_S, P256_SELFTEST_X, P256_SELFTEST_Y)
        );

        if (!success || result.length != 32 || abi.decode(result, (uint256)) != 1) {
            revert P256VerifierUnavailable();
        }
    }
}
