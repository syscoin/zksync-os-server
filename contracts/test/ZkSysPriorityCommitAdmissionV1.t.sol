// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";
import {IZkSysPriorityCommitAdmissionV1} from "contracts/src/zksys/IZkSysPriorityCommitAdmissionV1.sol";
import {
    IZkSysNativePriorityCommitSettlementV1,
    ZkSysPriorityCommitAdmissionV1
} from "contracts/src/zksys/ZkSysPriorityCommitAdmissionV1.sol";
import {
    PriorityCheckpointV1,
    ZkSysPriorityCheckpointReceiverV1,
    ZkSysRootPrioritySourceV1
} from "contracts/src/zksys/ZkSysPriorityGuardV1.sol";
import {IL1BridgehubMinimal} from "contracts/src/zksys/ZkSysRegistryBridge.sol";

contract PriorityCommitNativeMock is IZkSysNativePriorityCommitSettlementV1 {
    uint256 public getTotalPriorityTxs;
    uint256 public getPriorityTreeStartIndex;
    uint256 public getPriorityTreeHeight;
    bytes32 public getPriorityTreeRoot = keccak256("");
    uint256 public getFirstUnprocessedPriorityTx;
    bool public isPriorityQueueActive;
    uint256 public getTotalBatchesCommitted;
    uint256 public getTotalBatchesVerified;
    uint256 public getTotalBatchesExecuted;
    address public getSyscoinPriorityCommitPolicy;
    mapping(uint256 => uint256) public getPriorityTransactionTimestamp;
    mapping(uint256 => bytes32) public storedBatchHash;

    constructor() {
        storedBatchHash[0] = keccak256("genesis stored batch");
    }

    function getChainId() external pure returns (uint256) {
        return 57;
    }

    function setTree(uint256 total, uint256 height, bytes32 root) external {
        getTotalPriorityTxs = total;
        getPriorityTreeHeight = height;
        getPriorityTreeRoot = root;
    }

    function setRequestTimestamp(uint256 index, uint256 timestamp) external {
        getPriorityTransactionTimestamp[index] = timestamp;
    }

    function install(IZkSysPriorityCommitAdmissionV1 policy) external {
        getSyscoinPriorityCommitPolicy = address(policy);
    }

    function commit(bytes32 hash, uint256 priorityCount, bytes32 priorityHash) external {
        uint64 next = uint64(getTotalBatchesCommitted + 1);
        IZkSysPriorityCommitAdmissionV1(getSyscoinPriorityCommitPolicy)
            .recordCommit(next, hash, priorityCount, priorityHash);
        storedBatchHash[next] = hash;
        getTotalBatchesCommitted = next;
    }

    function rewind(uint64 keep) external {
        IZkSysPriorityCommitAdmissionV1(getSyscoinPriorityCommitPolicy).recordRevert(keep);
        getTotalBatchesCommitted = keep;
    }
}

contract ZkSysPriorityCommitAdmissionV1Test is Test {
    bytes32 private constant POLICY = keccak256("priority policy");
    bytes32 private constant FIRST_BATCH = keccak256("first stored batch");
    bytes32 private constant SECOND_BATCH = keccak256("second stored batch");
    address private constant SEQUENCER = address(0xB0B);

    PriorityCommitNativeMock private nativeChain;
    ZkSysPriorityCommitAdmissionV1 private admission;
    bytes32[4] private leaves;

    function setUp() public {
        vm.warp(1_000);
        vm.roll(100);
        nativeChain = new PriorityCommitNativeMock();
        admission = _deploy(nativeChain, address(_source(nativeChain)));
        nativeChain.install(admission);
        for (uint256 i; i < leaves.length; ++i) {
            leaves[i] = keccak256(abi.encode("priority", i));
        }
    }

    function testReservedEmptyBatchRetainsItsDutyAfterRequestsBecomeOverdue() public {
        uint64 first = _reserve();
        assertEq(admission.reservation().requiredEnd, 0);
        assertEq(admission.reservation().rootBlockNumber, 100);

        _appendFour(nativeChain);
        vm.warp(1_060);
        vm.roll(101);
        assertEq(admission.preparedCommitment(), bytes32(0));
        assertTrue(admission.commitReady(1, FIRST_BATCH, 0, keccak256("")));
        assertFalse(admission.commitReady(1, bytes32(0), 0, keccak256("")));
        assertFalse(admission.commitReady(1, FIRST_BATCH, 0, bytes32(0)));
        assertFalse(admission.commitReady(2, FIRST_BATCH, 0, keccak256("")));
        nativeChain.commit(FIRST_BATCH, 0, keccak256(""));
        assertFalse(admission.commitReady(1, FIRST_BATCH, 0, keccak256("")));

        uint64 second = _reserve();
        assertGt(second, first);
        assertEq(admission.reservation().requiredEnd, 1);
        assertFalse(admission.commitReady(2, SECOND_BATCH, 0, keccak256("")));
        vm.expectRevert(ZkSysPriorityCommitAdmissionV1.InvalidWitness.selector);
        nativeChain.commit(SECOND_BATCH, 0, keccak256(""));
        _publishOne(second, SECOND_BATCH, 0, false);
        nativeChain.commit(SECOND_BATCH, 1, _rolling(0));

        (bytes32 digest, uint64 from, uint64 to) = admission.admittedRange(1, 2);
        assertEq(digest, _expectedRange());
        assertEq(from, 0);
        assertEq(to, 1);
    }

    function testRewindPreservesPrefixAndRequiresFreshAdmissionForIdenticalRecommit() public {
        _appendFour(nativeChain);
        _publishOne(_reserve(), FIRST_BATCH, 0, false);
        nativeChain.commit(FIRST_BATCH, 1, _rolling(0));
        _publishOne(_reserve(), SECOND_BATCH, 1, false);
        nativeChain.commit(SECOND_BATCH, 1, _rolling(1));
        uint64 next = _reserve();
        assertEq(admission.committedPriorityCursor(), 2);

        nativeChain.rewind(1);
        assertEq(admission.committedPriorityCursor(), 1);
        assertEq(admission.reservation().id, 0);
        assertEq(nativeChain.storedBatchHash(2), SECOND_BATCH);
        vm.expectRevert(ZkSysPriorityCommitAdmissionV1.InvalidRange.selector);
        admission.admittedRange(1, 2);

        uint64 replacement = _reserve();
        assertGt(replacement, next);
        assertEq(admission.reservation().batchNumber, 2);
        assertFalse(admission.commitReady(2, SECOND_BATCH, 1, _rolling(1)));
        _publishOne(replacement, SECOND_BATCH, 1, false);
        assertTrue(admission.commitReady(2, SECOND_BATCH, 1, _rolling(1)));
        nativeChain.commit(SECOND_BATCH, 1, _rolling(1));
        (bytes32 digest, uint64 from, uint64 to) = admission.admittedRange(1, 2);
        assertEq(digest, _expectedRange());
        assertEq(from, 0);
        assertEq(to, 2);
    }

    function testGatewayLaneWaitsForDeliveryAndProvesAgainstBothTrees() public {
        PriorityCommitNativeMock rootMailbox = new PriorityCommitNativeMock();
        ZkSysRootPrioritySourceV1 source = _source(rootMailbox);
        ZkSysPriorityCheckpointReceiverV1 receiver = new ZkSysPriorityCheckpointReceiverV1(address(source), 57, POLICY);
        admission = _deploy(nativeChain, address(receiver));
        nativeChain.install(admission);
        _appendFour(rootMailbox);
        PriorityCheckpointV1 memory checkpoint = source.capture();
        vm.prank(receiver.aliasedRootSource());
        receiver.receiveCheckpoint(57, POLICY, checkpoint);

        vm.expectRevert(ZkSysPriorityCommitAdmissionV1.InvalidCheckpoint.selector);
        _reserve();
        assertEq(admission.lastReservationId(), 0);
        nativeChain.setTree(2, 1, _hash(leaves[0], leaves[1]));
        uint64 id = _reserve();
        assertEq(admission.reservation().rootTotal, 4);
        assertEq(admission.reservation().localTotal, 2);
        vm.expectRevert(ZkSysPriorityCommitAdmissionV1.InvalidWitness.selector);
        _publishOne(id, FIRST_BATCH, 0, false);
        _publishOne(id, FIRST_BATCH, 0, true);
        nativeChain.commit(FIRST_BATCH, 1, _rolling(0));
        assertEq(admission.committedPriorityCursor(), 1);
    }

    function testOnlySequencerCanReplaceAnUncommittedReservation() public {
        vm.expectRevert(ZkSysPriorityCommitAdmissionV1.Unauthorized.selector);
        admission.reserve();
        _appendFour(nativeChain);
        uint64 cancelled = _reserve();
        _publishOne(cancelled, FIRST_BATCH, 0, false);
        vm.expectRevert(ZkSysPriorityCommitAdmissionV1.Unauthorized.selector);
        admission.cancelReservation(cancelled);
        vm.prank(SEQUENCER);
        admission.cancelReservation(cancelled);
        uint64 replacement = _reserve();
        assertGt(replacement, cancelled);
        assertFalse(admission.commitReady(1, FIRST_BATCH, 1, _rolling(0)));
        vm.expectRevert(ZkSysPriorityCommitAdmissionV1.InvalidReservation.selector);
        _publishOne(cancelled, FIRST_BATCH, 0, false);
        _publishOne(replacement, FIRST_BATCH, 0, false);

        vm.expectRevert(ZkSysPriorityCommitAdmissionV1.Unauthorized.selector);
        admission.recordCommit(1, FIRST_BATCH, 1, _rolling(0));
        nativeChain.commit(FIRST_BATCH, 1, _rolling(0));
    }

    function _source(PriorityCommitNativeMock mailbox) private returns (ZkSysRootPrioritySourceV1) {
        return new ZkSysRootPrioritySourceV1(mailbox, IL1BridgehubMinimal(address(this)), address(0), 0, POLICY, 100);
    }

    function _deploy(PriorityCommitNativeMock mailbox, address source)
        private
        returns (ZkSysPriorityCommitAdmissionV1)
    {
        return new ZkSysPriorityCommitAdmissionV1(address(mailbox), source, SEQUENCER, POLICY, 50, 2);
    }

    function _reserve() private returns (uint64) {
        vm.prank(SEQUENCER);
        return admission.reserve();
    }

    function _publishOne(uint64 id, bytes32 hash, uint256 index, bool distinctLocalTree) private {
        bytes32[] memory items = new bytes32[](1);
        items[0] = leaves[index];
        bytes32[] memory rootPath = new bytes32[](2);
        rootPath[0] = leaves[1 - index];
        rootPath[1] = _hash(leaves[2], leaves[3]);
        bytes32[] memory localPath = new bytes32[](distinctLocalTree ? 1 : 0);
        if (distinctLocalTree) localPath[0] = rootPath[0];
        vm.prank(SEQUENCER);
        admission.publishWitness(id, hash, items, rootPath, rootPath, localPath, localPath);
    }

    function _appendFour(PriorityCommitNativeMock mailbox) private {
        mailbox.setTree(4, 2, _hash(_hash(leaves[0], leaves[1]), _hash(leaves[2], leaves[3])));
        for (uint256 i; i < leaves.length; ++i) {
            mailbox.setRequestTimestamp(i, 500 + i);
        }
    }

    function _rolling(uint256 index) private view returns (bytes32) {
        return _hash(keccak256(""), leaves[index]);
    }

    function _expectedRange() private view returns (bytes32) {
        bytes32 seed =
            keccak256(abi.encode(keccak256("ZkSysPriorityRangeV1"), address(nativeChain), uint64(1), uint64(2)));
        return keccak256(abi.encode(keccak256(abi.encode(seed, FIRST_BATCH)), SECOND_BATCH));
    }

    function _hash(bytes32 a, bytes32 b) private pure returns (bytes32) {
        return keccak256(abi.encodePacked(a, b));
    }
}
