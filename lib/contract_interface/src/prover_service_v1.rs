//! Portable encodings for the opt-in V1 prover service contracts.
//!
//! A sidecar carries endorsements and their native batch evidence; it never establishes proof
//! validity. The gate checks the live settlement configuration and invokes the native verifier.

use alloy::primitives::{Address, B256, Bytes, U256, keccak256};
use alloy::sol_types::{Eip712Domain, SolValue};
use serde::{Deserialize, Serialize};

pub const DOMAIN_VERSION: u32 = 1;
pub const PROTOCOL_VERSION: u32 = 32;
pub const MAX_BATCHES_PER_PACKAGE: u64 = 100;
pub const V8_PROOF_TYPE: u64 = 0x802;
pub const V8_PROOF_WORDS: usize = 46;

const SUBSCRIPTION_TYPE: &str = "ProverSubscriptionV1(address account,address operator,address beneficiary,address sequencer,uint64 firstPeriod,uint64 lastPeriod,uint64 nonce,uint8 services)";
const DUTY_TYPE: &str = "DutySuccessV1(address account,bytes32 subscriptionHash,uint64 batchNumber,bytes32 statementHash,bytes32 friProofHash,uint64 transactionCount,uint64 period,uint16 slot,uint32 attempt,bytes32 assignmentId)";
const PACKAGE_TYPE: &str = "AcceptedPackageV1(uint32 domainVersion,bytes32 policyHash,uint256 chainId,address chainAddress,bytes32 parent,uint64 batchFrom,uint64 batchTo,uint32 protocolVersion,bytes32 vkHash,uint64 period,bytes32 rosterRoot,uint32 turn,bytes32 manifestHash,bytes32 reportHash,bytes32 proofHash,address sequencer,address sequencerBeneficiary,address wrapper,address wrapperBeneficiary)";

alloy::sol! {
    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
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

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
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

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
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

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct WrapperCandidateV1 {
        uint32 index;
        address account;
        address operator;
        address beneficiary;
    }

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
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

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
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

    #[sol(rpc)]
    #[allow(clippy::too_many_arguments)]
    interface IZkSysProofGateV1 {
        function chain() external view returns (address);
        function timelock() external view returns (address);
        function sequencer() external view returns (address);
        function childChainId() external view returns (uint256);
        function policyHash() external view returns (bytes32);
        function productionVkHash() external view returns (bytes32);
        function productionVerifier() external view returns (address);
        function verifierCodeHash() external view returns (bytes32);
        function chainConfigHash() external view returns (bytes32);
        function coordinator() external view returns (address);
        function lastAcceptedPackage() external view returns (bytes32);
        function serviceActive() external view returns (bool);
        function bootstrapDigest(AcceptedPackageV1 accepted) external view returns (bytes32);
        function installCoordinator(address coordinator_) external;
        function activateService() external;
        function openPackage(AcceptedPackageV1 proposed) external;
        function repairPackage(AcceptedPackageV1 proposed) external;
        function submitBootstrap(
            AcceptedPackageV1 accepted, DutySuccessV1[] duties, BatchOutput[] outputs,
            bytes proofData, bytes sequencerSignature
        ) external;
        function submit(
            AcceptedPackageV1 accepted, DutySuccessV1[] duties, BatchOutput[] outputs,
            bytes proofData, WrapperCandidateV1 candidate, bytes32[] candidateProof,
            bytes sequencerSignature, bytes wrapperSignature
        ) external;
        event ServicePackageAccepted(bytes32 indexed packageHash, uint64 indexed from, uint64 indexed to, bool bootstrap);
        event ServiceActivated(address indexed coordinator, bytes32 qualifiedRosterRoot);
    }

    #[sol(rpc)]
    interface IZkSysWrapperCoordinatorV1 {
        function acceptanceGate() external view returns (address);
        function rootSource() external view returns (address);
        function rosterSource() external view returns (address);
        function sequencer() external view returns (address);
        function childChainId() external view returns (uint256);
        function childChainAddress() external view returns (address);
        function policyHash() external view returns (bytes32);
        function productionVkHash() external view returns (bytes32);
        function turnSeconds() external view returns (uint64);
        function firstServicePeriod() external view returns (uint64);
        function acceptedParent() external view returns (bytes32);
        function nextBatch() external view returns (uint64);
        function packageOpen() external view returns (bool);
        function frozenPackageHash() external view returns (bytes32);
        function rootHeight() external view returns (uint64);
        function turnsStartedAt() external view returns (uint64);
        function firstWrapperIndex() external view returns (uint32);
        function rosterCount() external view returns (uint32);
        function rosterRoot() external view returns (bytes32);
        function packageOrdinal() external view returns (uint64);
        function prepareRosterDraw(uint64 period) external returns (bytes32 commitment);
        function recordRosterDraw(uint64 period) external;
        function rosterDrawCommitment(uint64 period) external view returns (bytes32);
        function rosterSeed(uint64 period) external view returns (bytes32);
        function currentTurn() external view returns (uint32);
        function selectedWrapperIndex() external view returns (uint32);
        function turnDeadline() external view returns (uint256);
        function packageDigest(AcceptedPackageV1 accepted) external view returns (bytes32);
        function frozenPackage() external view returns (AcceptedPackageV1);
        function openPackage(AcceptedPackageV1 proposed) external;
        function repairPackage(AcceptedPackageV1 proposed) external;
        function acceptPackage(
            AcceptedPackageV1 accepted, WrapperCandidateV1 candidate, bytes32[] candidateProof,
            bytes sequencerSignature, bytes wrapperSignature
        ) external returns (bytes32 packageHash);
        event PackageOpened(bytes32 indexed packageHash, uint64 indexed rootHeight, bytes32 rosterRoot, uint32 rosterCount);
        event WrapperTurnsStarted(bytes32 indexed packageHash, uint32 firstWrapperIndex, uint64 startedAt);
        event RosterDrawPrepared(uint64 indexed period, bytes32 indexed commitment, bytes32 root, uint32 count);
        event RosterDrawRecorded(uint64 indexed period, uint64 rootHeight, bytes32 seed);
        event PackageAccepted(bytes32 indexed packageHash, uint64 indexed batchTo, address indexed wrapper, uint32 turn);
        event PackageRepaired(bytes32 indexed previousHash, bytes32 indexed replacementHash);
    }
}

pub fn hash_subscription(subscription: &ProverSubscriptionV1) -> B256 {
    keccak256((keccak256(SUBSCRIPTION_TYPE), subscription.clone()).abi_encode())
}

pub fn hash_duty(duty: &DutySuccessV1) -> B256 {
    // The operator signature authenticates this struct and is included only in the report hash.
    keccak256(
        (
            keccak256(DUTY_TYPE),
            duty.account,
            duty.subscriptionHash,
            duty.batchNumber,
            duty.statementHash,
            duty.friProofHash,
            duty.transactionCount,
            duty.period,
            duty.slot,
            duty.attempt,
            duty.assignmentId,
        )
            .abi_encode(),
    )
}

pub fn hash_package(accepted: &AcceptedPackageV1) -> B256 {
    keccak256((keccak256(PACKAGE_TYPE), accepted.clone()).abi_encode())
}

pub fn hash_report(duties: &[DutySuccessV1]) -> B256 {
    keccak256(duties.abi_encode())
}

pub fn wrapper_leaf(candidate: &WrapperCandidateV1) -> B256 {
    keccak256(keccak256(candidate.abi_encode()))
}

fn typed_digest(name: &'static str, chain_id: U256, contract: Address, struct_hash: B256) -> B256 {
    let domain = Eip712Domain {
        name: Some(name.into()),
        version: Some("1".into()),
        chain_id: Some(chain_id),
        verifying_contract: Some(contract),
        salt: None,
    };
    keccak256(
        (
            alloy::primitives::FixedBytes::<2>::from([0x19, 0x01]),
            domain.separator(),
            struct_hash,
        )
            .abi_encode_packed(),
    )
}

pub fn bootstrap_digest(
    accepted: &AcceptedPackageV1,
    settlement_chain_id: U256,
    gate: Address,
) -> B256 {
    typed_digest(
        "ZkSysProofGate",
        settlement_chain_id,
        gate,
        hash_package(accepted),
    )
}

pub fn package_digest(
    accepted: &AcceptedPackageV1,
    settlement_chain_id: U256,
    coordinator: Address,
) -> B256 {
    typed_digest(
        "ZkSysWrapperCoordinator",
        settlement_chain_id,
        coordinator,
        hash_package(accepted),
    )
}

pub fn duty_digest(duty: &DutySuccessV1, reward_chain_id: U256, registry: Address) -> B256 {
    typed_digest(
        "ZkSysProverService",
        reward_chain_id,
        registry,
        hash_duty(duty),
    )
}

pub fn subscription_digest(
    subscription: &ProverSubscriptionV1,
    reward_chain_id: U256,
    registry: Address,
) -> B256 {
    typed_digest(
        "ZkSysProverService",
        reward_chain_id,
        registry,
        hash_subscription(subscription),
    )
}

pub fn chain_config_hash(child_chain_id: U256) -> B256 {
    keccak256((child_chain_id, U256::ZERO, U256::from(1_u64 << 24)).abi_encode())
}

pub fn batch_output_preimage(output: &BatchOutput) -> Vec<u8> {
    // The timestamps occupy eight bytes each; all subsequent fields occupy 32 bytes.
    (
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
        output.edgeDARefsRoot,
    )
        .abi_encode_packed()
}

pub fn batch_output_hash(output: &BatchOutput) -> B256 {
    keccak256(batch_output_preimage(output))
}

/// Full native batch statement, before any wrapper public-input truncation.
pub fn public_statement_hash(
    previous_state: B256,
    new_state: B256,
    config_hash: B256,
    output_hash: B256,
) -> B256 {
    keccak256((previous_state, new_state, config_hash, output_hash).abi_encode_packed())
}

/// JSON uses snake_case envelope keys and the Solidity field names inside each ABI struct.
/// Byte strings, addresses, hashes and U256 values use Alloy's hexadecimal serde encodings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProverServiceSidecarV1 {
    pub accepted_package: AcceptedPackageV1,
    pub duties: Vec<DutySuccessV1>,
    pub batch_outputs: Vec<BatchOutput>,
    pub candidate: Option<WrapperCandidateV1>,
    pub candidate_proof: Vec<B256>,
    pub sequencer_signature: Bytes,
    pub wrapper_signature: Bytes,
}

/// Values read from the pinned contracts and the actual proof-sender batch range.
#[derive(Clone, Debug)]
pub struct ValidationContextV1 {
    pub child_chain_id: U256,
    pub child_chain_address: Address,
    pub settlement_chain_id: U256,
    pub policy_hash: B256,
    pub production_vk_hash: B256,
    pub sequencer: Address,
    pub expected_parent: B256,
    pub batch_from: u64,
    pub batch_to: u64,
    pub phase: PackagePhaseV1,
}

#[derive(Clone, Debug)]
pub enum PackagePhaseV1 {
    Bootstrap,
    Service {
        period: u64,
        roster_root: B256,
        selected_wrapper_index: u32,
        turn: u32,
        frozen_package_hash: B256,
    },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("invalid prover-service configuration: {0}")]
    Configuration(&'static str),
    #[error("invalid prover-service package: {0}")]
    Package(&'static str),
    #[error("invalid native V8 proof data: {0}")]
    ProofData(&'static str),
    #[error("invalid native batch output at index {0}")]
    BatchOutput(usize),
    #[error("invalid prover duty at index {0}")]
    Duty(usize),
    #[error("invalid wrapper candidate or roster proof")]
    Candidate,
}

impl ProverServiceSidecarV1 {
    /// Checks deterministic evidence and commitments. Signature validity (including ERC-1271),
    /// subscription eligibility, the current wrapper turn and native proof validity remain on chain.
    pub fn validate(
        &self,
        context: &ValidationContextV1,
        proof_data: &[u8],
    ) -> Result<(), ValidationError> {
        use ValidationError::{Configuration, Package, ProofData};
        if context.child_chain_id.is_zero()
            || context.settlement_chain_id.is_zero()
            || context.child_chain_address.is_zero()
            || context.sequencer.is_zero()
            || context.policy_hash.is_zero()
            || context.production_vk_hash.is_zero()
            || context.expected_parent.is_zero()
        {
            return Err(Configuration(
                "zero deployment identity, policy, parent or VK",
            ));
        }
        let accepted = &self.accepted_package;
        if accepted.domainVersion != DOMAIN_VERSION || accepted.protocolVersion != PROTOCOL_VERSION
        {
            return Err(Package("unsupported domain or protocol version"));
        }
        if accepted.chainId != context.child_chain_id
            || accepted.chainAddress != context.child_chain_address
            || accepted.policyHash != context.policy_hash
            || accepted.vkHash != context.production_vk_hash
            || accepted.sequencer != context.sequencer
            || accepted.parent != context.expected_parent
        {
            return Err(Package(
                "deployment identity, parent, policy or VK mismatch",
            ));
        }
        if accepted.batchFrom == 0
            || accepted.batchTo <= accepted.batchFrom
            || accepted.batchTo == u64::MAX
            || accepted.batchTo - accepted.batchFrom >= MAX_BATCHES_PER_PACKAGE
            || accepted.batchFrom != context.batch_from
            || accepted.batchTo != context.batch_to
        {
            return Err(Package("expected consecutive range of 2 to 100 batches"));
        }
        if accepted.manifestHash.is_zero()
            || accepted.sequencerBeneficiary.is_zero()
            || accepted.proofHash != keccak256(proof_data)
            || accepted.reportHash != hash_report(&self.duties)
        {
            return Err(Package(
                "missing beneficiary or mismatched manifest, report or proof commitment",
            ));
        }
        self.validate_phase(&context.phase)?;
        if proof_data.first() != Some(&1) {
            return Err(ProofData("unsupported encoding version"));
        }
        let decoded = <(StoredBatch, Vec<StoredBatch>, Vec<U256>)>::abi_decode_params_validate(
            &proof_data[1..],
        )
        .map_err(|_| ProofData("malformed ABI payload"))?;
        if decoded.abi_encode_params() != proof_data[1..] {
            return Err(ProofData("noncanonical ABI payload"));
        }
        let (mut previous, batches, proof) = decoded;
        if batches.len() != (accepted.batchTo - accepted.batchFrom + 1) as usize
            || self.batch_outputs.len() != batches.len()
            || self.duties.len() > batches.len()
            || proof.len() != V8_PROOF_WORDS
            || proof[0] != U256::from(V8_PROOF_TYPE)
            || !proof[1].is_zero()
            || previous.batchNumber != accepted.batchFrom - 1
        {
            return Err(ProofData("wrong batch range, evidence length or V8 header"));
        }
        let config_hash = chain_config_hash(context.child_chain_id);
        let mut statements = Vec::with_capacity(batches.len());
        for (index, (batch, output)) in batches.iter().zip(&self.batch_outputs).enumerate() {
            if batch.batchNumber != accepted.batchFrom + index as u64
                || batch_output_hash(output) != batch.commitment
                || output.settlementChainId != context.settlement_chain_id
                || batch.numberOfLayer1Txs != output.l1TxCount
                || batch.priorityOperationsHash != output.priorityOperationsHash
                || batch.dependencyRootsRollingHash != output.dependencyRootsRollingHash
                || batch.l2LogsTreeRoot != output.l2LogsRoot
            {
                return Err(ValidationError::BatchOutput(index));
            }
            statements.push(public_statement_hash(
                previous.batchHash,
                batch.batchHash,
                config_hash,
                batch.commitment,
            ));
            previous = batch.clone();
        }
        let mut seen = 0_u128;
        for (index, duty) in self.duties.iter().enumerate() {
            if duty.batchNumber < accepted.batchFrom || duty.batchNumber > accepted.batchTo {
                return Err(ValidationError::Duty(index));
            }
            let batch_index = (duty.batchNumber - accepted.batchFrom) as usize;
            let bit = 1_u128 << batch_index;
            let output = &self.batch_outputs[batch_index];
            let count = output.l1TxCount.checked_add(output.l2TxCount);
            if seen & bit != 0
                || duty.transactionCount == 0
                || count != Some(U256::from(duty.transactionCount))
                || duty.statementHash != statements[batch_index]
                || duty.period != accepted.period
                || duty.friProofHash.is_zero()
                || duty.account.is_zero()
                || duty.subscriptionHash.is_zero()
                || duty.assignmentId.is_zero()
                || duty.attempt == 0
                || duty.slot >= 64
            {
                return Err(ValidationError::Duty(index));
            }
            seen |= bit;
        }
        Ok(())
    }

    fn validate_phase(&self, phase: &PackagePhaseV1) -> Result<(), ValidationError> {
        let accepted = &self.accepted_package;
        match phase {
            PackagePhaseV1::Bootstrap => {
                if !accepted.wrapper.is_zero()
                    || !accepted.wrapperBeneficiary.is_zero()
                    || !accepted.rosterRoot.is_zero()
                    || accepted.turn != 0
                    || self.candidate.is_some()
                    || !self.candidate_proof.is_empty()
                    || !self.wrapper_signature.is_empty()
                {
                    return Err(ValidationError::Package(
                        "bootstrap contains wrapper authorization",
                    ));
                }
            }
            PackagePhaseV1::Service {
                period,
                roster_root,
                selected_wrapper_index,
                turn,
                frozen_package_hash,
            } => {
                let candidate = self.candidate.as_ref().ok_or(ValidationError::Candidate)?;
                if roster_root.is_zero()
                    || frozen_package_hash.is_zero()
                    || accepted.period != *period
                    || accepted.rosterRoot != *roster_root
                    || accepted.turn != *turn
                    || candidate.index != *selected_wrapper_index
                    || candidate.account.is_zero()
                    || candidate.operator.is_zero()
                    || candidate.operator == accepted.sequencer
                    || candidate.beneficiary.is_zero()
                    || accepted.wrapper != candidate.operator
                    || accepted.wrapperBeneficiary != candidate.beneficiary
                    || self.candidate_proof.len() > 32
                {
                    return Err(ValidationError::Candidate);
                }
                let root =
                    self.candidate_proof
                        .iter()
                        .fold(wrapper_leaf(candidate), |hash, sibling| {
                            let pair = if hash <= *sibling {
                                (hash, *sibling)
                            } else {
                                (*sibling, hash)
                            };
                            keccak256(pair.abi_encode_packed())
                        });
                if root != *roster_root {
                    return Err(ValidationError::Candidate);
                }
                let mut normalized = accepted.clone();
                normalized.turn = 0;
                normalized.proofHash = B256::ZERO;
                normalized.wrapper = Address::ZERO;
                normalized.wrapperBeneficiary = Address::ZERO;
                if hash_package(&normalized) != *frozen_package_hash {
                    return Err(ValidationError::Package(
                        "does not match the frozen package",
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::sol_types::SolCall;

    fn vector_value<T: serde::de::DeserializeOwned>(value: &serde_json::Value) -> T {
        serde_json::from_value(value.clone()).unwrap()
    }

    #[test]
    fn shared_cast_and_solidity_golden_vectors() {
        // The same fixture is checked against Solidity's ABI and OpenZeppelin EIP-712 code.
        let vector: serde_json::Value =
            serde_json::from_str(include_str!("../../../scripts/prover-service/vector.json"))
                .unwrap();
        let subscription: ProverSubscriptionV1 = vector_value(&vector["subscription"]);
        let duty: DutySuccessV1 = vector_value(&vector["duty"]);
        let package: AcceptedPackageV1 = vector_value(&vector["accepted_package"]);
        let duties: Vec<DutySuccessV1> = vector_value(&vector["duties"]);
        let candidate: WrapperCandidateV1 = vector_value(&vector["candidate"]);
        let previous: StoredBatch = vector_value(&vector["previous_batch"]);
        let stored: StoredBatch = vector_value(&vector["batches"][0]["stored"]);
        let output: BatchOutput = vector_value(&vector["batches"][0]["output"]);
        let config = &vector["config"];
        let child: U256 = vector_value(&config["execution_chain_id"]);
        let registry_chain: U256 = vector_value(&config["registry_chain_id"]);
        let settlement: U256 = vector_value(&config["settlement_chain_id"]);
        let registry: Address = vector_value(&config["registry"]);
        let expected = |name: &str| vector_value::<B256>(&vector["hashes"][name]);
        assert_eq!(hash_subscription(&subscription), expected("subscription"));
        assert_eq!(
            subscription_digest(&subscription, registry_chain, registry),
            expected("subscription_digest")
        );
        assert_eq!(hash_duty(&duty), expected("duty"));
        assert_eq!(
            duty_digest(&duty, registry_chain, registry),
            expected("duty_digest")
        );
        assert_eq!(hash_package(&package), expected("package"));
        assert_eq!(
            package_digest(&package, settlement, vector_value(&config["coordinator"])),
            expected("coordinator_digest")
        );
        assert_eq!(
            bootstrap_digest(&package, settlement, vector_value(&config["proof_gate"])),
            expected("bootstrap_digest")
        );
        assert_eq!(hash_report(&duties), expected("report"));
        assert_eq!(wrapper_leaf(&candidate), expected("wrapper_leaf"));
        assert_eq!(batch_output_hash(&output), expected("batch_output"));
        assert_eq!(chain_config_hash(child), expected("chain_config"));
        assert_eq!(
            public_statement_hash(
                previous.batchHash,
                stored.batchHash,
                chain_config_hash(child),
                stored.commitment
            ),
            expected("statement")
        );
        for (selector, name) in [
            (IZkSysProofGateV1::submitCall::SELECTOR, "submit"),
            (
                IZkSysProofGateV1::submitBootstrapCall::SELECTOR,
                "submitBootstrap",
            ),
            (
                IZkSysProofGateV1::repairPackageCall::SELECTOR,
                "repairPackage",
            ),
        ] {
            assert_eq!(
                format!("0x{}", alloy::hex::encode(selector)),
                vector["selectors"][name]
            );
        }
        let sidecar: ProverServiceSidecarV1 = vector_value(&vector["sidecar"]);
        let proof_data: Bytes = vector_value(&vector["proof_data"]);
        let mut frozen = package.clone();
        frozen.turn = 0;
        frozen.proofHash = B256::ZERO;
        frozen.wrapper = Address::ZERO;
        frozen.wrapperBeneficiary = Address::ZERO;
        let context = ValidationContextV1 {
            child_chain_id: child,
            child_chain_address: vector_value(&config["chain_address"]),
            settlement_chain_id: settlement,
            policy_hash: vector_value(&config["policy_hash"]),
            production_vk_hash: vector_value(&config["vk_hash"]),
            sequencer: vector_value(&config["sequencer"]),
            expected_parent: package.parent,
            batch_from: package.batchFrom,
            batch_to: package.batchTo,
            phase: PackagePhaseV1::Service {
                period: package.period,
                roster_root: package.rosterRoot,
                selected_wrapper_index: candidate.index,
                turn: package.turn,
                frozen_package_hash: hash_package(&frozen),
            },
        };
        sidecar.validate(&context, &proof_data).unwrap();
    }

    fn evidence() -> (ProverServiceSidecarV1, ValidationContextV1, Vec<u8>) {
        let context = ValidationContextV1 {
            child_chain_id: U256::from(270),
            child_chain_address: Address::repeat_byte(1),
            settlement_chain_id: U256::from(5700),
            policy_hash: B256::repeat_byte(2),
            production_vk_hash: B256::repeat_byte(3),
            sequencer: Address::repeat_byte(4),
            expected_parent: B256::repeat_byte(5),
            batch_from: 10,
            batch_to: 11,
            phase: PackagePhaseV1::Bootstrap,
        };
        let output = BatchOutput {
            firstBlockTimestamp: 100,
            lastBlockTimestamp: 110,
            daScheme: U256::from(2),
            daCommitment: B256::repeat_byte(6),
            l1TxCount: U256::from(1),
            l2TxCount: U256::from(2),
            priorityOperationsHash: B256::repeat_byte(7),
            l2LogsRoot: B256::repeat_byte(8),
            upgradeTxHash: B256::ZERO,
            dependencyRootsRollingHash: B256::repeat_byte(9),
            settlementChainId: context.settlement_chain_id,
            edgeDARefsRoot: B256::repeat_byte(10),
        };
        let previous = StoredBatch {
            batchNumber: 9,
            batchHash: B256::repeat_byte(11),
            indexRepeatedStorageChanges: 0,
            numberOfLayer1Txs: output.l1TxCount,
            priorityOperationsHash: output.priorityOperationsHash,
            dependencyRootsRollingHash: output.dependencyRootsRollingHash,
            l2LogsTreeRoot: output.l2LogsRoot,
            timestamp: U256::ZERO,
            commitment: batch_output_hash(&output),
        };
        let mut batches = vec![previous.clone(), previous.clone()];
        batches[0].batchNumber = 10;
        batches[0].batchHash = B256::repeat_byte(12);
        batches[1].batchNumber = 11;
        batches[1].batchHash = B256::repeat_byte(13);
        let mut proof = vec![U256::ZERO; V8_PROOF_WORDS];
        proof[0] = U256::from(V8_PROOF_TYPE);
        let proof_data = [
            vec![1],
            (previous.clone(), batches.clone(), proof).abi_encode_params(),
        ]
        .concat();
        let duty = DutySuccessV1 {
            account: Address::repeat_byte(14),
            subscriptionHash: B256::repeat_byte(15),
            batchNumber: 10,
            statementHash: public_statement_hash(
                previous.batchHash,
                batches[0].batchHash,
                chain_config_hash(context.child_chain_id),
                batches[0].commitment,
            ),
            friProofHash: B256::repeat_byte(16),
            transactionCount: 3,
            period: 7,
            slot: 0,
            attempt: 1,
            assignmentId: B256::repeat_byte(17),
            operatorSignature: Bytes::from(vec![0xaa; 65]),
        };
        let accepted = AcceptedPackageV1 {
            domainVersion: DOMAIN_VERSION,
            policyHash: context.policy_hash,
            chainId: context.child_chain_id,
            chainAddress: context.child_chain_address,
            parent: context.expected_parent,
            batchFrom: context.batch_from,
            batchTo: context.batch_to,
            protocolVersion: PROTOCOL_VERSION,
            vkHash: context.production_vk_hash,
            period: 7,
            rosterRoot: B256::ZERO,
            turn: 0,
            manifestHash: B256::repeat_byte(18),
            reportHash: hash_report(std::slice::from_ref(&duty)),
            proofHash: keccak256(&proof_data),
            sequencer: context.sequencer,
            sequencerBeneficiary: Address::repeat_byte(19),
            wrapper: Address::ZERO,
            wrapperBeneficiary: Address::ZERO,
        };
        let sidecar = ProverServiceSidecarV1 {
            accepted_package: accepted,
            duties: vec![duty],
            batch_outputs: vec![output.clone(), output],
            candidate: None,
            candidate_proof: vec![],
            sequencer_signature: Bytes::from(vec![0xbb; 65]),
            wrapper_signature: Bytes::new(),
        };
        (sidecar, context, proof_data)
    }

    fn service_evidence() -> (ProverServiceSidecarV1, ValidationContextV1, Vec<u8>) {
        let (mut sidecar, mut context, proof_data) = evidence();
        let candidate = WrapperCandidateV1 {
            index: 0,
            account: Address::repeat_byte(20),
            operator: Address::repeat_byte(21),
            beneficiary: Address::repeat_byte(22),
        };
        let sibling = B256::repeat_byte(23);
        let leaf = wrapper_leaf(&candidate);
        let pair = if leaf <= sibling {
            (leaf, sibling)
        } else {
            (sibling, leaf)
        };
        let roster_root = keccak256(pair.abi_encode_packed());
        sidecar.accepted_package.rosterRoot = roster_root;
        let mut frozen = sidecar.accepted_package.clone();
        frozen.proofHash = B256::ZERO;
        context.phase = PackagePhaseV1::Service {
            period: 7,
            roster_root,
            selected_wrapper_index: 0,
            turn: 2,
            frozen_package_hash: hash_package(&frozen),
        };
        sidecar.accepted_package.turn = 2;
        sidecar.accepted_package.wrapper = candidate.operator;
        sidecar.accepted_package.wrapperBeneficiary = candidate.beneficiary;
        sidecar.candidate = Some(candidate);
        sidecar.candidate_proof = vec![sibling];
        sidecar.wrapper_signature = Bytes::from(vec![0xcc; 65]);
        (sidecar, context, proof_data)
    }

    #[test]
    fn portable_evidence_validates_for_both_phases() {
        for (sidecar, context, proof_data) in [evidence(), service_evidence()] {
            sidecar.validate(&context, &proof_data).unwrap();
            let json = serde_json::to_string(&sidecar).unwrap();
            let decoded: ProverServiceSidecarV1 = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, sidecar);
            assert_eq!(serde_json::to_string(&decoded).unwrap(), json);
        }
    }

    #[test]
    fn sidecar_rejects_unknown_fields_in_envelope_and_abi_structs() {
        let (sidecar, _, _) = evidence();
        let mut json = serde_json::to_value(sidecar).unwrap();
        json["verified"] = true.into();
        assert!(serde_json::from_value::<ProverServiceSidecarV1>(json.clone()).is_err());
        json.as_object_mut().unwrap().remove("verified");
        json["accepted_package"]["verified"] = true.into();
        assert!(serde_json::from_value::<ProverServiceSidecarV1>(json).is_err());
    }

    #[test]
    fn deployment_identity_and_protocol_are_not_optional() {
        let (sidecar, context, proof_data) = evidence();
        let mut changed = context.clone();
        changed.production_vk_hash = B256::ZERO;
        assert!(matches!(
            sidecar.validate(&changed, &proof_data),
            Err(ValidationError::Configuration(_))
        ));
        let mut changed = sidecar.clone();
        changed.accepted_package.vkHash = B256::repeat_byte(99);
        assert!(matches!(
            changed.validate(&context, &proof_data),
            Err(ValidationError::Package(_))
        ));
        changed = sidecar.clone();
        changed.accepted_package.protocolVersion = 31;
        assert!(matches!(
            changed.validate(&context, &proof_data),
            Err(ValidationError::Package(_))
        ));
        changed = sidecar.clone();
        changed.accepted_package.batchFrom = 0;
        assert!(matches!(
            changed.validate(&context, &proof_data),
            Err(ValidationError::Package(_))
        ));
        changed = sidecar.clone();
        changed.accepted_package.batchTo = changed.accepted_package.batchFrom;
        assert!(matches!(
            changed.validate(&context, &proof_data),
            Err(ValidationError::Package(_))
        ));
    }

    #[test]
    fn proof_encoding_is_canonical_and_v8_only() {
        let (sidecar, context, proof_data) = evidence();
        let (previous, batches, mut proof) =
            <(StoredBatch, Vec<StoredBatch>, Vec<U256>)>::abi_decode_params(&proof_data[1..])
                .unwrap();
        let mut trailing = proof_data.clone();
        trailing.push(0);
        proof[0] = U256::from(0x801);
        let wrong_type = [vec![1], (previous, batches, proof).abi_encode_params()].concat();
        for malformed in [vec![], vec![0], vec![1], trailing, wrong_type] {
            let mut changed = sidecar.clone();
            changed.accepted_package.proofHash = keccak256(&malformed);
            assert!(matches!(
                changed.validate(&context, &malformed),
                Err(ValidationError::ProofData(_))
            ));
        }
    }

    #[test]
    fn reported_duty_must_match_unique_nonempty_native_statement() {
        let (sidecar, context, proof_data) = evidence();
        let mut variants = vec![sidecar.clone(); 5];
        variants[0].duties[0].statementHash = B256::repeat_byte(99);
        variants[1].duties[0].transactionCount = 0;
        variants[2].duties[0].batchNumber = 12;
        variants[3].duties[0].period = 8;
        variants[4].duties.push(sidecar.duties[0].clone());
        for mut changed in variants {
            changed.accepted_package.reportHash = hash_report(&changed.duties);
            assert!(matches!(
                changed.validate(&context, &proof_data),
                Err(ValidationError::Duty(_))
            ));
        }
    }

    #[test]
    fn batch_output_evidence_is_bound_to_native_proof() {
        let (sidecar, context, proof_data) = evidence();
        let mut changed = sidecar.clone();
        changed.batch_outputs[0].edgeDARefsRoot = B256::repeat_byte(99);
        assert_eq!(
            changed.validate(&context, &proof_data),
            Err(ValidationError::BatchOutput(0))
        );
        let mut wrong_settlement = context;
        wrong_settlement.settlement_chain_id += U256::from(1);
        assert_eq!(
            sidecar.validate(&wrong_settlement, &proof_data),
            Err(ValidationError::BatchOutput(0))
        );
        assert_eq!(batch_output_preimage(&sidecar.batch_outputs[0]).len(), 336);
    }

    #[test]
    fn service_requires_selected_candidate_and_exact_frozen_package() {
        let (sidecar, context, proof_data) = service_evidence();
        let mut variants = vec![sidecar.clone(); 3];
        variants[0].accepted_package.turn += 1;
        variants[1].candidate.as_mut().unwrap().index += 1;
        variants[2].candidate_proof[0] = B256::repeat_byte(99);
        for changed in variants {
            assert_eq!(
                changed.validate(&context, &proof_data),
                Err(ValidationError::Candidate)
            );
        }
        let mut changed = sidecar;
        changed.accepted_package.manifestHash = B256::repeat_byte(99);
        assert_eq!(
            changed.validate(&context, &proof_data),
            Err(ValidationError::Package(
                "does not match the frozen package"
            ))
        );
    }

    #[test]
    fn operator_signature_is_bound_by_report_but_not_its_own_digest() {
        let (sidecar, _, _) = evidence();
        let duty = sidecar.duties[0].clone();
        let mut changed = duty.clone();
        changed.operatorSignature = Bytes::from(vec![0xee; 65]);
        assert_eq!(hash_duty(&duty), hash_duty(&changed));
        assert_ne!(hash_report(&[duty]), hash_report(&[changed]));
    }
}
