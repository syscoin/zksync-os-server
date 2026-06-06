// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import {PasskeySmartAccount} from "./PasskeySmartAccount.sol";

contract PasskeyGuardianRecoveryValidator {
    bytes32 internal constant RECOVERY_TYPEHASH = keccak256("PALI_PASSKEY_GUARDIAN_RECOVERY_V1");
    uint256 internal constant SECP256K1_HALF_ORDER =
        0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0;

    struct RecoveryPolicy {
        uint256 delay;
        uint256 threshold;
        uint256 guardianCount;
    }

    struct GuardianSignature {
        address guardian;
        uint8 v;
        bytes32 r;
        bytes32 s;
    }

    struct StartRecoveryData {
        PasskeySmartAccount account;
        PasskeySmartAccount.PasskeyIdentity newIdentity;
        uint256 expectedRecoveryNonce;
        uint256 expiresAt;
        GuardianSignature[] signatures;
    }

    struct PendingRecovery {
        PasskeySmartAccount.PasskeyIdentity newIdentity;
        uint256 recoveryNonce;
        uint256 readyAt;
    }

    uint256 public immutable defaultDelay;

    mapping(address => RecoveryPolicy) public recoveryPolicies;
    mapping(address => mapping(address => bool)) public guardians;
    mapping(address => address[]) internal guardianLists;
    mapping(address => PendingRecovery) public pendingRecoveries;

    event GuardianAdded(address indexed account, address indexed guardian);
    event GuardianRemoved(address indexed account, address indexed guardian);
    event GuardianPolicyUpdated(address indexed account, uint256 delay, uint256 threshold, uint256 guardianCount);
    event RecoveryStarted(
        address indexed account,
        bytes32 indexed credentialIdHash,
        uint256 recoveryNonce,
        uint256 readyAt
    );
    event RecoveryCancelled(address indexed account, uint256 recoveryNonce);
    event RecoveryFinalized(address indexed account, uint256 recoveryNonce);

    error InvalidDelay();
    error InvalidGuardian();
    error DuplicateGuardian();
    error InvalidRecoveryPolicy();
    error InvalidGuardianSignature();
    error InsufficientGuardianSignatures(uint256 provided, uint256 required);
    error NoPendingRecovery();
    error RecoveryExpired();
    error RecoveryNotReady(uint256 readyAt);
    error OnlyRecoveringAccount();
    error InvalidRecoveryValidator();
    error RecoveryAlreadyPending();

    constructor(uint256 recoveryDelay) {
        if (recoveryDelay == 0) {
            revert InvalidDelay();
        }
        defaultDelay = recoveryDelay;
    }

    function delay() external view returns (uint256) {
        return defaultDelay;
    }

    function addGuardian(PasskeySmartAccount account, address guardian, uint256 recoveryDelay, uint256 threshold)
        external
    {
        _onlyAccount(account);
        _addGuardian(account, guardian, recoveryDelay, threshold);
    }

    function updateRecoveryPolicy(PasskeySmartAccount account, uint256 recoveryDelay, uint256 threshold) external {
        _onlyAccount(account);
        RecoveryPolicy storage policy = recoveryPolicies[address(account)];
        _validatePolicy(recoveryDelay, threshold, policy.guardianCount);

        policy.delay = recoveryDelay;
        policy.threshold = threshold;

        emit GuardianPolicyUpdated(address(account), recoveryDelay, threshold, policy.guardianCount);
    }

    function removeGuardian(PasskeySmartAccount account, address guardian, uint256 threshold) external {
        _onlyAccount(account);
        _removeGuardian(account, guardian, threshold);
    }

    function clearGuardians(PasskeySmartAccount account) external {
        _onlyAccount(account);
        _clearGuardians(account);
    }

    function guardianCount(address account) external view returns (uint256) {
        return guardianLists[account].length;
    }

    function guardianAt(address account, uint256 index) external view returns (address) {
        return guardianLists[account][index];
    }

    function getRecoveryHash(StartRecoveryData calldata data) public view returns (bytes32) {
        return keccak256(
            abi.encode(
                RECOVERY_TYPEHASH,
                block.chainid,
                address(this),
                msg.sender,
                address(data.account),
                _passkeyIdentityHash(data.newIdentity),
                data.expectedRecoveryNonce,
                data.expiresAt
            )
        );
    }

    function startRecovery(StartRecoveryData calldata data) external {
        if (data.expiresAt < block.timestamp) {
            revert RecoveryExpired();
        }

        PasskeySmartAccount account = data.account;
        address accountAddress = address(account);
        if (account.recoveryValidator() != address(this)) {
            revert InvalidRecoveryValidator();
        }
        if (pendingRecoveries[accountAddress].readyAt != 0) {
            revert RecoveryAlreadyPending();
        }

        RecoveryPolicy memory policy = recoveryPolicies[accountAddress];
        if (policy.threshold == 0 || policy.guardianCount == 0) {
            revert InvalidRecoveryPolicy();
        }
        if (data.signatures.length < policy.threshold) {
            revert InsufficientGuardianSignatures(data.signatures.length, policy.threshold);
        }

        uint256 currentRecoveryNonce = account.recoveryNonce();
        if (data.expectedRecoveryNonce != currentRecoveryNonce) {
            revert PasskeySmartAccount.BadRecoveryNonce(currentRecoveryNonce, data.expectedRecoveryNonce);
        }

        _verifyGuardianSignatures(accountAddress, getRecoveryHash(data), data.signatures, policy.threshold);
        _startRecovery(account, data.newIdentity, data.expectedRecoveryNonce);
    }

    function cancelRecovery(PasskeySmartAccount account) external {
        _onlyAccount(account);
        PendingRecovery memory pending = pendingRecoveries[address(account)];
        if (pending.readyAt == 0) {
            revert NoPendingRecovery();
        }

        delete pendingRecoveries[address(account)];
        emit RecoveryCancelled(address(account), pending.recoveryNonce);
    }

    function finalizeRecovery(PasskeySmartAccount account) external {
        PendingRecovery memory pending = pendingRecoveries[address(account)];
        if (pending.readyAt == 0) {
            revert NoPendingRecovery();
        }
        if (block.timestamp < pending.readyAt) {
            revert RecoveryNotReady(pending.readyAt);
        }

        delete pendingRecoveries[address(account)];
        account.recoverPasskey(pending.newIdentity, pending.recoveryNonce);

        emit RecoveryFinalized(address(account), pending.recoveryNonce);
    }

    function _onlyAccount(PasskeySmartAccount account) internal view {
        if (msg.sender != address(account)) {
            revert OnlyRecoveringAccount();
        }
    }

    function _addGuardian(
        PasskeySmartAccount account,
        address guardian,
        uint256 recoveryDelay,
        uint256 threshold
    ) internal {
        if (guardian == address(0) || guardian == address(account)) {
            revert InvalidGuardian();
        }

        address accountAddress = address(account);
        if (guardians[accountAddress][guardian]) {
            revert DuplicateGuardian();
        }

        uint256 nextCount = guardianLists[accountAddress].length + 1;
        _validatePolicy(recoveryDelay, threshold, nextCount);

        guardians[accountAddress][guardian] = true;
        guardianLists[accountAddress].push(guardian);
        recoveryPolicies[accountAddress] =
            RecoveryPolicy({delay: recoveryDelay, threshold: threshold, guardianCount: nextCount});

        emit GuardianAdded(accountAddress, guardian);
        emit GuardianPolicyUpdated(accountAddress, recoveryDelay, threshold, nextCount);
    }

    function _removeGuardian(PasskeySmartAccount account, address guardian, uint256 threshold) internal {
        address accountAddress = address(account);
        if (!guardians[accountAddress][guardian]) {
            revert InvalidGuardian();
        }

        delete guardians[accountAddress][guardian];
        address[] storage guardianList = guardianLists[accountAddress];
        for (uint256 i = 0; i < guardianList.length; ++i) {
            if (guardianList[i] == guardian) {
                guardianList[i] = guardianList[guardianList.length - 1];
                guardianList.pop();
                break;
            }
        }

        uint256 nextCount = guardianList.length;
        if (nextCount == 0) {
            delete recoveryPolicies[accountAddress];
            _cancelPendingRecovery(accountAddress);
        } else {
            RecoveryPolicy storage policy = recoveryPolicies[accountAddress];
            _validatePolicy(policy.delay, threshold, nextCount);
            policy.threshold = threshold;
            policy.guardianCount = nextCount;
            emit GuardianPolicyUpdated(accountAddress, policy.delay, threshold, nextCount);
        }

        emit GuardianRemoved(accountAddress, guardian);
    }

    function _clearGuardians(PasskeySmartAccount account) internal {
        address accountAddress = address(account);
        address[] storage guardianList = guardianLists[accountAddress];
        for (uint256 i = 0; i < guardianList.length; ++i) {
            delete guardians[accountAddress][guardianList[i]];
        }

        delete guardianLists[accountAddress];
        delete recoveryPolicies[accountAddress];
        _cancelPendingRecovery(accountAddress);
    }

    function _cancelPendingRecovery(address accountAddress) internal {
        PendingRecovery memory pending = pendingRecoveries[accountAddress];
        if (pending.readyAt != 0) {
            delete pendingRecoveries[accountAddress];
            emit RecoveryCancelled(accountAddress, pending.recoveryNonce);
        }
    }

    function _validatePolicy(uint256 recoveryDelay, uint256 threshold, uint256 guardianCount_) internal pure {
        if (recoveryDelay == 0) {
            revert InvalidDelay();
        }
        if (threshold == 0 || threshold > guardianCount_) {
            revert InvalidRecoveryPolicy();
        }
    }

    function _verifyGuardianSignatures(
        address accountAddress,
        bytes32 recoveryHash,
        GuardianSignature[] calldata signatures,
        uint256 threshold
    ) internal view {
        uint256 verified;
        bytes32 digest = keccak256(abi.encodePacked("\x19Ethereum Signed Message:\n32", recoveryHash));
        for (uint256 i = 0; i < signatures.length; ++i) {
            GuardianSignature calldata guardianSignature = signatures[i];
            if (!guardians[accountAddress][guardianSignature.guardian]) {
                revert InvalidGuardianSignature();
            }
            for (uint256 j = 0; j < i; ++j) {
                if (signatures[j].guardian == guardianSignature.guardian) {
                    revert DuplicateGuardian();
                }
            }
            if ((guardianSignature.v != 27 && guardianSignature.v != 28) || uint256(guardianSignature.s) > SECP256K1_HALF_ORDER) {
                revert InvalidGuardianSignature();
            }

            address recovered = ecrecover(digest, guardianSignature.v, guardianSignature.r, guardianSignature.s);
            if (recovered == address(0) || recovered != guardianSignature.guardian) {
                revert InvalidGuardianSignature();
            }

            ++verified;
            if (verified == threshold) {
                return;
            }
        }

        revert InsufficientGuardianSignatures(verified, threshold);
    }

    function _startRecovery(
        PasskeySmartAccount account,
        PasskeySmartAccount.PasskeyIdentity calldata newIdentity,
        uint256 expectedRecoveryNonce
    ) internal {
        uint256 readyAt = block.timestamp + recoveryPolicies[address(account)].delay;
        pendingRecoveries[address(account)] = PendingRecovery({
            newIdentity: newIdentity,
            recoveryNonce: expectedRecoveryNonce,
            readyAt: readyAt
        });

        emit RecoveryStarted(address(account), newIdentity.credentialIdHash, expectedRecoveryNonce, readyAt);
    }

    function _passkeyIdentityHash(PasskeySmartAccount.PasskeyIdentity calldata identity)
        internal
        pure
        returns (bytes32)
    {
        return keccak256(
            abi.encode(
                identity.passkeyX,
                identity.passkeyY,
                identity.credentialIdHash,
                identity.rpIdHash,
                identity.originHash,
                identity.originLength
            )
        );
    }
}
