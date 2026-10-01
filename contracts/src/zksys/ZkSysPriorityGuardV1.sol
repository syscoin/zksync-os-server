// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {IL1BridgehubMinimal} from "./ZkSysRegistryBridge.sol";

interface IZkSysPriorityMailboxV1 {
    function getChainId() external view returns (uint256);
    function getTotalPriorityTxs() external view returns (uint256);
    function getPriorityTreeStartIndex() external view returns (uint256);
    function getPriorityTreeRoot() external view returns (bytes32);
    function getPriorityTreeHeight() external view returns (uint256);
    function getPriorityTransactionTimestamp(uint256 index) external view returns (uint256);
    function getFirstUnprocessedPriorityTx() external view returns (uint256);
    function isPriorityQueueActive() external view returns (bool);
    function getTotalBatchesVerified() external view returns (uint256);
    function getTotalBatchesExecuted() external view returns (uint256);
}

struct PriorityCheckpointV1 {
    uint64 trackingStart;
    uint64 treeStart;
    uint64 total;
    uint64 overdueEnd;
    uint64 treeHeight;
    uint64 rootBlockNumber;
    uint64 rootTimestamp;
    bytes32 root;
}

interface IZkSysPriorityCheckpointReceiverV1 {
    function receiveCheckpoint(uint256 childChainId_, bytes32 policyHash_, PriorityCheckpointV1 calldata checkpoint)
        external;
    function latestCheckpoint() external view returns (PriorityCheckpointV1 memory);
    function childChainId() external view returns (uint256);
    function policyHash() external view returns (bytes32);
}

interface IZkSysPriorityGuardV1 {
    function acceptanceGate() external view returns (address);
    function chain() external view returns (IZkSysPriorityMailboxV1);
    function policyHash() external view returns (bytes32);
    function open(bytes32 workId, uint64 batchFrom) external;
    function refreshExpired(bytes32 workId) external;
    function consume(bytes32 workId, uint64 batchTo, uint256[] calldata counts, bytes32[] calldata hashes) external;
}

/// @notice NEVM source of authenticated queue age and ordered priority-tree checkpoints.
/// @dev Timestamps come from the canonical root Mailbox. Existing operations must be drained before
/// the guarded lane starts: timestamp coverage begins at this contract's immutable deployment cursor.
contract ZkSysRootPrioritySourceV1 {
    error InvalidConfiguration();
    error InvalidQueue();

    IZkSysPriorityMailboxV1 public immutable mailbox;
    IL1BridgehubMinimal public immutable bridgehub;
    address public immutable gatewayReceiver;
    uint256 public immutable gatewayChainId;
    uint256 public immutable childChainId;
    bytes32 public immutable policyHash;
    uint64 public immutable trackingStart;
    uint64 public immutable inclusionDelay;

    event CheckpointRelayed(bytes32 indexed checkpointHash, bytes32 indexed canonicalTxHash);

    constructor(
        IZkSysPriorityMailboxV1 mailbox_,
        IL1BridgehubMinimal bridgehub_,
        address gatewayReceiver_,
        uint256 gatewayChainId_,
        bytes32 policyHash_,
        uint64 inclusionDelay_
    ) {
        if (
            address(mailbox_).code.length == 0 || address(bridgehub_).code.length == 0
                || (gatewayReceiver_ == address(0)) != (gatewayChainId_ == 0) || inclusionDelay_ == 0
                || policyHash_ == bytes32(0) || mailbox_.getChainId() == 0
        ) revert InvalidConfiguration();
        mailbox = mailbox_;
        bridgehub = bridgehub_;
        gatewayReceiver = gatewayReceiver_;
        gatewayChainId = gatewayChainId_;
        childChainId = mailbox_.getChainId();
        policyHash = policyHash_;
        inclusionDelay = inclusionDelay_;
        uint256 total = mailbox_.getTotalPriorityTxs();
        if (total > type(uint64).max || mailbox_.isPriorityQueueActive()) revert InvalidQueue();
        trackingStart = uint64(total);
    }

    function capture() public view returns (PriorityCheckpointV1 memory checkpoint) {
        uint256 total = mailbox.getTotalPriorityTxs();
        uint256 treeStart = mailbox.getPriorityTreeStartIndex();
        uint256 height = mailbox.getPriorityTreeHeight();
        if (
            mailbox.isPriorityQueueActive() || mailbox.getChainId() != childChainId || total < trackingStart
                || total > type(uint64).max || treeStart > trackingStart || height > 64
                || block.number > type(uint64).max || block.timestamp > type(uint64).max
                || total - treeStart > uint256(1) << height
        ) revert InvalidQueue();
        uint256 left = trackingStart;
        uint256 right = total;
        uint256 cutoff = block.timestamp > inclusionDelay ? block.timestamp - inclusionDelay : 0;
        // Root Mailbox appends timestamps in transaction order, so this takes at most 64 reads.
        while (left < right) {
            uint256 mid = left + (right - left) / 2;
            uint256 requestedAt = mailbox.getPriorityTransactionTimestamp(mid);
            if (requestedAt == 0 || requestedAt > block.timestamp) revert InvalidQueue();
            if (requestedAt <= cutoff) left = mid + 1;
            else right = mid;
        }
        checkpoint = PriorityCheckpointV1({
            trackingStart: trackingStart,
            treeStart: uint64(treeStart),
            total: uint64(total),
            overdueEnd: uint64(left),
            treeHeight: uint64(height),
            rootBlockNumber: uint64(block.number),
            rootTimestamp: uint64(block.timestamp),
            root: mailbox.getPriorityTreeRoot()
        });
        if (checkpoint.root == bytes32(0)) revert InvalidQueue();
    }

    function relayCheckpoint(uint256 gasLimit, uint256 gasPerPubdataByteLimit, address refundRecipient)
        external
        payable
        returns (bytes32 canonicalTxHash)
    {
        if (gatewayReceiver == address(0)) revert InvalidConfiguration();
        PriorityCheckpointV1 memory checkpoint = capture();
        canonicalTxHash = bridgehub.requestL2TransactionDirect{value: msg.value}(
            IL1BridgehubMinimal.L2TransactionRequestDirect({
                chainId: gatewayChainId,
                mintValue: msg.value,
                l2Contract: gatewayReceiver,
                l2Value: 0,
                l2Calldata: abi.encodeCall(
                    IZkSysPriorityCheckpointReceiverV1.receiveCheckpoint, (childChainId, policyHash, checkpoint)
                ),
                l2GasLimit: gasLimit,
                l2GasPerPubdataByteLimit: gasPerPubdataByteLimit,
                factoryDeps: new bytes[](0),
                refundRecipient: refundRecipient == address(0) ? msg.sender : refundRecipient
            })
        );
        emit CheckpointRelayed(keccak256(abi.encode(checkpoint)), canonicalTxHash);
    }

    /// @dev A root-settled Gateway guard can read this same native source directly. No relay or
    /// caller-provided clock is needed when source and guard execute on the same settlement chain.
    function latestCheckpoint() external view returns (PriorityCheckpointV1 memory) {
        return capture();
    }
}

/// @notice Gateway checkpoint inbox authenticated by the canonical NEVM-to-Gateway sender alias.
contract ZkSysPriorityCheckpointReceiverV1 is IZkSysPriorityCheckpointReceiverV1 {
    error UnauthorizedSource();
    error InvalidCheckpoint();
    address public immutable aliasedRootSource;
    uint256 public immutable childChainId;
    bytes32 public immutable policyHash;
    PriorityCheckpointV1 private _latest;

    constructor(address rootSource, uint256 childChainId_, bytes32 policyHash_) {
        if (rootSource == address(0) || childChainId_ == 0 || policyHash_ == bytes32(0)) revert InvalidCheckpoint();
        unchecked {
            aliasedRootSource = address(uint160(rootSource) + uint160(0x1111000000000000000000000000000000001111));
        }
        childChainId = childChainId_;
        policyHash = policyHash_;
    }

    function receiveCheckpoint(uint256 childChainId_, bytes32 policyHash_, PriorityCheckpointV1 calldata checkpoint)
        external
    {
        if (msg.sender != aliasedRootSource) revert UnauthorizedSource();
        if (
            childChainId_ != childChainId || policyHash_ != policyHash || checkpoint.root == bytes32(0)
                || checkpoint.rootBlockNumber == 0 || checkpoint.rootTimestamp == 0
                || checkpoint.treeStart > checkpoint.trackingStart || checkpoint.trackingStart > checkpoint.overdueEnd
                || checkpoint.overdueEnd > checkpoint.total || checkpoint.treeHeight > 64
                || checkpoint.total - checkpoint.treeStart > uint256(1) << checkpoint.treeHeight
        ) revert InvalidCheckpoint();
        PriorityCheckpointV1 memory previous = _latest;
        if (checkpoint.rootBlockNumber < previous.rootBlockNumber) return;
        if (checkpoint.rootBlockNumber == previous.rootBlockNumber) {
            if (keccak256(abi.encode(checkpoint)) == keccak256(abi.encode(previous))) return;
            // Two root transactions in one block can observe different appended queue prefixes.
            if (checkpoint.total <= previous.total) revert InvalidCheckpoint();
        }
        if (
            previous.rootBlockNumber != 0
                && (checkpoint.trackingStart != previous.trackingStart
                    || checkpoint.treeStart != previous.treeStart
                    || checkpoint.total < previous.total
                    || checkpoint.overdueEnd < previous.overdueEnd
                    || checkpoint.rootTimestamp < previous.rootTimestamp)
        ) revert InvalidCheckpoint();
        _latest = checkpoint;
    }

    function latestCheckpoint() external view returns (PriorityCheckpointV1 memory) {
        return _latest;
    }
}

/// @notice Requires each native proof package to consume the frozen, bounded overdue root prefix.
/// @dev A proof cursor, separate from the native execution cursor, prevents delayed execution from
/// crediting the same queue entries twice. Withholding checkpoints or root-to-Gateway delivery stops
/// progress; this contract cannot make the parent Gateway include transactions or guarantee wall time.
contract ZkSysPriorityGuardV1 is IZkSysPriorityGuardV1 {
    error Unauthorized();
    error InvalidConfiguration();
    error InvalidCheckpoint();
    error InvalidWork();
    error ExpiredWork();
    error InvalidPrefix();

    struct Work {
        bytes32 id;
        bytes32 checkpointHash;
        bytes32 root;
        uint64 batchFrom;
        uint64 openedAt;
        uint64 treeStart;
        uint64 treeHeight;
        uint64 requiredEnd;
        uint64 maxEnd;
    }

    address public immutable acceptanceGate;
    IZkSysPriorityMailboxV1 public immutable chain;
    IZkSysPriorityCheckpointReceiverV1 public immutable checkpointReceiver;
    bytes32 public immutable policyHash;
    uint64 public immutable maxSnapshotAge;
    uint64 public immutable maxProofWorkSeconds;
    uint64 public immutable maxPriorityPerPackage;
    uint64 public priorityCursor;
    uint64 public lastVerifiedBatch;
    Work public work;
    mapping(bytes32 => bool) public prefixWitness;

    event WorkFrozen(bytes32 indexed workId, bytes32 indexed checkpointHash, uint64 cursor, uint64 requiredEnd);
    event PrefixConsumed(bytes32 indexed workId, uint64 batchTo, uint64 cursor);

    constructor(
        address gate_,
        IZkSysPriorityMailboxV1 chain_,
        IZkSysPriorityCheckpointReceiverV1 receiver_,
        bytes32 policyHash_,
        uint64 maxSnapshotAge_,
        uint64 maxProofWorkSeconds_,
        uint64 maxPriorityPerPackage_
    ) {
        if (
            gate_ == address(0) || address(chain_).code.length == 0 || address(receiver_).code.length == 0
                || policyHash_ == bytes32(0) || receiver_.policyHash() != policyHash_
                || receiver_.childChainId() != chain_.getChainId() || maxSnapshotAge_ == 0 || maxProofWorkSeconds_ == 0
                || maxPriorityPerPackage_ == 0 || maxPriorityPerPackage_ > 1024
        ) revert InvalidConfiguration();
        uint256 verified = chain_.getTotalBatchesVerified();
        uint256 cursor = chain_.getFirstUnprocessedPriorityTx();
        if (
            verified != chain_.getTotalBatchesExecuted() || verified > type(uint64).max || cursor > type(uint64).max
                || chain_.isPriorityQueueActive()
        ) revert InvalidConfiguration();
        acceptanceGate = gate_;
        chain = chain_;
        checkpointReceiver = receiver_;
        policyHash = policyHash_;
        maxSnapshotAge = maxSnapshotAge_;
        maxProofWorkSeconds = maxProofWorkSeconds_;
        maxPriorityPerPackage = maxPriorityPerPackage_;
        priorityCursor = uint64(cursor);
        lastVerifiedBatch = uint64(verified);
    }

    modifier onlyGate() {
        if (msg.sender != acceptanceGate) revert Unauthorized();
        _checkNativeCursor();
        _;
    }

    function open(bytes32 workId, uint64 batchFrom) external onlyGate {
        if (work.id != bytes32(0) || workId == bytes32(0) || uint256(batchFrom) != uint256(lastVerifiedBatch) + 1) {
            revert InvalidWork();
        }
        _freeze(workId, batchFrom);
    }

    function refreshExpired(bytes32 workId) external onlyGate {
        Work memory previous = work;
        if (previous.id != workId || workId == bytes32(0)) revert InvalidWork();
        if (block.timestamp <= uint256(previous.openedAt) + maxProofWorkSeconds) revert InvalidWork();
        _freeze(workId, previous.batchFrom);
    }

    function _freeze(bytes32 workId, uint64 batchFrom) private {
        PriorityCheckpointV1 memory checkpoint = checkpointReceiver.latestCheckpoint();
        if (
            checkpoint.rootBlockNumber == 0 || checkpoint.root == bytes32(0)
                || checkpoint.rootTimestamp > block.timestamp
                || block.timestamp - checkpoint.rootTimestamp > maxSnapshotAge
                || checkpoint.trackingStart > priorityCursor || checkpoint.treeStart > priorityCursor
                || checkpoint.treeStart != chain.getPriorityTreeStartIndex() || checkpoint.total < priorityCursor
                || checkpoint.overdueEnd > checkpoint.total || checkpoint.treeHeight > 64
                || block.timestamp > type(uint64).max
        ) revert InvalidCheckpoint();
        uint256 cap = uint256(priorityCursor) + maxPriorityPerPackage;
        uint64 requiredEnd = checkpoint.overdueEnd > priorityCursor ? checkpoint.overdueEnd : priorityCursor;
        if (requiredEnd > cap) requiredEnd = uint64(cap);
        uint64 maxEnd = checkpoint.total;
        if (maxEnd > cap) maxEnd = uint64(cap);
        work = Work({
            id: workId,
            checkpointHash: keccak256(abi.encode(checkpoint, block.timestamp)),
            root: checkpoint.root,
            batchFrom: batchFrom,
            openedAt: uint64(block.timestamp),
            treeStart: checkpoint.treeStart,
            treeHeight: checkpoint.treeHeight,
            requiredEnd: requiredEnd,
            maxEnd: maxEnd
        });
        emit WorkFrozen(workId, work.checkpointHash, priorityCursor, requiredEnd);
    }

    function publishPrefixWitness(
        bytes32 workId,
        uint256[] calldata batchCounts,
        bytes32[] calldata itemHashes,
        bytes32[] calldata leftPath,
        bytes32[] calldata rightPath
    ) external {
        Work memory pending = work;
        _checkWork(pending, workId);
        if (batchCounts.length < 2 || batchCounts.length > 100 || itemHashes.length > maxPriorityPerPackage) {
            revert InvalidPrefix();
        }
        uint256 end = uint256(priorityCursor) + itemHashes.length;
        if (end < pending.requiredEnd || end > pending.maxEnd) revert InvalidPrefix();
        if (itemHashes.length == 0) {
            if (leftPath.length != 0 || rightPath.length != 0) revert InvalidPrefix();
        } else if (
            leftPath.length != pending.treeHeight || rightPath.length != pending.treeHeight
                || _rangeRoot(leftPath, rightPath, priorityCursor - pending.treeStart, itemHashes) != pending.root
        ) {
            revert InvalidPrefix();
        }
        bytes32[] memory hashes = new bytes32[](batchCounts.length);
        uint256 offset;
        for (uint256 i; i < batchCounts.length; ++i) {
            uint256 count = batchCounts[i];
            if (count > itemHashes.length - offset) revert InvalidPrefix();
            bytes32 rolling = keccak256("");
            for (uint256 j; j < count; ++j) {
                rolling = keccak256(abi.encodePacked(rolling, itemHashes[offset++]));
            }
            hashes[i] = rolling;
        }
        if (offset != itemHashes.length) revert InvalidPrefix();
        prefixWitness[keccak256(abi.encode(pending.id, pending.checkpointHash, batchCounts, hashes))] = true;
    }

    function consume(bytes32 workId, uint64 batchTo, uint256[] calldata counts, bytes32[] calldata hashes)
        external
        onlyGate
    {
        Work memory pending = work;
        _checkWork(pending, workId);
        if (
            counts.length < 2 || counts.length > 100 || hashes.length != counts.length
                || batchTo != uint256(pending.batchFrom) + counts.length - 1
        ) revert InvalidPrefix();
        uint256 total;
        for (uint256 i; i < counts.length; ++i) {
            if (counts[i] > maxPriorityPerPackage || total + counts[i] > maxPriorityPerPackage) revert InvalidPrefix();
            total += counts[i];
            if (counts[i] == 0 && hashes[i] != keccak256("")) revert InvalidPrefix();
        }
        uint256 end = uint256(priorityCursor) + total;
        if (end < pending.requiredEnd || end > pending.maxEnd) revert InvalidPrefix();
        if (total != 0 && !prefixWitness[keccak256(abi.encode(pending.id, pending.checkpointHash, counts, hashes))]) {
            revert InvalidPrefix();
        }
        priorityCursor = uint64(end);
        lastVerifiedBatch = batchTo;
        delete work;
        emit PrefixConsumed(workId, batchTo, uint64(end));
    }

    function _checkNativeCursor() private view {
        if (
            chain.isPriorityQueueActive() || chain.getTotalBatchesVerified() != lastVerifiedBatch
                || chain.getTotalBatchesExecuted() > lastVerifiedBatch
                || chain.getFirstUnprocessedPriorityTx() > priorityCursor
        ) revert InvalidWork();
    }

    function _checkWork(Work memory pending, bytes32 workId) private view {
        if (workId == bytes32(0) || pending.id != workId) revert InvalidWork();
        if (block.timestamp > uint256(pending.openedAt) + maxProofWorkSeconds) revert ExpiredWork();
    }

    /// @dev Ordered range construction mirrors native Merkle.calculateRootPaths. Exact authenticated
    /// height is checked by the caller; accepting arbitrary shorter paths permits subtree substitution.
    function _rangeRoot(
        bytes32[] calldata leftPath,
        bytes32[] calldata rightPath,
        uint256 index,
        bytes32[] memory items
    ) private pure returns (bytes32) {
        uint256 length = items.length;
        if (index + length > uint256(1) << leftPath.length) revert InvalidPrefix();
        for (uint256 level; level < leftPath.length; ++level) {
            uint256 parity = index % 2;
            uint256 nextLength = length / 2 + (parity | (length % 2));
            for (uint256 i; i < nextLength; ++i) {
                bytes32 lhs = (i == 0 && parity == 1) ? leftPath[level] : items[2 * i - parity];
                bytes32 rhs =
                    (i == nextLength - 1 && (length - parity) % 2 == 1) ? rightPath[level] : items[2 * i + 1 - parity];
                items[i] = keccak256(abi.encodePacked(lhs, rhs));
            }
            length = nextLength;
            index /= 2;
        }
        return items[0];
    }
}
