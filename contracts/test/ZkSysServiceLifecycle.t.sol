// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts-v4/proxy/ERC1967/ERC1967Proxy.sol";
import {GateVerifier, GateSettlement, GateMessenger, GatePriorityCheckpoint} from "./ZkSysProofGate.t.sol";
import {ServiceMessageVerifierMockV1} from "./ZkSysProverServiceV1.t.sol";
import {RootDrawMailbox, RootDrawHub} from "./ZkSysRootDraw.t.sol";
import {ZkSysPriorityGuardV1, IZkSysPriorityMailboxV1} from "contracts/src/zksys/ZkSysPriorityGuardV1.sol";
import {ZkSysProofGateV1} from "contracts/src/zksys/ZkSysProofGateV1.sol";
import {ZkSysMembershipRegistry} from "contracts/src/zksys/ZkSysMembershipRegistry.sol";
import {ZkSysRewardWeightRegistry} from "contracts/src/zksys/ZkSysRewardWeightRegistry.sol";
import {SyscoinZKSYSToken} from "contracts/src/zksys/SyscoinZKSYSToken.sol";
import {
    ZkSysIssuer,
    IZkSysMintableToken,
    IZkSysRewardWeightSource,
    IZkSysProverServiceSource
} from "contracts/src/zksys/ZkSysIssuer.sol";
import {ZkSysProverServiceRegistryV1} from "contracts/src/zksys/ZkSysProverServiceRegistryV1.sol";
import {ZkSysWrapperCoordinatorV1} from "contracts/src/zksys/ZkSysWrapperCoordinatorV1.sol";
import {ZkSysRootDrawV1, ZkSysRootDrawReceiverV1} from "contracts/src/zksys/ZkSysRootDrawV1.sol";
import {
    ZkSysAcceptedServiceReceiverV1,
    ZkSysQualifiedRosterReceiverV1,
    IZkSysChildMailboxV1,
    ServiceMessageProofV1
} from "contracts/src/zksys/ZkSysServiceMessageReceiversV1.sol";
import {
    AcceptedPackageV1,
    DutySuccessV1,
    ProverSubscriptionV1,
    WrapperCandidateV1,
    IZkSysAuthenticatedRootSourceV1,
    ZkSysServiceTypesV1,
    IZkSysServiceMessageSinkV1
} from "contracts/src/zksys/ZkSysServiceTypesV1.sol";

/// @dev Composition test with real service/accounting contracts. Native cryptographic proof
/// verification and network message inclusion are explicit mocks, not a deployed-chain qualification.
contract ZkSysServiceLifecycleTest is Test {
    uint256 private constant CHILD = 57;
    uint256 private constant GATEWAY = 5050;
    uint256 private constant ROOT = 57_000;
    uint256 private constant START = 10_000;
    uint256 private constant PERIOD = 1 days;
    uint256 private constant SEQUENCER_KEY = 777;
    uint256 private constant ALICE_KEY = 11;
    uint256 private constant OPERATOR_KEY = 12;
    uint256 private constant BOB_KEY = 21;
    bytes32 private constant POLICY = keccak256("lifecycle-policy");
    address private constant BENEFICIARY = address(0xBEEF);

    GateVerifier private verifier;
    GateSettlement private settlement;
    ZkSysProofGateV1 private gate;
    ZkSysMembershipRegistry private membership;
    ZkSysRewardWeightRegistry private weights;
    SyscoinZKSYSToken private token;
    ZkSysIssuer private issuer;
    ZkSysProverServiceRegistryV1 private service;
    ZkSysAcceptedServiceReceiverV1 private acceptedReceiver;
    ZkSysQualifiedRosterReceiverV1 private rosterReceiver;
    ZkSysWrapperCoordinatorV1 private coordinator;
    ZkSysRootDrawReceiverV1 private drawReceiver;
    ZkSysRootDrawV1 private drawSource;
    RootDrawMailbox private rootMailbox;
    RootDrawHub private hub;
    ServiceMessageVerifierMockV1 private mailbox;
    bytes32 private subscriptionHash;
    bytes32 private rosterRoot;
    WrapperCandidateV1 private candidate;
    bytes32[] private candidateProof;
    AcceptedPackageV1 private accepted;
    DutySuccessV1[] private duties;
    ZkSysProofGateV1.BatchOutput[] private outputs;
    bytes private proofData;

    function setUp() public {
        _setUp(false);
    }

    function _setUp(bool gatewayEnabled) private {
        vm.warp(1_000);
        vm.chainId(GATEWAY);
        verifier = new GateVerifier();
        settlement = new GateSettlement(verifier);
        GatePriorityCheckpoint checkpoint = new GatePriorityCheckpoint(POLICY);
        address predictedGate = vm.computeCreateAddress(address(this), vm.getNonce(address(this)) + 1);
        ZkSysPriorityGuardV1 guard = new ZkSysPriorityGuardV1(
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
        GateMessenger messenger = new GateMessenger();
        vm.etch(address(0x8008), address(messenger).code);
        mailbox = new ServiceMessageVerifierMockV1();
        vm.etch(address(0x10009), address(mailbox).code);

        vm.chainId(CHILD);
        _deployAccounting();
        address predictedRegistry = vm.computeCreateAddress(address(this), vm.getNonce(address(this)) + 1);
        acceptedReceiver = new ZkSysAcceptedServiceReceiverV1(
            GATEWAY,
            address(gate),
            ZkSysProverServiceRegistryV1(predictedRegistry),
            gatewayEnabled ? address(0xBEEF1234) : address(0)
        );
        service = new ZkSysProverServiceRegistryV1(
            ZkSysProverServiceRegistryV1.Configuration({
                membershipRegistry: membership,
                issuer: address(issuer),
                acceptanceSource: address(acceptedReceiver),
                settlementChainAddress: address(settlement),
                policyHash: POLICY,
                firstServicePeriod: 0,
                receiptGraceSeconds: 100,
                membershipMaxAgeSeconds: 1_000_000,
                rosterPublicationLeadSeconds: 100,
                dutiesPerRound: 2,
                gatewayChainId: gatewayEnabled ? GATEWAY : 0,
                gatewayChainAddress: gatewayEnabled ? address(0x600D) : address(0),
                gatewayVkHash: gatewayEnabled ? verifier.vkHash() : bytes32(0),
                sharedSequencer: gatewayEnabled ? vm.addr(SEQUENCER_KEY) : address(0)
            })
        );
        assertEq(address(service), predictedRegistry);
        issuer.configureServiceAccounting(IZkSysProverServiceSource(address(service)), 0);
        _installSenior(vm.addr(ALICE_KEY));
        _installSenior(vm.addr(BOB_KEY));
        subscriptionHash = _subscribe(ALICE_KEY, OPERATOR_KEY);
        _subscribe(BOB_KEY, 22);
    }

    function _deployAccounting() private {
        token = SyscoinZKSYSToken(
            address(
                new ERC1967Proxy(
                    address(new SyscoinZKSYSToken()),
                    abi.encodeCall(SyscoinZKSYSToken.initialize, ("ZKSYS", "ZKSYS", uint8(18), address(this)))
                )
            )
        );
        membership = ZkSysMembershipRegistry(
            address(
                new ERC1967Proxy(
                    address(new ZkSysMembershipRegistry()),
                    abi.encodeCall(ZkSysMembershipRegistry.initialize, (address(this), address(0xA11CE)))
                )
            )
        );
        weights = ZkSysRewardWeightRegistry(
            address(
                new ERC1967Proxy(
                    address(new ZkSysRewardWeightRegistry()),
                    abi.encodeCall(ZkSysRewardWeightRegistry.initialize, (address(this), membership, uint256(1)))
                )
            )
        );
        issuer = ZkSysIssuer(
            address(
                new ERC1967Proxy(
                    address(new ZkSysIssuer()),
                    abi.encodeCall(
                        ZkSysIssuer.initialize,
                        (
                            IZkSysMintableToken(address(token)),
                            IZkSysRewardWeightSource(address(weights)),
                            address(this),
                            START,
                            PERIOD,
                            uint256(365)
                        )
                    )
                )
            )
        );
        weights.setWeightReceiver(issuer);
        membership.setSentryNodeReceiver(weights);
        token.grantRole(token.MINTER_ROLE(), address(issuer));
    }

    function _installSenior(address account) private {
        ZkSysMembershipRegistry.SentryNodeUpdate[] memory updates = new ZkSysMembershipRegistry.SentryNodeUpdate[](1);
        updates[0] = ZkSysMembershipRegistry.SentryNodeUpdate(account, 1_000, uint128(135_000 ether));
        address alias_ = membership.aliasedL1RegistryBridge();
        vm.prank(alias_);
        membership.applyL1SentryNodeUpdates(updates, 211_240, uint64(block.timestamp));
        weights.activatePendingWeightFor(account);
    }

    function _subscribe(uint256 accountKey, uint256 operatorKey) private returns (bytes32) {
        ProverSubscriptionV1 memory sub = ProverSubscriptionV1({
            account: vm.addr(accountKey),
            operator: vm.addr(operatorKey),
            beneficiary: BENEFICIARY,
            sequencer: vm.addr(SEQUENCER_KEY),
            firstPeriod: 0,
            lastPeriod: 3,
            nonce: 0,
            services: 3
        });
        return service.subscribe(sub, _sign(accountKey, service.subscriptionDigest(sub)));
    }

    function _sign(uint256 key, bytes32 digest) private pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, digest);
        return abi.encodePacked(r, s, v);
    }

    function _hashOutput(ZkSysProofGateV1.BatchOutput memory o) private pure returns (bytes32) {
        return keccak256(
            abi.encodePacked(
                o.firstBlockTimestamp,
                o.lastBlockTimestamp,
                o.daScheme,
                o.daCommitment,
                o.l1TxCount,
                o.l2TxCount,
                o.priorityOperationsHash,
                o.l2LogsRoot,
                o.upgradeTxHash,
                o.dependencyRootsRollingHash,
                o.settlementChainId,
                o.edgeDARefsRoot
            )
        );
    }

    function _buildPackage(uint64 from) private {
        delete outputs;
        delete duties;
        ZkSysProofGateV1.StoredBatch memory previous;
        previous.batchNumber = from - 1;
        previous.batchHash = keccak256(abi.encode("state", from - 1));
        ZkSysProofGateV1.StoredBatch[] memory batches = new ZkSysProofGateV1.StoredBatch[](2);
        bytes32 priorHash = previous.batchHash;
        for (uint64 i; i < 2; ++i) {
            ZkSysProofGateV1.BatchOutput memory output;
            output.firstBlockTimestamp = uint64(block.timestamp);
            output.lastBlockTimestamp = uint64(block.timestamp);
            output.l2TxCount = 1;
            output.settlementChainId = GATEWAY;
            output.priorityOperationsHash = keccak256("");
            outputs.push(output);
            batches[i].batchNumber = from + i;
            batches[i].batchHash = keccak256(abi.encode("state", from + i));
            batches[i].commitment = _hashOutput(output);
            batches[i].priorityOperationsHash = output.priorityOperationsHash;
            DutySuccessV1 memory duty;
            duty.account = vm.addr(ALICE_KEY);
            duty.subscriptionHash = subscriptionHash;
            duty.batchNumber = from + i;
            duty.statementHash = keccak256(
                abi.encodePacked(priorHash, batches[i].batchHash, gate.chainConfigHash(), batches[i].commitment)
            );
            duty.friProofHash = keccak256(abi.encode("fri", from + i));
            duty.transactionCount = 1;
            duty.slot = uint16(i);
            duty.attempt = 1;
            duty.assignmentId = keccak256(abi.encode("assignment", from + i));
            vm.chainId(CHILD);
            duty.operatorSignature = _sign(OPERATOR_KEY, service.dutyDigest(duty));
            vm.chainId(GATEWAY);
            duties.push(duty);
            priorHash = batches[i].batchHash;
        }
        uint256[] memory proof = new uint256[](46);
        proof[0] = 0x802;
        proof[2] = 123;
        proofData = bytes.concat(hex"01", abi.encode(previous, batches, proof));
        delete accepted;
        accepted.domainVersion = 1;
        accepted.policyHash = POLICY;
        accepted.chainId = CHILD;
        accepted.chainAddress = address(settlement);
        accepted.parent = gate.lastAcceptedPackage();
        accepted.batchFrom = from;
        accepted.batchTo = from + 1;
        accepted.protocolVersion = 32;
        accepted.vkHash = verifier.vkHash();
        accepted.manifestHash = keccak256(abi.encode("manifest", from));
        accepted.reportHash = ZkSysServiceTypesV1.hashReport(duties);
        accepted.proofHash = keccak256(proofData);
        accepted.sequencer = vm.addr(SEQUENCER_KEY);
        accepted.sequencerBeneficiary = address(999);
        settlement.configure(address(gate), keccak256(proofData));
        if (!gate.serviceActive()) {
            AcceptedPackageV1 memory proposed = accepted;
            proposed.proofHash = bytes32(0);
            vm.prank(vm.addr(SEQUENCER_KEY));
            gate.openBootstrapPackage(proposed);
        }
    }

    function _messageProof() private pure returns (ServiceMessageProofV1 memory) {
        return ServiceMessageProofV1(1, 0, 0, new bytes32[](0));
    }

    function _relayAccepted(bool bootstrap) private {
        bytes memory actual = GateMessenger(address(0x8008)).lastMessage();
        vm.chainId(CHILD);
        ServiceMessageVerifierMockV1(address(0x10009)).expectMessage(GATEWAY, 1, 0, 0, address(gate), actual);
        acceptedReceiver.relayAccepted(accepted, duties, bootstrap, _messageProof());
        acceptedReceiver.relayAccepted(accepted, duties, bootstrap, _messageProof());
    }

    function _qualify() private {
        vm.chainId(GATEWAY);
        _buildPackage(1);
        gate.submitBootstrap(accepted, duties, outputs, proofData, _sign(SEQUENCER_KEY, gate.bootstrapDigest(accepted)));
        _relayAccepted(true);
        assertEq(service.qualifiedBonusWeight(vm.addr(ALICE_KEY), 0), 35_000 ether);
        assertEq(service.qualifiedBonusWeight(vm.addr(BOB_KEY), 0), 0);
        assertEq(service.totalAdmittedBonusWeight(0), 0);
    }

    function _prepareRosterAndDraw() private {
        vm.chainId(CHILD);
        vm.warp(START - 100);
        candidate = WrapperCandidateV1(0, vm.addr(ALICE_KEY), vm.addr(OPERATOR_KEY), BENEFICIARY);
        WrapperCandidateV1[] memory candidates = new WrapperCandidateV1[](1);
        candidates[0] = candidate;
        rosterRoot = service.publishRosterPage(0, candidates);
        bytes memory actualRosterMessage = GateMessenger(address(0x8008)).lastMessage();
        vm.chainId(GATEWAY);
        rosterReceiver = new ZkSysQualifiedRosterReceiverV1(
            ZkSysQualifiedRosterReceiverV1.Configuration({
                childChainId: CHILD,
                childRegistry: address(service),
                messageSender: address(service),
                childMailbox: IZkSysChildMailboxV1(address(mailbox)),
                policyHash: POLICY,
                startTime: START,
                periodSeconds: PERIOD,
                firstServicePeriod: 0
            })
        );
        mailbox.expectMessage(0, 1, 0, 0, address(service), actualRosterMessage);
        rosterReceiver.relayRoster(0, rosterRoot, 1, _messageProof());
        rootMailbox = new RootDrawMailbox();
        hub = new RootDrawHub();
        address predictedSource = vm.computeCreateAddress(address(this), vm.getNonce(address(this)) + 2);
        drawReceiver = new ZkSysRootDrawReceiverV1(predictedSource);
        coordinator = new ZkSysWrapperCoordinatorV1(
            ZkSysWrapperCoordinatorV1.Configuration({
                acceptanceGate: address(gate),
                rootSource: IZkSysAuthenticatedRootSourceV1(address(drawReceiver)),
                rosterSource: rosterReceiver,
                sequencer: vm.addr(SEQUENCER_KEY),
                childChainId: CHILD,
                childChainAddress: address(settlement),
                policyHash: POLICY,
                productionVkHash: verifier.vkHash(),
                initialParent: gate.lastAcceptedPackage(),
                firstBatch: 3,
                firstServicePeriod: 0,
                turnSeconds: 60,
                nativeRootDraw: false
            })
        );
        vm.chainId(ROOT);
        drawSource = new ZkSysRootDrawV1(rootMailbox, hub, address(coordinator), address(drawReceiver), GATEWAY, 3, 2);
        assertEq(address(drawSource), predictedSource);
        vm.chainId(GATEWAY);
        gate.installCoordinator(coordinator);
        bytes32 commitment = coordinator.prepareRosterDraw(0);
        assertEq(GateMessenger(address(0x8008)).lastMessage(), abi.encode(drawSource.DRAW_DOMAIN(), commitment));
        vm.chainId(ROOT);
        rootMailbox.setExpected(address(coordinator), commitment);
        vm.roll(100);
        drawSource.requestDraw(commitment, 1, 0, 0, new bytes32[](0));
        vm.roll(105);
        vm.setBlockhash(103, keccak256("future entropy"));
        drawSource.relayDraw(commitment, 500_000, 800, address(this));
        vm.chainId(GATEWAY);
        address alias_ = drawReceiver.aliasedRootSource();
        bytes memory payload = hub.payload();
        vm.prank(alias_);
        (bool success,) = address(drawReceiver).call(payload);
        assertTrue(success);
        coordinator.recordRosterDraw(0);
        candidateProof = new bytes32[](32);
        bytes32 zero;
        for (uint256 i; i < 32; ++i) {
            candidateProof[i] = zero;
            zero = keccak256(abi.encodePacked(zero, zero));
        }
    }

    function _activate(bool relayToChild) private {
        gate.activateService();
        if (relayToChild) {
            bytes memory actual = GateMessenger(address(0x8008)).lastMessage();
            vm.chainId(CHILD);
            ServiceMessageVerifierMockV1(address(0x10009)).expectMessage(GATEWAY, 1, 0, 0, address(gate), actual);
            acceptedReceiver.relayActivation(verifier.vkHash(), rosterRoot, _messageProof());
        }
    }

    function _serviceProof() private {
        vm.warp(START);
        vm.chainId(GATEWAY);
        _buildPackage(3);
        accepted.rosterRoot = rosterRoot;
        bytes32 proofHash = accepted.proofHash;
        accepted.proofHash = bytes32(0);
        vm.prank(vm.addr(SEQUENCER_KEY));
        gate.openPackage(accepted);
        accepted.proofHash = proofHash;
        accepted.wrapper = vm.addr(OPERATOR_KEY);
        accepted.wrapperBeneficiary = BENEFICIARY;
        bytes32 digest = coordinator.packageDigest(accepted);
        gate.submit(
            accepted,
            duties,
            outputs,
            proofData,
            candidate,
            candidateProof,
            _sign(SEQUENCER_KEY, digest),
            _sign(OPERATOR_KEY, digest)
        );
        _relayAccepted(false);
    }

    function testBootstrapDrawActivationJointProofAndRewardComposition() public {
        _qualify();
        _prepareRosterAndDraw();
        _activate(true);
        _serviceProof();
        assertEq(service.assessedSuccesses(vm.addr(ALICE_KEY), 0), 2);
        assertEq(service.assessedSuccesses(vm.addr(BOB_KEY), 0), 0);
        assertEq(issuer.currentRewardDenominator(), 235_000 ether);
        assertEq(issuer.currentRewardWeight(vm.addr(BOB_KEY)), 100_000 ether);
        vm.warp(START + PERIOD + 100);
        issuer.checkpointServicePeriods(2);
        uint256[] memory periods = new uint256[](1);
        periods[0] = 0;
        vm.prank(vm.addr(ALICE_KEY));
        uint256 bonus = issuer.claimServiceRewards(periods, BENEFICIARY);
        assertGt(bonus, 0);
        assertEq(token.balanceOf(BENEFICIARY), bonus);
        vm.prank(vm.addr(BOB_KEY));
        uint256 passive = issuer.claim(vm.addr(BOB_KEY));
        assertGt(passive, bonus);
        assertEq(service.serviceFactorBps(vm.addr(ALICE_KEY), 0), 10_000);
        assertEq(service.serviceFactorBps(vm.addr(BOB_KEY), 0), 0);
    }

    function testMissingChildActivationIrreversiblyPreservesPassiveAccounting() public {
        _qualify();
        _prepareRosterAndDraw();
        _activate(false);
        bytes memory activationMessage = GateMessenger(address(0x8008)).lastMessage();
        vm.chainId(CHILD);
        vm.warp(START);
        issuer.abortMissedServiceActivation();
        assertTrue(issuer.serviceLaunchAborted());
        assertEq(issuer.currentRewardDenominator(), 200_000 ether);
        ServiceMessageVerifierMockV1(address(0x10009)).expectMessage(GATEWAY, 1, 0, 0, address(gate), activationMessage);
        bytes32 vk = verifier.vkHash();
        vm.expectRevert(ZkSysProverServiceRegistryV1.WrongServiceMode.selector);
        acceptedReceiver.relayActivation(vk, rosterRoot, _messageProof());
        vm.warp(START + PERIOD);
        issuer.checkpointServicePeriods(1);
        vm.prank(vm.addr(BOB_KEY));
        assertGt(issuer.claim(vm.addr(BOB_KEY)), 0);
        assertEq(service.totalAdmittedBonusWeight(0), 0);
    }

    function testMissingGatewayActivationCannotEnableBonusOrLockPassiveClaims() public {
        _setUp(true);
        _qualify();
        _prepareRosterAndDraw();
        _activate(true);
        vm.chainId(CHILD);
        assertFalse(service.serviceActive());
        assertEq(service.totalAdmittedBonusWeight(0), 0);
        assertEq(acceptedReceiver.childActivationVk(), verifier.vkHash());
        assertEq(acceptedReceiver.rootActivationVk(), bytes32(0));
        vm.warp(START);
        issuer.abortMissedServiceActivation();
        assertTrue(issuer.serviceLaunchAborted());
        assertEq(issuer.currentRewardDenominator(), 200_000 ether);
        vm.warp(START + PERIOD);
        issuer.checkpointServicePeriods(1);
        vm.prank(vm.addr(BOB_KEY));
        assertGt(issuer.claim(vm.addr(BOB_KEY)), 0);
    }

    function testPackageSignatureCannotReplaceAuthenticatedGatewayReceipt() public {
        vm.chainId(GATEWAY);
        _buildPackage(1);
        gate.submitBootstrap(accepted, duties, outputs, proofData, _sign(SEQUENCER_KEY, gate.bootstrapDigest(accepted)));
        vm.chainId(CHILD);
        ServiceMessageVerifierMockV1(address(0x10009))
            .expectMessage(GATEWAY, 1, 0, 0, address(0xBAD), GateMessenger(address(0x8008)).lastMessage());
        vm.expectRevert(ZkSysAcceptedServiceReceiverV1.MessageNotIncluded.selector);
        acceptedReceiver.relayAccepted(accepted, duties, true, _messageProof());
        assertEq(service.totalQualifiedBonusWeight(0), 0);
    }
}
