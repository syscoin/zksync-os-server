// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {AccessControlUpgradeable} from "@openzeppelin/contracts-upgradeable-v4/access/AccessControlUpgradeable.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable-v4/proxy/utils/Initializable.sol";
import {IZkSysSentryNodeReceiver, ZkSysMembershipRegistry} from "./ZkSysMembershipRegistry.sol";

interface IZkSysWeightReceiver {
    function onWeightChange(address account, uint256 oldWeight, uint256 newWeight, uint256 oldTotalWeight) external;
    function currentPeriod() external view returns (uint256);
    function startTime() external view returns (uint256);
}

interface IZkSysServiceWeightReceiver {
    function onWeightComponentsChange(
        address account,
        uint256 oldWeight,
        uint256 newWeight,
        uint256 oldPassiveWeight,
        uint256 newPassiveWeight,
        uint256 oldTotalWeight,
        uint256 oldTotalPassiveWeight
    ) external;
}

/// @title ZkSysRewardWeightRegistry
/// @notice Converts native SYS stake and membership facts into issuer reward weights.
contract ZkSysRewardWeightRegistry is Initializable, AccessControlUpgradeable, IZkSysSentryNodeReceiver {
    bytes32 public constant STAKE_WEIGHT_UPDATER_ROLE = keccak256("STAKE_WEIGHT_UPDATER_ROLE");
    uint256 public constant SENTRY_NODE_BASE_WEIGHT = 100_000 ether;

    struct Weight {
        uint256 stakeWeight;
        uint256 sentryNodeWeight;
    }

    struct PendingWeight {
        uint256 stakeWeight;
        uint256 stakeEffectivePeriod;
        uint256 sentryNodeWeight;
        uint256 sentryNodeEffectivePeriod;
    }

    struct PendingWeightView {
        uint256 stakeWeight;
        uint256 stakeEffectivePeriod;
        uint256 sentryNodeWeight;
        uint256 sentryNodeEffectivePeriod;
    }

    error InvalidAddress();
    error InvalidActivationDelay(uint256 activationDelayPeriods);
    error InvalidWeight(uint256 weight);
    error NoPendingWeight();
    error PendingWeightNotEffective(uint256 effectivePeriod, uint256 currentPeriod);
    error UnauthorizedMembershipRegistry();
    error WeightReceiverAlreadySet(address currentWeightReceiver);
    error WeightReceiverNotSet();
    error UnauthorizedWeightReceiver();
    error ServiceAccountingAlreadyConfigured();
    error NonemptyServiceAccountingMigration();

    ZkSysMembershipRegistry public membershipRegistry;
    IZkSysWeightReceiver public weightReceiver;
    // Preserve the legacy potential-weight getter and storage layout. Service issuance instead
    // combines totalPassiveWeight with the separate, verified period-admission aggregate.
    uint256 public totalWeight;

    mapping(address account => Weight weight) private _weights;
    uint256 public activationDelayPeriods;
    mapping(address account => PendingWeight pendingWeight) private _pendingWeights;
    uint256 public serviceStartPeriodPlusOne;
    uint256 public totalPassiveWeight;
    uint256[42] private __gap;

    event StakeWeightUpdated(address indexed account, uint256 oldStakeWeight, uint256 newStakeWeight);
    event StakeWeightQueued(
        address indexed account, uint256 activeStakeWeight, uint256 pendingStakeWeight, uint256 effectivePeriod
    );
    event SentryNodeWeightUpdated(address indexed account, uint256 oldSentryNodeWeight, uint256 newSentryNodeWeight);
    event SentryNodeWeightQueued(
        address indexed account,
        uint256 activeSentryNodeWeight,
        uint256 pendingSentryNodeWeight,
        uint256 effectivePeriod
    );
    event WeightReceiverUpdated(address indexed weightReceiver);
    event WeightUpdated(address indexed account, uint256 oldWeight, uint256 newWeight);
    event PendingWeightActivated(address indexed account, uint256 oldWeight, uint256 newWeight);
    event ServiceAccountingEnabled(uint256 activationPeriod);

    constructor() {
        _disableInitializers();
    }

    function initialize(address admin, ZkSysMembershipRegistry membershipRegistry_, uint256 activationDelayPeriods_)
        external
        initializer
    {
        if (admin == address(0) || address(membershipRegistry_) == address(0)) {
            revert InvalidAddress();
        }
        if (activationDelayPeriods_ == 0 || activationDelayPeriods_ > 7) {
            revert InvalidActivationDelay(activationDelayPeriods_);
        }

        __AccessControl_init();
        membershipRegistry = membershipRegistry_;
        activationDelayPeriods = activationDelayPeriods_;
        _grantRole(DEFAULT_ADMIN_ROLE, admin);
        _setRoleAdmin(STAKE_WEIGHT_UPDATER_ROLE, DEFAULT_ADMIN_ROLE);
    }

    function setWeightReceiver(IZkSysWeightReceiver weightReceiver_) external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (address(weightReceiver_) == address(0)) {
            revert InvalidAddress();
        }
        IZkSysWeightReceiver currentWeightReceiver = weightReceiver;
        if (address(currentWeightReceiver) != address(0) && currentWeightReceiver != weightReceiver_) {
            revert WeightReceiverAlreadySet(address(currentWeightReceiver));
        }
        weightReceiver = weightReceiver_;
        emit WeightReceiverUpdated(address(weightReceiver_));
    }

    function enableServiceAccounting(uint256 activationPeriod) external {
        if (msg.sender != address(weightReceiver)) {
            revert UnauthorizedWeightReceiver();
        }
        if (serviceStartPeriodPlusOne != 0) {
            revert ServiceAccountingAlreadyConfigured();
        }
        // Existing nonempty deployments need an explicit aggregate migration; an empty launch
        // can track passive weight exactly without trusting an administrator-supplied total.
        if (totalWeight != 0) {
            revert NonemptyServiceAccountingMigration();
        }
        serviceStartPeriodPlusOne = activationPeriod + 1;
        emit ServiceAccountingEnabled(activationPeriod);
    }

    function updateStakeWeight(address account, uint256 stakeWeight) external onlyRole(STAKE_WEIGHT_UPDATER_ROLE) {
        _updateStakeWeight(account, stakeWeight);
    }

    function updateStakeWeights(address[] calldata accounts, uint256[] calldata stakeWeights)
        external
        onlyRole(STAKE_WEIGHT_UPDATER_ROLE)
    {
        if (accounts.length != stakeWeights.length) {
            revert InvalidWeight(stakeWeights.length);
        }
        for (uint256 i = 0; i < accounts.length; ++i) {
            _updateStakeWeight(accounts[i], stakeWeights[i]);
        }
    }

    function onSentryNodeStatusChange(address account, uint32, uint32, uint128, uint128 newSentryNodeWeight) external {
        if (msg.sender != address(membershipRegistry)) {
            revert UnauthorizedMembershipRegistry();
        }
        _updateSentryNodeWeight(account, newSentryNodeWeight);
    }

    function activatePendingWeight() external {
        _activatePendingWeight(msg.sender);
    }

    function activatePendingWeightFor(address account) external {
        _activatePendingWeight(account);
    }

    /// @notice Legacy potential weight; use the issuer's currentRewardWeight for service issuance.
    function weightOf(address account) external view returns (uint256) {
        return _totalAccountWeight(_weights[account]);
    }

    function weightComponents(address account) external view returns (Weight memory) {
        return _weights[account];
    }

    function rewardWeightComponents(address account)
        external
        view
        returns (uint256 passiveWeight, uint256 seniorBonus)
    {
        Weight memory weight = _weights[account];
        passiveWeight = _passiveWeight(weight.stakeWeight, weight.sentryNodeWeight);
        seniorBonus = _totalAccountWeight(weight) - passiveWeight;
    }

    function pendingWeightComponents(address account) external view returns (PendingWeightView memory pendingWeight) {
        PendingWeight storage stored = _pendingWeights[account];
        if (stored.stakeEffectivePeriod != 0) {
            pendingWeight.stakeWeight = stored.stakeWeight;
            pendingWeight.stakeEffectivePeriod = stored.stakeEffectivePeriod - 1;
        }
        if (stored.sentryNodeEffectivePeriod != 0) {
            pendingWeight.sentryNodeWeight = stored.sentryNodeWeight;
            pendingWeight.sentryNodeEffectivePeriod = stored.sentryNodeEffectivePeriod - 1;
        }
    }

    function _updateStakeWeight(address account, uint256 stakeWeight) private {
        if (account == address(0)) {
            revert InvalidAddress();
        }
        Weight storage stored = _weights[account];
        PendingWeight storage pending = _pendingWeights[account];
        uint256 oldStakeWeight = stored.stakeWeight;

        if (stakeWeight <= oldStakeWeight) {
            pending.stakeEffectivePeriod = 0;
            _applyWeightChange(account, stakeWeight, stored.sentryNodeWeight);

            emit StakeWeightUpdated(account, oldStakeWeight, stakeWeight);
            return;
        }

        uint256 currentPendingStakeEffectivePeriod = pending.stakeEffectivePeriod;
        if (currentPendingStakeEffectivePeriod != 0 && stakeWeight <= pending.stakeWeight) {
            uint256 sentryNodeWeight =
                pending.sentryNodeEffectivePeriod == 0 ? stored.sentryNodeWeight : pending.sentryNodeWeight;
            _checkWeight(stakeWeight + sentryNodeWeight);
            pending.stakeWeight = stakeWeight;
            emit StakeWeightQueued(account, oldStakeWeight, stakeWeight, currentPendingStakeEffectivePeriod - 1);
            return;
        }

        {
            uint256 sentryNodeWeight =
                pending.sentryNodeEffectivePeriod == 0 ? stored.sentryNodeWeight : pending.sentryNodeWeight;
            _checkWeight(stakeWeight + sentryNodeWeight);
            uint256 effectivePeriod = _pendingEffectivePeriod();
            pending.stakeEffectivePeriod = effectivePeriod + 1;
            pending.stakeWeight = stakeWeight;
            emit StakeWeightQueued(account, oldStakeWeight, stakeWeight, effectivePeriod);
        }
    }

    function _updateSentryNodeWeight(address account, uint256 sentryNodeWeight) private {
        Weight storage stored = _weights[account];
        PendingWeight storage pending = _pendingWeights[account];
        uint256 oldSentryNodeWeight = stored.sentryNodeWeight;

        if (sentryNodeWeight <= oldSentryNodeWeight) {
            pending.sentryNodeEffectivePeriod = 0;
            _applyWeightChange(account, stored.stakeWeight, sentryNodeWeight);

            emit SentryNodeWeightUpdated(account, oldSentryNodeWeight, sentryNodeWeight);
            return;
        }

        uint256 currentPendingSentryNodeEffectivePeriod = pending.sentryNodeEffectivePeriod;
        if (currentPendingSentryNodeEffectivePeriod != 0 && sentryNodeWeight <= pending.sentryNodeWeight) {
            uint256 stakeWeight = pending.stakeEffectivePeriod == 0 ? stored.stakeWeight : pending.stakeWeight;
            _checkWeight(stakeWeight + sentryNodeWeight);
            pending.sentryNodeWeight = sentryNodeWeight;
            emit SentryNodeWeightQueued(
                account, oldSentryNodeWeight, sentryNodeWeight, currentPendingSentryNodeEffectivePeriod - 1
            );
            return;
        }

        {
            uint256 stakeWeight = pending.stakeEffectivePeriod == 0 ? stored.stakeWeight : pending.stakeWeight;
            _checkWeight(stakeWeight + sentryNodeWeight);
            uint256 effectivePeriod = _pendingEffectivePeriod();
            pending.sentryNodeEffectivePeriod = effectivePeriod + 1;
            pending.sentryNodeWeight = sentryNodeWeight;
            emit SentryNodeWeightQueued(account, oldSentryNodeWeight, sentryNodeWeight, effectivePeriod);
        }
    }

    function _activatePendingWeight(address account) private {
        Weight storage stored = _weights[account];
        PendingWeight storage pending = _pendingWeights[account];
        uint256 currentPeriod = _currentPeriod();
        uint256 newStakeWeight = stored.stakeWeight;
        uint256 newSentryNodeWeight = stored.sentryNodeWeight;
        bool hasEffectivePendingWeight;
        uint256 nextEffectivePeriod = type(uint256).max;

        uint256 stakeEffectivePeriod = pending.stakeEffectivePeriod;
        if (stakeEffectivePeriod != 0) {
            --stakeEffectivePeriod;
            if (currentPeriod >= stakeEffectivePeriod) {
                newStakeWeight = pending.stakeWeight;
                pending.stakeEffectivePeriod = 0;
                hasEffectivePendingWeight = true;
            } else {
                nextEffectivePeriod = stakeEffectivePeriod;
            }
        }

        uint256 sentryNodeEffectivePeriod = pending.sentryNodeEffectivePeriod;
        if (sentryNodeEffectivePeriod != 0) {
            --sentryNodeEffectivePeriod;
            if (currentPeriod >= sentryNodeEffectivePeriod) {
                newSentryNodeWeight = pending.sentryNodeWeight;
                pending.sentryNodeEffectivePeriod = 0;
                hasEffectivePendingWeight = true;
            } else if (sentryNodeEffectivePeriod < nextEffectivePeriod) {
                nextEffectivePeriod = sentryNodeEffectivePeriod;
            }
        }

        if (!hasEffectivePendingWeight) {
            if (nextEffectivePeriod != type(uint256).max) {
                revert PendingWeightNotEffective(nextEffectivePeriod, currentPeriod);
            }
            revert NoPendingWeight();
        }

        (uint256 oldWeight, uint256 newWeight) = _applyWeightChange(account, newStakeWeight, newSentryNodeWeight);
        emit PendingWeightActivated(account, oldWeight, newWeight);
    }

    function _applyWeightChange(address account, uint256 newStakeWeight, uint256 newSentryNodeWeight)
        private
        returns (uint256 oldWeight, uint256 newWeight)
    {
        Weight storage stored = _weights[account];
        oldWeight = _totalAccountWeight(stored);
        uint256 oldTotalWeight = totalWeight;
        uint256 oldPassiveWeight = _passiveWeight(stored.stakeWeight, stored.sentryNodeWeight);
        newWeight = newStakeWeight + newSentryNodeWeight;
        _checkWeight(newWeight);

        stored.stakeWeight = newStakeWeight;
        stored.sentryNodeWeight = newSentryNodeWeight;
        totalWeight = oldTotalWeight - oldWeight + newWeight;
        if (serviceStartPeriodPlusOne == 0) {
            _checkpointWeightChange(account, oldWeight, newWeight, oldTotalWeight);
        } else {
            uint256 oldTotalPassiveWeight = totalPassiveWeight;
            uint256 newPassiveWeight = _passiveWeight(newStakeWeight, newSentryNodeWeight);
            totalPassiveWeight = oldTotalPassiveWeight - oldPassiveWeight + newPassiveWeight;
            if (oldWeight != newWeight || oldPassiveWeight != newPassiveWeight) {
                IZkSysServiceWeightReceiver(address(weightReceiver))
                    .onWeightComponentsChange(
                        account,
                        oldWeight,
                        newWeight,
                        oldPassiveWeight,
                        newPassiveWeight,
                        oldTotalWeight,
                        oldTotalPassiveWeight
                    );
            }
        }

        emit WeightUpdated(account, oldWeight, newWeight);
    }

    function _checkpointWeightChange(address account, uint256 oldWeight, uint256 newWeight, uint256 oldTotalWeight)
        private
    {
        IZkSysWeightReceiver receiver = weightReceiver;
        if (oldWeight != newWeight) {
            if (address(receiver) == address(0)) {
                revert WeightReceiverNotSet();
            }
            receiver.onWeightChange(account, oldWeight, newWeight, oldTotalWeight);
        }
    }

    function _pendingEffectivePeriod() private view returns (uint256) {
        IZkSysWeightReceiver receiver = weightReceiver;
        if (address(receiver) == address(0)) {
            revert WeightReceiverNotSet();
        }
        if (block.timestamp < receiver.startTime()) {
            return 0;
        }
        return receiver.currentPeriod() + activationDelayPeriods;
    }

    function _currentPeriod() private view returns (uint256) {
        IZkSysWeightReceiver receiver = weightReceiver;
        if (address(receiver) == address(0)) {
            revert WeightReceiverNotSet();
        }
        return receiver.currentPeriod();
    }

    function _checkWeight(uint256 weight) private pure {
        if (weight > type(uint128).max) {
            revert InvalidWeight(weight);
        }
    }

    function _totalAccountWeight(Weight memory weight) private pure returns (uint256) {
        return weight.stakeWeight + weight.sentryNodeWeight;
    }

    function _passiveWeight(uint256 stakeWeight, uint256 sentryNodeWeight) private pure returns (uint256) {
        uint256 baseWeight = sentryNodeWeight < SENTRY_NODE_BASE_WEIGHT ? sentryNodeWeight : SENTRY_NODE_BASE_WEIGHT;
        return stakeWeight + baseWeight;
    }
}
