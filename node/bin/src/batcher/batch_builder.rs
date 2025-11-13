use alloy::primitives::Address;
use zksync_os_contract_interface::models::StoredBatchInfo;
use zksync_os_interface::types::BlockOutput;
use zksync_os_l1_sender::batcher_metrics::BatchExecutionStage;
use zksync_os_l1_sender::batcher_model::{
    BatchEnvelope, BatchForSigning, BatchMetadata, ProverInput,
};
use zksync_os_l1_sender::commitment::BatchInfo;

use zksync_os_storage_api::ReplayRecord;
use zksync_os_types::PubdataMode;

/// Takes a vector of blocks and produces a batch envelope.
/// This is a pure function that is meant to be stateless and not contained in the `Batcher` struct.
pub(crate) fn seal_batch(
    blocks: &[(
        BlockOutput,
        ReplayRecord,
        zksync_os_merkle_tree::TreeBatchOutput,
        ProverInput,
    )],
    prev_batch_info: StoredBatchInfo,
    batch_number: u64,
    chain_id: u64,
    chain_address: Address,
    pubdata_mode: PubdataMode,
) -> anyhow::Result<BatchForSigning<ProverInput>> {
    let block_number_from = blocks.first().unwrap().1.block_context.block_number;
    let block_number_to = blocks.last().unwrap().1.block_context.block_number;
    let execution_version = blocks.first().unwrap().1.block_context.execution_version;

    let batch_info = BatchInfo::new(
        blocks
            .iter()
            .map(|(block_output, replay_record, tree, _)| {
                (
                    block_output,
                    &replay_record.block_context,
                    replay_record.transactions.as_slice(),
                    tree,
                )
            })
            .collect(),
        chain_id,
        chain_address,
        batch_number,
        pubdata_mode,
    );

    use zk_os_forward_system::run::generate_batch_proof_input;

    // TODO: in the long-term we should generate proof input per batch
    let batch_prover_input: ProverInput = generate_batch_proof_input(
        blocks
            .iter()
            .map(|(_, _, _, prover_input)| prover_input.as_slice())
            .collect(),
        pubdata_mode.da_commitment_scheme().into(),
        blocks
            .iter()
            .map(|(block_output, _, _, _)| block_output.pubdata.as_slice())
            .collect(),
    );

    let batch_envelope = BatchEnvelope::new(
        BatchMetadata {
            previous_stored_batch_info: prev_batch_info,
            batch_info,
            first_block_number: block_number_from,
            last_block_number: block_number_to,
            tx_count: blocks
                .iter()
                .map(|(block_output, _, _, _)| block_output.tx_results.len())
                .sum(),
            execution_version,
        },
        batch_prover_input,
    )
    .with_stage(BatchExecutionStage::BatchSealed);

    Ok(batch_envelope)
}
