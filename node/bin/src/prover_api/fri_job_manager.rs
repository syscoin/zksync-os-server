//! Concurrent in‑memory queue for FRI prover work.
//!
//! * Incoming jobs are received via `add_job`.
//!   No more than `max_assigned_batch_range` batch span is accepted
//! * Assigned jobs are added to `ProverJobMap` immediately.
//! * Provers request work via [`pick_next_job`]:
//!     * If there is an already assigned job that has timed out, it is reassigned.
//!     * Otherwise, the next job from inbound is assigned and inserted into `ProverJobMap`.
//! * Fake provers call [`pick_next_job`] with a `min_age` param to avoid taking fresh items,
//!   letting real provers race first.
//! * When any proof is submitted (real or fake):
//!     * It is removed from `ProverJobMap`
//!     * It is enqueued to the ordered committer as `SignedBatchEnvelope<FriProof>`.
//!

use crate::prover_api::fri_proof_verifier;
use crate::prover_api::metrics::{ProverStage, ProverType};
use crate::prover_api::proof_storage::{
    PendingBatchProofKey, ProofStorage, ProvenBatch, StoredBatch,
};
use crate::prover_api::prover_job_map::ProverJobMap;
use alloy::primitives::Bytes;
use jsonrpsee::core::Serialize;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;
use zksync_os_batch_types::batcher_model::{
    BatchEnvelope, BatchMetadata, FriProof, ProverInput, RealFriProof, SignedBatchEnvelope,
};
use zksync_os_batcher_metrics::BatchExecutionStage;
use zksync_os_types::ProvingVersion;

// SYSCOIN
#[cfg(not(test))]
const ACCEPTED_PROOF_LOAD_RETRY_DELAY: Duration = Duration::from_secs(1);
// SYSCOIN
#[cfg(test)]
const ACCEPTED_PROOF_LOAD_RETRY_DELAY: Duration = Duration::from_millis(1);
// SYSCOIN
#[cfg(not(test))]
const ACCEPTED_PROOF_LOAD_MAX_ATTEMPTS: usize = 60;
// SYSCOIN
#[cfg(test)]
const ACCEPTED_PROOF_LOAD_MAX_ATTEMPTS: usize = 2;

#[derive(Error, Debug)]
pub enum SubmitError {
    #[error("FRI proof verification error")]
    FriProofVerificationError {
        expected_hash_u32s: [u32; 8],
        proof_final_register_values: [u32; 16],
    },
    #[error("batch {0} is not known to the server")]
    UnknownJob(u64),
    #[error("deserialization failed: {0:?}")]
    DeserializationFailed(bincode::error::DecodeError),
    // server execution version, prover execution version
    #[error("execution error mismatch - server expects {0:?}, but got {1:?} from prover")]
    ProvingVersionMismatch(ProvingVersion, ProvingVersion),
    #[error("server is shutting down")]
    ShuttingDown,
    #[error("internal error: {0}")]
    Other(String),
}

/// A FRI proof that failed verification, stored for debugging purposes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedFriProof {
    pub batch_number: u64,
    pub last_block_timestamp: u64,
    pub expected_hash_u32s: [u32; 8],
    pub proof_final_register_values: [u32; 16],
    pub vk_hash: String,
    pub proof_bytes: Bytes,
}

#[derive(Clone, Debug, Serialize)]
pub struct FriJob {
    pub batch_number: u64,
    pub vk_hash: String,
}

#[derive(Debug, Serialize)]
pub struct JobState {
    pub fri_job: FriJob,
    pub added_seconds_ago: u64,
    pub assigned_seconds_ago: Option<u64>,
    pub assigned_to_prover_id: Option<String>,
    pub current_attempt: usize,
}

// SYSCOIN
#[derive(Debug)]
struct AcceptedProof {
    batch_number: u64,
    proof_key: PendingBatchProofKey,
    batch_envelope: SignedBatchEnvelope<ProverInput>,
}

#[derive(Debug)]
pub struct FriJobManager {
    // == state ==
    jobs: Arc<ProverJobMap<ProverInput>>,
    // outbound
    batches_with_proof_sender: mpsc::Sender<ProvenBatch>,
    // SYSCOIN
    accepted_proof_sender: mpsc::Sender<AcceptedProof>,
    // == storage ==
    proof_storage: ProofStorage,
}

impl FriJobManager {
    pub fn new(
        batches_with_proof_sender: mpsc::Sender<ProvenBatch>,
        proof_storage: ProofStorage,
        assignment_timeout: Duration,
        max_assigned_batch_range: usize,
    ) -> Self {
        let jobs = Arc::new(ProverJobMap::<ProverInput>::new(
            assignment_timeout,
            max_assigned_batch_range,
            ProverStage::Fri,
        ));
        // SYSCOIN
        let (accepted_proof_sender, mut accepted_proof_receiver) =
            mpsc::channel::<AcceptedProof>(5);
        let proof_storage_for_forwarder = proof_storage.clone();
        let downstream_sender = batches_with_proof_sender.clone();
        let jobs_for_forwarder = jobs.clone();
        tokio::spawn(async move {
            'forwarder: loop {
                let accepted_proof = match accepted_proof_receiver.recv().await {
                    Some(accepted_proof) => accepted_proof,
                    None => return,
                };
                let AcceptedProof {
                    batch_number,
                    proof_key,
                    mut batch_envelope,
                } = accepted_proof;
                let mut load_attempts = 0;
                let mut stored_batch = loop {
                    match proof_storage_for_forwarder
                        .get_pending_batch_with_proof(&proof_key)
                        .await
                    {
                        Ok(Some(stored_batch)) => break stored_batch,
                        Ok(None) => {
                            tracing::error!(
                                batch_number,
                                retry_in = ?ACCEPTED_PROOF_LOAD_RETRY_DELAY,
                                "accepted FRI proof missing from proof storage; retrying"
                            );
                        }
                        Err(err) => {
                            tracing::error!(
                                batch_number,
                                ?err,
                                retry_in = ?ACCEPTED_PROOF_LOAD_RETRY_DELAY,
                                "failed to load accepted FRI proof from proof storage; retrying"
                            );
                        }
                    }
                    load_attempts += 1;
                    if load_attempts >= ACCEPTED_PROOF_LOAD_MAX_ATTEMPTS {
                        tracing::error!(
                            batch_number,
                            attempts = load_attempts,
                            "accepted FRI proof could not be loaded; quarantining pending proof"
                        );
                        proof_storage_for_forwarder
                            .quarantine_pending_batch_with_proof(&proof_key)
                            .await;
                        jobs_for_forwarder.restore_job(batch_envelope).await;
                        continue 'forwarder;
                    }
                    tokio::time::sleep(ACCEPTED_PROOF_LOAD_RETRY_DELAY).await;
                };
                stored_batch.latency_tracker = std::mem::take(&mut batch_envelope.latency_tracker);

                if downstream_sender
                    .send(ProvenBatch::pending(stored_batch, proof_key.clone()))
                    .await
                    .is_err()
                {
                    accepted_proof_receiver.close();
                    proof_storage_for_forwarder
                        .release_pending_batch_with_proof(&proof_key)
                        .await;
                    jobs_for_forwarder.restore_job(batch_envelope).await;
                    while let Ok(queued_proof) = accepted_proof_receiver.try_recv() {
                        proof_storage_for_forwarder
                            .release_pending_batch_with_proof(&queued_proof.proof_key)
                            .await;
                        jobs_for_forwarder
                            .restore_job(queued_proof.batch_envelope)
                            .await;
                    }
                    tracing::info!(
                        "accepted FRI proof downstream channel closed; restored jobs for retry"
                    );
                    return;
                }
            }
        });

        Self {
            jobs,
            batches_with_proof_sender,
            accepted_proof_sender,
            proof_storage,
        }
    }

    /// Adds a pending job to the queue.
    /// Awaits if the queue is full (ProverJobMap.max_assigned_batch_range).
    pub async fn add_job(&self, batch_envelope: SignedBatchEnvelope<ProverInput>) {
        self.jobs.add_job(batch_envelope).await
    }

    /// Peek batch data for a given batch number
    pub async fn peek_batch_data(&self, batch_number: u64) -> Option<(&str, ProverInput)> {
        match self.jobs.get_prover_input(batch_number).await {
            Some((vk_hash, prover_input)) => {
                tracing::info!("Batch data is peeked for batch number {batch_number}");
                Some((vk_hash, prover_input))
            }
            None => {
                tracing::debug!(
                    "Trying to peek batch number {batch_number} that is not present in the queue"
                );
                None
            }
        }
    }

    /// Picks the oldest batch that is either pending and old enough
    /// or whose assignment has timed‑out.
    ///
    /// `min_age` is used for fake provers to avoid taking fresh items,
    /// letting real provers race first.
    pub async fn pick_next_job(
        &self,
        min_age: Duration,
        prover_id: String,
    ) -> Option<(FriJob, ProverInput)> {
        self.jobs.pick_job(min_age, &prover_id).await
    }

    /// Submit a **real** proof provided by an external prover.
    /// On success the entry is removed from the assigned map.
    pub async fn submit_proof(
        &self,
        batch_number: u64,
        proof_bytes: Bytes,
        proving_version: ProvingVersion,
        prover_id: &str,
    ) -> Result<(), SubmitError> {
        // Snapshot the assigned job entry (if any).
        let (batch_metadata, signature_data) = match self
            .jobs
            .get_job_batch_metadata_and_signature(batch_number)
            .await
        {
            Some(e) => e,
            None => return Err(SubmitError::UnknownJob(batch_number)),
        };

        // Prover should generate the proof with VK received from server. These must always match.
        // If they don't, proof won't be accepted, validation will fail, therefore it's pointless to proceed.
        //
        // This should never happen, but we double-check to guarantee it's the case.
        //
        // NOTE: We don't check the actual values, but the value that server believes the prover should use.
        let server_proving_version = batch_metadata
            .proving_version()
            .expect("Must be valid execution as set by the server");

        if server_proving_version != proving_version {
            return Err(SubmitError::ProvingVersionMismatch(
                server_proving_version,
                proving_version,
            ));
        }

        self.verify_proof(&batch_metadata, &proof_bytes, batch_number, prover_id)
            .await?;

        // SYSCOIN: Persist the accepted proof before removing the in-memory job, so
        // storage failures leave the job retriable. Forwarding records the batch number
        // and tracker; the forwarder reloads the proof from disk before sending downstream.
        let proof = RealFriProof::V2 {
            proof: proof_bytes,
            proving_execution_version: proving_version as u32,
        };
        let stored_batch = StoredBatch::V1(BatchEnvelope {
            batch: batch_metadata.clone(),
            data: FriProof::Real(proof),
            signature_data,
            latency_tracker: Default::default(),
        });
        let pending_proof_key = self
            .proof_storage
            .save_pending_batch_with_proof(&stored_batch)
            .await
            .map_err(|err| SubmitError::Other(format!("failed to persist FRI proof: {err}")))?;

        let Some(removed_job) = self
            .jobs
            .complete_job(batch_number, ProverType::Real, prover_id)
            .await
        else {
            // If already removed due to a race
            // (another submit won), we still return success to keep the API idempotent.
            tracing::warn!(
                batch_number,
                prover_id,
                "Job already removed (racing submit)"
            );
            self.release_pending_batch_with_proof_in_background(pending_proof_key);
            return Ok(());
        };
        let completed_job = removed_job.with_stage(BatchExecutionStage::FriProvedReal);

        // SYSCOIN: The accepted-proof queue is bounded. Use `try_send` so there is no cancellation
        // point after the job is completed; if forwarding is saturated, restore the job and release
        // the pending proof in the background for a clean retry.
        match self.accepted_proof_sender.try_send(AcceptedProof {
            batch_number,
            proof_key: pending_proof_key.clone(),
            batch_envelope: completed_job,
        }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(accepted_proof)) => {
                // SYSCOIN: Move cleanup into an owned task before awaiting; dropping this request
                // future must not drop the completed job before it is restored.
                let restore_task = self
                    .restore_job_and_release_pending_batch_with_proof_in_background(accepted_proof);
                let _ = restore_task.await;
                return Err(SubmitError::Other(
                    "accepted FRI proof forwarder backpressure".to_string(),
                ));
            }
            Err(mpsc::error::TrySendError::Closed(accepted_proof)) => {
                // SYSCOIN: Same as the backpressure path: cleanup must outlive request
                // cancellation once the job has been completed.
                let restore_task = self
                    .restore_job_and_release_pending_batch_with_proof_in_background(accepted_proof);
                let _ = restore_task.await;
                return Err(SubmitError::ShuttingDown);
            }
        }

        Ok(())
    }

    /// Verifies the proof and handles failed proofs by saving them for debugging.
    /// Returns Ok(()) if the proof is valid, or an error if verification fails.
    async fn verify_proof(
        &self,
        batch_metadata: &BatchMetadata,
        proof_bytes: &Bytes,
        batch_number: u64,
        prover_id: &str,
    ) -> Result<(), SubmitError> {
        // TODO: This match is needed for the transition period.
        // v0.5.2 airbender cannot verify proofs generated with v0.5.1.
        // Once all networks are protocol upgraded, the code below can be removed.
        let proving_version = batch_metadata
            .proving_version()
            // should be safe to unwrap, as it's been checked before this call
            .expect("invalid proving version");
        let result = match proving_version {
            ProvingVersion::V1
            | ProvingVersion::V2
            | ProvingVersion::V3
            | ProvingVersion::V4
            | ProvingVersion::V5 => {
                panic!("proof verification for v1-v5 is not supported")
            }
            ProvingVersion::V6 | ProvingVersion::V7 => {
                tracing::debug!(
                    ?proving_version,
                    batch_number,
                    "Verifying FRI proof against expected batch public input"
                );
                // SYSCOIN
                fri_proof_verifier::verify_real_fri_proof_bytes(
                    batch_metadata.previous_stored_batch_info.state_commitment,
                    batch_metadata.batch_info.clone().into_stored(),
                    proof_bytes,
                )
            }
        };

        if let Err(SubmitError::FriProofVerificationError {
            expected_hash_u32s,
            proof_final_register_values,
        }) = result
        {
            tracing::warn!(
                batch_number,
                expected = ?expected_hash_u32s,
                actual = ?proof_final_register_values,
                "Proof verification failed",
            );

            // Persist the failed proof with some information about the batch for debugging
            let failed_proof = FailedFriProof {
                batch_number,
                last_block_timestamp: batch_metadata.batch_info.commit_info.last_block_timestamp,
                expected_hash_u32s,
                proof_final_register_values,
                vk_hash: batch_metadata
                    .verification_key_hash()
                    .expect("VK must exist")
                    .to_string(),
                proof_bytes: proof_bytes.clone(),
            };

            if let Err(save_err) = self.proof_storage.save_failed_proof(&failed_proof).await {
                tracing::error!(
                    batch_number,
                    ?save_err,
                    "Failed to persist failed proof for debugging",
                );
            } else {
                tracing::info!(batch_number, prover_id, "Failed proof saved for debugging",);
            }

            return Err(SubmitError::FriProofVerificationError {
                expected_hash_u32s,
                proof_final_register_values,
            });
        }

        Ok(())
    }

    /// Submit a **fake** proof on behalf of a fake prover worker.
    /// Entry is removed from the assigned map.
    pub async fn submit_fake_proof(
        &self,
        batch_number: u64,
        prover_id: &'static str,
    ) -> Result<(), SubmitError> {
        // We want to ensure we can send the result downstream before we remove the job
        let permit = self.try_reserve_permit_downstream()?;

        // Downstream has capacity - we remove the job from `assigned_jobs`.
        let assigned = match self
            .jobs
            .complete_job(batch_number, ProverType::Fake, prover_id)
            .await
        {
            Some(e) => e,
            None => return Err(SubmitError::UnknownJob(batch_number)),
        };

        let envelope = assigned
            .with_data(FriProof::Fake)
            .with_stage(BatchExecutionStage::FriProvedFake);

        permit.send(ProvenBatch::new(envelope));
        Ok(())
    }

    pub async fn status(&self) -> Vec<JobState> {
        self.jobs.status().await
    }

    // SYSCOIN
    fn release_pending_batch_with_proof_in_background(
        &self,
        pending_proof_key: PendingBatchProofKey,
    ) {
        let proof_storage = self.proof_storage.clone();
        tokio::spawn(async move {
            proof_storage
                .release_pending_batch_with_proof(&pending_proof_key)
                .await;
        });
    }

    // SYSCOIN
    fn restore_job_and_release_pending_batch_with_proof_in_background(
        &self,
        accepted_proof: AcceptedProof,
    ) -> tokio::task::JoinHandle<()> {
        let jobs = self.jobs.clone();
        let proof_storage = self.proof_storage.clone();
        tokio::spawn(async move {
            jobs.restore_job(accepted_proof.batch_envelope).await;
            proof_storage
                .release_pending_batch_with_proof(&accepted_proof.proof_key)
                .await;
        })
    }

    fn try_reserve_permit_downstream(&self) -> Result<mpsc::Permit<'_, ProvenBatch>, SubmitError> {
        match self.batches_with_proof_sender.try_reserve() {
            Ok(permit) => Ok(permit),
            Err(mpsc::error::TrySendError::Full(_)) => {
                Err(SubmitError::Other("downstream backpressure".to_string()))
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(SubmitError::ShuttingDown),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProofStorageConfig;
    use alloy::primitives::{Address, B256};
    use tempfile::TempDir;
    use zksync_os_batch_types::PendingBatchInfo;
    use zksync_os_batch_types::batcher_model::{BatchSignatureData, ProverInput};
    use zksync_os_contract_interface::models::{
        CommitBatchInfo, DACommitmentScheme, StoredBatchInfo,
    };
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
            // SYSCOIN
            edge_da_refs_input: Vec::new(),
            // SYSCOIN
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
            path: dir.keep(),
            ..ProofStorageConfig::default()
        };
        ProofStorage::new(config).await
    }

    #[tokio::test]
    async fn cancelled_restore_handle_still_restores_fri_job() -> anyhow::Result<()> {
        let proof_storage = proof_storage_for_test().await?;
        let (downstream_tx, _downstream_rx) = mpsc::channel(1);
        let manager = FriJobManager::new(
            downstream_tx,
            proof_storage.clone(),
            Duration::from_secs(30),
            16,
        );

        manager.add_job(dummy_input_batch(1)).await;
        let stored_batch = StoredBatch::V1(dummy_input_batch(1).with_data(FriProof::Fake));
        let pending_key = proof_storage
            .save_pending_batch_with_proof(&stored_batch)
            .await?;
        let completed_job = manager
            .jobs
            .complete_job(1, ProverType::Real, "prover-1")
            .await
            .expect("job should exist")
            .with_stage(BatchExecutionStage::FriProvedReal);
        assert!(manager.status().await.is_empty());

        let restore_handle = manager
            .restore_job_and_release_pending_batch_with_proof_in_background(AcceptedProof {
                batch_number: 1,
                proof_key: pending_key.clone(),
                batch_envelope: completed_job,
            });
        drop(restore_handle);

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if proof_storage
                    .get_pending_batch_with_proof(&pending_key)
                    .await
                    .expect("pending proof lookup should not fail")
                    .is_none()
                    && manager.status().await.len() == 1
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await?;

        Ok(())
    }

    #[tokio::test]
    async fn unloadable_accepted_proof_restores_fri_job() -> anyhow::Result<()> {
        let proof_storage = proof_storage_for_test().await?;
        let (downstream_tx, mut downstream_rx) = mpsc::channel(1);
        let manager = FriJobManager::new(
            downstream_tx,
            proof_storage.clone(),
            Duration::from_secs(30),
            16,
        );
        let input_batch = dummy_input_batch(1);

        manager.add_job(input_batch).await;
        let stored_batch = StoredBatch::V1(dummy_input_batch(1).with_data(FriProof::Fake));
        let pending_key = proof_storage
            .save_pending_batch_with_proof(&stored_batch)
            .await?;

        // SYSCOIN: Simulate the pending file disappearing after proof acceptance. The forwarder
        // must restore the FRI job instead of dropping the batch and creating a permanent gap.
        proof_storage
            .release_pending_batch_with_proof(&pending_key)
            .await;
        let completed_job = manager
            .jobs
            .complete_job(1, ProverType::Real, "prover-1")
            .await
            .expect("job should exist")
            .with_stage(BatchExecutionStage::FriProvedReal);
        assert!(manager.status().await.is_empty());
        manager
            .accepted_proof_sender
            .send(AcceptedProof {
                batch_number: 1,
                proof_key: pending_key,
                batch_envelope: completed_job,
            })
            .await?;

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if manager.status().await.len() == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await?;

        assert!(matches!(
            downstream_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        Ok(())
    }

    #[tokio::test]
    async fn downstream_close_restores_completed_fri_job() -> anyhow::Result<()> {
        let proof_storage = proof_storage_for_test().await?;
        let (downstream_tx, downstream_rx) = mpsc::channel(1);
        drop(downstream_rx);
        let manager = FriJobManager::new(
            downstream_tx,
            proof_storage.clone(),
            Duration::from_secs(30),
            16,
        );

        manager.add_job(dummy_input_batch(1)).await;
        let stored_batch = StoredBatch::V1(dummy_input_batch(1).with_data(FriProof::Fake));
        let pending_key = proof_storage
            .save_pending_batch_with_proof(&stored_batch)
            .await?;
        let completed_job = manager
            .jobs
            .complete_job(1, ProverType::Real, "prover-1")
            .await
            .expect("job should exist")
            .with_stage(BatchExecutionStage::FriProvedReal);
        assert!(manager.status().await.is_empty());

        manager
            .accepted_proof_sender
            .send(AcceptedProof {
                batch_number: 1,
                proof_key: pending_key.clone(),
                batch_envelope: completed_job,
            })
            .await?;

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if proof_storage
                    .get_pending_batch_with_proof(&pending_key)
                    .await
                    .expect("pending proof lookup should not fail")
                    .is_none()
                    && manager.status().await.len() == 1
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await?;

        Ok(())
    }
}
