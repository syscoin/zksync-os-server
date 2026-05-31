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

    enum SponsorMode {
        None,
        GasOnly,
        Required
    }

    struct WebAuthnProof {
        bytes authenticatorData;
        bytes clientDataJSON;
        uint256 challengeOffset;
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

    uint256 public nonce;
    SponsorMode public sponsorMode;
    address public sponsorSigner;
    bytes32 public sponsorUrlHash;

    constructor(
        bytes32 passkeyX_,
        bytes32 passkeyY_,
        bytes32 credentialIdHash_,
        bytes32 rpIdHash_,
        SponsorMode sponsorMode_,
        address sponsorSigner_,
        bytes32 sponsorUrlHash_
    ) payable {
        passkeyX = passkeyX_;
        passkeyY = passkeyY_;
        credentialIdHash = credentialIdHash_;
        rpIdHash = rpIdHash_;
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

        if (proof.challengeOffset + expectedChallenge.length > clientData.length) {
            revert BadChallenge();
        }

        for (uint256 i = 0; i < expectedChallenge.length; ++i) {
            if (clientData[proof.challengeOffset + i] != expectedChallenge[i]) {
                revert BadChallenge();
            }
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

        if (proof.challengeOffset + expectedChallenge.length > clientData.length) {
            return false;
        }

        for (uint256 i = 0; i < expectedChallenge.length; ++i) {
            if (clientData[proof.challengeOffset + i] != expectedChallenge[i]) {
                return false;
            }
        }

        if (!_hasRequiredWebAuthnFlags(proof.authenticatorData)) {
            return false;
        }
        if (!_hasExpectedRpIdHash(proof.authenticatorData)) {
            return false;
        }

        bytes32 clientDataHash = sha256(clientData);
        bytes32 digest = sha256(bytes.concat(proof.authenticatorData, clientDataHash));
        return P256Verifier.isValid(digest, proof.r, proof.s, passkeyX, passkeyY);
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
