// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

struct PriorityCommitReservationV1 {
    uint64 id;
    uint64 batchNumber;
    uint64 priorityFrom;
    uint64 requiredEnd;
    uint64 rootTotal;
    uint64 rootBlockNumber;
    uint64 rootTreeStart;
    uint64 rootTreeHeight;
    bytes32 root;
    uint64 localTotal;
    uint64 localTreeStart;
    uint64 localTreeHeight;
    bytes32 localRoot;
}

interface IZkSysPriorityCommitAdmissionV1 {
    function chain() external view returns (address);
    function policyHash() external view returns (bytes32);
    function maxPriorityPerPackage() external view returns (uint64);
    function sequencer() external view returns (address);
    function reservation() external view returns (PriorityCommitReservationV1 memory);
    function reserve() external returns (uint64 id);
    function cancelReservation(uint64 id) external;
    function publishWitness(
        uint64 id,
        bytes32 storedHash,
        bytes32[] calldata itemHashes,
        bytes32[] calldata leftPath,
        bytes32[] calldata rightPath,
        bytes32[] calldata localLeftPath,
        bytes32[] calldata localRightPath
    ) external;
    function commitReady(uint64 batchNumber, bytes32 storedHash, uint256 priorityCount, bytes32 priorityHash)
        external
        view
        returns (bool);
    function recordCommit(uint64 batchNumber, bytes32 storedHash, uint256 priorityCount, bytes32 priorityHash) external;
    function recordRevert(uint256 newLastBatch) external;
    function admittedRange(uint64 from, uint64 to)
        external
        view
        returns (bytes32 digest, uint64 priorityFrom, uint64 priorityEnd);
}
