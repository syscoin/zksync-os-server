// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {AccessControlUpgradeable} from "@openzeppelin/contracts-upgradeable-v4/access/AccessControlUpgradeable.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable-v4/proxy/utils/Initializable.sol";
import {Math} from "@openzeppelin/contracts/utils/math/Math.sol";
import {IZkSysWeightReceiver, IZkSysServiceWeightReceiver} from "./ZkSysRewardWeightRegistry.sol";

interface IZkSysMintableToken {
    function maxSupply() external view returns (uint256);
    function mint(address to, uint256 amount) external returns (bool);
    function totalSupply() external view returns (uint256);
}

interface IZkSysRewardWeightSource {
    function totalWeight() external view returns (uint256);
    function weightOf(address account) external view returns (uint256);
}

interface IZkSysServiceRewardWeightSource {
    function enableServiceAccounting(uint256 activationPeriod) external;
    function totalPassiveWeight() external view returns (uint256);
    function rewardWeightComponents(address account) external view returns (uint256 passiveWeight, uint256 seniorBonus);
}

interface IZkSysProverServiceSource {
    function issuer() external view returns (address);
    function startTime() external view returns (uint256);
    function periodSeconds() external view returns (uint256);
    function firstServicePeriod() external view returns (uint256);
    function serviceActive() external view returns (bool);
    function admittedBonusWeight(address account, uint256 period) external view returns (uint256);
    function totalAdmittedBonusWeight(uint256 period) external view returns (uint256);
    function serviceFactorBps(address account, uint256 period) external view returns (uint256);
    function isRoundFinalized(uint256 period) external view returns (bool);
}

/// @title ZkSysIssuer
/// @notice Indexed zkSYS reward distributor for L2-canonical issuance.
contract ZkSysIssuer is Initializable, AccessControlUpgradeable, IZkSysWeightReceiver, IZkSysServiceWeightReceiver {
    uint256 public constant REWARD_PRECISION = 1e36;
    uint256 public constant BPS_DENOMINATOR = 10_000;
    uint256 public constant SCHEDULE_YEAR_SECONDS = 365 days;
    uint256 public constant YEAR_1_RATE_BPS = 2_000;
    uint256 public constant YEAR_2_RATE_BPS = 1_200;
    uint256 public constant YEAR_3_RATE_BPS = 800;
    uint256 public constant LONG_RUN_RATE_BPS = 500;
    uint256 public constant MAX_SERVICE_PERIODS = 64;
    bytes32 public constant MISSED_SERVICE_ACTIVATION = keccak256("MISSED_SERVICE_ACTIVATION");

    struct ServiceAccount {
        uint256 lastRewardIndex;
        uint256 accruedPassiveRewards;
        uint256 remainder;
        bool initialized;
    }

    error InvalidAddress();
    error InvalidSchedule();
    error NoWeight();
    error NoRewardsAvailable();
    error SupplyCapExceeded(uint256 scheduledSupply, uint256 maxSupply);
    error UnauthorizedRegistry();
    error ServiceAccountingAlreadyConfigured();
    error InvalidServiceConfiguration();
    error ServiceNotActivated();
    error ServiceComponentCallbackRequired();
    error ServiceCheckpointRequired(uint256 nextPeriod, uint256 currentPeriod);
    error InvalidServicePeriodCount();
    error ServicePeriodNotCheckpointed(uint256 period);
    error ServiceRoundNotFinalized(uint256 period);
    error ServiceRewardAlreadyClaimed(uint256 period);
    error InvalidServiceFactor(uint256 factor);
    error InsufficientRewardBudget();
    error ReentrantRewardClaim();
    error ServiceLaunchCannotAbort();

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
    IZkSysProverServiceSource public serviceSource;
    bool public serviceAccountingStarted;
    bool private _claimEntered;
    bool public serviceLaunchAborted;
    uint256 public serviceStartPeriodPlusOne;
    uint256 public nextServicePeriod;
    uint256 public serviceStartRewardIndex;
    uint256 public legacyRewardBudget;
    uint256 public serviceRewardBudget;
    mapping(uint256 period => uint256 rewardIndexDelta) public servicePeriodRewardIndex;
    mapping(address account => ServiceAccount accounting) private _serviceAccounts;
    mapping(address account => mapping(uint256 period => bool claimed)) public serviceRewardClaimed;
    uint256[35] private __gap;

    event RewardsDistributed(uint256 amount, uint256 indexed distributedThroughPeriod, uint256 accRewardPerWeight);
    event RewardsSkipped(uint256 amount, uint256 indexed distributedThroughPeriod);
    event RewardsClaimed(address indexed account, address indexed receiver, uint256 amount);
    event WeightChanged(address indexed account, uint256 oldWeight, uint256 newWeight);
    event ServiceAccountingConfigured(address indexed source, uint256 activationPeriod);
    event ServiceLaunchAborted(uint256 indexed activationPeriod, bytes32 reason);
    event ServicePeriodCheckpointed(
        uint256 indexed period, uint256 scheduledAmount, uint256 denominator, uint256 indexDelta
    );
    event ServiceRewardsClaimed(
        address indexed account, address indexed receiver, uint256 indexed period, uint256 amount
    );

    modifier nonReentrantClaim() {
        if (_claimEntered) {
            revert ReentrantRewardClaim();
        }
        _claimEntered = true;
        _;
        _claimEntered = false;
    }

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
        if (startTime_ <= block.timestamp) {
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
        if (totalWeight == 0 && serviceStartPeriodPlusOne == 0) {
            revert NoWeight();
        }

        amount = _checkpointRewards(totalWeight);
        if (amount == 0) {
            revert NoRewardsAvailable();
        }
    }

    function configureServiceAccounting(IZkSysProverServiceSource source, uint256 activationPeriod)
        external
        onlyRole(DEFAULT_ADMIN_ROLE)
    {
        if (serviceStartPeriodPlusOne != 0) {
            revert ServiceAccountingAlreadyConfigured();
        }
        if (
            address(source).code.length == 0 || source.issuer() != address(this) || source.startTime() != startTime
                || source.periodSeconds() != periodSeconds || source.firstServicePeriod() != activationPeriod
                || activationPeriod > (type(uint256).max - startTime) / periodSeconds
                || block.timestamp >= startTime + activationPeriod * periodSeconds
        ) {
            revert InvalidServiceConfiguration();
        }
        IZkSysServiceRewardWeightSource(address(registry)).enableServiceAccounting(activationPeriod);
        serviceSource = source;
        serviceStartPeriodPlusOne = activationPeriod + 1;
        nextServicePeriod = activationPeriod;
        emit ServiceAccountingConfigured(address(source), activationPeriod);
    }

    function checkpointServicePeriods(uint256 maxPeriods) external returns (uint256 amount) {
        if (serviceStartPeriodPlusOne == 0) {
            revert InvalidServiceConfiguration();
        }
        if (maxPeriods == 0 || maxPeriods > MAX_SERVICE_PERIODS) {
            revert InvalidServicePeriodCount();
        }
        return _checkpointAccounting(
            registry.totalWeight(), IZkSysServiceRewardWeightSource(address(registry)).totalPassiveWeight(), maxPeriods
        );
    }

    function abortMissedServiceActivation() external {
        if (
            serviceStartPeriodPlusOne == 0 || serviceAccountingStarted || serviceLaunchAborted
                || block.timestamp < startTime + (serviceStartPeriodPlusOne - 1) * periodSeconds
                || serviceSource.serviceActive()
        ) {
            revert ServiceLaunchCannotAbort();
        }
        _abortServiceLaunch();
    }

    function _abortServiceLaunch() private {
        // Missing the activation gate cannot strand native stake in the vault. The bonus lane
        // closes permanently, so no later source response can resurrect its unused allocation.
        serviceLaunchAborted = true;
        emit ServiceLaunchAborted(serviceStartPeriodPlusOne - 1, MISSED_SERVICE_ACTIVATION);
    }

    /// @notice Current passive plus admitted bonus weight, before the recipient's service factor.
    /// @dev Registry legacy weights include senior potential even for an unqualified applicant.
    function currentRewardWeight(address account) external view returns (uint256) {
        if (!_serviceWeightViewActive()) {
            return registry.weightOf(account);
        }
        (uint256 passiveWeight,) = IZkSysServiceRewardWeightSource(address(registry)).rewardWeightComponents(account);
        return passiveWeight + (serviceLaunchAborted ? 0 : serviceSource.admittedBonusWeight(account, currentPeriod()));
    }

    function currentRewardDenominator() external view returns (uint256) {
        if (!_serviceWeightViewActive()) {
            return registry.totalWeight();
        }
        return IZkSysServiceRewardWeightSource(address(registry)).totalPassiveWeight()
            + (serviceLaunchAborted ? 0 : serviceSource.totalAdmittedBonusWeight(currentPeriod()));
    }

    function _serviceWeightViewActive() private view returns (bool) {
        if (
            serviceStartPeriodPlusOne == 0
                || block.timestamp < startTime + (serviceStartPeriodPlusOne - 1) * periodSeconds
        ) {
            return false;
        }
        if (!serviceLaunchAborted && !serviceSource.serviceActive()) {
            revert ServiceNotActivated();
        }
        return true;
    }

    function _checkpointRewards(uint256 totalWeight) private returns (uint256 amount) {
        if (serviceStartPeriodPlusOne != 0) {
            return _checkpointAccounting(
                totalWeight,
                IZkSysServiceRewardWeightSource(address(registry)).totalPassiveWeight(),
                MAX_SERVICE_PERIODS
            );
        }
        return _checkpointLegacyRewards(totalWeight, currentPeriod());
    }

    function _checkpointLegacyRewards(uint256 totalWeight, uint256 distributedThroughPeriod)
        private
        returns (uint256 amount)
    {
        uint256 scheduledRewards = cumulativeScheduledRewards(distributedThroughPeriod);
        amount = scheduledRewards - totalScheduledRewards;
        if (amount == 0) {
            return 0;
        }

        uint256 maxSupply = token.maxSupply();
        if (scheduledRewards > maxSupply) {
            revert SupplyCapExceeded(scheduledRewards, maxSupply);
        }

        if (totalWeight != 0) {
            accRewardPerWeight += amount * REWARD_PRECISION / totalWeight;
            scheduledUnclaimedRewards += amount;
            emit RewardsDistributed(amount, distributedThroughPeriod, accRewardPerWeight);
        } else {
            emit RewardsSkipped(amount, distributedThroughPeriod);
        }
        totalScheduledRewards = scheduledRewards;
        lastDistributedPeriod = distributedThroughPeriod;
    }

    function _checkpointBeforeFirstWeight() private {
        _checkpointLegacyRewards(0, currentPeriod());
    }

    function _checkpointAccounting(uint256 oldTotalWeight, uint256 passiveTotal, uint256 maxPeriods)
        private
        returns (uint256 amount)
    {
        uint256 current = currentPeriod();
        uint256 activationPeriod = serviceStartPeriodPlusOne - 1;
        if (!serviceAccountingStarted) {
            uint256 legacyThrough = current < activationPeriod ? current : activationPeriod;
            amount = _checkpointLegacyRewards(oldTotalWeight, legacyThrough);
            if (block.timestamp < startTime + activationPeriod * periodSeconds) {
                return amount;
            }
            if (!serviceLaunchAborted && !serviceSource.serviceActive()) {
                _abortServiceLaunch();
            }
            serviceAccountingStarted = true;
            serviceStartRewardIndex = accRewardPerWeight;
            legacyRewardBudget = scheduledUnclaimedRewards;
        }

        uint256 period = nextServicePeriod;
        uint256 stop = current - period > maxPeriods ? period + maxPeriods : current;
        while (period < stop) {
            uint256 scheduled = cumulativeScheduledRewards(period + 1);
            uint256 emitted = scheduled - totalScheduledRewards;
            uint256 denominator =
                passiveTotal + (serviceLaunchAborted ? 0 : serviceSource.totalAdmittedBonusWeight(period));
            uint256 indexDelta;
            if (denominator != 0) {
                indexDelta = Math.mulDiv(emitted, REWARD_PRECISION, denominator);
                accRewardPerWeight += indexDelta;
                serviceRewardBudget += emitted;
                scheduledUnclaimedRewards += emitted;
                emit RewardsDistributed(emitted, period + 1, accRewardPerWeight);
            } else {
                emit RewardsSkipped(emitted, period + 1);
            }
            // A period consumes its original scheduled budget even when some admitted bonus
            // earns no service factor. Only this immutable index can authorize its later claim.
            servicePeriodRewardIndex[period] = indexDelta;
            totalScheduledRewards = scheduled;
            lastDistributedPeriod = period + 1;
            amount += emitted;
            emit ServicePeriodCheckpointed(period, emitted, denominator, indexDelta);
            ++period;
        }
        nextServicePeriod = period;
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

    function claim(address receiver) external nonReentrantClaim returns (uint256 claimed) {
        if (receiver == address(0)) {
            revert InvalidAddress();
        }

        uint256 weight = registry.weightOf(msg.sender);
        if (serviceAccountingStarted) {
            (uint256 passiveWeight,) =
                IZkSysServiceRewardWeightSource(address(registry)).rewardWeightComponents(msg.sender);
            _settleServiceAccount(msg.sender, weight, passiveWeight);
            uint256 legacyAccrued = accruedRewardsOf[msg.sender];
            uint256 legacyClaim = legacyAccrued > legacyRewardBudget ? legacyRewardBudget : legacyAccrued;
            legacyRewardBudget -= legacyClaim;
            accruedRewardsOf[msg.sender] = 0;

            ServiceAccount storage accounting = _serviceAccounts[msg.sender];
            uint256 passiveClaim = accounting.accruedPassiveRewards;
            accounting.accruedPassiveRewards = 0;
            if (passiveClaim > serviceRewardBudget) {
                revert InsufficientRewardBudget();
            }
            serviceRewardBudget -= passiveClaim;
            claimed = legacyClaim + passiveClaim;
            scheduledUnclaimedRewards -= claimed;
            if (claimed != 0) {
                require(token.mint(receiver, claimed), "issuer: mint failed");
                emit RewardsClaimed(msg.sender, receiver, claimed);
            }
            return claimed;
        }

        _settle(msg.sender, weight);
        uint256 accrued = accruedRewardsOf[msg.sender];
        if (accrued == 0) {
            return 0;
        }

        uint256 available = scheduledUnclaimedRewards;
        claimed = accrued > available ? available : accrued;
        accruedRewardsOf[msg.sender] = 0;
        if (claimed == 0) {
            return 0;
        }

        scheduledUnclaimedRewards = available - claimed;
        require(token.mint(receiver, claimed), "issuer: mint failed");

        emit RewardsClaimed(msg.sender, receiver, claimed);
    }

    function claimServiceRewards(uint256[] calldata periods, address receiver)
        external
        nonReentrantClaim
        returns (uint256 claimed)
    {
        if (receiver == address(0)) {
            revert InvalidAddress();
        }
        if (periods.length == 0 || periods.length > MAX_SERVICE_PERIODS) {
            revert InvalidServicePeriodCount();
        }
        for (uint256 i; i < periods.length; ++i) {
            uint256 period = periods[i];
            if (serviceRewardClaimed[msg.sender][period]) {
                revert ServiceRewardAlreadyClaimed(period);
            }
            uint256 reward = _serviceReward(msg.sender, period);
            serviceRewardClaimed[msg.sender][period] = true;
            claimed += reward;
            emit ServiceRewardsClaimed(msg.sender, receiver, period, reward);
        }
        if (claimed > serviceRewardBudget) {
            revert InsufficientRewardBudget();
        }
        serviceRewardBudget -= claimed;
        scheduledUnclaimedRewards -= claimed;
        if (claimed != 0) {
            require(token.mint(receiver, claimed), "issuer: mint failed");
        }
    }

    function pendingServiceRewards(address account, uint256 period) external view returns (uint256) {
        if (serviceRewardClaimed[account][period]) {
            return 0;
        }
        return _serviceReward(account, period);
    }

    function _serviceReward(address account, uint256 period) private view returns (uint256) {
        if (!serviceAccountingStarted || period < serviceStartPeriodPlusOne - 1 || period >= nextServicePeriod) {
            revert ServicePeriodNotCheckpointed(period);
        }
        if (serviceLaunchAborted) {
            return 0;
        }
        if (!serviceSource.isRoundFinalized(period)) {
            revert ServiceRoundNotFinalized(period);
        }
        uint256 factor = serviceSource.serviceFactorBps(account, period);
        if (factor > BPS_DENOMINATOR) {
            revert InvalidServiceFactor(factor);
        }
        uint256 fullBonus = Math.mulDiv(
            serviceSource.admittedBonusWeight(account, period), servicePeriodRewardIndex[period], REWARD_PRECISION
        );
        return Math.mulDiv(fullBonus, factor, BPS_DENOMINATOR);
    }

    function pendingRewards(address account) external view returns (uint256) {
        uint256 weight = registry.weightOf(account);
        if (serviceAccountingStarted) {
            ServiceAccount memory accounting = _serviceAccounts[account];
            uint256 legacyAccrued = accruedRewardsOf[account];
            if (!accounting.initialized) {
                uint256 legacyAccumulated = Math.mulDiv(weight, serviceStartRewardIndex, REWARD_PRECISION);
                uint256 debt = rewardDebtOf[account];
                if (legacyAccumulated > debt) {
                    legacyAccrued += legacyAccumulated - debt;
                }
                accounting.lastRewardIndex = serviceStartRewardIndex;
            }
            (uint256 passiveWeight,) =
                IZkSysServiceRewardWeightSource(address(registry)).rewardWeightComponents(account);
            (uint256 additional,) =
                _passiveAccrual(passiveWeight, accRewardPerWeight - accounting.lastRewardIndex, accounting.remainder);
            return legacyAccrued + accounting.accruedPassiveRewards + additional;
        }
        uint256 accumulated = _rewardDebt(weight);
        return accruedRewardsOf[account] + accumulated - rewardDebtOf[account];
    }

    function onWeightChange(address account, uint256 oldWeight, uint256 newWeight, uint256 oldTotalWeight) external {
        if (msg.sender != address(registry)) {
            revert UnauthorizedRegistry();
        }
        if (serviceStartPeriodPlusOne != 0) {
            revert ServiceComponentCallbackRequired();
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

    function onWeightComponentsChange(
        address account,
        uint256 oldWeight,
        uint256 newWeight,
        uint256 oldPassiveWeight,
        uint256,
        uint256 oldTotalWeight,
        uint256 oldTotalPassiveWeight
    ) external {
        if (msg.sender != address(registry)) {
            revert UnauthorizedRegistry();
        }
        if (serviceStartPeriodPlusOne == 0) {
            revert InvalidServiceConfiguration();
        }
        _checkpointAccounting(oldTotalWeight, oldTotalPassiveWeight, MAX_SERVICE_PERIODS);
        if (serviceAccountingStarted) {
            uint256 current = currentPeriod();
            if (nextServicePeriod != current) {
                // A withdrawal must not rewrite the denominator of an unprocessed old period.
                // Anyone can advance the bounded checkpoint cursor before retrying this change.
                revert ServiceCheckpointRequired(nextServicePeriod, current);
            }
            _settleServiceAccount(account, oldWeight, oldPassiveWeight);
        } else {
            _settle(account, oldWeight);
            rewardDebtOf[account] = _rewardDebt(newWeight);
        }
        emit WeightChanged(account, oldWeight, newWeight);
    }

    function _settleServiceAccount(address account, uint256 legacyWeight, uint256 passiveWeight) private {
        ServiceAccount storage accounting = _serviceAccounts[account];
        if (!accounting.initialized) {
            uint256 legacyAccumulated = Math.mulDiv(legacyWeight, serviceStartRewardIndex, REWARD_PRECISION);
            uint256 debt = rewardDebtOf[account];
            if (legacyAccumulated > debt) {
                accruedRewardsOf[account] += legacyAccumulated - debt;
            }
            rewardDebtOf[account] = legacyAccumulated;
            accounting.lastRewardIndex = serviceStartRewardIndex;
            accounting.initialized = true;
        }
        (uint256 additional, uint256 remainder) =
            _passiveAccrual(passiveWeight, accRewardPerWeight - accounting.lastRewardIndex, accounting.remainder);
        accounting.accruedPassiveRewards += additional;
        accounting.remainder = remainder;
        accounting.lastRewardIndex = accRewardPerWeight;
    }

    function _passiveAccrual(uint256 weight, uint256 indexDelta, uint256 priorRemainder)
        private
        pure
        returns (uint256 amount, uint256 remainder)
    {
        // Carry only this recipient's earned fraction across weight changes. Subtracting two
        // independently rounded weight*index debts lets repeated churn create unbacked dust.
        amount = Math.mulDiv(weight, indexDelta, REWARD_PRECISION);
        remainder = mulmod(weight, indexDelta, REWARD_PRECISION) + priorRemainder;
        amount += remainder / REWARD_PRECISION;
        remainder %= REWARD_PRECISION;
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
