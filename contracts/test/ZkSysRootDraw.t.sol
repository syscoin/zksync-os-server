// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";
import {
    ZkSysRootDrawV1,
    ZkSysRootDrawReceiverV1,
    IZkSysMessageProofV1,
    IZkSysRootDrawReceiverV1
} from "contracts/src/zksys/ZkSysRootDrawV1.sol";
import {IL1BridgehubMinimal} from "contracts/src/zksys/ZkSysRegistryBridge.sol";

contract RootDrawMailbox is IZkSysMessageProofV1 {
    bytes32 public expected;

    function setExpected(address sender, bytes32 commitment) external {
        expected = keccak256(abi.encode(sender, abi.encode(keccak256("ZKSYS_WRAPPER_DRAW_V1"), commitment)));
    }

    function proveL2MessageInclusion(uint256, uint256, L2Message calldata message, bytes32[] calldata)
        external
        view
        returns (bool)
    {
        return expected == keccak256(abi.encode(message.sender, message.data));
    }
}

contract RootDrawHub is IL1BridgehubMinimal {
    bytes public payload;
    address public recipient;
    uint256 public chainId;

    function requestL2TransactionDirect(L2TransactionRequestDirect calldata request)
        external
        payable
        returns (bytes32)
    {
        payload = request.l2Calldata;
        recipient = request.l2Contract;
        chainId = request.chainId;
        return keccak256(payload);
    }
}

contract ZkSysRootDrawTest is Test {
    RootDrawMailbox internal mailbox;
    RootDrawHub internal hub;
    ZkSysRootDrawV1 internal source;
    ZkSysRootDrawReceiverV1 internal receiver;
    address internal constant COORDINATOR = address(0xCAFE);
    bytes32 internal constant COMMITMENT = keccak256("frozen work");
    bytes32 internal constant ENTROPY = keccak256("future block");

    function setUp() public {
        mailbox = new RootDrawMailbox();
        hub = new RootDrawHub();
        receiver = new ZkSysRootDrawReceiverV1(address(0xABCD));
        source = new ZkSysRootDrawV1(mailbox, hub, COORDINATOR, address(receiver), 5050, 3, 2);
        mailbox.setExpected(COORDINATOR, COMMITMENT);
        vm.roll(100);
    }

    function request() internal {
        source.requestDraw(COMMITMENT, 1, 0, 0, new bytes32[](0));
    }

    function testDrawIsFutureOfActualRootAndFrozenOnce() public {
        request();
        (uint64 height,) = source.draws(COMMITMENT);
        assertEq(height, 103);
        vm.roll(101);
        request();
        (height,) = source.draws(COMMITMENT);
        assertEq(height, 103);
        vm.roll(104);
        vm.setBlockhash(103, ENTROPY);
        vm.expectRevert(ZkSysRootDrawV1.DrawNotReady.selector);
        source.captureDraw(COMMITMENT);
        vm.roll(105);
        source.captureDraw(COMMITMENT);
        (, bytes32 result) = source.draws(COMMITMENT);
        assertEq(result, ENTROPY);
        vm.roll(900);
        vm.expectRevert(ZkSysRootDrawV1.DrawStillAvailable.selector);
        source.retryExpiredDraw(COMMITMENT);
    }

    function testRejectsWrongCoordinatorOrCommitmentProof() public {
        mailbox.setExpected(address(1), COMMITMENT);
        vm.expectRevert(ZkSysRootDrawV1.InvalidFreezeProof.selector);
        request();
        mailbox.setExpected(COORDINATOR, bytes32(uint256(2)));
        vm.expectRevert(ZkSysRootDrawV1.InvalidFreezeProof.selector);
        request();
    }

    function testExpiredEntropyKeepsCommitmentAndSchedulesFuture() public {
        request();
        vm.roll(359);
        vm.expectRevert(ZkSysRootDrawV1.DrawStillAvailable.selector);
        source.retryExpiredDraw(COMMITMENT);
        vm.roll(360);
        source.retryExpiredDraw(COMMITMENT);
        (uint64 height, bytes32 result) = source.draws(COMMITMENT);
        assertEq(height, 363);
        assertEq(result, bytes32(0));
    }

    function testRelayReplaysSameAuthenticatedPayload() public {
        request();
        vm.roll(105);
        vm.setBlockhash(103, ENTROPY);
        bytes32 first = source.relayDraw(COMMITMENT, 500_000, 800, address(this));
        assertEq(hub.recipient(), address(receiver));
        assertEq(hub.chainId(), 5050);
        assertEq(
            hub.payload(), abi.encodeCall(IZkSysRootDrawReceiverV1.receiveDraw, (COMMITMENT, uint64(103), ENTROPY))
        );
        vm.roll(1000);
        assertEq(source.relayDraw(COMMITMENT, 500_000, 800, address(this)), first);
    }

    function testReceiverRequiresAliasAndRejectsConflictingReplay() public {
        vm.expectRevert(ZkSysRootDrawReceiverV1.UnauthorizedSource.selector);
        receiver.receiveDraw(COMMITMENT, 103, ENTROPY);
        address alias_ = receiver.aliasedRootSource();
        vm.startPrank(alias_);
        receiver.receiveDraw(COMMITMENT, 103, ENTROPY);
        receiver.receiveDraw(COMMITMENT, 103, ENTROPY);
        vm.expectRevert(ZkSysRootDrawReceiverV1.InvalidDraw.selector);
        receiver.receiveDraw(COMMITMENT, 104, ENTROPY);
        vm.stopPrank();
        (uint64 height, bytes32 result) = receiver.drawFor(COMMITMENT);
        assertEq(height, 103);
        assertEq(result, ENTROPY);
    }
}
