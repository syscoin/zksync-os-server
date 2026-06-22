// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {ERC1967Proxy} from "@openzeppelin/contracts-v4/proxy/ERC1967/ERC1967Proxy.sol";
import {Test} from "forge-std/Test.sol";
import {IZkSysSentryNodeReceiver} from "contracts/src/zksys/ZkSysMembershipRegistry.sol";
import {ZkSysMembershipRegistry} from "contracts/src/zksys/ZkSysMembershipRegistry.sol";

contract MockSentryNodeReceiver is IZkSysSentryNodeReceiver {
    address public lastAccount;
    uint32 public lastOldCollateralHeight;
    uint32 public lastNewCollateralHeight;

    function onSentryNodeStatusChange(address account, uint32 oldCollateralHeight, uint32 newCollateralHeight) external {
        lastAccount = account;
        lastOldCollateralHeight = oldCollateralHeight;
        lastNewCollateralHeight = newCollateralHeight;
    }
}

contract ZkSysMembershipRegistryTest is Test {
    address private admin = address(0xAD);
    address private l1Bridge = address(0xB111D6E);
    address private alice = address(0xA11CE);
    address private bob = address(0xB0B);

    ZkSysMembershipRegistry private registry;

    function setUp() public {
        registry = _deployRegistry(admin, l1Bridge);
    }

    function testOnlyAliasedL1BridgeCanApplyL1Updates() public {
        ZkSysMembershipRegistry.SentryNodeUpdate[] memory updates = new ZkSysMembershipRegistry.SentryNodeUpdate[](1);
        updates[0] = ZkSysMembershipRegistry.SentryNodeUpdate({account: alice, sentryNodeCollateralHeight: 1_000});

        vm.expectRevert(
            abi.encodeWithSelector(ZkSysMembershipRegistry.UnauthorizedL1RegistryBridge.selector, address(this))
        );
        registry.applyL1SentryNodeUpdates(updates);

        _wireReceiver();
        vm.prank(registry.aliasedL1RegistryBridge());
        registry.applyL1SentryNodeUpdates(updates);

        ZkSysMembershipRegistry.Member memory member = registry.member(alice);
        assertEq(member.sentryNodeCollateralHeight, 1_000);
        assertTrue(registry.isActiveSentryNode(alice));
        assertEq(registry.activeSentryNodeCount(), 1);
        assertEq(registry.activeSentryNodeAt(0), alice);
    }

    function testAdminCanSetL1BridgeWhenBootstrappedWithZero() public {
        ZkSysMembershipRegistry zeroBridgeRegistry = _deployRegistry(admin, address(0));
        address bridge = address(0xB0B);

        vm.prank(admin);
        zeroBridgeRegistry.setL1RegistryBridge(bridge);

        assertEq(zeroBridgeRegistry.l1RegistryBridge(), bridge);
        assertEq(zeroBridgeRegistry.aliasedL1RegistryBridge(), zeroBridgeRegistry.l1ToL2Alias(bridge));
    }

    function testL1BridgeSetIsIdempotentButNotReplaceable() public {
        address newBridge = address(0xB0B);

        vm.startPrank(admin);
        registry.setL1RegistryBridge(l1Bridge);
        vm.expectRevert(abi.encodeWithSelector(ZkSysMembershipRegistry.L1RegistryBridgeAlreadySet.selector, l1Bridge));
        registry.setL1RegistryBridge(newBridge);
        vm.stopPrank();

        assertEq(registry.l1RegistryBridge(), l1Bridge);
        assertEq(registry.aliasedL1RegistryBridge(), registry.l1ToL2Alias(l1Bridge));
    }

    function testL1RemovalClearsActiveSentryNodeEnumeration() public {
        _wireReceiver();
        vm.startPrank(registry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 1_000);
        _applyL1Update(alice, 0);
        vm.stopPrank();

        ZkSysMembershipRegistry.Member memory member = registry.member(alice);
        assertEq(member.sentryNodeCollateralHeight, 0);
        assertFalse(registry.isActiveSentryNode(alice));
        assertEq(registry.activeSentryNodeCount(), 0);
    }

    function testL1AddressChangeIsRemoveOldAndAddNew() public {
        _wireReceiver();
        vm.startPrank(registry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 1_000);
        _applyL1Update(bob, 2_000);
        _applyL1Update(alice, 0);
        vm.stopPrank();

        assertFalse(registry.isActiveSentryNode(alice));
        assertTrue(registry.isActiveSentryNode(bob));
        assertEq(registry.activeSentryNodeCount(), 1);
        assertEq(registry.activeSentryNodeAt(0), bob);
    }

    function testChangedL1FactRequiresReceiver() public {
        vm.prank(registry.aliasedL1RegistryBridge());
        vm.expectRevert(ZkSysMembershipRegistry.SentryNodeReceiverNotSet.selector);
        _applyL1Update(alice, 1_000);
    }

    function testSentryNodeReceiverIsNotifiedOnChangedFacts() public {
        MockSentryNodeReceiver receiver = new MockSentryNodeReceiver();

        vm.prank(admin);
        registry.setSentryNodeReceiver(receiver);

        vm.prank(registry.aliasedL1RegistryBridge());
        _applyL1Update(alice, 1_000);

        assertEq(receiver.lastAccount(), alice);
        assertEq(receiver.lastOldCollateralHeight(), 0);
        assertEq(receiver.lastNewCollateralHeight(), 1_000);
    }

    function testSentryNodeReceiverCannotBeZero() public {
        vm.prank(admin);
        vm.expectRevert(ZkSysMembershipRegistry.InvalidAddress.selector);
        registry.setSentryNodeReceiver(IZkSysSentryNodeReceiver(address(0)));
    }

    function testSentryNodeReceiverSetIsIdempotentButNotReplaceable() public {
        MockSentryNodeReceiver receiver = new MockSentryNodeReceiver();
        MockSentryNodeReceiver replacement = new MockSentryNodeReceiver();

        vm.startPrank(admin);
        registry.setSentryNodeReceiver(receiver);
        registry.setSentryNodeReceiver(receiver);

        vm.expectRevert(
            abi.encodeWithSelector(ZkSysMembershipRegistry.SentryNodeReceiverAlreadySet.selector, address(receiver))
        );
        registry.setSentryNodeReceiver(replacement);
        vm.stopPrank();

        assertEq(address(registry.sentryNodeReceiver()), address(receiver));
    }

    function _wireReceiver() private {
        MockSentryNodeReceiver receiver = new MockSentryNodeReceiver();
        vm.prank(admin);
        registry.setSentryNodeReceiver(receiver);
    }

    function _deployRegistry(address admin_, address l1Bridge_) private returns (ZkSysMembershipRegistry) {
        ZkSysMembershipRegistry implementation = new ZkSysMembershipRegistry();
        ERC1967Proxy proxy = new ERC1967Proxy(
            address(implementation), abi.encodeCall(ZkSysMembershipRegistry.initialize, (admin_, l1Bridge_))
        );
        return ZkSysMembershipRegistry(address(proxy));
    }

    function _applyL1Update(address account, uint32 sentryNodeCollateralHeight) private {
        ZkSysMembershipRegistry.SentryNodeUpdate[] memory updates = new ZkSysMembershipRegistry.SentryNodeUpdate[](1);
        updates[0] = ZkSysMembershipRegistry.SentryNodeUpdate({
            account: account,
            sentryNodeCollateralHeight: sentryNodeCollateralHeight
        });
        registry.applyL1SentryNodeUpdates(updates);
    }
}
