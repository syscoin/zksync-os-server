// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {ERC1967Proxy} from "@openzeppelin/contracts-v4/proxy/ERC1967/ERC1967Proxy.sol";
import {Test} from "forge-std/Test.sol";
import {SyscoinZKSYSToken} from "contracts/src/zksys/SyscoinZKSYSToken.sol";
import {
    IZkSysMintableToken,
    IZkSysRewardWeightSource,
    IZkSysProverServiceSource,
    ZkSysIssuer
} from "contracts/src/zksys/ZkSysIssuer.sol";
import {ZkSysMembershipRegistry} from "contracts/src/zksys/ZkSysMembershipRegistry.sol";
import {IZkSysStakeWeightRegistry, ZkSysNativeStakingVault} from "contracts/src/zksys/ZkSysNativeStakingVault.sol";
import {
    IL1BridgehubMinimal,
    IZkSysMembershipRegistryL2,
    ZkSysRegistryBridge
} from "contracts/src/zksys/ZkSysRegistryBridge.sol";
import {ZkSysRewardWeightRegistry} from "contracts/src/zksys/ZkSysRewardWeightRegistry.sol";

contract IssuerBridgehubMock is IL1BridgehubMinimal {
    bytes32 public constant TX_HASH = keccak256("issuer-bridge-tx");

    L2TransactionRequestDirect public lastRequest;

    function requestL2TransactionDirect(L2TransactionRequestDirect calldata request)
        external
        payable
        returns (bytes32 canonicalTxHash)
    {
        lastRequest = request;
        return TX_HASH;
    }

    function lastDecodedUpdates() external view returns (IZkSysMembershipRegistryL2.SentryNodeUpdate[] memory updates) {
        updates = abi.decode(_withoutSelector(lastRequest.l2Calldata), (IZkSysMembershipRegistryL2.SentryNodeUpdate[]));
    }

    function _withoutSelector(bytes memory data) private pure returns (bytes memory result) {
        result = new bytes(data.length - 4);
        for (uint256 i = 4; i < data.length; ++i) {
            result[i - 4] = data[i];
        }
    }
}

contract IssuerServiceSourceMock {
    address public immutable issuer;
    uint256 public immutable startTime;
    uint256 public immutable periodSeconds;
    uint256 public immutable firstServicePeriod;
    bool public serviceActive = true;
    mapping(address => bool) public enrolled;
    mapping(address => mapping(uint256 => uint256)) public admittedBonusWeight;
    mapping(uint256 => uint256) public totalAdmittedBonusWeight;
    mapping(address => mapping(uint256 => uint256)) public serviceFactorBps;

    constructor(ZkSysIssuer issuer_, uint256 firstPeriod) {
        issuer = address(issuer_);
        startTime = issuer_.startTime();
        periodSeconds = issuer_.periodSeconds();
        firstServicePeriod = firstPeriod;
    }

    function setActive(bool active) external {
        serviceActive = active;
    }

    function enroll(address account, bool accepted) external {
        enrolled[account] = accepted;
    }

    function admit(address account, uint256 period, uint256 bonus) external {
        require(block.timestamp < startTime + period * periodSeconds, "admission already frozen");
        totalAdmittedBonusWeight[period] =
            totalAdmittedBonusWeight[period] - admittedBonusWeight[account][period] + bonus;
        admittedBonusWeight[account][period] = bonus;
    }

    function setFactor(address account, uint256 period, uint256 factor) external {
        require(!isRoundFinalized(period), "factor already final");
        serviceFactorBps[account][period] = factor;
    }

    function isRoundFinalized(uint256 period) public view returns (bool) {
        return block.timestamp >= startTime + (period + 1) * periodSeconds + 60;
    }
}

contract ZkSysIssuerTest is Test {
    uint64 private observationHeight = uint64(type(uint32).max) + 1;
    address private constant NEVM_ADDRESS_PRECOMPILE = address(0x62);

    uint256 private constant START_TIME = 1_000;
    uint256 private constant PERIOD_SECONDS = 1 days;
    uint256 private constant PERIODS_PER_YEAR = 365;
    uint256 private constant ACTIVATION_DELAY_PERIODS = 1;

    address private admin = address(0xAD);
    address private l1RegistryBridge = address(0xA11CE);
    address private alice = address(0xA11CE);
    address private bob = address(0xB0B);

    SyscoinZKSYSToken private token;
    ZkSysMembershipRegistry private membershipRegistry;
    ZkSysRewardWeightRegistry private registry;
    ZkSysNativeStakingVault private stakingVault;
    ZkSysIssuer private issuer;

    function setUp() public {
        SyscoinZKSYSToken implementation = new SyscoinZKSYSToken();
        ERC1967Proxy proxy = new ERC1967Proxy(
            address(implementation), abi.encodeCall(SyscoinZKSYSToken.initialize, ("ZKSYS", "ZKSYS", uint8(18), admin))
        );
        token = SyscoinZKSYSToken(address(proxy));

        membershipRegistry = _deployMembershipRegistry(admin, l1RegistryBridge);
        registry = _deployWeightRegistry(admin, membershipRegistry);
        stakingVault = _deployStakingVault(IZkSysStakeWeightRegistry(address(registry)));
        issuer = _deployIssuer(
            IZkSysMintableToken(address(token)),
            IZkSysRewardWeightSource(address(registry)),
            admin,
            START_TIME,
            PERIOD_SECONDS,
            PERIODS_PER_YEAR
        );

        vm.startPrank(admin);
        registry.setWeightReceiver(issuer);
        membershipRegistry.setSentryNodeReceiver(registry);
        registry.grantRole(registry.STAKE_WEIGHT_UPDATER_ROLE(), address(stakingVault));
        token.grantRole(token.MINTER_ROLE(), address(issuer));
        vm.stopPrank();
    }

    function testBatchUpdateAndDistributeThenClaim() public {
        _depositStake(alice, 1 ether);
        _depositStake(bob, 3 ether);

        assertEq(registry.totalWeight(), 4 ether);

        vm.warp(START_TIME + PERIOD_SECONDS);
        uint256 distributed = issuer.distribute();
        assertEq(distributed, yearOneEmission() / PERIODS_PER_YEAR);

        assertEq(issuer.pendingRewards(alice), distributed / 4);
        assertEq(issuer.pendingRewards(bob), distributed * 3 / 4);

        vm.prank(alice);
        assertEq(issuer.claim(alice), distributed / 4);

        vm.prank(bob);
        assertEq(issuer.claim(bob), distributed * 3 / 4);

        assertEq(token.balanceOf(alice), distributed / 4);
        assertEq(token.balanceOf(bob), distributed * 3 / 4);
        assertEq(issuer.scheduledUnclaimedRewards(), distributed - token.balanceOf(alice) - token.balanceOf(bob));
    }

    function testWeightIncreaseDoesNotEarnPastRewards() public {
        _depositStake(alice, 1 ether);

        vm.warp(START_TIME + PERIOD_SECONDS);
        uint256 firstDistribution = issuer.distribute();

        _depositStakePending(bob, 1 ether);

        vm.warp(START_TIME + 2 * PERIOD_SECONDS);
        _activateStake(bob);
        uint256 secondDistribution = issuer.pendingRewards(alice) - firstDistribution;

        assertEq(secondDistribution, firstDistribution);
        assertEq(issuer.pendingRewards(alice), firstDistribution + secondDistribution);
        assertEq(issuer.pendingRewards(bob), 0);

        vm.warp(START_TIME + 3 * PERIOD_SECONDS);
        uint256 thirdDistribution = issuer.distribute();

        assertEq(thirdDistribution, firstDistribution);
        assertEq(issuer.pendingRewards(alice), firstDistribution + secondDistribution + thirdDistribution / 2);
        assertEq(issuer.pendingRewards(bob), thirdDistribution / 2);
    }

    function testLateWeightIncreaseDoesNotEarnUndistributedBacklog() public {
        _depositStake(alice, 1 ether);

        vm.warp(START_TIME + PERIOD_SECONDS);
        uint256 firstDistribution = issuer.distribute();

        vm.warp(START_TIME + 3 * PERIOD_SECONDS);
        _depositStakePending(bob, 1 ether);

        uint256 twoPeriodBacklog = issuer.cumulativeScheduledRewards(3) - issuer.cumulativeScheduledRewards(1);
        assertEq(twoPeriodBacklog, 2 * firstDistribution);
        assertEq(issuer.pendingRewards(alice), firstDistribution);
        assertEq(issuer.pendingRewards(bob), 0);

        vm.warp(START_TIME + 4 * PERIOD_SECONDS);
        _activateStake(bob);
        uint256 delayedBacklog = issuer.cumulativeScheduledRewards(4) - issuer.cumulativeScheduledRewards(1);

        assertEq(delayedBacklog, issuer.cumulativeScheduledRewards(4) - issuer.cumulativeScheduledRewards(1));
        assertEq(issuer.pendingRewards(alice), firstDistribution + delayedBacklog);
        assertEq(issuer.pendingRewards(bob), 0);

        vm.warp(START_TIME + 5 * PERIOD_SECONDS);
        uint256 fifthPeriodDistribution = issuer.distribute();

        assertEq(fifthPeriodDistribution, issuer.cumulativeScheduledRewards(5) - issuer.cumulativeScheduledRewards(4));
        assertEq(issuer.pendingRewards(alice), firstDistribution + delayedBacklog + fifthPeriodDistribution / 2);
        assertEq(issuer.pendingRewards(bob), fifthPeriodDistribution / 2);
    }

    function testFirstWeightAfterStartDoesNotEarnEmptyRegistryBacklog() public {
        vm.warp(START_TIME + 2 * PERIOD_SECONDS);

        _depositStakePending(alice, 1 ether);
        vm.warp(START_TIME + 3 * PERIOD_SECONDS);
        _activateStake(alice);

        assertEq(issuer.pendingRewards(alice), 0);
        assertEq(issuer.totalScheduledRewards(), 3 * yearOneEmission() / PERIODS_PER_YEAR);
        assertEq(issuer.scheduledUnclaimedRewards(), 0);

        vm.warp(START_TIME + 4 * PERIOD_SECONDS);
        uint256 distribution = issuer.distribute();

        assertEq(distribution, issuer.cumulativeScheduledRewards(4) - issuer.cumulativeScheduledRewards(3));
        assertEq(issuer.pendingRewards(alice), distribution);
    }

    function testWeightDecreaseSettlesPriorRewards() public {
        _depositStake(alice, 2 ether);

        vm.warp(START_TIME + PERIOD_SECONDS);
        uint256 firstDistribution = issuer.distribute();

        _withdrawStake(alice, 1 ether);

        assertEq(issuer.pendingRewards(alice), firstDistribution);

        vm.warp(START_TIME + 2 * PERIOD_SECONDS);
        uint256 secondDistribution = issuer.distribute();

        assertEq(secondDistribution, firstDistribution);
        assertEq(issuer.pendingRewards(alice), firstDistribution + secondDistribution);
    }

    function testRemovingLastWeightSettlesBacklogAndLaterEmptyPeriodsAreSkipped() public {
        _depositStake(alice, 1 ether);

        vm.warp(START_TIME + PERIOD_SECONDS);
        uint256 firstDistribution = issuer.distribute();

        vm.warp(START_TIME + 3 * PERIOD_SECONDS);
        _withdrawStake(alice, 1 ether);

        uint256 twoPeriodBacklog = issuer.cumulativeScheduledRewards(3) - issuer.cumulativeScheduledRewards(1);
        assertEq(issuer.pendingRewards(alice), firstDistribution + twoPeriodBacklog);
        assertEq(registry.totalWeight(), 0);

        vm.warp(START_TIME + 5 * PERIOD_SECONDS);
        _depositStakePending(bob, 1 ether);

        assertEq(issuer.totalScheduledRewards(), issuer.cumulativeScheduledRewards(3));
        assertEq(issuer.pendingRewards(bob), 0);

        vm.warp(START_TIME + 6 * PERIOD_SECONDS);
        _activateStake(bob);

        assertEq(issuer.totalScheduledRewards(), issuer.cumulativeScheduledRewards(6));
        assertEq(issuer.pendingRewards(bob), 0);

        vm.warp(START_TIME + 7 * PERIOD_SECONDS);
        uint256 seventhPeriodDistribution = issuer.distribute();

        assertEq(seventhPeriodDistribution, issuer.cumulativeScheduledRewards(7) - issuer.cumulativeScheduledRewards(6));
        assertEq(issuer.pendingRewards(bob), seventhPeriodDistribution);
    }

    function testOutOfRangeWeightIsRejectedBeforeSettlement() public {
        _depositStake(alice, 1 ether);

        vm.warp(START_TIME + PERIOD_SECONDS);
        issuer.distribute();

        _withdrawStake(alice, 1 ether);

        vm.prank(address(stakingVault));
        vm.expectRevert(abi.encodeWithSelector(ZkSysRewardWeightRegistry.InvalidWeight.selector, type(uint256).max));
        registry.updateStakeWeight(bob, type(uint256).max);
    }

    function testDistributeRevertsBeforeRewardsAreAvailable() public {
        _depositStake(alice, 1 ether);

        vm.expectRevert(ZkSysIssuer.NoRewardsAvailable.selector);
        issuer.distribute();
    }

    function testDistributeRevertsWhenNoWeightExists() public {
        vm.warp(START_TIME + PERIOD_SECONDS);

        vm.expectRevert(ZkSysIssuer.NoWeight.selector);
        issuer.distribute();
    }

    function testOnlyWeightRegistryCanNotifyWeightChanges() public {
        vm.expectRevert(ZkSysIssuer.UnauthorizedRegistry.selector);
        issuer.onWeightChange(alice, 0, 1 ether, 0);
    }

    function testClaimRejectsZeroReceiver() public {
        _depositStake(alice, 1 ether);
        vm.warp(START_TIME + PERIOD_SECONDS);
        issuer.distribute();

        vm.prank(alice);
        vm.expectRevert(ZkSysIssuer.InvalidAddress.selector);
        issuer.claim(address(0));
    }

    function testClaimCanMintToThirdPartyAndDoubleClaimReturnsZero() public {
        address receiver = address(0xCAFE);
        _depositStake(alice, 1 ether);
        vm.warp(START_TIME + PERIOD_SECONDS);
        uint256 distributed = issuer.distribute();

        vm.prank(alice);
        assertEq(issuer.claim(receiver), distributed);
        assertEq(token.balanceOf(receiver), distributed);
        assertEq(issuer.scheduledUnclaimedRewards(), 0);

        vm.prank(alice);
        assertEq(issuer.claim(receiver), 0);
        assertEq(token.balanceOf(receiver), distributed);
    }

    function testBridgeEncodedSentryFactCanDriveIssuerRewardsEndToEnd() public {
        SyscoinZKSYSToken localToken = _deployToken();
        ZkSysMembershipRegistry localMembership = _deployMembershipRegistry(admin, address(0));
        ZkSysRewardWeightRegistry localRegistry = _deployWeightRegistry(admin, localMembership);
        ZkSysIssuer localIssuer = _deployIssuer(
            IZkSysMintableToken(address(localToken)),
            IZkSysRewardWeightSource(address(localRegistry)),
            admin,
            START_TIME,
            PERIOD_SECONDS,
            PERIODS_PER_YEAR
        );
        IssuerBridgehubMock bridgehub = new IssuerBridgehubMock();
        ZkSysRegistryBridge bridge =
            _deployRegistryBridge(bridgehub, 57, address(localMembership), 1_317_500, 210_240, 525_600, 3_500, 10_000);

        vm.startPrank(admin);
        localMembership.setL1RegistryBridge(address(bridge));
        localMembership.setSentryNodeReceiver(localRegistry);
        localRegistry.setWeightReceiver(localIssuer);
        localToken.grantRole(localToken.MINTER_ROLE(), address(localIssuer));
        vm.stopPrank();

        address[] memory accounts = new address[](1);
        accounts[0] = alice;
        vm.mockCall(NEVM_ADDRESS_PRECOMPILE, abi.encodePacked(alice), abi.encode(uint256(1_000)));

        vm.warp(START_TIME);
        bridge.pushSentryNodeUpdates(accounts, 1_000_000, 800, address(0));
        IZkSysMembershipRegistryL2.SentryNodeUpdate[] memory bridgeUpdates = bridgehub.lastDecodedUpdates();
        ZkSysMembershipRegistry.SentryNodeUpdate[] memory updates =
            new ZkSysMembershipRegistry.SentryNodeUpdate[](bridgeUpdates.length);
        for (uint256 i = 0; i < bridgeUpdates.length; ++i) {
            updates[i] = ZkSysMembershipRegistry.SentryNodeUpdate({
                account: bridgeUpdates[i].account,
                sentryNodeCollateralHeight: bridgeUpdates[i].sentryNodeCollateralHeight,
                sentryNodeWeight: bridgeUpdates[i].sentryNodeWeight
            });
        }

        vm.prank(localMembership.aliasedL1RegistryBridge());
        localMembership.applyL1SentryNodeUpdates(updates, ++observationHeight, uint64(block.timestamp));
        assertEq(localRegistry.weightOf(alice), 0);

        vm.warp(START_TIME + PERIOD_SECONDS);
        localRegistry.activatePendingWeightFor(alice);
        assertEq(localRegistry.weightOf(alice), 200_000 ether);

        vm.warp(START_TIME + 2 * PERIOD_SECONDS);
        uint256 distributed = localIssuer.distribute();

        vm.prank(alice);
        assertEq(localIssuer.claim(alice), distributed);
        assertEq(localToken.balanceOf(alice), distributed);
    }

    function testBoundaryStakeDoesNotEarnEndingPeriod() public {
        _depositStake(alice, 1 ether);

        vm.warp(START_TIME + PERIOD_SECONDS - 1);
        _depositStakePending(bob, 999 ether);

        vm.warp(START_TIME + PERIOD_SECONDS);
        uint256 firstDistribution = issuer.distribute();

        assertEq(issuer.pendingRewards(alice), firstDistribution);
        assertEq(issuer.pendingRewards(bob), 0);

        _activateStake(bob);

        vm.warp(START_TIME + 2 * PERIOD_SECONDS);
        uint256 secondDistribution = issuer.distribute();

        assertEq(secondDistribution, firstDistribution);
        assertEq(issuer.pendingRewards(alice), firstDistribution + secondDistribution / 1000);
        assertEq(issuer.pendingRewards(bob), secondDistribution * 999 / 1000);
    }

    function testStakeAfterBoundaryDoesNotEarnPreviousPeriod() public {
        _depositStake(alice, 1 ether);

        vm.warp(START_TIME + PERIOD_SECONDS + 1);
        _depositStakePending(bob, 999 ether);

        uint256 firstDistribution = issuer.distribute();

        assertEq(firstDistribution, yearOneEmission() / PERIODS_PER_YEAR);
        assertEq(issuer.pendingRewards(alice), firstDistribution);
        assertEq(issuer.pendingRewards(bob), 0);
    }

    function testWithdrawBeforeBoundaryDoesNotEarnEndingPeriod() public {
        _depositStake(alice, 1 ether);
        _depositStake(bob, 1 ether);

        vm.warp(START_TIME + PERIOD_SECONDS - 1);
        _withdrawStake(alice, 1 ether);

        vm.warp(START_TIME + PERIOD_SECONDS);
        uint256 firstDistribution = issuer.distribute();

        assertEq(firstDistribution, yearOneEmission() / PERIODS_PER_YEAR);
        assertEq(issuer.pendingRewards(alice), 0);
        assertEq(issuer.pendingRewards(bob), firstDistribution);
    }

    function testWithdrawAfterBoundaryEarnsCompletedPeriodThenStops() public {
        _depositStake(alice, 1 ether);
        _depositStake(bob, 1 ether);

        vm.warp(START_TIME + PERIOD_SECONDS + 1);
        _withdrawStake(alice, 1 ether);

        uint256 firstDistribution = yearOneEmission() / PERIODS_PER_YEAR;
        assertEq(issuer.pendingRewards(alice), firstDistribution / 2);
        assertEq(issuer.pendingRewards(bob), firstDistribution / 2);

        vm.warp(START_TIME + 2 * PERIOD_SECONDS);
        uint256 secondDistribution = issuer.distribute();

        assertEq(secondDistribution, issuer.cumulativeScheduledRewards(2) - issuer.cumulativeScheduledRewards(1));
        assertEq(issuer.pendingRewards(alice), firstDistribution / 2);
        assertEq(issuer.pendingRewards(bob), firstDistribution / 2 + secondDistribution);
    }

    function testActivatedStakeWithdrawBeforeNextBoundaryGetsNoPartialPeriodWindfall() public {
        _depositStake(alice, 1 ether);
        _depositStakePending(bob, 1 ether);

        vm.warp(START_TIME + PERIOD_SECONDS);
        _activateStake(bob);

        assertEq(issuer.pendingRewards(alice), yearOneEmission() / PERIODS_PER_YEAR);
        assertEq(issuer.pendingRewards(bob), 0);

        vm.warp(START_TIME + 2 * PERIOD_SECONDS - 1);
        _withdrawStake(bob, 1 ether);

        assertEq(issuer.pendingRewards(bob), 0);

        vm.warp(START_TIME + 2 * PERIOD_SECONDS);
        uint256 secondDistribution = issuer.distribute();

        assertEq(secondDistribution, issuer.cumulativeScheduledRewards(2) - issuer.cumulativeScheduledRewards(1));
        assertEq(issuer.pendingRewards(alice), issuer.cumulativeScheduledRewards(2));
        assertEq(issuer.pendingRewards(bob), 0);
    }

    function testActivatedStakeWithdrawAfterFullEpochGetsExactlyOneEpoch() public {
        _depositStake(alice, 1 ether);
        _depositStakePending(bob, 1 ether);

        vm.warp(START_TIME + PERIOD_SECONDS);
        _activateStake(bob);

        vm.warp(START_TIME + 2 * PERIOD_SECONDS + 1);
        _withdrawStake(bob, 1 ether);

        uint256 firstDistribution = issuer.cumulativeScheduledRewards(1);
        uint256 secondDistribution = issuer.cumulativeScheduledRewards(2) - issuer.cumulativeScheduledRewards(1);
        assertEq(issuer.pendingRewards(alice), firstDistribution + secondDistribution / 2);
        assertEq(issuer.pendingRewards(bob), secondDistribution / 2);

        vm.warp(START_TIME + 3 * PERIOD_SECONDS);
        uint256 thirdDistribution = issuer.distribute();

        assertEq(thirdDistribution, issuer.cumulativeScheduledRewards(3) - issuer.cumulativeScheduledRewards(2));
        assertEq(issuer.pendingRewards(alice), firstDistribution + secondDistribution / 2 + thirdDistribution);
        assertEq(issuer.pendingRewards(bob), secondDistribution / 2);
    }

    function testChurnOverAllocationDiscardsUnbackedDustInsteadOfReverting() public {
        address carol = address(0xCA20);
        address dave = address(0xDA7E);

        (uint256 alicePending, uint256 bobPending, uint256 carolPending, uint256 davePending) =
            _prepareClaimDustOverAllocation(carol, dave);

        vm.prank(alice);
        assertEq(issuer.claim(alice), alicePending);
        vm.prank(bob);
        assertEq(issuer.claim(bob), bobPending);
        vm.prank(carol);
        assertEq(issuer.claim(carol), carolPending);

        vm.prank(dave);
        assertEq(issuer.claim(dave), davePending - 1);
        assertEq(issuer.pendingRewards(dave), 0);

        vm.warp(START_TIME + 8 * PERIOD_SECONDS);
        issuer.distribute();

        uint256 daveRefilledPending = issuer.pendingRewards(dave);
        vm.prank(dave);
        assertEq(issuer.claim(dave), daveRefilledPending);
    }

    function testClaimCapAdversarialClaimOrdersNeverOvermint() public {
        address carol = address(0xCA20);
        address dave = address(0xDA7E);

        (uint256 alicePending, uint256 bobPending, uint256 carolPending, uint256 davePending) =
            _prepareClaimDustOverAllocation(carol, dave);
        uint256 backedRewards = issuer.scheduledUnclaimedRewards();
        uint256 scheduled = issuer.totalScheduledRewards();

        vm.prank(dave);
        assertEq(issuer.claim(dave), davePending);
        vm.prank(carol);
        assertEq(issuer.claim(carol), carolPending);
        vm.prank(bob);
        assertEq(issuer.claim(bob), bobPending);
        vm.prank(alice);
        assertEq(issuer.claim(alice), alicePending - 1);

        assertEq(token.totalSupply(), backedRewards);
        assertLe(token.totalSupply(), scheduled);
        assertEq(issuer.pendingRewards(alice), 0);
    }

    function testDelayedClaimAfterNextDistributionNeverExceedsTotalScheduledRewards() public {
        address carol = address(0xCA20);
        address dave = address(0xDA7E);

        _prepareClaimDustOverAllocation(carol, dave);

        vm.warp(START_TIME + 8 * PERIOD_SECONDS);
        issuer.distribute();

        vm.prank(alice);
        issuer.claim(alice);
        vm.prank(bob);
        issuer.claim(bob);
        vm.prank(carol);
        issuer.claim(carol);
        vm.prank(dave);
        issuer.claim(dave);

        assertLe(token.totalSupply(), issuer.totalScheduledRewards());
    }

    function testSentryNodeAddBeforeBoundaryDoesNotEarnEndingPeriod() public {
        _depositStake(alice, 1 ether);

        vm.warp(START_TIME + PERIOD_SECONDS - 1);
        _applyL1Update(bob, 1_000, 100_000 ether);

        vm.warp(START_TIME + PERIOD_SECONDS);
        uint256 firstDistribution = issuer.distribute();

        assertEq(issuer.pendingRewards(alice), firstDistribution);
        assertEq(issuer.pendingRewards(bob), 0);

        vm.warp(START_TIME + 2 * PERIOD_SECONDS);
        registry.activatePendingWeightFor(bob);

        assertEq(issuer.pendingRewards(alice), issuer.cumulativeScheduledRewards(2));
        assertEq(issuer.pendingRewards(bob), 0);
    }

    function testSentryNodeSeniorityIncreaseBeforeBoundaryDoesNotEarnEndingPeriodAtHigherWeight() public {
        _applyL1Update(alice, 1_000, 100_000 ether);
        _activateStake(alice);

        vm.warp(START_TIME + PERIOD_SECONDS);
        uint256 firstDistribution = issuer.distribute();

        vm.warp(START_TIME + 2 * PERIOD_SECONDS - 1);
        _applyL1Update(alice, 1_000, 200_000 ether);

        vm.warp(START_TIME + 2 * PERIOD_SECONDS);
        uint256 secondDistribution = issuer.distribute();

        assertEq(secondDistribution, issuer.cumulativeScheduledRewards(2) - issuer.cumulativeScheduledRewards(1));
        assertEq(issuer.pendingRewards(alice), firstDistribution + secondDistribution);

        registry.activatePendingWeightFor(alice);
        assertEq(issuer.pendingRewards(alice), firstDistribution + secondDistribution);
    }

    function testSentryNodeRemovalBeforeBoundaryIsExcludedImmediately() public {
        _applyL1Update(alice, 1_000, 100_000 ether);
        _applyL1Update(bob, 2_000, 100_000 ether);
        _activateStake(alice);
        _activateStake(bob);

        vm.warp(START_TIME + PERIOD_SECONDS - 1);
        _applyL1Update(alice, 0, 0);

        vm.warp(START_TIME + PERIOD_SECONDS);
        uint256 firstDistribution = issuer.distribute();

        assertEq(issuer.pendingRewards(alice), 0);
        assertEq(issuer.pendingRewards(bob), firstDistribution);
    }

    function testInitializerRejectsScheduleThatIsNotOneYear() public {
        ZkSysIssuer implementation = new ZkSysIssuer();

        vm.expectRevert(ZkSysIssuer.InvalidSchedule.selector);
        new ERC1967Proxy(
            address(implementation),
            abi.encodeCall(
                ZkSysIssuer.initialize,
                (
                    IZkSysMintableToken(address(token)),
                    IZkSysRewardWeightSource(address(registry)),
                    admin,
                    START_TIME,
                    1 days,
                    364
                )
            )
        );
    }

    function testInitializerRejectsStartTimeThatIsNotFuture() public {
        ZkSysIssuer implementation = new ZkSysIssuer();
        vm.warp(START_TIME);

        vm.expectRevert(ZkSysIssuer.InvalidSchedule.selector);
        new ERC1967Proxy(
            address(implementation),
            abi.encodeCall(
                ZkSysIssuer.initialize,
                (
                    IZkSysMintableToken(address(token)),
                    IZkSysRewardWeightSource(address(registry)),
                    admin,
                    START_TIME,
                    PERIOD_SECONDS,
                    PERIODS_PER_YEAR
                )
            )
        );
    }

    function testCumulativeScheduleUsesThreeYearBootstrapThenLongRunRate() public view {
        uint256 expectedYearOne = yearOneEmission();
        uint256 expectedYearTwo = remainingAfter(expectedYearOne) * 1_200 / 10_000;
        uint256 expectedYearThree = remainingAfter(expectedYearOne + expectedYearTwo) * 800 / 10_000;
        uint256 expectedYearFour = remainingAfter(expectedYearOne + expectedYearTwo + expectedYearThree) * 500 / 10_000;

        assertEq(issuer.cumulativeScheduledRewards(PERIODS_PER_YEAR), expectedYearOne);
        assertEq(issuer.cumulativeScheduledRewards(2 * PERIODS_PER_YEAR), expectedYearOne + expectedYearTwo);
        assertEq(
            issuer.cumulativeScheduledRewards(3 * PERIODS_PER_YEAR),
            expectedYearOne + expectedYearTwo + expectedYearThree
        );
        assertEq(
            issuer.cumulativeScheduledRewards(4 * PERIODS_PER_YEAR),
            expectedYearOne + expectedYearTwo + expectedYearThree + expectedYearFour
        );
    }

    function testServiceRegistrationAndRejectedReportsCannotDiluteQualifiedWeights() public {
        IssuerServiceSourceMock source = _configureService(0);
        _addSenior(alice, 135_000 ether);
        _addSenior(bob, 200_000 ether);
        source.admit(alice, 0, 35_000 ether);
        source.setFactor(alice, 0, 10_000);
        source.enroll(bob, true);
        source.enroll(bob, false);
        source.setFactor(bob, 0, 10_000);

        assertEq(registry.totalWeight(), 335_000 ether);
        assertEq(registry.totalPassiveWeight(), 200_000 ether);
        assertEq(source.totalAdmittedBonusWeight(0), 35_000 ether);
        vm.warp(START_TIME);
        assertEq(issuer.currentRewardDenominator(), 235_000 ether);
        assertEq(issuer.currentRewardWeight(alice), 135_000 ether);
        assertEq(issuer.currentRewardWeight(bob), 100_000 ether);
        vm.warp(START_TIME + PERIOD_SECONDS + 60);
        uint256 emitted = issuer.distribute();
        uint256 index = emitted * issuer.REWARD_PRECISION() / (235_000 ether);
        assertEq(issuer.servicePeriodRewardIndex(0), index);
        assertEq(issuer.pendingRewards(alice), 100_000 ether * index / issuer.REWARD_PRECISION());
        assertEq(issuer.pendingRewards(bob), issuer.pendingRewards(alice));
        assertEq(issuer.pendingServiceRewards(bob, 0), 0);
        assertEq(issuer.pendingServiceRewards(alice, 0), 35_000 ether * index / issuer.REWARD_PRECISION());
    }

    function testMissedAdmittedBonusExpiresWithoutSameRoundRedistributionOrCatchup() public {
        IssuerServiceSourceMock source = _configureService(0);
        _addSenior(alice, 135_000 ether);
        _addSenior(bob, 200_000 ether);
        source.admit(alice, 0, 35_000 ether);
        source.admit(bob, 0, 100_000 ether);
        source.admit(alice, 1, 35_000 ether);
        source.setFactor(alice, 0, 10_000);
        source.setFactor(alice, 1, 10_000);
        source.setFactor(bob, 0, 0);
        source.setFactor(bob, 1, 10_000);

        vm.warp(START_TIME + PERIOD_SECONDS + 60);
        uint256 firstEmission = issuer.distribute();
        uint256 firstIndex = firstEmission * issuer.REWARD_PRECISION() / (335_000 ether);
        assertEq(issuer.servicePeriodRewardIndex(0), firstIndex);
        uint256 unearned = 100_000 ether * firstIndex / issuer.REWARD_PRECISION();
        assertEq(_claimService(bob, 0), 0);
        assertEq(_claimService(alice, 0), 35_000 ether * firstIndex / issuer.REWARD_PRECISION());

        vm.prank(alice);
        issuer.claim(alice);
        vm.prank(bob);
        issuer.claim(bob);
        assertGe(firstEmission - token.totalSupply(), unearned);

        vm.warp(START_TIME + 2 * PERIOD_SECONDS + 60);
        uint256 secondEmission = issuer.distribute();
        uint256 secondIndex = secondEmission * issuer.REWARD_PRECISION() / (235_000 ether);
        // A later admission snapshot may change shares; it cannot reissue the previous shortfall.
        assertEq(issuer.servicePeriodRewardIndex(1), secondIndex);
        assertEq(_claimService(bob, 1), 0);
        _claimService(alice, 1);
        vm.prank(alice);
        issuer.claim(alice);
        vm.prank(bob);
        issuer.claim(bob);
        assertEq(issuer.totalScheduledRewards(), issuer.cumulativeScheduledRewards(2));
        assertGe(issuer.totalScheduledRewards() - token.totalSupply(), unearned);
        assertEq(issuer.pendingServiceRewards(bob, 0), 0);
    }

    function testServiceClaimRequiresFinalRoundAndCannotReplay() public {
        IssuerServiceSourceMock source = _configureService(0);
        _addSenior(alice, 135_000 ether);
        source.admit(alice, 0, 35_000 ether);
        source.setFactor(alice, 0, 5_000);
        vm.warp(START_TIME + PERIOD_SECONDS);
        issuer.distribute();
        vm.expectRevert(abi.encodeWithSelector(ZkSysIssuer.ServiceRoundNotFinalized.selector, 0));
        issuer.pendingServiceRewards(alice, 0);

        vm.warp(START_TIME + PERIOD_SECONDS + 60);
        uint256 expected = issuer.pendingServiceRewards(alice, 0);
        assertEq(_claimService(alice, 0), expected);
        uint256[] memory periods = new uint256[](1);
        periods[0] = 0;
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(ZkSysIssuer.ServiceRewardAlreadyClaimed.selector, 0));
        issuer.claimServiceRewards(periods, bob);
        assertEq(token.balanceOf(bob), 0);
    }

    function testDuplicateServiceClaimBatchRevertsWithoutPartialPayment() public {
        IssuerServiceSourceMock source = _configureService(0);
        _addSenior(alice, 135_000 ether);
        source.admit(alice, 0, 35_000 ether);
        source.setFactor(alice, 0, 10_000);
        vm.warp(START_TIME + PERIOD_SECONDS + 60);
        issuer.distribute();
        uint256[] memory periods = new uint256[](2);
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(ZkSysIssuer.ServiceRewardAlreadyClaimed.selector, 0));
        issuer.claimServiceRewards(periods, alice);
        assertFalse(issuer.serviceRewardClaimed(alice, 0));
        assertEq(token.totalSupply(), 0);
    }

    function testInvalidServiceFactorCannotMintOrConsumeClaim() public {
        IssuerServiceSourceMock source = _configureService(0);
        _addSenior(alice, 135_000 ether);
        source.admit(alice, 0, 35_000 ether);
        source.setFactor(alice, 0, 10_001);
        vm.warp(START_TIME + PERIOD_SECONDS + 60);
        issuer.distribute();
        uint256[] memory periods = new uint256[](1);
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(ZkSysIssuer.InvalidServiceFactor.selector, 10_001));
        issuer.claimServiceRewards(periods, alice);
        assertFalse(issuer.serviceRewardClaimed(alice, 0));
        assertEq(token.totalSupply(), 0);
        vm.prank(alice);
        assertGt(issuer.claim(alice), 0);
    }

    function testPastEarnedServiceSurvivesMembershipRemoval() public {
        IssuerServiceSourceMock source = _configureService(0);
        _addSenior(alice, 135_000 ether);
        _depositStake(bob, 1 ether);
        source.admit(alice, 0, 35_000 ether);
        source.setFactor(alice, 0, 10_000);
        vm.warp(START_TIME + PERIOD_SECONDS + 60);
        issuer.distribute();
        uint256 earned = issuer.pendingServiceRewards(alice, 0);
        uint256 passiveEarned = issuer.pendingRewards(alice);
        _applyL1Update(alice, 0, 0);
        assertEq(registry.weightOf(alice), 0);
        assertEq(registry.totalPassiveWeight(), 1 ether);
        assertEq(_claimService(alice, 0), earned);
        vm.prank(alice);
        assertEq(issuer.claim(alice), passiveEarned);
    }

    function testServiceWeightChangeSettlesOldPassiveDenominator() public {
        _configureService(0);
        _depositStake(alice, 1 ether);
        _depositStake(bob, 1 ether);
        vm.warp(START_TIME + PERIOD_SECONDS + 1);
        _withdrawStake(alice, 1 ether);
        uint256 first = issuer.cumulativeScheduledRewards(1);
        assertEq(issuer.pendingRewards(alice), first / 2);
        assertEq(issuer.pendingRewards(bob), first / 2);
        vm.warp(START_TIME + 2 * PERIOD_SECONDS);
        uint256 second = issuer.distribute();
        assertEq(issuer.pendingRewards(alice), first / 2);
        assertEq(issuer.pendingRewards(bob), first / 2 + second);
        assertEq(registry.totalPassiveWeight(), 1 ether);
    }

    function testServiceCheckpointBacklogIsBoundedAndNeverDroppedOnWeightChange() public {
        _configureService(0);
        _depositStake(alice, 1 ether);
        vm.warp(START_TIME + 70 * PERIOD_SECONDS);
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(ZkSysIssuer.ServiceCheckpointRequired.selector, 64, 70));
        stakingVault.withdraw(1 ether);
        assertEq(issuer.nextServicePeriod(), 0);
        assertEq(registry.totalPassiveWeight(), 1 ether);
        issuer.checkpointServicePeriods(64);
        assertEq(issuer.nextServicePeriod(), 64);
        assertEq(issuer.totalScheduledRewards(), issuer.cumulativeScheduledRewards(64));
        issuer.checkpointServicePeriods(6);
        _withdrawStake(alice, 1 ether);
        assertEq(issuer.nextServicePeriod(), 70);
        assertEq(issuer.totalScheduledRewards(), issuer.cumulativeScheduledRewards(70));
        vm.prank(alice);
        assertEq(issuer.claim(alice), issuer.cumulativeScheduledRewards(70));
    }

    function testServicePassiveChurnNeverBorrowsUnearnedBudget() public {
        _configureService(0);
        address carol = address(0xCA20);
        address dave = address(0xDA7E);
        (uint256 a, uint256 b, uint256 c, uint256 d) = _prepareClaimDustOverAllocation(carol, dave);
        assertLe(a + b + c + d, issuer.totalScheduledRewards());
        vm.prank(alice);
        assertEq(issuer.claim(alice), a);
        vm.prank(bob);
        assertEq(issuer.claim(bob), b);
        vm.prank(carol);
        assertEq(issuer.claim(carol), c);
        vm.prank(dave);
        assertEq(issuer.claim(dave), d);
        assertLe(token.totalSupply(), issuer.totalScheduledRewards());
    }

    function testServiceConfigurationRequiresCleanRegistryAndFutureMatchingPeriod() public {
        IssuerServiceSourceMock source = new IssuerServiceSourceMock(issuer, 0);
        _depositStake(alice, 1 ether);
        vm.prank(admin);
        vm.expectRevert(ZkSysRewardWeightRegistry.NonemptyServiceAccountingMigration.selector);
        issuer.configureServiceAccounting(IZkSysProverServiceSource(address(source)), 0);
        assertEq(issuer.serviceStartPeriodPlusOne(), 0);
        assertEq(registry.serviceStartPeriodPlusOne(), 0);

        _withdrawStake(alice, 1 ether);
        vm.prank(admin);
        vm.expectRevert(ZkSysIssuer.InvalidServiceConfiguration.selector);
        issuer.configureServiceAccounting(IZkSysProverServiceSource(address(source)), 1);
        vm.warp(START_TIME);
        vm.prank(admin);
        vm.expectRevert(ZkSysIssuer.InvalidServiceConfiguration.selector);
        issuer.configureServiceAccounting(IZkSysProverServiceSource(address(source)), 0);
    }

    function testServiceConfigurationIsOneWayAndCannotChangeEmissionSchedule() public {
        uint256 priorFourYears = issuer.cumulativeScheduledRewards(4 * PERIODS_PER_YEAR);
        uint256 supplyCap = token.maxSupply();
        IssuerServiceSourceMock source = _configureService(0);
        assertEq(issuer.YEAR_1_RATE_BPS(), 2_000);
        assertEq(issuer.YEAR_2_RATE_BPS(), 1_200);
        assertEq(issuer.YEAR_3_RATE_BPS(), 800);
        assertEq(issuer.LONG_RUN_RATE_BPS(), 500);
        assertEq(issuer.cumulativeScheduledRewards(4 * PERIODS_PER_YEAR), priorFourYears);
        assertEq(token.maxSupply(), supplyCap);
        vm.prank(admin);
        vm.expectRevert(ZkSysIssuer.ServiceAccountingAlreadyConfigured.selector);
        issuer.configureServiceAccounting(IZkSysProverServiceSource(address(source)), 0);
    }

    function testMissedServiceActivationCannotLockPassiveClaimsOrPrincipal() public {
        IssuerServiceSourceMock source = _configureService(2);
        source.setActive(false);
        _depositStake(alice, 1 ether);
        vm.warp(START_TIME + PERIOD_SECONDS);
        issuer.distribute();
        vm.warp(START_TIME + 2 * PERIOD_SECONDS);
        uint256 balanceBefore = alice.balance;
        _withdrawStake(alice, 1 ether);
        assertEq(alice.balance, balanceBefore + 1 ether);
        assertTrue(issuer.serviceLaunchAborted());
        assertTrue(issuer.serviceAccountingStarted());
        vm.prank(alice);
        assertEq(issuer.claim(alice), issuer.cumulativeScheduledRewards(2));

        source.setActive(true);
        source.admit(alice, 3, 100_000 ether);
        source.setFactor(alice, 3, 10_000);
        vm.warp(START_TIME + 4 * PERIOD_SECONDS + 60);
        issuer.distribute();
        assertEq(issuer.currentRewardDenominator(), 0);
        assertEq(issuer.currentRewardWeight(alice), 0);
        assertEq(_claimService(alice, 3), 0);
        assertEq(token.totalSupply(), issuer.cumulativeScheduledRewards(2));
    }

    function testOnlyMissedInactiveLaunchCanBeAbortedPermissionlessly() public {
        IssuerServiceSourceMock source = _configureService(0);
        source.setActive(false);
        vm.expectRevert(ZkSysIssuer.ServiceLaunchCannotAbort.selector);
        issuer.abortMissedServiceActivation();
        vm.warp(START_TIME);
        source.setActive(true);
        vm.expectRevert(ZkSysIssuer.ServiceLaunchCannotAbort.selector);
        issuer.abortMissedServiceActivation();
        source.setActive(false);
        vm.prank(bob);
        issuer.abortMissedServiceActivation();
        assertTrue(issuer.serviceLaunchAborted());
        assertEq(issuer.currentRewardDenominator(), 0);
        vm.expectRevert(ZkSysIssuer.ServiceLaunchCannotAbort.selector);
        issuer.abortMissedServiceActivation();
    }

    function testServiceActivationPreservesLegacyAccrualAndSeparatesBudgets() public {
        IssuerServiceSourceMock source = _configureService(2);
        _addSenior(alice, 135_000 ether);
        _addSenior(bob, 135_000 ether);
        source.admit(alice, 2, 35_000 ether);
        source.setFactor(alice, 2, 10_000);
        vm.warp(START_TIME + 3 * PERIOD_SECONDS + 60);
        issuer.distribute();
        uint256 legacy = issuer.cumulativeScheduledRewards(2);
        assertEq(issuer.legacyRewardBudget(), legacy);
        assertEq(issuer.serviceRewardBudget(), issuer.cumulativeScheduledRewards(3) - legacy);
        uint256 index = issuer.servicePeriodRewardIndex(2);
        uint256 passive = 100_000 ether * index / issuer.REWARD_PRECISION();
        uint256 legacyShare = 135_000 ether * issuer.serviceStartRewardIndex() / issuer.REWARD_PRECISION();
        assertEq(issuer.pendingRewards(alice), legacyShare + passive);
        assertEq(issuer.pendingRewards(bob), legacyShare + passive);
        vm.prank(alice);
        issuer.claim(alice);
        vm.prank(bob);
        issuer.claim(bob);
        _claimService(alice, 2);
        assertEq(issuer.legacyRewardBudget(), legacy - 2 * legacyShare);
        assertLe(token.totalSupply(), issuer.totalScheduledRewards());
        assertEq(issuer.scheduledUnclaimedRewards(), issuer.legacyRewardBudget() + issuer.serviceRewardBudget());
    }

    function testLegacyChurnDustCannotSpendTheNewServiceBudget() public {
        _configureService(7);
        address carol = address(0xCA20);
        address dave = address(0xDA7E);
        _prepareClaimDustOverAllocation(carol, dave);
        uint256 legacyBudget = issuer.legacyRewardBudget();
        vm.warp(START_TIME + 8 * PERIOD_SECONDS);
        uint256 emitted = issuer.distribute();
        uint256 index = issuer.servicePeriodRewardIndex(7);
        uint256 passiveTotal = (271 ether * index / issuer.REWARD_PRECISION())
            + (189 ether * index / issuer.REWARD_PRECISION()) + (116 ether * index / issuer.REWARD_PRECISION())
            + (422 ether * index / issuer.REWARD_PRECISION());
        vm.prank(dave);
        issuer.claim(dave);
        vm.prank(carol);
        issuer.claim(carol);
        vm.prank(bob);
        issuer.claim(bob);
        vm.prank(alice);
        issuer.claim(alice);
        assertEq(issuer.legacyRewardBudget(), 0);
        assertEq(token.totalSupply(), legacyBudget + passiveTotal);
        assertEq(issuer.serviceRewardBudget(), emitted - passiveTotal);
    }

    function testFuzzServiceMintBudgetPreservesEveryUnearnedQuota(
        uint96 aliceStake,
        uint96 bobStake,
        uint16 aliceFactor,
        uint16 bobFactor,
        bool admitBob,
        bool serviceFirst
    ) public {
        IssuerServiceSourceMock source = _configureService(0);
        _addSenior(alice, 135_000 ether);
        _addSenior(bob, 200_000 ether);
        _depositStake(alice, bound(uint256(aliceStake), 1, 1_000_000 ether));
        _depositStake(bob, bound(uint256(bobStake), 1, 1_000_000 ether));
        uint256 aFactor = bound(uint256(aliceFactor), 0, 10_000);
        uint256 bFactor = bound(uint256(bobFactor), 0, 10_000);
        source.admit(alice, 0, 35_000 ether);
        if (admitBob) {
            source.admit(bob, 0, 100_000 ether);
        }
        source.setFactor(alice, 0, aFactor);
        source.setFactor(bob, 0, bFactor);
        vm.warp(START_TIME + PERIOD_SECONDS + 60);
        uint256 emitted = issuer.distribute();
        uint256 aService = issuer.pendingServiceRewards(alice, 0);
        uint256 bService = issuer.pendingServiceRewards(bob, 0);
        uint256 aPassive = issuer.pendingRewards(alice);
        uint256 bPassive = issuer.pendingRewards(bob);
        if (serviceFirst) {
            assertEq(_claimService(alice, 0), aService);
            assertEq(_claimService(bob, 0), bService);
        }
        vm.prank(alice);
        assertEq(issuer.claim(alice), aPassive);
        vm.prank(bob);
        assertEq(issuer.claim(bob), bPassive);
        if (!serviceFirst) {
            assertEq(_claimService(alice, 0), aService);
            assertEq(_claimService(bob, 0), bService);
        }
        uint256 index = issuer.servicePeriodRewardIndex(0);
        uint256 aQuota = 35_000 ether * index / issuer.REWARD_PRECISION();
        uint256 bQuota = admitBob ? 100_000 ether * index / issuer.REWARD_PRECISION() : 0;
        assertLe(token.totalSupply(), emitted);
        assertGe(emitted - token.totalSupply(), aQuota - aService + bQuota - bService);
        assertEq(token.totalSupply() + issuer.serviceRewardBudget(), emitted);
    }

    function _configureService(uint256 activationPeriod) private returns (IssuerServiceSourceMock source) {
        source = new IssuerServiceSourceMock(issuer, activationPeriod);
        vm.prank(admin);
        issuer.configureServiceAccounting(IZkSysProverServiceSource(address(source)), activationPeriod);
    }

    function _addSenior(address account, uint128 weight) private {
        _applyL1Update(account, 1_000, weight);
        _activateStake(account);
    }

    function _claimService(address account, uint256 period) private returns (uint256) {
        uint256[] memory periods = new uint256[](1);
        periods[0] = period;
        vm.prank(account);
        return issuer.claimServiceRewards(periods, account);
    }

    function yearOneEmission() private view returns (uint256) {
        return token.maxSupply() * 2_000 / 10_000;
    }

    function remainingAfter(uint256 scheduledRewards) private view returns (uint256) {
        return token.maxSupply() - scheduledRewards;
    }

    function _prepareClaimDustOverAllocation(address carol, address dave)
        private
        returns (uint256 alicePending, uint256 bobPending, uint256 carolPending, uint256 davePending)
    {
        vm.warp(START_TIME);
        _depositStakePending(carol, 116 ether);

        vm.warp(START_TIME + PERIOD_SECONDS);
        _activateStake(carol);
        _depositStakePending(bob, 189 ether);

        vm.warp(START_TIME + 2 * PERIOD_SECONDS);
        _activateStake(bob);

        vm.warp(START_TIME + 4 * PERIOD_SECONDS);
        _depositStakePending(alice, 271 ether);

        vm.warp(START_TIME + 5 * PERIOD_SECONDS);
        _depositStakePending(dave, 176 ether);
        _depositStakePending(dave, 246 ether);

        vm.warp(START_TIME + 6 * PERIOD_SECONDS);
        _activateStake(alice);
        _activateStake(dave);

        vm.warp(START_TIME + 7 * PERIOD_SECONDS);
        issuer.distribute();

        alicePending = issuer.pendingRewards(alice);
        bobPending = issuer.pendingRewards(bob);
        carolPending = issuer.pendingRewards(carol);
        davePending = issuer.pendingRewards(dave);
        uint256 scheduledUnclaimed = issuer.scheduledUnclaimedRewards();

        if (issuer.serviceAccountingStarted() && issuer.serviceStartRewardIndex() == 0) {
            assertLe(alicePending + bobPending + carolPending + davePending, scheduledUnclaimed);
        } else {
            assertEq(alicePending + bobPending + carolPending + davePending, scheduledUnclaimed + 1);
        }
    }

    function _depositStake(address account, uint256 amount) private {
        _depositStakePending(account, amount);
        _activateStake(account);
    }

    function _depositStakePending(address account, uint256 amount) private {
        vm.deal(account, account.balance + amount);
        vm.prank(account);
        stakingVault.deposit{value: amount}();
    }

    function _activateStake(address account) private {
        vm.prank(account);
        registry.activatePendingWeight();
    }

    function _withdrawStake(address account, uint256 amount) private {
        vm.prank(account);
        stakingVault.withdraw(amount);
    }

    function _applyL1Update(address account, uint32 sentryNodeCollateralHeight, uint128 sentryNodeWeight) private {
        ZkSysMembershipRegistry.SentryNodeUpdate[] memory updates = new ZkSysMembershipRegistry.SentryNodeUpdate[](1);
        updates[0] = ZkSysMembershipRegistry.SentryNodeUpdate({
            account: account, sentryNodeCollateralHeight: sentryNodeCollateralHeight, sentryNodeWeight: sentryNodeWeight
        });
        vm.prank(membershipRegistry.aliasedL1RegistryBridge());
        membershipRegistry.applyL1SentryNodeUpdates(updates, ++observationHeight, uint64(block.timestamp));
    }

    function _deployToken() private returns (SyscoinZKSYSToken) {
        SyscoinZKSYSToken implementation = new SyscoinZKSYSToken();
        ERC1967Proxy proxy = new ERC1967Proxy(
            address(implementation), abi.encodeCall(SyscoinZKSYSToken.initialize, ("ZKSYS", "ZKSYS", uint8(18), admin))
        );
        return SyscoinZKSYSToken(address(proxy));
    }

    function _deployMembershipRegistry(address admin_, address l1RegistryBridge_)
        private
        returns (ZkSysMembershipRegistry)
    {
        ZkSysMembershipRegistry implementation = new ZkSysMembershipRegistry();
        ERC1967Proxy proxy = new ERC1967Proxy(
            address(implementation), abi.encodeCall(ZkSysMembershipRegistry.initialize, (admin_, l1RegistryBridge_))
        );
        return ZkSysMembershipRegistry(address(proxy));
    }

    function _deployWeightRegistry(address admin_, ZkSysMembershipRegistry membershipRegistry_)
        private
        returns (ZkSysRewardWeightRegistry)
    {
        ZkSysRewardWeightRegistry implementation = new ZkSysRewardWeightRegistry();
        ERC1967Proxy proxy = new ERC1967Proxy(
            address(implementation),
            abi.encodeCall(
                ZkSysRewardWeightRegistry.initialize, (admin_, membershipRegistry_, ACTIVATION_DELAY_PERIODS)
            )
        );
        return ZkSysRewardWeightRegistry(address(proxy));
    }

    function _deployStakingVault(IZkSysStakeWeightRegistry weightRegistry_) private returns (ZkSysNativeStakingVault) {
        ZkSysNativeStakingVault implementation = new ZkSysNativeStakingVault();
        ERC1967Proxy proxy = new ERC1967Proxy(
            address(implementation), abi.encodeCall(ZkSysNativeStakingVault.initialize, (weightRegistry_))
        );
        return ZkSysNativeStakingVault(payable(address(proxy)));
    }

    function _deployIssuer(
        IZkSysMintableToken token_,
        IZkSysRewardWeightSource registry_,
        address admin_,
        uint256 startTime_,
        uint256 periodSeconds_,
        uint256 periodsPerYear_
    ) private returns (ZkSysIssuer) {
        ZkSysIssuer implementation = new ZkSysIssuer();
        ERC1967Proxy proxy = new ERC1967Proxy(
            address(implementation),
            abi.encodeCall(
                ZkSysIssuer.initialize, (token_, registry_, admin_, startTime_, periodSeconds_, periodsPerYear_)
            )
        );
        return ZkSysIssuer(address(proxy));
    }

    function _deployRegistryBridge(
        IL1BridgehubMinimal bridgehub_,
        uint256 zksysChainId_,
        address l2Registry_,
        uint32 nevmStartBlock_,
        uint32 seniorityHeight1_,
        uint32 seniorityHeight2_,
        uint16 seniorityLevel1Bps_,
        uint16 seniorityLevel2Bps_
    ) private returns (ZkSysRegistryBridge) {
        ZkSysRegistryBridge implementation = new ZkSysRegistryBridge();
        ERC1967Proxy proxy = new ERC1967Proxy(
            address(implementation),
            abi.encodeCall(
                ZkSysRegistryBridge.initialize,
                (
                    bridgehub_,
                    zksysChainId_,
                    l2Registry_,
                    nevmStartBlock_,
                    seniorityHeight1_,
                    seniorityHeight2_,
                    seniorityLevel1Bps_,
                    seniorityLevel2Bps_
                )
            )
        );
        return ZkSysRegistryBridge(address(proxy));
    }
}
