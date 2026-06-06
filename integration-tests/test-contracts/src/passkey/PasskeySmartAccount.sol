// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import {Base64Url} from "./Base64Url.sol";
import {P256Verifier} from "./P256Verifier.sol";

contract PasskeySmartAccount {
    bytes4 internal constant EIP1271_MAGIC_VALUE = 0x1626ba7e;
    bytes4 internal constant EIP1271_INVALID_VALUE = 0xffffffff;
    bytes32 internal constant PASSKEY_EXECUTE_TYPEHASH = keccak256("PALI_PASSKEY_SMART_ACCOUNT_EXECUTE_V1");
    bytes4 internal constant SET_SPONSOR_SELECTOR = PasskeySmartAccount.setSponsor.selector;
    uint256 internal constant MAX_SPONSOR_URL_LENGTH = 128;
    uint256 internal constant WEBAUTHN_AUTH_DATA_MIN_LENGTH = 37;
    bytes1 internal constant WEBAUTHN_FLAG_USER_PRESENT = 0x01;
    bytes1 internal constant WEBAUTHN_FLAG_USER_VERIFIED = 0x04;
    bytes13 internal constant WEBAUTHN_CHALLENGE_PREFIX = '"challenge":"';
    bytes8 internal constant WEBAUTHN_TYPE_PREFIX = '"type":"';
    bytes12 internal constant WEBAUTHN_TYPE_VALUE = "webauthn.get";
    bytes10 internal constant WEBAUTHN_ORIGIN_PREFIX = '"origin":"';
    uint256 internal constant SECP256K1_HALF_ORDER =
        0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0;

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

    struct AccountParams {
        bytes32 passkeyX;
        bytes32 passkeyY;
        bytes32 credentialIdHash;
        bytes32 rpIdHash;
        bytes32 originHash;
        uint256 originLength;
        address recoveryValidator;
        bytes32 salt;
    }

    struct PasskeyIdentity {
        bytes32 passkeyX;
        bytes32 passkeyY;
        bytes32 credentialIdHash;
        bytes32 rpIdHash;
        bytes32 originHash;
        uint256 originLength;
    }

    struct RecoveryMetadata {
        bytes32 passkeyX;
        bytes32 passkeyY;
        bytes32 credentialIdHash;
        bytes32 rpIdHash;
        bytes32 originHash;
        uint256 originLength;
        SponsorMode sponsorMode;
        address sponsorSigner;
        string sponsorUrl;
    }

    event Executed(bytes32 indexed actionHash, address indexed target, uint256 value, address indexed submitter);
    event SponsorUpdated(SponsorMode mode, address indexed signer, string url);
    event PasskeyRecovered(
        bytes32 indexed credentialIdHash,
        bytes32 passkeyX,
        bytes32 passkeyY,
        bytes32 rpIdHash,
        bytes32 originHash,
        uint256 originLength,
        uint256 recoveryNonce
    );

    error BadChallenge();
    error BadWebAuthnAuthenticatorData();
    error BadWebAuthnRpIdHash();
    error BadNonce(uint256 expected, uint256 provided);
    error CallFailed(bytes returndata);
    error Expired();
    error AlreadyInitialized();
    error InvalidSponsor();
    error OnlySelf();
    error OnlyRecoveryValidator();
    error BadRecoveryNonce(uint256 expected, uint256 provided);
    error SponsorRequired();
    error SponsorUrlTooLong();

    bytes32 private passkeyX;
    bytes32 private passkeyY;
    bytes32 private credentialIdHash;
    bytes32 private rpIdHash;
    bytes32 private originHash;
    uint256 private originLength;
    address public recoveryValidator;

    bool private initialized;
    uint256 public nonce;
    uint256 public recoveryNonce;
    SponsorMode private sponsorMode;
    address private sponsorSigner;
    string private sponsorUrl;

    modifier onlySelf() {
        if (msg.sender != address(this)) {
            revert OnlySelf();
        }
        _;
    }

    modifier onlyRecoveryValidator() {
        if (msg.sender != recoveryValidator) {
            revert OnlyRecoveryValidator();
        }
        _;
    }

    constructor() payable {
        initialized = true;
    }

    receive() external payable {}

    function initialize(AccountParams calldata params) external {
        if (initialized) {
            revert AlreadyInitialized();
        }

        initialized = true;
        passkeyX = params.passkeyX;
        passkeyY = params.passkeyY;
        credentialIdHash = params.credentialIdHash;
        rpIdHash = params.rpIdHash;
        originHash = params.originHash;
        originLength = params.originLength;
        recoveryValidator = params.recoveryValidator;
    }

    function getRecoveryMetadata() external view returns (RecoveryMetadata memory metadata) {
        metadata = RecoveryMetadata({
            passkeyX: passkeyX,
            passkeyY: passkeyY,
            credentialIdHash: credentialIdHash,
            rpIdHash: rpIdHash,
            originHash: originHash,
            originLength: originLength,
            sponsorMode: sponsorMode,
            sponsorSigner: sponsorSigner,
            sponsorUrl: sponsorUrl
        });
    }

    function execute(Execution[] calldata executions, WebAuthnProof calldata proof, SponsorProof calldata sponsorProof)
        external
        payable
        returns (bytes[] memory returndata)
    {
        if (executions.length == 0) {
            return new bytes[](0);
        }
        if (executions.length == 1) {
            return _executeSingle(executions[0], proof, sponsorProof);
        }

        uint256 expectedNonce = nonce;
        for (uint256 i = 0; i < executions.length; ++i) {
            if (executions[i].deadline < block.timestamp) {
                revert Expired();
            }
            if (executions[i].nonce != expectedNonce + i) {
                revert BadNonce(expectedNonce + i, executions[i].nonce);
            }
        }

        bytes32 actionHash = getActionHash(executions);
        _verifyWebAuthnProof(actionHash, proof);
        _verifyBatchSponsor(actionHash, sponsorProof, executions);

        nonce = expectedNonce + executions.length;
        returndata = new bytes[](executions.length);
        for (uint256 i = 0; i < executions.length; ++i) {
            (bool success, bytes memory result) =
                executions[i].target.call{value: executions[i].value}(executions[i].data);
            if (!success) {
                revert CallFailed(result);
            }

            returndata[i] = result;
            emit Executed(actionHash, executions[i].target, executions[i].value, msg.sender);
        }
    }

    function _executeSingle(
        Execution calldata execution,
        WebAuthnProof calldata proof,
        SponsorProof calldata sponsorProof
    ) internal returns (bytes[] memory returndata) {
        if (execution.deadline < block.timestamp) {
            revert Expired();
        }

        uint256 expectedNonce = nonce;
        if (execution.nonce != expectedNonce) {
            revert BadNonce(expectedNonce, execution.nonce);
        }

        SponsorMode currentSponsorMode = sponsorMode;
        address currentSponsorSigner = sponsorSigner;
        bytes32 actionHash = _getSingleActionHash(execution, currentSponsorMode, currentSponsorSigner);
        _verifyWebAuthnProof(actionHash, proof);
        _verifySingleSponsor(actionHash, sponsorProof, execution, currentSponsorMode, currentSponsorSigner);

        nonce = expectedNonce + 1;
        returndata = new bytes[](1);
        (bool success, bytes memory result) = execution.target.call{value: execution.value}(execution.data);
        if (!success) {
            revert CallFailed(result);
        }

        returndata[0] = result;
        emit Executed(actionHash, execution.target, execution.value, msg.sender);
    }

    function setSponsor(SponsorMode mode, address signer, string calldata url) external onlySelf {
        _setSponsor(mode, signer, url);
    }

    function recoverPasskey(
        PasskeyIdentity calldata newIdentity,
        uint256 expectedRecoveryNonce
    ) external onlyRecoveryValidator {
        uint256 currentRecoveryNonce = recoveryNonce;
        if (expectedRecoveryNonce != currentRecoveryNonce) {
            revert BadRecoveryNonce(currentRecoveryNonce, expectedRecoveryNonce);
        }

        passkeyX = newIdentity.passkeyX;
        passkeyY = newIdentity.passkeyY;
        credentialIdHash = newIdentity.credentialIdHash;
        rpIdHash = newIdentity.rpIdHash;
        originHash = newIdentity.originHash;
        originLength = newIdentity.originLength;
        recoveryNonce = currentRecoveryNonce + 1;

        emit PasskeyRecovered(
            newIdentity.credentialIdHash,
            newIdentity.passkeyX,
            newIdentity.passkeyY,
            newIdentity.rpIdHash,
            newIdentity.originHash,
            newIdentity.originLength,
            currentRecoveryNonce
        );
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

    function getActionHash(Execution[] calldata executions) public view returns (bytes32) {
        if (executions.length == 1) {
            return _getSingleActionHash(executions[0], sponsorMode, sponsorSigner);
        }

        bytes32[] memory executionHashes = new bytes32[](executions.length);
        for (uint256 i = 0; i < executions.length; ++i) {
            executionHashes[i] = _getExecutionHash(executions[i]);
        }

        return _getActionHashFromExecutionRoot(keccak256(abi.encodePacked(executionHashes)), sponsorMode, sponsorSigner);
    }

    function _getSingleActionHash(
        Execution calldata execution,
        SponsorMode currentSponsorMode,
        address currentSponsorSigner
    ) internal view returns (bytes32) {
        return _getActionHashFromExecutionRoot(
            _getSingleExecutionRoot(_getExecutionHash(execution)), currentSponsorMode, currentSponsorSigner
        );
    }

    function _getExecutionHash(Execution calldata execution) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(execution.target, execution.value, keccak256(execution.data), execution.nonce, execution.deadline)
        );
    }

    function _getSingleExecutionRoot(bytes32 executionHash) internal pure returns (bytes32 executionRoot) {
        assembly {
            let ptr := mload(0x40)
            mstore(ptr, executionHash)
            executionRoot := keccak256(ptr, 0x20)
        }
    }

    function _getActionHashFromExecutionRoot(
        bytes32 executionRoot,
        SponsorMode currentSponsorMode,
        address currentSponsorSigner
    ) internal view returns (bytes32) {
        return keccak256(
            abi.encode(
                PASSKEY_EXECUTE_TYPEHASH,
                block.chainid,
                address(this),
                executionRoot,
                currentSponsorMode,
                currentSponsorSigner
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

    function _verifyBatchSponsor(bytes32 actionHash, SponsorProof calldata proof, Execution[] calldata executions)
        internal
        view
    {
        if (_verifyRequiredSponsorMode(actionHash, proof, sponsorMode, sponsorSigner)) {
            return;
        }

        address requiredSigner = _requiredSponsorSignerFromBatch(executions);
        if (requiredSigner != address(0)) {
            _verifySponsorSigner(actionHash, proof, requiredSigner);
        }
    }

    function _verifyRequiredSponsorMode(
        bytes32 actionHash,
        SponsorProof calldata proof,
        SponsorMode currentSponsorMode,
        address currentSponsorSigner
    ) internal pure returns (bool) {
        if (currentSponsorMode != SponsorMode.Required) {
            return false;
        }
        if (currentSponsorSigner == address(0)) {
            revert SponsorRequired();
        }

        _verifySponsorSigner(actionHash, proof, currentSponsorSigner);
        return true;
    }

    function _verifySponsorSigner(bytes32 actionHash, SponsorProof calldata proof, address signer) internal pure {
        if ((proof.v != 27 && proof.v != 28) || uint256(proof.s) > SECP256K1_HALF_ORDER) {
            revert InvalidSponsor();
        }

        bytes32 sponsorDigest = keccak256(abi.encodePacked("\x19Ethereum Signed Message:\n32", actionHash));
        address recovered = ecrecover(sponsorDigest, proof.v, proof.r, proof.s);
        if (recovered == address(0) || recovered != signer) {
            revert InvalidSponsor();
        }
    }

    function _requiredSponsorSignerFromBatch(Execution[] calldata executions) internal view returns (address) {
        for (uint256 i = 0; i < executions.length; ++i) {
            if (executions[i].target != address(this)) {
                continue;
            }

            (bool isSetSponsor, SponsorMode mode, address signer) = _decodeSetSponsor(executions[i].data);
            if (isSetSponsor && mode == SponsorMode.Required) {
                if (signer == address(0)) {
                    revert SponsorRequired();
                }
                return signer;
            }
        }

        return address(0);
    }

    function _verifySingleSponsor(
        bytes32 actionHash,
        SponsorProof calldata proof,
        Execution calldata execution,
        SponsorMode currentSponsorMode,
        address currentSponsorSigner
    ) internal view {
        if (_verifyRequiredSponsorMode(actionHash, proof, currentSponsorMode, currentSponsorSigner)) {
            return;
        }

        if (execution.target != address(this)) {
            return;
        }

        (bool isSetSponsor, SponsorMode mode, address signer) = _decodeSetSponsor(execution.data);
        if (isSetSponsor && mode == SponsorMode.Required) {
            if (signer == address(0)) {
                revert SponsorRequired();
            }
            _verifySponsorSigner(actionHash, proof, signer);
        }
    }

    function _decodeSetSponsor(bytes calldata data)
        internal
        pure
        returns (bool isSetSponsor, SponsorMode mode, address signer)
    {
        if (data.length < 4 + 32 * 4) {
            return (false, SponsorMode.None, address(0));
        }

        bytes4 selector;
        assembly {
            selector := calldataload(data.offset)
        }
        if (selector != SET_SPONSOR_SELECTOR) {
            return (false, SponsorMode.None, address(0));
        }

        string memory url;
        (mode, signer, url) = abi.decode(data[4:], (SponsorMode, address, string));
        url;
        return (true, mode, signer);
    }

    function _setSponsor(SponsorMode mode, address signer, string calldata url) internal {
        if (mode == SponsorMode.Required && signer == address(0)) {
            revert SponsorRequired();
        }
        if (bytes(url).length > MAX_SPONSOR_URL_LENGTH) {
            revert SponsorUrlTooLong();
        }
        sponsorMode = mode;
        sponsorSigner = signer;
        sponsorUrl = url;
        emit SponsorUpdated(mode, signer, url);
    }
}
