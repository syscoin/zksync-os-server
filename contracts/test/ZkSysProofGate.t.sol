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
import {ZkSysWrapperCoordinatorV1} from "contracts/src/zksys/ZkSysWrapperCoordinatorV1.sol";

contract GateDrawSource is IZkSysAuthenticatedRootSourceV1 {
    function drawFor(bytes32) external pure returns (uint64, bytes32) {
        return (123, keccak256("root draw"));
    }
}

contract GateRosterSource is IZkSysWrapperRosterSourceV1 {
    bytes32 public root;
    uint64 public period;

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
        return (period, root, 1);
    }

    function setPeriod(uint64 period_) external {
        period = period_;
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

    function setVerified(uint256 value) external {
        verified = value;
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
        gate = new ZkSysProofGateV1(
            settlement,
            settlement,
            vm.addr(SEQUENCER_KEY),
            POLICY,
            verifier.vkHash(),
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
        previous.batchNumber = uint64(settlement.getTotalBatchesVerified());
        previous.batchHash = keccak256(abi.encode("state", previous.batchNumber));
        ZkSysProofGateV1.StoredBatch[] memory batches = new ZkSysProofGateV1.StoredBatch[](2);
        for (uint256 i; i < 2; ++i) {
            ZkSysProofGateV1.BatchOutput memory output;
            output.firstBlockTimestamp = uint64(10 + i);
            output.lastBlockTimestamp = uint64(10 + i);
            output.l2TxCount = !allEmpty && i == 0 ? 1 : 0;
            output.settlementChainId = block.chainid;
            output.priorityOperationsHash = keccak256("");
            outputs.push(output);
            batches[i].batchNumber = previous.batchNumber + uint64(i + 1);
            batches[i].batchHash = keccak256(abi.encode("state", batches[i].batchNumber));
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
            duty.batchNumber = batches[0].batchNumber;
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
        accepted.batchFrom = batches[0].batchNumber;
        accepted.batchTo = batches[1].batchNumber;
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
        if (gate.bootstrapPackage().batchFrom == 0) {
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

    function testBootstrapRequiresOpenPackage() public {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(SEQUENCER_KEY, gate.bootstrapDigest(accepted));
        vm.expectRevert(ZkSysProofGateV1.InvalidPackage.selector);
        gate.submitBootstrap(accepted, duties, outputs, proofData, abi.encodePacked(r, s, v));
    }

    function testDeploymentFrontierExcludesPreviouslyProvenBatchesFromServiceWork() public {
        settlement.setVerified(40);
        ZkSysProofGateV1 fresh = new ZkSysProofGateV1(
            settlement,
            settlement,
            vm.addr(SEQUENCER_KEY),
            POLICY,
            verifier.vkHash(),
            IZkSysServiceMessageSinkV1(address(0))
        );
        assertEq(fresh.initialNativeBatch(), 40);
        assertEq(fresh.observedNativeBatch(), 40);
        settlement.setVerified(41);
        fresh.observeNativeSettlement(41);
        assertEq(fresh.initialNativeBatch(), 40);
        assertEq(fresh.observedNativeBatch(), 41);
    }

    function testBootstrapFollowsNativeAdvanceAndRetiresTheOverlappingPackage() public {
        bytes memory staleSignature = _signature();
        bytes32 parent = gate.lastAcceptedPackage();
        settlement.setVerified(1);
        vm.expectRevert(ZkSysProofGateV1.InvalidPackage.selector);
        gate.observeNativeSettlement(0);
        assertEq(gate.bootstrapPackage().batchFrom, 1);
        gate.observeNativeSettlement(1);
        gate.observeNativeSettlement(1);
        assertEq(gate.bootstrapPackage().batchFrom, 0);
        assertEq(gate.lastAcceptedPackage(), parent);
        assertEq(GateMessenger(address(0x8008)).lastMessage().length, 0);
        vm.expectRevert(ZkSysProofGateV1.InvalidPackage.selector);
        gate.submitBootstrap(accepted, duties, outputs, proofData, staleSignature);
        _buildProof(false);
        gate.submitBootstrap(accepted, duties, outputs, proofData, _signature());
        assertEq(gate.observedNativeBatch(), 3);
        assertEq(duties[0].batchNumber, 2);
    }

    function testCannotFollowARewrittenServiceHistory() public {
        gate.submitBootstrap(accepted, duties, outputs, proofData, _signature());
        settlement.setVerified(1);
        vm.expectRevert(ZkSysProofGateV1.SettlementConfigurationChanged.selector);
        gate.observeNativeSettlement(1);
        settlement.setVerified(4);
        gate.observeNativeSettlement(4);
        vm.mockCall(
            address(settlement), abi.encodeCall(settlement.storedBatchHash, (2)), abi.encode(keccak256("replacement"))
        );
        vm.expectRevert(ZkSysProofGateV1.SettlementConfigurationChanged.selector);
        gate.observeNativeSettlement(4);
        assertEq(gate.observedNativeBatch(), 4);
    }

    function testNativePriorityOutputAndOpenPackageSurviveFailedProof() public {
        bytes32 item = keccak256("root-priority");
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
        settlement.setRefuseProof(true);
        vm.expectRevert("native proof invalid");
        gate.submitBootstrap(accepted, duties, outputs, proofData, signature);
        assertEq(gate.bootstrapPackage().batchFrom, 1);
        settlement.setRefuseProof(false);
        gate.submitBootstrap(accepted, duties, outputs, proofData, signature);
        assertEq(gate.bootstrapPackage().batchFrom, 0);
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

    function testBootstrapCannotImportDutyFromEarlierPeriod() public {
        accepted.period = 6;
        duties[0].period = 5;
        accepted.reportHash = ZkSysServiceTypesV1.hashReport(duties);
        bytes memory signature = _signature();
        vm.expectRevert(ZkSysProofGateV1.InvalidDutyStatement.selector);
        gate.submitBootstrap(accepted, duties, outputs, proofData, signature);
        assertEq(settlement.verified(), 0);
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
        return _activateAndOpenPeriod(0);
    }

    function _activateAndOpenPeriod(uint64 period)
        internal
        returns (ZkSysWrapperCoordinatorV1 coordinator, WrapperCandidateV1 memory candidate)
    {
        (coordinator, candidate) = _installReadyCoordinator();
        gate.activateService();
        if (period != 0) {
            vm.warp(999 + uint256(period) * 100);
            GateRosterSource(address(coordinator.rosterSource())).setPeriod(period);
            coordinator.prepareRosterDraw(period);
            coordinator.recordRosterDraw(period);
        }
        vm.warp(1_000 + uint256(period) * 100);
        accepted.period = period;
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

    function testNextPeriodServicePackagePreservesEarlierDutyPeriod() public {
        duties[0].period = 5;
        accepted.reportHash = ZkSysServiceTypesV1.hashReport(duties);
        (ZkSysWrapperCoordinatorV1 coordinator, WrapperCandidateV1 memory candidate) = _activateAndOpenPeriod(6);
        (bytes memory seqSig, bytes memory wrapSig) = _jointSignatures(coordinator);
        gate.submit(accepted, duties, outputs, proofData, candidate, new bytes32[](0), seqSig, wrapSig);
        assertEq(settlement.verified(), 2);
        assertEq(duties[0].period, 5);
        assertEq(coordinator.acceptedParent(), gate.lastAcceptedPackage());
        assertEq(
            GateMessenger(address(0x8008)).lastMessage(),
            abi.encode(gate.ACCEPTED_DOMAIN(), gate.lastAcceptedPackage(), false)
        );
    }

    function testServicePackageCannotImportFutureDutyPeriod() public {
        duties[0].period = 7;
        accepted.reportHash = ZkSysServiceTypesV1.hashReport(duties);
        (ZkSysWrapperCoordinatorV1 coordinator, WrapperCandidateV1 memory candidate) = _activateAndOpenPeriod(6);
        (bytes memory seqSig, bytes memory wrapSig) = _jointSignatures(coordinator);
        vm.expectRevert(ZkSysProofGateV1.InvalidDutyStatement.selector);
        gate.submit(accepted, duties, outputs, proofData, candidate, new bytes32[](0), seqSig, wrapSig);
        assertEq(settlement.verified(), 0);
        assertTrue(coordinator.packageOpen());
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

    function testCoordinatorOpensAtNativeTipAfterIndependentSettlement() public {
        (ZkSysWrapperCoordinatorV1 coordinator, WrapperCandidateV1 memory candidate) = _activateAndOpen();
        (bytes memory oldSequencer, bytes memory oldWrapper) = _jointSignatures(coordinator);
        bytes32 parent = coordinator.acceptedParent();
        settlement.setVerified(110);
        assertEq(coordinator.nextBatch(), 111);
        vm.expectRevert(ZkSysProofGateV1.InvalidPackage.selector);
        gate.submit(accepted, duties, outputs, proofData, candidate, new bytes32[](0), oldSequencer, oldWrapper);
        _buildProof(false);
        AcceptedPackageV1 memory proposed = accepted;
        proposed.wrapper = address(0);
        proposed.wrapperBeneficiary = address(0);
        proposed.proofHash = bytes32(0);
        vm.expectEmit(true, true, false, true, address(gate));
        emit ZkSysProofGateV1.NativeSettlementAdvanced(1, 110, settlement.storedBatchHash(110));
        vm.prank(vm.addr(SEQUENCER_KEY));
        gate.openPackage(proposed);
        assertEq(coordinator.frozenPackage().batchFrom, 111);
        assertEq(coordinator.acceptedParent(), parent);
        assertEq(coordinator.packageOrdinal(), 0);
        assertEq(
            GateMessenger(address(0x8008)).lastMessage(),
            abi.encode(gate.ACTIVATION_DOMAIN(), verifier.vkHash(), accepted.rosterRoot)
        );
        (bytes memory seqSig, bytes memory wrapSig) = _jointSignatures(coordinator);
        gate.submit(accepted, duties, outputs, proofData, candidate, new bytes32[](0), seqSig, wrapSig);
        assertEq(coordinator.nextBatch(), 113);
        assertEq(coordinator.acceptedParent(), gate.lastAcceptedPackage());
        assertEq(coordinator.packageOrdinal(), 1);
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
}
