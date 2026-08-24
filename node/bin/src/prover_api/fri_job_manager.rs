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

use crate::prover_api::metrics::ProverStage;
use crate::prover_api::proof_storage::{
    PendingBatchProofKey, ProofStorage, ProvenBatch, StoredBatch,
};
use crate::prover_api::prover_job_map::{
    BeginSubmissionError, LeasedJob, ProverJobMap, ReservedFriJob, SubmissionLease,
};
use crate::prover_api::{fri_input_fits_response_contract, fri_proof_verifier};
use alloy::primitives::Bytes;
use base64::{Engine as _, engine::general_purpose};
use jsonrpsee::core::Serialize;
use serde::Deserialize;
use std::future::Future;
use std::sync::Arc;
#[cfg(not(test))]
use std::sync::LazyLock;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::{Semaphore, mpsc};
use zksync_os_batch_types::batcher_model::{
    BatchEnvelope, BatchMetadata, FriProof, ProverInput, RealFriProof, SignedBatchEnvelope,
};
use zksync_os_batcher_metrics::BatchExecutionStage;
use zksync_os_types::ProvingVersion;

// SYSCOIN: Retry durable accepted-proof reads long enough for transient filesystem visibility.
#[cfg(not(test))]
const ACCEPTED_PROOF_LOAD_RETRY_DELAY: Duration = Duration::from_secs(1);
// SYSCOIN: Keep the same retry path fast in unit tests.
#[cfg(test)]
const ACCEPTED_PROOF_LOAD_RETRY_DELAY: Duration = Duration::from_millis(1);
// SYSCOIN: Bound accepted-proof recovery before restoring the original proving job.
#[cfg(not(test))]
const ACCEPTED_PROOF_LOAD_MAX_ATTEMPTS: usize = 60;
// SYSCOIN: Exercise exhaustion without a minute-long unit test.
#[cfg(test)]
const ACCEPTED_PROOF_LOAD_MAX_ATTEMPTS: usize = 2;
// SYSCOIN: Bound verifier CPU independently of HTTP concurrency and fail fast when saturated.
const MAX_CONCURRENT_FRI_VERIFICATIONS: usize = 3;
// SYSCOIN: Reserve durable-handoff capacity before verification so a valid proof is never consumed
// and then rejected merely because the in-process accepted-proof queue filled concurrently.
const ACCEPTED_PROOF_QUEUE_CAPACITY: usize = 5;
// SYSCOIN: Share the bound across every production manager/router in this process. Tests use an
// isolated semaphore per fixture so independently scheduled regression cases cannot interfere.
#[cfg(not(test))]
static FRI_VERIFICATION_SEMAPHORE: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_CONCURRENT_FRI_VERIFICATIONS)));

#[derive(Error, Debug)]
pub enum SubmitError {
    #[error("FRI proof verification error")]
    FriProofVerificationError {
        expected_hash_u32s: [u32; 8],
        proof_final_register_values: [u32; 16],
    },
    #[error("batch {0} is not known to the server")]
    UnknownJob(u64),
    // SYSCOIN: Public prover IDs and ranges never authorize submission without the pick token.
    #[error("invalid or stale prover lease")]
    InvalidLease,
    // SYSCOIN: The same authenticated proof is already being processed; this is not stale.
    #[error("this prover lease already has a submission in progress; retry later")]
    SubmissionInProgress,
    // SYSCOIN: Do not queue unbounded blocking verifier work behind attacker-controlled requests.
    #[error("FRI verification capacity is busy; retry this lease later")]
    VerificationBusy,
    // SYSCOIN: This response retains the exact lease and is safe to retry with identical bytes.
    #[error("FRI accepted-proof handoff capacity is busy; retry this lease later")]
    AcceptedProofBackpressure,
    // SYSCOIN: Durable persistence failed before completion, so the same token remains valid.
    #[error("temporary FRI proof persistence failure: {0}")]
    TemporaryStorage(String),
    #[error("deserialization failed: {0:?}")]
    DeserializationFailed(bincode::error::DecodeError),
    // SYSCOIN: Malformed encoded proof from the authenticated lease owner is definitive.
    #[error("invalid base64 FRI proof: {0}")]
    InvalidBase64(base64::DecodeError),
    // SYSCOIN: Wrapper-incompatible map shapes are rejected before native verification.
    #[error("invalid V8 proof shape: {0}")]
    InvalidProofShape(String),
    // SYSCOIN: Parse the claimed VK only after exact-token admission; this rejection is therefore
    // a definitive disposition for the durable capability owner.
    #[error("no Proving Version matches the provided verification key: {0}")]
    UnknownVerificationKey(String),
    // server execution version, prover execution version
    #[error("execution error mismatch - server expects {0:?}, but got {1:?} from prover")]
    ProvingVersionMismatch(ProvingVersion, ProvingVersion),
    #[error("server is shutting down")]
    ShuttingDown,
    // SYSCOIN: Internal verifier execution failed before exact completion; release retains token.
    #[error("temporary internal verifier error: {0}")]
    TemporaryInternal(String),
    // SYSCOIN: An owned handoff task ended unexpectedly. Retry converges safely: the same token is
    // admitted if completion did not occur, or returns definitive 409 after exact completion.
    #[error("ambiguous internal handoff error: {0}")]
    AmbiguousHandoff(String),
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

// SYSCOIN: Track the durable pending key until the accepted FRI proof reaches the next stage.
#[derive(Debug)]
struct AcceptedProof {
    batch_number: u64,
    proof_key: PendingBatchProofKey,
    // SYSCOIN: Retain the removed prover input and its endpoint-capacity fence until the
    // forwarder either transfers durable proof ownership or restores the exact job.
    reserved_job: ReservedFriJob<ProverInput>,
    // SYSCOIN: Couples this queued message to capacity reserved before expensive verification.
    queue_permit: tokio::sync::OwnedSemaphorePermit,
}

#[derive(Clone, Debug)]
pub struct FriJobManager {
    // == state ==
    jobs: Arc<ProverJobMap<ProverInput>>,
    // outbound
    batches_with_proof_sender: mpsc::Sender<ProvenBatch>,
    // SYSCOIN: Serialize accepted-proof durability and downstream handoff independently of API requests.
    accepted_proof_sender: mpsc::Sender<AcceptedProof>,
    // SYSCOIN: Mirrors the accepted-proof channel capacity but can be reserved before job removal.
    accepted_proof_capacity: Arc<Semaphore>,
    // SYSCOIN: Process-wide manager bound for CPU-heavy FRI verification tasks.
    verification_semaphore: Arc<Semaphore>,
    #[cfg(test)]
    verification_invocations: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    proof_decode_invocations: Arc<std::sync::atomic::AtomicUsize>,
    // == storage ==
    proof_storage: ProofStorage,
}

impl FriJobManager {
    pub fn new(
        batches_with_proof_sender: mpsc::Sender<ProvenBatch>,
        proof_storage: ProofStorage,
        assignment_timeout: Duration,
        max_assigned_batch_range: usize,
    ) -> (Self, impl Future<Output = ()> + Send + 'static) {
        let jobs = Arc::new(ProverJobMap::<ProverInput>::new(
            assignment_timeout,
            max_assigned_batch_range,
            ProverStage::Fri,
        ));
        // SYSCOIN: Drain durable accepted proofs in a background handoff queue so external FRI
        // submitters are not coupled to SNARK-stage backpressure.
        let (accepted_proof_sender, mut accepted_proof_receiver) =
            mpsc::channel::<AcceptedProof>(ACCEPTED_PROOF_QUEUE_CAPACITY);
        let accepted_proof_capacity = Arc::new(Semaphore::new(ACCEPTED_PROOF_QUEUE_CAPACITY));
        let proof_storage_for_forwarder = proof_storage.clone();
        let downstream_sender = batches_with_proof_sender.clone();
        // SYSCOIN: Return this long-lived future to the node runtime so production can supervise it
        // as a critical task. A detached forwarder could die while completed jobs remain durable
        // but invisible until restart.
        let accepted_proof_forwarder = async move {
            'forwarder: loop {
                let accepted_proof = match accepted_proof_receiver.recv().await {
                    Some(accepted_proof) => accepted_proof,
                    None => return,
                };
                let AcceptedProof {
                    batch_number,
                    proof_key,
                    mut reserved_job,
                    queue_permit,
                } = accepted_proof;
                // SYSCOIN: The channel slot has now been removed; release its mirrored reservation
                // before potentially slow storage/downstream work.
                drop(queue_permit);
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
                        reserved_job.rollback().await;
                        continue 'forwarder;
                    }
                    tokio::time::sleep(ACCEPTED_PROOF_LOAD_RETRY_DELAY).await;
                };
                if !reserved_job.matches_batch_metadata(&stored_batch) {
                    tracing::error!(
                        expected_batch_number = batch_number,
                        loaded_batch_number = stored_batch.batch_number(),
                        ?proof_key,
                        "accepted FRI pending file no longer matches its reserved canonical batch; quarantining"
                    );
                    proof_storage_for_forwarder
                        .quarantine_pending_batch_with_proof(&proof_key)
                        .await;
                    reserved_job.rollback().await;
                    continue 'forwarder;
                }
                stored_batch.latency_tracker =
                    std::mem::take(&mut reserved_job.batch_envelope_mut().latency_tracker);

                if downstream_sender
                    .send(ProvenBatch::pending(stored_batch, proof_key.clone()))
                    .await
                    .is_err()
                {
                    accepted_proof_receiver.close();
                    proof_storage_for_forwarder
                        .release_pending_batch_with_proof(&proof_key)
                        .await;
                    reserved_job.rollback().await;
                    while let Ok(queued_proof) = accepted_proof_receiver.try_recv() {
                        proof_storage_for_forwarder
                            .release_pending_batch_with_proof(&queued_proof.proof_key)
                            .await;
                        queued_proof.reserved_job.rollback().await;
                    }
                    tracing::info!(
                        "accepted FRI proof downstream channel closed; restored jobs for retry"
                    );
                    return;
                }
                // SYSCOIN: A successful channel transfer is the same terminal ownership boundary
                // used before reservations existed; release only this exact endpoint fence.
                reserved_job.commit().await;
            }
        };

        (
            Self {
                jobs,
                batches_with_proof_sender,
                accepted_proof_sender,
                accepted_proof_capacity,
                #[cfg(not(test))]
                verification_semaphore: Arc::clone(&FRI_VERIFICATION_SEMAPHORE),
                #[cfg(test)]
                verification_semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_FRI_VERIFICATIONS)),
                #[cfg(test)]
                verification_invocations: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                #[cfg(test)]
                proof_decode_invocations: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                proof_storage,
            },
            accepted_proof_forwarder,
        )
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
    ///
    /// `supported_proving_versions` restricts assignment to batches of these versions;
    /// `None` means the prover declared nothing and any batch qualifies.
    ///
    /// SYSCOIN: `maximum_response_bytes` is the worker capacity clamped by the HTTP handler. The
    /// existing atomic predicate skips inputs that cannot fit before creating a bearer lease.
    pub async fn pick_next_job(
        &self,
        min_age: Duration,
        prover_id: String,
        supported_proving_versions: Option<&[ProvingVersion]>,
        maximum_response_bytes: usize,
    ) -> Option<LeasedJob<ProverInput>> {
        self.jobs
            .pick_job(min_age, &prover_id, |job| {
                supported_proving_versions
                    .is_none_or(|versions| versions.contains(&job.metadata.proving_version))
                    && fri_input_fits_response_contract(
                        &job.batch_envelope.data,
                        maximum_response_bytes,
                    )
            })
            .await
    }

    /// Submit a **real** proof provided by an external prover.
    /// On success the entry is removed from the assigned map.
    pub async fn submit_proof(
        &self,
        batch_number: u64,
        encoded_proof: String,
        vk_hash: String,
        prover_id: &str,
        lease_token: &str,
    ) -> Result<(), SubmitError> {
        // SYSCOIN: Authenticate and atomically mark the exact lease before any expensive proof
        // parsing or verification. Duplicate/stale requests stop here and consume zero verifier CPU.
        let submission = self
            .jobs
            .begin_submission(batch_number, batch_number, lease_token)
            .await
            .map_err(|error| match error {
                BeginSubmissionError::AlreadySubmitting => SubmitError::SubmissionInProgress,
                _ => SubmitError::InvalidLease,
            })?;
        // SYSCOIN: Unknown VK parsing is cheap but still belongs behind lease admission; otherwise
        // a generic pre-manager 400 could make a durable prover discard live proof ownership.
        let proving_version = match ProvingVersion::try_from_vk_hash(&vk_hash) {
            Ok(proving_version) => proving_version,
            Err(error) => {
                submission.revoke().await;
                return Err(SubmitError::UnknownVerificationKey(error.to_string()));
            }
        };
        let verification_permit = match Arc::clone(&self.verification_semaphore).try_acquire_owned()
        {
            Ok(permit) => permit,
            Err(_) => {
                submission.release().await;
                return Err(SubmitError::VerificationBusy);
            }
        };
        // SYSCOIN: Reserve accepted-proof capacity before verifier CPU or durable completion. If
        // full, release only in-progress and let the client retry the same proof/token verbatim.
        let accepted_proof_permit =
            match Arc::clone(&self.accepted_proof_capacity).try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    submission.release().await;
                    return Err(SubmitError::AcceptedProofBackpressure);
                }
            };
        if self.accepted_proof_sender.is_closed() {
            submission.release().await;
            return Err(SubmitError::ShuttingDown);
        }

        // SYSCOIN: The owned task retains the submission state and semaphore permit if the HTTP
        // request is cancelled; its RAII lease cleanup is exact-token checked on every exit.
        let manager = self.clone();
        let prover_id = prover_id.to_owned();
        let task = tokio::spawn(async move {
            manager
                .verify_persist_and_enqueue_external_submission(
                    submission,
                    encoded_proof,
                    proving_version,
                    prover_id,
                    verification_permit,
                    accepted_proof_permit,
                )
                .await
        });
        match task.await {
            Ok(result) => result,
            Err(join_error) => Err(SubmitError::AmbiguousHandoff(format!(
                "owned FRI submission task failed: {join_error}"
            ))),
        }
    }

    /// SYSCOIN: Own verification through durable handoff so caller cancellation cannot reopen a
    /// lease while its blocking verifier is still running.
    async fn verify_persist_and_enqueue_external_submission(
        &self,
        submission: SubmissionLease<ProverInput>,
        encoded_proof: String,
        proving_version: ProvingVersion,
        prover_id: String,
        verification_permit: tokio::sync::OwnedSemaphorePermit,
        accepted_proof_permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<(), SubmitError> {
        let batch_metadata = submission
            .first_batch_metadata()
            .expect("single FRI submission lease must contain one batch")
            .clone();
        let batch_number = batch_metadata.batch_info.commit_info.batch_number;
        let signature_data = submission
            .first_signature_data()
            .expect("single FRI submission lease must contain signature state")
            .clone();

        // SYSCOIN: Prover should generate the proof with VK received from server. Reject a mismatch before
        // allocating decoded proof bytes; the exact authenticated owner is revoked immediately.
        let server_proving_version = batch_metadata
            .proving_version()
            .expect("Must be valid execution as set by the server");
        if server_proving_version != proving_version {
            drop(verification_permit);
            submission.revoke().await;
            return Err(SubmitError::ProvingVersionMismatch(
                server_proving_version,
                proving_version,
            ));
        }

        // SYSCOIN: Decode only after exact-token admission and both global CPU/durable-handoff
        // capacity reservations. Invalid/stale clients therefore consume no base64 allocation.
        #[cfg(test)]
        self.proof_decode_invocations
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let proof_bytes = match general_purpose::STANDARD.decode(encoded_proof) {
            Ok(proof_bytes) => Bytes::from(proof_bytes),
            Err(error) => {
                drop(verification_permit);
                submission.revoke().await;
                return Err(SubmitError::InvalidBase64(error));
            }
        };

        let verdict = self
            .verify_proof(&batch_metadata, &proof_bytes, batch_number, &prover_id)
            .await;
        drop(verification_permit);
        if let Err(err) = verdict {
            // SYSCOIN: Only the authenticated current capability can revoke itself; an old token
            // cannot clear a reassigned job. Internal failures retain the lease for safe retry.
            if matches!(
                err,
                SubmitError::ProvingVersionMismatch(..)
                    | SubmitError::FriProofVerificationError { .. }
                    | SubmitError::DeserializationFailed(..)
                    | SubmitError::InvalidBase64(..)
                    | SubmitError::InvalidProofShape(..)
            ) {
                submission.revoke().await;
            } else {
                submission.release().await;
            }
            return Err(err);
        }

        // SYSCOIN: Persist the accepted proof before removing the in-memory job, so
        // storage failures leave the job retriable. Forwarding records the batch number
        // and tracker; the forwarder reloads the proof from disk before sending downstream.
        let proof = RealFriProof {
            proof: proof_bytes,
            proving_execution_version: proving_version as u32,
        };
        let stored_batch = StoredBatch(BatchEnvelope {
            batch: batch_metadata.clone(),
            data: FriProof::Real(proof),
            signature_data,
            latency_tracker: Default::default(),
        });
        // SYSCOIN: Transfer ownership before the first durable-write await. Dropping the HTTP
        // request must not interrupt the persist -> complete -> enqueue/rollback transaction and
        // strand a capacity-protected pending proof file or a completed in-memory job.
        let handoff = self.persist_and_enqueue_accepted_proof(
            stored_batch,
            prover_id,
            submission,
            accepted_proof_permit,
        );
        match handoff.await {
            Ok(result) => result,
            Err(join_error) => Err(SubmitError::AmbiguousHandoff(format!(
                "accepted FRI proof handoff task failed: {join_error}"
            ))),
        }
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
        #[cfg(test)]
        self.verification_invocations
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let proving_version = batch_metadata
            .proving_version()
            // should be safe to unwrap, as it's been checked before this call
            .expect("invalid proving version");

        // Deserialization + cryptographic verification are CPU-heavy (seconds of work) -
        // run them on a blocking thread so prover API requests don't stall the runtime.
        // `spawn_blocking` also catches panics that escape the verifiers' own `catch_unwind`.
        let verify_result = tokio::task::spawn_blocking({
            let batch_metadata = batch_metadata.clone();
            let proof_bytes = proof_bytes.clone();
            move || {
                Self::verify_proof_blocking(
                    proving_version,
                    &batch_metadata,
                    &proof_bytes,
                    batch_number,
                )
            }
        })
        .await;

        let result = match verify_result {
            Ok(result) => result,
            Err(join_error) if join_error.is_panic() => {
                tracing::error!(
                    batch_number,
                    prover_id,
                    %join_error,
                    "proof verification panicked; rejecting the proof"
                );
                // The verifier died before producing register values; still report the
                // expected hash so the persisted proof stays diagnosable.
                let expected_hash_u32s =
                    fri_proof_verifier::expected_public_input_registers(batch_metadata)
                        .unwrap_or([0u32; 8]);
                Err(SubmitError::FriProofVerificationError {
                    expected_hash_u32s,
                    proof_final_register_values: [0u32; 16],
                })
            }
            Err(join_error) => {
                return Err(SubmitError::TemporaryInternal(format!(
                    "proof verification task failed: {join_error}"
                )));
            }
        };
        match result {
            Ok(()) => Ok(()),
            Err(SubmitError::FriProofVerificationError {
                expected_hash_u32s,
                proof_final_register_values,
            }) => {
                tracing::warn!(
                    batch_number,
                    expected = ?expected_hash_u32s,
                    actual = ?proof_final_register_values,
                    "Proof verification failed",
                );

                // Persist the failed proof with some information about the batch for debugging
                let failed_proof = FailedFriProof {
                    batch_number,
                    last_block_timestamp: batch_metadata
                        .batch_info
                        .commit_info
                        .last_block_timestamp,
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

                Err(SubmitError::FriProofVerificationError {
                    expected_hash_u32s,
                    proof_final_register_values,
                })
            }
            // Any other error (deserialization, unsupported version, ...) must reject the
            // submission too - falling through here would accept an unverified proof.
            Err(err) => Err(err),
        }
    }

    /// Deserializes and cryptographically verifies the proof.
    /// CPU-heavy and may panic on malformed input - always call via `spawn_blocking`
    /// (see `verify_proof`).
    fn verify_proof_blocking(
        proving_version: ProvingVersion,
        batch_metadata: &BatchMetadata,
        proof_bytes: &Bytes,
        batch_number: u64,
    ) -> Result<(), SubmitError> {
        debug_assert_eq!(proving_version, ProvingVersion::V8);
        let expected_hash_u32s =
            fri_proof_verifier::expected_public_input_registers(batch_metadata)?;
        tracing::debug!(
            "Using airbender unified-layer proof verifier for batch {}",
            batch_number
        );
        // SYSCOIN: Reject a valid bincode prefix with trailing bytes before verification and
        // persistence; restart verification uses this same canonical decoder.
        let program_proof = fri_proof_verifier::decode_canonical_real_fri_proof(proof_bytes)
            .inspect_err(|err| {
                tracing::warn!(batch_number, ?err, "Failed to deserialize canonical proof");
            })?;
        fri_proof_verifier::verify_fri_proof(expected_hash_u32s, &program_proof, batch_number)
    }

    /// Submit a **fake** proof on behalf of a fake prover worker.
    /// Entry is removed from the assigned map.
    pub async fn submit_fake_proof(
        &self,
        batch_number: u64,
        prover_id: &'static str,
        lease_token: &str,
    ) -> Result<(), SubmitError> {
        // SYSCOIN: fake workers submit only once, so normal pipeline backpressure must wait here;
        // returning would strand the assigned job until the much longer assignment timeout.
        let permit = self.reserve_permit_downstream().await?;

        // SYSCOIN: The fake worker may sleep past assignment timeout. Exact token admission keeps
        // it from consuming a fresh real-prover reassignment when it wakes.
        let submission = self
            .jobs
            .begin_submission(batch_number, batch_number, lease_token)
            .await
            .map_err(|_| SubmitError::InvalidLease)?;
        let mut completed = submission
            .complete_fake_fri(prover_id)
            .await
            .ok_or(SubmitError::InvalidLease)?;
        let assigned = completed
            .pop()
            .expect("single fake FRI lease must complete one job");

        let envelope = assigned
            .with_data(FriProof::Fake)
            .with_stage(BatchExecutionStage::FriProvedFake);

        permit.send(ProvenBatch::new(envelope));
        Ok(())
    }

    pub async fn status(&self) -> Vec<JobState> {
        self.jobs.status().await
    }

    // SYSCOIN: Own the complete accepted-proof transaction independently of the request future.
    // The returned JoinHandle may be dropped; Tokio keeps the task running to a terminal handoff
    // or rollback, while restart recovery covers process termination after the durable write.
    fn persist_and_enqueue_accepted_proof(
        &self,
        stored_batch: StoredBatch,
        prover_id: String,
        submission: SubmissionLease<ProverInput>,
        accepted_proof_permit: tokio::sync::OwnedSemaphorePermit,
    ) -> tokio::task::JoinHandle<Result<(), SubmitError>> {
        let proof_storage = self.proof_storage.clone();
        let accepted_proof_sender = self.accepted_proof_sender.clone();
        tokio::spawn(async move {
            let batch_number = stored_batch.0.batch_number();
            let pending_proof_key = match proof_storage
                .save_pending_batch_with_proof(&stored_batch)
                .await
            {
                Ok(key) => key,
                Err(err) => {
                    submission.release().await;
                    return Err(SubmitError::TemporaryStorage(err.to_string()));
                }
            };

            // SYSCOIN: A closed handoff queue detected before completion retains this exact lease.
            // Release the pending file and report retryable shutdown while we still own the job.
            if accepted_proof_sender.is_closed() {
                proof_storage
                    .release_pending_batch_with_proof(&pending_proof_key)
                    .await;
                submission.release().await;
                return Err(SubmitError::ShuttingDown);
            }

            let Some(reserved_job) = submission
                .complete_fri_with_rollback_reservation(&prover_id)
                .await
            else {
                // SYSCOIN: Exact token/in-progress completion failed; release only this task's
                // pending key and never consume or mutate a newer assignment.
                tracing::warn!(
                    batch_number,
                    prover_id,
                    "FRI submission lease changed before durable completion"
                );
                proof_storage
                    .release_pending_batch_with_proof(&pending_proof_key)
                    .await;
                return Err(SubmitError::InvalidLease);
            };
            let completed_job = reserved_job.map_batch_envelope(|batch_envelope| {
                batch_envelope.with_stage(BatchExecutionStage::FriProvedReal)
            });

            // SYSCOIN: The accepted-proof queue is bounded and this owned task performs every
            // rollback await, so neither backpressure nor caller cancellation can lose the job.
            match accepted_proof_sender.try_send(AcceptedProof {
                batch_number,
                proof_key: pending_proof_key.clone(),
                reserved_job: completed_job,
                queue_permit: accepted_proof_permit,
            }) {
                Ok(()) => Ok(()),
                Err(mpsc::error::TrySendError::Full(accepted_proof)) => {
                    accepted_proof.reserved_job.rollback().await;
                    proof_storage
                        .release_pending_batch_with_proof(&accepted_proof.proof_key)
                        .await;
                    // SYSCOIN: Capacity was reserved before completion, so this is an internal
                    // invariant failure. The rollback is deliberately unassigned; the old token
                    // is invalid and the truthful client response is a definitive conflict.
                    Err(SubmitError::InvalidLease)
                }
                Err(mpsc::error::TrySendError::Closed(accepted_proof)) => {
                    accepted_proof.reserved_job.rollback().await;
                    proof_storage
                        .release_pending_batch_with_proof(&accepted_proof.proof_key)
                        .await;
                    // SYSCOIN: The receiver closed in the narrow post-completion race. Roll back
                    // unassigned and tell the client its old capability is definitively invalid.
                    Err(SubmitError::InvalidLease)
                }
            }
        })
    }

    async fn reserve_permit_downstream(
        &self,
    ) -> Result<mpsc::Permit<'_, ProvenBatch>, SubmitError> {
        self.batches_with_proof_sender
            .reserve()
            .await
            .map_err(|_| SubmitError::ShuttingDown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProofStorageConfig;
    use alloy::primitives::{Address, B256, keccak256};
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
            l2_da_commitment_scheme: DACommitmentScheme::BlobsZKsyncOS,
            da_commitment: keccak256([0u8; 32]),
            first_block_timestamp: 0,
            first_block_number: Some(from),
            last_block_timestamp: 0,
            last_block_number: Some(to),
            chain_id: 270,
            operator_da_input: vec![0u8; 32],
            // SYSCOIN: Synthetic prover metadata carries no compact edge DA openings.
            edge_da_refs_input: Vec::new(),
            // SYSCOIN: Synthetic prover metadata carries the empty compact edge DA root.
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
                // SYSCOIN: V32 fixtures must exercise the canonical V8 proving lane.
                protocol_version: ProtocolSemanticVersion::new(0, 32, 0),
                upgrade_tx_hash: None,
            },
            chain_address: Address::ZERO,
            first_block_number: from,
            last_block_number: to,
            last_block_hash: None,
            pubdata_mode: PubdataMode::Blobs,
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

    fn manager_for_test(
        downstream: mpsc::Sender<ProvenBatch>,
        proof_storage: ProofStorage,
        assignment_timeout: Duration,
        max_assigned_batch_range: usize,
    ) -> FriJobManager {
        let (manager, forwarder) = FriJobManager::new(
            downstream,
            proof_storage,
            assignment_timeout,
            max_assigned_batch_range,
        );
        // SYSCOIN: Production registers this future as runtime-critical; focused unit tests spawn
        // it locally and exercise fail-closed behavior separately when it is absent.
        tokio::spawn(forwarder);
        manager
    }

    async fn begin_test_submission(
        manager: &FriJobManager,
        batch_number: u64,
        prover_id: &str,
    ) -> SubmissionLease<ProverInput> {
        let picked = manager
            .pick_next_job(Duration::ZERO, prover_id.to_owned(), None, usize::MAX)
            .await
            .expect("test job must be assigned");
        assert_eq!(picked.job.batch_number, batch_number);
        manager
            .jobs
            .begin_submission(batch_number, batch_number, &picked.lease_token)
            .await
            .expect("test lease must enter submission state")
    }

    fn accepted_proof_permit_for_test(
        manager: &FriJobManager,
    ) -> tokio::sync::OwnedSemaphorePermit {
        Arc::clone(&manager.accepted_proof_capacity)
            .try_acquire_owned()
            .expect("test must reserve accepted-proof capacity")
    }

    // SYSCOIN: Build the same exact rollback-capacity guard used by a real accepted submission.
    async fn completed_reserved_job_for_test(
        manager: &FriJobManager,
        batch_number: u64,
        prover_id: &str,
    ) -> ReservedFriJob<ProverInput> {
        begin_test_submission(manager, batch_number, prover_id)
            .await
            .complete_fri_with_rollback_reservation(prover_id)
            .await
            .expect("test FRI submission must complete into a reservation")
            .map_batch_envelope(|batch_envelope| {
                batch_envelope.with_stage(BatchExecutionStage::FriProvedReal)
            })
    }

    // SYSCOIN: Capacity is part of the existing atomic eligibility predicate. An oversized head
    // remains unassigned for a larger worker while a fitting later job receives the only lease.
    #[tokio::test]
    async fn response_capacity_filters_before_fri_lease_assignment() -> anyhow::Result<()> {
        let proof_storage = proof_storage_for_test().await?;
        let (downstream_tx, _downstream_rx) = mpsc::channel(1);
        let manager = manager_for_test(downstream_tx, proof_storage, Duration::from_secs(60), 16);
        let mut oversized = dummy_input_batch(1);
        oversized.data = ProverInput::Real(vec![0]);
        manager.add_job(oversized).await;
        manager.add_job(dummy_input_batch(2)).await;

        let picked = manager
            .pick_next_job(
                Duration::ZERO,
                "small-worker".to_owned(),
                None,
                crate::prover_api::FRI_PICK_RESPONSE_FRAMING_BYTES,
            )
            .await
            .expect("the later empty input fits the worker response capacity");
        assert_eq!(picked.job.batch_number, 2);
        let status = manager.status().await;
        assert_eq!(status[0].fri_job.batch_number, 1);
        assert_eq!(status[0].assigned_to_prover_id, None);
        assert_eq!(
            status[1].assigned_to_prover_id.as_deref(),
            Some("small-worker")
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_fri_submits_verify_once_and_invalid_owner_is_repickable()
    -> anyhow::Result<()> {
        let proof_storage = proof_storage_for_test().await?;
        let (downstream_tx, _downstream_rx) = mpsc::channel(1);
        let manager = Arc::new(manager_for_test(
            downstream_tx,
            proof_storage,
            Duration::from_secs(60),
            16,
        ));
        manager.add_job(dummy_input_batch(1)).await;
        let picked = manager
            .pick_next_job(
                Duration::ZERO,
                "victim-display-id".to_owned(),
                None,
                usize::MAX,
            )
            .await
            .expect("FRI job must be assigned");

        const CONCURRENT_SUBMITS: usize = 8;
        let barrier = Arc::new(tokio::sync::Barrier::new(CONCURRENT_SUBMITS + 1));
        let mut submits = Vec::new();
        for index in 0..CONCURRENT_SUBMITS {
            let manager = Arc::clone(&manager);
            let barrier = Arc::clone(&barrier);
            let token = picked.lease_token.clone();
            submits.push(tokio::spawn(async move {
                let display_id = format!("submitted-display-{index}");
                barrier.wait().await;
                manager
                    .submit_proof(
                        1,
                        String::new(),
                        ProvingVersion::V8.vk_hash().to_owned(),
                        &display_id,
                        &token,
                    )
                    .await
            }));
        }
        barrier.wait().await;
        let mut verifier_rejections = 0;
        for submit in submits {
            match submit.await? {
                Err(SubmitError::DeserializationFailed(_))
                | Err(SubmitError::InvalidProofShape(_))
                | Err(SubmitError::FriProofVerificationError { .. }) => {
                    verifier_rejections += 1;
                }
                Err(SubmitError::InvalidLease | SubmitError::SubmissionInProgress) => {}
                other => panic!("unexpected concurrent submit result: {other:?}"),
            }
        }
        assert_eq!(verifier_rejections, 1);
        assert_eq!(
            manager
                .verification_invocations
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            manager
                .proof_decode_invocations
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(manager.status().await[0].assigned_to_prover_id, None);

        let reassigned = manager
            .pick_next_job(
                Duration::ZERO,
                "new-display-id".to_owned(),
                None,
                usize::MAX,
            )
            .await
            .expect("invalid owner submission must be immediately repickable");
        assert_ne!(picked.lease_token, reassigned.lease_token);

        // SYSCOIN: A stale token plus the victim's public ID cannot touch the new assignment.
        assert!(matches!(
            manager
                .submit_proof(
                    1,
                    String::new(),
                    ProvingVersion::V8.vk_hash().to_owned(),
                    "new-display-id",
                    &picked.lease_token,
                )
                .await,
            Err(SubmitError::InvalidLease)
        ));
        assert_eq!(
            manager
                .verification_invocations
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            manager
                .proof_decode_invocations
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "stale authority must be rejected before base64 allocation"
        );
        assert_eq!(
            manager.status().await[0].assigned_to_prover_id.as_deref(),
            Some("new-display-id")
        );
        Ok(())
    }

    #[tokio::test]
    async fn overlapping_fri_replay_waits_but_stale_reassignment_is_definitive()
    -> anyhow::Result<()> {
        let proof_storage = proof_storage_for_test().await?;
        let (downstream_tx, _downstream_rx) = mpsc::channel(1);
        let manager = manager_for_test(downstream_tx, proof_storage, Duration::ZERO, 16);
        manager.add_job(dummy_input_batch(1)).await;
        let picked = manager
            .pick_next_job(Duration::ZERO, "display-a".to_owned(), None, usize::MAX)
            .await
            .expect("FRI job must be assigned");
        let original = manager
            .jobs
            .begin_submission(1, 1, &picked.lease_token)
            .await
            .expect("original submission must hold the token");

        assert!(matches!(
            manager
                .submit_proof(
                    1,
                    "not-even-base64".to_owned(),
                    ProvingVersion::V8.vk_hash().to_owned(),
                    "display-b",
                    &picked.lease_token,
                )
                .await,
            Err(SubmitError::SubmissionInProgress)
        ));
        assert_eq!(
            manager
                .proof_decode_invocations
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        original.release().await;

        let retry = manager
            .jobs
            .begin_submission(1, 1, &picked.lease_token)
            .await
            .expect("same token must be admissible after transient release");
        retry.release().await;
        let reassigned = manager
            .pick_next_job(Duration::ZERO, "display-c".to_owned(), None, usize::MAX)
            .await
            .expect("released zero-timeout assignment must be reassignable");
        assert_ne!(picked.lease_token, reassigned.lease_token);
        assert!(matches!(
            manager
                .submit_proof(
                    1,
                    "not-even-base64".to_owned(),
                    ProvingVersion::V8.vk_hash().to_owned(),
                    "display-a",
                    &picked.lease_token,
                )
                .await,
            Err(SubmitError::InvalidLease)
        ));
        assert_eq!(
            manager
                .proof_decode_invocations
                .load(std::sync::atomic::Ordering::SeqCst),
            0,
            "stale token must remain pre-decode"
        );
        Ok(())
    }

    // SYSCOIN: Unknown VK rejection is terminal only after the exact capability is admitted;
    // stale callers remain pre-parse and cannot revoke another prover's assignment.
    #[tokio::test]
    async fn unknown_fri_vk_is_parsed_after_exact_lease_admission() -> anyhow::Result<()> {
        let proof_storage = proof_storage_for_test().await?;
        let (downstream_tx, _downstream_rx) = mpsc::channel(1);
        let manager = manager_for_test(downstream_tx, proof_storage, Duration::from_secs(60), 16);
        manager.add_job(dummy_input_batch(1)).await;
        let picked = manager
            .pick_next_job(Duration::ZERO, "display-a".to_owned(), None, usize::MAX)
            .await
            .expect("FRI job must be assigned");

        let result = manager
            .submit_proof(
                1,
                String::new(),
                "0xunknown-vk".to_owned(),
                "display-a",
                &picked.lease_token,
            )
            .await;
        assert!(matches!(
            result,
            Err(SubmitError::UnknownVerificationKey(_))
        ));
        assert_eq!(
            manager
                .proof_decode_invocations
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(manager.status().await[0].assigned_to_prover_id, None);
        Ok(())
    }

    #[tokio::test]
    async fn saturated_fri_verifier_fails_fast_without_stranding_lease() -> anyhow::Result<()> {
        let proof_storage = proof_storage_for_test().await?;
        let (downstream_tx, _downstream_rx) = mpsc::channel(1);
        let manager = manager_for_test(downstream_tx, proof_storage, Duration::from_secs(60), 16);
        manager.add_job(dummy_input_batch(1)).await;
        let picked = manager
            .pick_next_job(Duration::ZERO, "display-a".to_owned(), None, usize::MAX)
            .await
            .expect("FRI job must be assigned");
        let mut permits = Vec::new();
        for _ in 0..MAX_CONCURRENT_FRI_VERIFICATIONS {
            permits.push(
                Arc::clone(&manager.verification_semaphore)
                    .try_acquire_owned()
                    .expect("test must saturate the verifier"),
            );
        }

        assert!(matches!(
            manager
                .submit_proof(
                    1,
                    String::new(),
                    ProvingVersion::V8.vk_hash().to_owned(),
                    "display-a",
                    &picked.lease_token,
                )
                .await,
            Err(SubmitError::VerificationBusy)
        ));
        assert_eq!(
            manager
                .verification_invocations
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        drop(permits);

        // The same capability can retry after backpressure because only in-progress was released.
        assert!(!matches!(
            manager
                .submit_proof(
                    1,
                    String::new(),
                    ProvingVersion::V8.vk_hash().to_owned(),
                    "display-b",
                    &picked.lease_token,
                )
                .await,
            Err(SubmitError::InvalidLease | SubmitError::VerificationBusy)
        ));
        assert_eq!(
            manager
                .verification_invocations
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn accepted_proof_backpressure_retains_token_without_verifier_work() -> anyhow::Result<()>
    {
        let proof_storage = proof_storage_for_test().await?;
        let (downstream_tx, _downstream_rx) = mpsc::channel(1);
        let manager = manager_for_test(downstream_tx, proof_storage, Duration::from_secs(60), 16);
        manager.add_job(dummy_input_batch(1)).await;
        let picked = manager
            .pick_next_job(Duration::ZERO, "display-a".to_owned(), None, usize::MAX)
            .await
            .expect("FRI job must be assigned");
        let capacity = Arc::clone(&manager.accepted_proof_capacity)
            .try_acquire_many_owned(ACCEPTED_PROOF_QUEUE_CAPACITY as u32)
            .expect("test must saturate accepted-proof handoff capacity");

        assert!(matches!(
            manager
                .submit_proof(
                    1,
                    String::new(),
                    ProvingVersion::V8.vk_hash().to_owned(),
                    "display-b",
                    &picked.lease_token,
                )
                .await,
            Err(SubmitError::AcceptedProofBackpressure)
        ));
        assert_eq!(
            manager
                .verification_invocations
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        drop(capacity);

        // SYSCOIN: Capacity rejection releases only submission-in-progress; the same capability
        // remains authoritative and can be admitted without waiting for assignment timeout.
        let retry = manager
            .jobs
            .begin_submission(1, 1, &picked.lease_token)
            .await
            .expect("backpressured capability must remain current");
        retry.release().await;
        Ok(())
    }

    #[tokio::test]
    async fn terminal_forwarder_failure_fails_closed_and_retains_lease() -> anyhow::Result<()> {
        let proof_storage = proof_storage_for_test().await?;
        let (downstream_tx, _downstream_rx) = mpsc::channel(1);
        let (manager, forwarder) =
            FriJobManager::new(downstream_tx, proof_storage, Duration::from_secs(60), 16);
        // SYSCOIN: Simulate the runtime-critical forwarder terminating before a submission. The
        // manager must surface shutdown before decode/completion; production also shuts the node.
        drop(forwarder);
        manager.add_job(dummy_input_batch(1)).await;
        let picked = manager
            .pick_next_job(Duration::ZERO, "display-a".to_owned(), None, usize::MAX)
            .await
            .expect("FRI job must be assigned");

        assert!(matches!(
            manager
                .submit_proof(
                    1,
                    String::new(),
                    ProvingVersion::V8.vk_hash().to_owned(),
                    "display-b",
                    &picked.lease_token,
                )
                .await,
            Err(SubmitError::ShuttingDown)
        ));
        assert_eq!(
            manager
                .proof_decode_invocations
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        let retry = manager
            .jobs
            .begin_submission(1, 1, &picked.lease_token)
            .await
            .expect("forwarder shutdown before completion must retain the lease");
        retry.release().await;
        Ok(())
    }

    #[tokio::test]
    async fn persistence_failure_retains_exact_token_for_retry() -> anyhow::Result<()> {
        let dir = TempDir::new()?;
        let storage_path = dir.path().to_path_buf();
        let proof_storage = ProofStorage::new(ProofStorageConfig {
            path: storage_path.clone(),
            ..ProofStorageConfig::default()
        })
        .await?;
        let (downstream_tx, _downstream_rx) = mpsc::channel(1);
        let manager = manager_for_test(downstream_tx, proof_storage, Duration::from_secs(60), 16);
        manager.add_job(dummy_input_batch(1)).await;
        let picked = manager
            .pick_next_job(Duration::ZERO, "display-a".to_owned(), None, usize::MAX)
            .await
            .expect("FRI job must be assigned");
        let submission = manager
            .jobs
            .begin_submission(1, 1, &picked.lease_token)
            .await
            .expect("picked token must be admitted");
        tokio::fs::remove_dir_all(storage_path.join("fri_batches")).await?;
        // SYSCOIN: Replace the storage directory with a regular file so the pending write fails
        // deterministically even though bounded storage normally recreates a missing directory.
        tokio::fs::write(storage_path.join("fri_batches"), b"blocked").await?;

        let result = manager
            .persist_and_enqueue_accepted_proof(
                StoredBatch(dummy_input_batch(1).with_data(FriProof::Fake)),
                "display-b".to_owned(),
                submission,
                accepted_proof_permit_for_test(&manager),
            )
            .await?;
        assert!(matches!(result, Err(SubmitError::TemporaryStorage(_))));

        // SYSCOIN: Durable-write failure occurs before exact completion and releases in-progress,
        // so replaying identical proof bytes under the original token is safe.
        let retry = manager
            .jobs
            .begin_submission(1, 1, &picked.lease_token)
            .await
            .expect("persistence failure must retain the exact token");
        retry.release().await;
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_request_after_durable_save_still_completes_handoff() -> anyhow::Result<()> {
        let proof_storage = proof_storage_for_test().await?;
        let (downstream_tx, mut downstream_rx) = mpsc::channel(1);
        let manager = manager_for_test(
            downstream_tx,
            proof_storage.clone(),
            Duration::from_secs(30),
            16,
        );

        manager.add_job(dummy_input_batch(1)).await;
        let stored_batch = StoredBatch(dummy_input_batch(1).with_data(FriProof::Fake));
        let submission = begin_test_submission(&manager, 1, "prover-1").await;
        let jobs_guard = manager.jobs.lock_jobs_for_test().await;
        let handoff = manager.persist_and_enqueue_accepted_proof(
            stored_batch,
            "prover-1".to_string(),
            submission,
            accepted_proof_permit_for_test(&manager),
        );

        // SYSCOIN: Wait until the pending lease and file are durable while completion is
        // deterministically blocked on the job-map mutex, then simulate HTTP cancellation.
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if proof_storage.pending_batch_proof_count_for_test().await == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await?;
        drop(handoff);
        drop(jobs_guard);

        let proven = tokio::time::timeout(Duration::from_secs(1), downstream_rx.recv())
            .await?
            .expect("owned handoff must reach downstream after request cancellation");
        assert_eq!(proven.batch.batch_number(), 1);
        let pending_key = proven
            .pending_proof_key
            .expect("durable pending lease must accompany the proven batch");
        assert!(manager.status().await.is_empty());

        proof_storage
            .release_pending_batch_with_proof(&pending_key)
            .await;
        assert_eq!(proof_storage.pending_batch_proof_count_for_test().await, 0);

        Ok(())
    }

    #[tokio::test]
    async fn cancelled_request_before_durable_save_still_completes_handoff() -> anyhow::Result<()> {
        let proof_storage = proof_storage_for_test().await?;
        let (downstream_tx, mut downstream_rx) = mpsc::channel(1);
        let manager = manager_for_test(
            downstream_tx,
            proof_storage.clone(),
            Duration::from_secs(30),
            16,
        );

        manager.add_job(dummy_input_batch(1)).await;
        let stored_batch = StoredBatch(dummy_input_batch(1).with_data(FriProof::Fake));
        let submission = begin_test_submission(&manager, 1, "prover-1").await;
        let handoff = manager.persist_and_enqueue_accepted_proof(
            stored_batch,
            "prover-1".to_string(),
            submission,
            accepted_proof_permit_for_test(&manager),
        );
        // SYSCOIN: Dropping the caller's only handle before the spawned task is scheduled must
        // not cancel a pending-file write or any subsequent ownership transfer.
        drop(handoff);

        let proven = tokio::time::timeout(Duration::from_secs(1), downstream_rx.recv())
            .await?
            .expect("owned handoff must survive immediate request cancellation");
        assert_eq!(proven.batch.batch_number(), 1);
        let pending_key = proven
            .pending_proof_key
            .expect("durable pending lease must accompany the proven batch");
        assert!(manager.status().await.is_empty());

        proof_storage
            .release_pending_batch_with_proof(&pending_key)
            .await;
        assert_eq!(proof_storage.pending_batch_proof_count_for_test().await, 0);

        Ok(())
    }

    #[tokio::test]
    async fn unloadable_accepted_proof_restores_fri_job() -> anyhow::Result<()> {
        let proof_storage = proof_storage_for_test().await?;
        let (downstream_tx, mut downstream_rx) = mpsc::channel(1);
        let manager = manager_for_test(
            downstream_tx,
            proof_storage.clone(),
            Duration::from_secs(30),
            16,
        );
        let input_batch = dummy_input_batch(1);

        manager.add_job(input_batch).await;
        let stored_batch = StoredBatch(dummy_input_batch(1).with_data(FriProof::Fake));
        let pending_key = proof_storage
            .save_pending_batch_with_proof(&stored_batch)
            .await?;

        // SYSCOIN: Simulate the pending file disappearing after proof acceptance. The forwarder
        // must restore the FRI job instead of dropping the batch and creating a permanent gap.
        proof_storage
            .release_pending_batch_with_proof(&pending_key)
            .await;
        let completed_job = completed_reserved_job_for_test(&manager, 1, "prover-1").await;
        assert!(manager.status().await.is_empty());
        manager
            .accepted_proof_sender
            .send(AcceptedProof {
                batch_number: 1,
                proof_key: pending_key,
                reserved_job: completed_job,
                queue_permit: accepted_proof_permit_for_test(&manager),
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
    async fn mismatched_accepted_proof_is_quarantined_and_restores_fri_job() -> anyhow::Result<()> {
        let proof_storage = proof_storage_for_test().await?;
        let (downstream_tx, mut downstream_rx) = mpsc::channel(1);
        let manager = manager_for_test(
            downstream_tx,
            proof_storage.clone(),
            Duration::from_secs(30),
            16,
        );
        manager.add_job(dummy_input_batch(1)).await;

        // SYSCOIN: Model a valid JSON pending file swapped after durable acceptance. It names the
        // reserved batch but carries different authoritative metadata, exercising digest binding
        // independently of the batch-number check.
        let mut mismatched_batch = dummy_input_batch(1);
        mismatched_batch.batch.tx_count += 1;
        let pending_key = proof_storage
            .save_pending_batch_with_proof(&StoredBatch(mismatched_batch.with_data(FriProof::Fake)))
            .await?;
        let completed_job = completed_reserved_job_for_test(&manager, 1, "prover-1").await;
        manager
            .accepted_proof_sender
            .send(AcceptedProof {
                batch_number: 1,
                proof_key: pending_key.clone(),
                reserved_job: completed_job,
                queue_permit: accepted_proof_permit_for_test(&manager),
            })
            .await?;

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if manager.status().await.len() == 1
                    && proof_storage
                        .get_pending_batch_with_proof(&pending_key)
                        .await
                        .expect("pending proof lookup should not fail")
                        .is_none()
                {
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
        let manager = manager_for_test(
            downstream_tx,
            proof_storage.clone(),
            Duration::from_secs(30),
            16,
        );

        manager.add_job(dummy_input_batch(1)).await;
        let stored_batch = StoredBatch(dummy_input_batch(1).with_data(FriProof::Fake));
        let pending_key = proof_storage
            .save_pending_batch_with_proof(&stored_batch)
            .await?;
        let completed_job = completed_reserved_job_for_test(&manager, 1, "prover-1").await;
        assert!(manager.status().await.is_empty());

        manager
            .accepted_proof_sender
            .send(AcceptedProof {
                batch_number: 1,
                proof_key: pending_key.clone(),
                reserved_job: completed_job,
                queue_permit: accepted_proof_permit_for_test(&manager),
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

    #[tokio::test]
    async fn stale_fake_fri_cannot_consume_reassigned_real_lease() -> anyhow::Result<()> {
        let proof_storage = proof_storage_for_test().await?;
        let (downstream_tx, _downstream_rx) = mpsc::channel(1);
        let manager = manager_for_test(downstream_tx, proof_storage, Duration::ZERO, 16);
        manager.add_job(dummy_input_batch(1)).await;

        let fake = manager
            .pick_next_job(Duration::ZERO, "fake_prover".to_owned(), None, usize::MAX)
            .await
            .expect("fake worker must pick the job first");
        let real = manager
            .pick_next_job(Duration::ZERO, "real-prover".to_owned(), None, usize::MAX)
            .await
            .expect("zero-timeout assignment must be re-pickable by the real prover");
        assert_ne!(fake.lease_token, real.lease_token);

        assert!(matches!(
            manager
                .submit_fake_proof(1, "fake_prover", &fake.lease_token)
                .await,
            Err(SubmitError::InvalidLease)
        ));
        let status = manager.status().await;
        assert_eq!(
            status[0].assigned_to_prover_id.as_deref(),
            Some("real-prover")
        );
        let real_submission = manager
            .jobs
            .begin_submission(1, 1, &real.lease_token)
            .await
            .expect("stale fake completion must preserve the fresh real lease");
        real_submission.release().await;
        Ok(())
    }

    #[tokio::test]
    async fn fake_proof_waits_for_downstream_capacity() -> anyhow::Result<()> {
        let proof_storage = proof_storage_for_test().await?;
        let (downstream_tx, mut downstream_rx) = mpsc::channel(1);
        let manager = Arc::new(manager_for_test(
            downstream_tx,
            proof_storage,
            Duration::from_secs(30),
            16,
        ));

        let mut picked = Vec::new();
        for batch_number in 1..=2 {
            manager.add_job(dummy_input_batch(batch_number)).await;
            picked.push(
                manager
                    .pick_next_job(Duration::ZERO, "fake_prover".to_owned(), None, usize::MAX)
                    .await
                    .expect("fake prover should receive the queued job"),
            );
        }

        manager
            .submit_fake_proof(1, "fake_prover", &picked[0].lease_token)
            .await?;
        let manager_for_submit = Arc::clone(&manager);
        let second_token = picked[1].lease_token.clone();
        let second_submit = tokio::spawn(async move {
            manager_for_submit
                .submit_fake_proof(2, "fake_prover", &second_token)
                .await
        });

        tokio::task::yield_now().await;
        assert!(!second_submit.is_finished());
        assert_eq!(manager.status().await.len(), 1);

        assert_eq!(downstream_rx.recv().await.unwrap().batch.batch_number(), 1);
        second_submit.await??;
        assert_eq!(downstream_rx.recv().await.unwrap().batch.batch_number(), 2);
        assert!(manager.status().await.is_empty());

        Ok(())
    }
}
