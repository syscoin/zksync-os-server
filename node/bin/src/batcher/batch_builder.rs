use crate::pig_telemetry::{BatchPigTelemetry, record_batch_pig_telemetry};
use alloy::primitives::Address;
use std::time::Duration;
use zksync_os_batch_types::batcher_model::{
    BatchEnvelope, BatchForSigning, BatchMetadata, ProverInput,
};
use zksync_os_batcher_metrics::BatchExecutionStage;
use zksync_os_contract_interface::models::{L2Log, StoredBatchInfo};
use zksync_os_merkle_tree::{MerkleTree, RocksDBWrapper};
use zksync_os_native_pig::{NativeBatchBlock, generate_batch_run};
use zksync_os_storage_api::{ReadStateHistory, TreeBlock, read_multichain_root};
use zksync_os_types::{ProvingVersion, PubdataMode, SystemTxType, ZkEnvelope};

#[derive(Debug, Clone, Copy)]
struct BatchPigMeasurement {
    prover_input_words: usize,
    elapsed: Duration,
}

// SYSCOIN: Carry V8 canonical pubdata past native sealing for Bitcoin DA publication.
pub(crate) struct SealedBatch {
    pub batch: BatchForSigning<ProverInput>,
    pub canonical_pubdata: Vec<u8>,
}

/// Takes a vector of blocks and produces a batch envelope.
#[allow(clippy::too_many_arguments)]
pub(crate) fn seal_batch<ReadState: ReadStateHistory>(
    blocks: &[TreeBlock],
    prev_batch_info: StoredBatchInfo,
    batch_number: u64,
    chain_id: u64,
    chain_address_sl: Address,
    pubdata_mode: PubdataMode,
    sl_chain_id: u64,
    compact_edge_da_commit_target: Address,
    read_state: &ReadState,
    merkle_tree: &MerkleTree<RocksDBWrapper>,
) -> anyhow::Result<SealedBatch> {
    let block_number_from = blocks.first().unwrap().record.block_context.block_number;
    let block_number_to = blocks.last().unwrap().record.block_context.block_number;
    let last_block_hash = blocks.last().unwrap().output.header.hash();
    let protocol_version = blocks.first().unwrap().record.protocol_version.clone();
    let proving_version = ProvingVersion::try_from(protocol_version.clone())?;
    let batch_computational_native_used: u64 = blocks
        .iter()
        .map(|block| block.output.computational_native_used)
        .sum();

    let state_view = read_state.state_view_at(block_number_to)?;
    let multichain_root = read_multichain_root(state_view);
    let native_blocks = blocks
        .iter()
        .map(|block| {
            Ok(NativeBatchBlock {
                replay_record: &block.record,
                tree_data: &block.tree,
                block_output: &block.output,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let started_at = std::time::Instant::now();
    // SYSCOIN: The patched guest authenticates compact edge-DA calls to this fixed target.
    let native_batch_run = generate_batch_run(
        &native_blocks,
        read_state,
        merkle_tree.clone(),
        pubdata_mode,
        compact_edge_da_commit_target,
    )?;
    let native_pig_measurement = BatchPigMeasurement {
        prover_input_words: native_batch_run.prover_input.len(),
        elapsed: started_at.elapsed(),
    };
    tracing::info!(
        batch_number,
        block_number_from,
        block_number_to,
        block_count = blocks.len(),
        ?protocol_version,
        ?proving_version,
        pubdata_mode = ?pubdata_mode,
        sl_chain_id,
        prover_input_words = native_batch_run.prover_input.len(),
        canonical_pubdata_bytes = native_batch_run.pubdata.len(),
        "Using native batch PIG for batch sealing",
    );

    anyhow::ensure!(
        native_batch_run.previous_state_commitment == prev_batch_info.state_commitment,
        "native batch run previous state commitment {} does not match previous batch {}",
        native_batch_run.previous_state_commitment,
        prev_batch_info.state_commitment,
    );
    let batch_info = native_batch_run.build_batch_info(
        batch_number,
        block_number_from,
        block_number_to,
        pubdata_mode,
        &protocol_version,
        chain_id,
        sl_chain_id,
    )?;

    let mut logs = Vec::new();
    let mut messages = Vec::new();
    for block in blocks {
        for output in block.output.tx_results.iter().flatten() {
            // SYSCOIN: A reverted transaction cannot authenticate a durable interop bundle.
            // Keep batch metadata aligned with the canonical block-level detector used by the
            // companion-batch policy and priority SNARK readiness.
            if !output.is_success() {
                continue;
            }
            for l2_to_l1_log in &output.l2_to_l1_logs {
                logs.push(L2Log {
                    l2_shard_id: l2_to_l1_log.log.l2_shard_id,
                    is_service: l2_to_l1_log.log.is_service,
                    tx_number_in_batch: l2_to_l1_log.log.tx_number_in_block,
                    sender: l2_to_l1_log.log.sender,
                    key: l2_to_l1_log.log.key,
                    value: l2_to_l1_log.log.value,
                });
                if let Some(preimage) = l2_to_l1_log.preimage.as_ref() {
                    messages.push(preimage.clone());
                }
            }
        }
    }

    let canonical_pubdata = native_batch_run.pubdata.clone();
    let batch_prover_input = ProverInput::Real(native_batch_run.prover_input);
    record_batch_pig_telemetry(BatchPigTelemetry {
        batch_number,
        chain_id,
        first_block_number: block_number_from,
        last_block_number: block_number_to,
        proving_version,
        prover_input_words: native_pig_measurement.prover_input_words,
        computational_native_used: batch_computational_native_used,
        elapsed: native_pig_measurement.elapsed,
    });

    // Sanity check: all blocks in the batch should have the same protocol version
    for block in blocks.iter().skip(1) {
        anyhow::ensure!(
            block.record.protocol_version == protocol_version,
            "mismatched protocol versions in batch: expected {}, found {}; blocks: {}-{}",
            protocol_version,
            block.record.protocol_version,
            block_number_from,
            block_number_to,
        );
    }

    // Detect any `SetSLChainId` system transaction across all blocks in the batch.
    // Excludes the sentinel value `u64::MAX` which is used during protocol upgrades and is
    // unrelated to gateway migrations.
    let set_sl_chain_id_migration_number = blocks.iter().find_map(|block| {
        block.record.transactions.iter().find_map(|tx| {
            if let ZkEnvelope::System(system_tx) = tx.envelope()
                && let SystemTxType::SetSLChainId(_, n) = system_tx.system_subtype()
                && *n != u64::MAX
            {
                Some(*n)
            } else {
                None
            }
        })
    });

    let batch_envelope = BatchEnvelope::new(
        BatchMetadata {
            previous_stored_batch_info: prev_batch_info,
            batch_info,
            chain_address: chain_address_sl,
            first_block_number: block_number_from,
            last_block_number: block_number_to,
            last_block_hash: Some(last_block_hash),
            pubdata_mode,
            tx_count: blocks
                .iter()
                .map(|block| block.output.tx_results.len())
                .sum(),
            computational_native_used: Some(batch_computational_native_used),
            logs,
            messages,
            multichain_root,
            set_sl_chain_id_migration_number,
        },
        batch_prover_input,
    )
    .with_stage(BatchExecutionStage::BatchSealed);

    Ok(SealedBatch {
        batch: batch_envelope,
        canonical_pubdata,
    })
}
