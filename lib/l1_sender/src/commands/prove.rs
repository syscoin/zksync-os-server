use crate::commands::SendToL1;
use alloy::primitives::{Address, B256, Bytes, U256, keccak256};
use alloy::sol_types::SolCall;
use std::fmt::Display;
use tokio::sync::mpsc;
use zksync_os_batch_types::batcher_model::{FriProof, SignedBatchEnvelope, SnarkProof};
use zksync_os_batcher_metrics::BatchExecutionStage;
use zksync_os_contract_interface::IExecutor;
use zksync_os_contract_interface::IExecutor::{proofPayloadCall, proveBatchesSharedBridgeCall};
use zksync_os_contract_interface::models::StoredBatchInfo;
use zksync_os_types::syscoin_chain_config_hash;

const OHBENDER_PROOF_TYPE: u32 = 2;
const FAKE_PROOF_TYPE: u32 = 3;
const FAKE_PROOF_MAGIC_VALUE: u32 = 13;
// SYSCOIN: Fresh deployments use only the regenerated app-bound V8 verifier slot.
const CANONICAL_VERIFIER_VERSION: u32 = 8;
// SYSCOIN: The pinned V32 non-recursive PLONK verifier template requires exactly 44 proof
// words. The Executor wrapper prepends two routing words separately; they are not part of these
// 1,408 prover-supplied bytes.
pub const ZKSYNC_OS_V8_REAL_PROOF_WORDS: usize = 44;
pub const ZKSYNC_OS_V8_REAL_PROOF_BYTES: usize = ZKSYNC_OS_V8_REAL_PROOF_WORDS * 32;

/// SYSCOIN: Exact arguments passed by the pinned V32 Executor to the active zkOS wrapper. Keep
/// this type free of `Debug`: a derived implementation would expose every submitted proof word.
pub struct ZksyncOsVerifierInput {
    pub public_inputs: Vec<U256>,
    pub proof: Vec<U256>,
}

/// SYSCOIN: Fallible verifier-input construction lets HTTP admission retain the exact lease on an
/// internal metadata invariant failure instead of panicking after an expensive wrapper upload.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ZksyncOsVerifierInputError {
    #[error("proof command contains no batches")]
    EmptyBatches,
    #[error("proof batches are not contiguous: batch {current} follows batch {previous}")]
    NonContiguous { previous: u64, current: u64 },
    #[error("batch {batch} does not reference the preceding stored batch")]
    PreviousBatchMismatch { batch: u64 },
    #[error("proof range crosses chain addresses at batch {batch}")]
    ChainAddressMismatch { batch: u64 },
    #[error("proof range crosses chain IDs at batch {batch}")]
    ChainIdMismatch { batch: u64 },
    #[error("unsupported or old proving execution version {0}")]
    UnsupportedVersion(u32),
    #[error("real proof payload is empty")]
    EmptyRealProof,
    #[error("real V8 proof payload must be exactly {ZKSYNC_OS_V8_REAL_PROOF_BYTES} bytes; got {0}")]
    InvalidRealProofLength(usize),
}

pub struct ProofCommand {
    batches: Vec<SignedBatchEnvelope<FriProof>>,
    proof: SnarkProof,
    // SYSCOIN: The unbounded notification contains only a small non-secret journal key. Failure
    // to deliver leaves the fsynced journal intact for restart recovery.
    durable_confirmation: Option<DurableProofConfirmation>,
}

// SYSCOIN: Couple an in-memory L1 command to the node-local durable wrapper record that created it.
pub struct DurableProofConfirmation {
    journal_key: String,
    sender: mpsc::UnboundedSender<String>,
}

impl std::fmt::Debug for DurableProofConfirmation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DurableProofConfirmation")
            .field("journal_key", &self.journal_key)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for ProofCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProofCommand")
            .field(
                "batch_from",
                &self.batches.first().map(|batch| batch.batch_number()),
            )
            .field(
                "batch_to",
                &self.batches.last().map(|batch| batch.batch_number()),
            )
            .field("batch_count", &self.batches.len())
            // SYSCOIN: Never dump multi-megabyte wrapper bytes through command diagnostics.
            .field("proof", &RedactedProofDebug(&self.proof))
            .field("durable_confirmation", &self.durable_confirmation)
            .finish()
    }
}

// SYSCOIN: Proof diagnostics expose only bounded routing metadata, never proof bytes.
struct RedactedProofDebug<'a>(&'a SnarkProof);

impl std::fmt::Debug for RedactedProofDebug<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            SnarkProof::Fake => formatter
                .debug_struct("Fake")
                .field("proof_bytes", &0)
                .finish(),
            SnarkProof::Real(real) => formatter
                .debug_struct("Real")
                .field("proof_bytes", &real.proof().len())
                .field("proving_execution_version", &real.proving_execution_version)
                .finish(),
        }
    }
}

impl ProofCommand {
    pub fn new(batches: Vec<SignedBatchEnvelope<FriProof>>, proof: SnarkProof) -> Self {
        assert!(
            !batches.is_empty(),
            "ProofCommand must contain at least one batch"
        );
        Self {
            batches,
            proof,
            durable_confirmation: None,
        }
    }

    // SYSCOIN: Construct a proof command whose wrapper journal is retired only after confirmed L1
    // inclusion. The journal key is deliberately node-local and never enters calldata.
    pub fn new_durable(
        batches: Vec<SignedBatchEnvelope<FriProof>>,
        proof: SnarkProof,
        journal_key: String,
        confirmation_sender: mpsc::UnboundedSender<String>,
    ) -> Self {
        let mut command = Self::new(batches, proof);
        command.durable_confirmation = Some(DurableProofConfirmation {
            journal_key,
            sender: confirmation_sender,
        });
        command
    }
}

impl SendToL1 for ProofCommand {
    const COMPONENT_ID: zksync_os_pipeline::ComponentId =
        zksync_os_pipeline::ComponentId::L1SenderProve;
    const SENT_STAGE: BatchExecutionStage = BatchExecutionStage::ProveL1TxSent;
    const MINED_STAGE: BatchExecutionStage = BatchExecutionStage::ProveL1TxMined;
    const PASSTHROUGH_STAGE: BatchExecutionStage = BatchExecutionStage::ProveL1Passthrough;

    fn solidity_call(&self, _gateway: bool, _operator: &Address) -> Bytes {
        proveBatchesSharedBridgeCall::new((
            self.batches.first().unwrap().batch.chain_address,
            U256::from(self.batches.first().unwrap().batch_number()),
            U256::from(self.batches.last().unwrap().batch_number()),
            self.to_calldata_suffix().into(),
        ))
        .abi_encode()
        .into()
    }

    // SYSCOIN: Confirmation makes the wrapper reproducible from L1 state; notify the node reaper
    // without making L1 progress depend on the local cleanup channel remaining alive.
    fn notify_confirmed(&self) {
        let Some(confirmation) = &self.durable_confirmation else {
            return;
        };
        if confirmation
            .sender
            .send(confirmation.journal_key.clone())
            .is_err()
        {
            tracing::warn!(
                journal_key = confirmation.journal_key,
                "durable SNARK journal confirmation receiver is closed; retaining record for restart recovery"
            );
        }
    }
}

impl AsRef<[SignedBatchEnvelope<FriProof>]> for ProofCommand {
    fn as_ref(&self) -> &[SignedBatchEnvelope<FriProof>] {
        self.batches.as_slice()
    }
}

impl AsMut<[SignedBatchEnvelope<FriProof>]> for ProofCommand {
    fn as_mut(&mut self) -> &mut [SignedBatchEnvelope<FriProof>] {
        self.batches.as_mut_slice()
    }
}

impl From<ProofCommand> for Vec<SignedBatchEnvelope<FriProof>> {
    fn from(value: ProofCommand) -> Self {
        value.batches
    }
}

impl Display for ProofCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "prove batches {}-{}",
            self.batches.first().unwrap().batch_number(),
            self.batches.last().unwrap().batch_number()
        )?;
        Ok(())
    }
}

impl ProofCommand {
    fn verifier_version_for_proving_execution_version(
        proving_execution_version: Option<u32>,
    ) -> Result<u32, ZksyncOsVerifierInputError> {
        match proving_execution_version {
            // SYSCOIN: Fake and real proofs are both bound to the sole canonical V8 verifier slot;
            // unsupported metadata is rejected without panicking the sender pipeline.
            None | Some(CANONICAL_VERIFIER_VERSION) => Ok(CANONICAL_VERIFIER_VERSION),
            Some(execution_version) => Err(ZksyncOsVerifierInputError::UnsupportedVersion(
                execution_version,
            )),
        }
    }

    fn shift_b256_right(input: &B256) -> B256 {
        let mut bytes = [0_u8; 32];
        bytes[4..32].copy_from_slice(&input.as_slice()[0..28]);
        B256::from_slice(&bytes)
    }

    /// Canonical batch public input: chain config hash folded between the state commitments and
    /// the chain-id-less batch-output hash (`batch.commitment`).
    fn batch_public_input(
        prev_batch: &StoredBatchInfo,
        batch: &StoredBatchInfo,
        chain_config_hash: &B256,
    ) -> B256 {
        let mut bytes = Vec::with_capacity(32 * 4);
        bytes.extend_from_slice(prev_batch.state_commitment.as_slice());
        bytes.extend_from_slice(batch.state_commitment.as_slice());
        bytes.extend_from_slice(chain_config_hash.as_slice());
        bytes.extend_from_slice(batch.commitment.as_slice());
        keccak256(&bytes)
    }
    fn folded_snark_public_input(elements: &[B256]) -> B256 {
        debug_assert!(!elements.is_empty());
        let folded = if elements.len() == 1 {
            elements[0]
        } else {
            keccak256(
                elements
                    .iter()
                    .flat_map(|element| element.0)
                    .collect::<Vec<u8>>(),
            )
        };
        Self::shift_b256_right(&folded)
    }

    /// SYSCOIN: Reproduce both sides of the pinned V32 call boundary: Executor supplies one
    /// unshifted per-batch hash, while the wrapper folds the complete array and shifts once.
    /// `to_calldata_suffix` and admission preflight share this implementation to prevent drift.
    pub fn zksync_os_verifier_input(
        batches: &[SignedBatchEnvelope<FriProof>],
        snark_proof: &SnarkProof,
    ) -> Result<ZksyncOsVerifierInput, ZksyncOsVerifierInputError> {
        let first = batches
            .first()
            .ok_or(ZksyncOsVerifierInputError::EmptyBatches)?;
        let expected_chain_address = first.batch.chain_address;
        let expected_chain_id = first.batch.batch_info.commit_info.chain_id;
        let chain_config_hash = syscoin_chain_config_hash(expected_chain_id);
        let mut previous = first.batch.previous_stored_batch_info.clone();
        let mut batch_public_inputs = Vec::with_capacity(batches.len());

        for envelope in batches {
            let batch_number = envelope.batch_number();
            if envelope.batch.chain_address != expected_chain_address {
                return Err(ZksyncOsVerifierInputError::ChainAddressMismatch {
                    batch: batch_number,
                });
            }
            if envelope.batch.batch_info.commit_info.chain_id != expected_chain_id {
                return Err(ZksyncOsVerifierInputError::ChainIdMismatch {
                    batch: batch_number,
                });
            }
            if envelope.batch.previous_stored_batch_info != previous {
                return Err(ZksyncOsVerifierInputError::PreviousBatchMismatch {
                    batch: batch_number,
                });
            }

            let stored = envelope.batch.batch_info.clone().into_stored();
            let expected_batch_number = previous.batch_number.checked_add(1).ok_or(
                ZksyncOsVerifierInputError::NonContiguous {
                    previous: previous.batch_number,
                    current: stored.batch_number,
                },
            )?;
            if stored.batch_number != expected_batch_number {
                return Err(ZksyncOsVerifierInputError::NonContiguous {
                    previous: previous.batch_number,
                    current: stored.batch_number,
                });
            }
            batch_public_inputs.push(Self::batch_public_input(
                &previous,
                &stored,
                &chain_config_hash,
            ));
            previous = stored;
        }

        let verifier_version = Self::verifier_version_for_proving_execution_version(
            snark_proof.proving_execution_version(),
        )?;
        let folded_public_input = Self::folded_snark_public_input(&batch_public_inputs);
        let proof = match snark_proof {
            SnarkProof::Fake => vec![
                U256::from(FAKE_PROOF_TYPE | (verifier_version << 8)),
                U256::ZERO,
                U256::from(FAKE_PROOF_MAGIC_VALUE),
                U256::from_be_bytes(folded_public_input.0),
            ],
            SnarkProof::Real(real) => {
                if real.proof().is_empty() {
                    return Err(ZksyncOsVerifierInputError::EmptyRealProof);
                }
                if real.proof().len() != ZKSYNC_OS_V8_REAL_PROOF_BYTES {
                    return Err(ZksyncOsVerifierInputError::InvalidRealProofLength(
                        real.proof().len(),
                    ));
                }
                vec![
                    U256::from(OHBENDER_PROOF_TYPE | (verifier_version << 8)),
                    U256::ZERO,
                ]
                .into_iter()
                .chain(real.proof().chunks_exact(32).map(|chunk| {
                    let mut word = [0_u8; 32];
                    word.copy_from_slice(chunk);
                    U256::from_be_bytes(word)
                }))
                .collect()
            }
        };

        Ok(ZksyncOsVerifierInput {
            public_inputs: batch_public_inputs
                .into_iter()
                .map(|input| U256::from_be_bytes(input.0))
                .collect(),
            proof,
        })
    }

    fn to_calldata_suffix(&self) -> Vec<u8> {
        let previous_batch_info = &self
            .batches
            .first()
            .unwrap()
            .batch
            .previous_stored_batch_info;
        let stored_batch_infos: Vec<StoredBatchInfo> = self
            .batches
            .iter()
            .map(|batch| batch.batch.batch_info.clone().into_stored())
            .collect();
        // SYSCOIN: The same checked proof words are sent to the wrapper during preflight and later
        // encoded for the Executor; this `expect` is an internal pipeline invariant, not RPC input.
        let proof = Self::zksync_os_verifier_input(&self.batches, &self.proof)
            .expect("ProofCommand must contain one canonical contiguous proof range")
            .proof;

        let proof_payload = proofPayloadCall {
            old: IExecutor::StoredBatchInfo::from(previous_batch_info),
            newInfo: stored_batch_infos
                .iter()
                .map(Into::into) // into `IExecutor::StoredBatchInfo`
                .collect(),
            proof,
        };

        /// Current commitment encoding version as per protocol.
        const SUPPORTED_ENCODING_VERSION: u8 = 1;

        let mut proof_data = vec![SUPPORTED_ENCODING_VERSION];
        proof_payload.abi_encode_raw(&mut proof_data);
        proof_data
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FAKE_PROOF_TYPE, OHBENDER_PROOF_TYPE, ProofCommand, RedactedProofDebug, StoredBatchInfo,
    };
    use alloy::primitives::{B256, keccak256};
    use zksync_os_batch_types::batcher_model::{RealSnarkProof, SnarkProof};

    #[test]
    fn real_proofs_use_v8_verifier_slot() {
        let verifier_version =
            ProofCommand::verifier_version_for_proving_execution_version(Some(8)).unwrap();

        assert_eq!(verifier_version, 8);
        assert_eq!(OHBENDER_PROOF_TYPE | (verifier_version << 8), 0x802);
    }

    #[test]
    fn fake_proofs_use_v8_verifier_slot() {
        let verifier_version =
            ProofCommand::verifier_version_for_proving_execution_version(None).unwrap();

        assert_eq!(verifier_version, 8);
        assert_eq!(FAKE_PROOF_TYPE | (verifier_version << 8), 0x803);
    }

    // SYSCOIN: The pinned V32 Executor sends full per-batch hashes and the wrapper performs one
    // concat-keccak plus one 32-bit shift. A rolling fold or per-element shift diverges at N >= 2.
    #[test]
    fn v32_public_inputs_are_unshifted_and_folded_once() {
        fn stored(batch_number: u64, state: u8, commitment: u8) -> StoredBatchInfo {
            StoredBatchInfo {
                batch_number,
                state_commitment: B256::repeat_byte(state),
                number_of_layer1_txs: 0,
                priority_operations_hash: B256::ZERO,
                dependency_roots_rolling_hash: B256::ZERO,
                l2_to_l1_logs_root_hash: B256::ZERO,
                commitment: B256::repeat_byte(commitment),
                last_block_timestamp: None,
            }
        }

        let chain_config_hash = B256::repeat_byte(0x44);
        let previous = stored(0, 0x10, 0x20);
        let first = stored(1, 0x11, 0x21);
        let second = stored(2, 0x12, 0x22);
        let first_input = ProofCommand::batch_public_input(&previous, &first, &chain_config_hash);
        let second_input = ProofCommand::batch_public_input(&first, &second, &chain_config_hash);
        let expected_fold = ProofCommand::shift_b256_right(&keccak256(
            [first_input.as_slice(), second_input.as_slice()].concat(),
        ));

        assert_eq!(
            ProofCommand::folded_snark_public_input(&[first_input, second_input]),
            expected_fold
        );
        assert_eq!(
            ProofCommand::folded_snark_public_input(&[first_input]),
            ProofCommand::shift_b256_right(&first_input)
        );
    }

    #[test]
    fn proof_debug_is_bounded_and_redacted() {
        let proof = SnarkProof::Real(RealSnarkProof {
            proof: vec![0xAB; 64],
            proving_execution_version: 8,
        });
        let debug = format!("{:?}", RedactedProofDebug(&proof));
        assert!(debug.contains("proof_bytes: 64"));
        assert!(debug.contains("proving_execution_version: 8"));
        assert!(!debug.contains("171"), "raw 0xAB bytes leaked: {debug}");
    }
}
