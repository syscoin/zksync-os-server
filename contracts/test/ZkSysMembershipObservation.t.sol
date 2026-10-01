// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts-v4/proxy/ERC1967/ERC1967Proxy.sol";
import {ZkSysMembershipRegistry, IZkSysSentryNodeReceiver} from "contracts/src/zksys/ZkSysMembershipRegistry.sol";
import {
    ZkSysRegistryBridge,
    IL1BridgehubMinimal,
    IZkSysMembershipRegistryL2
} from "contracts/src/zksys/ZkSysRegistryBridge.sol";

contract ObservationReceiver is IZkSysSentryNodeReceiver {
    uint256 public notifications;

    function onSentryNodeStatusChange(address, uint32, uint32, uint128, uint128) external {
        ++notifications;
    }
}

contract ObservationBridgehub is IL1BridgehubMinimal {
    bytes public payload;

    function requestL2TransactionDirect(L2TransactionRequestDirect calldata request)
        external
        payable
        returns (bytes32)
    {
        payload = request.l2Calldata;
        return keccak256(payload);
    }
}

contract ZkSysMembershipObservationTest is Test {
    ZkSysMembershipRegistry internal registry;
    ObservationReceiver internal receiver;
    address internal constant MEMBER = address(0x123);
    address internal constant BRIDGE = address(0x456);
    address internal bridgeAlias;

    function setUp() public {
        vm.warp(10_000);
        registry = ZkSysMembershipRegistry(
            address(
                new ERC1967Proxy(
                    address(new ZkSysMembershipRegistry()),
                    abi.encodeCall(ZkSysMembershipRegistry.initialize, (address(this), BRIDGE))
                )
            )
        );
        bridgeAlias = registry.aliasedL1RegistryBridge();
        receiver = new ObservationReceiver();
        registry.setSentryNodeReceiver(receiver);
    }

    function updates(uint32 collateralHeight, uint128 weight)
        internal
        pure
        returns (ZkSysMembershipRegistry.SentryNodeUpdate[] memory result)
    {
        result = new ZkSysMembershipRegistry.SentryNodeUpdate[](1);
        result[0] = ZkSysMembershipRegistry.SentryNodeUpdate(MEMBER, collateralHeight, weight);
    }

    function applyObservation(uint64 height, uint64 time, uint32 collateral, uint128 weight) internal {
        vm.prank(bridgeAlias);
        registry.applyL1SentryNodeUpdates(updates(collateral, weight), height, time);
    }

    function testOnlyAuthenticatedSourceCanEstablishAge() public {
        vm.expectRevert(
            abi.encodeWithSelector(ZkSysMembershipRegistry.UnauthorizedL1RegistryBridge.selector, address(this))
        );
        registry.applyL1SentryNodeUpdates(updates(1_000, 135_000 ether), 211_240, 9_000);
        applyObservation(211_240, 9_000, 1_000, 135_000 ether);
        (uint64 height, uint64 time) = registry.membershipObservation(MEMBER);
        assertEq(height - registry.member(MEMBER).sentryNodeCollateralHeight, 210_240);
        assertEq(time, 9_000);
    }

    function testDelayedObservationCannotResurrectRemovedMember() public {
        applyObservation(211_240, 9_000, 1_000, 135_000 ether);
        applyObservation(211_241, 9_001, 0, 0);
        applyObservation(211_240, 9_000, 1_000, 135_000 ether);
        assertFalse(registry.isActiveSentryNode(MEMBER));
        assertEq(receiver.notifications(), 2);
    }

    function testExactReplayIsIdempotentAndConflictRejected() public {
        applyObservation(211_240, 9_000, 1_000, 135_000 ether);
        applyObservation(211_240, 9_000, 1_000, 135_000 ether);
        assertEq(receiver.notifications(), 1);
        vm.expectRevert(
            abi.encodeWithSelector(
                ZkSysMembershipRegistry.ConflictingMembershipObservation.selector, MEMBER, uint64(211_240)
            )
        );
        applyObservation(211_240, 9_000, 0, 0);
    }

    function testSourceClockCannotMoveBackwards() public {
        applyObservation(211_240, 9_000, 1_000, 135_000 ether);
        vm.expectRevert(ZkSysMembershipRegistry.InvalidMembershipObservation.selector);
        applyObservation(211_241, 8_999, 0, 0);
        assertTrue(registry.isActiveSentryNode(MEMBER));
    }

    function testFutureOrContradictoryObservationsRejected() public {
        vm.expectRevert(ZkSysMembershipRegistry.InvalidMembershipObservation.selector);
        applyObservation(211_240, 10_001, 1_000, 135_000 ether);
        vm.expectRevert(ZkSysMembershipRegistry.InvalidMembershipObservation.selector);
        applyObservation(999, 9_000, 1_000, 135_000 ether);
        vm.expectRevert(ZkSysMembershipRegistry.InvalidMembershipObservation.selector);
        applyObservation(211_240, 9_000, 0, 135_000 ether);
    }

    function testBridgeSamplesObservationRatherThanAcceptingCallerAge() public {
        ObservationBridgehub hub = new ObservationBridgehub();
        ZkSysRegistryBridge bridge = ZkSysRegistryBridge(
            address(
                new ERC1967Proxy(
                    address(new ZkSysRegistryBridge()),
                    abi.encodeCall(
                        ZkSysRegistryBridge.initialize,
                        (hub, 57, address(registry), 1_317_500, 210_240, 525_600, 3_500, 10_000)
                    )
                )
            )
        );
        vm.roll(100);
        vm.mockCall(address(0x62), abi.encodePacked(MEMBER), abi.encode(uint256(1_000)));
        address[] memory accounts = new address[](1);
        accounts[0] = MEMBER;
        bridge.pushSentryNodeUpdates(accounts, 1_000_000, 800, address(this));
        bytes memory payload = hub.payload();
        assertEq(bytes4(payload), IZkSysMembershipRegistryL2.applyL1SentryNodeUpdates.selector);
        bytes memory body = new bytes(payload.length - 4);
        for (uint256 i; i < body.length; ++i) {
            body[i] = payload[i + 4];
        }
        (IZkSysMembershipRegistryL2.SentryNodeUpdate[] memory decoded, uint64 height, uint64 time) =
            abi.decode(body, (IZkSysMembershipRegistryL2.SentryNodeUpdate[], uint64, uint64));
        assertEq(height, 1_317_600);
        assertEq(time, 10_000);
        assertEq(decoded[0].sentryNodeCollateralHeight, 1_000);
    }
}
