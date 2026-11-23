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
import {IZkSysPriorityCommitAdmissionV1} from "contracts/src/zksys/IZkSysPriorityCommitAdmissionV1.sol";
import {RootDrawHub} from "./ZkSysRootDraw.t.sol";

contract PriorityMailboxMock is IZkSysPriorityMailboxV1 {
    uint256 public total;
    uint256 public treeStart;
    uint256 public height;
    bytes32 public root = keccak256("");
    uint256 public cursor;
    uint256 public committed;
    uint256 public verified;
    uint256 public executed;
    bool public legacy;
    mapping(uint256 => uint256) public requestedAt;
    mapping(uint256 => bytes32) public storedBatchHash;
    mapping(uint64 => MockPriorityRange) public committedPriorityRange;
    address public getSyscoinPriorityCommitPolicy;

    function installAdmission(address admission) external {
        getSyscoinPriorityCommitPolicy = admission;
    }

    function setAdmittedBatch(uint64 batchNumber, uint64 from, uint64 end) external {
        storedBatchHash[batchNumber] = keccak256(abi.encode(batchNumber, from, end));
        committedPriorityRange[batchNumber] = MockPriorityRange(from, end);
        if (batchNumber > committed) committed = batchNumber;
    }

    function setTree(uint256 total_, uint256 height_, bytes32 root_) external {
        total = total_;
        height = height_;
        root = root_;
    }

    function setTimestamp(uint256 index, uint256 value) external {
        requestedAt[index] = value;
    }

    function setProgress(uint256 verified_, uint256 executed_, uint256 cursor_) external {
        committed = verified_;
        verified = verified_;
        executed = executed_;
        cursor = cursor_;
    }

    function setCommitted(uint256 value) external {
        committed = value;
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

    function getTotalBatchesCommitted() external view returns (uint256) {
        return committed;
    }

    function getTotalBatchesExecuted() external view returns (uint256) {
        return executed;
    }
}

struct MockPriorityRange {
    uint64 from;
    uint64 end;
}

interface MockPriorityChain {
    function storedBatchHash(uint256 batchNumber) external view returns (bytes32);
    function committedPriorityRange(uint64 batchNumber) external view returns (uint64 from, uint64 end);
}

contract PriorityAdmissionMock {
    address public immutable chain;
    bytes32 public immutable policyHash;
    address public immutable sequencer;
    uint64 public immutable maxPriorityPerPackage;

    constructor(address chain_, bytes32 policyHash_, address sequencer_, uint64 limit_) {
        chain = chain_;
        policyHash = policyHash_;
        sequencer = sequencer_;
        maxPriorityPerPackage = limit_;
    }

    function admittedRange(uint64 from, uint64 to)
        external
        view
        returns (bytes32 digest, uint64 priorityFrom, uint64 priorityEnd)
    {
        MockPriorityChain native = MockPriorityChain(chain);
        (priorityFrom,) = native.committedPriorityRange(from);
        priorityEnd = priorityFrom;
        digest = keccak256(abi.encode(keccak256("ZkSysPriorityRangeV1"), chain, from, to));
        for (uint256 i = from; i <= to; ++i) {
            bytes32 hash = native.storedBatchHash(i);
            (uint64 start, uint64 end) = native.committedPriorityRange(uint64(i));
            require(hash != bytes32(0) && start == priorityEnd && end >= start, "no exact admission");
            priorityEnd = end;
            digest = keccak256(abi.encode(digest, hash));
        }
    }
}

contract ZkSysPriorityGuardTest is Test {
    PriorityMailboxMock private mailbox;
    RootDrawHub private hub;
    ZkSysRootPrioritySourceV1 private source;
    ZkSysPriorityCheckpointReceiverV1 private receiver;
    ZkSysPriorityGuardV1 private guard;
    PriorityAdmissionMock private admission;
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
        admission = new PriorityAdmissionMock(address(mailbox), POLICY, address(this), 2);
        guard = _newGuard();
        mailbox.installAdmission(address(admission));
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

    function testRootAgeUsesNativeTimestampsAndBoundedPrefix() public view {
        PriorityCheckpointV1 memory checkpoint = source.capture();
        assertEq(checkpoint.trackingStart, 0);
        assertEq(checkpoint.total, 4);
        assertEq(checkpoint.overdueEnd, 2);
        assertEq(checkpoint.rootTimestamp, 1_000);
        assertEq(checkpoint.treeHeight, 2);
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

    function testRootSourceRejectsMissingTimestampAndIncorrectHeight() public {
        mailbox.setTimestamp(2, 0);
        vm.expectRevert(ZkSysRootPrioritySourceV1.InvalidQueue.selector);
        source.capture();
        mailbox.setTree(4, 1, bytes32(uint256(1)));
        vm.expectRevert(ZkSysRootPrioritySourceV1.InvalidQueue.selector);
        source.capture();
    }

    function _newGuard() private returns (ZkSysPriorityGuardV1) {
        return new ZkSysPriorityGuardV1(
            address(this), mailbox, IZkSysPriorityCommitAdmissionV1(address(admission)), POLICY, 200, 2
        );
    }

    function testHistoricalRangesAdvanceAfterLaterPriorityRequestsBecomeDue() public {
        mailbox.setAdmittedBatch(1, 0, 0);
        mailbox.setAdmittedBatch(2, 0, 0);
        mailbox.setAdmittedBatch(3, 0, 1);
        mailbox.setAdmittedBatch(4, 1, 2);
        vm.warp(1_400);
        vm.roll(101);
        _relay();
        assertEq(receiver.latestCheckpoint().overdueEnd, 4);

        (bytes32 oldDigest,,) = admission.admittedRange(1, 2);
        guard.open(WORK, 1, 2);
        vm.warp(1_601);
        guard.refreshExpired(WORK);
        (, bytes32 refreshed, uint64 from, uint64 to, uint64 opened, uint64 priorityFrom, uint64 priorityEnd) =
            guard.work();
        assertEq(refreshed, oldDigest);
        assertEq(from, 1);
        assertEq(to, 2);
        assertEq(opened, 1_601);
        assertEq(priorityFrom, 0);
        assertEq(priorityEnd, 0);
        guard.consume(WORK, 2, oldDigest, 0);

        mailbox.setProgress(2, 0, 0);
        mailbox.setCommitted(4);
        (bytes32 nextDigest,,) = admission.admittedRange(3, 4);
        guard.open(WORK, 3, 4);
        guard.consume(WORK, 4, nextDigest, 2);
        assertEq(guard.priorityCursor(), 2);
        assertEq(guard.lastVerifiedBatch(), 4);
    }

    function testRepairKeepsStartAndClockAndUsesReplacementRange() public {
        mailbox.setAdmittedBatch(1, 0, 0);
        mailbox.setAdmittedBatch(2, 0, 1);
        mailbox.setAdmittedBatch(3, 1, 2);
        guard.open(WORK, 1, 2);
        vm.warp(1_050);
        guard.repair(WORK, 3);
        (bytes32 expected,,) = admission.admittedRange(1, 3);
        (bytes32 id, bytes32 digest, uint64 from, uint64 to, uint64 opened, uint64 priorityFrom, uint64 priorityEnd) =
            guard.work();
        assertEq(id, WORK);
        assertEq(digest, expected);
        assertEq(from, 1);
        assertEq(to, 3);
        assertEq(opened, 1_000);
        assertEq(priorityFrom, 0);
        assertEq(priorityEnd, 2);
        guard.consume(WORK, 3, expected, 2);
    }

    function testConstructorRequiresDrainedNativeFrontier() public {
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidConfiguration.selector);
        _newGuard();
        mailbox.setProgress(7, 6, 4);
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidConfiguration.selector);
        _newGuard();
        mailbox.setProgress(7, 7, 4);
        mailbox.setCommitted(8);
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidConfiguration.selector);
        _newGuard();
        mailbox.setCommitted(7);
        assertEq(_newGuard().priorityCursor(), 4);
    }

    function testAdmissionMustBeInstalledAndMatchTheSequencer() public {
        assertTrue(guard.isAdmissionActive(address(this)));
        assertFalse(guard.isAdmissionActive(address(123)));
        mailbox.installAdmission(address(123));
        assertFalse(guard.isAdmissionActive(address(this)));
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidConfiguration.selector);
        guard.open(WORK, 1, 2);
    }

    function testRenewalRequiresExpiryAndTheSameCanonicalRange() public {
        mailbox.setAdmittedBatch(1, 0, 1);
        mailbox.setAdmittedBatch(2, 1, 2);
        guard.open(WORK, 1, 2);
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidWork.selector);
        guard.refreshExpired(WORK);
        vm.warp(1_201);
        (bytes32 digest,,) = admission.admittedRange(1, 2);
        vm.expectRevert(ZkSysPriorityGuardV1.ExpiredWork.selector);
        guard.consume(WORK, 2, digest, 2);
        guard.refreshExpired(WORK);
        guard.consume(WORK, 2, digest, 2);
    }

    function testNativeOutOfBandProofFailsClosed() public {
        mailbox.setProgress(1, 0, 0);
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidWork.selector);
        guard.open(WORK, 1, 2);
    }
}
