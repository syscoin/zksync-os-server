use crate::prover_api::fri_job_manager::FriJob;
use crate::prover_api::fri_job_manager::JobState;
use crate::prover_api::metrics::{ProverStage, ProverType};
use crate::prover_api::prover_job_map::{JobEntry, ProverJobMap, SnarkJobPick};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Permit;
use tokio::sync::mpsc::error::TrySendError;
use zksync_os_batch_types::batcher_model::{
    FriProof, RealSnarkProof, SignedBatchEnvelope, SnarkProof,
};
use zksync_os_batcher_metrics::BatchExecutionStage;
use zksync_os_l1_sender::commands::prove::ProofCommand;
use zksync_os_types::ProvingVersion;

/// Job manager for SNARK proving.
///
/// Supports multiple SNARK provers
///
/// Supports both real and fake proofs.
///  - Fake FRI proofs always result in fake SNARK proofs.
///  - Real FRI proofs may result in real or fake SNARK proofs depending on prover availability
///
/// `SnarkJobManager` aims to assign real prover jobs to real SNARK provers -
///     but if jobs are not picked within a timeout (`max_batch_age`), it releases it to a fake prover
pub struct SnarkJobManager {
    // == state ==
    jobs: ProverJobMap<FriProof>,
    // outbound
    prove_batches_sender: mpsc::Sender<ProofCommand>,
    // config
    // SYSCOIN: Amortize wrapping with a two-proof floor and target-or-age release policy.
    max_fris_per_snark: usize,
    target_fris_per_snark: usize,
    max_snark_batch_wait: Duration,
}

impl SnarkJobManager {
    pub fn new(
        prove_batches_sender: mpsc::Sender<ProofCommand>,
        max_fris_per_snark: usize,
        target_fris_per_snark: usize,
        max_snark_batch_wait: Duration,
        assignment_timeout: Duration,
        max_assigned_batch_range: usize,
    ) -> Self {
        let jobs = ProverJobMap::<FriProof>::new(
            assignment_timeout,
            max_assigned_batch_range,
            ProverStage::Snark,
        );
        Self {
            jobs,
            prove_batches_sender,
            max_fris_per_snark,
            target_fris_per_snark,
            max_snark_batch_wait,
        }
    }

    pub async fn add_job(&self, batch_envelope: SignedBatchEnvelope<FriProof>) {
        self.jobs.add_job(batch_envelope).await
    }

    /// SYSCOIN: Rehydrates a stored FRI proof without resetting the aggregation wait clock.
    pub async fn add_rehydrated_job(
        &self,
        batch_envelope: SignedBatchEnvelope<FriProof>,
        accepted_age: Duration,
    ) {
        // Readiness only distinguishes ages below / above this threshold. Capping also keeps the
        // reconstructed monotonic instant representable even for unexpectedly ancient files.
        self.jobs
            .add_job_with_age(batch_envelope, accepted_age.min(self.max_snark_batch_wait))
            .await
    }

    // If there is a job pending, returns a non-empty list of tuples (`batch_number`, `verification_key_hash`, `real_fri_proof`)
    pub async fn pick_real_job(
        &self,
        prover_id: String,
        supported_proving_versions: Option<&[ProvingVersion]>,
    ) -> anyhow::Result<Option<Vec<(FriJob, FriProof)>>> {
        // consume/remove all fake jobs that may be in the front of the queue
        self.process_pending_fake_fri_proofs().await?;

        let pick = self
            .jobs
            .pick_ready_snark_jobs(
                self.max_fris_per_snark,
                self.target_fris_per_snark,
                self.max_snark_batch_wait,
                &prover_id,
                |job| {
                    !job.batch_envelope.data.is_fake()
                        && supported_proving_versions
                            .is_none_or(|versions| versions.contains(&job.metadata.proving_version))
                },
            )
            .await;
        match pick {
            SnarkJobPick::Assigned(batches) => Ok(Some(batches)),
            SnarkJobPick::Waiting(wait) => {
                tracing::trace!(
                    prover_id,
                    eligible_fris = wait.eligible_fris,
                    minimum_fris = 2,
                    target_fris = self.target_fris_per_snark,
                    oldest_eligible_age_seconds = wait.oldest_eligible_age.as_secs(),
                    max_wait_seconds = self.max_snark_batch_wait.as_secs(),
                    "SNARK proofs are queued but intentionally waiting for the two-proof minimum and target, age, or interop readiness",
                );
                Ok(None)
            }
            SnarkJobPick::Empty => {
                tracing::trace!(prover_id, "no SNARK prove jobs are available for pick up",);
                Ok(None)
            }
        }
    }

    pub async fn submit_proof(
        &self,
        batch_from: u64,
        batch_to: u64,
        proving_version: ProvingVersion,
        payload: Vec<u8>,
        prover_id: String,
    ) -> anyhow::Result<()> {
        // SYSCOIN: Reject malformed external SNARK submit ranges before touching job state.
        anyhow::ensure!(
            batch_from <= batch_to,
            "invalid batch range: from batch {batch_from} is greater than to batch {batch_to}"
        );
        // SYSCOIN: `ProofCommand::to_calldata_suffix()` converts the prover payload into U256 words.
        // Reject malformed framing before consuming the exact lease so downstream encoding can
        // neither panic on a short final chunk nor forward an empty real proof.
        anyhow::ensure!(!payload.is_empty(), "SNARK proof payload must not be empty");
        anyhow::ensure!(
            payload.len().is_multiple_of(32),
            "SNARK proof payload length must be a multiple of 32 bytes; got {}",
            payload.len()
        );

        // Prover should generate the proof with VK received from server. These must always match.
        // If they don't, proof won't be accepted, validation will fail, therefore it's pointless to proceed.
        //
        // This should never happen, but we double-check to guarantee it's the case before consuming jobs.
        let server_vk = self
            .jobs
            .get_job_proving_vk_hash(batch_from)
            .await
            .ok_or_else(|| {
                anyhow::anyhow!("race condition: some batches were completed earlier")
            })?;
        let prover_vk = proving_version.vk_hash();
        anyhow::ensure!(
            server_vk == prover_vk,
            "Verification key hash mismatch: server got {server_vk}, prover got {prover_vk}"
        );

        // note: we still hold mutex while verifying the proof -
        // this is desired since we don't want the batches to timeout

        // todo: verify_snark_proof()
        // if false {
        //     anyhow::bail!("proof validation failed")
        // }

        // Prover should generate the proof with VK received from server. These must always match.
        // If they don't, proof won't be accepted, validation will fail, therefore it's pointless to proceed.
        //
        // This should never happen, but we double-check to guarantee it's the case.
        let Some(batch_metadata) = self.jobs.get_job_batch_metadata(batch_from).await else {
            anyhow::bail!("race condition: some batches were completed earlier")
        };
        let server_vk = batch_metadata
            .verification_key_hash()
            .expect("verification key hash must be present as it was set by server");
        let prover_vk = proving_version.vk_hash();
        anyhow::ensure!(
            server_vk == prover_vk,
            "Verification key hash mismatch: server got {server_vk}, prover got {prover_vk}"
        );

        // Ensure we can send downstream before consuming jobs from the retryable map.
        let permit = self.try_reserve_permit_downstream()?;

        // prove is valid - consuming proven batches
        let Some(consumed_batches_proven) = self
            .jobs
            .complete_assigned_many_jobs(batch_from, batch_to, ProverType::Real, &prover_id)
            .await
        else {
            anyhow::bail!("submitted batch range does not match the current prover assignment")
        };

        let consumed_batches_proven: Vec<_> = consumed_batches_proven
            .into_iter()
            .map(|batch| batch.with_stage(BatchExecutionStage::SnarkProvedReal))
            .collect();

        permit.send(ProofCommand::new(
            consumed_batches_proven,
            SnarkProof::Real(RealSnarkProof {
                proof: payload,
                proving_execution_version: proving_version as u32,
            }),
        ));
        Ok(())
    }

    /// Consumes fake FRI proofs from the head of the queue and turns them into fake SNARKs.
    async fn process_pending_fake_fri_proofs(&self) -> anyhow::Result<()> {
        self.process_pending_fake_or_timed_out_fri_proofs(None)
            .await
    }

    /// Consumes FRI proofs from the head of the queue that satisfy the following conditions:
    /// * FRI proof is fake
    /// * if `timeout_for_real_fris` is Some, then also jobs that are older than `timeout_for_real_fris`
    async fn process_pending_fake_or_timed_out_fri_proofs(
        &self,
        timeout_for_real_fris: Option<Duration>,
    ) -> anyhow::Result<()> {
        loop {
            let is_fake_or_timed_out = |job: &JobEntry<FriProof>| {
                job.batch_envelope.data.is_fake()
                    || timeout_for_real_fris
                        .is_some_and(|timeout| job.metadata.added_at.elapsed() >= timeout)
            };
            if !self.jobs.has_assignable_job(is_fake_or_timed_out).await {
                return Ok(());
            }

            let permit = self.try_reserve_permit_downstream()?;
            let assigned: Vec<(FriJob, FriProof)> = self
                .jobs
                .pick_jobs_while_with_limit(
                    self.max_fris_per_snark,
                    "fake_prover",
                    is_fake_or_timed_out,
                )
                .await;

            if assigned.is_empty() {
                return Ok(());
            }
            let real_proofs_count = assigned
                .iter()
                .filter(|(_, proof)| !proof.is_fake())
                .count();
            tracing::info!(
                "consuming fake proofs for SNARKing for batches {}-{} ({} real proofs; {} fake proofs)",
                assigned.first().unwrap().0.batch_number,
                assigned.last().unwrap().0.batch_number,
                real_proofs_count,
                assigned.len() - real_proofs_count,
            );

            let batch_from = assigned.first().unwrap().0.batch_number;
            let batch_to = assigned.last().unwrap().0.batch_number;
            let Some(completed) = self
                .jobs
                .complete_many_jobs(batch_from, batch_to, ProverType::Fake, "fake_prover")
                .await
            else {
                tracing::info!(
                    batch_from,
                    batch_to,
                    "skipping fake SNARK proof because another prover completed part of the range"
                );
                continue;
            };

            // Add observability traces
            let batches_with_fake_proofs = completed
                .into_iter()
                .map(|batch| batch.with_stage(BatchExecutionStage::SnarkProvedFake))
                .collect();

            permit.send(ProofCommand::new(
                batches_with_fake_proofs,
                SnarkProof::Fake,
            ));
        }
    }

    fn try_reserve_permit_downstream(&self) -> anyhow::Result<Permit<'_, ProofCommand>> {
        Ok(match self.prove_batches_sender.try_reserve() {
            Ok(permit) => permit,
            Err(TrySendError::Full(_)) => {
                anyhow::bail!("downstream backpressure");
            }
            Err(TrySendError::Closed(_)) => {
                anyhow::bail!("server is shutting down");
            }
        })
    }

    // SYSCOIN: Expose aggregate queue state for multi-worker prover orchestration.
    pub async fn status(&self) -> Vec<JobState> {
        self.jobs.status().await
    }
}

const POLL_INTERVAL_MS: u64 = 1000;

pub struct FakeSnarkProver {
    job_manager: Arc<SnarkJobManager>,

    // config
    max_batch_age: Duration,
    polling_interval: Duration,
}

impl FakeSnarkProver {
    pub fn new(job_manager: Arc<SnarkJobManager>, max_batch_age: Duration) -> Self {
        Self {
            job_manager,
            max_batch_age,
            polling_interval: Duration::from_millis(POLL_INTERVAL_MS),
        }
    }

    pub async fn run(self) {
        loop {
            tokio::time::sleep(self.polling_interval).await;
            if let Err(err) = self
                .job_manager
                .process_pending_fake_or_timed_out_fri_proofs(Some(self.max_batch_age))
                .await
            {
                tracing::info!("`FakeSnarkProver` iteration failed: {err}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prover_api::test_util::{
        create_test_batch_envelope_with_data, mark_test_batch_as_interop_bundle,
    };
    use alloy::primitives::Bytes;
    use zksync_os_batch_types::batcher_model::RealFriProof;
    use zksync_os_types::{ProtocolSemanticVersion, ProvingVersion};

    fn real_fri_proof() -> FriProof {
        FriProof::Real(RealFriProof {
            proof: Bytes::from_static(b"stored-fri-proof"),
            proving_execution_version: ProvingVersion::V8 as u32,
        })
    }

    #[tokio::test]
    async fn rehydrated_acceptance_age_releases_two_proof_real_range() -> anyhow::Result<()> {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let (sender, _receiver) = mpsc::channel(1);
        let manager = SnarkJobManager::new(
            sender,
            100,
            100,
            Duration::from_secs(3600),
            Duration::from_secs(60),
            100,
        );
        manager
            .add_rehydrated_job(
                create_test_batch_envelope_with_data(1, protocol_version.clone(), real_fri_proof()),
                Duration::from_secs(3601),
            )
            .await;

        // SYSCOIN: The server's atomic pick is both the readiness decision and lease. Even after
        // the age threshold, a singleton must remain unassigned so a standalone CPU SNARK worker
        // cannot invent a local aggregation policy or duplicate speculative wrapping.
        assert!(
            manager
                .pick_real_job("cpu-snark-1".to_string(), Some(&[ProvingVersion::V8]))
                .await?
                .is_none()
        );
        assert_eq!(manager.status().await[0].assigned_to_prover_id, None);

        manager
            .add_job(create_test_batch_envelope_with_data(
                2,
                protocol_version,
                real_fri_proof(),
            ))
            .await;

        let picked = manager
            .pick_real_job("cpu-snark-1".to_string(), Some(&[ProvingVersion::V8]))
            .await?
            .expect("stored acceptance age must release a two-proof range after restart");
        assert_eq!(picked.len(), 2);
        assert_eq!(picked[0].0.batch_number, 1);
        assert_eq!(picked[1].0.batch_number, 2);
        Ok(())
    }

    #[tokio::test]
    async fn rehydrated_interop_metadata_releases_fresh_two_proof_range() -> anyhow::Result<()> {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let (sender, _receiver) = mpsc::channel(1);
        let manager = SnarkJobManager::new(
            sender,
            100,
            100,
            Duration::from_secs(3600),
            Duration::from_secs(60),
            100,
        );
        manager
            .add_job(create_test_batch_envelope_with_data(
                1,
                protocol_version.clone(),
                real_fri_proof(),
            ))
            .await;
        let mut rehydrated_interop_batch =
            create_test_batch_envelope_with_data(2, protocol_version, real_fri_proof());
        mark_test_batch_as_interop_bundle(&mut rehydrated_interop_batch);
        manager
            .add_rehydrated_job(rehydrated_interop_batch, Duration::ZERO)
            .await;

        let picked = manager
            .pick_real_job("snark-prover".to_string(), Some(&[ProvingVersion::V8]))
            .await?
            .expect("rehydrated interop metadata must retain its priority signal");
        assert_eq!(picked.len(), 2);
        assert_eq!(picked[0].0.batch_number, 1);
        assert_eq!(picked[1].0.batch_number, 2);
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_after_rehydration_preserves_age_and_active_assignment() -> anyhow::Result<()>
    {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let (sender, mut receiver) = mpsc::channel(1);
        let manager = SnarkJobManager::new(
            sender,
            100,
            100,
            Duration::from_secs(1),
            Duration::from_secs(60),
            100,
        );
        manager
            .add_rehydrated_job(
                create_test_batch_envelope_with_data(1, protocol_version.clone(), real_fri_proof()),
                Duration::from_secs(2),
            )
            .await;
        manager
            .add_job(create_test_batch_envelope_with_data(
                2,
                protocol_version.clone(),
                real_fri_proof(),
            ))
            .await;

        let assigned = manager
            .pick_real_job("snark-prover".to_string(), Some(&[ProvingVersion::V8]))
            .await?
            .expect("rehydrated age must make the two-proof range ready");
        assert_eq!(assigned.len(), 2);

        // This is the normal recreated-pipeline arrival that follows startup rehydration.
        manager
            .add_job(create_test_batch_envelope_with_data(
                1,
                protocol_version,
                real_fri_proof(),
            ))
            .await;

        let status = manager.status().await;
        assert_eq!(status.len(), 2);
        assert!(status[0].added_seconds_ago >= 1);
        assert_eq!(
            status[0].assigned_to_prover_id.as_deref(),
            Some("snark-prover")
        );
        assert_eq!(status[0].current_attempt, 1);

        manager
            .submit_proof(
                1,
                2,
                ProvingVersion::V8,
                vec![0; 32],
                "snark-prover".to_string(),
            )
            .await?;
        assert!(receiver.recv().await.is_some());
        assert!(manager.status().await.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn malformed_snark_framing_is_rejected_without_consuming_assignment() -> anyhow::Result<()>
    {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let (sender, mut receiver) = mpsc::channel(1);
        let manager = SnarkJobManager::new(
            sender,
            2,
            2,
            Duration::from_secs(3600),
            Duration::from_secs(60),
            100,
        );
        manager
            .add_job(create_test_batch_envelope_with_data(
                1,
                protocol_version.clone(),
                real_fri_proof(),
            ))
            .await;
        manager
            .add_job(create_test_batch_envelope_with_data(
                2,
                protocol_version,
                real_fri_proof(),
            ))
            .await;
        assert!(
            manager
                .pick_real_job("snark-prover".to_string(), Some(&[ProvingVersion::V8]))
                .await?
                .is_some()
        );

        let empty_err = manager
            .submit_proof(
                1,
                2,
                ProvingVersion::V8,
                Vec::new(),
                "snark-prover".to_string(),
            )
            .await
            .unwrap_err();
        assert_eq!(
            empty_err.to_string(),
            "SNARK proof payload must not be empty"
        );

        let unaligned_err = manager
            .submit_proof(
                1,
                2,
                ProvingVersion::V8,
                vec![0; 31],
                "snark-prover".to_string(),
            )
            .await
            .unwrap_err();
        assert_eq!(
            unaligned_err.to_string(),
            "SNARK proof payload length must be a multiple of 32 bytes; got 31"
        );
        assert_eq!(manager.status().await.len(), 2);

        manager
            .submit_proof(
                1,
                2,
                ProvingVersion::V8,
                vec![0; 32],
                "snark-prover".to_string(),
            )
            .await?;
        assert!(receiver.recv().await.is_some());
        assert!(manager.status().await.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn external_submit_must_match_exact_assigned_range() -> anyhow::Result<()> {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let (sender, mut receiver) = mpsc::channel(1);
        let manager = SnarkJobManager::new(
            sender,
            100,
            2,
            Duration::from_secs(3600),
            Duration::from_secs(60),
            100,
        );
        for batch_number in 1..=2 {
            manager
                .add_job(create_test_batch_envelope_with_data(
                    batch_number,
                    protocol_version.clone(),
                    real_fri_proof(),
                ))
                .await;
        }

        let assigned = manager
            .pick_real_job("snark-prover".to_string(), Some(&[ProvingVersion::V8]))
            .await?
            .expect("target-sized range must be assigned");
        assert_eq!(assigned.len(), 2);

        let err = manager
            .submit_proof(
                1,
                1,
                ProvingVersion::V8,
                vec![0; 32],
                "snark-prover".to_string(),
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "submitted batch range does not match the current prover assignment"
        );
        assert_eq!(manager.status().await.len(), 2);

        manager
            .submit_proof(
                1,
                2,
                ProvingVersion::V8,
                vec![0; 32],
                "snark-prover".to_string(),
            )
            .await?;
        let command = receiver
            .recv()
            .await
            .expect("valid proof must be forwarded");
        assert_eq!(command.as_ref().len(), 2);
        assert!(manager.status().await.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn backpressure_does_not_lease_fake_jobs() {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let (sender, mut receiver) = mpsc::channel(1);
        sender
            .try_send(ProofCommand::new(
                vec![create_test_batch_envelope_with_data(
                    100,
                    protocol_version.clone(),
                    FriProof::Fake,
                )],
                SnarkProof::Fake,
            ))
            .unwrap();

        let manager = SnarkJobManager::new(
            sender,
            2,
            2,
            Duration::from_secs(3600),
            Duration::from_secs(60),
            100,
        );
        manager
            .add_job(create_test_batch_envelope_with_data(
                1,
                protocol_version,
                FriProof::Fake,
            ))
            .await;

        let err = manager.process_pending_fake_fri_proofs().await.unwrap_err();
        assert_eq!(err.to_string(), "downstream backpressure");
        let status = manager.jobs.status().await;
        assert_eq!(status[0].assigned_to_prover_id, None);
        assert_eq!(status[0].current_attempt, 0);

        receiver.recv().await.unwrap();
        manager.process_pending_fake_fri_proofs().await.unwrap();

        let command = receiver.recv().await.unwrap();
        assert_eq!(command.as_ref()[0].batch_number(), 1);
        assert!(manager.jobs.status().await.is_empty());
    }
}
