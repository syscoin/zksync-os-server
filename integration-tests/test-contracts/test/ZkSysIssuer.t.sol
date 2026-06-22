// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {ERC1967Proxy} from "@openzeppelin/contracts-v4/proxy/ERC1967/ERC1967Proxy.sol";
import {Test} from "forge-std/Test.sol";
import {SyscoinZKSYSToken} from "contracts/src/zksys/SyscoinZKSYSToken.sol";
import {IZkSysMintableToken, IZkSysRewardWeightSource, ZkSysIssuer} from "contracts/src/zksys/ZkSysIssuer.sol";
import {ZkSysMembershipRegistry} from "contracts/src/zksys/ZkSysMembershipRegistry.sol";
import {IZkSysStakeWeightRegistry, ZkSysNativeStakingVault} from "contracts/src/zksys/ZkSysNativeStakingVault.sol";
import {ZkSysRewardWeightRegistry} from "contracts/src/zksys/ZkSysRewardWeightRegistry.sol";

contract ZkSysIssuerTest is Test {
    uint256 private constant START_TIME = 1_000;
    uint256 private constant PERIOD_SECONDS = 1 days;
    uint256 private constant PERIODS_PER_YEAR = 365;

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

        _depositStake(bob, 1 ether);

        vm.warp(START_TIME + 2 * PERIOD_SECONDS);
        uint256 secondDistribution = issuer.distribute();

        assertEq(secondDistribution, firstDistribution);
        assertEq(issuer.pendingRewards(alice), firstDistribution + secondDistribution / 2);
        assertEq(issuer.pendingRewards(bob), secondDistribution / 2);
    }

    function testLateWeightIncreaseDoesNotEarnUndistributedBacklog() public {
        _depositStake(alice, 1 ether);

        vm.warp(START_TIME + PERIOD_SECONDS);
        uint256 firstDistribution = issuer.distribute();

        vm.warp(START_TIME + 3 * PERIOD_SECONDS);
        _depositStake(bob, 1 ether);

        uint256 twoPeriodBacklog = issuer.cumulativeScheduledRewards(3) - issuer.cumulativeScheduledRewards(1);
        assertEq(issuer.pendingRewards(alice), firstDistribution + twoPeriodBacklog);
        assertEq(issuer.pendingRewards(bob), 0);

        vm.warp(START_TIME + 4 * PERIOD_SECONDS);
        uint256 fourthPeriodDistribution = issuer.distribute();

        assertEq(fourthPeriodDistribution, issuer.cumulativeScheduledRewards(4) - issuer.cumulativeScheduledRewards(3));
        assertEq(issuer.pendingRewards(alice), firstDistribution + twoPeriodBacklog + fourthPeriodDistribution / 2);
        assertEq(issuer.pendingRewards(bob), fourthPeriodDistribution / 2);
    }

    function testFirstWeightAfterStartDoesNotEarnEmptyRegistryBacklog() public {
        vm.warp(START_TIME + 2 * PERIOD_SECONDS);

        _depositStake(alice, 1 ether);

        assertEq(issuer.pendingRewards(alice), 0);
        assertEq(issuer.totalScheduledRewards(), 2 * yearOneEmission() / PERIODS_PER_YEAR);
        assertEq(issuer.scheduledUnclaimedRewards(), 0);

        vm.warp(START_TIME + 3 * PERIOD_SECONDS);
        uint256 distribution = issuer.distribute();

        assertEq(distribution, yearOneEmission() / PERIODS_PER_YEAR);
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
        _depositStake(bob, 1 ether);

        assertEq(issuer.totalScheduledRewards(), issuer.cumulativeScheduledRewards(5));
        assertEq(issuer.pendingRewards(bob), 0);

        vm.warp(START_TIME + 6 * PERIOD_SECONDS);
        uint256 sixthPeriodDistribution = issuer.distribute();

        assertEq(sixthPeriodDistribution, issuer.cumulativeScheduledRewards(6) - issuer.cumulativeScheduledRewards(5));
        assertEq(issuer.pendingRewards(bob), sixthPeriodDistribution);
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

    function testInitializerRejectsScheduleThatIsNotOneYear() public {
        ZkSysIssuer implementation = new ZkSysIssuer();

        vm.expectRevert(ZkSysIssuer.InvalidSchedule.selector);
        new ERC1967Proxy(
            address(implementation),
            abi.encodeCall(
                ZkSysIssuer.initialize,
                (IZkSysMintableToken(address(token)), IZkSysRewardWeightSource(address(registry)), admin, START_TIME, 1 days, 364)
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
        vm.deal(account, account.balance + amount);
        vm.prank(account);
        stakingVault.deposit{value: amount}();
    }

    function _withdrawStake(address account, uint256 amount) private {
        vm.prank(account);
        stakingVault.withdraw(amount);
    }

    function _deployMembershipRegistry(address admin_, address l1RegistryBridge_) private returns (ZkSysMembershipRegistry) {
        ZkSysMembershipRegistry implementation = new ZkSysMembershipRegistry();
        ERC1967Proxy proxy = new ERC1967Proxy(
            address(implementation), abi.encodeCall(ZkSysMembershipRegistry.initialize, (admin_, l1RegistryBridge_))
        );
        return ZkSysMembershipRegistry(address(proxy));
    }

    function _deployWeightRegistry(
        address admin_,
        ZkSysMembershipRegistry membershipRegistry_
    ) private returns (ZkSysRewardWeightRegistry) {
        ZkSysRewardWeightRegistry implementation = new ZkSysRewardWeightRegistry();
        ERC1967Proxy proxy = new ERC1967Proxy(
            address(implementation),
            abi.encodeCall(ZkSysRewardWeightRegistry.initialize, (admin_, membershipRegistry_))
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
                ZkSysIssuer.initialize,
                (token_, registry_, admin_, startTime_, periodSeconds_, periodsPerYear_)
            )
        );
        return ZkSysIssuer(address(proxy));
    }
}
