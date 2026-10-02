// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {EIP712} from "@openzeppelin/contracts-v4/utils/cryptography/EIP712.sol";
import {SignatureChecker} from "@openzeppelin/contracts-v4/utils/cryptography/SignatureChecker.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts-v4/security/ReentrancyGuard.sol";
import {ZkSysWrapperCoordinatorV1} from "./ZkSysWrapperCoordinatorV1.sol";
import {IZkSysPriorityGuardV1} from "./ZkSysPriorityGuardV1.sol";
import {
    AcceptedPackageV1,
    DutySuccessV1,
    WrapperCandidateV1,
    ZkSysServiceTypesV1,
    IZkSysServiceMessageSinkV1
} from "./ZkSysServiceTypesV1.sol";

interface IZkSysNativeSettlementV1 {
    function getVerifier() external view returns (address);
    function getChainId() external view returns (uint256);
    function getSemverProtocolVersion() external view returns (uint32, uint32, uint32);
    function getTotalBatchesVerified() external view returns (uint256);
    function storedBatchHash(uint256 batch) external view returns (bytes32);
    function isValidator(address validator) external view returns (bool);
}

interface IZkSysProductionVerifierV1 {
    function IS_TESTNET_VERIFIER() external view returns (bool);
    function verificationKeyHash(uint256 proofType) external view returns (bytes32);
    function plonkVerifiers(uint32 version) external view returns (address);
}

interface IZkSysProverTimelockV1 {
    function getRoleMemberCount(address chain, bytes32 role) external view returns (uint256);
    function getRoleMember(address chain, bytes32 role, uint256 index) external view returns (address);
    function proveBatchesSharedBridge(address chain, uint256 from, uint256 to, bytes calldata proofData) external;
}

interface IZkSysL1MessengerV1 {
    function sendToL1(bytes calldata message) external returns (bytes32);
}

/// @notice Opt-in Gateway proof lane binding native V8 validity to both operator endorsements.
/// @dev Deployment must remove every alternative diamond validator/prover path, including emergency
/// priority mode. The diamond validator mapping is not enumerable, so sole timelock PROVER_ROLE
/// membership alone cannot establish that global deployment invariant. Existing commit/execute
/// calls and timelock delays remain in their original contracts.
contract ZkSysProofGateV1 is EIP712, ReentrancyGuard {
    using SignatureChecker for address;
    bytes32 public constant ACCEPTED_DOMAIN = keccak256("ZKSYS_ACCEPTED_SERVICE_V1");
    bytes32 public constant ACTIVATION_DOMAIN = keccak256("ZKSYS_SERVICE_ACTIVATION_V1");
    bytes32 private constant PROVER_ROLE = keccak256("PROVER_ROLE");
    uint256 private constant V8_PROOF_TYPE = 0x802;
    IZkSysL1MessengerV1 private constant MESSENGER = IZkSysL1MessengerV1(address(0x8008));

    struct StoredBatch {
        uint64 batchNumber;
        bytes32 batchHash;
        uint64 indexRepeatedStorageChanges;
        uint256 numberOfLayer1Txs;
        bytes32 priorityOperationsHash;
        bytes32 dependencyRootsRollingHash;
        bytes32 l2LogsTreeRoot;
        uint256 timestamp;
        bytes32 commitment;
    }

    struct BatchOutput {
        uint64 firstBlockTimestamp;
        uint64 lastBlockTimestamp;
        uint256 daScheme;
        bytes32 daCommitment;
        uint256 l1TxCount;
        uint256 l2TxCount;
        bytes32 priorityOperationsHash;
        bytes32 l2LogsRoot;
        bytes32 upgradeTxHash;
        bytes32 dependencyRootsRollingHash;
        uint256 settlementChainId;
        bytes32 edgeDARefsRoot;
    }

    error InvalidConfiguration();
    error Unauthorized();
    error WrongPhase();
    error InvalidPackage();
    error InvalidProofData();
    error InvalidDutyStatement();
    error SettlementConfigurationChanged();

    IZkSysNativeSettlementV1 public immutable chain;
    IZkSysProverTimelockV1 public immutable timelock;
    address public immutable sequencer;
    address public immutable deploymentAuthority;
    uint256 public immutable childChainId;
    bytes32 public immutable policyHash;
    bytes32 public immutable productionVkHash;
    address public immutable productionVerifier;
    bytes32 public immutable verifierCodeHash;
    address public immutable plonkVerifier;
    bytes32 public immutable plonkVerifierCodeHash;
    bytes32 public immutable chainConfigHash;
    IZkSysPriorityGuardV1 public immutable priorityGuard;
    IZkSysServiceMessageSinkV1 public immutable messageSink;
    ZkSysWrapperCoordinatorV1 public coordinator;
    bytes32 public lastAcceptedPackage;
    bytes32 public priorityWorkId;
    AcceptedPackageV1 private _bootstrapPackage;
    bool public serviceActive;
    bool public transitionWork;

    event ServicePackageAccepted(bytes32 indexed packageHash, uint64 indexed from, uint64 indexed to, bool bootstrap);
    event ServiceActivated(address indexed coordinator, bytes32 qualifiedRosterRoot);
    event ServiceControlPackageAccepted(bytes32 indexed packageHash);

    constructor(
        IZkSysNativeSettlementV1 chain_,
        IZkSysProverTimelockV1 timelock_,
        address sequencer_,
        bytes32 policyHash_,
        bytes32 productionVkHash_,
        IZkSysPriorityGuardV1 priorityGuard_,
        IZkSysServiceMessageSinkV1 messageSink_
    ) EIP712("ZkSysProofGate", "1") {
        if (
            address(chain_).code.length == 0 || address(timelock_).code.length == 0 || sequencer_ == address(0)
                || policyHash_ == bytes32(0) || productionVkHash_ == bytes32(0)
                || address(priorityGuard_).code.length == 0 || priorityGuard_.acceptanceGate() != address(this)
                || address(priorityGuard_.chain()) != address(chain_) || priorityGuard_.policyHash() != policyHash_
                || (address(messageSink_) != address(0)
                    && (address(messageSink_).code.length == 0 || messageSink_.publisher() != address(this)))
        ) revert InvalidConfiguration();
        chain = chain_;
        timelock = timelock_;
        sequencer = sequencer_;
        deploymentAuthority = msg.sender;
        childChainId = chain_.getChainId();
        policyHash = policyHash_;
        productionVkHash = productionVkHash_;
        priorityGuard = priorityGuard_;
        messageSink = messageSink_;
        productionVerifier = chain_.getVerifier();
        verifierCodeHash = productionVerifier.codehash;
        plonkVerifier = IZkSysProductionVerifierV1(productionVerifier).plonkVerifiers(8);
        plonkVerifierCodeHash = plonkVerifier.codehash;
        chainConfigHash = keccak256(abi.encode(childChainId, uint256(0), uint256(1 << 24)));
        uint256 verified = chain_.getTotalBatchesVerified();
        lastAcceptedPackage = keccak256(
            abi.encode(block.chainid, address(this), childChainId, verified, chain_.storedBatchHash(verified))
        );
        _checkVerifier();
    }

    function bootstrapDigest(AcceptedPackageV1 calldata accepted) external view returns (bytes32) {
        return _hashTypedDataV4(ZkSysServiceTypesV1.hashPackage(accepted));
    }

    /// @dev One-time installation follows bootstrap qualification, because the coordinator pins
    /// the final bootstrap parent and first batch. It cannot replace a live coordinator.
    function installCoordinator(ZkSysWrapperCoordinatorV1 coordinator_) external {
        if (msg.sender != deploymentAuthority) revert Unauthorized();
        if (address(coordinator) != address(0) || serviceActive || priorityWorkId != bytes32(0)) revert WrongPhase();
        if (
            address(coordinator_).code.length == 0 || coordinator_.acceptanceGate() != address(this)
                || coordinator_.childChainId() != childChainId || coordinator_.childChainAddress() != address(chain)
                || coordinator_.sequencer() != sequencer || coordinator_.policyHash() != policyHash
                || coordinator_.productionVkHash() != productionVkHash
                || coordinator_.nativeRootDraw() != (address(messageSink) != address(0))
                || coordinator_.acceptedParent() != lastAcceptedPackage
                || coordinator_.nextBatch() != chain.getTotalBatchesVerified() + 1
        ) revert InvalidConfiguration();
        coordinator = coordinator_;
    }

    function activateService() external nonReentrant {
        if (serviceActive || address(coordinator) == address(0)) revert WrongPhase();
        _checkSettlement();
        uint64 firstPeriod = coordinator.firstServicePeriod();
        uint256 firstPeriodStart =
            coordinator.rosterSource().startTime() + uint256(firstPeriod) * coordinator.rosterSource().periodSeconds();
        if (block.timestamp >= firstPeriodStart) revert WrongPhase();
        (bytes32 root, uint32 count) = coordinator.rosterSource().rosterFor(firstPeriod);
        if (root == bytes32(0) || count == 0 || coordinator.rosterSeed(firstPeriod) == bytes32(0)) {
            revert InvalidConfiguration();
        }
        serviceActive = true;
        _publishMessage(abi.encode(ACTIVATION_DOMAIN, productionVkHash, root));
        emit ServiceActivated(address(coordinator), root);
    }

    function openPackage(AcceptedPackageV1 calldata proposed) external nonReentrant {
        if (msg.sender != sequencer) revert Unauthorized();
        if (!serviceActive) revert WrongPhase();
        _checkSettlement();
        if (proposed.batchFrom != chain.getTotalBatchesVerified() + 1) revert InvalidPackage();
        (,,, transitionWork) = coordinator.openingRoster();
        if (transitionWork && proposed.reportHash != _emptyReportHash()) revert InvalidPackage();
        _openPriorityWork(proposed);
        coordinator.openPackage(proposed);
    }

    function openBootstrapPackage(AcceptedPackageV1 calldata proposed) external nonReentrant {
        if (msg.sender != sequencer) revert Unauthorized();
        if (serviceActive || address(coordinator) != address(0)) revert WrongPhase();
        _checkSettlement();
        if (
            proposed.domainVersion != 1 || proposed.policyHash != policyHash || proposed.chainId != childChainId
                || proposed.chainAddress != address(chain) || proposed.parent != lastAcceptedPackage
                || proposed.protocolVersion != 32 || proposed.vkHash != productionVkHash
                || proposed.sequencer != sequencer || proposed.sequencerBeneficiary == address(0)
                || proposed.batchFrom != chain.getTotalBatchesVerified() + 1 || proposed.batchTo <= proposed.batchFrom
                || proposed.batchTo - proposed.batchFrom >= 100 || proposed.manifestHash == bytes32(0)
                || proposed.proofHash != bytes32(0) || proposed.wrapper != address(0)
                || proposed.wrapperBeneficiary != address(0) || proposed.rosterRoot != bytes32(0) || proposed.turn != 0
        ) revert InvalidPackage();
        _openPriorityWork(proposed);
        _bootstrapPackage = proposed;
    }

    /// @dev Repairing the batch range or work artifacts does not renew the frozen inclusion deadline.
    function repairBootstrapPackage(AcceptedPackageV1 calldata proposed) external nonReentrant {
        if (msg.sender != sequencer) revert Unauthorized();
        if (serviceActive || address(coordinator) != address(0) || priorityWorkId == bytes32(0)) revert WrongPhase();
        AcceptedPackageV1 memory frozen = _bootstrapPackage;
        frozen.batchTo = proposed.batchTo;
        frozen.manifestHash = proposed.manifestHash;
        frozen.reportHash = proposed.reportHash;
        if (
            proposed.batchTo <= proposed.batchFrom || proposed.batchTo - proposed.batchFrom >= 100
                || proposed.manifestHash == bytes32(0)
                || ZkSysServiceTypesV1.hashPackage(frozen) != ZkSysServiceTypesV1.hashPackage(proposed)
        ) revert InvalidPackage();
        _bootstrapPackage = proposed;
    }

    function bootstrapPackage() external view returns (AcceptedPackageV1 memory) {
        return _bootstrapPackage;
    }

    function refreshPriorityCheckpoint() external nonReentrant {
        if (msg.sender != sequencer) revert Unauthorized();
        _checkSettlement();
        priorityGuard.refreshExpired(priorityWorkId);
    }

    function _openPriorityWork(AcceptedPackageV1 calldata proposed) private {
        if (priorityWorkId != bytes32(0)) revert WrongPhase();
        priorityWorkId = keccak256(abi.encode(proposed.parent, proposed.batchFrom));
        priorityGuard.open(priorityWorkId, proposed.batchFrom);
    }

    function repairPackage(AcceptedPackageV1 calldata proposed) external nonReentrant {
        if (msg.sender != sequencer) revert Unauthorized();
        if (!serviceActive) revert WrongPhase();
        _checkSettlement();
        if (proposed.batchFrom != chain.getTotalBatchesVerified() + 1) revert InvalidPackage();
        if (transitionWork && proposed.reportHash != _emptyReportHash()) revert InvalidPackage();
        coordinator.repairPackage(proposed);
    }

    function submitBootstrap(
        AcceptedPackageV1 calldata accepted,
        DutySuccessV1[] calldata duties,
        BatchOutput[] calldata outputs,
        bytes calldata proofData,
        bytes calldata sequencerSignature
    ) external nonReentrant {
        if (serviceActive || address(coordinator) != address(0)) revert WrongPhase();
        AcceptedPackageV1 memory normalized = accepted;
        normalized.proofHash = bytes32(0);
        if (
            priorityWorkId == bytes32(0)
                || ZkSysServiceTypesV1.hashPackage(normalized) != ZkSysServiceTypesV1.hashPackage(_bootstrapPackage)
                || accepted.wrapper != address(0) || accepted.wrapperBeneficiary != address(0)
                || accepted.rosterRoot != bytes32(0) || accepted.turn != 0
                || !sequencer.isValidSignatureNow(
                    _hashTypedDataV4(ZkSysServiceTypesV1.hashPackage(accepted)), sequencerSignature
                )
        ) revert InvalidPackage();
        _verifyAndProve(accepted, duties, outputs, proofData);
        _publishAccepted(accepted, true);
    }

    function submit(
        AcceptedPackageV1 calldata accepted,
        DutySuccessV1[] calldata duties,
        BatchOutput[] calldata outputs,
        bytes calldata proofData,
        WrapperCandidateV1 calldata candidate,
        bytes32[] calldata candidateProof,
        bytes calldata sequencerSignature,
        bytes calldata wrapperSignature
    ) external nonReentrant {
        if (!serviceActive) revert WrongPhase();
        if (transitionWork && duties.length != 0) revert InvalidDutyStatement();
        coordinator.acceptPackage(accepted, candidate, candidateProof, sequencerSignature, wrapperSignature);
        _verifyAndProve(accepted, duties, outputs, proofData);
        _publishAccepted(accepted, false);
    }

    function _verifyAndProve(
        AcceptedPackageV1 calldata accepted,
        DutySuccessV1[] calldata duties,
        BatchOutput[] calldata outputs,
        bytes calldata proofData
    ) private {
        _checkSettlement();
        if (
            accepted.domainVersion != 1 || accepted.policyHash != policyHash || accepted.chainId != childChainId
                || accepted.chainAddress != address(chain) || accepted.parent != lastAcceptedPackage
                || accepted.protocolVersion != 32 || accepted.vkHash != productionVkHash
                || accepted.sequencer != sequencer || accepted.sequencerBeneficiary == address(0)
                || accepted.batchFrom == 0 || accepted.batchTo < accepted.batchFrom
                || accepted.batchTo - accepted.batchFrom >= 100 || accepted.manifestHash == bytes32(0)
                || accepted.proofHash != keccak256(proofData)
                || accepted.reportHash != ZkSysServiceTypesV1.hashReport(duties)
                || accepted.batchFrom != chain.getTotalBatchesVerified() + 1
        ) revert InvalidPackage();
        if (proofData.length == 0 || uint8(proofData[0]) != 1) revert InvalidProofData();
        (StoredBatch memory previous, StoredBatch[] memory batches, uint256[] memory proof) =
            abi.decode(proofData[1:], (StoredBatch, StoredBatch[], uint256[]));
        if (
            batches.length < 2 || batches.length != accepted.batchTo - accepted.batchFrom + 1
                || outputs.length != batches.length || proof.length != 46 || proof[0] != V8_PROOF_TYPE || proof[1] != 0
                || previous.batchNumber != accepted.batchFrom - 1
                || keccak256(abi.encode(previous, batches, proof)) != keccak256(proofData[1:])
                || duties.length > batches.length
        ) revert InvalidProofData();

        bytes32[] memory statements = new bytes32[](batches.length);
        uint256[] memory priorityCounts = new uint256[](batches.length);
        bytes32[] memory priorityHashes = new bytes32[](batches.length);
        for (uint256 i; i < batches.length; ++i) {
            StoredBatch memory batch = batches[i];
            BatchOutput calldata output = outputs[i];
            if (
                batch.batchNumber != accepted.batchFrom + i || _outputHash(output) != batch.commitment
                    || output.settlementChainId != block.chainid || batch.numberOfLayer1Txs != output.l1TxCount
                    || batch.priorityOperationsHash != output.priorityOperationsHash
                    || batch.dependencyRootsRollingHash != output.dependencyRootsRollingHash
                    || batch.l2LogsTreeRoot != output.l2LogsRoot
            ) revert InvalidProofData();
            statements[i] =
                keccak256(abi.encodePacked(previous.batchHash, batch.batchHash, chainConfigHash, batch.commitment));
            priorityCounts[i] = output.l1TxCount;
            priorityHashes[i] = output.priorityOperationsHash;
            previous = batch;
        }
        uint256 seen;
        for (uint256 i; i < duties.length; ++i) {
            DutySuccessV1 calldata duty = duties[i];
            if (duty.batchNumber < accepted.batchFrom || duty.batchNumber > accepted.batchTo) {
                revert InvalidDutyStatement();
            }
            uint256 index = duty.batchNumber - accepted.batchFrom;
            uint256 bit = uint256(1) << index;
            uint256 count = outputs[index].l1TxCount + outputs[index].l2TxCount;
            if (
                seen & bit != 0 || count == 0 || count != duty.transactionCount
                    || duty.statementHash != statements[index] || duty.period != accepted.period
                    || duty.friProofHash == bytes32(0)
            ) revert InvalidDutyStatement();
            seen |= bit;
        }

        priorityGuard.consume(priorityWorkId, accepted.batchTo, priorityCounts, priorityHashes);
        // The native Executor checks committed batch hashes and invokes the production verifier.
        // Reversion also rolls back coordinator acceptance; a signature can never substitute for proof validity.
        timelock.proveBatchesSharedBridge(address(chain), accepted.batchFrom, accepted.batchTo, proofData);
        if (chain.getTotalBatchesVerified() != accepted.batchTo) revert InvalidProofData();
    }

    function _publishAccepted(AcceptedPackageV1 calldata accepted, bool bootstrap) private {
        bytes32 packageHash = ZkSysServiceTypesV1.hashPackage(accepted);
        bool control = transitionWork;
        lastAcceptedPackage = packageHash;
        priorityWorkId = bytes32(0);
        transitionWork = false;
        if (bootstrap) delete _bootstrapPackage;
        if (control) emit ServiceControlPackageAccepted(packageHash);
        else _publishMessage(abi.encode(ACCEPTED_DOMAIN, packageHash, bootstrap));
        emit ServicePackageAccepted(packageHash, accepted.batchFrom, accepted.batchTo, bootstrap);
    }

    function _publishMessage(bytes memory message) private {
        if (address(messageSink) == address(0)) MESSENGER.sendToL1(message);
        else messageSink.publish(message);
    }

    function _emptyReportHash() private pure returns (bytes32) {
        return ZkSysServiceTypesV1.hashReport(new DutySuccessV1[](0));
    }

    function _checkVerifier() private view {
        (uint32 major, uint32 minor, uint32 patch) = chain.getSemverProtocolVersion();
        if (
            major != 0 || minor != 32 || patch != 0 || childChainId == 0 || chain.getChainId() != childChainId
                || chain.getVerifier() != productionVerifier || productionVerifier.code.length == 0
                || productionVerifier.codehash != verifierCodeHash
                || IZkSysProductionVerifierV1(productionVerifier).plonkVerifiers(8) != plonkVerifier
                || plonkVerifier.code.length == 0 || plonkVerifier.codehash != plonkVerifierCodeHash
                || IZkSysProductionVerifierV1(productionVerifier).IS_TESTNET_VERIFIER()
                || IZkSysProductionVerifierV1(productionVerifier).verificationKeyHash(V8_PROOF_TYPE) != productionVkHash
        ) revert SettlementConfigurationChanged();
    }

    function _checkSettlement() private view {
        _checkVerifier();
        if (
            !chain.isValidator(address(timelock)) || timelock.getRoleMemberCount(address(chain), PROVER_ROLE) != 1
                || timelock.getRoleMember(address(chain), PROVER_ROLE, 0) != address(this)
        ) revert SettlementConfigurationChanged();
    }

    function _outputHash(BatchOutput calldata output) private pure returns (bytes32) {
        return keccak256(
            abi.encodePacked(
                output.firstBlockTimestamp,
                output.lastBlockTimestamp,
                output.daScheme,
                output.daCommitment,
                output.l1TxCount,
                output.l2TxCount,
                output.priorityOperationsHash,
                output.l2LogsRoot,
                output.upgradeTxHash,
                output.dependencyRootsRollingHash,
                output.settlementChainId,
                output.edgeDARefsRoot
            )
        );
    }
}
