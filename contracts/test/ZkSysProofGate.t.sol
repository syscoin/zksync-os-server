// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";
import {
    ZkSysProofGateV1,
    IZkSysNativeSettlementV1,
    IZkSysProverTimelockV1,
    IZkSysProductionVerifierV1
} from "contracts/src/zksys/ZkSysProofGateV1.sol";
import {
    AcceptedPackageV1,
    DutySuccessV1,
    ZkSysServiceTypesV1,
    IZkSysServiceMessageSinkV1
} from "contracts/src/zksys/ZkSysServiceTypesV1.sol";
import {
    WrapperCandidateV1,
    IZkSysAuthenticatedRootSourceV1,
    IZkSysWrapperRosterSourceV1
} from "contracts/src/zksys/ZkSysServiceTypesV1.sol";
import {
    ZkSysPriorityGuardV1,
    IZkSysPriorityMailboxV1,
    IZkSysPriorityCheckpointReceiverV1,
    PriorityCheckpointV1
} from "contracts/src/zksys/ZkSysPriorityGuardV1.sol";
import {ZkSysWrapperCoordinatorV1} from "contracts/src/zksys/ZkSysWrapperCoordinatorV1.sol";

contract GateDrawSource is IZkSysAuthenticatedRootSourceV1 {
    function drawFor(bytes32) external pure returns (uint64, bytes32) {
        return (123, keccak256("root draw"));
    }
}

contract GateRosterSource is IZkSysWrapperRosterSourceV1 {
    bytes32 public root;

    constructor(bytes32 root_) {
        root = root_;
    }

    function startTime() external pure returns (uint256) {
        return 1_000;
    }

    function periodSeconds() external pure returns (uint256) {
        return 100;
    }

    function firstServicePeriod() external pure returns (uint64) {
        return 0;
    }

    function currentRoster() external view returns (uint64, bytes32, uint32) {
        return (0, root, 1);
    }

    function rosterFor(uint64) external view returns (bytes32, uint32) {
        return (root, 1);
    }
}

contract GateVerifier is IZkSysProductionVerifierV1 {
    bytes32 public vkHash = keccak256("production vk fixture");
    bool public fake;

    function setFake(bool value) external {
        fake = value;
    }

    function setVkHash(bytes32 value) external {
        vkHash = value;
    }

    function IS_TESTNET_VERIFIER() external view returns (bool) {
        return fake;
    }

    function verificationKeyHash(uint256) external view returns (bytes32) {
        return vkHash;
    }

    function plonkVerifiers(uint32) external view returns (address) {
        return address(this);
    }
}

contract GateSettlement is IZkSysNativeSettlementV1, IZkSysProverTimelockV1 {
    GateVerifier public verifier;
    address public gate;
    uint256 public verified;
    uint256 public nativeChainId = 57;
    uint256 public roleCount = 1;
    bytes32 public validProofDataHash;
    bool public refuseProof;

    constructor(GateVerifier verifier_) {
        verifier = verifier_;
    }

    function configure(address gate_, bytes32 validHash) external {
        gate = gate_;
        validProofDataHash = validHash;
    }

    function setRoleCount(uint256 count) external {
        roleCount = count;
    }

    function setRefuseProof(bool value) external {
        refuseProof = value;
    }

    function getVerifier() external view returns (address) {
        return address(verifier);
    }

    function getChainId() external view returns (uint256) {
        return nativeChainId;
    }

    function setChainId(uint256 value) external {
        nativeChainId = value;
    }

    function getSemverProtocolVersion() external pure returns (uint32, uint32, uint32) {
        return (0, 32, 0);
    }

    function getTotalBatchesVerified() external view returns (uint256) {
        return verified;
    }

    function getTotalBatchesExecuted() external pure returns (uint256) {
        return 0;
    }

    function getFirstUnprocessedPriorityTx() external pure returns (uint256) {
        return 0;
    }

    function getTotalPriorityTxs() external pure returns (uint256) {
        return 0;
    }

    function getPriorityTreeStartIndex() external pure returns (uint256) {
        return 0;
    }

    function isPriorityQueueActive() external pure returns (bool) {
        return false;
    }

    function storedBatchHash(uint256 batch) external pure returns (bytes32) {
        return keccak256(abi.encode(batch));
    }

    function isValidator(address validator) external view returns (bool) {
        return validator == address(this);
    }

    function getRoleMemberCount(address, bytes32) external view returns (uint256) {
        return roleCount;
    }

    function getRoleMember(address, bytes32, uint256) external view returns (address) {
        return gate;
    }

    function proveBatchesSharedBridge(address, uint256 from, uint256 to, bytes calldata proofData) external {
        require(
            msg.sender == gate && from == verified + 1 && keccak256(proofData) == validProofDataHash && !refuseProof,
            "native proof invalid"
        );
        verified = to;
    }
}

contract GatePriorityCheckpoint is IZkSysPriorityCheckpointReceiverV1 {
    bytes32 public immutable policyHash;
    uint256 public childChainId = 57;
    uint64 public total;
    uint64 public overdueEnd;
    uint64 public height;
    bytes32 public root = keccak256("");

    constructor(bytes32 policy) {
        policyHash = policy;
    }

    function setChainId(uint256 value) external {
        childChainId = value;
    }

    function receiveCheckpoint(uint256, bytes32, PriorityCheckpointV1 calldata) external pure {
        revert("mock only");
    }

    function setPriority(bytes32 root_, uint64 height_, uint64 total_, uint64 overdueEnd_) external {
        root = root_;
        height = height_;
        total = total_;
        overdueEnd = overdueEnd_;
    }

    function latestCheckpoint() external view returns (PriorityCheckpointV1 memory checkpoint) {
        checkpoint.rootBlockNumber = 1;
        checkpoint.rootTimestamp = uint64(block.timestamp);
        checkpoint.root = root;
        checkpoint.total = total;
        checkpoint.overdueEnd = overdueEnd;
        checkpoint.treeHeight = height;
    }
}

contract GateMessenger {
    bytes public lastMessage;

    function sendToL1(bytes calldata message) external returns (bytes32) {
        lastMessage = message;
        return keccak256(message);
    }
}

contract ZkSysProofGateTest is Test {
    GateVerifier internal verifier;
    GateSettlement internal settlement;
    ZkSysProofGateV1 internal gate;
    GatePriorityCheckpoint internal checkpoint;
    ZkSysPriorityGuardV1 internal guard;
    uint256 internal constant SEQUENCER_KEY = 777;
    uint256 internal constant WRAPPER_KEY = 778;
    bytes32 internal constant POLICY = keccak256("policy");
    AcceptedPackageV1 internal accepted;
    DutySuccessV1[] internal duties;
    ZkSysProofGateV1.BatchOutput[] internal outputs;
    bytes internal proofData;

    function setUp() public {
        vm.chainId(5050);
        verifier = new GateVerifier();
        settlement = new GateSettlement(verifier);
        checkpoint = new GatePriorityCheckpoint(POLICY);
        address predictedGate = vm.computeCreateAddress(address(this), vm.getNonce(address(this)) + 1);
        guard = new ZkSysPriorityGuardV1(
            predictedGate, IZkSysPriorityMailboxV1(address(settlement)), checkpoint, POLICY, 100, 600, 64
        );
        gate = new ZkSysProofGateV1(
            settlement,
            settlement,
            vm.addr(SEQUENCER_KEY),
            POLICY,
            verifier.vkHash(),
            guard,
            IZkSysServiceMessageSinkV1(address(0))
        );
        GateMessenger implementation = new GateMessenger();
        vm.etch(address(0x8008), address(implementation).code);
        _buildProof(false);
    }

    function _hashOutput(ZkSysProofGateV1.BatchOutput memory output) internal pure returns (bytes32) {
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

    function _buildProof(bool allEmpty) internal {
        delete outputs;
        delete duties;
        ZkSysProofGateV1.StoredBatch memory previous;
        previous.batchHash = keccak256("genesis");
        ZkSysProofGateV1.StoredBatch[] memory batches = new ZkSysProofGateV1.StoredBatch[](2);
        for (uint256 i; i < 2; ++i) {
            ZkSysProofGateV1.BatchOutput memory output;
            output.firstBlockTimestamp = uint64(10 + i);
            output.lastBlockTimestamp = uint64(10 + i);
            output.l2TxCount = !allEmpty && i == 0 ? 1 : 0;
            output.settlementChainId = block.chainid;
            output.priorityOperationsHash = keccak256("");
            outputs.push(output);
            batches[i].batchNumber = uint64(i + 1);
            batches[i].batchHash = keccak256(abi.encode("state", i));
            batches[i].commitment = _hashOutput(output);
            batches[i].priorityOperationsHash = output.priorityOperationsHash;
        }
        uint256[] memory proof = new uint256[](46);
        proof[0] = 0x802;
        proof[2] = 123;
        proofData = bytes.concat(hex"01", abi.encode(previous, batches, proof));
        if (!allEmpty) {
            DutySuccessV1 memory duty;
            duty.account = address(1);
            duty.batchNumber = 1;
            duty.statementHash = keccak256(
                abi.encodePacked(
                    previous.batchHash, batches[0].batchHash, gate.chainConfigHash(), batches[0].commitment
                )
            );
            duty.friProofHash = keccak256("fri artifact");
            duty.transactionCount = 1;
            duties.push(duty);
        }
        accepted.domainVersion = 1;
        accepted.policyHash = POLICY;
        accepted.chainId = 57;
        accepted.chainAddress = address(settlement);
        accepted.parent = gate.lastAcceptedPackage();
        accepted.batchFrom = 1;
        accepted.batchTo = 2;
        accepted.protocolVersion = 32;
        accepted.vkHash = verifier.vkHash();
        accepted.manifestHash = keccak256("portable manifest");
        accepted.reportHash = ZkSysServiceTypesV1.hashReport(duties);
        accepted.proofHash = keccak256(proofData);
        accepted.sequencer = vm.addr(SEQUENCER_KEY);
        accepted.sequencerBeneficiary = address(999);
        settlement.configure(address(gate), keccak256(proofData));
    }

    function _signature() internal returns (bytes memory) {
        if (gate.priorityWorkId() == bytes32(0)) {
            AcceptedPackageV1 memory proposed = accepted;
            proposed.proofHash = bytes32(0);
            vm.prank(vm.addr(SEQUENCER_KEY));
            gate.openBootstrapPackage(proposed);
        }
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(SEQUENCER_KEY, gate.bootstrapDigest(accepted));
        return abi.encodePacked(r, s, v);
    }

    function testCanonicalProofWithEmptyCompanionAuthenticatesReceipt() public {
        gate.submitBootstrap(accepted, duties, outputs, proofData, _signature());
        assertEq(settlement.verified(), 2);
        bytes32 packageHash = ZkSysServiceTypesV1.hashPackage(accepted);
        assertEq(gate.lastAcceptedPackage(), packageHash);
        assertEq(GateMessenger(address(0x8008)).lastMessage(), abi.encode(gate.ACCEPTED_DOMAIN(), packageHash, true));
    }

    function testBootstrapCannotBypassCheckpointFreeze() public {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(SEQUENCER_KEY, gate.bootstrapDigest(accepted));
        vm.expectRevert(ZkSysProofGateV1.InvalidPackage.selector);
        gate.submitBootstrap(accepted, duties, outputs, proofData, abi.encodePacked(r, s, v));
    }

    function testValidNativeProofCannotOmitOverdueRootPriority() public {
        checkpoint.setPriority(keccak256("root-priority"), 0, 1, 1);
        bytes memory signature = _signature();
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidPrefix.selector);
        gate.submitBootstrap(accepted, duties, outputs, proofData, signature);
        assertEq(settlement.verified(), 0);
    }

    function testExpiredEmptyFreezeMustRefreshAndIncludeNewOverdueRequest() public {
        bytes memory signature = _signature();
        vm.warp(block.timestamp + 601);
        checkpoint.setPriority(keccak256("root-priority"), 0, 1, 1);
        vm.expectRevert(ZkSysPriorityGuardV1.ExpiredWork.selector);
        gate.submitBootstrap(accepted, duties, outputs, proofData, signature);
        vm.prank(vm.addr(SEQUENCER_KEY));
        gate.refreshPriorityCheckpoint();
        vm.expectRevert(ZkSysPriorityGuardV1.InvalidPrefix.selector);
        gate.submitBootstrap(accepted, duties, outputs, proofData, signature);
    }

    function testPriorityCursorAndFreezeRollbackWhenNativeProofFails() public {
        bytes32 item = keccak256("root-priority");
        checkpoint.setPriority(item, 0, 1, 1);
        (
            ZkSysProofGateV1.StoredBatch memory previous,
            ZkSysProofGateV1.StoredBatch[] memory batches,
            uint256[] memory proof
        ) = abi.decode(
            _withoutVersion(proofData), (ZkSysProofGateV1.StoredBatch, ZkSysProofGateV1.StoredBatch[], uint256[])
        );
        outputs[0].l1TxCount = 1;
        outputs[0].priorityOperationsHash = keccak256(abi.encodePacked(keccak256(""), item));
        batches[0].numberOfLayer1Txs = 1;
        batches[0].priorityOperationsHash = outputs[0].priorityOperationsHash;
        batches[0].commitment = _hashOutput(outputs[0]);
        duties[0].transactionCount = 2;
        duties[0].statementHash = keccak256(
            abi.encodePacked(previous.batchHash, batches[0].batchHash, gate.chainConfigHash(), batches[0].commitment)
        );
        proofData = bytes.concat(hex"01", abi.encode(previous, batches, proof));
        accepted.proofHash = keccak256(proofData);
        accepted.reportHash = ZkSysServiceTypesV1.hashReport(duties);
        settlement.configure(address(gate), keccak256(proofData));
        bytes memory signature = _signature();
        uint256[] memory counts = new uint256[](2);
        counts[0] = 1;
        bytes32[] memory items = new bytes32[](1);
        items[0] = item;
        bytes32 workId = gate.priorityWorkId();
        guard.publishPrefixWitness(workId, counts, items, new bytes32[](0), new bytes32[](0));
        settlement.setRefuseProof(true);
        vm.expectRevert("native proof invalid");
        gate.submitBootstrap(accepted, duties, outputs, proofData, signature);
        assertEq(guard.priorityCursor(), 0);
        assertEq(guard.lastVerifiedBatch(), 0);
        assertEq(gate.priorityWorkId(), workId);
        settlement.setRefuseProof(false);
        gate.submitBootstrap(accepted, duties, outputs, proofData, signature);
        assertEq(guard.priorityCursor(), 1);
        assertEq(guard.lastVerifiedBatch(), 2);
        assertEq(gate.priorityWorkId(), bytes32(0));
    }

    function _withoutVersion(bytes memory data) private pure returns (bytes memory result) {
        result = new bytes(data.length - 1);
        for (uint256 i; i < result.length; ++i) {
            result[i] = data[i + 1];
        }
    }

    function testAllEmptyRangeProgressesWithoutServiceCredit() public {
        _buildProof(true);
        assertEq(duties.length, 0);
        gate.submitBootstrap(accepted, duties, outputs, proofData, _signature());
        assertEq(settlement.verified(), 2);
    }

    function testNativeVerifierFailureCannotPublishAcceptance() public {
        bytes32 parent = gate.lastAcceptedPackage();
        bytes memory signature = _signature();
        settlement.setRefuseProof(true);
        vm.expectRevert("native proof invalid");
        gate.submitBootstrap(accepted, duties, outputs, proofData, signature);
        assertEq(gate.lastAcceptedPackage(), parent);
        assertEq(settlement.verified(), 0);
        assertEq(GateMessenger(address(0x8008)).lastMessage().length, 0);
    }

    function testChangedStatementCannotGetCreditForValidProof() public {
        duties[0].statementHash = bytes32(uint256(42));
        accepted.reportHash = ZkSysServiceTypesV1.hashReport(duties);
        bytes memory signature = _signature();
        vm.expectRevert(ZkSysProofGateV1.InvalidDutyStatement.selector);
        gate.submitBootstrap(accepted, duties, outputs, proofData, signature);
    }

    function testCannotClaimEmptyCompanionAsWork() public {
        duties[0].batchNumber = 2;
        accepted.reportHash = ZkSysServiceTypesV1.hashReport(duties);
        bytes memory signature = _signature();
        vm.expectRevert(ZkSysProofGateV1.InvalidDutyStatement.selector);
        gate.submitBootstrap(accepted, duties, outputs, proofData, signature);
    }

    function testOutputPreimageCannotLieAboutTransactions() public {
        outputs[0].l2TxCount = 100;
        bytes memory signature = _signature();
        vm.expectRevert(ZkSysProofGateV1.InvalidProofData.selector);
        gate.submitBootstrap(accepted, duties, outputs, proofData, signature);
    }

    function testDuplicateCanonicalBatchWithinReportRejected() public {
        duties.push(duties[0]);
        accepted.reportHash = ZkSysServiceTypesV1.hashReport(duties);
        bytes memory signature = _signature();
        vm.expectRevert(ZkSysProofGateV1.InvalidDutyStatement.selector);
        gate.submitBootstrap(accepted, duties, outputs, proofData, signature);
    }

    function testBeneficiaryTamperingAndReplayRejected() public {
        bytes memory signature = _signature();
        accepted.sequencerBeneficiary = address(111);
        vm.expectRevert(ZkSysProofGateV1.InvalidPackage.selector);
        gate.submitBootstrap(accepted, duties, outputs, proofData, signature);
        accepted.sequencerBeneficiary = address(999);
        gate.submitBootstrap(accepted, duties, outputs, proofData, signature);
        vm.expectRevert(ZkSysProofGateV1.InvalidPackage.selector);
        gate.submitBootstrap(accepted, duties, outputs, proofData, signature);
    }

    function testTestnetVerifierOrSecondProverRoleFailsClosed() public {
        bytes memory signature = _signature();
        verifier.setFake(true);
        vm.expectRevert(ZkSysProofGateV1.SettlementConfigurationChanged.selector);
        gate.submitBootstrap(accepted, duties, outputs, proofData, signature);
        verifier.setFake(false);
        settlement.setRoleCount(2);
        vm.expectRevert(ZkSysProofGateV1.SettlementConfigurationChanged.selector);
        gate.submitBootstrap(accepted, duties, outputs, proofData, signature);
    }

    function _installReadyCoordinator()
        internal
        returns (ZkSysWrapperCoordinatorV1 coordinator, WrapperCandidateV1 memory candidate)
    {
        vm.warp(100);
        candidate = WrapperCandidateV1(0, address(888), vm.addr(WRAPPER_KEY), address(889));
        bytes32 rosterRoot = ZkSysServiceTypesV1.wrapperLeaf(candidate);
        coordinator = new ZkSysWrapperCoordinatorV1(
            ZkSysWrapperCoordinatorV1.Configuration({
                acceptanceGate: address(gate),
                rootSource: new GateDrawSource(),
                rosterSource: new GateRosterSource(rosterRoot),
                sequencer: vm.addr(SEQUENCER_KEY),
                childChainId: 57,
                childChainAddress: address(settlement),
                policyHash: POLICY,
                productionVkHash: verifier.vkHash(),
                initialParent: gate.lastAcceptedPackage(),
                firstBatch: 1,
                firstServicePeriod: 0,
                turnSeconds: 30,
                nativeRootDraw: false
            })
        );
        gate.installCoordinator(coordinator);
        coordinator.prepareRosterDraw(0);
        coordinator.recordRosterDraw(0);
    }

    function _activateAndOpen()
        internal
        returns (ZkSysWrapperCoordinatorV1 coordinator, WrapperCandidateV1 memory candidate)
    {
        (coordinator, candidate) = _installReadyCoordinator();
        gate.activateService();
        vm.warp(1_000);
        accepted.rosterRoot = ZkSysServiceTypesV1.wrapperLeaf(candidate);
        accepted.proofHash = bytes32(0);
        vm.prank(vm.addr(SEQUENCER_KEY));
        gate.openPackage(accepted);
        accepted.proofHash = keccak256(proofData);
        accepted.wrapper = candidate.operator;
        accepted.wrapperBeneficiary = candidate.beneficiary;
    }

    function testGatewayCannotActivateAtChildServiceDeadline() public {
        _installReadyCoordinator();
        vm.warp(1_000);
        vm.expectRevert(ZkSysProofGateV1.WrongPhase.selector);
        gate.activateService();
        assertFalse(gate.serviceActive());
    }

    function _jointSignatures(ZkSysWrapperCoordinatorV1 coordinator)
        internal
        view
        returns (bytes memory, bytes memory)
    {
        bytes32 digest = coordinator.packageDigest(accepted);
        (uint8 sv, bytes32 sr, bytes32 ss) = vm.sign(SEQUENCER_KEY, digest);
        (uint8 wv, bytes32 wr, bytes32 ws) = vm.sign(WRAPPER_KEY, digest);
        return (abi.encodePacked(sr, ss, sv), abi.encodePacked(wr, ws, wv));
    }

    function testJointPackageNativeFailureRollsBackCoordinatorAndRetrySucceeds() public {
        (ZkSysWrapperCoordinatorV1 coordinator, WrapperCandidateV1 memory candidate) = _activateAndOpen();
        (bytes memory seqSig, bytes memory wrapSig) = _jointSignatures(coordinator);
        bytes32 parent = coordinator.acceptedParent();
        settlement.setRefuseProof(true);
        vm.expectRevert("native proof invalid");
        gate.submit(accepted, duties, outputs, proofData, candidate, new bytes32[](0), seqSig, wrapSig);
        assertEq(coordinator.acceptedParent(), parent);
        assertTrue(coordinator.packageOpen());
        assertEq(coordinator.packageOrdinal(), 0);
        settlement.setRefuseProof(false);
        gate.submit(accepted, duties, outputs, proofData, candidate, new bytes32[](0), seqSig, wrapSig);
        assertFalse(coordinator.packageOpen());
        assertEq(coordinator.packageOrdinal(), 1);
        assertEq(settlement.verified(), 2);
        assertEq(coordinator.acceptedParent(), gate.lastAcceptedPackage());
    }

    function testRepairInvalidatesOldEndorsementWithoutResettingWrapperTurn() public {
        (ZkSysWrapperCoordinatorV1 coordinator, WrapperCandidateV1 memory candidate) = _activateAndOpen();
        (bytes memory seqSig, bytes memory wrapSig) = _jointSignatures(coordinator);
        AcceptedPackageV1 memory proposed = coordinator.frozenPackage();
        proposed.manifestHash = keccak256("repaired immutable manifest");
        vm.warp(1_010);
        vm.prank(vm.addr(SEQUENCER_KEY));
        gate.repairPackage(proposed);
        assertEq(coordinator.turnsStartedAt(), 1_000);
        assertEq(coordinator.packageOrdinal(), 0);
        vm.expectRevert(ZkSysWrapperCoordinatorV1.InvalidPackage.selector);
        gate.submit(accepted, duties, outputs, proofData, candidate, new bytes32[](0), seqSig, wrapSig);
        accepted.manifestHash = proposed.manifestHash;
        (seqSig, wrapSig) = _jointSignatures(coordinator);
        gate.submit(accepted, duties, outputs, proofData, candidate, new bytes32[](0), seqSig, wrapSig);
        assertEq(settlement.verified(), 2);
    }

    function testExpiredPriorityRefreshPreservesWrapperClockAndSelection() public {
        (ZkSysWrapperCoordinatorV1 coordinator,) = _activateAndOpen();
        vm.warp(1_601);
        uint32 turnBefore = coordinator.currentTurn();
        vm.prank(vm.addr(SEQUENCER_KEY));
        gate.refreshPriorityCheckpoint();
        assertEq(coordinator.turnsStartedAt(), 1_000);
        assertEq(coordinator.currentTurn(), turnBefore);
        assertEq(coordinator.packageOrdinal(), 0);
        assertTrue(coordinator.packageOpen());
    }
}
