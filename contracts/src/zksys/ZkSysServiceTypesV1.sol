// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

struct ProverSubscriptionV1 {
    address account;
    address operator;
    address beneficiary;
    address sequencer;
    uint64 firstPeriod;
    uint64 lastPeriod;
    uint64 nonce;
    uint8 services;
}

struct AcceptedPackageV1 {
    uint32 domainVersion;
    bytes32 policyHash;
    uint256 chainId;
    address chainAddress;
    bytes32 parent;
    uint64 batchFrom;
    uint64 batchTo;
    uint32 protocolVersion;
    bytes32 vkHash;
    uint64 period;
    bytes32 rosterRoot;
    uint32 turn;
    bytes32 manifestHash;
    bytes32 reportHash;
    bytes32 proofHash;
    address sequencer;
    address sequencerBeneficiary;
    address wrapper;
    address wrapperBeneficiary;
}

struct DutySuccessV1 {
    address account;
    bytes32 subscriptionHash;
    uint64 batchNumber;
    bytes32 statementHash;
    bytes32 friProofHash;
    uint64 transactionCount;
    uint64 period;
    uint16 slot;
    uint32 attempt;
    bytes32 assignmentId;
    bytes operatorSignature;
}

struct WrapperCandidateV1 {
    uint32 index;
    address account;
    address operator;
    address beneficiary;
}

library ZkSysServiceTypesV1 {
    uint32 internal constant DOMAIN_VERSION = 1;
    uint8 internal constant FRI_SERVICE = 1;
    uint8 internal constant WRAPPER_SERVICE = 2;
    uint64 internal constant MAX_BATCHES_PER_PACKAGE = 100;

    bytes32 internal constant SUBSCRIPTION_TYPEHASH = keccak256(
        "ProverSubscriptionV1(address account,address operator,address beneficiary,address sequencer,uint64 firstPeriod,uint64 lastPeriod,uint64 nonce,uint8 services)"
    );
    bytes32 internal constant DUTY_TYPEHASH = keccak256(
        "DutySuccessV1(address account,bytes32 subscriptionHash,uint64 batchNumber,bytes32 statementHash,bytes32 friProofHash,uint64 transactionCount,uint64 period,uint16 slot,uint32 attempt,bytes32 assignmentId)"
    );
    bytes32 internal constant PACKAGE_TYPEHASH = keccak256(
        "AcceptedPackageV1(uint32 domainVersion,bytes32 policyHash,uint256 chainId,address chainAddress,bytes32 parent,uint64 batchFrom,uint64 batchTo,uint32 protocolVersion,bytes32 vkHash,uint64 period,bytes32 rosterRoot,uint32 turn,bytes32 manifestHash,bytes32 reportHash,bytes32 proofHash,address sequencer,address sequencerBeneficiary,address wrapper,address wrapperBeneficiary)"
    );

    function hashSubscription(ProverSubscriptionV1 memory subscription) internal pure returns (bytes32) {
        return keccak256(abi.encode(SUBSCRIPTION_TYPEHASH, subscription));
    }

    function hashDuty(DutySuccessV1 memory duty) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(
                DUTY_TYPEHASH,
                duty.account,
                duty.subscriptionHash,
                duty.batchNumber,
                duty.statementHash,
                duty.friProofHash,
                duty.transactionCount,
                duty.period,
                duty.slot,
                duty.attempt,
                duty.assignmentId
            )
        );
    }

    function hashPackage(AcceptedPackageV1 memory accepted) internal pure returns (bytes32) {
        return keccak256(abi.encode(PACKAGE_TYPEHASH, accepted));
    }

    function hashReport(DutySuccessV1[] memory duties) internal pure returns (bytes32) {
        return keccak256(abi.encode(duties));
    }

    function wrapperLeaf(WrapperCandidateV1 memory candidate) internal pure returns (bytes32) {
        // Double hashing separates leaf encodings from Merkle branch preimages.
        return keccak256(bytes.concat(keccak256(abi.encode(candidate))));
    }
}

interface IZkSysServiceClockV1 {
    function startTime() external view returns (uint256);
    function periodSeconds() external view returns (uint256);
}

interface IZkSysAuthenticatedRootSourceV1 {
    /// @dev The root-side draw must register this exact already-published commitment before its
    /// target block exists. An offset from a delayed relayed head cannot establish that ordering.
    function drawFor(bytes32 commitment) external view returns (uint64 height, bytes32 hash);
}

interface IZkSysNativeRootDrawV1 is IZkSysAuthenticatedRootSourceV1 {
    function coordinator() external view returns (address);
    function requestDraw(bytes32 commitment) external;
}

interface IZkSysServiceMessageSinkV1 {
    function publisher() external view returns (address);
    function publish(bytes calldata message) external;
}

interface IZkSysL1MessengerV1 {
    function sendToL1(bytes calldata message) external returns (bytes32);
}

interface IZkSysWrapperRosterSourceV1 {
    /// @dev The source must authenticate qualification, unique accounts/operators, and an immutable
    /// period snapshot. A caller-supplied list or owner-replaceable Merkle root is not that source.
    function currentRoster() external view returns (uint64 period, bytes32 root, uint32 count);
    function rosterFor(uint64 period) external view returns (bytes32 root, uint32 count);
    function startTime() external view returns (uint256);
    function periodSeconds() external view returns (uint256);
    function firstServicePeriod() external view returns (uint64);
}
