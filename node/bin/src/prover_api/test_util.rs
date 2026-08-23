use alloy::primitives::{Address, B256, keccak256};
use zksync_os_batch_types::PendingBatchInfo;
use zksync_os_batch_types::batcher_model::{
    BatchForSigning, BatchMetadata, L2_TO_L1_MESSENGER_ADDRESS, SignedBatchEnvelope,
};
use zksync_os_contract_interface::models::{
    CommitBatchInfo, DACommitmentScheme, L2Log, StoredBatchInfo,
};
use zksync_os_types::{L2_INTEROP_CENTER_ADDRESS, ProtocolSemanticVersion, PubdataMode};

// SYSCOIN: Construct the exact durable metadata emitted for a V32 InteropCenter bundle.
pub(super) fn mark_test_batch_as_interop_bundle<T>(batch: &mut SignedBatchEnvelope<T>) {
    let message = vec![0x01, 0x12, 0x34];
    batch.batch.logs.push(L2Log {
        l2_shard_id: 0,
        is_service: true,
        tx_number_in_batch: 0,
        sender: L2_TO_L1_MESSENGER_ADDRESS,
        key: B256::left_padding_from(L2_INTEROP_CENTER_ADDRESS.as_slice()),
        value: keccak256(&message),
    });
    batch.batch.messages.push(message);
}

pub(super) fn create_test_batch_envelope_with_data<T>(
    batch_number: u64,
    protocol_version: ProtocolSemanticVersion,
    data: T,
) -> SignedBatchEnvelope<T> {
    let batch = BatchMetadata {
        previous_stored_batch_info: StoredBatchInfo {
            batch_number: batch_number.saturating_sub(1),
            state_commitment: B256::ZERO,
            number_of_layer1_txs: 0,
            priority_operations_hash: B256::ZERO,
            dependency_roots_rolling_hash: B256::ZERO,
            l2_to_l1_logs_root_hash: B256::ZERO,
            commitment: B256::ZERO,
            last_block_timestamp: Some(0),
        },
        batch_info: PendingBatchInfo {
            commit_info: CommitBatchInfo {
                batch_number,
                new_state_commitment: B256::ZERO,
                number_of_layer1_txs: 0,
                number_of_layer2_txs: 0,
                priority_operations_hash: B256::ZERO,
                dependency_roots_rolling_hash: B256::ZERO,
                l2_to_l1_logs_root_hash: B256::ZERO,
                l2_da_commitment_scheme: DACommitmentScheme::BlobsZKsyncOS,
                da_commitment: keccak256([0u8; 32]),
                first_block_timestamp: 0,
                first_block_number: Some(batch_number),
                last_block_timestamp: 0,
                last_block_number: Some(batch_number),
                chain_id: 1,
                operator_da_input: vec![0u8; 32],
                // SYSCOIN: synthetic prover jobs do not carry Gateway edge-DA openings.
                edge_da_refs_input: vec![],
                edge_da_refs_root: B256::ZERO,
                sl_chain_id: 2,
            },
            protocol_version,
            upgrade_tx_hash: None,
        },
        chain_address: Address::ZERO,
        first_block_number: batch_number,
        last_block_number: batch_number,
        last_block_hash: None,
        pubdata_mode: PubdataMode::Blobs,
        tx_count: 10,
        computational_native_used: None,
        logs: vec![],
        messages: vec![],
        multichain_root: Default::default(),
        set_sl_chain_id_migration_number: None,
    };

    BatchForSigning::new(batch, data)
        .with_signatures(zksync_os_batch_types::batcher_model::BatchSignatureData::NotNeeded)
}
