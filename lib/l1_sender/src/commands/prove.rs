use crate::commands::SendToL1;
use alloy::primitives::{Address, B256, Bytes, U256, keccak256};
use alloy::sol_types::SolCall;
use std::fmt::Display;
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

#[derive(Debug)]
pub struct ProofCommand {
    batches: Vec<SignedBatchEnvelope<FriProof>>,
    proof: SnarkProof,
}

impl ProofCommand {
    pub fn new(batches: Vec<SignedBatchEnvelope<FriProof>>, proof: SnarkProof) -> Self {
        assert!(
            !batches.is_empty(),
            "ProofCommand must contain at least one batch"
        );
        Self { batches, proof }
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
    ) -> u32 {
        match proving_execution_version {
            // Fake and real proofs are both bound to the sole canonical V8 verifier slot.
            None | Some(CANONICAL_VERIFIER_VERSION) => CANONICAL_VERIFIER_VERSION,
            Some(execution_version) => panic!(
                "unsupported or old execution version: {execution_version}; there's no verifier defined for it"
            ),
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
    fn snark_public_input(
        previous_batch: &StoredBatchInfo,
        batches: &[StoredBatchInfo],
        chain_config_hash: B256,
    ) -> B256 {
        let mut elements = Vec::with_capacity(batches.len());
        let mut previous = previous_batch;
        for batch in batches {
            assert_eq!(
                batch.batch_number,
                previous.batch_number + 1,
                "proof batches must be contiguous"
            );
            elements.push(Self::batch_public_input(
                previous,
                batch,
                &chain_config_hash,
            ));
            previous = batch;
        }

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
        // todo: awful and temporary
        let verifier_version = Self::verifier_version_for_proving_execution_version(
            self.proof.proving_execution_version(),
        );

        let first_batch = self.batches.first().unwrap();
        // SYSCOIN: Use the same fixed-gas-limit chain-config commitment as native PIG.
        let chain_config_hash =
            syscoin_chain_config_hash(first_batch.batch.batch_info.commit_info.chain_id);
        let public_input =
            Self::snark_public_input(previous_batch_info, &stored_batch_infos, chain_config_hash);

        tracing::info!(">> public input: {}", public_input);

        let proof: Vec<U256> = match &self.proof {
            SnarkProof::Fake => {
                vec![
                    // Fake proof type, bound to the canonical V8 verifier slot.
                    U256::from(FAKE_PROOF_TYPE | (verifier_version << 8)),
                    // OhBender 'previous hash' - for fake proof, we can always assume that it matches the range perfectly.
                    U256::from(0),
                    // Fake proof magic value (just for sanity)
                    U256::from(FAKE_PROOF_MAGIC_VALUE),
                    // Public input (fake proof **will** verify this against batch data stored in the contract)
                    U256::from_be_bytes(public_input.0),
                ]
            }
            SnarkProof::Real(real) => {
                let proof: Vec<U256> = real
                    .proof()
                    .chunks(32)
                    .map(|chunk| {
                        let arr: [u8; 32] = chunk
                            .try_into()
                            .expect("proof bytes must be a multiple of 32");
                        U256::from_be_bytes(arr)
                    })
                    .collect();
                vec![
                    // Real proof versioned with a specific verifier
                    U256::from(OHBENDER_PROOF_TYPE | (verifier_version << 8)),
                    // we generate SNARK proofs to always match the range perfectly.
                    U256::from(0),
                ]
                .into_iter()
                .chain(proof)
                .collect()
            }
        };

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
    use super::{FAKE_PROOF_TYPE, OHBENDER_PROOF_TYPE, ProofCommand};

    #[test]
    fn real_proofs_use_v8_verifier_slot() {
        let verifier_version =
            ProofCommand::verifier_version_for_proving_execution_version(Some(8));

        assert_eq!(verifier_version, 8);
        assert_eq!(OHBENDER_PROOF_TYPE | (verifier_version << 8), 0x802);
    }

    #[test]
    fn fake_proofs_use_v8_verifier_slot() {
        let verifier_version = ProofCommand::verifier_version_for_proving_execution_version(None);

        assert_eq!(verifier_version, 8);
        assert_eq!(FAKE_PROOF_TYPE | (verifier_version << 8), 0x803);
    }
}
