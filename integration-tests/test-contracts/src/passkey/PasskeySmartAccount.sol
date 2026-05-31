// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import {Base64Url} from "./Base64Url.sol";
import {P256Verifier} from "./P256Verifier.sol";

contract PasskeySmartAccount {
    bytes4 internal constant EIP1271_MAGIC_VALUE = 0x1626ba7e;
    bytes4 internal constant EIP1271_INVALID_VALUE = 0xffffffff;
    uint256 internal constant WEBAUTHN_AUTH_DATA_MIN_LENGTH = 37;
    bytes1 internal constant WEBAUTHN_FLAG_USER_PRESENT = 0x01;
    bytes1 internal constant WEBAUTHN_FLAG_USER_VERIFIED = 0x04;
    bytes13 internal constant WEBAUTHN_CHALLENGE_PREFIX = '"challenge":"';
    bytes8 internal constant WEBAUTHN_TYPE_PREFIX = '"type":"';
    bytes12 internal constant WEBAUTHN_TYPE_VALUE = "webauthn.get";
    bytes10 internal constant WEBAUTHN_ORIGIN_PREFIX = '"origin":"';

    enum SponsorMode {
        None,
        GasOnly,
        Required
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

    struct SponsorProof {
        uint8 v;
        bytes32 r;
        bytes32 s;
    }

    struct Execution {
        address target;
        uint256 value;
        bytes data;
        uint256 nonce;
        uint256 deadline;
    }

    event Executed(bytes32 indexed actionHash, address indexed target, uint256 value, address indexed submitter);
    event SponsorUpdated(SponsorMode mode, address indexed signer, bytes32 urlHash);

    error BadChallenge();
    error BadWebAuthnAuthenticatorData();
    error BadWebAuthnRpIdHash();
    error BadNonce(uint256 expected, uint256 provided);
    error CallFailed(bytes returndata);
    error Expired();
    error InvalidSponsor();
    error OnlySelf();
    error SponsorRequired();

    bytes32 public immutable passkeyX;
    bytes32 public immutable passkeyY;
    bytes32 public immutable credentialIdHash;
    bytes32 public immutable rpIdHash;
    bytes32 public immutable originHash;
    uint256 public immutable originLength;

    uint256 public nonce;
    SponsorMode public sponsorMode;
    address public sponsorSigner;
    bytes32 public sponsorUrlHash;

    constructor(
        bytes32 passkeyX_,
        bytes32 passkeyY_,
        bytes32 credentialIdHash_,
        bytes32 rpIdHash_,
        bytes32 originHash_,
        uint256 originLength_,
        SponsorMode sponsorMode_,
        address sponsorSigner_,
        bytes32 sponsorUrlHash_
    ) payable {
        passkeyX = passkeyX_;
        passkeyY = passkeyY_;
        credentialIdHash = credentialIdHash_;
        rpIdHash = rpIdHash_;
        originHash = originHash_;
        originLength = originLength_;
        _setSponsor(sponsorMode_, sponsorSigner_, sponsorUrlHash_);
    }

    receive() external payable {}

    function execute(Execution calldata execution, WebAuthnProof calldata proof, SponsorProof calldata sponsorProof)
        external
        payable
        returns (bytes memory returndata)
    {
        if (execution.deadline < block.timestamp) {
            revert Expired();
        }
        if (execution.nonce != nonce) {
            revert BadNonce(nonce, execution.nonce);
        }

        bytes32 actionHash = getActionHash(execution);
        _verifyWebAuthnProof(actionHash, proof);
        _verifySponsor(actionHash, sponsorProof);

        unchecked {
            nonce = execution.nonce + 1;
        }

        (bool success, bytes memory result) = execution.target.call{value: execution.value}(execution.data);
        if (!success) {
            revert CallFailed(result);
        }

        emit Executed(actionHash, execution.target, execution.value, msg.sender);
        return result;
    }

    function setSponsor(SponsorMode mode, address signer, bytes32 urlHash) external {
        if (msg.sender != address(this)) {
            revert OnlySelf();
        }
        _setSponsor(mode, signer, urlHash);
    }

    function isValidSignature(bytes32 hash, bytes calldata signature) external view returns (bytes4) {
        try this.validateSignature(hash, signature) returns (bool valid) {
            return valid ? EIP1271_MAGIC_VALUE : EIP1271_INVALID_VALUE;
        } catch {
            return EIP1271_INVALID_VALUE;
        }
    }

    function validateSignature(bytes32 hash, bytes calldata signature) external view returns (bool) {
        WebAuthnProof memory proof = abi.decode(signature, (WebAuthnProof));
        return _isValidWebAuthnProof(hash, proof);
    }

    function getActionHash(Execution calldata execution) public view returns (bytes32) {
        return keccak256(
            abi.encode(
                keccak256("PALI_PASSKEY_SMART_ACCOUNT_EXECUTE_V1"),
                block.chainid,
                address(this),
                execution.target,
                execution.value,
                keccak256(execution.data),
                execution.nonce,
                execution.deadline,
                sponsorMode,
                sponsorSigner
            )
        );
    }

    function _verifyWebAuthnProof(bytes32 actionHash, WebAuthnProof calldata proof) internal view {
        bytes memory expectedChallenge = Base64Url.encode32(actionHash);
        bytes calldata clientData = proof.clientDataJSON;

        if (
            !_hasExpectedTypeCalldata(clientData, proof.typeOffset)
                || !_hasExpectedChallengeCalldata(clientData, proof.challengeOffset, expectedChallenge)
                || !_hasExpectedOriginCalldata(clientData, proof.originOffset)
        ) {
            revert BadChallenge();
        }

        if (!_hasRequiredWebAuthnFlags(proof.authenticatorData)) {
            revert BadWebAuthnAuthenticatorData();
        }
        if (!_hasExpectedRpIdHash(proof.authenticatorData)) {
            revert BadWebAuthnRpIdHash();
        }

        bytes32 clientDataHash = sha256(clientData);
        bytes32 digest = sha256(bytes.concat(proof.authenticatorData, clientDataHash));
        P256Verifier.verify(digest, proof.r, proof.s, passkeyX, passkeyY);
    }

    function _isValidWebAuthnProof(bytes32 actionHash, WebAuthnProof memory proof) internal view returns (bool) {
        bytes memory expectedChallenge = Base64Url.encode32(actionHash);
        bytes memory clientData = proof.clientDataJSON;

        if (
            !_hasExpectedTypeMemory(clientData, proof.typeOffset)
                || !_hasExpectedChallengeMemory(clientData, proof.challengeOffset, expectedChallenge)
                || !_hasExpectedOriginMemory(clientData, proof.originOffset)
                || !_hasRequiredWebAuthnFlags(proof.authenticatorData)
        ) {
            return false;
        }
        if (!_hasExpectedRpIdHash(proof.authenticatorData)) {
            return false;
        }

        bytes32 clientDataHash = sha256(clientData);
        bytes32 digest = sha256(bytes.concat(proof.authenticatorData, clientDataHash));
        return P256Verifier.isValid(digest, proof.r, proof.s, passkeyX, passkeyY);
    }

    function _hasExpectedTypeCalldata(bytes calldata clientData, uint256 typeOffset) internal pure returns (bool) {
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

    function _hasExpectedTypeMemory(bytes memory clientData, uint256 typeOffset) internal pure returns (bool) {
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

    function _hasExpectedChallengeCalldata(
        bytes calldata clientData,
        uint256 challengeOffset,
        bytes memory expectedChallenge
    ) internal pure returns (bool) {
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

    function _hasExpectedChallengeMemory(
        bytes memory clientData,
        uint256 challengeOffset,
        bytes memory expectedChallenge
    ) internal pure returns (bool) {
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

    function _hasExpectedOriginCalldata(bytes calldata clientData, uint256 expectedOriginOffset)
        internal
        view
        returns (bool)
    {
        if (
            expectedOriginOffset < WEBAUTHN_ORIGIN_PREFIX.length
                || expectedOriginOffset + originLength >= clientData.length
                || clientData[expectedOriginOffset + originLength] != '"'
        ) {
            return false;
        }

        for (uint256 i = 0; i < WEBAUTHN_ORIGIN_PREFIX.length; ++i) {
            if (clientData[expectedOriginOffset - WEBAUTHN_ORIGIN_PREFIX.length + i] != WEBAUTHN_ORIGIN_PREFIX[i]) {
                return false;
            }
        }

        return keccak256(clientData[expectedOriginOffset:expectedOriginOffset + originLength]) == originHash;
    }

    function _hasExpectedOriginMemory(bytes memory clientData, uint256 expectedOriginOffset)
        internal
        view
        returns (bool)
    {
        uint256 expectedOriginLength = originLength;
        if (
            expectedOriginOffset < WEBAUTHN_ORIGIN_PREFIX.length
                || expectedOriginOffset + expectedOriginLength >= clientData.length
                || clientData[expectedOriginOffset + expectedOriginLength] != '"'
        ) {
            return false;
        }

        for (uint256 i = 0; i < WEBAUTHN_ORIGIN_PREFIX.length; ++i) {
            if (clientData[expectedOriginOffset - WEBAUTHN_ORIGIN_PREFIX.length + i] != WEBAUTHN_ORIGIN_PREFIX[i]) {
                return false;
            }
        }

        bytes32 actualOriginHash;
        assembly {
            actualOriginHash := keccak256(add(add(clientData, 0x20), expectedOriginOffset), expectedOriginLength)
        }
        return actualOriginHash == originHash;
    }

    function _hasRequiredWebAuthnFlags(bytes memory authenticatorData) internal pure returns (bool) {
        if (authenticatorData.length < WEBAUTHN_AUTH_DATA_MIN_LENGTH) {
            return false;
        }

        bytes1 flags = authenticatorData[32];
        return (flags & WEBAUTHN_FLAG_USER_PRESENT) == WEBAUTHN_FLAG_USER_PRESENT
            && (flags & WEBAUTHN_FLAG_USER_VERIFIED) == WEBAUTHN_FLAG_USER_VERIFIED;
    }

    function _hasExpectedRpIdHash(bytes memory authenticatorData) internal view returns (bool) {
        if (authenticatorData.length < WEBAUTHN_AUTH_DATA_MIN_LENGTH) {
            return false;
        }

        bytes32 actualRpIdHash;
        assembly {
            actualRpIdHash := mload(add(authenticatorData, 0x20))
        }
        return actualRpIdHash == rpIdHash;
    }

    function _verifySponsor(bytes32 actionHash, SponsorProof calldata proof) internal view {
        if (sponsorMode != SponsorMode.Required) {
            return;
        }
        if (sponsorSigner == address(0)) {
            revert SponsorRequired();
        }

        bytes32 sponsorDigest = keccak256(abi.encodePacked("\x19Ethereum Signed Message:\n32", actionHash));
        address recovered = ecrecover(sponsorDigest, proof.v, proof.r, proof.s);
        if (recovered == address(0) || recovered != sponsorSigner) {
            revert InvalidSponsor();
        }
    }

    function _setSponsor(SponsorMode mode, address signer, bytes32 urlHash) internal {
        if (mode == SponsorMode.Required && signer == address(0)) {
            revert SponsorRequired();
        }
        sponsorMode = mode;
        sponsorSigner = signer;
        sponsorUrlHash = urlHash;
        emit SponsorUpdated(mode, signer, urlHash);
    }
}
