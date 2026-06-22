// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {ERC1967Proxy} from "@openzeppelin/contracts-v4/proxy/ERC1967/ERC1967Proxy.sol";
import {Test} from "forge-std/Test.sol";
import {ZkSysMembershipRegistry} from "contracts/src/zksys/ZkSysMembershipRegistry.sol";
import {IZkSysWeightReceiver, ZkSysRewardWeightRegistry} from "contracts/src/zksys/ZkSysRewardWeightRegistry.sol";

contract MockRewardWeightReceiver is IZkSysWeightReceiver {
    address public lastAccount;
    uint256 public lastOldWeight;
    uint256 public lastNewWeight;
    uint256 public lastOldTotalWeight;
    uint256 public startTime = 1_000;
    uint256 public currentPeriod;

    function setCurrentPeriod(uint256 currentPeriod_) external {
        currentPeriod = currentPeriod_;
    }

    function onWeightChange(address account, uint256 oldWeight, uint256 newWeight, uint256 oldTotalWeight) external {
        lastAccount = account;
        lastOldWeight = oldWeight;
        lastNewWeight = newWeight;
        lastOldTotalWeight = oldTotalWeight;
    }
}

contract ZkSysRewardWeightRegistryTest is Test {
    uint256 private constant ACTIVATION_DELAY_PERIODS = 1;
    uint128 private constant DEFAULT_SENTRY_NODE_WEIGHT = 100_000 ether;

    address private admin = address(0xAD);
    address private stakeWeightUpdater = address(0x57A7E);
    address private l1Bridge = address(0xB111D6E);
    address private alice = address(0xA11CE);
    address private bob = address(0xB0B);

    ZkSysMembershipRegistry private membershipRegistry;
    ZkSysRewardWeightRegistry private weightRegistry;
    MockRewardWeightReceiver private receiver;

    function setUp() public {
        membershipRegistry = _deployMembershipRegistry(admin, l1Bridge);
        weightRegistry = _deployWeightRegistry(admin, membershipRegistry);
        receiver = new MockRewardWeightReceiver();

        vm.startPrank(admin);
        membershipRegistry.setSentryNodeReceiver(weightRegistry);
        weightRegistry.setWeightReceiver(receiver);
        weightRegistry.grantRole(weightRegistry.STAKE_WEIGHT_UPDATER_ROLE(), stakeWeightUpdater);
        vm.stopPrank();
    }

    function testUpdaterCanUpdateStakeWeightSeparatelyFromSentryNodeWeight() public {
        vm.prank(stakeWeightUpdater);
        weightRegistry.updateStakeWeight(alice, 2);

        assertEq(weightRegistry.weightOf(alice), 0);
        assertEq(weightRegistry.totalWeight(), 0);

        receiver.setCurrentPeriod(ACTIVATION_DELAY_PERIODS);
        vm.prank(alice);
        weightRegistry.activatePendingWeight();

        assertEq(weightRegistry.weightOf(alice), 2);
        assertEq(weightRegistry.totalWeight(), 2);
        assertEq(receiver.lastAccount(), alice);
        assertEq(receiver.lastOldWeight(), 0);
        assertEq(receiver.lastNewWeight(), 2);
        assertEq(receiver.lastOldTotalWeight(), 0);
    }

    function testL1SentryNodeFactAddsAndRemovesSentryNodeWeight() public {
        vm.startPrank(membershipRegistry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 1_000);
        vm.stopPrank();

        assertEq(weightRegistry.weightOf(alice), 0);
        assertEq(weightRegistry.totalWeight(), 0);

        receiver.setCurrentPeriod(ACTIVATION_DELAY_PERIODS);
        vm.prank(alice);
        weightRegistry.activatePendingWeight();

        assertEq(weightRegistry.weightOf(alice), DEFAULT_SENTRY_NODE_WEIGHT);
        assertEq(weightRegistry.totalWeight(), DEFAULT_SENTRY_NODE_WEIGHT);

        vm.startPrank(membershipRegistry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 0);
        vm.stopPrank();

        assertEq(weightRegistry.weightOf(alice), 0);
        assertEq(weightRegistry.totalWeight(), 0);
        assertEq(receiver.lastAccount(), alice);
        assertEq(receiver.lastOldWeight(), DEFAULT_SENTRY_NODE_WEIGHT);
        assertEq(receiver.lastNewWeight(), 0);
        assertEq(receiver.lastOldTotalWeight(), DEFAULT_SENTRY_NODE_WEIGHT);
    }

    function testL1AddressChangeIsRemoveOldAndAddNewWeight() public {
        vm.startPrank(membershipRegistry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 1_000);
        _applyL1Update(bob, 2_000);
        vm.stopPrank();

        receiver.setCurrentPeriod(ACTIVATION_DELAY_PERIODS);
        vm.prank(alice);
        weightRegistry.activatePendingWeight();
        vm.prank(bob);
        weightRegistry.activatePendingWeight();

        vm.prank(membershipRegistry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 0);

        assertEq(weightRegistry.weightOf(alice), 0);
        assertEq(weightRegistry.weightOf(bob), DEFAULT_SENTRY_NODE_WEIGHT);
        assertEq(weightRegistry.totalWeight(), DEFAULT_SENTRY_NODE_WEIGHT);
    }

    function testStakeWeightSurvivesSentryNodeRemoval() public {
        vm.prank(stakeWeightUpdater);
        weightRegistry.updateStakeWeight(alice, 7);
        receiver.setCurrentPeriod(ACTIVATION_DELAY_PERIODS);
        vm.prank(alice);
        weightRegistry.activatePendingWeight();

        vm.startPrank(membershipRegistry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 1_000);
        vm.stopPrank();
        receiver.setCurrentPeriod(2 * ACTIVATION_DELAY_PERIODS);
        vm.prank(alice);
        weightRegistry.activatePendingWeight();

        vm.startPrank(membershipRegistry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 0);
        vm.stopPrank();

        assertEq(weightRegistry.weightOf(alice), 7);
        assertEq(weightRegistry.totalWeight(), 7);
    }

    function testOnlyMembershipRegistryCanUpdateSentryNodeWeight() public {
        address unauthorized = address(uint160(address(membershipRegistry)) + 1);
        uint128 sentryNodeWeight = DEFAULT_SENTRY_NODE_WEIGHT;
        vm.startPrank(unauthorized);
        vm.expectRevert(ZkSysRewardWeightRegistry.UnauthorizedMembershipRegistry.selector);
        weightRegistry.onSentryNodeStatusChange(alice, 0, 1_000, 0, sentryNodeWeight);
        vm.stopPrank();
    }

    function testL1SentryNodeSeniorityWeightUpdatesWithoutHeightChange() public {
        vm.prank(membershipRegistry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 1_000, 100_000 ether);
        receiver.setCurrentPeriod(ACTIVATION_DELAY_PERIODS);
        vm.prank(alice);
        weightRegistry.activatePendingWeight();

        vm.prank(membershipRegistry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 1_000, 135_000 ether);

        assertEq(weightRegistry.weightOf(alice), 100_000 ether);

        receiver.setCurrentPeriod(2 * ACTIVATION_DELAY_PERIODS);
        vm.prank(alice);
        weightRegistry.activatePendingWeight();

        assertEq(weightRegistry.weightOf(alice), 135_000 ether);
        assertEq(weightRegistry.totalWeight(), 135_000 ether);
        assertEq(receiver.lastAccount(), alice);
        assertEq(receiver.lastOldWeight(), 100_000 ether);
        assertEq(receiver.lastNewWeight(), 135_000 ether);
        assertEq(receiver.lastOldTotalWeight(), 100_000 ether);
    }

    function testDuplicateSentryNodeIncreaseDoesNotResetPendingEffectivePeriod() public {
        vm.warp(receiver.startTime());
        vm.prank(membershipRegistry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 1_000, 100_000 ether);

        ZkSysRewardWeightRegistry.PendingWeightView memory pending = weightRegistry.pendingWeightComponents(alice);
        assertEq(pending.sentryNodeWeight, 100_000 ether);
        assertEq(pending.sentryNodeEffectivePeriod, 1);

        receiver.setCurrentPeriod(1);
        vm.prank(membershipRegistry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 1_000, 100_000 ether);

        pending = weightRegistry.pendingWeightComponents(alice);
        assertEq(pending.sentryNodeWeight, 100_000 ether);
        assertEq(pending.sentryNodeEffectivePeriod, 1);

        vm.prank(alice);
        weightRegistry.activatePendingWeight();

        assertEq(weightRegistry.weightOf(alice), 100_000 ether);
        assertEq(weightRegistry.totalWeight(), 100_000 ether);
    }

    function testLowerPendingSentryNodeIncreaseKeepsOriginalEffectivePeriod() public {
        vm.warp(receiver.startTime());
        vm.prank(membershipRegistry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 1_000, 135_000 ether);

        ZkSysRewardWeightRegistry.PendingWeightView memory pending = weightRegistry.pendingWeightComponents(alice);
        assertEq(pending.sentryNodeWeight, 135_000 ether);
        assertEq(pending.sentryNodeEffectivePeriod, 1);

        receiver.setCurrentPeriod(1);
        vm.prank(membershipRegistry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 1_000, 100_000 ether);

        pending = weightRegistry.pendingWeightComponents(alice);
        assertEq(pending.sentryNodeWeight, 100_000 ether);
        assertEq(pending.sentryNodeEffectivePeriod, 1);

        vm.prank(alice);
        weightRegistry.activatePendingWeight();

        assertEq(weightRegistry.weightOf(alice), 100_000 ether);
        assertEq(weightRegistry.totalWeight(), 100_000 ether);
    }

    function testOnlyStakeWeightUpdaterCanUpdateStakeWeight() public {
        vm.expectRevert(_accessControlRevert(alice, weightRegistry.STAKE_WEIGHT_UPDATER_ROLE()));
        vm.prank(alice);
        weightRegistry.updateStakeWeight(alice, 1);
    }

    function testWeightMustFitUint128() public {
        vm.expectRevert(
            abi.encodeWithSelector(ZkSysRewardWeightRegistry.InvalidWeight.selector, uint256(type(uint128).max) + 1)
        );
        vm.prank(stakeWeightUpdater);
        weightRegistry.updateStakeWeight(alice, uint256(type(uint128).max) + 1);
    }

    function testPendingWeightCannotActivateBeforeEffectivePeriod() public {
        vm.warp(receiver.startTime());
        vm.prank(stakeWeightUpdater);
        weightRegistry.updateStakeWeight(alice, 2);

        vm.prank(alice);
        vm.expectRevert(
            abi.encodeWithSelector(ZkSysRewardWeightRegistry.PendingWeightNotEffective.selector, ACTIVATION_DELAY_PERIODS, 0)
        );
        weightRegistry.activatePendingWeight();
    }

    function testDecreaseClearsPendingStakeIncreaseImmediately() public {
        vm.prank(stakeWeightUpdater);
        weightRegistry.updateStakeWeight(alice, 10);

        vm.prank(stakeWeightUpdater);
        weightRegistry.updateStakeWeight(alice, 0);

        receiver.setCurrentPeriod(ACTIVATION_DELAY_PERIODS);
        vm.prank(alice);
        vm.expectRevert(ZkSysRewardWeightRegistry.NoPendingWeight.selector);
        weightRegistry.activatePendingWeight();

        assertEq(weightRegistry.weightOf(alice), 0);
        assertEq(weightRegistry.totalWeight(), 0);
    }

    function testWithdrawAfterPendingMaturesDoesNotResetStakeEffectivePeriod() public {
        vm.warp(receiver.startTime());
        vm.prank(stakeWeightUpdater);
        weightRegistry.updateStakeWeight(alice, 10);

        receiver.setCurrentPeriod(ACTIVATION_DELAY_PERIODS);
        vm.prank(stakeWeightUpdater);
        weightRegistry.updateStakeWeight(alice, 6);

        ZkSysRewardWeightRegistry.PendingWeightView memory pending = weightRegistry.pendingWeightComponents(alice);
        assertEq(pending.stakeWeight, 6);
        assertEq(pending.stakeEffectivePeriod, ACTIVATION_DELAY_PERIODS);
        assertEq(weightRegistry.weightOf(alice), 0);
        assertEq(weightRegistry.totalWeight(), 0);

        vm.prank(alice);
        weightRegistry.activatePendingWeight();

        assertEq(weightRegistry.weightOf(alice), 6);
        assertEq(weightRegistry.totalWeight(), 6);
    }

    function testSentryNodeRemovalDoesNotClearPendingStakeIncrease() public {
        vm.warp(receiver.startTime());
        vm.prank(membershipRegistry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 1_000);

        receiver.setCurrentPeriod(1);
        vm.prank(alice);
        weightRegistry.activatePendingWeight();

        vm.prank(stakeWeightUpdater);
        weightRegistry.updateStakeWeight(alice, 10);

        vm.prank(membershipRegistry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 0);

        assertEq(weightRegistry.weightOf(alice), 0);
        assertEq(weightRegistry.totalWeight(), 0);

        receiver.setCurrentPeriod(2);
        vm.prank(alice);
        weightRegistry.activatePendingWeight();

        assertEq(weightRegistry.weightOf(alice), 10);
        assertEq(weightRegistry.totalWeight(), 10);
    }

    function testMatureStakePendingCanActivateWhileSentryNodeIncreaseIsImmature() public {
        vm.warp(receiver.startTime());
        vm.prank(membershipRegistry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 1_000, 100_000 ether);

        receiver.setCurrentPeriod(1);
        vm.prank(alice);
        weightRegistry.activatePendingWeight();

        vm.prank(stakeWeightUpdater);
        weightRegistry.updateStakeWeight(alice, 10);

        receiver.setCurrentPeriod(2);
        vm.prank(membershipRegistry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 1_000, 135_000 ether);

        vm.prank(alice);
        weightRegistry.activatePendingWeight();

        assertEq(weightRegistry.weightOf(alice), 100_000 ether + 10);
        assertEq(weightRegistry.totalWeight(), 100_000 ether + 10);

        ZkSysRewardWeightRegistry.PendingWeightView memory pending = weightRegistry.pendingWeightComponents(alice);
        assertEq(pending.stakeEffectivePeriod, 0);
        assertEq(pending.sentryNodeWeight, 135_000 ether);
        assertEq(pending.sentryNodeEffectivePeriod, 3);

        receiver.setCurrentPeriod(3);
        vm.prank(alice);
        weightRegistry.activatePendingWeight();

        assertEq(weightRegistry.weightOf(alice), 135_000 ether + 10);
        assertEq(weightRegistry.totalWeight(), 135_000 ether + 10);
    }

    function testMatureSentryNodePendingCanActivateWhileStakeIncreaseIsImmature() public {
        vm.warp(receiver.startTime());
        vm.prank(membershipRegistry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 1_000, 100_000 ether);

        receiver.setCurrentPeriod(1);
        vm.prank(stakeWeightUpdater);
        weightRegistry.updateStakeWeight(alice, 10);

        vm.prank(alice);
        weightRegistry.activatePendingWeight();

        assertEq(weightRegistry.weightOf(alice), 100_000 ether);
        assertEq(weightRegistry.totalWeight(), 100_000 ether);

        ZkSysRewardWeightRegistry.PendingWeightView memory pending = weightRegistry.pendingWeightComponents(alice);
        assertEq(pending.stakeWeight, 10);
        assertEq(pending.stakeEffectivePeriod, 2);
        assertEq(pending.sentryNodeEffectivePeriod, 0);

        receiver.setCurrentPeriod(2);
        vm.prank(alice);
        weightRegistry.activatePendingWeight();

        assertEq(weightRegistry.weightOf(alice), 100_000 ether + 10);
        assertEq(weightRegistry.totalWeight(), 100_000 ether + 10);
    }

    function testAnyoneCanActivateMaturePendingWeightForAccount() public {
        vm.warp(receiver.startTime());
        vm.prank(membershipRegistry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 1_000);

        receiver.setCurrentPeriod(1);
        vm.prank(bob);
        weightRegistry.activatePendingWeightFor(alice);

        assertEq(weightRegistry.weightOf(alice), DEFAULT_SENTRY_NODE_WEIGHT);
        assertEq(weightRegistry.totalWeight(), DEFAULT_SENTRY_NODE_WEIGHT);
    }

    function _applyL1Update(address account, uint32 sentryNodeCollateralHeight) private {
        _applyL1Update(
            account,
            sentryNodeCollateralHeight,
            sentryNodeCollateralHeight == 0 ? 0 : DEFAULT_SENTRY_NODE_WEIGHT
        );
    }

    function _applyL1Update(address account, uint32 sentryNodeCollateralHeight, uint128 sentryNodeWeight) private {
        ZkSysMembershipRegistry.SentryNodeUpdate[] memory updates = new ZkSysMembershipRegistry.SentryNodeUpdate[](1);
        updates[0] = ZkSysMembershipRegistry.SentryNodeUpdate({
            account: account,
            sentryNodeCollateralHeight: sentryNodeCollateralHeight,
            sentryNodeWeight: sentryNodeWeight
        });
        membershipRegistry.applyL1SentryNodeUpdates(updates);
    }

    function _deployMembershipRegistry(address admin_, address l1Bridge_) private returns (ZkSysMembershipRegistry) {
        ZkSysMembershipRegistry implementation = new ZkSysMembershipRegistry();
        ERC1967Proxy proxy = new ERC1967Proxy(
            address(implementation), abi.encodeCall(ZkSysMembershipRegistry.initialize, (admin_, l1Bridge_))
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
            abi.encodeCall(ZkSysRewardWeightRegistry.initialize, (admin_, membershipRegistry_, ACTIVATION_DELAY_PERIODS))
        );
        return ZkSysRewardWeightRegistry(address(proxy));
    }

    function _accessControlRevert(address account, bytes32 role) private pure returns (bytes memory) {
        return abi.encodePacked(
            "AccessControl: account ",
            _toLowerHexString(account),
            " is missing role ",
            _toLowerHexString(role)
        );
    }

    function _toLowerHexString(address account) private pure returns (bytes memory) {
        return _toLowerHexString(bytes32(uint256(uint160(account))), 20);
    }

    function _toLowerHexString(bytes32 value) private pure returns (bytes memory) {
        return _toLowerHexString(value, 32);
    }

    function _toLowerHexString(bytes32 value, uint256 length) private pure returns (bytes memory buffer) {
        bytes16 symbols = "0123456789abcdef";
        buffer = new bytes(2 + length * 2);
        buffer[0] = "0";
        buffer[1] = "x";
        for (uint256 i = 0; i < length; ++i) {
            uint8 b = uint8(value[32 - length + i]);
            buffer[2 + i * 2] = symbols[b >> 4];
            buffer[3 + i * 2] = symbols[b & 0x0f];
        }
    }
}
