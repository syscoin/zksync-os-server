// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";
import {EIP712} from "@openzeppelin/contracts-v4/utils/cryptography/EIP712.sol";
import {
    ProverSubscriptionV1,
    AcceptedPackageV1,
    DutySuccessV1,
    WrapperCandidateV1,
    ZkSysServiceTypesV1
} from "contracts/src/zksys/ZkSysServiceTypesV1.sol";
import {ZkSysProofGateV1} from "contracts/src/zksys/ZkSysProofGateV1.sol";
import {ZkSysWrapperCoordinatorV1} from "contracts/src/zksys/ZkSysWrapperCoordinatorV1.sol";

contract ServiceDomainDigestV1Harness is EIP712 {
    constructor(string memory name) EIP712(name, "1") {}

    function digest(bytes32 structHash) external view returns (bytes32) {
        return _hashTypedDataV4(structHash);
    }
}

/// @dev Rust and Python read the same fixture; Solidity supplies an independent ABI implementation.
contract ZkSysServiceEncodingV1Test is Test {
    string private vector;

    function setUp() public {
        vector = vm.readFile("../../scripts/prover-service/vector.json");
    }

    function testSharedStructAndDynamicReportVectors() public view {
        ProverSubscriptionV1 memory subscription = _subscription(vector, ".subscription");
        AcceptedPackageV1 memory accepted = _package(vector, ".accepted_package");
        DutySuccessV1 memory duty = _duty(vector, ".duty");
        DutySuccessV1[] memory duties = new DutySuccessV1[](2);
        duties[0] = _duty(vector, ".duties[0]");
        duties[1] = _duty(vector, ".duties[1]");
        assertEq(ZkSysServiceTypesV1.hashSubscription(subscription), _hash("subscription"));
        assertEq(ZkSysServiceTypesV1.hashPackage(accepted), _hash("package"));
        assertEq(ZkSysServiceTypesV1.hashDuty(duty), _hash("duty"));
        assertEq(ZkSysServiceTypesV1.hashReport(duties), _hash("report"));
        assertEq(ZkSysServiceTypesV1.wrapperLeaf(_candidate(vector, ".candidate")), _hash("wrapper_leaf"));
    }

    function testSharedEip712Domains() public {
        address registry = vm.parseJsonAddress(vector, ".config.registry");
        address coordinator = vm.parseJsonAddress(vector, ".config.coordinator");
        address gate = vm.parseJsonAddress(vector, ".config.proof_gate");
        ServiceDomainDigestV1Harness service = new ServiceDomainDigestV1Harness("ZkSysProverService");
        ServiceDomainDigestV1Harness wrapper = new ServiceDomainDigestV1Harness("ZkSysWrapperCoordinator");
        ServiceDomainDigestV1Harness bootstrap = new ServiceDomainDigestV1Harness("ZkSysProofGate");
        // Etching at fixture addresses forces OpenZeppelin to rebuild the domain for each target.
        vm.etch(registry, address(service).code);
        vm.etch(coordinator, address(wrapper).code);
        vm.etch(gate, address(bootstrap).code);
        vm.chainId(vm.parseUint(vm.parseJsonString(vector, ".config.registry_chain_id")));
        assertEq(ServiceDomainDigestV1Harness(registry).digest(_hash("subscription")), _hash("subscription_digest"));
        assertEq(ServiceDomainDigestV1Harness(registry).digest(_hash("duty")), _hash("duty_digest"));
        vm.chainId(vm.parseUint(vm.parseJsonString(vector, ".config.settlement_chain_id")));
        assertEq(ServiceDomainDigestV1Harness(coordinator).digest(_hash("package")), _hash("coordinator_digest"));
        assertEq(ServiceDomainDigestV1Harness(gate).digest(_hash("package")), _hash("bootstrap_digest"));
    }

    function testSharedNativeProofAndStatementVectors() public view {
        ZkSysProofGateV1.BatchOutput memory output = _output(vector, ".batches[0].output");
        bytes32 outputHash = keccak256(
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
        bytes32 config = keccak256(
            abi.encode(
                vm.parseUint(vm.parseJsonString(vector, ".config.execution_chain_id")), uint256(0), uint256(1 << 24)
            )
        );
        assertEq(outputHash, _hash("batch_output"));
        assertEq(config, _hash("chain_config"));
        assertEq(
            keccak256(
                abi.encodePacked(
                    vm.parseJsonBytes32(vector, ".previous_batch.batchHash"),
                    vm.parseJsonBytes32(vector, ".batches[0].stored.batchHash"),
                    config,
                    outputHash
                )
            ),
            _hash("statement")
        );

        bytes memory encoded = vm.parseJsonBytes(vector, ".proof_data");
        assertEq(uint8(encoded[0]), 1);
        bytes memory body = new bytes(encoded.length - 1);
        for (uint256 i; i < body.length; ++i) {
            body[i] = encoded[i + 1];
        }
        (
            ZkSysProofGateV1.StoredBatch memory previous,
            ZkSysProofGateV1.StoredBatch[] memory batches,
            uint256[] memory proof
        ) = abi.decode(body, (ZkSysProofGateV1.StoredBatch, ZkSysProofGateV1.StoredBatch[], uint256[]));
        assertEq(body, abi.encode(previous, batches, proof));
        assertEq(keccak256(abi.encode(previous)), keccak256(abi.encode(_stored(vector, ".previous_batch"))));
        assertEq(batches.length, 2);
        assertEq(keccak256(abi.encode(batches[0])), keccak256(abi.encode(_stored(vector, ".batches[0].stored"))));
        assertEq(keccak256(abi.encode(batches[1])), keccak256(abi.encode(_stored(vector, ".batches[1].stored"))));
        assertEq(proof.length, 46);
        assertEq(proof[0], 0x802);
        assertEq(proof[1], 0);
    }

    function testSharedGateSelectors() public view {
        assertEq(ZkSysProofGateV1.submit.selector, bytes4(vm.parseJsonBytes(vector, ".selectors.submit")));
        assertEq(
            ZkSysProofGateV1.submitBootstrap.selector, bytes4(vm.parseJsonBytes(vector, ".selectors.submitBootstrap"))
        );
        assertEq(ZkSysProofGateV1.repairPackage.selector, bytes4(vm.parseJsonBytes(vector, ".selectors.repairPackage")));
        assertEq(ZkSysWrapperCoordinatorV1.repairPackage.selector, ZkSysProofGateV1.repairPackage.selector);
    }

    function _hash(string memory name) private view returns (bytes32) {
        return vm.parseJsonBytes32(vector, string.concat(".hashes.", name));
    }

    function _subscription(string memory json, string memory prefix)
        private
        pure
        returns (ProverSubscriptionV1 memory value)
    {
        value.account = vm.parseJsonAddress(json, string.concat(prefix, ".account"));
        value.operator = vm.parseJsonAddress(json, string.concat(prefix, ".operator"));
        value.beneficiary = vm.parseJsonAddress(json, string.concat(prefix, ".beneficiary"));
        value.sequencer = vm.parseJsonAddress(json, string.concat(prefix, ".sequencer"));
        value.firstPeriod = uint64(vm.parseJsonUint(json, string.concat(prefix, ".firstPeriod")));
        value.lastPeriod = uint64(vm.parseJsonUint(json, string.concat(prefix, ".lastPeriod")));
        value.nonce = uint64(vm.parseJsonUint(json, string.concat(prefix, ".nonce")));
        value.services = uint8(vm.parseJsonUint(json, string.concat(prefix, ".services")));
    }

    function _package(string memory json, string memory prefix) private pure returns (AcceptedPackageV1 memory value) {
        value.domainVersion = uint32(vm.parseJsonUint(json, string.concat(prefix, ".domainVersion")));
        value.policyHash = vm.parseJsonBytes32(json, string.concat(prefix, ".policyHash"));
        value.chainId = vm.parseUint(vm.parseJsonString(json, string.concat(prefix, ".chainId")));
        value.chainAddress = vm.parseJsonAddress(json, string.concat(prefix, ".chainAddress"));
        value.parent = vm.parseJsonBytes32(json, string.concat(prefix, ".parent"));
        value.batchFrom = uint64(vm.parseJsonUint(json, string.concat(prefix, ".batchFrom")));
        value.batchTo = uint64(vm.parseJsonUint(json, string.concat(prefix, ".batchTo")));
        value.protocolVersion = uint32(vm.parseJsonUint(json, string.concat(prefix, ".protocolVersion")));
        value.vkHash = vm.parseJsonBytes32(json, string.concat(prefix, ".vkHash"));
        value.period = uint64(vm.parseJsonUint(json, string.concat(prefix, ".period")));
        value.rosterRoot = vm.parseJsonBytes32(json, string.concat(prefix, ".rosterRoot"));
        value.turn = uint32(vm.parseJsonUint(json, string.concat(prefix, ".turn")));
        value.manifestHash = vm.parseJsonBytes32(json, string.concat(prefix, ".manifestHash"));
        value.reportHash = vm.parseJsonBytes32(json, string.concat(prefix, ".reportHash"));
        value.proofHash = vm.parseJsonBytes32(json, string.concat(prefix, ".proofHash"));
        value.sequencer = vm.parseJsonAddress(json, string.concat(prefix, ".sequencer"));
        value.sequencerBeneficiary = vm.parseJsonAddress(json, string.concat(prefix, ".sequencerBeneficiary"));
        value.wrapper = vm.parseJsonAddress(json, string.concat(prefix, ".wrapper"));
        value.wrapperBeneficiary = vm.parseJsonAddress(json, string.concat(prefix, ".wrapperBeneficiary"));
    }

    function _duty(string memory json, string memory prefix) private pure returns (DutySuccessV1 memory value) {
        value.account = vm.parseJsonAddress(json, string.concat(prefix, ".account"));
        value.subscriptionHash = vm.parseJsonBytes32(json, string.concat(prefix, ".subscriptionHash"));
        value.batchNumber = uint64(vm.parseJsonUint(json, string.concat(prefix, ".batchNumber")));
        value.statementHash = vm.parseJsonBytes32(json, string.concat(prefix, ".statementHash"));
        value.friProofHash = vm.parseJsonBytes32(json, string.concat(prefix, ".friProofHash"));
        value.transactionCount = uint64(vm.parseJsonUint(json, string.concat(prefix, ".transactionCount")));
        value.period = uint64(vm.parseJsonUint(json, string.concat(prefix, ".period")));
        value.slot = uint16(vm.parseJsonUint(json, string.concat(prefix, ".slot")));
        value.attempt = uint32(vm.parseJsonUint(json, string.concat(prefix, ".attempt")));
        value.assignmentId = vm.parseJsonBytes32(json, string.concat(prefix, ".assignmentId"));
        value.operatorSignature = vm.parseJsonBytes(json, string.concat(prefix, ".operatorSignature"));
    }

    function _candidate(string memory json, string memory prefix)
        private
        pure
        returns (WrapperCandidateV1 memory value)
    {
        value.index = uint32(vm.parseJsonUint(json, string.concat(prefix, ".index")));
        value.account = vm.parseJsonAddress(json, string.concat(prefix, ".account"));
        value.operator = vm.parseJsonAddress(json, string.concat(prefix, ".operator"));
        value.beneficiary = vm.parseJsonAddress(json, string.concat(prefix, ".beneficiary"));
    }

    function _output(string memory json, string memory prefix)
        private
        pure
        returns (ZkSysProofGateV1.BatchOutput memory value)
    {
        value.firstBlockTimestamp = uint64(vm.parseJsonUint(json, string.concat(prefix, ".firstBlockTimestamp")));
        value.lastBlockTimestamp = uint64(vm.parseJsonUint(json, string.concat(prefix, ".lastBlockTimestamp")));
        value.daScheme = vm.parseUint(vm.parseJsonString(json, string.concat(prefix, ".daScheme")));
        value.daCommitment = vm.parseJsonBytes32(json, string.concat(prefix, ".daCommitment"));
        value.l1TxCount = vm.parseUint(vm.parseJsonString(json, string.concat(prefix, ".l1TxCount")));
        value.l2TxCount = vm.parseUint(vm.parseJsonString(json, string.concat(prefix, ".l2TxCount")));
        value.priorityOperationsHash = vm.parseJsonBytes32(json, string.concat(prefix, ".priorityOperationsHash"));
        value.l2LogsRoot = vm.parseJsonBytes32(json, string.concat(prefix, ".l2LogsRoot"));
        value.upgradeTxHash = vm.parseJsonBytes32(json, string.concat(prefix, ".upgradeTxHash"));
        value.dependencyRootsRollingHash =
            vm.parseJsonBytes32(json, string.concat(prefix, ".dependencyRootsRollingHash"));
        value.settlementChainId = vm.parseUint(vm.parseJsonString(json, string.concat(prefix, ".settlementChainId")));
        value.edgeDARefsRoot = vm.parseJsonBytes32(json, string.concat(prefix, ".edgeDARefsRoot"));
    }

    function _stored(string memory json, string memory prefix)
        private
        pure
        returns (ZkSysProofGateV1.StoredBatch memory value)
    {
        value.batchNumber = uint64(vm.parseJsonUint(json, string.concat(prefix, ".batchNumber")));
        value.batchHash = vm.parseJsonBytes32(json, string.concat(prefix, ".batchHash"));
        value.indexRepeatedStorageChanges =
            uint64(vm.parseJsonUint(json, string.concat(prefix, ".indexRepeatedStorageChanges")));
        value.numberOfLayer1Txs = vm.parseUint(vm.parseJsonString(json, string.concat(prefix, ".numberOfLayer1Txs")));
        value.priorityOperationsHash = vm.parseJsonBytes32(json, string.concat(prefix, ".priorityOperationsHash"));
        value.dependencyRootsRollingHash =
            vm.parseJsonBytes32(json, string.concat(prefix, ".dependencyRootsRollingHash"));
        value.l2LogsTreeRoot = vm.parseJsonBytes32(json, string.concat(prefix, ".l2LogsTreeRoot"));
        value.timestamp = vm.parseUint(vm.parseJsonString(json, string.concat(prefix, ".timestamp")));
        value.commitment = vm.parseJsonBytes32(json, string.concat(prefix, ".commitment"));
    }
}
