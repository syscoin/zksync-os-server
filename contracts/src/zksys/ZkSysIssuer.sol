// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {AccessControlUpgradeable} from "@openzeppelin/contracts-upgradeable-v4/access/AccessControlUpgradeable.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable-v4/proxy/utils/Initializable.sol";
import {Math} from "@openzeppelin/contracts/utils/math/Math.sol";
import {IZkSysWeightReceiver} from "./ZkSysRewardWeightRegistry.sol";

interface IZkSysMintableToken {
    function maxSupply() external view returns (uint256);
    function mint(address to, uint256 amount) external returns (bool);
    function totalSupply() external view returns (uint256);
}

interface IZkSysRewardWeightSource {
    function totalWeight() external view returns (uint256);
    function weightOf(address account) external view returns (uint256);
}

/// @title ZkSysIssuer
/// @notice Indexed zkSYS reward distributor for L2-canonical issuance.
contract ZkSysIssuer is Initializable, AccessControlUpgradeable, IZkSysWeightReceiver {
    uint256 public constant REWARD_PRECISION = 1e36;
    uint256 public constant BPS_DENOMINATOR = 10_000;
    uint256 public constant SCHEDULE_YEAR_SECONDS = 365 days;
    uint256 public constant YEAR_1_RATE_BPS = 2_000;
    uint256 public constant YEAR_2_RATE_BPS = 1_200;
    uint256 public constant YEAR_3_RATE_BPS = 800;
    uint256 public constant LONG_RUN_RATE_BPS = 500;

    error InvalidAddress();
    error InvalidSchedule();
    error NoWeight();
    error NoRewardsAvailable();
    error SupplyCapExceeded(uint256 scheduledSupply, uint256 maxSupply);
    error UnauthorizedRegistry();

    IZkSysMintableToken public token;
    IZkSysRewardWeightSource public registry;
    uint256 public startTime;
    uint256 public periodSeconds;
    uint256 public periodsPerYear;

    uint256 public accRewardPerWeight;
    uint256 public scheduledUnclaimedRewards;
    uint256 public totalScheduledRewards;
    uint256 public lastDistributedPeriod;

    mapping(address account => uint256 rewardDebt) public rewardDebtOf;
    mapping(address account => uint256 accruedRewards) public accruedRewardsOf;
    uint256[44] private __gap;

    event RewardsDistributed(uint256 amount, uint256 indexed distributedThroughPeriod, uint256 accRewardPerWeight);
    event RewardsSkipped(uint256 amount, uint256 indexed distributedThroughPeriod);
    event RewardsClaimed(address indexed account, address indexed receiver, uint256 amount);
    event WeightChanged(address indexed account, uint256 oldWeight, uint256 newWeight);

    constructor() {
        _disableInitializers();
    }

    function initialize(
        IZkSysMintableToken token_,
        IZkSysRewardWeightSource registry_,
        address admin,
        uint256 startTime_,
        uint256 periodSeconds_,
        uint256 periodsPerYear_
    ) external initializer {
        if (address(token_) == address(0) || address(registry_) == address(0) || admin == address(0)) {
            revert InvalidAddress();
        }
        if (periodSeconds_ == 0 || periodsPerYear_ == 0) {
            revert InvalidSchedule();
        }
        if (periodSeconds_ > type(uint256).max / periodsPerYear_) {
            revert InvalidSchedule();
        }
        if (periodSeconds_ * periodsPerYear_ != SCHEDULE_YEAR_SECONDS) {
            revert InvalidSchedule();
        }
        if (token_.maxSupply() > type(uint256).max / REWARD_PRECISION) {
            revert InvalidSchedule();
        }

        __AccessControl_init();
        token = token_;
        registry = registry_;
        startTime = startTime_;
        periodSeconds = periodSeconds_;
        periodsPerYear = periodsPerYear_;

        _grantRole(DEFAULT_ADMIN_ROLE, admin);
    }

    function distribute() external returns (uint256 amount) {
        uint256 totalWeight = registry.totalWeight();
        if (totalWeight == 0) {
            revert NoWeight();
        }

        amount = _checkpointRewards(totalWeight);
        if (amount == 0) {
            revert NoRewardsAvailable();
        }
    }

    function _checkpointRewards(uint256 totalWeight) private returns (uint256 amount) {
        uint256 distributedThroughPeriod = currentPeriod();
        uint256 scheduledRewards = cumulativeScheduledRewards(distributedThroughPeriod);
        amount = scheduledRewards - totalScheduledRewards;
        if (amount == 0) {
            return 0;
        }

        uint256 maxSupply = token.maxSupply();
        if (scheduledRewards > maxSupply) {
            revert SupplyCapExceeded(scheduledRewards, maxSupply);
        }

        accRewardPerWeight += amount * REWARD_PRECISION / totalWeight;
        scheduledUnclaimedRewards += amount;
        totalScheduledRewards = scheduledRewards;
        lastDistributedPeriod = distributedThroughPeriod;

        emit RewardsDistributed(amount, distributedThroughPeriod, accRewardPerWeight);
    }

    function _checkpointBeforeFirstWeight() private {
        uint256 distributedThroughPeriod = currentPeriod();
        uint256 scheduledRewards = cumulativeScheduledRewards(distributedThroughPeriod);
        uint256 amount = scheduledRewards - totalScheduledRewards;
        if (amount == 0) {
            return;
        }

        uint256 maxSupply = token.maxSupply();
        if (scheduledRewards > maxSupply) {
            revert SupplyCapExceeded(scheduledRewards, maxSupply);
        }

        totalScheduledRewards = scheduledRewards;
        lastDistributedPeriod = distributedThroughPeriod;

        emit RewardsSkipped(amount, distributedThroughPeriod);
    }

    function currentPeriod() public view returns (uint256) {
        if (block.timestamp < startTime) {
            return 0;
        }
        return (block.timestamp - startTime) / periodSeconds;
    }

    function cumulativeScheduledRewards(uint256 periodsElapsed) public view returns (uint256 scheduledRewards) {
        uint256 remainingPeriods = periodsElapsed;
        uint256 yearIndex;
        uint256 maxSupply = token.maxSupply();

        while (remainingPeriods != 0 && scheduledRewards < maxSupply) {
            uint256 periodsInYear = remainingPeriods;
            if (periodsInYear > periodsPerYear) {
                periodsInYear = periodsPerYear;
            }

            uint256 remainingSupply = maxSupply - scheduledRewards;
            uint256 annualEmission = Math.mulDiv(remainingSupply, annualRateBps(yearIndex), BPS_DENOMINATOR);
            if (annualEmission == 0) {
                return scheduledRewards;
            }
            scheduledRewards += Math.mulDiv(annualEmission, periodsInYear, periodsPerYear);

            remainingPeriods -= periodsInYear;
            ++yearIndex;
        }
    }

    function annualRateBps(uint256 yearIndex) public pure returns (uint256) {
        if (yearIndex == 0) {
            return YEAR_1_RATE_BPS;
        }
        if (yearIndex == 1) {
            return YEAR_2_RATE_BPS;
        }
        if (yearIndex == 2) {
            return YEAR_3_RATE_BPS;
        }
        return LONG_RUN_RATE_BPS;
    }

    function claim(address receiver) external returns (uint256 claimed) {
        if (receiver == address(0)) {
            revert InvalidAddress();
        }

        _settle(msg.sender, registry.weightOf(msg.sender));
        claimed = accruedRewardsOf[msg.sender];
        if (claimed == 0) {
            return 0;
        }

        accruedRewardsOf[msg.sender] = 0;
        scheduledUnclaimedRewards -= claimed;
        require(token.mint(receiver, claimed), "issuer: mint failed");

        emit RewardsClaimed(msg.sender, receiver, claimed);
    }

    function pendingRewards(address account) external view returns (uint256) {
        uint256 weight = registry.weightOf(account);
        uint256 accumulated = _rewardDebt(weight);
        return accruedRewardsOf[account] + accumulated - rewardDebtOf[account];
    }

    function onWeightChange(address account, uint256 oldWeight, uint256 newWeight, uint256 oldTotalWeight) external {
        if (msg.sender != address(registry)) {
            revert UnauthorizedRegistry();
        }

        if (oldTotalWeight == 0) {
            _checkpointBeforeFirstWeight();
        } else {
            _checkpointRewards(oldTotalWeight);
        }
        _settle(account, oldWeight);
        rewardDebtOf[account] = _rewardDebt(newWeight);

        emit WeightChanged(account, oldWeight, newWeight);
    }

    function _settle(address account, uint256 weight) private {
        uint256 accumulated = _rewardDebt(weight);
        uint256 rewardDebt = rewardDebtOf[account];
        if (accumulated > rewardDebt) {
            accruedRewardsOf[account] += accumulated - rewardDebt;
        }
        rewardDebtOf[account] = accumulated;
    }

    function _rewardDebt(uint256 weight) private view returns (uint256) {
        return Math.mulDiv(weight, accRewardPerWeight, REWARD_PRECISION);
    }
}
