// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {AccessControl} from "@openzeppelin/contracts/access/AccessControl.sol";
import {IZkSysSentryNodeReceiver, ZkSysMembershipRegistry} from "./ZkSysMembershipRegistry.sol";

interface IZkSysWeightReceiver {
    function onWeightChange(address account, uint256 oldWeight, uint256 newWeight) external;
}

/// @title ZkSysRewardWeightRegistry
/// @notice Converts membership facts and admin-managed stake weights into issuer reward weights.
contract ZkSysRewardWeightRegistry is AccessControl, IZkSysSentryNodeReceiver {
    uint256 public constant SENTRY_NODE_WEIGHT = 100_000 ether;

    struct Weight {
        uint256 stakeWeight;
        uint256 sentryNodeWeight;
    }

    error InvalidAddress();
    error InvalidWeight(uint256 weight);
    error UnauthorizedMembershipRegistry();
    error WeightReceiverAlreadySet(address currentWeightReceiver);
    error WeightReceiverNotSet();

    ZkSysMembershipRegistry public immutable membershipRegistry;
    IZkSysWeightReceiver public weightReceiver;
    uint256 public totalWeight;

    mapping(address account => Weight weight) private _weights;

    event StakeWeightUpdated(address indexed account, uint256 oldStakeWeight, uint256 newStakeWeight);
    event SentryNodeWeightUpdated(address indexed account, uint256 oldSentryNodeWeight, uint256 newSentryNodeWeight);
    event WeightReceiverUpdated(address indexed weightReceiver);
    event WeightUpdated(address indexed account, uint256 oldWeight, uint256 newWeight);

    constructor(address admin, ZkSysMembershipRegistry membershipRegistry_) {
        if (admin == address(0) || address(membershipRegistry_) == address(0)) {
            revert InvalidAddress();
        }

        membershipRegistry = membershipRegistry_;
        _grantRole(DEFAULT_ADMIN_ROLE, admin);
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

    function adminUpdateStakeWeight(address account, uint256 stakeWeight) external onlyRole(DEFAULT_ADMIN_ROLE) {
        _updateStakeWeight(account, stakeWeight);
    }

    function adminUpdateStakeWeights(address[] calldata accounts, uint256[] calldata stakeWeights)
        external
        onlyRole(DEFAULT_ADMIN_ROLE)
    {
        if (accounts.length != stakeWeights.length) {
            revert InvalidWeight(stakeWeights.length);
        }
        for (uint256 i = 0; i < accounts.length; ++i) {
            _updateStakeWeight(accounts[i], stakeWeights[i]);
        }
    }

    function onSentryNodeStatusChange(address account, uint32, uint32 newCollateralHeight) external {
        if (msg.sender != address(membershipRegistry)) {
            revert UnauthorizedMembershipRegistry();
        }
        _updateSentryNodeWeight(account, newCollateralHeight == 0 ? 0 : SENTRY_NODE_WEIGHT);
    }

    function weightOf(address account) external view returns (uint256) {
        return _totalAccountWeight(_weights[account]);
    }

    function weightComponents(address account) external view returns (Weight memory) {
        return _weights[account];
    }

    function _updateStakeWeight(address account, uint256 stakeWeight) private {
        if (account == address(0)) {
            revert InvalidAddress();
        }
        Weight storage stored = _weights[account];
        uint256 oldWeight = _totalAccountWeight(stored);
        uint256 oldStakeWeight = stored.stakeWeight;
        uint256 newWeight = stakeWeight + stored.sentryNodeWeight;
        _checkWeight(newWeight);
        _checkpointWeightChange(account, oldWeight, newWeight);

        stored.stakeWeight = stakeWeight;
        totalWeight = totalWeight - oldWeight + newWeight;

        emit StakeWeightUpdated(account, oldStakeWeight, stakeWeight);
        emit WeightUpdated(account, oldWeight, newWeight);
    }

    function _updateSentryNodeWeight(address account, uint256 sentryNodeWeight) private {
        Weight storage stored = _weights[account];
        uint256 oldWeight = _totalAccountWeight(stored);
        uint256 oldSentryNodeWeight = stored.sentryNodeWeight;
        uint256 newWeight = stored.stakeWeight + sentryNodeWeight;
        _checkWeight(newWeight);
        _checkpointWeightChange(account, oldWeight, newWeight);

        stored.sentryNodeWeight = sentryNodeWeight;
        totalWeight = totalWeight - oldWeight + newWeight;

        emit SentryNodeWeightUpdated(account, oldSentryNodeWeight, sentryNodeWeight);
        emit WeightUpdated(account, oldWeight, newWeight);
    }

    function _checkpointWeightChange(address account, uint256 oldWeight, uint256 newWeight) private {
        IZkSysWeightReceiver receiver = weightReceiver;
        if (oldWeight != newWeight) {
            if (address(receiver) == address(0)) {
                revert WeightReceiverNotSet();
            }
            receiver.onWeightChange(account, oldWeight, newWeight);
        }
    }

    function _checkWeight(uint256 weight) private pure {
        if (weight > type(uint128).max) {
            revert InvalidWeight(weight);
        }
    }

    function _totalAccountWeight(Weight memory weight) private pure returns (uint256) {
        return weight.stakeWeight + weight.sentryNodeWeight;
    }
}
