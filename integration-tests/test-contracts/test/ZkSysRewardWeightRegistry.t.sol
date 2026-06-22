// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";
import {ZkSysMembershipRegistry} from "contracts/src/zksys/ZkSysMembershipRegistry.sol";
import {IZkSysWeightReceiver, ZkSysRewardWeightRegistry} from "contracts/src/zksys/ZkSysRewardWeightRegistry.sol";

contract MockRewardWeightReceiver is IZkSysWeightReceiver {
    address public lastAccount;
    uint256 public lastOldWeight;
    uint256 public lastNewWeight;
    uint256 public lastOldTotalWeight;

    function onWeightChange(address account, uint256 oldWeight, uint256 newWeight, uint256 oldTotalWeight) external {
        lastAccount = account;
        lastOldWeight = oldWeight;
        lastNewWeight = newWeight;
        lastOldTotalWeight = oldTotalWeight;
    }
}

contract ZkSysRewardWeightRegistryTest is Test {
    address private admin = address(0xAD);
    address private l1Bridge = address(0xB111D6E);
    address private alice = address(0xA11CE);
    address private bob = address(0xB0B);

    ZkSysMembershipRegistry private membershipRegistry;
    ZkSysRewardWeightRegistry private weightRegistry;
    MockRewardWeightReceiver private receiver;

    function setUp() public {
        membershipRegistry = new ZkSysMembershipRegistry(admin, l1Bridge);
        weightRegistry = new ZkSysRewardWeightRegistry(admin, membershipRegistry);
        receiver = new MockRewardWeightReceiver();

        vm.startPrank(admin);
        membershipRegistry.setSentryNodeReceiver(weightRegistry);
        weightRegistry.setWeightReceiver(receiver);
        vm.stopPrank();
    }

    function testAdminCanUpdateStakeWeightSeparatelyFromSentryNodeWeight() public {
        vm.prank(admin);
        weightRegistry.adminUpdateStakeWeight(alice, 2);

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
        assertEq(weightRegistry.weightOf(alice), weightRegistry.SENTRY_NODE_WEIGHT());
        assertEq(weightRegistry.totalWeight(), weightRegistry.SENTRY_NODE_WEIGHT());

        _applyL1Update(alice, 0);
        vm.stopPrank();

        assertEq(weightRegistry.weightOf(alice), 0);
        assertEq(weightRegistry.totalWeight(), 0);
        assertEq(receiver.lastAccount(), alice);
        assertEq(receiver.lastOldWeight(), weightRegistry.SENTRY_NODE_WEIGHT());
        assertEq(receiver.lastNewWeight(), 0);
        assertEq(receiver.lastOldTotalWeight(), weightRegistry.SENTRY_NODE_WEIGHT());
    }

    function testL1AddressChangeIsRemoveOldAndAddNewWeight() public {
        vm.startPrank(membershipRegistry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 1_000);
        _applyL1Update(bob, 2_000);
        _applyL1Update(alice, 0);
        vm.stopPrank();

        assertEq(weightRegistry.weightOf(alice), 0);
        assertEq(weightRegistry.weightOf(bob), weightRegistry.SENTRY_NODE_WEIGHT());
        assertEq(weightRegistry.totalWeight(), weightRegistry.SENTRY_NODE_WEIGHT());
    }

    function testStakeWeightSurvivesSentryNodeRemoval() public {
        vm.prank(admin);
        weightRegistry.adminUpdateStakeWeight(alice, 7);

        vm.startPrank(membershipRegistry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 1_000);
        _applyL1Update(alice, 0);
        vm.stopPrank();

        assertEq(weightRegistry.weightOf(alice), 7);
        assertEq(weightRegistry.totalWeight(), 7);
    }

    function testOnlyMembershipRegistryCanUpdateSentryNodeWeight() public {
        vm.expectRevert(ZkSysRewardWeightRegistry.UnauthorizedMembershipRegistry.selector);
        weightRegistry.onSentryNodeStatusChange(alice, 0, 1_000);
    }

    function testWeightMustFitUint128() public {
        vm.expectRevert(
            abi.encodeWithSelector(ZkSysRewardWeightRegistry.InvalidWeight.selector, uint256(type(uint128).max) + 1)
        );
        vm.prank(admin);
        weightRegistry.adminUpdateStakeWeight(alice, uint256(type(uint128).max) + 1);
    }

    function _applyL1Update(address account, uint32 sentryNodeCollateralHeight) private {
        ZkSysMembershipRegistry.SentryNodeUpdate[] memory updates = new ZkSysMembershipRegistry.SentryNodeUpdate[](1);
        updates[0] = ZkSysMembershipRegistry.SentryNodeUpdate({
            account: account,
            sentryNodeCollateralHeight: sentryNodeCollateralHeight
        });
        membershipRegistry.applyL1SentryNodeUpdates(updates);
    }
}
