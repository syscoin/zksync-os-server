// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";
import {IL1BridgehubMinimal, IZkSysMembershipRegistryL2, ZkSysRegistryBridge} from "contracts/src/zksys/ZkSysRegistryBridge.sol";

contract MockBridgehub is IL1BridgehubMinimal {
    bytes32 public constant TX_HASH = keccak256("tx");

    L2TransactionRequestDirect public lastRequest;
    uint256 public lastValue;
    IZkSysMembershipRegistryL2.SentryNodeUpdate public lastDecodedUpdate;

    function requestL2TransactionDirect(L2TransactionRequestDirect calldata request)
        external
        payable
        returns (bytes32 canonicalTxHash)
    {
        lastRequest = request;
        lastValue = msg.value;
        IZkSysMembershipRegistryL2.SentryNodeUpdate[] memory updates =
            abi.decode(_withoutSelector(request.l2Calldata), (IZkSysMembershipRegistryL2.SentryNodeUpdate[]));
        if (updates.length > 0) {
            lastDecodedUpdate = updates[0];
        }
        return TX_HASH;
    }

    function lastRequestFields()
        external
        view
        returns (
            uint256 chainId,
            uint256 mintValue,
            address l2Contract,
            uint256 l2Value,
            bytes memory l2Calldata,
            uint256 l2GasLimit,
            uint256 l2GasPerPubdataByteLimit,
            address refundRecipient
        )
    {
        return (
            lastRequest.chainId,
            lastRequest.mintValue,
            lastRequest.l2Contract,
            lastRequest.l2Value,
            lastRequest.l2Calldata,
            lastRequest.l2GasLimit,
            lastRequest.l2GasPerPubdataByteLimit,
            lastRequest.refundRecipient
        );
    }

    function lastDecodedUpdateFields() external view returns (address account, uint32 sentryNodeCollateralHeight) {
        IZkSysMembershipRegistryL2.SentryNodeUpdate memory update = lastDecodedUpdate;
        return (update.account, update.sentryNodeCollateralHeight);
    }

    function _withoutSelector(bytes memory data) private pure returns (bytes memory result) {
        result = new bytes(data.length - 4);
        for (uint256 i = 4; i < data.length; ++i) {
            result[i - 4] = data[i];
        }
    }
}

contract ZkSysRegistryBridgeTest is Test {
    address private constant NEVM_ADDRESS_PRECOMPILE = address(0x62);

    uint256 private zksysChainId = 57;
    address private l2Registry = address(0x1234);
    address private alice = address(0xA11CE);

    MockBridgehub private bridgehub;
    ZkSysRegistryBridge private bridge;

    function setUp() public {
        bridgehub = new MockBridgehub();
        bridge = new ZkSysRegistryBridge(bridgehub, zksysChainId, l2Registry);
    }

    function testPushUpdatesVerifiesNevmFactAndRequestsL2Transaction() public {
        address[] memory accounts = new address[](1);
        accounts[0] = alice;

        vm.mockCall(NEVM_ADDRESS_PRECOMPILE, abi.encodePacked(alice), abi.encode(uint256(1_000)));

        bytes32 txHash = bridge.pushSentryNodeUpdates{value: 1 ether}(accounts, 1_000_000, 800, address(0xFEE));

        assertEq(txHash, bridgehub.TX_HASH());
        assertEq(bridgehub.lastValue(), 1 ether);

        (
            uint256 chainId,
            uint256 mintValue,
            address l2Contract,
            uint256 l2Value,
            bytes memory l2Calldata,
            uint256 l2GasLimit,
            uint256 l2GasPerPubdataByteLimit,
            address refundRecipient
        ) = bridgehub.lastRequestFields();

        assertEq(chainId, zksysChainId);
        assertEq(mintValue, 1 ether);
        assertEq(l2Contract, l2Registry);
        assertEq(l2Value, 0);
        assertEq(l2GasLimit, 1_000_000);
        assertEq(l2GasPerPubdataByteLimit, 800);
        assertEq(refundRecipient, address(0xFEE));
        assertEq(bytes4(l2Calldata), IZkSysMembershipRegistryL2.applyL1SentryNodeUpdates.selector);

        (address account, uint32 sentryNodeCollateralHeight) = bridgehub.lastDecodedUpdateFields();
        assertEq(account, alice);
        assertEq(sentryNodeCollateralHeight, 1_000);
    }

    function testPushUpdatesIsPermissionless() public {
        address[] memory accounts = new address[](1);
        accounts[0] = alice;

        vm.mockCall(NEVM_ADDRESS_PRECOMPILE, abi.encodePacked(alice), abi.encode(uint256(1_000)));

        vm.prank(address(0xCA11));
        bridge.pushSentryNodeUpdates(accounts, 1_000_000, 800, address(0));

        (address account, uint32 sentryNodeCollateralHeight) = bridgehub.lastDecodedUpdateFields();
        assertEq(account, alice);
        assertEq(sentryNodeCollateralHeight, 1_000);
    }

    function testPushUpdatesUsesNevmFactInsteadOfCallerHeight() public {
        address[] memory accounts = new address[](1);
        accounts[0] = alice;

        vm.mockCall(NEVM_ADDRESS_PRECOMPILE, abi.encodePacked(alice), abi.encode(uint256(999)));

        bridge.pushSentryNodeUpdates(accounts, 1_000_000, 800, address(0));

        (, uint32 sentryNodeCollateralHeight) = bridgehub.lastDecodedUpdateFields();
        assertEq(sentryNodeCollateralHeight, 999);
    }

    function testPushUpdatesRejectsOverflowingNevmHeight() public {
        address[] memory accounts = new address[](1);
        accounts[0] = alice;
        uint256 overflowingHeight = uint256(type(uint32).max) + 1;

        vm.mockCall(NEVM_ADDRESS_PRECOMPILE, abi.encodePacked(alice), abi.encode(overflowingHeight));

        vm.expectRevert(
            abi.encodeWithSelector(ZkSysRegistryBridge.NevmCollateralHeightOverflow.selector, alice, overflowingHeight)
        );
        bridge.pushSentryNodeUpdates(accounts, 1_000_000, 800, address(0));
    }
}
