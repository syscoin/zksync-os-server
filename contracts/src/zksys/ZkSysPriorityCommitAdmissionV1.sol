// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {IZkSysPriorityCommitAdmissionV1, PriorityCommitReservationV1} from "./IZkSysPriorityCommitAdmissionV1.sol";
import {
    IZkSysPriorityMailboxV1,
    IZkSysPriorityCheckpointReceiverV1,
    PriorityCheckpointV1
} from "./ZkSysPriorityGuardV1.sol";
import {ZkSysServiceTypesV1} from "./ZkSysServiceTypesV1.sol";

interface IZkSysNativePriorityCommitSettlementV1 is IZkSysPriorityMailboxV1 {
    function getSyscoinPriorityCommitPolicy() external view returns (address);
    function storedBatchHash(uint256 batchNumber) external view returns (bytes32);
}

/// @notice Authenticates the next batch's priority prefix before the native commitment is admitted.
contract ZkSysPriorityCommitAdmissionV1 is IZkSysPriorityCommitAdmissionV1 {
    bytes32 private constant RANGE_DOMAIN = keccak256("ZkSysPriorityRangeV1");
    bytes32 private constant EMPTY_PRIORITY_HASH = keccak256("");

    struct Admission {
        bytes32 storedHash;
        uint64 priorityEnd;
    }

    error Unauthorized();
    error InvalidConfiguration();
    error InvalidFrontier();
    error InvalidCheckpoint();
    error InvalidReservation();
    error InvalidWitness();
    error InvalidRange();

    address public immutable chain;
    IZkSysPriorityCheckpointReceiverV1 public immutable checkpointReceiver;
    address public immutable sequencer;
    bytes32 public immutable policyHash;
    uint64 public immutable maxSnapshotAge;
    uint64 public immutable maxPriorityPerPackage;
    uint64 public immutable initialBatch;
    uint64 public immutable initialPriorityCursor;
    bytes32 public immutable initialStoredHash;

    uint64 public lastCommittedBatch;
    uint64 public committedPriorityCursor;
    uint64 public lastReservationId;
    bytes32 public preparedCommitment;
    PriorityCommitReservationV1 private _reservation;
    mapping(uint64 batchNumber => Admission admission) private _admissions;

    event ReservationOpened(
        uint64 indexed id, uint64 indexed batchNumber, uint64 rootBlockNumber, uint64 priorityFrom, uint64 requiredEnd
    );
    event ReservationCancelled(uint64 indexed id);
    event WitnessPrepared(uint64 indexed id, bytes32 indexed storedHash, uint256 priorityCount, bytes32 priorityHash);
    event CommitAdmitted(uint64 indexed batchNumber, bytes32 indexed storedHash, uint64 priorityEnd);
    event CommitRewound(uint64 indexed batchNumber, uint64 priorityEnd);

    constructor(
        address chain_,
        address checkpointReceiver_,
        address sequencer_,
        bytes32 policyHash_,
        uint64 maxSnapshotAge_,
        uint64 maxPriorityPerPackage_
    ) {
        IZkSysNativePriorityCommitSettlementV1 nativeChain = IZkSysNativePriorityCommitSettlementV1(chain_);
        IZkSysPriorityCheckpointReceiverV1 receiver = IZkSysPriorityCheckpointReceiverV1(checkpointReceiver_);
        if (
            chain_.code.length == 0 || checkpointReceiver_.code.length == 0 || sequencer_ == address(0)
                || policyHash_ == bytes32(0) || maxSnapshotAge_ == 0 || maxPriorityPerPackage_ < 2
                || maxPriorityPerPackage_ > 1024 || receiver.policyHash() != policyHash_
                || receiver.childChainId() != nativeChain.getChainId() || receiver.childChainId() == 0
        ) revert InvalidConfiguration();

        uint256 batch = nativeChain.getTotalBatchesCommitted();
        uint256 cursor = nativeChain.getFirstUnprocessedPriorityTx();
        bytes32 storedHash = nativeChain.storedBatchHash(batch);
        if (
            batch != nativeChain.getTotalBatchesVerified() || batch != nativeChain.getTotalBatchesExecuted()
                || cursor != nativeChain.getTotalPriorityTxs() || nativeChain.isPriorityQueueActive()
                || batch > type(uint64).max || cursor > type(uint64).max || storedHash == bytes32(0)
        ) revert InvalidConfiguration();

        chain = chain_;
        checkpointReceiver = receiver;
        sequencer = sequencer_;
        policyHash = policyHash_;
        maxSnapshotAge = maxSnapshotAge_;
        maxPriorityPerPackage = maxPriorityPerPackage_;
        initialBatch = uint64(batch);
        initialPriorityCursor = uint64(cursor);
        initialStoredHash = storedHash;
        lastCommittedBatch = uint64(batch);
        committedPriorityCursor = uint64(cursor);
    }

    modifier onlySequencer() {
        if (msg.sender != sequencer) revert Unauthorized();
        if (!_nativeFrontierValid()) revert InvalidFrontier();
        _;
    }

    modifier onlyNativeChain() {
        if (msg.sender != chain) revert Unauthorized();
        if (!_nativeFrontierValid()) revert InvalidFrontier();
        _;
    }

    function reservation() external view returns (PriorityCommitReservationV1 memory) {
        return _reservation;
    }

    function reserve() external onlySequencer returns (uint64 id) {
        if (_reservation.id != 0) revert InvalidReservation();
        uint64 cursor = committedPriorityCursor;
        PriorityCheckpointV1 memory checkpoint = checkpointReceiver.latestCheckpoint();
        if (
            checkpoint.rootBlockNumber == 0 || checkpoint.rootTimestamp == 0
                || checkpoint.rootTimestamp > block.timestamp
                || block.timestamp - checkpoint.rootTimestamp > maxSnapshotAge || checkpoint.trackingStart > cursor
                || checkpoint.treeStart > checkpoint.trackingStart || checkpoint.overdueEnd < checkpoint.trackingStart
                || checkpoint.overdueEnd > checkpoint.total
                || !_validTree(checkpoint.root, checkpoint.treeStart, checkpoint.total, checkpoint.treeHeight, cursor)
        ) revert InvalidCheckpoint();

        uint64 requiredEnd = cursor;
        if (checkpoint.overdueEnd > cursor) ++requiredEnd;
        IZkSysNativePriorityCommitSettlementV1 nativeChain = IZkSysNativePriorityCommitSettlementV1(chain);
        uint256 localTotal = nativeChain.getTotalPriorityTxs();
        uint256 localStart = nativeChain.getPriorityTreeStartIndex();
        uint256 localHeight = nativeChain.getPriorityTreeHeight();
        bytes32 localRoot = nativeChain.getPriorityTreeRoot();
        // Do not reserve a batch for an overdue prefix that has yet to reach the settlement Mailbox.
        if (localTotal < requiredEnd || !_validTree(localRoot, localStart, localTotal, localHeight, cursor)) {
            revert InvalidCheckpoint();
        }

        id = ++lastReservationId;
        uint64 batch = lastCommittedBatch + 1;
        _reservation = PriorityCommitReservationV1({
            id: id,
            batchNumber: batch,
            priorityFrom: cursor,
            requiredEnd: requiredEnd,
            rootTotal: checkpoint.total,
            rootBlockNumber: checkpoint.rootBlockNumber,
            rootTreeStart: checkpoint.treeStart,
            rootTreeHeight: checkpoint.treeHeight,
            root: checkpoint.root,
            localTotal: uint64(localTotal),
            localTreeStart: uint64(localStart),
            localTreeHeight: uint64(localHeight),
            localRoot: localRoot
        });
        emit ReservationOpened(id, batch, checkpoint.rootBlockNumber, cursor, requiredEnd);
    }

    function cancelReservation(uint64 id) external onlySequencer {
        if (id == 0 || id != _reservation.id) revert InvalidReservation();
        _clearReservation();
        emit ReservationCancelled(id);
    }

    function publishWitness(
        uint64 id,
        bytes32 storedHash,
        bytes32[] calldata itemHashes,
        bytes32[] calldata leftPath,
        bytes32[] calldata rightPath,
        bytes32[] calldata localLeftPath,
        bytes32[] calldata localRightPath
    ) external onlySequencer {
        PriorityCommitReservationV1 memory pending = _reservation;
        if (id == 0 || id != pending.id) revert InvalidReservation();
        uint256 count = itemHashes.length;
        uint256 end = uint256(pending.priorityFrom) + count;
        if (
            storedHash == bytes32(0) || count == 0 || count > maxPriorityPerPackage / 2 || end < pending.requiredEnd
                || end > pending.rootTotal || end > pending.localTotal
        ) revert InvalidWitness();

        _verifyRange(
            pending.root,
            pending.rootTreeHeight,
            pending.priorityFrom - pending.rootTreeStart,
            itemHashes,
            leftPath,
            rightPath
        );
        if (
            pending.localRoot == pending.root && pending.localTreeHeight == pending.rootTreeHeight
                && pending.localTreeStart == pending.rootTreeStart && pending.localTotal == pending.rootTotal
        ) {
            if (localLeftPath.length != 0 || localRightPath.length != 0) revert InvalidWitness();
        } else {
            // The relayed root proves the original queue order. The local root also proves delivery in that order.
            _verifyRange(
                pending.localRoot,
                pending.localTreeHeight,
                pending.priorityFrom - pending.localTreeStart,
                itemHashes,
                localLeftPath,
                localRightPath
            );
        }
        bytes32 rolling = EMPTY_PRIORITY_HASH;
        for (uint256 i; i < count; ++i) {
            rolling = keccak256(abi.encodePacked(rolling, itemHashes[i]));
        }
        preparedCommitment = keccak256(abi.encode(id, pending.batchNumber, storedHash, count, rolling));
        emit WitnessPrepared(id, storedHash, count, rolling);
    }

    function commitReady(uint64 batchNumber, bytes32 storedHash, uint256 priorityCount, bytes32 priorityHash)
        external
        view
        returns (bool)
    {
        return _nativeFrontierValid() && _matchesReservation(batchNumber, storedHash, priorityCount, priorityHash);
    }

    function recordCommit(uint64 batchNumber, bytes32 storedHash, uint256 priorityCount, bytes32 priorityHash)
        external
        onlyNativeChain
    {
        if (!_matchesReservation(batchNumber, storedHash, priorityCount, priorityHash)) {
            revert InvalidWitness();
        }
        uint64 end = uint64(uint256(committedPriorityCursor) + priorityCount);
        _admissions[batchNumber] = Admission(storedHash, end);
        lastCommittedBatch = batchNumber;
        committedPriorityCursor = end;
        _clearReservation();
        emit CommitAdmitted(batchNumber, storedHash, end);
    }

    function recordRevert(uint256 newLastBatch) external onlyNativeChain {
        IZkSysNativePriorityCommitSettlementV1 nativeChain = IZkSysNativePriorityCommitSettlementV1(chain);
        if (
            newLastBatch < initialBatch || newLastBatch >= lastCommittedBatch
                || newLastBatch < nativeChain.getTotalBatchesVerified()
        ) revert InvalidFrontier();

        uint64 keptBatch = uint64(newLastBatch);
        (bytes32 keptHash, uint64 cursor) = _admission(keptBatch);
        if (keptHash == bytes32(0) || keptHash != nativeChain.storedBatchHash(keptBatch)) revert InvalidFrontier();
        lastCommittedBatch = keptBatch;
        committedPriorityCursor = cursor;
        _clearReservation();
        emit CommitRewound(keptBatch, cursor);
    }

    function admittedRange(uint64 from, uint64 to)
        external
        view
        returns (bytes32 digest, uint64 priorityFrom, uint64 priorityEnd)
    {
        if (!_nativeFrontierValid()) revert InvalidFrontier();
        if (
            from <= initialBatch || to <= from || to > lastCommittedBatch
                || to - from >= ZkSysServiceTypesV1.MAX_BATCHES_PER_PACKAGE
        ) revert InvalidRange();
        IZkSysNativePriorityCommitSettlementV1 nativeChain = IZkSysNativePriorityCommitSettlementV1(chain);
        uint64 previous = from - 1;
        (bytes32 previousHash, uint64 cursor) = _admission(previous);
        if (previousHash == bytes32(0) || previousHash != nativeChain.storedBatchHash(previous)) {
            revert InvalidFrontier();
        }

        priorityFrom = cursor;
        priorityEnd = cursor;
        digest = keccak256(abi.encode(RANGE_DOMAIN, chain, from, to));
        for (uint256 i = from; i <= to; ++i) {
            Admission storage current = _admissions[uint64(i)];
            bytes32 hash = current.storedHash;
            if (hash == bytes32(0) || hash != nativeChain.storedBatchHash(i) || current.priorityEnd < priorityEnd) {
                revert InvalidRange();
            }
            digest = keccak256(abi.encode(digest, hash));
            priorityEnd = current.priorityEnd;
        }
    }

    function _admission(uint64 batch) private view returns (bytes32 hash, uint64 cursor) {
        if (batch == initialBatch) return (initialStoredHash, initialPriorityCursor);
        Admission storage current = _admissions[batch];
        return (current.storedHash, current.priorityEnd);
    }

    function _nativeFrontierValid() private view returns (bool) {
        IZkSysNativePriorityCommitSettlementV1 nativeChain = IZkSysNativePriorityCommitSettlementV1(chain);
        if (
            nativeChain.getSyscoinPriorityCommitPolicy() != address(this)
                || nativeChain.getTotalBatchesCommitted() != lastCommittedBatch || nativeChain.isPriorityQueueActive()
                || nativeChain.getFirstUnprocessedPriorityTx() > committedPriorityCursor
        ) return false;
        (bytes32 hash,) = _admission(lastCommittedBatch);
        return hash != bytes32(0) && nativeChain.storedBatchHash(lastCommittedBatch) == hash;
    }

    function _matchesReservation(uint64 batchNumber, bytes32 storedHash, uint256 priorityCount, bytes32 priorityHash)
        private
        view
        returns (bool)
    {
        PriorityCommitReservationV1 storage pending = _reservation;
        if (
            pending.id == 0 || pending.batchNumber != batchNumber || pending.priorityFrom != committedPriorityCursor
                || storedHash == bytes32(0)
        ) return false;
        if (priorityCount == 0) {
            return pending.requiredEnd == pending.priorityFrom && priorityHash == EMPTY_PRIORITY_HASH;
        }
        return preparedCommitment != bytes32(0)
            && preparedCommitment
                == keccak256(abi.encode(pending.id, batchNumber, storedHash, priorityCount, priorityHash));
    }

    function _clearReservation() private {
        delete _reservation;
        delete preparedCommitment;
    }

    function _validTree(bytes32 root, uint256 start, uint256 total, uint256 height, uint64 cursor)
        private
        pure
        returns (bool)
    {
        return root != bytes32(0) && start <= cursor && cursor <= total && total <= type(uint64).max && height <= 64
            && total - start <= uint256(1) << height;
    }

    /// @dev Mirrors native Merkle.calculateRootPaths for the reserved absolute queue range.
    function _verifyRange(
        bytes32 expectedRoot,
        uint64 height,
        uint256 index,
        bytes32[] memory items,
        bytes32[] calldata leftPath,
        bytes32[] calldata rightPath
    ) private pure {
        uint256 length = items.length;
        if (leftPath.length != height || rightPath.length != height || index + length > uint256(1) << height) {
            revert InvalidWitness();
        }
        for (uint256 level; level < height; ++level) {
            uint256 parity = index % 2;
            uint256 nextLength = length / 2 + (parity | (length % 2));
            for (uint256 i; i < nextLength; ++i) {
                bytes32 lhs = (i == 0 && parity == 1) ? leftPath[level] : items[2 * i - parity];
                bytes32 rhs =
                    (i == nextLength - 1 && (length - parity) % 2 == 1) ? rightPath[level] : items[2 * i - parity + 1];
                items[i] = keccak256(abi.encodePacked(lhs, rhs));
            }
            length = nextLength;
            index /= 2;
        }
        if (items[0] != expectedRoot) revert InvalidWitness();
    }
}
