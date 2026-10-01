// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {EIP712} from "@openzeppelin/contracts-v4/utils/cryptography/EIP712.sol";
import {SignatureChecker} from "@openzeppelin/contracts-v4/utils/cryptography/SignatureChecker.sol";
import {ZkSysMembershipRegistry} from "./ZkSysMembershipRegistry.sol";
import {
    AcceptedPackageV1,
    DutySuccessV1,
    ProverSubscriptionV1,
    WrapperCandidateV1,
    IZkSysServiceClockV1,
    IZkSysL1MessengerV1,
    ZkSysServiceTypesV1
} from "./ZkSysServiceTypesV1.sol";

/// @notice Versioned service records; registering alone never reserves reward weight.
/// @dev The immutable acceptance source must authenticate production proof acceptance and its full
/// work report. Signatures authenticate accountable operators, not hardware ownership or private
/// delivery times. This contract is deployed directly, independently of existing proxy storage.
contract ZkSysProverServiceRegistryV1 is EIP712 {
    using SignatureChecker for address;

    uint256 public constant BPS_DENOMINATOR = 10_000;
    uint128 public constant SENTRY_BASE_WEIGHT = 100_000 ether;
    uint64 public constant FIRST_SENIOR_AGE = 210_240;
    uint64 public constant FULL_SENIOR_AGE = 525_600;
    uint64 public constant MAX_SUBSCRIPTION_PERIODS = 64;
    uint16 public constant MAX_DUTIES_PER_ROUND = 64;
    uint32 public constant MAX_ROSTER_PAGE_SIZE = 64;
    bytes32 public constant ROSTER_DOMAIN = keccak256("ZKSYS_QUALIFIED_WRAPPER_ROSTER_V1");
    IZkSysL1MessengerV1 public constant L1_MESSENGER = IZkSysL1MessengerV1(address(0x8008));

    struct Configuration {
        ZkSysMembershipRegistry membershipRegistry;
        address issuer;
        address acceptanceSource;
        address settlementChainAddress;
        bytes32 policyHash;
        uint64 firstServicePeriod;
        uint64 receiptGraceSeconds;
        uint64 membershipMaxAgeSeconds;
        uint64 rosterPublicationLeadSeconds;
        uint16 dutiesPerRound;
        uint256 gatewayChainId;
        address gatewayChainAddress;
        bytes32 gatewayVkHash;
        address sharedSequencer;
    }

    struct RosterPublication {
        uint32 count;
        address lastAccount;
        bytes32[32] frontier;
    }

    error InvalidConfiguration();
    error UnauthorizedAcceptanceSource();
    error NotSeniorOrStale(address account);
    error InvalidSubscription();
    error ServiceNotVerified(address account, address sequencer);
    error InvalidSignature();
    error SubscriptionPeriodAlreadyUsed(address account, address sequencer, uint64 period);
    error OperatorAlreadyUsed(address operator, uint64 period);
    error InvalidPackage();
    error InvalidDuty();
    error DutyAlreadyCredited(uint64 batchNumber);
    error DutySlotAlreadyUsed(address account, uint64 period, uint16 slot);
    error RoundClosed(uint64 period);
    error RoundNotFinalized(uint256 period);
    error WrongServiceMode();
    error NoQualifiedRoster();
    error RosterNotFrozen();
    error InvalidRoster();

    ZkSysMembershipRegistry public immutable membershipRegistry;
    address public immutable issuer;
    address public immutable acceptanceSource;
    address public immutable settlementChainAddress;
    bytes32 public immutable policyHash;
    uint256 public immutable startTime;
    uint256 public immutable periodSeconds;
    uint64 public immutable firstServicePeriod;
    uint64 public immutable receiptGraceSeconds;
    uint64 public immutable membershipMaxAgeSeconds;
    uint64 public immutable rosterPublicationLeadSeconds;
    uint16 public immutable dutiesPerRound;
    uint256 public immutable gatewayChainId;
    address public immutable gatewayChainAddress;
    bytes32 public immutable gatewayVkHash;
    address public immutable sharedSequencer;

    bool public serviceActive;
    bytes32 public productionVkHash;
    bytes32 public bootstrapVkHash;
    bytes32 public initialQualifiedRosterRoot;
    mapping(address account => uint64 nonce) public nonces;
    mapping(bytes32 subscriptionHash => ProverSubscriptionV1 subscription) private _subscriptions;
    mapping(address account => mapping(address sequencer => mapping(uint64 period => bytes32 subscriptionHash))) public
        subscriptionAt;
    mapping(address operator => mapping(uint64 period => address account)) public operatorAccountAt;
    mapping(address sequencer => mapping(uint64 period => address[] accounts)) private _friSubscribers;
    mapping(bytes32 packageHash => bool accepted) public acceptedPackages;
    mapping(bytes32 canonicalDuty => bool credited) public creditedDuties;
    mapping(address account => mapping(uint64 period => uint64 bitmap)) public dutySlots;
    mapping(address account => uint64 bitmap) public bootstrapDutySlots;
    mapping(address account => uint16 count) public bootstrapSuccesses;
    mapping(address account => mapping(uint64 period => uint16 count)) public assessedSuccesses;
    mapping(address account => mapping(uint64 period => uint16 count)) public rewardedSuccesses;
    mapping(address account => mapping(address sequencer => bool verified)) public verifiedForSequencer;
    mapping(address account => mapping(uint256 period => uint256 bonus)) private _admittedBonus;
    mapping(uint256 period => uint256 bonus) private _totalAdmittedBonus;
    mapping(address account => mapping(uint64 period => WrapperCandidateV1 candidate)) private _wrapperCandidates;
    mapping(uint64 period => uint32 count) public qualifiedWrapperCount;
    mapping(uint64 period => bytes32 root) public publishedRosterRoot;
    mapping(address publisher => mapping(uint64 period => RosterPublication publication)) private _publications;

    event Subscribed(bytes32 indexed subscriptionHash, address indexed account, address indexed operator);
    event DutyAccepted(
        bytes32 indexed packageHash,
        address indexed account,
        uint64 indexed period,
        uint64 batchNumber,
        uint16 slot,
        bool qualificationOnly
    );
    event ProverAdmitted(address indexed account, uint64 indexed period, uint256 bonusWeight);
    event ServiceVerified(address indexed account, address indexed sequencer);
    event WrapperRenewed(address indexed account, uint64 indexed period, address operator, address beneficiary);
    event ServiceActivated(uint64 indexed firstPeriod, bytes32 productionVkHash, bytes32 qualifiedRosterRoot);
    event RosterPublished(uint64 indexed period, bytes32 root, uint32 count, bytes32 messageHash);

    constructor(Configuration memory config) EIP712("ZkSysProverService", "1") {
        // Account-scoped quota and roster state must never combine distinct sequencers.
        if (
            address(config.membershipRegistry).code.length == 0 || config.issuer.code.length == 0
                || config.acceptanceSource.code.length == 0 || config.settlementChainAddress == address(0)
                || config.policyHash == bytes32(0) || config.dutiesPerRound == 0
                || config.dutiesPerRound > MAX_DUTIES_PER_ROUND || config.membershipMaxAgeSeconds == 0
                || config.sharedSequencer == address(0)
        ) revert InvalidConfiguration();

        if (config.gatewayChainId == 0) {
            if (config.gatewayChainAddress != address(0) || config.gatewayVkHash != bytes32(0)) {
                revert InvalidConfiguration();
            }
        } else if (
            config.gatewayChainId == block.chainid || config.gatewayChainAddress == address(0)
                || config.gatewayVkHash == bytes32(0)
        ) {
            revert InvalidConfiguration();
        }

        uint256 start = IZkSysServiceClockV1(config.issuer).startTime();
        uint256 secondsPerPeriod = IZkSysServiceClockV1(config.issuer).periodSeconds();
        if (
            secondsPerPeriod == 0 || config.receiptGraceSeconds > secondsPerPeriod
                || config.rosterPublicationLeadSeconds == 0 || config.rosterPublicationLeadSeconds > secondsPerPeriod
                || start + uint256(config.firstServicePeriod) * secondsPerPeriod
                    <= block.timestamp + config.rosterPublicationLeadSeconds
        ) revert InvalidConfiguration();

        membershipRegistry = config.membershipRegistry;
        issuer = config.issuer;
        acceptanceSource = config.acceptanceSource;
        settlementChainAddress = config.settlementChainAddress;
        policyHash = config.policyHash;
        startTime = start;
        periodSeconds = secondsPerPeriod;
        firstServicePeriod = config.firstServicePeriod;
        receiptGraceSeconds = config.receiptGraceSeconds;
        membershipMaxAgeSeconds = config.membershipMaxAgeSeconds;
        rosterPublicationLeadSeconds = config.rosterPublicationLeadSeconds;
        dutiesPerRound = config.dutiesPerRound;
        gatewayChainId = config.gatewayChainId;
        gatewayChainAddress = config.gatewayChainAddress;
        gatewayVkHash = config.gatewayVkHash;
        sharedSequencer = config.sharedSequencer;
    }

    modifier onlyAcceptanceSource() {
        if (msg.sender != acceptanceSource) revert UnauthorizedAcceptanceSource();
        _;
    }

    function subscribe(ProverSubscriptionV1 calldata subscription_, bytes calldata signature)
        external
        returns (bytes32 subscriptionHash)
    {
        uint64 earliestPeriod = _nextUnstartedPeriod();
        if (
            subscription_.account == address(0) || subscription_.operator == address(0)
                || subscription_.beneficiary == address(0) || subscription_.sequencer == address(0)
                || subscription_.operator == subscription_.sequencer || subscription_.firstPeriod < earliestPeriod
                || subscription_.lastPeriod < subscription_.firstPeriod
                || subscription_.lastPeriod - subscription_.firstPeriod >= MAX_SUBSCRIPTION_PERIODS
                || subscription_.services != (ZkSysServiceTypesV1.FRI_SERVICE | ZkSysServiceTypesV1.WRAPPER_SERVICE)
                || subscription_.nonce != nonces[subscription_.account] || subscription_.sequencer != sharedSequencer
        ) revert InvalidSubscription();
        _seniorBonus(subscription_.account);
        subscriptionHash = ZkSysServiceTypesV1.hashSubscription(subscription_);
        if (!subscription_.account.isValidSignatureNow(_hashTypedDataV4(subscriptionHash), signature)) {
            revert InvalidSignature();
        }

        ++nonces[subscription_.account];
        _subscriptions[subscriptionHash] = subscription_;
        for (uint64 period = subscription_.firstPeriod;; ++period) {
            if (subscriptionAt[subscription_.account][subscription_.sequencer][period] != bytes32(0)) {
                revert SubscriptionPeriodAlreadyUsed(subscription_.account, subscription_.sequencer, period);
            }
            address operatorAccount = operatorAccountAt[subscription_.operator][period];
            if (operatorAccount != address(0) && operatorAccount != subscription_.account) {
                revert OperatorAlreadyUsed(subscription_.operator, period);
            }
            subscriptionAt[subscription_.account][subscription_.sequencer][period] = subscriptionHash;
            operatorAccountAt[subscription_.operator][period] = subscription_.account;
            _friSubscribers[subscription_.sequencer][period].push(subscription_.account);
            if (period == subscription_.lastPeriod) break;
        }
        emit Subscribed(subscriptionHash, subscription_.account, subscription_.operator);
    }

    function subscription(bytes32 subscriptionHash) external view returns (ProverSubscriptionV1 memory) {
        return _subscriptions[subscriptionHash];
    }

    function supportedLane(uint256 executionChainId) public view returns (address chainAddress, bytes32 vkHash) {
        if (executionChainId == block.chainid) {
            return (settlementChainAddress, serviceActive ? productionVkHash : bootstrapVkHash);
        }
        if (gatewayChainId != 0 && executionChainId == gatewayChainId) return (gatewayChainAddress, gatewayVkHash);
        return (address(0), bytes32(0));
    }

    function friSubscriberCount(address sequencer, uint64 period) external view returns (uint256) {
        return _friSubscribers[sequencer][period].length;
    }

    function friSubscriberAt(address sequencer, uint64 period, uint256 index) external view returns (address) {
        return _friSubscribers[sequencer][period][index];
    }

    /// @dev A removed or stale subscriber must not block a complete dispatcher snapshot. Enumeration
    /// remains immutable; this view makes eligibility exclusions explicit at the same pinned block.
    function isEligibleFriSubscriber(address account, address sequencer, uint64 period) external view returns (bool) {
        ProverSubscriptionV1 storage sub = _subscriptions[subscriptionAt[account][sequencer][period]];
        if (
            sub.account == address(0)
                || sub.services != (ZkSysServiceTypesV1.FRI_SERVICE | ZkSysServiceTypesV1.WRAPPER_SERVICE)
        ) return false;
        try this.seniorBonus(account) returns (uint256) {
            return true;
        } catch {
            return false;
        }
    }

    function subscriptionDigest(ProverSubscriptionV1 calldata subscription_) external view returns (bytes32) {
        return _hashTypedDataV4(ZkSysServiceTypesV1.hashSubscription(subscription_));
    }

    function dutyDigest(DutySuccessV1 calldata duty) external view returns (bytes32) {
        return _hashTypedDataV4(ZkSysServiceTypesV1.hashDuty(duty));
    }

    function activateService(bytes32 vkHash, bytes32 qualifiedRosterRoot) external onlyAcceptanceSource {
        if (serviceActive || block.timestamp >= _periodStart(firstServicePeriod)) revert WrongServiceMode();
        if (vkHash == bytes32(0) || vkHash != bootstrapVkHash || qualifiedRosterRoot == bytes32(0)) {
            revert InvalidPackage();
        }
        if (
            _totalAdmittedBonus[firstServicePeriod] == 0 || qualifiedWrapperCount[firstServicePeriod] == 0
                || publishedRosterRoot[firstServicePeriod] != qualifiedRosterRoot
        ) revert NoQualifiedRoster();
        productionVkHash = vkHash;
        initialQualifiedRosterRoot = qualifiedRosterRoot;
        serviceActive = true;
        emit ServiceActivated(firstServicePeriod, vkHash, qualifiedRosterRoot);
    }

    /// @dev Bootstrap must use the production verifier's canonical accepted-proof lane; the source
    /// may call this only before enabling the joint wrapper gate. It cannot mint service credit.
    function acceptBootstrapDuties(AcceptedPackageV1 calldata accepted, DutySuccessV1[] calldata duties)
        external
        onlyAcceptanceSource
    {
        if (serviceActive || block.timestamp >= _periodStart(firstServicePeriod)) revert WrongServiceMode();
        if (accepted.wrapper != address(0) || accepted.wrapperBeneficiary != address(0)) revert InvalidPackage();
        if (accepted.chainId == block.chainid && bootstrapVkHash != bytes32(0) && accepted.vkHash != bootstrapVkHash) {
            revert InvalidPackage();
        }
        _acceptDuties(accepted, duties, true);
        if (accepted.chainId == block.chainid) bootstrapVkHash = accepted.vkHash;
    }

    function acceptDuties(AcceptedPackageV1 calldata accepted, DutySuccessV1[] calldata duties)
        external
        onlyAcceptanceSource
    {
        if (!serviceActive) revert WrongServiceMode();
        if (
            accepted.wrapper == address(0) || accepted.wrapperBeneficiary == address(0)
                || accepted.wrapper == accepted.sequencer || accepted.rosterRoot == bytes32(0)
                || (accepted.chainId == block.chainid && accepted.vkHash != productionVkHash)
        ) revert InvalidPackage();
        _acceptDuties(accepted, duties, false);
    }

    function _acceptDuties(AcceptedPackageV1 calldata accepted, DutySuccessV1[] calldata duties, bool qualificationOnly)
        private
    {
        (address laneAddress, bytes32 laneVk) = supportedLane(accepted.chainId);
        if (
            accepted.domainVersion != ZkSysServiceTypesV1.DOMAIN_VERSION || accepted.policyHash != policyHash
                || laneAddress == address(0) || accepted.chainAddress != laneAddress
                || (laneVk != bytes32(0) && accepted.vkHash != laneVk) || accepted.sequencer != sharedSequencer
                || accepted.batchFrom == 0 || accepted.batchTo < accepted.batchFrom
                || accepted.batchTo - accepted.batchFrom >= ZkSysServiceTypesV1.MAX_BATCHES_PER_PACKAGE
                || accepted.vkHash == bytes32(0) || accepted.proofHash == bytes32(0)
                || accepted.manifestHash == bytes32(0) || accepted.sequencer == address(0)
                || accepted.sequencerBeneficiary == address(0) || accepted.protocolVersion != 32
                || duties.length > accepted.batchTo - accepted.batchFrom + 1
                || ZkSysServiceTypesV1.hashReport(duties) != accepted.reportHash
        ) revert InvalidPackage();

        bytes32 packageHash = ZkSysServiceTypesV1.hashPackage(accepted);
        if (acceptedPackages[packageHash]) return;
        if (isRoundFinalized(accepted.period)) revert RoundClosed(accepted.period);
        if (!qualificationOnly && block.timestamp < _periodStart(accepted.period)) revert InvalidPackage();
        acceptedPackages[packageHash] = true;

        for (uint256 i; i < duties.length; ++i) {
            DutySuccessV1 calldata duty = duties[i];
            ProverSubscriptionV1 storage subscription_ = _subscriptions[duty.subscriptionHash];
            if (
                duty.account == address(0) || subscription_.account != duty.account
                    || subscription_.sequencer != accepted.sequencer || duty.period != accepted.period
                    || duty.period < subscription_.firstPeriod || duty.period > subscription_.lastPeriod
                    || subscription_.services != (ZkSysServiceTypesV1.FRI_SERVICE | ZkSysServiceTypesV1.WRAPPER_SERVICE)
                    || duty.batchNumber < accepted.batchFrom || duty.batchNumber > accepted.batchTo
                    || duty.statementHash == bytes32(0) || duty.friProofHash == bytes32(0) || duty.transactionCount == 0
                    || duty.assignmentId == bytes32(0) || duty.attempt == 0 || duty.slot >= dutiesPerRound
            ) revert InvalidDuty();
            if (!subscription_.operator
                    .isValidSignatureNow(_hashTypedDataV4(ZkSysServiceTypesV1.hashDuty(duty)), duty.operatorSignature)) revert InvalidSignature();

            bytes32 canonicalDuty = keccak256(abi.encode(accepted.chainId, accepted.chainAddress, duty.batchNumber));
            if (creditedDuties[canonicalDuty]) revert DutyAlreadyCredited(duty.batchNumber);
            uint64 bit = uint64(1) << duty.slot;
            uint64 slots = qualificationOnly ? bootstrapDutySlots[duty.account] : dutySlots[duty.account][duty.period];
            if ((slots & bit) != 0) {
                revert DutySlotAlreadyUsed(duty.account, duty.period, duty.slot);
            }
            creditedDuties[canonicalDuty] = true;
            uint16 successes;
            if (qualificationOnly) {
                // Qualification before the first round must not consume that round's paid slots.
                bootstrapDutySlots[duty.account] = slots | bit;
                successes = ++bootstrapSuccesses[duty.account];
            } else {
                dutySlots[duty.account][duty.period] = slots | bit;
                successes = ++assessedSuccesses[duty.account][duty.period];
                if (duty.period >= firstServicePeriod) ++rewardedSuccesses[duty.account][duty.period];
            }
            if (successes == dutiesPerRound) {
                if (!verifiedForSequencer[duty.account][subscription_.sequencer]) {
                    verifiedForSequencer[duty.account][subscription_.sequencer] = true;
                    emit ServiceVerified(duty.account, subscription_.sequencer);
                }
                _admitForFuturePeriod(duty.account, subscription_);
            }
            emit DutyAccepted(packageHash, duty.account, duty.period, duty.batchNumber, duty.slot, qualificationOnly);
        }
    }

    function _admitForFuturePeriod(address account, ProverSubscriptionV1 storage subscription_) private {
        uint64 period = nextAdmissionPeriod();
        if (period < subscription_.firstPeriod || period > subscription_.lastPeriod) return;
        if (_admittedBonus[account][period] != 0) return;

        // Removal stops renewal, but cannot retroactively erase an accepted duty or a frozen quota.
        try this.seniorBonus(account) returns (uint256 bonus) {
            _admittedBonus[account][period] = bonus;
            _totalAdmittedBonus[period] += bonus;
            _installWrapper(subscription_, period);
            emit ProverAdmitted(account, period, bonus);
        } catch {}
    }

    /// @dev An idle chain still needs wrappers for its next proof. Historical verified capability
    /// permits fresh roster renewal, but only newly accepted work can reserve reward bonus weight.
    function renewWrapper(bytes32 subscriptionHash, uint64 period) external {
        ProverSubscriptionV1 storage subscription_ = _subscriptions[subscriptionHash];
        if (
            subscription_.account == address(0) || period != nextAdmissionPeriod() || period < subscription_.firstPeriod
                || period > subscription_.lastPeriod
                || subscription_.services != (ZkSysServiceTypesV1.FRI_SERVICE | ZkSysServiceTypesV1.WRAPPER_SERVICE)
        ) revert InvalidSubscription();
        if (!verifiedForSequencer[subscription_.account][subscription_.sequencer]) {
            revert ServiceNotVerified(subscription_.account, subscription_.sequencer);
        }
        _seniorBonus(subscription_.account);
        _installWrapper(subscription_, period);
    }

    function nextAdmissionPeriod() public view returns (uint64 period) {
        period = _nextUnstartedPeriod();
        if (period < firstServicePeriod) period = firstServicePeriod;
        if (block.timestamp >= rosterCutoff(period)) ++period;
    }

    function _installWrapper(ProverSubscriptionV1 storage subscription_, uint64 period) private {
        if (_wrapperCandidates[subscription_.account][period].account != address(0)) return;
        _wrapperCandidates[subscription_.account][period] = WrapperCandidateV1({
            index: 0,
            account: subscription_.account,
            operator: subscription_.operator,
            beneficiary: subscription_.beneficiary
        });
        ++qualifiedWrapperCount[period];
        emit WrapperRenewed(subscription_.account, period, subscription_.operator, subscription_.beneficiary);
    }

    function seniorBonus(address account) external view returns (uint256) {
        return _seniorBonus(account);
    }

    function rosterCutoff(uint64 period) public view returns (uint256) {
        uint256 periodStart = _periodStart(period);
        return periodStart > rosterPublicationLeadSeconds ? periodStart - rosterPublicationLeadSeconds : 0;
    }

    function qualifiedWrapper(address account, uint64 period) external view returns (WrapperCandidateV1 memory) {
        return _wrapperCandidates[account][period];
    }

    /// @dev Exact count plus sorted unique membership requires the complete qualified set. Each
    /// publisher has independent progress so a malicious unfinished prefix cannot lock publication.
    function publishRosterPage(uint64 period, WrapperCandidateV1[] calldata candidates)
        external
        returns (bytes32 root)
    {
        if (block.timestamp < rosterCutoff(period)) revert RosterNotFrozen();
        uint32 count = qualifiedWrapperCount[period];
        RosterPublication storage publication = _publications[msg.sender][period];
        if (
            count == 0 || candidates.length == 0 || candidates.length > MAX_ROSTER_PAGE_SIZE
                || candidates.length > uint256(count) - publication.count
        ) revert InvalidRoster();

        for (uint256 i; i < candidates.length; ++i) {
            WrapperCandidateV1 calldata candidate = candidates[i];
            WrapperCandidateV1 storage expected = _wrapperCandidates[candidate.account][period];
            if (
                candidate.index != publication.count || candidate.account <= publication.lastAccount
                    || expected.account != candidate.account || expected.operator != candidate.operator
                    || expected.beneficiary != candidate.beneficiary
            ) revert InvalidRoster();
            publication.lastAccount = candidate.account;
            _appendRosterLeaf(publication, ZkSysServiceTypesV1.wrapperLeaf(candidate));
        }
        if (publication.count != count) return bytes32(0);
        root = _completeRosterRoot(publication);
        bytes32 previousRoot = publishedRosterRoot[period];
        if (previousRoot != bytes32(0) && previousRoot != root) revert InvalidRoster();
        publishedRosterRoot[period] = root;
        delete _publications[msg.sender][period];
        bytes32 messageHash = L1_MESSENGER.sendToL1(
            abi.encode(
                ROSTER_DOMAIN,
                block.chainid,
                address(this),
                policyHash,
                startTime,
                periodSeconds,
                firstServicePeriod,
                period,
                root,
                count
            )
        );
        emit RosterPublished(period, root, count, messageHash);
    }

    function publicationProgress(address publisher, uint64 period)
        external
        view
        returns (uint32 count, address lastAccount)
    {
        RosterPublication storage publication = _publications[publisher][period];
        return (publication.count, publication.lastAccount);
    }

    function resetOwnPublication(uint64 period) external {
        delete _publications[msg.sender][period];
    }

    function _appendRosterLeaf(RosterPublication storage publication, bytes32 node) private {
        uint256 size = ++publication.count;
        for (uint256 level; level < 32; ++level) {
            if ((size & 1) == 1) {
                publication.frontier[level] = node;
                return;
            }
            node = _hashPair(publication.frontier[level], node);
            size >>= 1;
        }
        revert InvalidRoster();
    }

    function _completeRosterRoot(RosterPublication storage publication) private view returns (bytes32 node) {
        uint256 size = publication.count;
        bytes32 zero;
        for (uint256 level; level < 32; ++level) {
            node = (size & 1) == 1 ? _hashPair(publication.frontier[level], node) : _hashPair(node, zero);
            zero = _hashPair(zero, zero);
            size >>= 1;
        }
    }

    function _hashPair(bytes32 left, bytes32 right) private pure returns (bytes32) {
        return left < right ? keccak256(abi.encodePacked(left, right)) : keccak256(abi.encodePacked(right, left));
    }

    function _seniorBonus(address account) private view returns (uint256 bonus) {
        ZkSysMembershipRegistry.Member memory member = membershipRegistry.member(account);
        (uint64 observedCoreHeight, uint64 observedAt) = membershipRegistry.membershipObservation(account);
        if (
            member.sentryNodeCollateralHeight == 0 || observedAt == 0 || observedAt > block.timestamp
                || block.timestamp - observedAt > membershipMaxAgeSeconds
                || observedCoreHeight < uint64(member.sentryNodeCollateralHeight) + FIRST_SENIOR_AGE
        ) revert NotSeniorOrStale(account);

        bool fullySenior = observedCoreHeight >= uint64(member.sentryNodeCollateralHeight) + FULL_SENIOR_AGE;
        bonus = fullySenior ? SENTRY_BASE_WEIGHT : uint256(SENTRY_BASE_WEIGHT) * 3_500 / BPS_DENOMINATOR;
        if (member.sentryNodeWeight != SENTRY_BASE_WEIGHT + bonus) revert NotSeniorOrStale(account);
    }

    function admittedBonusWeight(address account, uint256 period) external view returns (uint256) {
        return serviceActive ? _admittedBonus[account][period] : 0;
    }

    function totalAdmittedBonusWeight(uint256 period) external view returns (uint256) {
        return serviceActive ? _totalAdmittedBonus[period] : 0;
    }

    function qualifiedBonusWeight(address account, uint256 period) external view returns (uint256) {
        return _admittedBonus[account][period];
    }

    function totalQualifiedBonusWeight(uint256 period) external view returns (uint256) {
        return _totalAdmittedBonus[period];
    }

    function serviceFactorBps(address account, uint256 period) external view returns (uint256) {
        if (!isRoundFinalized(period)) revert RoundNotFinalized(period);
        if (!serviceActive || period < firstServicePeriod || period > type(uint64).max) return 0;
        return uint256(rewardedSuccesses[account][uint64(period)]) * BPS_DENOMINATOR / dutiesPerRound;
    }

    function isRoundFinalized(uint256 period) public view returns (bool) {
        return block.timestamp >= _periodStart(period + 1) + receiptGraceSeconds;
    }

    function _periodStart(uint256 period) private view returns (uint256) {
        return startTime + period * periodSeconds;
    }

    function _nextUnstartedPeriod() private view returns (uint64) {
        if (block.timestamp < startTime) return 0;
        uint256 period = (block.timestamp - startTime) / periodSeconds + 1;
        if (period > type(uint64).max) revert InvalidConfiguration();
        return uint64(period);
    }
}
