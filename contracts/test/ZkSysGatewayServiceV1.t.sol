// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";
import {
    GateVerifier,
    GateSettlement,
    GatePriorityCheckpoint,
    GateMessenger,
    GateDrawSource
} from "./ZkSysProofGate.t.sol";
import {RootDrawHub} from "./ZkSysRootDraw.t.sol";
import {ServiceClockMockV1, ServiceMembershipMockV1, ServiceMessageVerifierMockV1} from "./ZkSysProverServiceV1.t.sol";
import {ZkSysMembershipRegistry} from "contracts/src/zksys/ZkSysMembershipRegistry.sol";
import {ZkSysProofGateV1} from "contracts/src/zksys/ZkSysProofGateV1.sol";
import {ZkSysPriorityGuardV1, IZkSysPriorityMailboxV1} from "contracts/src/zksys/ZkSysPriorityGuardV1.sol";
import {ZkSysProverServiceRegistryV1} from "contracts/src/zksys/ZkSysProverServiceRegistryV1.sol";
import {ZkSysWrapperCoordinatorV1} from "contracts/src/zksys/ZkSysWrapperCoordinatorV1.sol";
import {ZkSysRootServiceMessageSinkV1, ZkSysNativeRootDrawV1} from "contracts/src/zksys/ZkSysRootServiceV1.sol";
import {
    ZkSysAcceptedServiceReceiverV1,
    ZkSysQualifiedRosterReceiverV1,
    IZkSysChildMailboxV1,
    ServiceMessageProofV1,
    ServiceAcceptedClaimV1,
    ServiceRootAcceptedClaimV1,
    ZkSysL2MessageV1
} from "contracts/src/zksys/ZkSysServiceMessageReceiversV1.sol";
import {
    AcceptedPackageV1,
    DutySuccessV1,
    ProverSubscriptionV1,
    WrapperCandidateV1,
    ZkSysServiceTypesV1,
    IZkSysServiceMessageSinkV1,
    IZkSysAuthenticatedRootSourceV1
} from "contracts/src/zksys/ZkSysServiceTypesV1.sol";

// The native cryptographic verifier and cross-chain proof transport are explicit mocks. The real
// gates, coordinators, root draw/outbox, receiver and shared accounting compose around them.
contract GatewayRosterMailboxMock is IZkSysChildMailboxV1 {
    GateSettlement public immutable settlement;
    bytes32 public expected;
    uint256 public minimumVerified;

    constructor(GateSettlement settlement_) {
        settlement = settlement_;
    }

    function expectMessage(address sender, bytes calldata data, uint256 minimum) external {
        expected = keccak256(abi.encode(sender, data));
        minimumVerified = minimum;
    }

    function proveL2MessageInclusion(uint256, uint256, ZkSysL2MessageV1 calldata message, bytes32[] calldata)
        external
        view
        returns (bool)
    {
        return settlement.verified() >= minimumVerified
            && expected == keccak256(abi.encode(message.sender, message.data));
    }
}

/// @dev Models the direct-L1 batch-root comparison while checking the actual message leaf/path.
contract GatewayMerkleSettlement is GateSettlement, IZkSysChildMailboxV1 {
    mapping(uint256 => bytes32) public executedBatchRoot;

    constructor(GateVerifier verifier_) GateSettlement(verifier_) {}

    function setExecutedBatchRoot(uint256 batch, bytes32 root) external {
        executedBatchRoot[batch] = root;
    }

    function proveL2MessageInclusion(
        uint256 batch,
        uint256 index,
        ZkSysL2MessageV1 calldata message,
        bytes32[] calldata proof
    ) external view returns (bool) {
        if (proof.length != 2 || index > 1 || executedBatchRoot[batch] == bytes32(0)) return false;
        bytes32 leaf = keccak256(
            abi.encodePacked(
                uint8(0),
                true,
                message.txNumberInBatch,
                address(0x8008),
                bytes32(uint256(uint160(message.sender))),
                keccak256(message.data)
            )
        );
        bytes32 localRoot =
            index == 0 ? keccak256(abi.encodePacked(leaf, proof[0])) : keccak256(abi.encodePacked(proof[0], leaf));
        return keccak256(abi.encodePacked(localRoot, proof[1])) == executedBatchRoot[batch];
    }
}

contract GatewayRootMappingMock {
    mapping(uint256 => mapping(uint256 => bytes32)) public chainBatchRoots;

    function setRoot(uint256 chain, uint256 batch, bytes32 root) external {
        chainBatchRoots[chain][batch] = root;
    }
}

contract ZkSysGatewayServiceV1Test is Test {
    uint256 private constant CHILD = 57;
    uint256 private constant GATEWAY = 5050;
    uint256 private constant ROOT = 57_000;
    uint256 private constant ACCOUNT_KEY = 11;
    uint256 private constant OPERATOR_KEY = 12;
    uint256 private constant SEQUENCER_KEY = 777;
    bytes32 private constant POLICY = keccak256("shared-gateway-service");
    bytes32 private constant ROOT_VK = keccak256("gateway-production-vk");
    uint256 private constant START = 10_000;
    uint256 private nextGatewayRootBatch = 1_000;

    struct Lane {
        uint256 executionId;
        uint256 settlementId;
        GateVerifier verifier;
        GateSettlement settlement;
        ZkSysProofGateV1 gate;
        ZkSysWrapperCoordinatorV1 coordinator;
    }

    struct Package {
        AcceptedPackageV1 accepted;
        DutySuccessV1[] duties;
        ZkSysProofGateV1.BatchOutput[] outputs;
        bytes proofData;
    }

    Lane private child;
    Lane private gateway;
    RootDrawHub private hub;
    ZkSysRootServiceMessageSinkV1 private sink;
    ZkSysAcceptedServiceReceiverV1 private receiver;
    ZkSysProverServiceRegistryV1 private service;
    ServiceMembershipMockV1 private membership;
    ServiceClockMockV1 private clock;
    ServiceMessageVerifierMockV1 private childMailbox;
    GatewayRosterMailboxMock private rootMailbox;
    ZkSysQualifiedRosterReceiverV1 private gatewayRoster;
    ZkSysQualifiedRosterReceiverV1 private rootRoster;
    ZkSysNativeRootDrawV1 private rootDraw;
    bytes32 private subscriptionHash;
    bytes32 private rosterRoot;
    WrapperCandidateV1 private candidate;
    bytes32[] private candidateProof;

    function setUp() public {
        vm.warp(1_000);
        vm.roll(100);
        GateMessenger messenger = new GateMessenger();
        vm.etch(address(0x8008), address(messenger).code);
        childMailbox = new ServiceMessageVerifierMockV1();
        vm.etch(address(0x10009), address(childMailbox).code);
        hub = new RootDrawHub();
        membership = new ServiceMembershipMockV1();
        clock = new ServiceClockMockV1();
        child.executionId = CHILD;
        child.settlementId = GATEWAY;
        gateway.executionId = GATEWAY;
        gateway.settlementId = ROOT;
        child.verifier = new GateVerifier();
        gateway.verifier = new GateVerifier();
        gateway.verifier.setVkHash(ROOT_VK);
        child.settlement = new GateSettlement(child.verifier);
        gateway.settlement = new GatewayMerkleSettlement(gateway.verifier);
        gateway.settlement.setChainId(GATEWAY);
        hub.setChain(GATEWAY, address(gateway.settlement));
        GatePriorityCheckpoint childCheckpoint = new GatePriorityCheckpoint(POLICY);
        GatePriorityCheckpoint rootCheckpoint = new GatePriorityCheckpoint(POLICY);
        rootCheckpoint.setChainId(GATEWAY);

        vm.chainId(GATEWAY);
        address predictedGate = vm.computeCreateAddress(address(this), vm.getNonce(address(this)) + 1);
        ZkSysPriorityGuardV1 guard = new ZkSysPriorityGuardV1(
            predictedGate, IZkSysPriorityMailboxV1(address(child.settlement)), childCheckpoint, POLICY, 100, 600, 64
        );
        child.gate = new ZkSysProofGateV1(
            child.settlement,
            child.settlement,
            vm.addr(SEQUENCER_KEY),
            POLICY,
            child.verifier.vkHash(),
            guard,
            IZkSysServiceMessageSinkV1(address(0))
        );
        child.settlement.configure(address(child.gate), bytes32(0));

        vm.chainId(ROOT);
        uint64 nonce = vm.getNonce(address(this));
        predictedGate = vm.computeCreateAddress(address(this), nonce + 2);
        address predictedReceiver = vm.computeCreateAddress(address(this), nonce + 3);
        sink = new ZkSysRootServiceMessageSinkV1(
            ZkSysRootServiceMessageSinkV1.Configuration({
                publisher: predictedGate,
                gatewayGate: address(child.gate),
                bridgehub: hub,
                gatewayChainId: GATEWAY,
                gatewayChainAddress: address(gateway.settlement),
                registryChainId: CHILD,
                registryReceiver: predictedReceiver,
                policyHash: POLICY
            })
        );
        guard = new ZkSysPriorityGuardV1(
            predictedGate, IZkSysPriorityMailboxV1(address(gateway.settlement)), rootCheckpoint, POLICY, 100, 600, 64
        );
        gateway.gate = new ZkSysProofGateV1(
            gateway.settlement, gateway.settlement, vm.addr(SEQUENCER_KEY), POLICY, ROOT_VK, guard, sink
        );
        gateway.settlement.configure(address(gateway.gate), bytes32(0));

        vm.chainId(CHILD);
        address predictedRegistry = vm.computeCreateAddress(address(this), vm.getNonce(address(this)) + 1);
        receiver = new ZkSysAcceptedServiceReceiverV1(
            GATEWAY, address(child.gate), ZkSysProverServiceRegistryV1(predictedRegistry), address(sink)
        );
        assertEq(address(receiver), predictedReceiver);
        service = new ZkSysProverServiceRegistryV1(
            ZkSysProverServiceRegistryV1.Configuration({
                membershipRegistry: ZkSysMembershipRegistry(address(membership)),
                issuer: address(clock),
                acceptanceSource: address(receiver),
                settlementChainAddress: address(child.settlement),
                policyHash: POLICY,
                firstServicePeriod: 0,
                receiptGraceSeconds: 100,
                rosterPublicationLeadSeconds: 100,
                dutiesPerRound: 2,
                gatewayChainId: GATEWAY,
                gatewayChainAddress: address(gateway.settlement),
                gatewayVkHash: ROOT_VK,
                sharedSequencer: vm.addr(SEQUENCER_KEY)
            })
        );
        membership.set(vm.addr(ACCOUNT_KEY), 1_000, 135_000 ether, 211_240, uint64(block.timestamp));
        ProverSubscriptionV1 memory sub = ProverSubscriptionV1(
            vm.addr(ACCOUNT_KEY), vm.addr(OPERATOR_KEY), address(0xBEEF), vm.addr(SEQUENCER_KEY), 0, 10, 0, 3
        );
        bytes32 digest = service.subscriptionDigest(sub);
        subscriptionHash = service.subscribe(sub, _sign(ACCOUNT_KEY, digest), _sign(OPERATOR_KEY, digest));
    }

    function _sign(uint256 key, bytes32 digest) private pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, digest);
        return abi.encodePacked(r, s, v);
    }

    function _inclusion() private pure returns (ServiceMessageProofV1 memory) {
        return ServiceMessageProofV1(1, 0, 0, new bytes32[](0));
    }

    function _outputHash(ZkSysProofGateV1.BatchOutput memory o) private pure returns (bytes32) {
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

    function _package(Lane memory lane, uint16 slot, bool positive) private returns (Package memory p) {
        vm.chainId(lane.settlementId);
        uint64 from = uint64(lane.settlement.verified() + 1);
        ZkSysProofGateV1.StoredBatch memory previous;
        previous.batchNumber = from - 1;
        previous.batchHash = keccak256(abi.encode(lane.executionId, from - 1));
        ZkSysProofGateV1.StoredBatch[] memory batches = new ZkSysProofGateV1.StoredBatch[](2);
        p.outputs = new ZkSysProofGateV1.BatchOutput[](2);
        for (uint256 i; i < 2; ++i) {
            p.outputs[i].firstBlockTimestamp = uint64(block.timestamp);
            p.outputs[i].lastBlockTimestamp = uint64(block.timestamp);
            p.outputs[i].settlementChainId = lane.settlementId;
            p.outputs[i].priorityOperationsHash = keccak256("");
            p.outputs[i].l2TxCount = positive && i == 0 ? 1 : 0;
            batches[i].batchNumber = from + uint64(i);
            batches[i].batchHash = keccak256(abi.encode(lane.executionId, from + i));
            batches[i].priorityOperationsHash = p.outputs[i].priorityOperationsHash;
            batches[i].commitment = _outputHash(p.outputs[i]);
        }
        uint256[] memory proof = new uint256[](46);
        proof[0] = 0x802;
        proof[2] = 123;
        p.proofData = bytes.concat(hex"01", abi.encode(previous, batches, proof));
        p.duties = new DutySuccessV1[](positive ? 1 : 0);
        if (positive) {
            DutySuccessV1 memory duty = DutySuccessV1({
                account: vm.addr(ACCOUNT_KEY),
                subscriptionHash: subscriptionHash,
                batchNumber: from,
                statementHash: keccak256(
                    abi.encodePacked(
                        previous.batchHash, batches[0].batchHash, lane.gate.chainConfigHash(), batches[0].commitment
                    )
                ),
                friProofHash: keccak256(abi.encode("fri", lane.executionId, from)),
                transactionCount: 1,
                period: 0,
                slot: slot,
                attempt: 1,
                assignmentId: keccak256(abi.encode("assignment", lane.executionId, from)),
                operatorSignature: bytes("")
            });
            vm.chainId(CHILD);
            duty.operatorSignature = _sign(OPERATOR_KEY, service.dutyDigest(duty));
            vm.chainId(lane.settlementId);
            p.duties[0] = duty;
        }
        p.accepted = AcceptedPackageV1({
            domainVersion: 1,
            policyHash: POLICY,
            chainId: lane.executionId,
            chainAddress: address(lane.settlement),
            parent: lane.gate.lastAcceptedPackage(),
            batchFrom: from,
            batchTo: from + 1,
            protocolVersion: 32,
            vkHash: lane.verifier.vkHash(),
            period: 0,
            rosterRoot: bytes32(0),
            turn: 0,
            manifestHash: keccak256(abi.encode("manifest", lane.executionId, from)),
            reportHash: ZkSysServiceTypesV1.hashReport(p.duties),
            proofHash: keccak256(p.proofData),
            sequencer: vm.addr(SEQUENCER_KEY),
            sequencerBeneficiary: address(0x5E9),
            wrapper: address(0),
            wrapperBeneficiary: address(0)
        });
        lane.settlement.configure(address(lane.gate), keccak256(p.proofData));
    }

    function _bootstrap(Lane memory lane, uint16 slot, bool positive) private returns (Package memory p) {
        p = _package(lane, slot, positive);
        AcceptedPackageV1 memory proposed = abi.decode(abi.encode(p.accepted), (AcceptedPackageV1));
        proposed.proofHash = bytes32(0);
        vm.prank(vm.addr(SEQUENCER_KEY));
        lane.gate.openBootstrapPackage(proposed);
        lane.gate
            .submitBootstrap(
                p.accepted,
                p.duties,
                p.outputs,
                p.proofData,
                _sign(SEQUENCER_KEY, lane.gate.bootstrapDigest(p.accepted))
            );
    }

    function _relayAccepted(Lane memory lane, Package memory p, bool bootstrap) private {
        if (lane.executionId == CHILD) {
            bytes memory message = GateMessenger(address(0x8008)).lastMessage();
            ServiceMessageProofV1 memory proof = _publishSingleGatewayMessage(message);
            ServiceAcceptedClaimV1[] memory claims = new ServiceAcceptedClaimV1[](1);
            claims[0] = ServiceAcceptedClaimV1(p.accepted, p.duties, bootstrap, proof);
            receiver.claimAcceptedBatch(claims);
        } else {
            vm.chainId(ROOT);
            sink.relayAccepted(p.accepted, p.duties, bootstrap, 1_000_000, 800, address(this));
            assertEq(hub.chainId(), CHILD);
            assertEq(hub.recipient(), address(receiver));
            _deliverRoot();
        }
    }

    function _publishSingleGatewayMessage(bytes memory message) private returns (ServiceMessageProofV1 memory proof) {
        (ServiceMessageProofV1[] memory proofs, bytes32 root) = _batchProofs(message, message, address(child.gate));
        uint256 batchNumber = nextGatewayRootBatch++;
        _publishGatewayRootAt(batchNumber, root);
        proofs[0].blockOrBatchNumber = batchNumber;
        return proofs[0];
    }

    function _deliverRoot() private {
        vm.chainId(CHILD);
        bytes memory payload = hub.payload();
        vm.prank(receiver.aliasedRootMessageSink());
        (bool success, bytes memory result) = address(receiver).call(payload);
        if (!success) assembly { revert(add(result, 32), mload(result)) }
    }

    function _batchProofs(bytes memory first, bytes memory second, address sender)
        private
        pure
        returns (ServiceMessageProofV1[] memory proofs, bytes32 root)
    {
        bytes32[2] memory leaves = [
            keccak256(
                abi.encodePacked(
                    uint8(0), true, uint16(0), address(0x8008), bytes32(uint256(uint160(sender))), keccak256(first)
                )
            ),
            keccak256(
                abi.encodePacked(
                    uint8(0), true, uint16(0), address(0x8008), bytes32(uint256(uint160(sender))), keccak256(second)
                )
            )
        ];
        proofs = new ServiceMessageProofV1[](2);
        proofs[0] = ServiceMessageProofV1(42, 0, 0, new bytes32[](16));
        proofs[1] = ServiceMessageProofV1(42, 1, 0, new bytes32[](16));
        proofs[0].merkleProof[0] = bytes32(uint256(0x010f0001) << 224);
        proofs[1].merkleProof[0] = proofs[0].merkleProof[0];
        proofs[0].merkleProof[1] = leaves[1];
        proofs[1].merkleProof[1] = leaves[0];
        bytes32 node = keccak256(abi.encodePacked(leaves[0], leaves[1]));
        bytes32 empty = keccak256(new bytes(88));
        empty = keccak256(abi.encodePacked(empty, empty));
        for (uint256 level = 1; level < 14; ++level) {
            proofs[0].merkleProof[level + 1] = empty;
            proofs[1].merkleProof[level + 1] = empty;
            node = keccak256(abi.encodePacked(node, empty));
            empty = keccak256(abi.encodePacked(empty, empty));
        }
        bytes32 shared = keccak256("shared gateway root");
        proofs[0].merkleProof[15] = shared;
        proofs[1].merkleProof[15] = shared;
        root = keccak256(abi.encodePacked(node, shared));
    }

    function _publishGatewayRoot(bytes32 root) private {
        _publishGatewayRootAt(42, root);
    }

    function _publishGatewayRootAt(uint256 batchNumber, bytes32 root) private {
        GatewayRootMappingMock rootMapping = new GatewayRootMappingMock();
        rootMapping.setRoot(GATEWAY, batchNumber, root);
        vm.mockCall(address(hub), abi.encodeWithSignature("messageRoot()"), abi.encode(address(rootMapping)));
        vm.chainId(ROOT);
        sink.relayGatewayBatchRoot(batchNumber, 1_000_000, 800, address(this));
        assertEq(hub.payload(), abi.encodeCall(receiver.receiveGatewayBatchRoot, (batchNumber, root)));
        _deliverRoot();
    }

    function testRootPublicationAndLocalClaimsReplayAndAuthenticity() public {
        Package memory first = _bootstrap(child, 0, true);
        bytes memory firstMessage = GateMessenger(address(0x8008)).lastMessage();
        Package memory second = _bootstrap(child, 0, false);
        bytes memory secondMessage = GateMessenger(address(0x8008)).lastMessage();
        (ServiceMessageProofV1[] memory proofs, bytes32 root) =
            _batchProofs(firstMessage, secondMessage, address(child.gate));
        _publishGatewayRoot(root);
        assertEq(receiver.gatewayBatchRoots(42), root);
        ServiceAcceptedClaimV1[] memory claims = new ServiceAcceptedClaimV1[](2);
        claims[0] = ServiceAcceptedClaimV1(first.accepted, first.duties, true, proofs[0]);
        claims[1] = ServiceAcceptedClaimV1(second.accepted, second.duties, true, proofs[1]);
        claims[1].inclusion.merkleProof[4] = bytes32(uint256(42));
        vm.expectRevert(ZkSysAcceptedServiceReceiverV1.MessageNotIncluded.selector);
        receiver.claimAcceptedBatch(claims);
        assertEq(service.bootstrapSuccesses(vm.addr(ACCOUNT_KEY)), 0);
        claims[1].inclusion = proofs[1];
        claims[1].inclusion.merkleProof[4] = bytes32(uint256(42));
        // Memory assignment aliases dynamic arrays; reconstruct the valid proof after mutation.
        (proofs,) = _batchProofs(firstMessage, secondMessage, address(child.gate));
        claims[1].inclusion = proofs[1];
        receiver.claimAcceptedBatch(claims);
        assertEq(service.bootstrapSuccesses(vm.addr(ACCOUNT_KEY)), 1);
        receiver.claimAcceptedBatch(claims);
        assertEq(service.bootstrapSuccesses(vm.addr(ACCOUNT_KEY)), 1);
        vm.prank(receiver.aliasedRootMessageSink());
        receiver.receiveGatewayBatchRoot(42, root);
        vm.prank(receiver.aliasedRootMessageSink());
        vm.expectRevert(ZkSysAcceptedServiceReceiverV1.RootConflict.selector);
        receiver.receiveGatewayBatchRoot(42, bytes32(uint256(root) ^ 1));
        vm.expectRevert(ZkSysAcceptedServiceReceiverV1.MessageNotIncluded.selector);
        receiver.receiveGatewayBatchRoot(43, root);
    }

    function testBundledRootMessageAndMixedClaims() public {
        Package memory childReport = _bootstrap(child, 0, true);
        bytes memory message = GateMessenger(address(0x8008)).lastMessage();
        Package memory rootReport = _bootstrap(gateway, 1, true);
        (ServiceMessageProofV1[] memory proofs, bytes32 root) = _batchProofs(message, message, address(child.gate));
        GatewayRootMappingMock rootMapping = new GatewayRootMappingMock();
        rootMapping.setRoot(GATEWAY, 42, root);
        vm.mockCall(address(hub), abi.encodeWithSignature("messageRoot()"), abi.encode(address(rootMapping)));
        vm.chainId(ROOT);
        bytes32 hash =
            keccak256(abi.encode(sink.ACCEPTED_DOMAIN(), ZkSysServiceTypesV1.hashPackage(rootReport.accepted), true));
        vm.expectRevert(ZkSysRootServiceMessageSinkV1.MessageNotPublished.selector);
        sink.relayGatewayBatchRootWithMessage(42, bytes32(uint256(123)), 1_000_000, 800, address(this));
        sink.relayGatewayBatchRootWithMessage(42, hash, 1_000_000, 800, address(this));
        assertEq(hub.payload(), abi.encodeCall(receiver.receiveGatewayBatchRootWithMessage, (42, root, hash)));
        _deliverRoot();
        assertTrue(receiver.authorizedRootMessages(hash));
        ServiceAcceptedClaimV1[] memory children = new ServiceAcceptedClaimV1[](1);
        children[0] = ServiceAcceptedClaimV1(childReport.accepted, childReport.duties, true, proofs[0]);
        ServiceRootAcceptedClaimV1[] memory roots = new ServiceRootAcceptedClaimV1[](1);
        roots[0] = ServiceRootAcceptedClaimV1(rootReport.accepted, rootReport.duties, true);
        bytes32 originalManifestHash = roots[0].accepted.manifestHash;
        roots[0].accepted.manifestHash = bytes32(uint256(99));
        vm.expectRevert(ZkSysAcceptedServiceReceiverV1.MessageNotIncluded.selector);
        receiver.claimMixedBatch(children, roots);
        assertEq(service.bootstrapSuccesses(vm.addr(ACCOUNT_KEY)), 0);
        roots[0].accepted.manifestHash = originalManifestHash;
        receiver.claimMixedBatch(children, roots);
        assertEq(service.bootstrapSuccesses(vm.addr(ACCOUNT_KEY)), 2);
        receiver.claimMixedBatch(children, roots);
        assertEq(service.bootstrapSuccesses(vm.addr(ACCOUNT_KEY)), 2);
        vm.chainId(ROOT);
        sink.relayAccepted(rootReport.accepted, rootReport.duties, true, 1_000_000, 800, address(this));
        _deliverRoot();
        assertEq(service.bootstrapSuccesses(vm.addr(ACCOUNT_KEY)), 2);
        vm.chainId(ROOT);
        sink.relayGatewayBatchRootWithMessage(42, hash, 1_000_000, 800, address(this));
        _deliverRoot();
        assertTrue(receiver.authorizedRootMessages(hash));
    }

    function testRootClaimRejectsWrongGateBatchAndMalformedProof() public {
        Package memory first = _bootstrap(child, 0, true);
        bytes memory message = GateMessenger(address(0x8008)).lastMessage();
        (ServiceMessageProofV1[] memory proofs, bytes32 wrongRoot) = _batchProofs(message, message, address(0xBAD));
        _publishGatewayRoot(wrongRoot);
        ServiceAcceptedClaimV1[] memory claims = new ServiceAcceptedClaimV1[](1);
        claims[0] = ServiceAcceptedClaimV1(first.accepted, first.duties, true, proofs[0]);
        vm.expectRevert(ZkSysAcceptedServiceReceiverV1.MessageNotIncluded.selector);
        receiver.claimAcceptedBatch(claims);
        claims[0].inclusion.blockOrBatchNumber = 43;
        vm.expectRevert(ZkSysAcceptedServiceReceiverV1.MessageNotIncluded.selector);
        receiver.claimAcceptedBatch(claims);
        claims[0].inclusion.blockOrBatchNumber = 42;
        claims[0].inclusion.merkleProof[0] = bytes32(0);
        vm.expectRevert(ZkSysAcceptedServiceReceiverV1.MessageNotIncluded.selector);
        receiver.claimAcceptedBatch(claims);
        claims[0].inclusion.messageIndex = 16384;
        vm.expectRevert(ZkSysAcceptedServiceReceiverV1.MessageNotIncluded.selector);
        receiver.claimAcceptedBatch(claims);
    }

    function testRootPublicationRequiresCanonicalExecutedRoot() public {
        GatewayRootMappingMock rootMapping = new GatewayRootMappingMock();
        vm.mockCall(address(hub), abi.encodeWithSignature("messageRoot()"), abi.encode(address(rootMapping)));
        vm.chainId(ROOT);
        vm.expectRevert(ZkSysRootServiceMessageSinkV1.MessageNotPublished.selector);
        sink.relayGatewayBatchRoot(42, 1_000_000, 800, address(this));
        rootMapping.setRoot(GATEWAY, 42, keccak256("root"));
        hub.setChain(GATEWAY, address(0xBAD));
        vm.expectRevert(ZkSysRootServiceMessageSinkV1.MessageNotPublished.selector);
        sink.relayGatewayBatchRoot(42, 1_000_000, 800, address(this));
    }

    function testPinnedV32ProofVector() public pure {
        bytes memory message = abi.encode(keccak256("ZKSYS_ACCEPTED_SERVICE_V1"), bytes32(uint256(77)), false);
        bytes32 node = keccak256(
            abi.encodePacked(
                uint8(0),
                true,
                uint16(7),
                address(0x8008),
                bytes32(uint256(uint160(address(0x1234)))),
                keccak256(message)
            )
        );
        assertEq(node, 0xba949e7dc9476b647ffaaa960facdffa8a3df4a554f343c73e3cc10a6168c064);
        for (uint256 i; i < 14; ++i) {
            bytes32 sibling = bytes32(i + 1);
            node = ((42 >> i) & 1) == 0
                ? keccak256(abi.encodePacked(node, sibling))
                : keccak256(abi.encodePacked(sibling, node));
        }
        assertEq(node, 0xe03b7cbd0f8351f786dd5fb907b7097fa2b91bc6c298b74359217d5577cce447);
        assertEq(
            keccak256(abi.encodePacked(node, bytes32(uint256(0xdeadbeef)))),
            0xa29e576866c001919f3fc6568b17ac19f01a89c11b0640f81c26fe056da3500a
        );
    }

    function testInvalidDutyRollsBackEntireLocalBatch() public {
        Package memory first = _bootstrap(child, 0, true);
        bytes memory firstMessage = GateMessenger(address(0x8008)).lastMessage();
        Package memory second = _bootstrap(child, 0, true);
        bytes memory secondMessage = GateMessenger(address(0x8008)).lastMessage();
        (ServiceMessageProofV1[] memory proofs, bytes32 root) =
            _batchProofs(firstMessage, secondMessage, address(child.gate));
        _publishGatewayRoot(root);
        ServiceAcceptedClaimV1[] memory claims = new ServiceAcceptedClaimV1[](2);
        claims[0] = ServiceAcceptedClaimV1(first.accepted, first.duties, true, proofs[0]);
        claims[1] = ServiceAcceptedClaimV1(second.accepted, second.duties, true, proofs[1]);
        address account = vm.addr(ACCOUNT_KEY);
        vm.expectRevert(
            abi.encodeWithSelector(
                ZkSysProverServiceRegistryV1.DutySlotAlreadyUsed.selector, account, uint64(0), uint16(0)
            )
        );
        receiver.claimAcceptedBatch(claims);
        assertEq(service.bootstrapSuccesses(account), 0);
        assertFalse(service.acceptedPackages(ZkSysServiceTypesV1.hashPackage(first.accepted)));
        ServiceAcceptedClaimV1[] memory isolated = new ServiceAcceptedClaimV1[](1);
        isolated[0] = claims[0];
        receiver.claimAcceptedBatch(isolated);
        assertEq(service.bootstrapSuccesses(account), 1);
    }

    function _measuredClaim(uint256 index) private returns (ServiceAcceptedClaimV1 memory claim) {
        uint256 accountKey = 10_000 + index;
        uint256 operatorKey = 20_000 + index;
        address account = vm.addr(accountKey);
        address operator = vm.addr(operatorKey);
        membership.set(account, 1_000, 135_000 ether, 211_240, uint64(block.timestamp));
        ProverSubscriptionV1 memory sub =
            ProverSubscriptionV1(account, operator, address(0xBEEF), vm.addr(SEQUENCER_KEY), 0, 0, 0, 3);
        bytes32 digest = service.subscriptionDigest(sub);
        bytes32 subHash = service.subscribe(sub, _sign(accountKey, digest), _sign(operatorKey, digest));
        DutySuccessV1 memory duty = DutySuccessV1({
            account: account,
            subscriptionHash: subHash,
            batchNumber: uint64(1 + 2 * index),
            statementHash: keccak256(abi.encode("statement", index)),
            friProofHash: keccak256(abi.encode("fri", index)),
            transactionCount: 1,
            period: 0,
            slot: 0,
            attempt: 1,
            assignmentId: keccak256(abi.encode("assignment", index)),
            operatorSignature: bytes("")
        });
        duty.operatorSignature = _sign(operatorKey, service.dutyDigest(duty));
        claim.duties = new DutySuccessV1[](1);
        claim.duties[0] = duty;
        claim.accepted = AcceptedPackageV1({
            domainVersion: 1,
            policyHash: POLICY,
            chainId: CHILD,
            chainAddress: address(child.settlement),
            parent: keccak256(abi.encode("parent", index)),
            batchFrom: duty.batchNumber,
            batchTo: duty.batchNumber + 1,
            protocolVersion: 32,
            vkHash: child.verifier.vkHash(),
            period: 0,
            rosterRoot: bytes32(0),
            turn: 0,
            manifestHash: keccak256(abi.encode("manifest", index)),
            reportHash: ZkSysServiceTypesV1.hashReport(claim.duties),
            proofHash: keccak256(abi.encode("proof", index)),
            sequencer: vm.addr(SEQUENCER_KEY),
            sequencerBeneficiary: address(0x5E9),
            wrapper: address(0),
            wrapperBeneficiary: address(0)
        });
        claim.bootstrap = true;
        claim.inclusion = ServiceMessageProofV1(42, index, uint16(index), new bytes32[](16));
        claim.inclusion.merkleProof[0] = bytes32(uint256(0x010f0001) << 224);
    }

    function _measuredRoot(ServiceAcceptedClaimV1[] memory claims) private view returns (bytes32) {
        bytes32[] memory nodes = new bytes32[](128);
        for (uint256 i; i < claims.length; ++i) {
            bytes memory message =
                abi.encode(receiver.ACCEPTED_DOMAIN(), ZkSysServiceTypesV1.hashPackage(claims[i].accepted), true);
            nodes[i] = keccak256(
                abi.encodePacked(
                    uint8(0),
                    true,
                    uint16(i),
                    address(0x8008),
                    bytes32(uint256(uint160(address(child.gate)))),
                    keccak256(message)
                )
            );
        }
        bytes32 empty = keccak256(new bytes(88));
        for (uint256 i = claims.length; i < 128; ++i) {
            nodes[i] = empty;
        }
        for (uint256 level; level < 7; ++level) {
            for (uint256 i; i < claims.length; ++i) {
                claims[i].inclusion.merkleProof[level + 1] = nodes[(i >> level) ^ 1];
            }
            for (uint256 i; i < (128 >> (level + 1)); ++i) {
                nodes[i] = keccak256(abi.encodePacked(nodes[2 * i], nodes[2 * i + 1]));
            }
            empty = keccak256(abi.encodePacked(empty, empty));
        }
        bytes32 node = nodes[0];
        for (uint256 level = 7; level < 14; ++level) {
            for (uint256 i; i < claims.length; ++i) {
                claims[i].inclusion.merkleProof[level + 1] = empty;
            }
            node = keccak256(abi.encodePacked(node, empty));
            empty = keccak256(abi.encodePacked(empty, empty));
        }
        bytes32 shared = keccak256("shared gateway root");
        for (uint256 i; i < claims.length; ++i) {
            claims[i].inclusion.merkleProof[15] = shared;
        }
        return keccak256(abi.encodePacked(node, shared));
    }

    function testMeasureHundredChildAndOneGatewayClaims() public {
        Package memory rootReport = _bootstrap(gateway, 1, true);
        vm.chainId(CHILD);
        ServiceAcceptedClaimV1[] memory claims = new ServiceAcceptedClaimV1[](100);
        for (uint256 i; i < claims.length; ++i) {
            claims[i] = _measuredClaim(i);
        }
        bytes32 root = _measuredRoot(claims);
        GatewayRootMappingMock rootMapping = new GatewayRootMappingMock();
        rootMapping.setRoot(GATEWAY, 42, root);
        vm.mockCall(address(hub), abi.encodeWithSignature("messageRoot()"), abi.encode(address(rootMapping)));
        bytes32 messageHash =
            keccak256(abi.encode(sink.ACCEPTED_DOMAIN(), ZkSysServiceTypesV1.hashPackage(rootReport.accepted), true));
        vm.chainId(ROOT);
        sink.relayGatewayBatchRootWithMessage(42, messageHash, 1_000_000, 800, address(this));
        uint256 rootPriorityBytes = hub.payload().length;
        _deliverRoot();
        ServiceRootAcceptedClaimV1[] memory roots = new ServiceRootAcceptedClaimV1[](1);
        roots[0] = ServiceRootAcceptedClaimV1(rootReport.accepted, rootReport.duties, true);
        uint256 claimCalldataBytes = abi.encodeCall(receiver.claimMixedBatch, (claims, roots)).length;
        uint256 beforeGas = gasleft();
        receiver.claimMixedBatch(claims, roots);
        uint256 gasUsed = beforeGas - gasleft();
        for (uint256 i; i < claims.length; ++i) {
            assertEq(service.bootstrapSuccesses(vm.addr(10_000 + i)), 1);
        }
        assertEq(service.bootstrapSuccesses(vm.addr(ACCOUNT_KEY)), 1);
        emit log_named_uint("100 child + 1 gateway local claims modeled EVM gas", gasUsed);
        emit log_named_uint("mixed local claims calldata bytes", claimCalldataBytes);
        emit log_named_uint("root and gateway hash priority calldata bytes", rootPriorityBytes);
    }

    function _qualify() private {
        _relayAccepted(child, _bootstrap(child, 0, true), true);
        assertEq(service.bootstrapSuccesses(vm.addr(ACCOUNT_KEY)), 1);
        _relayAccepted(gateway, _bootstrap(gateway, 1, true), true);
        assertEq(service.bootstrapSuccesses(vm.addr(ACCOUNT_KEY)), 2);
        assertEq(service.totalQualifiedBonusWeight(0), 35_000 ether);
        assertEq(service.totalAdmittedBonusWeight(0), 0);
    }

    function testSharedRootThenBatchRootClaimDoesNotDuplicateChildReceipt() public {
        Package memory p = _bootstrap(child, 0, true);
        bytes memory message = GateMessenger(address(0x8008)).lastMessage();
        vm.chainId(CHILD);
        ServiceMessageVerifierMockV1(address(0x10009)).expectMessage(GATEWAY, 1, 0, 0, address(child.gate), message);
        receiver.relayAccepted(p.accepted, p.duties, true, _inclusion());
        ServiceMessageProofV1 memory proof = _publishSingleGatewayMessage(message);
        ServiceAcceptedClaimV1[] memory claims = new ServiceAcceptedClaimV1[](1);
        claims[0] = ServiceAcceptedClaimV1(p.accepted, p.duties, true, proof);
        receiver.claimAcceptedBatch(claims);
        assertEq(service.bootstrapSuccesses(vm.addr(ACCOUNT_KEY)), 1);
    }

    function testLocalActivationRequiresBothLanesAndValidGatewayProof() public {
        _qualify();
        _prepareRostersAndCoordinators();
        vm.chainId(ROOT);
        gateway.gate.activateService();
        sink.relayActivation(ROOT_VK, rosterRoot, 1_000_000, 800, address(this));
        _deliverRoot();
        assertFalse(service.serviceActive());
        vm.chainId(GATEWAY);
        child.gate.activateService();
        bytes memory message = GateMessenger(address(0x8008)).lastMessage();
        (ServiceMessageProofV1[] memory proofs, bytes32 root) = _batchProofs(message, message, address(child.gate));
        _publishGatewayRoot(root);
        bytes32 childVk = child.verifier.vkHash();
        vm.expectRevert(ZkSysAcceptedServiceReceiverV1.MessageNotIncluded.selector);
        receiver.claimActivation(bytes32(uint256(1)), rosterRoot, proofs[0]);
        receiver.claimActivation(childVk, rosterRoot, proofs[0]);
        assertTrue(service.serviceActive());
        receiver.claimActivation(childVk, rosterRoot, proofs[0]);
    }

    function _prepareRostersAndCoordinators() private {
        vm.chainId(CHILD);
        vm.warp(START - 100);
        candidate = WrapperCandidateV1(0, vm.addr(ACCOUNT_KEY), vm.addr(OPERATOR_KEY), address(0xBEEF));
        WrapperCandidateV1[] memory page = new WrapperCandidateV1[](1);
        page[0] = candidate;
        rosterRoot = service.publishRosterPage(0, page);
        bytes memory message = GateMessenger(address(0x8008)).lastMessage();
        vm.chainId(GATEWAY);
        gatewayRoster = new ZkSysQualifiedRosterReceiverV1(
            _rosterConfig(address(service), IZkSysChildMailboxV1(address(childMailbox)))
        );
        childMailbox.expectMessage(0, 1, 0, 0, address(service), message);
        gatewayRoster.relayRoster(0, rosterRoot, 1, _inclusion());
        gatewayRoster.forwardRosterToRoot(0);
        bytes memory forwarded = GateMessenger(address(0x8008)).lastMessage();
        assertEq(forwarded, message);
        vm.chainId(ROOT);
        rootMailbox = new GatewayRosterMailboxMock(gateway.settlement);
        rootMailbox.expectMessage(address(gatewayRoster), forwarded, 4);
        rootRoster = new ZkSysQualifiedRosterReceiverV1(_rosterConfig(address(gatewayRoster), rootMailbox));
        vm.expectRevert(ZkSysQualifiedRosterReceiverV1.MessageNotIncluded.selector);
        rootRoster.relayRoster(0, rosterRoot, 1, _inclusion());
        // Native bootstrap remains available until the root coordinator is installed. It carries
        // the initial roster rather than waiting for a roster that needs its own proof to exist.
        _bootstrap(gateway, 0, false);
        rootRoster.relayRoster(0, rosterRoot, 1, _inclusion());
        assertEq(gateway.settlement.verified(), 4);
        vm.chainId(GATEWAY);
        child.coordinator =
            new ZkSysWrapperCoordinatorV1(_coordinatorConfig(child, new GateDrawSource(), gatewayRoster, false));
        child.gate.installCoordinator(child.coordinator);
        child.coordinator.prepareRosterDraw(0);
        child.coordinator.recordRosterDraw(0);
        vm.chainId(ROOT);
        address predictedCoordinator = vm.computeCreateAddress(address(this), vm.getNonce(address(this)) + 1);
        rootDraw = new ZkSysNativeRootDrawV1(predictedCoordinator, 3, 2);
        gateway.coordinator = new ZkSysWrapperCoordinatorV1(_coordinatorConfig(gateway, rootDraw, rootRoster, true));
        gateway.gate.installCoordinator(gateway.coordinator);
        bytes32 commitment = gateway.coordinator.prepareRosterDraw(0);
        vm.roll(105);
        vm.setBlockhash(103, keccak256("future-root-block"));
        rootDraw.captureDraw(commitment);
        gateway.coordinator.recordRosterDraw(0);
        candidateProof = new bytes32[](32);
        bytes32 zero;
        for (uint256 i; i < 32; ++i) {
            candidateProof[i] = zero;
            zero = keccak256(abi.encodePacked(zero, zero));
        }
    }

    function _rosterConfig(address sender, IZkSysChildMailboxV1 mailbox)
        private
        view
        returns (ZkSysQualifiedRosterReceiverV1.Configuration memory)
    {
        return ZkSysQualifiedRosterReceiverV1.Configuration({
            childChainId: CHILD,
            childRegistry: address(service),
            messageSender: sender,
            childMailbox: mailbox,
            policyHash: POLICY,
            startTime: START,
            periodSeconds: 1_000,
            firstServicePeriod: 0
        });
    }

    function _coordinatorConfig(
        Lane memory lane,
        IZkSysAuthenticatedRootSourceV1 source,
        ZkSysQualifiedRosterReceiverV1 roster,
        bool nativeDraw
    ) private view returns (ZkSysWrapperCoordinatorV1.Configuration memory) {
        return ZkSysWrapperCoordinatorV1.Configuration({
            acceptanceGate: address(lane.gate),
            rootSource: source,
            rosterSource: roster,
            sequencer: vm.addr(SEQUENCER_KEY),
            childChainId: lane.executionId,
            childChainAddress: address(lane.settlement),
            policyHash: POLICY,
            productionVkHash: lane.verifier.vkHash(),
            initialParent: lane.gate.lastAcceptedPackage(),
            firstBatch: uint64(lane.settlement.verified() + 1),
            firstServicePeriod: 0,
            turnSeconds: 30,
            nativeRootDraw: nativeDraw
        });
    }

    function _activate() private {
        vm.chainId(GATEWAY);
        child.gate.activateService();
        bytes memory message = GateMessenger(address(0x8008)).lastMessage();
        ServiceMessageProofV1 memory proof = _publishSingleGatewayMessage(message);
        receiver.claimActivation(child.verifier.vkHash(), rosterRoot, proof);
        assertFalse(service.serviceActive());
        assertEq(service.totalAdmittedBonusWeight(0), 0);
        vm.chainId(ROOT);
        gateway.gate.activateService();
        sink.relayActivation(ROOT_VK, rosterRoot, 1_000_000, 800, address(this));
        Package memory control = _openJointWithWork(gateway, 0, false);
        assertTrue(gateway.gate.transitionWork());
        _joint(gateway, control);
        assertFalse(
            sink.messages(
                keccak256(abi.encode(sink.ACCEPTED_DOMAIN(), ZkSysServiceTypesV1.hashPackage(control.accepted), false))
            )
        );
        _deliverRoot();
        assertTrue(service.serviceActive());
        assertEq(service.totalAdmittedBonusWeight(0), 35_000 ether);
    }

    function _openJoint(Lane memory lane, uint16 slot) private returns (Package memory p) {
        return _openJointWithWork(lane, slot, true);
    }

    function _openJointWithWork(Lane memory lane, uint16 slot, bool positive) private returns (Package memory p) {
        p = _package(lane, slot, positive);
        (uint64 period, bytes32 root,,) = lane.coordinator.openingRoster();
        p.accepted.period = period;
        p.accepted.rosterRoot = root;
        if (positive) {
            p.duties[0].period = period;
            vm.chainId(CHILD);
            p.duties[0].operatorSignature = _sign(OPERATOR_KEY, service.dutyDigest(p.duties[0]));
            vm.chainId(lane.settlementId);
            p.accepted.reportHash = ZkSysServiceTypesV1.hashReport(p.duties);
        }
        AcceptedPackageV1 memory proposed = abi.decode(abi.encode(p.accepted), (AcceptedPackageV1));
        proposed.proofHash = bytes32(0);
        vm.prank(vm.addr(SEQUENCER_KEY));
        lane.gate.openPackage(proposed);
        p.accepted.wrapper = candidate.operator;
        p.accepted.wrapperBeneficiary = candidate.beneficiary;
    }

    function _joint(Lane memory lane, Package memory p) private {
        vm.chainId(lane.settlementId);
        bytes32 digest = lane.coordinator.packageDigest(p.accepted);
        lane.gate
            .submit(
                p.accepted,
                p.duties,
                p.outputs,
                p.proofData,
                candidate,
                candidateProof,
                _sign(SEQUENCER_KEY, digest),
                _sign(OPERATOR_KEY, digest)
            );
    }

    function testBothNativeProofLanesShareOneQuotaAndOneBonusAdmission() public {
        _qualify();
        _prepareRostersAndCoordinators();
        _activate();
        vm.warp(START);
        Package memory cp = _openJoint(child, 0);
        _joint(child, cp);
        _relayAccepted(child, cp, false);
        assertEq(service.assessedSuccesses(vm.addr(ACCOUNT_KEY), 0), 1);
        Package memory gp = _openJoint(gateway, 1);
        _joint(gateway, gp);
        _relayAccepted(gateway, gp, false);
        assertEq(service.assessedSuccesses(vm.addr(ACCOUNT_KEY), 0), 2);
        assertEq(service.dutySlots(vm.addr(ACCOUNT_KEY), 0), 3);
        assertEq(service.totalQualifiedBonusWeight(1), 35_000 ether);
        assertEq(child.coordinator.packageOrdinal(), 1);
        assertEq(gateway.coordinator.packageOrdinal(), 2);
        _relayAccepted(gateway, gp, false);
        assertEq(service.assessedSuccesses(vm.addr(ACCOUNT_KEY), 0), 2);
        (address chainAddress, bytes32 vk) = service.supportedLane(GATEWAY);
        assertEq(chainAddress, address(gateway.settlement));
        assertEq(vk, ROOT_VK);
        (chainAddress, vk) = service.supportedLane(CHILD);
        assertEq(chainAddress, address(child.settlement));
        assertEq(vk, child.verifier.vkHash());
    }

    function testGatewayAcceptedMixedReportSurvivesDeliveryAfterOriginalDutyGrace() public {
        _qualify();
        _prepareRostersAndCoordinators();
        _activate();
        vm.chainId(CHILD);
        vm.warp(START + 6_000 - 200);
        service.renewWrapper(subscriptionHash, 6);
        vm.warp(service.rosterCutoff(6));
        WrapperCandidateV1[] memory page = new WrapperCandidateV1[](1);
        page[0] = candidate;
        rosterRoot = service.publishRosterPage(6, page);
        bytes memory message = GateMessenger(address(0x8008)).lastMessage();
        vm.chainId(GATEWAY);
        childMailbox.expectMessage(0, 1, 0, 0, address(service), message);
        gatewayRoster.relayRoster(6, rosterRoot, 1, _inclusion());
        child.coordinator.prepareRosterDraw(6);
        child.coordinator.recordRosterDraw(6);

        vm.warp(START + 6_000 + 50);
        assertFalse(service.isRoundFinalized(5));
        Package memory p = _package(child, 0, true);
        uint64 from = p.accepted.batchFrom;
        ZkSysProofGateV1.StoredBatch memory previous;
        previous.batchNumber = from - 1;
        previous.batchHash = keccak256(abi.encode(CHILD, from - 1));
        ZkSysProofGateV1.StoredBatch[] memory batches = new ZkSysProofGateV1.StoredBatch[](2);
        p.duties = new DutySuccessV1[](2);
        bytes32 priorHash = previous.batchHash;
        for (uint64 i; i < 2; ++i) {
            p.outputs[i].l2TxCount = 1;
            batches[i].batchNumber = from + i;
            batches[i].batchHash = keccak256(abi.encode(CHILD, uint256(from) + i));
            batches[i].priorityOperationsHash = p.outputs[i].priorityOperationsHash;
            batches[i].commitment = _outputHash(p.outputs[i]);
            p.duties[i] = DutySuccessV1({
                account: vm.addr(ACCOUNT_KEY),
                subscriptionHash: subscriptionHash,
                batchNumber: from + i,
                statementHash: keccak256(
                    abi.encodePacked(
                        priorHash, batches[i].batchHash, child.gate.chainConfigHash(), batches[i].commitment
                    )
                ),
                friProofHash: keccak256(abi.encode("fri", CHILD, from + i)),
                transactionCount: 1,
                period: 5 + i,
                slot: 0,
                attempt: 1,
                assignmentId: keccak256(abi.encode("assignment", CHILD, from + i)),
                operatorSignature: bytes("")
            });
            vm.chainId(CHILD);
            p.duties[i].operatorSignature = _sign(OPERATOR_KEY, service.dutyDigest(p.duties[i]));
            vm.chainId(GATEWAY);
            priorHash = batches[i].batchHash;
        }
        uint256[] memory proof = new uint256[](46);
        proof[0] = 0x802;
        proof[2] = 123;
        p.proofData = bytes.concat(hex"01", abi.encode(previous, batches, proof));
        p.accepted.period = 6;
        p.accepted.rosterRoot = rosterRoot;
        p.accepted.reportHash = ZkSysServiceTypesV1.hashReport(p.duties);
        p.accepted.proofHash = keccak256(p.proofData);
        child.settlement.configure(address(child.gate), p.accepted.proofHash);
        AcceptedPackageV1 memory proposed = abi.decode(abi.encode(p.accepted), (AcceptedPackageV1));
        proposed.proofHash = bytes32(0);
        vm.prank(vm.addr(SEQUENCER_KEY));
        child.gate.openPackage(proposed);
        p.accepted.wrapper = candidate.operator;
        p.accepted.wrapperBeneficiary = candidate.beneficiary;
        _joint(child, p);
        bytes32 originalHash = ZkSysServiceTypesV1.hashPackage(p.accepted);
        assertEq(child.gate.lastAcceptedPackage(), originalHash);
        bytes memory originalMessage = GateMessenger(address(0x8008)).lastMessage();
        assertEq(originalMessage, abi.encode(receiver.ACCEPTED_DOMAIN(), originalHash, false));

        vm.chainId(CHILD);
        vm.warp(START + 6_000 + service.receiptGraceSeconds());
        assertTrue(service.isRoundFinalized(5));
        assertFalse(service.isRoundFinalized(6));
        ServiceMessageVerifierMockV1(address(0x10009))
            .expectMessage(GATEWAY, 1, 0, 0, address(child.gate), originalMessage);
        receiver.relayAccepted(p.accepted, p.duties, false, _inclusion());
        receiver.relayAccepted(p.accepted, p.duties, false, _inclusion());
        assertTrue(service.acceptedPackages(originalHash));
        assertEq(p.accepted.reportHash, ZkSysServiceTypesV1.hashReport(p.duties));
        address account = vm.addr(ACCOUNT_KEY);
        assertEq(service.assessedSuccesses(account, 5), 0);
        assertEq(service.rewardedSuccesses(account, 5), 0);
        assertEq(service.dutySlots(account, 5), 0);
        assertEq(service.assessedSuccesses(account, 6), 1);
        assertEq(service.rewardedSuccesses(account, 6), 1);
        assertEq(service.dutySlots(account, 6), 1);
    }

    function testGatewayOnlyQualificationCannotSetOrOverwriteChildVk() public {
        _relayAccepted(gateway, _bootstrap(gateway, 0, true), true);
        _relayAccepted(gateway, _bootstrap(gateway, 1, true), true);
        assertEq(service.bootstrapSuccesses(vm.addr(ACCOUNT_KEY)), 2);
        assertEq(service.totalQualifiedBonusWeight(0), 35_000 ether);
        assertEq(service.bootstrapVkHash(), bytes32(0));
        _relayAccepted(child, _bootstrap(child, 0, false), true);
        assertEq(service.bootstrapVkHash(), child.verifier.vkHash());
        assertEq(service.bootstrapSuccesses(vm.addr(ACCOUNT_KEY)), 2);
        assertEq(service.totalQualifiedBonusWeight(0), 35_000 ether);
    }

    function testPrestartControlCannotGainCreditByRepairOrCrossingPeriodStart() public {
        _qualify();
        _prepareRostersAndCoordinators();
        _activate();
        Package memory nonempty = _package(gateway, 0, true);
        AcceptedPackageV1 memory proposed = abi.decode(abi.encode(nonempty.accepted), (AcceptedPackageV1));
        proposed.rosterRoot = rosterRoot;
        proposed.proofHash = bytes32(0);
        vm.prank(vm.addr(SEQUENCER_KEY));
        vm.expectRevert(ZkSysProofGateV1.InvalidPackage.selector);
        gateway.gate.openPackage(proposed);

        Package memory control = _openJointWithWork(gateway, 0, false);
        uint64 startedAt = gateway.coordinator.turnsStartedAt();
        vm.warp(START + 1);
        proposed = abi.decode(abi.encode(control.accepted), (AcceptedPackageV1));
        proposed.wrapper = address(0);
        proposed.wrapperBeneficiary = address(0);
        proposed.proofHash = bytes32(0);
        proposed.reportHash = nonempty.accepted.reportHash;
        vm.prank(vm.addr(SEQUENCER_KEY));
        vm.expectRevert(ZkSysProofGateV1.InvalidPackage.selector);
        gateway.gate.repairPackage(proposed);
        assertEq(gateway.coordinator.turnsStartedAt(), startedAt);
        vm.expectRevert(ZkSysProofGateV1.InvalidDutyStatement.selector);
        gateway.gate
            .submit(
                control.accepted,
                nonempty.duties,
                control.outputs,
                control.proofData,
                candidate,
                candidateProof,
                bytes(""),
                bytes("")
            );

        control.accepted.turn = gateway.coordinator.currentTurn();
        _joint(gateway, control);
        bytes32 messageHash =
            keccak256(abi.encode(sink.ACCEPTED_DOMAIN(), ZkSysServiceTypesV1.hashPackage(control.accepted), false));
        assertFalse(sink.messages(messageHash));
        assertEq(service.assessedSuccesses(vm.addr(ACCOUNT_KEY), 0), 0);
        Package memory paid = _openJoint(gateway, 0);
        assertFalse(gateway.gate.transitionWork());
        _joint(gateway, paid);
        _relayAccepted(gateway, paid, false);
        assertEq(service.assessedSuccesses(vm.addr(ACCOUNT_KEY), 0), 1);
    }

    function testRolloverControlProofTransportsMissingRosterThenCurrentRosterResumesCredit() public {
        _qualify();
        _prepareRostersAndCoordinators();
        _activate();
        vm.chainId(CHILD);
        vm.warp(START);
        service.renewWrapper(subscriptionHash, 1);
        vm.warp(START + 900);
        WrapperCandidateV1[] memory page = new WrapperCandidateV1[](1);
        page[0] = candidate;
        bytes32 nextRoot = service.publishRosterPage(1, page);
        bytes memory message = GateMessenger(address(0x8008)).lastMessage();
        vm.chainId(GATEWAY);
        childMailbox.expectMessage(0, 1, 0, 0, address(service), message);
        gatewayRoster.relayRoster(1, nextRoot, 1, _inclusion());
        gatewayRoster.forwardRosterToRoot(1);
        message = GateMessenger(address(0x8008)).lastMessage();
        vm.chainId(ROOT);
        vm.warp(START + 1_000);
        rootMailbox.expectMessage(address(gatewayRoster), message, gateway.settlement.verified() + 2);
        vm.expectRevert(ZkSysQualifiedRosterReceiverV1.MessageNotIncluded.selector);
        rootRoster.relayRoster(1, nextRoot, 1, _inclusion());

        Package memory recovery = _openJointWithWork(gateway, 0, false);
        assertEq(recovery.accepted.period, 0);
        assertTrue(gateway.gate.transitionWork());
        _joint(gateway, recovery);
        bytes32 messageHash =
            keccak256(abi.encode(sink.ACCEPTED_DOMAIN(), ZkSysServiceTypesV1.hashPackage(recovery.accepted), false));
        assertFalse(sink.messages(messageHash));
        rootRoster.relayRoster(1, nextRoot, 1, _inclusion());
        bytes32 commitment = gateway.coordinator.prepareRosterDraw(1);
        vm.roll(110);
        vm.setBlockhash(108, keccak256("next-future-root-block"));
        rootDraw.captureDraw(commitment);
        gateway.coordinator.recordRosterDraw(1);
        (uint64 period, bytes32 actual,, bool control) = gateway.coordinator.openingRoster();
        assertEq(period, 1);
        assertEq(actual, nextRoot);
        assertFalse(control);

        Package memory paid = _openJoint(gateway, 0);
        assertEq(paid.accepted.period, 1);
        assertFalse(gateway.gate.transitionWork());
        _joint(gateway, paid);
        _relayAccepted(gateway, paid, false);
        assertEq(service.assessedSuccesses(vm.addr(ACCOUNT_KEY), 1), 1);
        assertEq(service.totalAdmittedBonusWeight(1), 0);
    }

    function testRootNativeFailureCannotPublishAndRequiresBothEndorsements() public {
        _qualify();
        _prepareRostersAndCoordinators();
        _activate();
        vm.warp(START);
        Package memory p = _openJoint(gateway, 0);
        bytes32 digest = gateway.coordinator.packageDigest(p.accepted);
        bytes memory seq = _sign(SEQUENCER_KEY, digest);
        bytes memory wrong = _sign(99, digest);
        vm.expectRevert(ZkSysWrapperCoordinatorV1.InvalidSignature.selector);
        gateway.gate.submit(p.accepted, p.duties, p.outputs, p.proofData, candidate, candidateProof, seq, wrong);
        gateway.settlement.setRefuseProof(true);
        bytes memory wrapperSig = _sign(OPERATOR_KEY, digest);
        vm.expectRevert("native proof invalid");
        gateway.gate.submit(p.accepted, p.duties, p.outputs, p.proofData, candidate, candidateProof, seq, wrapperSig);
        bytes32 messageHash =
            keccak256(abi.encode(sink.ACCEPTED_DOMAIN(), ZkSysServiceTypesV1.hashPackage(p.accepted), false));
        assertFalse(sink.messages(messageHash));
        assertTrue(gateway.coordinator.packageOpen());
        gateway.settlement.setRefuseProof(false);
        _joint(gateway, p);
        assertTrue(sink.messages(messageHash));
    }

    function testSameSlotCannotBeCreditedAgainOnOtherChain() public {
        _qualify();
        _prepareRostersAndCoordinators();
        _activate();
        vm.warp(START);
        Package memory cp = _openJoint(child, 0);
        _joint(child, cp);
        _relayAccepted(child, cp, false);
        Package memory gp = _openJoint(gateway, 0);
        _joint(gateway, gp);
        sink.relayAccepted(gp.accepted, gp.duties, false, 1_000_000, 800, address(this));
        vm.chainId(CHILD);
        address aliasAddress = receiver.aliasedRootMessageSink();
        vm.prank(aliasAddress);
        vm.expectRevert(
            abi.encodeWithSelector(
                ZkSysProverServiceRegistryV1.DutySlotAlreadyUsed.selector, vm.addr(ACCOUNT_KEY), uint64(0), uint16(0)
            )
        );
        receiver.receiveRootAccepted(gp.accepted, gp.duties, false);
        assertEq(service.assessedSuccesses(vm.addr(ACCOUNT_KEY), 0), 1);
    }

    function testWrongRootPublisherAliasLaneAndReportCannotManufactureCredit() public {
        vm.chainId(ROOT);
        bytes memory bogus = abi.encode(sink.ACTIVATION_DOMAIN(), ROOT_VK, keccak256("root"));
        vm.expectRevert(ZkSysRootServiceMessageSinkV1.UnauthorizedPublisher.selector);
        sink.publish(bogus);
        Package memory p = _bootstrap(gateway, 0, true);
        vm.chainId(CHILD);
        vm.expectRevert(ZkSysAcceptedServiceReceiverV1.MessageNotIncluded.selector);
        receiver.receiveRootAccepted(p.accepted, p.duties, true);
        vm.prank(receiver.aliasedRootMessageSink());
        AcceptedPackageV1 memory wrong = abi.decode(abi.encode(p.accepted), (AcceptedPackageV1));
        wrong.chainId = CHILD;
        vm.expectRevert(ZkSysAcceptedServiceReceiverV1.WrongRegistry.selector);
        receiver.receiveRootAccepted(wrong, p.duties, true);
        vm.chainId(ROOT);
        p.duties[0].transactionCount = 9;
        vm.expectRevert(ZkSysRootServiceMessageSinkV1.MessageNotPublished.selector);
        sink.relayAccepted(p.accepted, p.duties, true, 1_000_000, 800, address(this));
    }

    function testSharedPoolRejectsUnrelatedSequencerSubscription() public {
        vm.chainId(CHILD);
        ProverSubscriptionV1 memory sub = ProverSubscriptionV1(
            vm.addr(ACCOUNT_KEY), vm.addr(OPERATOR_KEY), address(0xBEEF), address(0xBAD), 1, 10, 1, 3
        );
        bytes memory signature = _sign(ACCOUNT_KEY, service.subscriptionDigest(sub));
        bytes memory operatorSignature = _sign(OPERATOR_KEY, service.subscriptionDigest(sub));
        vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidSubscription.selector);
        service.subscribe(sub, signature, operatorSignature);
    }

    function testNativeRootEntropyIsFutureImmutableAndCannotBeRequestedByCaller() public {
        _qualify();
        _prepareRostersAndCoordinators();
        vm.chainId(ROOT);
        bytes32 commitment = gateway.coordinator.rosterDrawCommitment(0);
        (uint64 height, bytes32 hash) = rootDraw.drawFor(commitment);
        assertEq(height, 103);
        assertEq(hash, keccak256("future-root-block"));
        vm.expectRevert(ZkSysNativeRootDrawV1.UnauthorizedCoordinator.selector);
        rootDraw.requestDraw(keccak256("caller-reroll"));
        vm.roll(999);
        vm.expectRevert(ZkSysNativeRootDrawV1.DrawStillAvailable.selector);
        rootDraw.retryExpiredDraw(commitment);
        gateway.coordinator.recordRosterDraw(0);
        (, bytes32 unchanged) = rootDraw.drawFor(commitment);
        assertEq(unchanged, hash);
    }

    function testNativeRootDrawRetriesOnlyAfterUncapturedBlockExpires() public {
        vm.chainId(ROOT);
        ZkSysNativeRootDrawV1 draw = new ZkSysNativeRootDrawV1(address(this), 3, 2);
        bytes32 commitment = keccak256("uncaptured-roster");
        draw.requestDraw(commitment);
        vm.roll(104);
        vm.setBlockhash(103, keccak256("initial-hash"));
        vm.expectRevert(ZkSysNativeRootDrawV1.DrawNotReady.selector);
        draw.captureDraw(commitment);
        vm.roll(359);
        vm.expectRevert(ZkSysNativeRootDrawV1.DrawStillAvailable.selector);
        draw.retryExpiredDraw(commitment);
        vm.roll(360);
        draw.retryExpiredDraw(commitment);
        (uint64 height, bytes32 hash) = draw.drawFor(commitment);
        assertEq(height, 363);
        assertEq(hash, bytes32(0));
        vm.roll(365);
        vm.setBlockhash(363, keccak256("recovered-hash"));
        draw.captureDraw(commitment);
        (, hash) = draw.drawFor(commitment);
        assertEq(hash, keccak256("recovered-hash"));
    }
}
