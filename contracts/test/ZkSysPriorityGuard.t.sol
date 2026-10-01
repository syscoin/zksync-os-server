// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";
import {
    ZkSysPriorityGuardV1,
    ZkSysRootPrioritySourceV1,
    ZkSysPriorityCheckpointReceiverV1,
    IZkSysPriorityMailboxV1,
    IZkSysPriorityCheckpointReceiverV1,
    PriorityCheckpointV1
} from "contracts/src/zksys/ZkSysPriorityGuardV1.sol";
import {RootDrawHub} from "./ZkSysRootDraw.t.sol";

contract PriorityMailboxMock is IZkSysPriorityMailboxV1 {
    uint256 public total;
    uint256 public treeStart;
    uint256 public height;
    bytes32 public root = keccak256("");
    uint256 public cursor;
    uint256 public verified;
    uint256 public executed;
    bool public legacy;
    mapping(uint256 => uint256) public requestedAt;

    function setTree(uint256 total_, uint256 height_, bytes32 root_) external {
        total = total_;
        height = height_;
        root = root_;
    }

    function setTimestamp(uint256 index, uint256 value) external {
        requestedAt[index] = value;
    }

    function setProgress(uint256 verified_, uint256 executed_, uint256 cursor_) external {
        verified = verified_;
        executed = executed_;
        cursor = cursor_;
    }

    function setLegacy(bool value) external {
        legacy = value;
    }

    function getChainId() external pure returns (uint256) {
        return 57;
    }

    function getTotalPriorityTxs() external view returns (uint256) {
        return total;
    }

    function getPriorityTreeStartIndex() external view returns (uint256) {
        return treeStart;
    }

    function getPriorityTreeRoot() external view returns (bytes32) {
        return root;
    }

    function getPriorityTreeHeight() external view returns (uint256) {
        return height;
    }

    function getPriorityTransactionTimestamp(uint256 index) external view returns (uint256) {
        return requestedAt[index];
    }

    function getFirstUnprocessedPriorityTx() external view returns (uint256) {
        return cursor;
    }

    function isPriorityQueueActive() external view returns (bool) {
        return legacy;
    }

    function getTotalBatchesVerified() external view returns (uint256) {
        return verified;
    }

    function getTotalBatchesExecuted() external view returns (uint256) {
        return executed;
    }
}

contract ZkSysPriorityGuardTest is Test {
    PriorityMailboxMock private mailbox;
    RootDrawHub private hub;
    ZkSysRootPrioritySourceV1 private source;
    ZkSysPriorityCheckpointReceiverV1 private receiver;
    ZkSysPriorityGuardV1 private guard;
    bytes32 private constant POLICY = keccak256("priority-policy");
    bytes32 private constant WORK = keccak256("parent-and-first-batch");
    bytes32[] private leaves;

    function setUp() public {
        vm.warp(1_000);
        vm.roll(100);
        mailbox = new PriorityMailboxMock();
        hub = new RootDrawHub();
        address predictedReceiver = vm.computeCreateAddress(address(this), vm.getNonce(address(this)) + 1);
        source = new ZkSysRootPrioritySourceV1(mailbox, hub, predictedReceiver, 5050, POLICY, 100);
        receiver = new ZkSysPriorityCheckpointReceiverV1(address(source), 57, POLICY);
        guard = new ZkSysPriorityGuardV1(address(this), mailbox, receiver, POLICY, 50, 200, 2);
        for (uint256 i; i < 4; ++i) {
            leaves.push(keccak256(abi.encode("priority", i)));
        }
        mailbox.setTree(4, 2, _hash(_hash(leaves[0], leaves[1]), _hash(leaves[2], leaves[3])));
        mailbox.setTimestamp(0, 500);
        mailbox.setTimestamp(1, 800);
        mailbox.setTimestamp(2, 950);
        mailbox.setTimestamp(3, 990);
        _relay();
    }

    function _hash(bytes32 a, bytes32 b) private pure returns (bytes32) {
        return keccak256(abi.encodePacked(a, b));
    }

    function _relay() private {
        PriorityCheckpointV1 memory checkpoint = source.capture();
        vm.prank(receiver.aliasedRootSource());
        receiver.receiveCheckpoint(57, POLICY, checkpoint);
    }

    function _counts(uint256 first, uint256 second) private pure returns (uint256[] memory counts) {
        counts = new uint256[](2);
        counts[0] = first;
        counts[1] = second;
    }

    function _hashes(uint256 from, uint256 first, uint256 second) private view returns (bytes32[] memory hashes) {
        hashes = new bytes32[](2);
        hashes[0] = keccak256("");
        hashes[1] = keccak256("");
        uint256 cursor = from;
        for (uint256 i; i < first; ++i) {
            hashes[0] = _hash(hashes[0], leaves[cursor++]);
        }
        for (uint256 i; i < second; ++i) {
            hashes[1] = _hash(hashes[1], leaves[cursor++]);
        }
    }

    function _witness(uint256 from, uint256 first, uint256 second) private {
        bytes32[] memory items = new bytes32[](2);
        items[0] = leaves[from];
        items[1] = leaves[from + 1];
        bytes32[] memory left = new bytes32[](2);
        bytes32[] memory right = new bytes32[](2);
        left[0] = leaves[from + 1];
        right[0] = leaves[from];
        left[1] = from == 0 ? _hash(leaves[2], leaves[3]) : _hash(leaves[0], leaves[1]);
        right[1] = left[1];
        guard.publishPrefixWitness(WORK, _counts(first, second), items, left, right);
    }

    function testRootAgeUsesNativeTimestampsAndBoundedPrefix() public view {
        PriorityCheckpointV1 memory checkpoint = source.capture();
        assertEq(checkpoint.trackingStart, 0);
        assertEq(checkpoint.total, 4);
        assertEq(checkpoint.overdueEnd, 2);
        assertEq(checkpoint.rootTimestamp, 1_000);
        assertEq(checkpoint.treeHeight, 2);
    }

    function testRootSettlementReadsNativeSourceWithoutRelayOrCallerClock() public {
        mailbox.setTree(0, 0, keccak256(""));
        ZkSysRootPrioritySourceV1 direct = new ZkSysRootPrioritySourceV1(mailbox, hub, address(0), 0, POLICY, 100);
        guard = new ZkSysPriorityGuardV1(
            address(this), mailbox, IZkSysPriorityCheckpointReceiverV1(address(direct)), POLICY, 50, 200, 2
        );
        mailbox.setTree(4, 2, _hash(_hash(leaves[0], leaves[1]), _hash(leaves[2], leaves[3])));
        vm.warp(1_100);
        guard.open(WORK, 1);
        _witness(0, 1, 1);
        guard.consume(WORK, 2, _counts(1, 1), _hashes(0, 1, 1));
        assertEq(guard.priorityCursor(), 2);
        assertEq(direct.latestCheckpoint().overdueEnd, 4);
        vm.expectRevert(ZkSysRootPrioritySourceV1.InvalidConfiguration.selector);
        direct.relayCheckpoint(1_000_000, 800, address(this));
    }

    function testOverdueQueueRejectsAllEmptyPackage() public {
        guard.open(WORK, 1);
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidPrefix.selector);
        guard.consume(WORK, 2, _counts(0, 0), _hashes(0, 0, 0));
    }

    function testProofCursorAdvancesBeforeExecutionWithoutCountingPrefixTwice() public {
        guard.open(WORK, 1);
        _witness(0, 1, 1);
        guard.consume(WORK, 2, _counts(1, 1), _hashes(0, 1, 1));
        assertEq(guard.priorityCursor(), 2);
        mailbox.setProgress(2, 0, 0);
        vm.warp(1_100);
        vm.roll(101);
        _relay();
        guard.open(WORK, 3);
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidPrefix.selector);
        _witness(0, 1, 1);
        _witness(2, 2, 0);
        guard.consume(WORK, 4, _counts(2, 0), _hashes(2, 2, 0));
        assertEq(guard.priorityCursor(), 4);
    }

    function testHashWitnessCannotSubstituteCountsOrBatchPartition() public {
        guard.open(WORK, 1);
        _witness(0, 1, 1);
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidPrefix.selector);
        guard.consume(WORK, 2, _counts(2, 0), _hashes(0, 2, 0));
        bytes32[] memory hashes = _hashes(0, 1, 1);
        hashes[1] = bytes32(uint256(1));
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidPrefix.selector);
        guard.consume(WORK, 2, _counts(1, 1), hashes);
    }

    function testMissingWitnessAndUnboundedCountsRejected() public {
        guard.open(WORK, 1);
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidPrefix.selector);
        guard.consume(WORK, 2, _counts(1, 1), _hashes(0, 1, 1));
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidPrefix.selector);
        guard.consume(WORK, 2, _counts(3, 0), _hashes(0, 0, 0));
    }

    function testSkippedOrReorderedPriorityLeafCannotProvePrefix() public {
        guard.open(WORK, 1);
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidPrefix.selector);
        _witness(2, 1, 1);
        bytes32 leaf = leaves[0];
        leaves[0] = leaves[1];
        leaves[1] = leaf;
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidPrefix.selector);
        _witness(0, 1, 1);
    }

    function testShorterSubtreeProofCannotReplaceLeafRange() public {
        guard.open(WORK, 1);
        bytes32[] memory items = new bytes32[](2);
        items[0] = _hash(leaves[0], leaves[1]);
        items[1] = _hash(leaves[2], leaves[3]);
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidPrefix.selector);
        guard.publishPrefixWitness(WORK, _counts(1, 1), items, new bytes32[](1), new bytes32[](1));
    }

    function testLaterRootArrivalsCannotInvalidateInFlightPrefix() public {
        guard.open(WORK, 1);
        _witness(0, 1, 1);
        vm.warp(1_110);
        vm.roll(101);
        _relay();
        assertEq(source.capture().overdueEnd, 4);
        guard.consume(WORK, 2, _counts(1, 1), _hashes(0, 1, 1));
        assertEq(guard.priorityCursor(), 2);
    }

    function testExpiredWorkNeedsFreshCheckpointAndNewWitness() public {
        guard.open(WORK, 1);
        _witness(0, 1, 1);
        vm.warp(1_201);
        vm.expectRevert(ZkSysPriorityGuardV1.ExpiredWork.selector);
        guard.consume(WORK, 2, _counts(1, 1), _hashes(0, 1, 1));
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidCheckpoint.selector);
        guard.refreshExpired(WORK);
        vm.roll(101);
        _relay();
        guard.refreshExpired(WORK);
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidPrefix.selector);
        guard.consume(WORK, 2, _counts(1, 1), _hashes(0, 1, 1));
        _witness(0, 1, 1);
        guard.consume(WORK, 2, _counts(1, 1), _hashes(0, 1, 1));
    }

    function testFreshWorkCannotRefreshOrBeOverwritten() public {
        guard.open(WORK, 1);
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidWork.selector);
        guard.refreshExpired(WORK);
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidWork.selector);
        guard.open(keccak256("reroll"), 1);
    }

    function testStaleOrFutureRootClockFailsClosed() public {
        vm.warp(1_051);
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidCheckpoint.selector);
        guard.open(WORK, 1);
        vm.warp(999);
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidCheckpoint.selector);
        guard.open(WORK, 1);
    }

    function testCallerCannotForgeCheckpointAndOldDeliveryCannotRollback() public {
        PriorityCheckpointV1 memory checkpoint = source.capture();
        vm.expectRevert(ZkSysPriorityCheckpointReceiverV1.UnauthorizedSource.selector);
        receiver.receiveCheckpoint(57, POLICY, checkpoint);
        vm.warp(1_100);
        vm.roll(101);
        _relay();
        vm.prank(receiver.aliasedRootSource());
        receiver.receiveCheckpoint(57, POLICY, checkpoint);
        assertEq(receiver.latestCheckpoint().rootBlockNumber, 101);
        checkpoint.rootBlockNumber = 101;
        vm.prank(receiver.aliasedRootSource());
        vm.expectRevert(ZkSysPriorityCheckpointReceiverV1.InvalidCheckpoint.selector);
        receiver.receiveCheckpoint(57, POLICY, checkpoint);
    }

    function testAuthenticatedMessageBindsChildAndPolicyAndSupportsSameBlockAppend() public {
        PriorityCheckpointV1 memory checkpoint = source.capture();
        vm.prank(receiver.aliasedRootSource());
        vm.expectRevert(ZkSysPriorityCheckpointReceiverV1.InvalidCheckpoint.selector);
        receiver.receiveCheckpoint(58, POLICY, checkpoint);
        vm.prank(receiver.aliasedRootSource());
        vm.expectRevert(ZkSysPriorityCheckpointReceiverV1.InvalidCheckpoint.selector);
        receiver.receiveCheckpoint(57, keccak256("other-policy"), checkpoint);
        mailbox.setTree(5, 3, keccak256("five-leaf-tree"));
        mailbox.setTimestamp(4, 999);
        _relay();
        assertEq(receiver.latestCheckpoint().total, 5);
    }

    function testRootRelayUsesCanonicalBridgehubAndExactDomainPayload() public {
        PriorityCheckpointV1 memory checkpoint = source.capture();
        source.relayCheckpoint(1_000_000, 800, address(this));
        assertEq(hub.recipient(), address(receiver));
        assertEq(hub.chainId(), 5050);
        assertEq(
            hub.payload(),
            abi.encodeCall(IZkSysPriorityCheckpointReceiverV1.receiveCheckpoint, (57, POLICY, checkpoint))
        );
    }

    function testFuzzOrderedRangeProofMatchesNativeTree(uint8 startSeed, uint8 limitSeed) public {
        uint256 from = bound(startSeed, 0, 3);
        uint256 limit = bound(limitSeed, 1, 2);
        uint256 count = 4 - from < limit ? 4 - from : limit;
        mailbox.setProgress(0, 0, from);
        guard = new ZkSysPriorityGuardV1(address(this), mailbox, receiver, POLICY, 50, 200, uint64(limit));
        vm.warp(1_100);
        vm.roll(101);
        _relay();
        guard.open(WORK, 1);
        bytes32[] memory items = new bytes32[](count);
        for (uint256 i; i < count; ++i) {
            items[i] = leaves[from + i];
        }
        bytes32[] memory left = new bytes32[](2);
        bytes32[] memory right = new bytes32[](2);
        uint256 end = from + count - 1;
        left[0] = leaves[from ^ 1];
        right[0] = leaves[end ^ 1];
        left[1] = from < 2 ? _hash(leaves[2], leaves[3]) : _hash(leaves[0], leaves[1]);
        right[1] = end < 2 ? _hash(leaves[2], leaves[3]) : _hash(leaves[0], leaves[1]);
        guard.publishPrefixWitness(WORK, _counts(count, 0), items, left, right);
        guard.consume(WORK, 2, _counts(count, 0), _hashes(from, count, 0));
        assertEq(guard.priorityCursor(), from + count);
    }

    function testNativeOutOfBandProofOrExecutionFailsClosed() public {
        mailbox.setProgress(2, 0, 0);
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidWork.selector);
        guard.open(WORK, 1);
        mailbox.setProgress(0, 0, 1);
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidWork.selector);
        guard.open(WORK, 1);
    }

    function testNoPendingPriorityStillAllowsCanonicalEmptyProgress() public {
        mailbox.setTimestamp(0, 990);
        mailbox.setTimestamp(1, 990);
        mailbox.setTimestamp(2, 990);
        receiver = new ZkSysPriorityCheckpointReceiverV1(address(source), 57, POLICY);
        guard = new ZkSysPriorityGuardV1(address(this), mailbox, receiver, POLICY, 50, 200, 2);
        vm.roll(101);
        _relay();
        guard.open(WORK, 1);
        guard.consume(WORK, 2, _counts(0, 0), _hashes(0, 0, 0));
        assertEq(guard.priorityCursor(), 0);
    }

    function testRootSourceRejectsMissingTimestampAndIncorrectHeight() public {
        mailbox.setTimestamp(2, 0);
        vm.expectRevert(ZkSysRootPrioritySourceV1.InvalidQueue.selector);
        source.capture();
        mailbox.setTree(4, 1, bytes32(uint256(1)));
        vm.expectRevert(ZkSysRootPrioritySourceV1.InvalidQueue.selector);
        source.capture();
    }

    function testOnlyGateCanFreezeConsumeOrRefresh() public {
        vm.prank(address(123));
        vm.expectRevert(ZkSysPriorityGuardV1.Unauthorized.selector);
        guard.open(WORK, 1);
        guard.open(WORK, 1);
        vm.prank(address(123));
        vm.expectRevert(ZkSysPriorityGuardV1.Unauthorized.selector);
        guard.consume(WORK, 2, _counts(0, 0), _hashes(0, 0, 0));
    }
}
