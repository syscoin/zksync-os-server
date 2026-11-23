// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {IL1BridgehubMinimal} from "./ZkSysRegistryBridge.sol";
import {IZkSysPriorityCommitAdmissionV1} from "./IZkSysPriorityCommitAdmissionV1.sol";

interface IZkSysPriorityMailboxV1 {
    function getChainId() external view returns (uint256);
    function getTotalPriorityTxs() external view returns (uint256);
    function getPriorityTreeStartIndex() external view returns (uint256);
    function getPriorityTreeRoot() external view returns (bytes32);
    function getPriorityTreeHeight() external view returns (uint256);
    function getPriorityTransactionTimestamp(uint256 index) external view returns (uint256);
    function getFirstUnprocessedPriorityTx() external view returns (uint256);
    function isPriorityQueueActive() external view returns (bool);
    function getTotalBatchesCommitted() external view returns (uint256);
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

interface IZkSysPriorityAdmissionChainV1 {
    function getSyscoinPriorityCommitPolicy() external view returns (address);
}

interface IZkSysPriorityGuardV1 {
    function acceptanceGate() external view returns (address);
    function chain() external view returns (IZkSysPriorityMailboxV1);
    function admission() external view returns (IZkSysPriorityCommitAdmissionV1);
    function policyHash() external view returns (bytes32);
    function isAdmissionActive(address sequencer) external view returns (bool);
    function open(bytes32 workId, uint64 batchFrom, uint64 batchTo) external;
    function repair(bytes32 workId, uint64 batchTo) external;
    function refreshExpired(bytes32 workId) external;
    function consume(bytes32 workId, uint64 batchTo, bytes32 rangeDigest, uint256 priorityCount) external;
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

/// @notice Binds proof work to the exact priority range authenticated during native commitment.
/// @dev The proof cursor advances independently of execution. Later requests affect subsequent
/// admissions, while repairs and renewals can only reference native commitments that already exist.
contract ZkSysPriorityGuardV1 is IZkSysPriorityGuardV1 {
    error Unauthorized();
    error InvalidConfiguration();
    error InvalidWork();
    error ExpiredWork();
    error InvalidPrefix();

    struct Work {
        bytes32 id;
        bytes32 rangeDigest;
        uint64 batchFrom;
        uint64 batchTo;
        uint64 openedAt;
        uint64 priorityFrom;
        uint64 priorityEnd;
    }

    address public immutable acceptanceGate;
    IZkSysPriorityMailboxV1 public immutable chain;
    IZkSysPriorityCommitAdmissionV1 public immutable admission;
    bytes32 public immutable admissionCodeHash;
    bytes32 public immutable policyHash;
    uint64 public immutable maxProofWorkSeconds;
    uint64 public immutable maxPriorityPerPackage;
    uint64 public priorityCursor;
    uint64 public lastVerifiedBatch;
    Work public work;

    event WorkBound(bytes32 indexed workId, bytes32 indexed rangeDigest, uint64 batchFrom, uint64 batchTo);
    event WorkRenewed(bytes32 indexed workId, uint64 openedAt);
    event PrefixConsumed(bytes32 indexed workId, uint64 batchTo, uint64 cursor);

    constructor(
        address gate_,
        IZkSysPriorityMailboxV1 chain_,
        IZkSysPriorityCommitAdmissionV1 admission_,
        bytes32 policyHash_,
        uint64 maxProofWorkSeconds_,
        uint64 maxPriorityPerPackage_
    ) {
        if (
            gate_ == address(0) || address(chain_).code.length == 0 || address(admission_).code.length == 0
                || policyHash_ == bytes32(0) || admission_.policyHash() != policyHash_
                || admission_.chain() != address(chain_) || admission_.sequencer() == address(0)
                || maxProofWorkSeconds_ == 0 || maxPriorityPerPackage_ < 2 || maxPriorityPerPackage_ > 1024
                || admission_.maxPriorityPerPackage() != maxPriorityPerPackage_
        ) revert InvalidConfiguration();
        uint256 verified = chain_.getTotalBatchesVerified();
        uint256 cursor = chain_.getFirstUnprocessedPriorityTx();
        if (
            verified != chain_.getTotalBatchesCommitted() || verified != chain_.getTotalBatchesExecuted()
                || verified > type(uint64).max || cursor > type(uint64).max || cursor != chain_.getTotalPriorityTxs()
                || chain_.isPriorityQueueActive()
        ) revert InvalidConfiguration();
        acceptanceGate = gate_;
        chain = chain_;
        admission = admission_;
        admissionCodeHash = address(admission_).codehash;
        policyHash = policyHash_;
        maxProofWorkSeconds = maxProofWorkSeconds_;
        maxPriorityPerPackage = maxPriorityPerPackage_;
        priorityCursor = uint64(cursor);
        lastVerifiedBatch = uint64(verified);
    }

    modifier onlyGate() {
        if (msg.sender != acceptanceGate) revert Unauthorized();
        if (!_admissionInstalled()) revert InvalidConfiguration();
        _checkNativeCursor();
        _;
    }

    function isAdmissionActive(address sequencer) external view returns (bool) {
        return _admissionInstalled() && admission.sequencer() == sequencer;
    }

    function _admissionInstalled() private view returns (bool) {
        return address(admission).codehash == admissionCodeHash
            && IZkSysPriorityAdmissionChainV1(address(chain)).getSyscoinPriorityCommitPolicy() == address(admission)
            && admission.chain() == address(chain) && admission.policyHash() == policyHash
            && admission.maxPriorityPerPackage() == maxPriorityPerPackage;
    }

    function open(bytes32 workId, uint64 batchFrom, uint64 batchTo) external onlyGate {
        if (work.id != bytes32(0) || workId == bytes32(0) || uint256(batchFrom) != uint256(lastVerifiedBatch) + 1) {
            revert InvalidWork();
        }
        (bytes32 digest, uint64 from, uint64 end) = _admittedRange(batchFrom, batchTo);
        work = Work({
            id: workId,
            rangeDigest: digest,
            batchFrom: batchFrom,
            batchTo: batchTo,
            openedAt: _clock(),
            priorityFrom: from,
            priorityEnd: end
        });
        emit WorkBound(workId, digest, batchFrom, batchTo);
    }

    function repair(bytes32 workId, uint64 batchTo) external onlyGate {
        Work memory pending = _work(workId);
        (bytes32 digest, uint64 from, uint64 end) = _admittedRange(pending.batchFrom, batchTo);
        if (from != pending.priorityFrom) revert InvalidPrefix();
        work.rangeDigest = digest;
        work.batchTo = batchTo;
        work.priorityEnd = end;
        emit WorkBound(workId, digest, pending.batchFrom, batchTo);
    }

    function refreshExpired(bytes32 workId) external onlyGate {
        Work memory pending = _work(workId);
        if (block.timestamp <= uint256(pending.openedAt) + maxProofWorkSeconds) revert InvalidWork();
        _checkAdmittedRange(pending);
        work.openedAt = _clock();
        emit WorkRenewed(workId, work.openedAt);
    }

    function consume(bytes32 workId, uint64 batchTo, bytes32 rangeDigest, uint256 priorityCount) external onlyGate {
        Work memory pending = _work(workId);
        if (block.timestamp > uint256(pending.openedAt) + maxProofWorkSeconds) revert ExpiredWork();
        if (
            batchTo != pending.batchTo || rangeDigest != pending.rangeDigest
                || priorityCount != uint256(pending.priorityEnd) - pending.priorityFrom
        ) revert InvalidPrefix();
        _checkAdmittedRange(pending);
        priorityCursor = pending.priorityEnd;
        lastVerifiedBatch = batchTo;
        delete work;
        emit PrefixConsumed(workId, batchTo, pending.priorityEnd);
    }

    function _checkAdmittedRange(Work memory pending) private view {
        (bytes32 digest, uint64 from, uint64 end) = _admittedRange(pending.batchFrom, pending.batchTo);
        if (digest != pending.rangeDigest || from != pending.priorityFrom || end != pending.priorityEnd) {
            revert InvalidPrefix();
        }
    }

    function _admittedRange(uint64 batchFrom, uint64 batchTo) private view returns (bytes32, uint64, uint64) {
        if (batchTo <= batchFrom || batchTo - batchFrom >= 100) revert InvalidPrefix();
        (bytes32 digest, uint64 from, uint64 end) = admission.admittedRange(batchFrom, batchTo);
        if (digest == bytes32(0) || from != priorityCursor || end < from || end - from > maxPriorityPerPackage) {
            revert InvalidPrefix();
        }
        return (digest, from, end);
    }

    function _checkNativeCursor() private view {
        if (
            chain.isPriorityQueueActive() || chain.getTotalBatchesVerified() != lastVerifiedBatch
                || chain.getTotalBatchesExecuted() > lastVerifiedBatch
                || chain.getFirstUnprocessedPriorityTx() > priorityCursor
        ) revert InvalidWork();
    }

    function _work(bytes32 id) private view returns (Work memory pending) {
        pending = work;
        if (id == bytes32(0) || id != pending.id) revert InvalidWork();
    }

    function _clock() private view returns (uint64) {
        if (block.timestamp > type(uint64).max) revert InvalidWork();
        return uint64(block.timestamp);
    }
}
