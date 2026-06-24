// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {ERC1967Proxy} from "@openzeppelin/contracts-v4/proxy/ERC1967/ERC1967Proxy.sol";
import {Test} from "forge-std/Test.sol";
import {SyscoinZKSYSToken} from "contracts/src/zksys/SyscoinZKSYSToken.sol";
import {IZkSysMintableToken, IZkSysRewardWeightSource, ZkSysIssuer} from "contracts/src/zksys/ZkSysIssuer.sol";
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

    function lastDecodedUpdates()
        external
        view
        returns (IZkSysMembershipRegistryL2.SentryNodeUpdate[] memory updates)
    {
        updates = abi.decode(_withoutSelector(lastRequest.l2Calldata), (IZkSysMembershipRegistryL2.SentryNodeUpdate[]));
    }

    function _withoutSelector(bytes memory data) private pure returns (bytes memory result) {
        result = new bytes(data.length - 4);
        for (uint256 i = 4; i < data.length; ++i) {
            result[i - 4] = data[i];
        }
    }
}

contract ZkSysIssuerTest is Test {
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
            new ZkSysRegistryBridge(bridgehub, 57, address(localMembership), 1_317_500, 210_240, 525_600, 3_500, 10_000);

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
        localMembership.applyL1SentryNodeUpdates(updates);
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

    function testChurnOverAllocationCarriesForwardDustInsteadOfReverting() public {
        address carol = address(0xCA20);
        address dave = address(0xDA7E);

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

        uint256 alicePending = issuer.pendingRewards(alice);
        uint256 bobPending = issuer.pendingRewards(bob);
        uint256 carolPending = issuer.pendingRewards(carol);
        uint256 davePending = issuer.pendingRewards(dave);
        uint256 scheduledUnclaimed = issuer.scheduledUnclaimedRewards();

        assertEq(alicePending + bobPending + carolPending + davePending, scheduledUnclaimed + 1);

        vm.prank(alice);
        assertEq(issuer.claim(alice), alicePending);
        vm.prank(bob);
        assertEq(issuer.claim(bob), bobPending);
        vm.prank(carol);
        assertEq(issuer.claim(carol), carolPending);

        vm.prank(dave);
        assertEq(issuer.claim(dave), davePending - 1);
        assertEq(issuer.pendingRewards(dave), 1);

        vm.warp(START_TIME + 8 * PERIOD_SECONDS);
        issuer.distribute();

        uint256 daveRefilledPending = issuer.pendingRewards(dave);
        vm.prank(dave);
        assertEq(issuer.claim(dave), daveRefilledPending);
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

    function yearOneEmission() private view returns (uint256) {
        return token.maxSupply() * 2_000 / 10_000;
    }

    function remainingAfter(uint256 scheduledRewards) private view returns (uint256) {
        return token.maxSupply() - scheduledRewards;
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
            account: account,
            sentryNodeCollateralHeight: sentryNodeCollateralHeight,
            sentryNodeWeight: sentryNodeWeight
        });
        vm.prank(membershipRegistry.aliasedL1RegistryBridge());
        membershipRegistry.applyL1SentryNodeUpdates(updates);
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
}
