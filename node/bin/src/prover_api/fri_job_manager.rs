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
use crate::prover_api::proof_storage::{ProofStorage, StoredBatch};
use crate::prover_api::prover_job_map::ProverJobMap;
use alloy::primitives::Bytes;
use jsonrpsee::core::Serialize;
use serde::Deserialize;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Permit;
use tokio::sync::mpsc::error::TrySendError;
use zksync_os_l1_sender::batcher_metrics::BatchExecutionStage;
use zksync_os_l1_sender::batcher_model::{
    BatchEnvelope, BatchMetadata, FriProof, ProverInput, RealFriProof, SignedBatchEnvelope,
};
use zksync_os_observability::{
    ComponentStateHandle, ComponentStateReporter, GenericComponentState,
};
use zksync_os_types::ProvingVersion;

// SYSCOIN
const ACCEPTED_PROOF_LOAD_RETRY_DELAY: Duration = Duration::from_secs(1);

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

#[derive(Debug)]
pub struct FriJobManager {
    // == state ==
    jobs: ProverJobMap<ProverInput>,
    // outbound
    batches_with_proof_sender: mpsc::Sender<SignedBatchEnvelope<FriProof>>,
    // SYSCOIN
    accepted_proof_sender: mpsc::UnboundedSender<u64>,
    // == storage ==
    proof_storage: ProofStorage,
    // == metrics ==
    latency_tracker: ComponentStateHandle<GenericComponentState>,
}

impl FriJobManager {
    pub fn new(
        batches_with_proof_sender: mpsc::Sender<SignedBatchEnvelope<FriProof>>,
        proof_storage: ProofStorage,
        assignment_timeout: Duration,
        max_assigned_batch_range: usize,
    ) -> Self {
        let jobs = ProverJobMap::<ProverInput>::new(
            assignment_timeout,
            max_assigned_batch_range,
            ProverStage::Fri,
        );
        let latency_tracker = ComponentStateReporter::global().handle_for(
            "fri_job_manager",
            GenericComponentState::ProcessingOrWaitingRecv,
        );
        // SYSCOIN
        let (accepted_proof_sender, mut accepted_proof_receiver) = mpsc::unbounded_channel();
        let proof_storage_for_forwarder = proof_storage.clone();
        let downstream_sender = batches_with_proof_sender.clone();
        tokio::spawn(async move {
            while let Some(batch_number) = accepted_proof_receiver.recv().await {
                let stored_batch = loop {
                    match proof_storage_for_forwarder
                        .get_batch_with_proof(batch_number)
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
                    tokio::time::sleep(ACCEPTED_PROOF_LOAD_RETRY_DELAY).await;
                };

                if downstream_sender.send(stored_batch).await.is_err() {
                    tracing::info!("accepted FRI proof downstream channel closed");
                    return;
                }
            }
        });

        Self {
            jobs,
            batches_with_proof_sender,
            accepted_proof_sender,
            proof_storage,
            latency_tracker,
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
        // storage failures leave the job retriable. Forwarding only records the batch
        // number; the forwarder reloads the proof from disk before sending downstream.
        let proof = RealFriProof::V2 {
            proof: proof_bytes,
            proving_execution_version: proving_version as u32,
        };
        let stored_batch = StoredBatch::V1(
            BatchEnvelope {
                batch: batch_metadata.clone(),
                data: FriProof::Real(proof),
                signature_data,
                latency_tracker: Default::default(),
            }
            .with_stage(BatchExecutionStage::FriProvedReal),
        );
        self.proof_storage
            .save_batch_with_proof(&stored_batch)
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
            return Ok(());
        };
        drop(removed_job);

        self.enqueue_accepted_proof(batch_number)?;

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
                    batch_metadata
                        .batch_info
                        .clone()
                        .into_stored(&batch_metadata.protocol_version),
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

        permit.send(envelope);
        Ok(())
    }

    pub async fn status(&self) -> Vec<JobState> {
        self.jobs.status().await
    }

    // SYSCOIN
    fn enqueue_accepted_proof(&self, batch_number: u64) -> Result<(), SubmitError> {
        self.accepted_proof_sender
            .send(batch_number)
            .map_err(|_| SubmitError::Other("server is shutting down".to_string()))
    }

    fn try_reserve_permit_downstream(
        &self,
    ) -> Result<Permit<'_, SignedBatchEnvelope<FriProof>>, SubmitError> {
        Ok(match self.batches_with_proof_sender.try_reserve() {
            Ok(permit) => {
                self.latency_tracker
                    .enter_state(GenericComponentState::ProcessingOrWaitingRecv);
                permit
            }
            Err(TrySendError::Full(_)) => {
                self.latency_tracker
                    .enter_state(GenericComponentState::WaitingSend);
                return Err(SubmitError::Other("downstream backpressure".to_string()));
            }
            Err(TrySendError::Closed(_)) => {
                return Err(SubmitError::Other("server is shutting down".to_string()));
            }
        })
    }
}
