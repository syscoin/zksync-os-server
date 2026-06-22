// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";
import {
    IL1BridgehubMinimal,
    IZkSysMembershipRegistryL2,
    ZkSysRegistryBridge
} from "contracts/src/zksys/ZkSysRegistryBridge.sol";

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

    function lastDecodedUpdateFields()
        external
        view
        returns (address account, uint32 sentryNodeCollateralHeight, uint128 sentryNodeWeight)
    {
        IZkSysMembershipRegistryL2.SentryNodeUpdate memory update = lastDecodedUpdate;
        return (update.account, update.sentryNodeCollateralHeight, update.sentryNodeWeight);
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
    uint32 private nevmStartBlock = 1_317_500;
    uint32 private seniorityHeight1 = 210_240;
    uint32 private seniorityHeight2 = 525_600;
    uint16 private seniorityLevel1Bps = 3_500;
    uint16 private seniorityLevel2Bps = 10_000;

    MockBridgehub private bridgehub;
    ZkSysRegistryBridge private bridge;

    function setUp() public {
        bridgehub = new MockBridgehub();
        bridge = new ZkSysRegistryBridge(
            bridgehub,
            zksysChainId,
            l2Registry,
            nevmStartBlock,
            seniorityHeight1,
            seniorityHeight2,
            seniorityLevel1Bps,
            seniorityLevel2Bps
        );
        vm.roll(seniorityHeight2);
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

        (address account, uint32 sentryNodeCollateralHeight, uint128 sentryNodeWeight) =
            bridgehub.lastDecodedUpdateFields();
        assertEq(account, alice);
        assertEq(sentryNodeCollateralHeight, 1_000);
        assertEq(sentryNodeWeight, 200_000 ether);
    }

    function testPushUpdatesIsPermissionless() public {
        address[] memory accounts = new address[](1);
        accounts[0] = alice;

        vm.mockCall(NEVM_ADDRESS_PRECOMPILE, abi.encodePacked(alice), abi.encode(uint256(1_000)));

        vm.prank(address(0xCA11));
        bridge.pushSentryNodeUpdates(accounts, 1_000_000, 800, address(0));

        (address account, uint32 sentryNodeCollateralHeight, uint128 sentryNodeWeight) =
            bridgehub.lastDecodedUpdateFields();
        assertEq(account, alice);
        assertEq(sentryNodeCollateralHeight, 1_000);
        assertEq(sentryNodeWeight, 200_000 ether);
    }

    function testPushUpdatesUsesNevmFactInsteadOfCallerHeight() public {
        address[] memory accounts = new address[](1);
        accounts[0] = alice;

        vm.mockCall(NEVM_ADDRESS_PRECOMPILE, abi.encodePacked(alice), abi.encode(uint256(999)));

        bridge.pushSentryNodeUpdates(accounts, 1_000_000, 800, address(0));

        (, uint32 sentryNodeCollateralHeight, uint128 sentryNodeWeight) = bridgehub.lastDecodedUpdateFields();
        assertEq(sentryNodeCollateralHeight, 999);
        assertEq(sentryNodeWeight, 200_000 ether);
    }

    function testSentryNodeWeightUsesSyscoinSeniorityTiers() public {
        uint32 collateralHeight = nevmStartBlock;

        assertEq(bridge.sentryNodeWeight(0, 0), 0);
        assertEq(bridge.sentryNodeWeight(collateralHeight, 0), 100_000 ether);
        assertEq(bridge.sentryNodeWeight(collateralHeight, seniorityHeight1 - 1), 100_000 ether);
        assertEq(bridge.sentryNodeWeight(collateralHeight, seniorityHeight1), 135_000 ether);
        assertEq(bridge.sentryNodeWeight(collateralHeight, seniorityHeight2), 200_000 ether);
    }

    function testConstructorRejectsSeniorityBpsAboveDenominator() public {
        uint16 invalidBps = uint16(bridge.BPS_DENOMINATOR() + 1);

        vm.expectRevert();
        this.deployBridgeWithLevel2Bps(invalidBps);
    }

    function deployBridgeWithLevel2Bps(uint16 seniorityLevel2Bps_) external returns (ZkSysRegistryBridge) {
        return new ZkSysRegistryBridge(
            bridgehub,
            zksysChainId,
            l2Registry,
            nevmStartBlock,
            seniorityHeight1,
            seniorityHeight2,
            seniorityLevel1Bps,
            seniorityLevel2Bps_
        );
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

    function testPushUpdatesRejectsEmptyBatch() public {
        address[] memory accounts = new address[](0);

        vm.expectRevert(ZkSysRegistryBridge.EmptyBatch.selector);
        bridge.pushSentryNodeUpdates(accounts, 1_000_000, 800, address(0));
    }

    function testPushUpdatesRejectsOversizedBatch() public {
        address[] memory accounts = new address[](bridge.MAX_BATCH_SIZE() + 1);

        vm.expectRevert(abi.encodeWithSelector(ZkSysRegistryBridge.InvalidBatchSize.selector, accounts.length));
        bridge.pushSentryNodeUpdates(accounts, 1_000_000, 800, address(0));
    }

    function testPushUpdatesRejectsZeroAccount() public {
        address[] memory accounts = new address[](1);
        accounts[0] = address(0);

        vm.expectRevert(ZkSysRegistryBridge.InvalidAddress.selector);
        bridge.pushSentryNodeUpdates(accounts, 1_000_000, 800, address(0));
    }

    function testPushUpdatesRejectsDuplicateAccounts() public {
        address[] memory accounts = new address[](2);
        accounts[0] = alice;
        accounts[1] = alice;

        vm.expectRevert(abi.encodeWithSelector(ZkSysRegistryBridge.DuplicateAccount.selector, alice));
        bridge.pushSentryNodeUpdates(accounts, 1_000_000, 800, address(0));
    }
}
