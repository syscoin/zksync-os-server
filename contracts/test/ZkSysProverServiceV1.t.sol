// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";
import {ECDSA} from "@openzeppelin/contracts-v4/utils/cryptography/ECDSA.sol";
import {ZkSysMembershipRegistry} from "../src/zksys/ZkSysMembershipRegistry.sol";
import {ZkSysProverServiceRegistryV1} from "../src/zksys/ZkSysProverServiceRegistryV1.sol";
import {ZkSysWrapperCoordinatorV1} from "../src/zksys/ZkSysWrapperCoordinatorV1.sol";
import {
    ZkSysAcceptedServiceReceiverV1,
    ZkSysQualifiedRosterReceiverV1,
    IZkSysChildMailboxV1,
    ZkSysL2MessageV1,
    ServiceMessageProofV1
} from "../src/zksys/ZkSysServiceMessageReceiversV1.sol";
import {
    AcceptedPackageV1,
    DutySuccessV1,
    ProverSubscriptionV1,
    WrapperCandidateV1,
    IZkSysAuthenticatedRootSourceV1,
    IZkSysWrapperRosterSourceV1,
    ZkSysServiceTypesV1
} from "../src/zksys/ZkSysServiceTypesV1.sol";

contract ServiceClockMockV1 {
    uint256 public startTime = 10_000;
    uint256 public periodSeconds = 1_000;
}

contract ServiceContractSignerMockV1 {
    address private immutable _signer;

    constructor(address signer) {
        _signer = signer;
    }

    function isValidSignature(bytes32 digest, bytes calldata signature) external view returns (bytes4) {
        (address recovered, ECDSA.RecoverError error) = ECDSA.tryRecover(digest, signature);
        return error == ECDSA.RecoverError.NoError && recovered == _signer ? bytes4(0x1626ba7e) : bytes4(0xffffffff);
    }
}

contract ServiceMembershipMockV1 {
    mapping(address => ZkSysMembershipRegistry.Member) private _members;
    mapping(address => uint64) private _heights;
    mapping(address => uint64) private _times;

    function set(address account, uint32 collateral, uint128 weight, uint64 height, uint64 observedAt) external {
        _members[account] = ZkSysMembershipRegistry.Member(collateral, weight);
        _heights[account] = height;
        _times[account] = observedAt;
    }

    function member(address account) external view returns (ZkSysMembershipRegistry.Member memory) {
        return _members[account];
    }

    function membershipObservation(address account) external view returns (uint64, uint64) {
        return (_heights[account], _times[account]);
    }
}

contract ServiceMessengerMockV1 {
    bytes public lastMessage;
    uint256 public messages;

    function sendToL1(bytes calldata message) external returns (bytes32) {
        lastMessage = message;
        ++messages;
        return keccak256(message);
    }
}

// This unit-test caller represents the separately tested production proof/inclusion boundary.
contract AcceptedServiceSourceMockV1 {
    function bootstrap(
        ZkSysProverServiceRegistryV1 service,
        AcceptedPackageV1 calldata accepted,
        DutySuccessV1[] calldata duties
    ) external {
        service.acceptBootstrapDuties(accepted, duties);
    }

    function accept(
        ZkSysProverServiceRegistryV1 service,
        AcceptedPackageV1 calldata accepted,
        DutySuccessV1[] calldata duties
    ) external {
        service.acceptDuties(accepted, duties);
    }

    function activate(ZkSysProverServiceRegistryV1 service, bytes32 vk, bytes32 root) external {
        service.activateService(vk, root);
    }
}

contract ServiceRootMockV1 is IZkSysAuthenticatedRootSourceV1 {
    mapping(bytes32 => uint64) public heights;
    mapping(bytes32 => bytes32) public hashes;

    function set(bytes32 commitment, uint64 height, bytes32 hash) external {
        heights[commitment] = height;
        hashes[commitment] = hash;
    }

    function drawFor(bytes32 commitment) external view returns (uint64, bytes32) {
        return (heights[commitment], hashes[commitment]);
    }
}

contract ServiceRosterMockV1 is IZkSysWrapperRosterSourceV1 {
    uint256 public constant startTime = 1_000;
    uint256 public constant periodSeconds = 1_000;
    uint64 public constant firstServicePeriod = 0;
    uint64 public period;
    bytes32 public root;
    uint32 public count;
    mapping(uint64 => bytes32) private _roots;
    mapping(uint64 => uint32) private _counts;

    function set(uint64 period_, bytes32 root_, uint32 count_) external {
        period = period_;
        root = root_;
        count = count_;
        _roots[period_] = root_;
        _counts[period_] = count_;
    }

    function rosterFor(uint64 period_) external view returns (bytes32, uint32) {
        return (_roots[period_], _counts[period_]);
    }

    function currentRoster() external view returns (uint64, bytes32, uint32) {
        return (period, root, count);
    }
}

contract ServiceGateMockV1 {
    function open(ZkSysWrapperCoordinatorV1 coordinator, AcceptedPackageV1 calldata accepted) external {
        coordinator.openPackage(accepted);
    }

    function repair(ZkSysWrapperCoordinatorV1 coordinator, AcceptedPackageV1 calldata accepted) external {
        coordinator.repairPackage(accepted);
    }

    function accept(
        ZkSysWrapperCoordinatorV1 coordinator,
        AcceptedPackageV1 calldata accepted,
        WrapperCandidateV1 calldata candidate,
        bytes32[] calldata proof,
        bytes calldata sequencerSignature,
        bytes calldata wrapperSignature
    ) external returns (bytes32) {
        return coordinator.acceptPackage(accepted, candidate, proof, sequencerSignature, wrapperSignature);
    }
}

contract ServiceMessageVerifierMockV1 {
    bytes32 public expected;
    bool public enabled;

    function expectMessage(
        uint256 chain,
        uint256 blockNumber,
        uint256 index,
        uint16 txNumber,
        address sender,
        bytes calldata data
    ) external {
        expected = keccak256(abi.encode(chain, blockNumber, index, txNumber, sender, data));
        enabled = true;
    }

    function proveL2MessageInclusionShared(
        uint256 chain,
        uint256 blockNumber,
        uint256 index,
        ZkSysL2MessageV1 calldata message,
        bytes32[] calldata
    ) external view returns (bool) {
        return enabled
            && expected
                == keccak256(
                abi.encode(chain, blockNumber, index, message.txNumberInBatch, message.sender, message.data)
            );
    }

    function proveL2MessageInclusion(
        uint256 blockNumber,
        uint256 index,
        ZkSysL2MessageV1 calldata message,
        bytes32[] calldata
    ) external view returns (bool) {
        return enabled
            && expected
                == keccak256(
                abi.encode(uint256(0), blockNumber, index, message.txNumberInBatch, message.sender, message.data)
            );
    }
}

abstract contract ServiceTestBaseV1 is Test {
    bytes32 internal constant POLICY = keccak256("reviewed-service-policy-v1");
    bytes32 internal constant VK = keccak256("production-v8-key");
    address internal constant CHAIN = address(0x57057);
    uint256 internal constant SEQUENCER_KEY = 99;
    uint256 internal constant ALICE_KEY = 1;
    uint256 internal constant ALICE_OPERATOR_KEY = 11;
    uint256 internal constant BOB_KEY = 2;
    uint256 internal constant BOB_OPERATOR_KEY = 22;

    function _sign(uint256 key, bytes32 digest) internal pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, digest);
        return abi.encodePacked(r, s, v);
    }

    function _pair(bytes32 left, bytes32 right) internal pure returns (bytes32) {
        return left < right ? keccak256(abi.encodePacked(left, right)) : keccak256(abi.encodePacked(right, left));
    }

    function _tree(WrapperCandidateV1[] memory candidates, uint256 selected)
        internal
        pure
        returns (bytes32 root, bytes32[] memory proof)
    {
        uint256 width = 1;
        while (width < candidates.length) width *= 2;
        bytes32[] memory nodes = new bytes32[](width);
        for (uint256 i; i < candidates.length; ++i) {
            nodes[i] = ZkSysServiceTypesV1.wrapperLeaf(candidates[i]);
        }
        proof = new bytes32[](32);
        uint256 level;
        bytes32 zero;
        while (width > 1) {
            proof[level++] = nodes[selected ^ 1];
            for (uint256 i; i < width; i += 2) {
                nodes[i / 2] = _pair(nodes[i], nodes[i + 1]);
            }
            selected >>= 1;
            width >>= 1;
            zero = _pair(zero, zero);
        }
        root = nodes[0];
        while (level < 32) {
            proof[level++] = zero;
            root = _pair(root, zero);
            zero = _pair(zero, zero);
        }
    }

    function _basePackage() internal view returns (AcceptedPackageV1 memory accepted) {
        accepted.domainVersion = 1;
        accepted.policyHash = POLICY;
        accepted.chainId = block.chainid;
        accepted.chainAddress = CHAIN;
        accepted.parent = keccak256("genesis-parent");
        accepted.batchFrom = 1;
        accepted.batchTo = 2;
        accepted.protocolVersion = 32;
        accepted.vkHash = VK;
        accepted.manifestHash = keccak256("manifest");
        accepted.reportHash = keccak256("report");
        accepted.sequencer = vm.addr(SEQUENCER_KEY);
        accepted.sequencerBeneficiary = address(0x5E9);
    }

    function _installMessenger() internal {
        ServiceMessengerMockV1 messenger = new ServiceMessengerMockV1();
        vm.etch(address(0x8008), address(messenger).code);
    }
}

contract ZkSysProverServiceRegistryV1Test is ServiceTestBaseV1 {
    ServiceMembershipMockV1 internal membership;
    ServiceClockMockV1 internal issuer;
    AcceptedServiceSourceMockV1 internal source;
    ZkSysProverServiceRegistryV1 internal service;

    function setUp() public {
        vm.warp(1_000);
        _installMessenger();
        membership = new ServiceMembershipMockV1();
        issuer = new ServiceClockMockV1();
        source = new AcceptedServiceSourceMockV1();
        service = new ZkSysProverServiceRegistryV1(_config(address(source)));
        _senior(ALICE_KEY, false);
        _senior(BOB_KEY, true);
    }

    function _config(address acceptanceSource)
        internal
        view
        returns (ZkSysProverServiceRegistryV1.Configuration memory)
    {
        return ZkSysProverServiceRegistryV1.Configuration({
            membershipRegistry: ZkSysMembershipRegistry(address(membership)),
            issuer: address(issuer),
            acceptanceSource: acceptanceSource,
            settlementChainAddress: CHAIN,
            policyHash: POLICY,
            firstServicePeriod: 0,
            receiptGraceSeconds: 100,
            membershipMaxAgeSeconds: 100_000,
            rosterPublicationLeadSeconds: 100,
            dutiesPerRound: 2,
            gatewayChainId: 0,
            gatewayChainAddress: address(0),
            gatewayVkHash: bytes32(0),
            sharedSequencer: vm.addr(SEQUENCER_KEY)
        });
    }

    function _configureLane(uint256 lane) internal returns (uint256 chainId, address chainAddress) {
        vm.warp(1_000);
        ZkSysProverServiceRegistryV1.Configuration memory config = _config(address(source));
        if (lane != 0) {
            config.gatewayChainId = block.chainid + 1;
            config.gatewayChainAddress = address(0x600D);
            config.gatewayVkHash = VK;
        }
        service = new ZkSysProverServiceRegistryV1(config);
        return lane == 2
            ? (config.gatewayChainId, config.gatewayChainAddress)
            : (block.chainid, config.settlementChainAddress);
    }

    function _senior(uint256 accountKey, bool full) internal {
        membership.set(
            vm.addr(accountKey),
            1_000,
            full ? 200_000 ether : 135_000 ether,
            full ? 526_600 : 211_240,
            uint64(block.timestamp)
        );
    }

    function _subscription(uint256 accountKey, uint256 operatorKey)
        internal
        pure
        returns (ProverSubscriptionV1 memory sub)
    {
        sub = ProverSubscriptionV1({
            account: vm.addr(accountKey),
            operator: vm.addr(operatorKey),
            beneficiary: address(uint160(accountKey + 500)),
            sequencer: vm.addr(SEQUENCER_KEY),
            firstPeriod: 0,
            lastPeriod: 10,
            nonce: 0,
            services: 3
        });
    }

    function _subscribe(uint256 accountKey, uint256 operatorKey) internal returns (bytes32) {
        ProverSubscriptionV1 memory sub = _subscription(accountKey, operatorKey);
        bytes32 digest = service.subscriptionDigest(sub);
        return service.subscribe(sub, _sign(accountKey, digest), _sign(operatorKey, digest));
    }

    function _duty(uint256 accountKey, uint256 operatorKey, bytes32 sub, uint64 batch, uint64 period, uint16 slot)
        internal
        view
        returns (DutySuccessV1 memory duty)
    {
        duty.account = vm.addr(accountKey);
        duty.subscriptionHash = sub;
        duty.batchNumber = batch;
        duty.statementHash = keccak256(abi.encode("statement", batch));
        duty.friProofHash = keccak256(abi.encode("fri", batch));
        duty.transactionCount = 1;
        duty.period = period;
        duty.slot = slot;
        duty.attempt = 1;
        duty.assignmentId = keccak256(abi.encode("assignment", batch));
        duty.operatorSignature = _sign(operatorKey, service.dutyDigest(duty));
    }

    function _package(DutySuccessV1[] memory duties, bool bootstrap, uint64 from, uint64 to, uint64 period)
        internal
        view
        returns (AcceptedPackageV1 memory accepted)
    {
        accepted = _basePackage();
        accepted.batchFrom = from;
        accepted.batchTo = to;
        accepted.period = period;
        accepted.reportHash = ZkSysServiceTypesV1.hashReport(duties);
        accepted.proofHash = keccak256(abi.encode("snark", from, to));
        if (!bootstrap) {
            accepted.wrapper = vm.addr(BOB_OPERATOR_KEY);
            accepted.wrapperBeneficiary = address(0xBEEF);
            accepted.rosterRoot = service.initialQualifiedRosterRoot();
        }
    }

    function _qualify(uint256 key, uint256 operatorKey, uint64 from) internal returns (bytes32 sub) {
        sub = _subscribe(key, operatorKey);
        DutySuccessV1[] memory duties = new DutySuccessV1[](2);
        duties[0] = _duty(key, operatorKey, sub, from, 0, 0);
        duties[1] = _duty(key, operatorKey, sub, from + 1, 0, 1);
        source.bootstrap(service, _package(duties, true, from, from + 1, 0), duties);
    }

    function _candidates(bool bob) internal view returns (WrapperCandidateV1[] memory candidates) {
        candidates = new WrapperCandidateV1[](bob ? 2 : 1);
        candidates[0] = service.qualifiedWrapper(vm.addr(ALICE_KEY), 0);
        if (bob) {
            candidates[1] = service.qualifiedWrapper(vm.addr(BOB_KEY), 0);
            if (candidates[0].account > candidates[1].account) {
                (candidates[0], candidates[1]) = (candidates[1], candidates[0]);
            }
            candidates[1].index = 1;
        }
    }

    function _activate(bool bob) internal returns (bytes32 root) {
        vm.warp(service.rosterCutoff(0));
        root = service.publishRosterPage(0, _candidates(bob));
        source.activate(service, VK, root);
        vm.warp(issuer.startTime());
    }

    function testPendingEnrollmentNeverDilutesAndFullQuotaRequired() public {
        bytes32 sub = _subscribe(ALICE_KEY, ALICE_OPERATOR_KEY);
        assertEq(service.totalQualifiedBonusWeight(0), 0);
        assertEq(service.totalAdmittedBonusWeight(0), 0);
        DutySuccessV1[] memory duties = new DutySuccessV1[](1);
        duties[0] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, sub, 1, 0, 0);
        source.bootstrap(service, _package(duties, true, 1, 2, 0), duties);
        assertEq(service.totalQualifiedBonusWeight(0), 0);
        duties[0] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, sub, 2, 0, 1);
        source.bootstrap(service, _package(duties, true, 1, 2, 0), duties);
        assertEq(service.qualifiedBonusWeight(vm.addr(ALICE_KEY), 0), 35_000 ether);
        assertEq(service.totalAdmittedBonusWeight(0), 0);
        _activate(false);
        assertEq(service.totalAdmittedBonusWeight(0), 35_000 ether);
    }

    function testChildOnlyConfigurationRejectsZeroSequencer() public {
        ZkSysProverServiceRegistryV1.Configuration memory config = _config(address(source));
        config.sharedSequencer = address(0);
        vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidConfiguration.selector);
        new ZkSysProverServiceRegistryV1(config);
    }

    function testDualLaneConfigurationRejectsZeroSequencer() public {
        ZkSysProverServiceRegistryV1.Configuration memory config = _config(address(source));
        config.gatewayChainId = block.chainid + 1;
        config.gatewayChainAddress = address(0x600D);
        config.gatewayVkHash = VK;
        config.sharedSequencer = address(0);
        vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidConfiguration.selector);
        new ZkSysProverServiceRegistryV1(config);
    }

    function testAlternateSequencerSubscriptionCannotReserveNonceOperatorOrRoster() public {
        for (uint256 lane; lane < 2; ++lane) {
            _configureLane(lane);
            ProverSubscriptionV1 memory sub = _subscription(ALICE_KEY, ALICE_OPERATOR_KEY);
            sub.sequencer = address(0xBAD);
            bytes32 hash = ZkSysServiceTypesV1.hashSubscription(sub);
            bytes memory signature = _sign(ALICE_KEY, service.subscriptionDigest(sub));
            bytes memory operatorSignature = _sign(ALICE_OPERATOR_KEY, service.subscriptionDigest(sub));
            vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidSubscription.selector);
            service.subscribe(sub, signature, operatorSignature);
            assertEq(service.nonces(sub.account), 0);
            assertEq(service.subscription(hash).account, address(0));
            assertEq(service.subscriptionAt(sub.account, sub.sequencer, 0), bytes32(0));
            assertEq(service.operatorAccountAt(sub.operator, 0), address(0));
            assertEq(service.friSubscriberCount(sub.sequencer, 0), 0);
            assertEq(service.friSubscriberCount(vm.addr(SEQUENCER_KEY), 0), 0);
            assertEq(service.qualifiedWrapperCount(0), 0);
            assertEq(service.totalQualifiedBonusWeight(0), 0);
            assertEq(service.totalAdmittedBonusWeight(0), 0);
            _subscribe(ALICE_KEY, ALICE_OPERATOR_KEY);
            assertEq(service.nonces(sub.account), 1);
            assertEq(service.operatorAccountAt(sub.operator, 0), sub.account);
        }
    }

    function testAlternateSequencerBootstrapCannotAcceptControlOrCreditDuties() public {
        for (uint256 lane; lane < 3; ++lane) {
            (uint256 chainId, address chainAddress) = _configureLane(lane);
            bytes32 sub = _subscribe(ALICE_KEY, ALICE_OPERATOR_KEY);
            DutySuccessV1[] memory duties = new DutySuccessV1[](0);
            AcceptedPackageV1 memory accepted = _package(duties, true, 1, 2, 0);
            accepted.chainId = chainId;
            accepted.chainAddress = chainAddress;
            accepted.sequencer = address(0xBAD);
            vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidPackage.selector);
            source.bootstrap(service, accepted, duties);
            assertFalse(service.acceptedPackages(ZkSysServiceTypesV1.hashPackage(accepted)));
            assertEq(service.bootstrapVkHash(), bytes32(0));

            duties = new DutySuccessV1[](2);
            duties[0] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, sub, 1, 0, 0);
            duties[1] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, sub, 2, 0, 1);
            accepted.reportHash = ZkSysServiceTypesV1.hashReport(duties);
            vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidPackage.selector);
            source.bootstrap(service, accepted, duties);
            address alice = vm.addr(ALICE_KEY);
            assertFalse(service.acceptedPackages(ZkSysServiceTypesV1.hashPackage(accepted)));
            assertFalse(service.creditedDuties(keccak256(abi.encode(chainId, chainAddress, uint64(1)))));
            assertFalse(service.creditedDuties(keccak256(abi.encode(chainId, chainAddress, uint64(2)))));
            assertEq(service.bootstrapSuccesses(alice), 0);
            assertEq(service.bootstrapDutySlots(alice), 0);
            assertEq(service.assessedSuccesses(alice, 0), 0);
            assertEq(service.rewardedSuccesses(alice, 0), 0);
            assertEq(service.dutySlots(alice, 0), 0);
            assertFalse(service.verifiedForSequencer(alice, accepted.sequencer));
            assertFalse(service.verifiedForSequencer(alice, vm.addr(SEQUENCER_KEY)));
            assertEq(service.qualifiedWrapperCount(0), 0);
            assertEq(service.totalQualifiedBonusWeight(0), 0);
            assertEq(service.totalAdmittedBonusWeight(0), 0);

            accepted.sequencer = vm.addr(SEQUENCER_KEY);
            source.bootstrap(service, accepted, duties);
            assertEq(service.bootstrapSuccesses(alice), 2);
            assertEq(service.bootstrapDutySlots(alice), 3);
            assertTrue(service.verifiedForSequencer(alice, accepted.sequencer));
            assertEq(service.totalQualifiedBonusWeight(0), 35_000 ether);
        }
    }

    function testAlternateSequencerPaidPackageCannotAcceptControlOrCreditDuties() public {
        for (uint256 lane; lane < 3; ++lane) {
            (uint256 chainId, address chainAddress) = _configureLane(lane);
            bytes32 sub = _qualify(ALICE_KEY, ALICE_OPERATOR_KEY, 1);
            _activate(false);
            DutySuccessV1[] memory duties = new DutySuccessV1[](0);
            AcceptedPackageV1 memory accepted = _package(duties, false, 3, 4, 0);
            accepted.chainId = chainId;
            accepted.chainAddress = chainAddress;
            accepted.sequencer = address(0xBAD);
            vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidPackage.selector);
            source.accept(service, accepted, duties);
            assertFalse(service.acceptedPackages(ZkSysServiceTypesV1.hashPackage(accepted)));

            duties = new DutySuccessV1[](1);
            duties[0] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, sub, 3, 0, 0);
            accepted.reportHash = ZkSysServiceTypesV1.hashReport(duties);
            vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidPackage.selector);
            source.accept(service, accepted, duties);
            address alice = vm.addr(ALICE_KEY);
            assertFalse(service.acceptedPackages(ZkSysServiceTypesV1.hashPackage(accepted)));
            assertFalse(service.creditedDuties(keccak256(abi.encode(chainId, chainAddress, uint64(3)))));
            assertEq(service.assessedSuccesses(alice, 0), 0);
            assertEq(service.rewardedSuccesses(alice, 0), 0);
            assertEq(service.dutySlots(alice, 0), 0);
            assertEq(service.bootstrapSuccesses(alice), 2);
            assertEq(service.bootstrapDutySlots(alice), 3);
            assertFalse(service.verifiedForSequencer(alice, accepted.sequencer));
            assertTrue(service.verifiedForSequencer(alice, vm.addr(SEQUENCER_KEY)));
            assertEq(service.totalAdmittedBonusWeight(0), 35_000 ether);
            assertEq(service.totalAdmittedBonusWeight(1), 0);
            assertEq(service.qualifiedWrapperCount(0), 1);
            assertEq(service.qualifiedWrapperCount(1), 0);

            accepted.sequencer = vm.addr(SEQUENCER_KEY);
            source.accept(service, accepted, duties);
            assertEq(service.assessedSuccesses(alice, 0), 1);
            assertEq(service.rewardedSuccesses(alice, 0), 1);
            assertEq(service.dutySlots(alice, 0), 1);
            assertEq(service.bootstrapSuccesses(alice), 2);
            assertEq(service.bootstrapDutySlots(alice), 3);
        }
    }

    function testDispatcherEnumerationIncludesPendingApplicantsAndScopesEligibility() public {
        _subscribe(ALICE_KEY, ALICE_OPERATOR_KEY);
        _subscribe(BOB_KEY, BOB_OPERATOR_KEY);
        address seq = vm.addr(SEQUENCER_KEY);
        assertEq(service.friSubscriberCount(seq, 0), 2);
        assertEq(service.friSubscriberCount(seq, 10), 2);
        assertEq(service.friSubscriberCount(seq, 11), 0);
        assertEq(service.friSubscriberCount(address(123), 0), 0);
        assertEq(service.friSubscriberAt(seq, 0, 0), vm.addr(ALICE_KEY));
        assertTrue(service.isEligibleFriSubscriber(vm.addr(ALICE_KEY), seq, 0));
        assertTrue(service.isEligibleFriSubscriber(vm.addr(BOB_KEY), seq, 0));
        assertEq(service.totalQualifiedBonusWeight(0), 0);
        membership.set(vm.addr(ALICE_KEY), 0, 0, 211_241, uint64(block.timestamp));
        assertFalse(service.isEligibleFriSubscriber(vm.addr(ALICE_KEY), seq, 0));
        assertEq(service.friSubscriberCount(seq, 0), 2);
    }

    function testSubscriptionRequiresTheSharedFriAndWrapperPool() public {
        ProverSubscriptionV1 memory sub = _subscription(ALICE_KEY, ALICE_OPERATOR_KEY);
        uint8[6] memory invalidServices = [uint8(0), 1, 2, 4, 7, 255];
        for (uint256 i; i < invalidServices.length; ++i) {
            sub.services = invalidServices[i];
            bytes32 hash = ZkSysServiceTypesV1.hashSubscription(sub);
            bytes memory signature = _sign(ALICE_KEY, service.subscriptionDigest(sub));
            bytes memory operatorSignature = _sign(ALICE_OPERATOR_KEY, service.subscriptionDigest(sub));
            vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidSubscription.selector);
            service.subscribe(sub, signature, operatorSignature);
            assertEq(service.nonces(sub.account), 0);
            assertEq(service.subscription(hash).account, address(0));
            assertEq(service.subscriptionAt(sub.account, sub.sequencer, 0), bytes32(0));
            assertEq(service.operatorAccountAt(sub.operator, 0), address(0));
            assertEq(service.friSubscriberCount(sub.sequencer, 0), 0);
            assertEq(service.qualifiedWrapperCount(0), 0);
        }
        sub.services = 3;
        service.subscribe(
            sub,
            _sign(ALICE_KEY, service.subscriptionDigest(sub)),
            _sign(ALICE_OPERATOR_KEY, service.subscriptionDigest(sub))
        );
        assertEq(service.nonces(sub.account), 1);
        assertEq(service.friSubscriberCount(sub.sequencer, 0), 1);
        assertTrue(service.isEligibleFriSubscriber(vm.addr(ALICE_KEY), sub.sequencer, 0));
    }

    function testFirstSeniorAgeBoundaryAndAuthenticatedObservationRequired() public {
        ProverSubscriptionV1 memory sub = _subscription(ALICE_KEY, ALICE_OPERATOR_KEY);
        bytes memory signature = _sign(ALICE_KEY, service.subscriptionDigest(sub));
        bytes memory operatorSignature = _sign(ALICE_OPERATOR_KEY, service.subscriptionDigest(sub));
        membership.set(vm.addr(ALICE_KEY), 1_000, 135_000 ether, 211_239, uint64(block.timestamp));
        vm.expectRevert(
            abi.encodeWithSelector(ZkSysProverServiceRegistryV1.NotSeniorOrStale.selector, vm.addr(ALICE_KEY))
        );
        service.subscribe(sub, signature, operatorSignature);
        membership.set(vm.addr(ALICE_KEY), 1_000, 135_000 ether, 211_240, 0);
        vm.expectRevert(
            abi.encodeWithSelector(ZkSysProverServiceRegistryV1.NotSeniorOrStale.selector, vm.addr(ALICE_KEY))
        );
        service.subscribe(sub, signature, operatorSignature);
        _senior(ALICE_KEY, false);
        service.subscribe(sub, signature, operatorSignature);
        assertEq(service.seniorBonus(vm.addr(ALICE_KEY)), 35_000 ether);
        assertEq(service.seniorBonus(vm.addr(BOB_KEY)), 100_000 ether);
    }

    function testWrongAgeWeightAndStaleObservationCannotEnroll() public {
        ProverSubscriptionV1 memory sub = _subscription(ALICE_KEY, ALICE_OPERATOR_KEY);
        bytes memory signature = _sign(ALICE_KEY, service.subscriptionDigest(sub));
        bytes memory operatorSignature = _sign(ALICE_OPERATOR_KEY, service.subscriptionDigest(sub));
        membership.set(vm.addr(ALICE_KEY), 1_000, 200_000 ether, 211_240, uint64(block.timestamp));
        vm.expectRevert(
            abi.encodeWithSelector(ZkSysProverServiceRegistryV1.NotSeniorOrStale.selector, vm.addr(ALICE_KEY))
        );
        service.subscribe(sub, signature, operatorSignature);
        _senior(ALICE_KEY, false);
        vm.warp(block.timestamp + 100_001);
        vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidSubscription.selector);
        service.subscribe(sub, signature, operatorSignature);
        vm.expectRevert(
            abi.encodeWithSelector(ZkSysProverServiceRegistryV1.NotSeniorOrStale.selector, vm.addr(ALICE_KEY))
        );
        service.seniorBonus(vm.addr(ALICE_KEY));
    }

    function testSubscriptionSignatureBindsPayeeAndNonceAndOperatorCannotBeShared() public {
        ProverSubscriptionV1 memory sub = _subscription(ALICE_KEY, ALICE_OPERATOR_KEY);
        bytes memory signature = _sign(ALICE_KEY, service.subscriptionDigest(sub));
        bytes memory operatorSignature = _sign(ALICE_OPERATOR_KEY, service.subscriptionDigest(sub));
        address beneficiary = sub.beneficiary;
        sub.beneficiary = address(0xBAD);
        vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidSignature.selector);
        service.subscribe(sub, signature, operatorSignature);
        sub.beneficiary = beneficiary;
        service.subscribe(sub, signature, operatorSignature);
        vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidSubscription.selector);
        service.subscribe(sub, signature, operatorSignature);
        sub = _subscription(BOB_KEY, ALICE_OPERATOR_KEY);
        signature = _sign(BOB_KEY, service.subscriptionDigest(sub));
        operatorSignature = _sign(ALICE_OPERATOR_KEY, service.subscriptionDigest(sub));
        vm.expectRevert(
            abi.encodeWithSelector(ZkSysProverServiceRegistryV1.OperatorAlreadyUsed.selector, sub.operator, uint64(0))
        );
        service.subscribe(sub, signature, operatorSignature);
    }

    function _assertUnregistered(ProverSubscriptionV1 memory sub) internal view {
        assertEq(service.nonces(sub.account), 0);
        assertEq(service.subscription(ZkSysServiceTypesV1.hashSubscription(sub)).account, address(0));
        for (uint64 period = sub.firstPeriod; period <= sub.lastPeriod; ++period) {
            assertEq(service.subscriptionAt(sub.account, sub.sequencer, period), bytes32(0));
            assertEq(service.operatorAccountAt(sub.operator, period), address(0));
            assertEq(service.friSubscriberCount(sub.sequencer, period), 0);
            assertEq(service.qualifiedWrapperCount(period), 0);
            assertEq(service.totalQualifiedBonusWeight(period), 0);
        }
    }

    function testUnrelatedAccountCannotReserveAnOperatorWithoutItsConsent() public {
        ProverSubscriptionV1 memory sub = _subscription(ALICE_KEY, BOB_OPERATOR_KEY);
        bytes32 digest = service.subscriptionDigest(sub);
        bytes memory accountSignature = _sign(ALICE_KEY, digest);
        bytes memory wrongOperatorSignature = _sign(ALICE_OPERATOR_KEY, digest);
        vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidSignature.selector);
        service.subscribe(sub, accountSignature, "");
        _assertUnregistered(sub);
        vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidSignature.selector);
        service.subscribe(sub, accountSignature, wrongOperatorSignature);
        _assertUnregistered(sub);

        _subscribe(BOB_KEY, BOB_OPERATOR_KEY);
        assertEq(service.operatorAccountAt(sub.operator, 0), vm.addr(BOB_KEY));
        assertEq(service.nonces(sub.account), 0);
    }

    function testOperatorConsentBindsEverySubscriptionField() public {
        ProverSubscriptionV1 memory sub = _subscription(ALICE_KEY, ALICE_OPERATOR_KEY);
        bytes memory accountSignature = _sign(ALICE_KEY, service.subscriptionDigest(sub));
        for (uint256 field; field < 8; ++field) {
            ProverSubscriptionV1 memory authorized = _subscription(ALICE_KEY, ALICE_OPERATOR_KEY);
            if (field == 0) authorized.account = vm.addr(BOB_KEY);
            else if (field == 1) authorized.operator = vm.addr(BOB_OPERATOR_KEY);
            else if (field == 2) authorized.beneficiary = address(0xBAD);
            else if (field == 3) authorized.sequencer = address(0xBAD);
            else if (field == 4) authorized.firstPeriod = 1;
            else if (field == 5) authorized.lastPeriod = 11;
            else if (field == 6) authorized.nonce = 1;
            else authorized.services = 1;
            bytes memory operatorSignature = _sign(ALICE_OPERATOR_KEY, service.subscriptionDigest(authorized));
            vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidSignature.selector);
            service.subscribe(sub, accountSignature, operatorSignature);
            _assertUnregistered(sub);
        }
    }

    function testOperatorConsentCannotCrossRegistryOrChainDomains() public {
        ProverSubscriptionV1 memory sub = _subscription(ALICE_KEY, ALICE_OPERATOR_KEY);
        bytes memory accountSignature = _sign(ALICE_KEY, service.subscriptionDigest(sub));
        ZkSysProverServiceRegistryV1 other = new ZkSysProverServiceRegistryV1(_config(address(source)));
        bytes memory otherRegistrySignature = _sign(ALICE_OPERATOR_KEY, other.subscriptionDigest(sub));
        uint256 chain = block.chainid;
        vm.chainId(chain + 1);
        bytes memory otherChainSignature = _sign(ALICE_OPERATOR_KEY, service.subscriptionDigest(sub));
        vm.chainId(chain);
        vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidSignature.selector);
        service.subscribe(sub, accountSignature, otherRegistrySignature);
        _assertUnregistered(sub);
        vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidSignature.selector);
        service.subscribe(sub, accountSignature, otherChainSignature);
        _assertUnregistered(sub);
    }

    function testContractAccountAndOperatorBothAuthenticateEnrollment() public {
        ProverSubscriptionV1 memory sub = _subscription(ALICE_KEY, ALICE_OPERATOR_KEY);
        sub.account = address(new ServiceContractSignerMockV1(vm.addr(ALICE_KEY)));
        sub.operator = address(new ServiceContractSignerMockV1(vm.addr(ALICE_OPERATOR_KEY)));
        membership.set(sub.account, 1_000, 135_000 ether, 211_240, uint64(block.timestamp));
        bytes32 digest = service.subscriptionDigest(sub);
        bytes memory accountSignature = _sign(ALICE_KEY, digest);
        bytes memory operatorSignature = _sign(ALICE_OPERATOR_KEY, digest);
        vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidSignature.selector);
        service.subscribe(sub, operatorSignature, operatorSignature);
        _assertUnregistered(sub);
        vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidSignature.selector);
        service.subscribe(sub, accountSignature, accountSignature);
        _assertUnregistered(sub);
        service.subscribe(sub, accountSignature, operatorSignature);
        assertEq(service.nonces(sub.account), 1);
        assertEq(service.operatorAccountAt(sub.operator, 0), sub.account);
    }

    function testSameAccountAndOperatorCanReuseOneSignatureForEoaOrContract() public {
        for (uint256 contractSigner; contractSigner < 2; ++contractSigner) {
            ProverSubscriptionV1 memory sub = _subscription(ALICE_KEY, ALICE_KEY);
            if (contractSigner != 0) {
                sub.account = address(new ServiceContractSignerMockV1(vm.addr(ALICE_KEY)));
                sub.operator = sub.account;
                membership.set(sub.account, 1_000, 135_000 ether, 211_240, uint64(block.timestamp));
            }
            bytes memory signature = _sign(ALICE_KEY, service.subscriptionDigest(sub));
            service.subscribe(sub, signature, signature);
            assertEq(service.nonces(sub.account), 1);
            assertEq(service.operatorAccountAt(sub.operator, 0), sub.account);
        }
    }

    function testDistinctBatchesCannotReuseQuotaSlotAndDifferentWorkersCannotReuseBatch() public {
        bytes32 aliceSub = _subscribe(ALICE_KEY, ALICE_OPERATOR_KEY);
        bytes32 bobSub = _subscribe(BOB_KEY, BOB_OPERATOR_KEY);
        DutySuccessV1[] memory duties = new DutySuccessV1[](1);
        duties[0] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, aliceSub, 1, 0, 0);
        source.bootstrap(service, _package(duties, true, 1, 2, 0), duties);
        duties[0] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, aliceSub, 2, 0, 0);
        AcceptedPackageV1 memory accepted = _package(duties, true, 1, 2, 0);
        vm.expectRevert(
            abi.encodeWithSelector(
                ZkSysProverServiceRegistryV1.DutySlotAlreadyUsed.selector, vm.addr(ALICE_KEY), uint64(0), uint16(0)
            )
        );
        source.bootstrap(service, accepted, duties);
        duties[0] = _duty(BOB_KEY, BOB_OPERATOR_KEY, bobSub, 1, 0, 0);
        accepted = _package(duties, true, 1, 2, 0);
        vm.expectRevert(abi.encodeWithSelector(ZkSysProverServiceRegistryV1.DutyAlreadyCredited.selector, uint64(1)));
        source.bootstrap(service, accepted, duties);
        assertEq(service.bootstrapSuccesses(vm.addr(ALICE_KEY)), 1);
        assertEq(service.bootstrapSuccesses(vm.addr(BOB_KEY)), 0);
        assertEq(service.totalQualifiedBonusWeight(0), 0);
    }

    function testBootstrapDoesNotConsumePaidSlotsOrEarnRewards() public {
        bytes32 sub = _qualify(ALICE_KEY, ALICE_OPERATOR_KEY, 1);
        _activate(false);
        assertEq(service.bootstrapSuccesses(vm.addr(ALICE_KEY)), 2);
        assertEq(service.dutySlots(vm.addr(ALICE_KEY), 0), 0);
        DutySuccessV1[] memory duties = new DutySuccessV1[](1);
        duties[0] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, sub, 3, 0, 0);
        source.accept(service, _package(duties, false, 3, 4, 0), duties);
        vm.warp(11_100);
        assertEq(service.serviceFactorBps(vm.addr(ALICE_KEY), 0), 5_000);
        assertEq(service.serviceFactorBps(vm.addr(BOB_KEY), 0), 0);
        assertEq(service.totalAdmittedBonusWeight(0), 35_000 ether);
        assertEq(service.totalAdmittedBonusWeight(1), 0);
    }

    function testRenewalIsFutureOnlyAndNeverChangesFrozenRoundDenominator() public {
        bytes32 sub = _qualify(ALICE_KEY, ALICE_OPERATOR_KEY, 1);
        _activate(false);
        DutySuccessV1[] memory duties = new DutySuccessV1[](2);
        duties[0] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, sub, 3, 0, 0);
        duties[1] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, sub, 4, 0, 1);
        source.accept(service, _package(duties, false, 3, 4, 0), duties);
        assertEq(service.totalAdmittedBonusWeight(0), 35_000 ether);
        assertEq(service.totalAdmittedBonusWeight(1), 35_000 ether);
        vm.warp(11_100);
        assertEq(service.serviceFactorBps(vm.addr(ALICE_KEY), 0), 10_000);
    }

    function _completeAdjacentRenewal(bool rotateOperator, bool afterCutoff) internal {
        ProverSubscriptionV1 memory oldSubscription = _subscription(ALICE_KEY, ALICE_OPERATOR_KEY);
        oldSubscription.lastPeriod = 63;
        bytes32 digest = service.subscriptionDigest(oldSubscription);
        bytes32 oldHash =
            service.subscribe(oldSubscription, _sign(ALICE_KEY, digest), _sign(ALICE_OPERATOR_KEY, digest));
        DutySuccessV1[] memory duties = new DutySuccessV1[](2);
        duties[0] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, oldHash, 1, 0, 0);
        duties[1] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, oldHash, 2, 0, 1);
        source.bootstrap(service, _package(duties, true, 1, 2, 0), duties);

        uint256 nextOperatorKey = rotateOperator ? BOB_OPERATOR_KEY : ALICE_OPERATOR_KEY;
        ProverSubscriptionV1 memory successor = _subscription(ALICE_KEY, nextOperatorKey);
        successor.firstPeriod = 64;
        successor.lastPeriod = 127;
        successor.nonce = 1;
        if (rotateOperator) successor.beneficiary = address(0xBEEF);
        digest = service.subscriptionDigest(successor);
        bytes32 successorHash = service.subscribe(successor, _sign(ALICE_KEY, digest), _sign(nextOperatorKey, digest));
        _activate(false);
        vm.warp(issuer.startTime() + 63 * issuer.periodSeconds() + (afterCutoff ? 900 : 0));
        uint64 destination = afterCutoff ? 65 : 64;
        address account = oldSubscription.account;
        assertEq(service.nextAdmissionPeriod(), destination);
        assertEq(service.subscriptionAt(account, successor.sequencer, destination), successorHash);
        uint256 frozenBonus = service.totalAdmittedBonusWeight(63);
        duties[0] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, oldHash, 3, 63, 0);
        duties[1] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, oldHash, 4, 63, 1);
        AcceptedPackageV1 memory accepted = _package(duties, false, 3, 4, 63);
        if (rotateOperator) {
            duties[0].operatorSignature = _sign(nextOperatorKey, service.dutyDigest(duties[0]));
            accepted.reportHash = ZkSysServiceTypesV1.hashReport(duties);
            vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidSignature.selector);
            source.accept(service, accepted, duties);
            duties[0].operatorSignature = _sign(ALICE_OPERATOR_KEY, service.dutyDigest(duties[0]));
            accepted.reportHash = ZkSysServiceTypesV1.hashReport(duties);
        }
        source.accept(service, accepted, duties);
        assertEq(service.assessedSuccesses(account, 63), 2);
        assertEq(service.totalAdmittedBonusWeight(63), frozenBonus);
        assertEq(service.admittedBonusWeight(account, destination), 35_000 ether);
        assertEq(service.totalAdmittedBonusWeight(destination), 35_000 ether);
        assertEq(service.qualifiedWrapperCount(destination), 1);
        WrapperCandidateV1[] memory candidates = new WrapperCandidateV1[](1);
        candidates[0] = service.qualifiedWrapper(account, destination);
        assertEq(candidates[0].operator, successor.operator);
        assertEq(candidates[0].beneficiary, successor.beneficiary);
        if (afterCutoff) {
            assertEq(service.totalAdmittedBonusWeight(64), 0);
            assertEq(service.qualifiedWrapperCount(64), 0);
        }

        source.accept(service, accepted, duties);
        service.renewWrapper(successorHash, destination);
        assertEq(service.totalAdmittedBonusWeight(destination), 35_000 ether);
        assertEq(service.qualifiedWrapperCount(destination), 1);
        vm.warp(service.rosterCutoff(destination));
        bytes32 frozenRoot = service.publishRosterPage(destination, candidates);
        source.accept(service, accepted, duties);
        assertEq(service.publishedRosterRoot(destination), frozenRoot);
        assertEq(service.totalAdmittedBonusWeight(destination), 35_000 ether);
        assertEq(service.totalAdmittedBonusWeight(destination + 1), 0);
        assertEq(service.qualifiedWrapperCount(destination + 1), 0);
    }

    function testAdjacentSubscriptionRenewalPreservesBonusAdmission() public {
        _completeAdjacentRenewal(false, false);
    }

    function testAdjacentSubscriptionUsesNewOperatorAndPayeeButOriginalDutySignature() public {
        _completeAdjacentRenewal(true, false);
    }

    function testAdjacentRenewalAfterCutoffUsesLaterDestinationWithoutChangingFrozenPeriod() public {
        _completeAdjacentRenewal(false, true);
    }

    function testCompletedQuotaCannotAdmitAnExpiredSubscriptionWithoutSuccessor() public {
        bytes32 sub = _qualify(ALICE_KEY, ALICE_OPERATOR_KEY, 1);
        _activate(false);
        vm.warp(issuer.startTime() + 10 * issuer.periodSeconds());
        DutySuccessV1[] memory duties = new DutySuccessV1[](2);
        duties[0] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, sub, 3, 10, 0);
        duties[1] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, sub, 4, 10, 1);
        source.accept(service, _package(duties, false, 3, 4, 10), duties);
        assertEq(service.assessedSuccesses(vm.addr(ALICE_KEY), 10), 2);
        assertEq(service.totalAdmittedBonusWeight(11), 0);
        assertEq(service.qualifiedWrapperCount(11), 0);
    }

    function testIdleVerifiedWrapperCanRenewWithoutAnyBonusAdmission() public {
        bytes32 sub = _qualify(ALICE_KEY, ALICE_OPERATOR_KEY, 1);
        _activate(false);
        assertTrue(service.verifiedForSequencer(vm.addr(ALICE_KEY), vm.addr(SEQUENCER_KEY)));
        assertEq(service.nextAdmissionPeriod(), 1);
        service.renewWrapper(sub, 1);
        service.renewWrapper(sub, 1);
        assertEq(service.qualifiedWrapperCount(1), 1);
        assertEq(service.totalAdmittedBonusWeight(1), 0);
        assertEq(service.totalQualifiedBonusWeight(1), 0);
        assertEq(service.assessedSuccesses(vm.addr(ALICE_KEY), 0), 0);
        vm.warp(service.rosterCutoff(1));
        WrapperCandidateV1[] memory candidates = new WrapperCandidateV1[](1);
        candidates[0] = service.qualifiedWrapper(vm.addr(ALICE_KEY), 1);
        assertTrue(service.publishRosterPage(1, candidates) != bytes32(0));
        vm.warp(11_100);
        assertEq(service.serviceFactorBps(vm.addr(ALICE_KEY), 0), 0);
    }

    function testUnverifiedAndPartialQuotaCannotRenewWrapper() public {
        bytes32 sub = _subscribe(ALICE_KEY, ALICE_OPERATOR_KEY);
        vm.expectRevert(
            abi.encodeWithSelector(
                ZkSysProverServiceRegistryV1.ServiceNotVerified.selector, vm.addr(ALICE_KEY), vm.addr(SEQUENCER_KEY)
            )
        );
        service.renewWrapper(sub, 0);
        DutySuccessV1[] memory duties = new DutySuccessV1[](1);
        duties[0] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, sub, 1, 0, 0);
        source.bootstrap(service, _package(duties, true, 1, 2, 0), duties);
        vm.expectRevert(
            abi.encodeWithSelector(
                ZkSysProverServiceRegistryV1.ServiceNotVerified.selector, vm.addr(ALICE_KEY), vm.addr(SEQUENCER_KEY)
            )
        );
        service.renewWrapper(sub, 0);
        assertEq(service.qualifiedWrapperCount(0), 0);
    }

    function testRemovedMembershipBlocksHistoricalWrapperRenewal() public {
        bytes32 sub = _qualify(ALICE_KEY, ALICE_OPERATOR_KEY, 1);
        _activate(false);
        membership.set(vm.addr(ALICE_KEY), 0, 0, 211_250, uint64(block.timestamp));
        vm.expectRevert(
            abi.encodeWithSelector(ZkSysProverServiceRegistryV1.NotSeniorOrStale.selector, vm.addr(ALICE_KEY))
        );
        service.renewWrapper(sub, 1);
        assertEq(service.qualifiedWrapperCount(1), 0);
        assertEq(service.totalAdmittedBonusWeight(1), 0);
    }

    function testHistoricalVerificationCannotBeReusedForAnotherSequencer() public {
        _qualify(ALICE_KEY, ALICE_OPERATOR_KEY, 1);
        _activate(false);
        ProverSubscriptionV1 memory subscription_ = _subscription(ALICE_KEY, ALICE_OPERATOR_KEY);
        subscription_.sequencer = address(0xBAD);
        subscription_.nonce = 1;
        subscription_.firstPeriod = 1;
        bytes memory signature = _sign(ALICE_KEY, service.subscriptionDigest(subscription_));
        bytes memory operatorSignature = _sign(ALICE_OPERATOR_KEY, service.subscriptionDigest(subscription_));
        vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidSubscription.selector);
        service.subscribe(subscription_, signature, operatorSignature);
        assertEq(service.nonces(subscription_.account), 1);
        assertEq(service.subscriptionAt(subscription_.account, subscription_.sequencer, 1), bytes32(0));
        assertFalse(service.verifiedForSequencer(subscription_.account, subscription_.sequencer));
        assertTrue(service.verifiedForSequencer(subscription_.account, vm.addr(SEQUENCER_KEY)));
        assertEq(service.bootstrapSuccesses(subscription_.account), 2);
        assertEq(service.totalAdmittedBonusWeight(0), 35_000 ether);
        assertEq(service.totalAdmittedBonusWeight(1), 0);
        assertEq(service.qualifiedWrapperCount(0), 1);
        assertEq(service.qualifiedWrapperCount(1), 0);
    }

    function testIdleRenewalCannotChangeRosterAfterCutoff() public {
        bytes32 sub = _qualify(ALICE_KEY, ALICE_OPERATOR_KEY, 1);
        _activate(false);
        vm.warp(service.rosterCutoff(1));
        assertEq(service.nextAdmissionPeriod(), 2);
        vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidSubscription.selector);
        service.renewWrapper(sub, 1);
        service.renewWrapper(sub, 2);
        assertEq(service.qualifiedWrapperCount(1), 0);
        assertEq(service.qualifiedWrapperCount(2), 1);
        assertEq(service.totalAdmittedBonusWeight(2), 0);
    }

    function testQualificationAfterRosterCutoffRollsToLaterPeriod() public {
        bytes32 sub = _qualify(ALICE_KEY, ALICE_OPERATOR_KEY, 1);
        _activate(false);
        vm.warp(service.rosterCutoff(1));
        DutySuccessV1[] memory duties = new DutySuccessV1[](2);
        duties[0] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, sub, 3, 0, 0);
        duties[1] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, sub, 4, 0, 1);
        source.accept(service, _package(duties, false, 3, 4, 0), duties);
        assertEq(service.totalAdmittedBonusWeight(1), 0);
        assertEq(service.totalAdmittedBonusWeight(2), 35_000 ether);
    }

    function testRemovalStopsRenewalWithoutErasingEarnedWorkOrFrozenQuota() public {
        bytes32 sub = _qualify(ALICE_KEY, ALICE_OPERATOR_KEY, 1);
        _activate(false);
        membership.set(vm.addr(ALICE_KEY), 0, 0, 211_250, uint64(block.timestamp));
        DutySuccessV1[] memory duties = new DutySuccessV1[](2);
        duties[0] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, sub, 3, 0, 0);
        duties[1] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, sub, 4, 0, 1);
        source.accept(service, _package(duties, false, 3, 4, 0), duties);
        assertEq(service.totalAdmittedBonusWeight(0), 35_000 ether);
        assertEq(service.totalAdmittedBonusWeight(1), 0);
        vm.warp(11_100);
        assertEq(service.serviceFactorBps(vm.addr(ALICE_KEY), 0), 10_000);
    }

    function testDuplicatePackageIsIdempotentButRetryCannotInflateCredit() public {
        bytes32 sub = _subscribe(ALICE_KEY, ALICE_OPERATOR_KEY);
        DutySuccessV1[] memory duties = new DutySuccessV1[](1);
        duties[0] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, sub, 1, 0, 0);
        AcceptedPackageV1 memory accepted = _package(duties, true, 1, 2, 0);
        source.bootstrap(service, accepted, duties);
        source.bootstrap(service, accepted, duties);
        assertEq(service.bootstrapSuccesses(vm.addr(ALICE_KEY)), 1);
        duties[0].attempt = 2;
        duties[0].slot = 1;
        duties[0].operatorSignature = _sign(ALICE_OPERATOR_KEY, service.dutyDigest(duties[0]));
        accepted.reportHash = ZkSysServiceTypesV1.hashReport(duties);
        vm.expectRevert(abi.encodeWithSelector(ZkSysProverServiceRegistryV1.DutyAlreadyCredited.selector, uint64(1)));
        source.bootstrap(service, accepted, duties);
    }

    function testEmptyCompanionHasNoCreditAndEmptyReportCanProgress() public {
        bytes32 sub = _subscribe(ALICE_KEY, ALICE_OPERATOR_KEY);
        DutySuccessV1[] memory duties = new DutySuccessV1[](1);
        duties[0] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, sub, 1, 0, 0);
        duties[0].transactionCount = 0;
        duties[0].operatorSignature = _sign(ALICE_OPERATOR_KEY, service.dutyDigest(duties[0]));
        AcceptedPackageV1 memory accepted = _package(duties, true, 1, 2, 0);
        vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidDuty.selector);
        source.bootstrap(service, accepted, duties);
        duties = new DutySuccessV1[](0);
        accepted.reportHash = ZkSysServiceTypesV1.hashReport(duties);
        source.bootstrap(service, accepted, duties);
        assertTrue(service.acceptedPackages(ZkSysServiceTypesV1.hashPackage(accepted)));
        assertEq(service.totalQualifiedBonusWeight(0), 0);
    }

    function testUnauthorizedCallerAndForgedWorkerCannotCredit() public {
        bytes32 sub = _subscribe(ALICE_KEY, ALICE_OPERATOR_KEY);
        DutySuccessV1[] memory duties = new DutySuccessV1[](1);
        duties[0] = _duty(ALICE_KEY, BOB_OPERATOR_KEY, sub, 1, 0, 0);
        AcceptedPackageV1 memory accepted = _package(duties, true, 1, 2, 0);
        vm.expectRevert(ZkSysProverServiceRegistryV1.UnauthorizedAcceptanceSource.selector);
        service.acceptBootstrapDuties(accepted, duties);
        vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidSignature.selector);
        source.bootstrap(service, accepted, duties);
        assertEq(service.totalQualifiedBonusWeight(0), 0);
    }

    function testReportHashAndChainDomainCannotBeSubstituted() public {
        bytes32 sub = _subscribe(ALICE_KEY, ALICE_OPERATOR_KEY);
        DutySuccessV1[] memory duties = new DutySuccessV1[](1);
        duties[0] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, sub, 1, 0, 0);
        AcceptedPackageV1 memory accepted = _package(duties, true, 1, 2, 0);
        accepted.chainId++;
        vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidPackage.selector);
        source.bootstrap(service, accepted, duties);
        accepted.chainId--;
        accepted.reportHash = bytes32(uint256(1));
        vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidPackage.selector);
        source.bootstrap(service, accepted, duties);
    }

    function testRoundFinalizationIsImmutableAtExactCutoff() public {
        bytes32 sub = _qualify(ALICE_KEY, ALICE_OPERATOR_KEY, 1);
        _activate(false);
        vm.expectRevert(abi.encodeWithSelector(ZkSysProverServiceRegistryV1.RoundNotFinalized.selector, uint256(0)));
        service.serviceFactorBps(vm.addr(ALICE_KEY), 0);
        DutySuccessV1[] memory duties = new DutySuccessV1[](1);
        duties[0] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, sub, 3, 0, 0);
        AcceptedPackageV1 memory accepted = _package(duties, false, 3, 4, 0);
        vm.warp(11_100);
        assertTrue(service.isRoundFinalized(0));
        vm.expectRevert(abi.encodeWithSelector(ZkSysProverServiceRegistryV1.RoundClosed.selector, uint64(0)));
        source.accept(service, accepted, duties);
        assertEq(service.serviceFactorBps(vm.addr(ALICE_KEY), 0), 0);
    }

    function testPagedRosterCannotOmitMembersOrPoisonAnotherPublisher() public {
        _qualify(ALICE_KEY, ALICE_OPERATOR_KEY, 1);
        _qualify(BOB_KEY, BOB_OPERATOR_KEY, 3);
        WrapperCandidateV1[] memory all = _candidates(true);
        vm.warp(service.rosterCutoff(0));
        WrapperCandidateV1[] memory page = new WrapperCandidateV1[](1);
        page[0] = WrapperCandidateV1(0, all[1].account, all[1].operator, all[1].beneficiary);
        vm.prank(address(0xBAD));
        assertEq(service.publishRosterPage(0, page), bytes32(0));
        assertEq(service.publishedRosterRoot(0), bytes32(0));
        page[0] = all[0];
        assertEq(service.publishRosterPage(0, page), bytes32(0));
        page[0] = all[1];
        bytes32 root = service.publishRosterPage(0, page);
        (bytes32 expected,) = _tree(all, 0);
        assertEq(root, expected);
        assertEq(service.publishedRosterRoot(0), expected);
        source.activate(service, VK, root);
        assertEq(service.totalAdmittedBonusWeight(0), 135_000 ether);
    }

    function testRosterRejectsPayeeSubstitutionAndOversizedPage() public {
        _qualify(ALICE_KEY, ALICE_OPERATOR_KEY, 1);
        vm.warp(service.rosterCutoff(0));
        WrapperCandidateV1[] memory candidates = _candidates(false);
        candidates[0].beneficiary = address(0xBAD);
        vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidRoster.selector);
        service.publishRosterPage(0, candidates);
        candidates = new WrapperCandidateV1[](65);
        vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidRoster.selector);
        service.publishRosterPage(0, candidates);
    }

    function testPagedOddSizedRosterMatchesFixedDepthMerkleTree() public {
        _qualify(ALICE_KEY, ALICE_OPERATOR_KEY, 1);
        _qualify(BOB_KEY, BOB_OPERATOR_KEY, 3);
        _senior(3, false);
        _qualify(3, 33, 5);
        WrapperCandidateV1[] memory all = new WrapperCandidateV1[](3);
        for (uint256 i; i < 3; ++i) {
            all[i] = service.qualifiedWrapper(vm.addr(i + 1), 0);
        }
        for (uint256 i; i < 3; ++i) {
            for (uint256 j = i + 1; j < 3; ++j) {
                if (all[i].account > all[j].account) (all[i], all[j]) = (all[j], all[i]);
            }
            all[i].index = uint32(i);
        }
        vm.warp(service.rosterCutoff(0));
        WrapperCandidateV1[] memory page = new WrapperCandidateV1[](2);
        page[0] = all[0];
        page[1] = all[1];
        assertEq(service.publishRosterPage(0, page), bytes32(0));
        page = new WrapperCandidateV1[](1);
        page[0] = all[2];
        bytes32 root = service.publishRosterPage(0, page);
        (bytes32 expected,) = _tree(all, 2);
        assertEq(root, expected);
    }

    function testActivationCannotUseUnpublishedRosterWrongVkOrLateMessage() public {
        _qualify(ALICE_KEY, ALICE_OPERATOR_KEY, 1);
        vm.expectRevert(ZkSysProverServiceRegistryV1.NoQualifiedRoster.selector);
        source.activate(service, VK, keccak256("unpublished"));
        vm.warp(service.rosterCutoff(0));
        bytes32 root = service.publishRosterPage(0, _candidates(false));
        vm.expectRevert(ZkSysProverServiceRegistryV1.InvalidPackage.selector);
        source.activate(service, keccak256("wrong-key"), root);
        vm.warp(issuer.startTime());
        vm.expectRevert(ZkSysProverServiceRegistryV1.WrongServiceMode.selector);
        source.activate(service, VK, root);
        assertFalse(service.serviceActive());
    }

    function testChildReceiverAuthenticatesGatewayMessageAndPayload() public {
        uint64 nonce = vm.getNonce(address(this));
        address predictedRegistry = vm.computeCreateAddress(address(this), nonce + 1);
        ZkSysAcceptedServiceReceiverV1 receiver = new ZkSysAcceptedServiceReceiverV1(
            506, address(0x6A7E), ZkSysProverServiceRegistryV1(predictedRegistry), address(0)
        );
        service = new ZkSysProverServiceRegistryV1(_config(address(receiver)));
        assertEq(address(service), predictedRegistry);
        ServiceMessageVerifierMockV1 implementation = new ServiceMessageVerifierMockV1();
        vm.etch(address(0x10009), address(implementation).code);
        ServiceMessageVerifierMockV1 verifier = ServiceMessageVerifierMockV1(address(0x10009));
        bytes32 sub = _subscribe(ALICE_KEY, ALICE_OPERATOR_KEY);
        DutySuccessV1[] memory duties = new DutySuccessV1[](1);
        duties[0] = _duty(ALICE_KEY, ALICE_OPERATOR_KEY, sub, 1, 0, 0);
        AcceptedPackageV1 memory accepted = _package(duties, true, 1, 2, 0);
        ServiceMessageProofV1 memory inclusion = ServiceMessageProofV1(7, 8, 9, new bytes32[](0));
        vm.expectRevert(ZkSysAcceptedServiceReceiverV1.MessageNotIncluded.selector);
        receiver.relayAccepted(accepted, duties, true, inclusion);
        bytes memory data = abi.encode(receiver.ACCEPTED_DOMAIN(), ZkSysServiceTypesV1.hashPackage(accepted), true);
        verifier.expectMessage(506, 7, 8, 9, address(0x6A7E), data);
        receiver.relayAccepted(accepted, duties, true, inclusion);
        receiver.relayAccepted(accepted, duties, true, inclusion);
        assertEq(service.bootstrapSuccesses(vm.addr(ALICE_KEY)), 1);
        accepted.manifestHash = keccak256("substitution");
        vm.expectRevert(ZkSysAcceptedServiceReceiverV1.MessageNotIncluded.selector);
        receiver.relayAccepted(accepted, duties, true, inclusion);
    }
}

contract ZkSysWrapperCoordinatorV1Test is ServiceTestBaseV1 {
    ServiceGateMockV1 internal gate;
    ServiceRootMockV1 internal roots;
    ServiceRosterMockV1 internal rosters;
    ZkSysWrapperCoordinatorV1 internal coordinator;
    WrapperCandidateV1[] internal candidates;

    function setUp() public {
        vm.warp(1_000);
        _installMessenger();
        gate = new ServiceGateMockV1();
        roots = new ServiceRootMockV1();
        rosters = new ServiceRosterMockV1();
        candidates.push(WrapperCandidateV1(0, vm.addr(ALICE_KEY), vm.addr(ALICE_OPERATOR_KEY), address(0xA)));
        candidates.push(WrapperCandidateV1(1, vm.addr(BOB_KEY), vm.addr(BOB_OPERATOR_KEY), address(0xB)));
        if (candidates[0].account > candidates[1].account) {
            WrapperCandidateV1 memory first = candidates[0];
            candidates[0] = candidates[1];
            candidates[1] = first;
            candidates[0].index = 0;
            candidates[1].index = 1;
        }
        (bytes32 root,) = _tree(candidates, 0);
        rosters.set(0, root, 2);
        coordinator = new ZkSysWrapperCoordinatorV1(
            ZkSysWrapperCoordinatorV1.Configuration({
                acceptanceGate: address(gate),
                rootSource: roots,
                rosterSource: rosters,
                sequencer: vm.addr(SEQUENCER_KEY),
                childChainId: block.chainid,
                childChainAddress: CHAIN,
                policyHash: POLICY,
                productionVkHash: VK,
                initialParent: keccak256("genesis-parent"),
                firstBatch: 1,
                firstServicePeriod: 0,
                turnSeconds: 10,
                nativeRootDraw: false
            })
        );
    }

    function _prepare() internal {
        bytes32 commitment = coordinator.prepareRosterDraw(0);
        roots.set(commitment, 123, keccak256("authenticated-later-root-block"));
        coordinator.recordRosterDraw(0);
    }

    function _proposed() internal view returns (AcceptedPackageV1 memory accepted) {
        accepted = _basePackage();
        accepted.rosterRoot = rosters.root();
    }

    function _signed(AcceptedPackageV1 memory proposed)
        internal
        view
        returns (
            AcceptedPackageV1 memory,
            WrapperCandidateV1 memory candidate,
            bytes32[] memory proof,
            bytes memory seqSig,
            bytes memory wrapperSig
        )
    {
        AcceptedPackageV1 memory accepted = abi.decode(abi.encode(proposed), (AcceptedPackageV1));
        uint32 index = coordinator.selectedWrapperIndex();
        candidate = candidates[index];
        (, proof) = _tree(candidates, index);
        accepted.turn = coordinator.currentTurn();
        accepted.wrapper = candidate.operator;
        accepted.wrapperBeneficiary = candidate.beneficiary;
        accepted.proofHash = keccak256("native-valid-snark");
        bytes32 digest = coordinator.packageDigest(accepted);
        seqSig = _sign(SEQUENCER_KEY, digest);
        wrapperSig =
            _sign(candidate.operator == vm.addr(ALICE_OPERATOR_KEY) ? ALICE_OPERATOR_KEY : BOB_OPERATOR_KEY, digest);
        return (accepted, candidate, proof, seqSig, wrapperSig);
    }

    function testRootDrawMustMatchFrozenRosterCommitment() public {
        bytes32 commitment = coordinator.prepareRosterDraw(0);
        assertEq(
            ServiceMessengerMockV1(address(0x8008)).lastMessage(), abi.encode(coordinator.DRAW_DOMAIN(), commitment)
        );
        assertEq(coordinator.prepareRosterDraw(0), commitment);
        assertEq(ServiceMessengerMockV1(address(0x8008)).messages(), 1);
        roots.set(keccak256("unrelated"), 123, keccak256("known-hash"));
        vm.expectRevert(ZkSysWrapperCoordinatorV1.RootNotAvailable.selector);
        coordinator.recordRosterDraw(0);
        AcceptedPackageV1 memory proposed = _proposed();
        vm.expectRevert(ZkSysWrapperCoordinatorV1.RootNotAvailable.selector);
        gate.open(coordinator, proposed);
        roots.set(commitment, 124, keccak256("causally-later-root"));
        coordinator.recordRosterDraw(0);
        bytes32 seed = coordinator.rosterSeed(0);
        roots.set(commitment, 125, keccak256("changed-source"));
        coordinator.recordRosterDraw(0);
        assertEq(coordinator.rosterSeed(0), seed);
    }

    function testNoRepeatWithinCycleAndNoDeadlineResetAcrossRetry() public {
        _prepare();
        gate.open(coordinator, _proposed());
        uint32 first = coordinator.selectedWrapperIndex();
        vm.warp(1_010);
        assertEq(coordinator.currentTurn(), 1);
        assertTrue(coordinator.selectedWrapperIndex() != first);
        vm.warp(1_020);
        assertEq(coordinator.currentTurn(), 2);
        assertEq(coordinator.selectedWrapperIndex(), first);
        assertEq(coordinator.turnsStartedAt(), 1_000);
        assertEq(coordinator.packageOrdinal(), 0);
    }

    function testCurrentRosterWinsAndFutureDrawCannotHideRecoveryRoster() public {
        _prepare();
        bytes32 initialRoot = rosters.root();
        bytes32 nextRoot = keccak256("next-roster");
        rosters.set(1, nextRoot, 2);
        bytes32 commitment = coordinator.prepareRosterDraw(1);
        roots.set(commitment, 124, keccak256("next-root-block"));
        coordinator.recordRosterDraw(1);
        (uint64 period, bytes32 root,, bool control) = coordinator.openingRoster();
        assertEq(period, 0);
        assertEq(root, initialRoot);
        assertFalse(control);

        vm.warp(3_000);
        (period, root,, control) = coordinator.openingRoster();
        assertEq(period, 1);
        assertEq(root, nextRoot);
        assertTrue(control);
        rosters.set(3, keccak256("future-roster"), 2);
        commitment = coordinator.prepareRosterDraw(3);
        roots.set(commitment, 125, keccak256("future-root-block"));
        coordinator.recordRosterDraw(3);
        (period, root,, control) = coordinator.openingRoster();
        assertEq(period, 1);
        assertEq(root, nextRoot);
        assertTrue(control);

        bytes32 currentRoot = keccak256("current-roster");
        rosters.set(2, currentRoot, 2);
        commitment = coordinator.prepareRosterDraw(2);
        roots.set(commitment, 126, keccak256("current-root-block"));
        coordinator.recordRosterDraw(2);
        (period, root,, control) = coordinator.openingRoster();
        assertEq(period, 2);
        assertEq(root, currentRoot);
        assertFalse(control);
    }

    function testOnlyNextFutureDrawCanEnterReadyCache() public {
        _prepare();
        rosters.set(2, keccak256("too-far-roster"), 2);
        bytes32 commitment = coordinator.prepareRosterDraw(2);
        roots.set(commitment, 124, keccak256("too-far-root-block"));
        vm.expectRevert(ZkSysWrapperCoordinatorV1.InvalidRoster.selector);
        coordinator.recordRosterDraw(2);
        vm.warp(2_000);
        coordinator.recordRosterDraw(2);
        (uint64 period,,, bool control) = coordinator.openingRoster();
        assertEq(period, 0);
        assertTrue(control);
        vm.warp(3_000);
        (period,,, control) = coordinator.openingRoster();
        assertEq(period, 2);
        assertFalse(control);
    }

    function testOnlyGateAndBothCorrectSignaturesCanAccept() public {
        _prepare();
        AcceptedPackageV1 memory proposed = _proposed();
        vm.expectRevert(ZkSysWrapperCoordinatorV1.UnauthorizedGate.selector);
        coordinator.openPackage(proposed);
        gate.open(coordinator, proposed);
        (
            AcceptedPackageV1 memory accepted,
            WrapperCandidateV1 memory candidate,
            bytes32[] memory proof,
            bytes memory seqSig,
            bytes memory wrapperSig
        ) = _signed(proposed);
        vm.expectRevert(ZkSysWrapperCoordinatorV1.InvalidSignature.selector);
        gate.accept(coordinator, accepted, candidate, proof, wrapperSig, seqSig);
        bytes32 packageHash = gate.accept(coordinator, accepted, candidate, proof, seqSig, wrapperSig);
        assertEq(coordinator.acceptedParent(), packageHash);
        assertEq(coordinator.nextBatch(), 3);
        assertEq(coordinator.packageOrdinal(), 1);
        vm.expectRevert(ZkSysWrapperCoordinatorV1.NoOpenPackage.selector);
        gate.accept(coordinator, accepted, candidate, proof, seqSig, wrapperSig);
    }

    function testExpiredSignatureCannotReturnInLaterCycle() public {
        _prepare();
        gate.open(coordinator, _proposed());
        (
            AcceptedPackageV1 memory accepted,
            WrapperCandidateV1 memory candidate,
            bytes32[] memory proof,
            bytes memory seqSig,
            bytes memory wrapperSig
        ) = _signed(_proposed());
        vm.warp(1_020);
        assertEq(coordinator.selectedWrapperIndex(), candidate.index);
        vm.expectRevert(ZkSysWrapperCoordinatorV1.WrongWrapper.selector);
        gate.accept(coordinator, accepted, candidate, proof, seqSig, wrapperSig);
    }

    function testRepairPreservesDrawClockAndRejectsPriorEndorsement() public {
        _prepare();
        AcceptedPackageV1 memory proposed = _proposed();
        gate.open(coordinator, proposed);
        uint32 first = coordinator.firstWrapperIndex();
        (
            AcceptedPackageV1 memory accepted,
            WrapperCandidateV1 memory candidate,
            bytes32[] memory proof,
            bytes memory seqSig,
            bytes memory wrapperSig
        ) = _signed(proposed);
        proposed.reportHash = keccak256("corrected-report");
        proposed.manifestHash = keccak256("repaired-fri");
        proposed.batchTo = 3;
        gate.repair(coordinator, proposed);
        assertEq(coordinator.firstWrapperIndex(), first);
        assertEq(coordinator.turnsStartedAt(), 1_000);
        assertEq(coordinator.packageOrdinal(), 0);
        vm.expectRevert(ZkSysWrapperCoordinatorV1.InvalidPackage.selector);
        gate.accept(coordinator, accepted, candidate, proof, seqSig, wrapperSig);
        (accepted, candidate, proof, seqSig, wrapperSig) = _signed(proposed);
        gate.accept(coordinator, accepted, candidate, proof, seqSig, wrapperSig);
        assertEq(coordinator.nextBatch(), 4);
    }

    function testRepairCannotChangeParentOrPayeeAndCannotRerollOpenPackage() public {
        _prepare();
        AcceptedPackageV1 memory proposed = _proposed();
        gate.open(coordinator, proposed);
        vm.expectRevert(ZkSysWrapperCoordinatorV1.PackageAlreadyOpen.selector);
        gate.open(coordinator, proposed);
        proposed.sequencerBeneficiary = address(0xBAD);
        vm.expectRevert(ZkSysWrapperCoordinatorV1.InvalidPackage.selector);
        gate.repair(coordinator, proposed);
        proposed = _proposed();
        proposed.parent = keccak256("wrong-parent");
        vm.expectRevert(ZkSysWrapperCoordinatorV1.InvalidPackage.selector);
        gate.repair(coordinator, proposed);
    }

    function testRosterReceiverRequiresExactExecutedChildMessage() public {
        ServiceMessageVerifierMockV1 mailbox = new ServiceMessageVerifierMockV1();
        ZkSysQualifiedRosterReceiverV1 receiver = new ZkSysQualifiedRosterReceiverV1(
            ZkSysQualifiedRosterReceiverV1.Configuration({
                childChainId: block.chainid,
                childRegistry: address(0x5E),
                messageSender: address(0x5E),
                childMailbox: IZkSysChildMailboxV1(address(mailbox)),
                policyHash: POLICY,
                startTime: 2_000,
                periodSeconds: 1_000,
                firstServicePeriod: 0
            })
        );
        ServiceMessageProofV1 memory inclusion = ServiceMessageProofV1(7, 8, 9, new bytes32[](0));
        bytes32 root = rosters.root();
        vm.expectRevert(ZkSysQualifiedRosterReceiverV1.MessageNotIncluded.selector);
        receiver.relayRoster(0, root, 2, inclusion);
        bytes memory data = abi.encode(
            receiver.ROSTER_DOMAIN(),
            block.chainid,
            address(0x5E),
            POLICY,
            uint256(2_000),
            uint256(1_000),
            uint64(0),
            uint64(0),
            root,
            uint32(2)
        );
        mailbox.expectMessage(0, 7, 8, 9, address(0x5E), data);
        ZkSysQualifiedRosterReceiverV1 wrongClock = new ZkSysQualifiedRosterReceiverV1(
            ZkSysQualifiedRosterReceiverV1.Configuration({
                childChainId: block.chainid,
                childRegistry: address(0x5E),
                messageSender: address(0x5E),
                childMailbox: IZkSysChildMailboxV1(address(mailbox)),
                policyHash: POLICY,
                startTime: 2_001,
                periodSeconds: 1_000,
                firstServicePeriod: 0
            })
        );
        vm.expectRevert(ZkSysQualifiedRosterReceiverV1.MessageNotIncluded.selector);
        wrongClock.relayRoster(0, root, 2, inclusion);
        receiver.relayRoster(0, root, 2, inclusion);
        (bytes32 actual, uint32 count) = receiver.rosterFor(0);
        assertEq(actual, root);
        assertEq(count, 2);
        vm.expectRevert(ZkSysQualifiedRosterReceiverV1.ServiceNotStarted.selector);
        receiver.currentRoster();
        vm.warp(2_000);
        (uint64 period, bytes32 currentRoot,) = receiver.currentRoster();
        assertEq(period, 0);
        assertEq(currentRoot, root);
        vm.expectRevert(ZkSysQualifiedRosterReceiverV1.RosterConflict.selector);
        receiver.relayRoster(0, root, 3, inclusion);
    }

    function testCoordinatorRejectsMismatchedFirstServicePeriod() public {
        ZkSysWrapperCoordinatorV1.Configuration memory config = ZkSysWrapperCoordinatorV1.Configuration({
            acceptanceGate: address(gate),
            rootSource: roots,
            rosterSource: rosters,
            sequencer: vm.addr(SEQUENCER_KEY),
            childChainId: block.chainid,
            childChainAddress: CHAIN,
            policyHash: POLICY,
            productionVkHash: VK,
            initialParent: keccak256("genesis-parent"),
            firstBatch: 1,
            firstServicePeriod: 1,
            turnSeconds: 10,
            nativeRootDraw: false
        });
        vm.expectRevert(ZkSysWrapperCoordinatorV1.InvalidConfiguration.selector);
        new ZkSysWrapperCoordinatorV1(config);
    }
}
