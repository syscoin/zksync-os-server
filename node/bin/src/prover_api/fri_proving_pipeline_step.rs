use super::proof_storage::{ProofStorage, ProvenBatch};
use crate::prover_api::fri_job_manager::FriJobManager;
use crate::prover_api::fri_proof_verifier;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use zksync_os_batch_types::batcher_model::{FriProof, ProverInput, SignedBatchEnvelope};
use zksync_os_observability::ComponentStateReporter;
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent, SendAndRecordExt};

/// Pipeline step that waits for batches to be FRI proved.
///
/// This component:
/// - Receives batches with ProverInput from the batcher
/// - Adds them directly to FriJobManager (which makes them available via HTTP API)
/// - Receives proofs from FriJobManager (submitted via HTTP API or fake provers)
/// - Forwards the proofs downstream in the pipeline
///
/// The FriJobManager itself is purely reactive (no run loop), accessed/driven by:
/// - HTTP server (provers call pick_next_job, submit_proof, etc.)
/// - Fake provers pool
/// - This pipeline step (adds jobs via add_job)
pub struct FriProvingPipelineStep {
    last_proved_batch_number: u64,
    proof_storage: ProofStorage,
    fri_job_manager: Arc<FriJobManager>,
    batches_with_proof_receiver: mpsc::Receiver<ProvenBatch>,
}

impl FriProvingPipelineStep {
    pub fn new(
        proof_storage: ProofStorage,
        last_proved_batch_number: u64,
        assignment_timeout: Duration,
        max_assigned_batch_range: usize,
    ) -> (Self, Arc<FriJobManager>) {
        // Create channel for completed proofs - between FriProveManager and GaplessCommitter
        let (batches_with_proof_sender, batches_with_proof_receiver) =
            mpsc::channel::<ProvenBatch>(5);

        let fri_job_manager = Arc::new(FriJobManager::new(
            batches_with_proof_sender,
            proof_storage.clone(),
            assignment_timeout,
            max_assigned_batch_range,
        ));

        let result = Self {
            last_proved_batch_number,
            proof_storage,
            fri_job_manager: fri_job_manager.clone(),
            batches_with_proof_receiver,
        };

        (result, fri_job_manager)
    }
    // SYSCOIN
    fn can_rehydrate_batch(
        expected_batch: &SignedBatchEnvelope<ProverInput>,
        stored_batch: &SignedBatchEnvelope<FriProof>,
    ) -> bool {
        if stored_batch.batch_number() != expected_batch.batch_number() {
            tracing::warn!(
                expected_batch_number = expected_batch.batch_number(),
                actual_batch_number = stored_batch.batch_number(),
                "skipping FRI rehydration due to stored proof batch number mismatch"
            );
            return false;
        }

        if stored_batch.batch.previous_stored_batch_info
            != expected_batch.batch.previous_stored_batch_info
        {
            tracing::warn!(
                batch_number = expected_batch.batch_number(),
                stored_previous = ?stored_batch.batch.previous_stored_batch_info.hash(),
                expected_previous = ?expected_batch.batch.previous_stored_batch_info.hash(),
                "skipping FRI rehydration due to previous batch info mismatch"
            );
            return false;
        }

        let expected_stored_batch = expected_batch.batch.batch_info.clone().into_stored();
        let stored_batch_info = stored_batch.batch.batch_info.clone().into_stored();

        let expected_hash = expected_stored_batch.hash();
        let stored_hash = stored_batch_info.hash();
        if expected_hash != stored_hash {
            tracing::warn!(
                batch_number = expected_batch.batch_number(),
                ?expected_hash,
                ?stored_hash,
                "skipping FRI rehydration due to committed/local batch hash mismatch"
            );
            return false;
        }

        match &stored_batch.data {
            FriProof::Real(real) => {
                if let Err(err) = fri_proof_verifier::verify_real_fri_proof_bytes(
                    expected_batch
                        .batch
                        .previous_stored_batch_info
                        .state_commitment,
                    expected_stored_batch,
                    real.proof(),
                ) {
                    tracing::warn!(
                        batch_number = expected_batch.batch_number(),
                        ?err,
                        "skipping FRI rehydration due to invalid stored FRI proof"
                    );
                    return false;
                }
                true
            }
            FriProof::Fake => {
                // SYSCOIN: Fake proofs are valid only for the run that explicitly enabled fake
                // provers. Never rehydrate them from disk, since a later production run may now
                // require real proofs for the same batch.
                tracing::warn!(
                    batch_number = expected_batch.batch_number(),
                    "skipping FRI rehydration because stored proof is fake"
                );
                false
            }
            FriProof::AlreadySubmittedToL1 => {
                tracing::warn!(
                    batch_number = expected_batch.batch_number(),
                    "skipping FRI rehydration because stored batch is marked AlreadySubmittedToL1"
                );
                false
            }
        }
    }

    async fn try_rehydrate_batch(
        proof_storage: &ProofStorage,
        batch: &SignedBatchEnvelope<ProverInput>,
    ) -> Option<SignedBatchEnvelope<FriProof>> {
        let stored_batch = match proof_storage
            .get_batch_with_proof(batch.batch_number())
            .await
        {
            Ok(Some(batch)) => batch,
            Ok(None) => return None,
            Err(err) => {
                tracing::warn!(
                    batch_number = batch.batch_number(),
                    ?err,
                    "failed to load stored FRI proof during restart rehydration"
                );
                return None;
            }
        };

        if Self::can_rehydrate_batch(batch, &stored_batch) {
            tracing::info!(
                batch_number = batch.batch_number(),
                "Reusing stored FRI proof after restart"
            );
            Some(stored_batch)
        } else {
            None
        }
    }

    // SYSCOIN
    async fn try_rehydrate_pending_batch(
        proof_storage: &ProofStorage,
        batch: &SignedBatchEnvelope<ProverInput>,
    ) -> Option<ProvenBatch> {
        let pending_keys = proof_storage.recovered_pending_batch_proof_keys().await;
        for pending_key in pending_keys {
            if pending_key.batch_number() < batch.batch_number() {
                tracing::warn!(
                    batch_number = pending_key.batch_number(),
                    current_batch_number = batch.batch_number(),
                    ?pending_key,
                    "dropping stale pending FRI proof before rehydration"
                );
                proof_storage
                    .release_pending_batch_with_proof(&pending_key)
                    .await;
                continue;
            }
            if pending_key.batch_number() != batch.batch_number() {
                continue;
            }

            let stored_batch = match proof_storage
                .get_pending_batch_with_proof(&pending_key)
                .await
            {
                Ok(Some(batch)) => batch,
                Ok(None) => {
                    tracing::warn!(
                        batch_number = batch.batch_number(),
                        ?pending_key,
                        "dropping missing pending FRI proof during rehydration"
                    );
                    proof_storage
                        .release_pending_batch_with_proof(&pending_key)
                        .await;
                    continue;
                }
                Err(err) => {
                    tracing::warn!(
                        batch_number = batch.batch_number(),
                        ?pending_key,
                        ?err,
                        "quarantining unloadable pending FRI proof during rehydration"
                    );
                    proof_storage
                        .quarantine_pending_batch_with_proof(&pending_key)
                        .await;
                    continue;
                }
            };

            if Self::can_rehydrate_batch(batch, &stored_batch) {
                tracing::info!(
                    batch_number = batch.batch_number(),
                    ?pending_key,
                    "Reusing pending FRI proof after restart"
                );
                proof_storage
                    .remove_recovered_pending_batch_proof_key(&pending_key)
                    .await;
                return Some(ProvenBatch::pending(stored_batch, pending_key));
            }

            tracing::warn!(
                batch_number = batch.batch_number(),
                ?pending_key,
                "dropping stale pending FRI proof during rehydration"
            );
            proof_storage
                .release_pending_batch_with_proof(&pending_key)
                .await;
        }

        None
    }
}

#[async_trait]
impl PipelineComponent for FriProvingPipelineStep {
    type Input = SignedBatchEnvelope<ProverInput>;
    type Output = ProvenBatch;

    const COMPONENT_ID: zksync_os_pipeline::ComponentId =
        zksync_os_pipeline::ComponentId::FriJobManager;
    const OUTPUT_CHANNEL_CAPACITY: usize = 5;

    async fn run(
        mut self,
        mut input: PeekableReceiver<Self::Input>,
        output: mpsc::Sender<Self::Output>,
        state_reporter: ComponentStateReporter,
    ) -> anyhow::Result<()> {
        let last_proved_batch_number = self.last_proved_batch_number;
        let proof_storage = self.proof_storage.clone();
        let fri_job_manager = self.fri_job_manager.clone();
        let mut batches_with_proof_receiver = self.batches_with_proof_receiver;

        // Forward batches: pipeline input → FriJobManager (add_job) → pipeline output (via proofs channel)
        // Two concurrent tasks handle the bidirectional flow
        tokio::select! {
            result = async {
                while let Some(batch) = input.recv_and_record_picked(&state_reporter).await {
                    if batch.batch_number() > last_proved_batch_number {
                        // SYSCOIN
                        if let Some(stored_batch) = Self::try_rehydrate_pending_batch(&proof_storage, &batch).await {
                            output.send_and_record(stored_batch, &state_reporter)?;
                            continue;
                        }
                        if let Some(stored_batch) = Self::try_rehydrate_batch(&proof_storage, &batch).await {
                            output.send_and_record(ProvenBatch::new(stored_batch), &state_reporter)?;
                            continue;
                        }
                        tracing::info!(
                            "Received batch for FRI proving: {:?}",
                            batch.batch_number()
                        );
                        // Add job directly to FriJobManager - this will await if queue is full
                        fri_job_manager.add_job(batch).await
                    } else {
                        // Already proven - send with fake proof to pass through the pipeline
                        let batch_with_fake_proof = batch.with_data(FriProof::AlreadySubmittedToL1);
                        output
                            .send_and_record(ProvenBatch::new(batch_with_fake_proof), &state_reporter)?;
                    }
                }
                Ok::<(), anyhow::Error>(())
            } => {
                result?;
                tracing::info!("inbound channel closed");
                return Ok(());
            },
            result = async {
                while let Some(proof) = batches_with_proof_receiver.recv().await {
                    tracing::info!(
                        "Received batch after FRI proving: {:?}",
                        proof.batch.batch_number()
                    );
                    output.send_and_record(proof, &state_reporter)?;
                }
                Ok::<(), anyhow::Error>(())
            } => {
                result?;
                tracing::info!("outbound channel closed");
                return Ok(());
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProofStorageConfig;
    use crate::prover_api::proof_storage::StoredBatch;
    use alloy::primitives::{Address, B256};
    use tempfile::TempDir;
    use zksync_os_batch_types::PendingBatchInfo;
    use zksync_os_batch_types::batcher_model::{BatchEnvelope, BatchMetadata, BatchSignatureData};
    use zksync_os_contract_interface::models::{
        CommitBatchInfo, DACommitmentScheme, StoredBatchInfo,
    };
    use zksync_os_observability::ComponentStateReporter;
    use zksync_os_types::{ProtocolSemanticVersion, PubdataMode};

    fn dummy_commit_batch_info(batch_number: u64, from: u64, to: u64) -> CommitBatchInfo {
        CommitBatchInfo {
            batch_number,
            new_state_commitment: B256::ZERO,
            number_of_layer1_txs: 0,
            number_of_layer2_txs: 0,
            priority_operations_hash: B256::ZERO,
            dependency_roots_rolling_hash: B256::ZERO,
            l2_to_l1_logs_root_hash: B256::ZERO,
            l2_da_commitment_scheme: DACommitmentScheme::BlobsAndPubdataKeccak256,
            da_commitment: B256::ZERO,
            first_block_timestamp: 0,
            first_block_number: Some(from),
            last_block_timestamp: 0,
            last_block_number: Some(to),
            chain_id: 270,
            operator_da_input: Vec::new(),
            // SYSCOIN: dummy batches do not include compact edge DA ref openings.
            edge_da_refs_input: Vec::new(),
            // SYSCOIN: dummy batches do not include compact edge DA refs.
            edge_da_refs_root: B256::ZERO,
            sl_chain_id: 123,
        }
    }

    fn dummy_batch_metadata(batch_number: u64, from: u64, to: u64) -> BatchMetadata {
        BatchMetadata {
            previous_stored_batch_info: StoredBatchInfo {
                batch_number: batch_number - 1,
                state_commitment: B256::ZERO,
                number_of_layer1_txs: 0,
                priority_operations_hash: B256::ZERO,
                dependency_roots_rolling_hash: B256::ZERO,
                l2_to_l1_logs_root_hash: B256::ZERO,
                commitment: B256::ZERO,
                last_block_timestamp: Some(0),
            },
            batch_info: PendingBatchInfo {
                commit_info: dummy_commit_batch_info(batch_number, from, to),
                protocol_version: ProtocolSemanticVersion::new(0, 30, 0),
                upgrade_tx_hash: None,
            },
            chain_address: Address::ZERO,
            blob_sidecar: None,
            first_block_number: from,
            last_block_number: to,
            last_block_hash: None,
            pubdata_mode: PubdataMode::Calldata,
            tx_count: 0,
            computational_native_used: None,
            logs: vec![],
            messages: vec![],
            multichain_root: B256::ZERO,
            set_sl_chain_id_migration_number: None,
        }
    }

    fn dummy_input_batch(batch_number: u64) -> SignedBatchEnvelope<ProverInput> {
        BatchEnvelope::new(
            dummy_batch_metadata(batch_number, batch_number * 10, batch_number * 10),
            ProverInput::Fake,
        )
        .with_signatures(BatchSignatureData::NotNeeded)
    }

    async fn proof_storage_for_test() -> anyhow::Result<ProofStorage> {
        let dir = TempDir::new()?;
        let config = ProofStorageConfig {
            path: dir.into_path(),
            ..ProofStorageConfig::default()
        };
        ProofStorage::new(config).await
    }

    #[tokio::test]
    async fn run_does_not_reuse_stored_fake_fri_proof_after_restart() -> anyhow::Result<()> {
        let proof_storage = proof_storage_for_test().await?;
        let input_batch = dummy_input_batch(1);
        let stored_batch = StoredBatch::V1(dummy_input_batch(1).with_data(FriProof::Fake));
        proof_storage.save_batch_with_proof(&stored_batch).await?;

        let (step, job_manager) =
            FriProvingPipelineStep::new(proof_storage, 0, Duration::from_secs(30), 16);

        let (input_tx, input_rx) = mpsc::channel(1);
        let (output_tx, mut output_rx) = mpsc::channel(1);
        let peekable = PeekableReceiver::new(input_rx);

        input_tx.send(input_batch).await?;
        drop(input_tx);

        let (state_reporter, _) = ComponentStateReporter::new("fri_proving_pipeline_step_test");
        let run_handle =
            tokio::spawn(async move { step.run(peekable, output_tx, state_reporter).await });

        run_handle.await.expect("run task should complete")?;
        assert!(output_rx.recv().await.is_none());
        assert_eq!(job_manager.status().await.len(), 1);

        Ok(())
    }

    #[tokio::test]
    async fn run_does_not_reuse_mismatched_stored_fri_proof() -> anyhow::Result<()> {
        let proof_storage = proof_storage_for_test().await?;
        let input_batch = dummy_input_batch(1);
        let mut mismatched_metadata = dummy_batch_metadata(1, 10, 10);
        mismatched_metadata.batch_info.new_state_commitment = B256::repeat_byte(1);
        let mismatched_batch = BatchEnvelope::new(mismatched_metadata, FriProof::Fake)
            .with_signatures(BatchSignatureData::NotNeeded);
        proof_storage
            .save_batch_with_proof(&StoredBatch::V1(mismatched_batch))
            .await?;

        let (step, _job_manager) =
            FriProvingPipelineStep::new(proof_storage, 0, Duration::from_secs(30), 16);

        let (input_tx, input_rx) = mpsc::channel(1);
        let (output_tx, mut output_rx) = mpsc::channel(1);
        let peekable = PeekableReceiver::new(input_rx);

        input_tx.send(input_batch).await?;
        drop(input_tx);

        let (state_reporter, _) = ComponentStateReporter::new("fri_proving_pipeline_step_test");
        let run_handle =
            tokio::spawn(async move { step.run(peekable, output_tx, state_reporter).await });
        run_handle.await.expect("run task should complete")?;

        assert!(output_rx.recv().await.is_none());

        Ok(())
    }
}
