use super::proof_storage::ProofStorage;
use super::snark_job_manager::SnarkJobManager;
use crate::prover_api::fri_proof_verifier;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use zksync_os_batch_types::batcher_model::{FriProof, SignedBatchEnvelope};
use zksync_os_l1_sender::commands::L1SenderCommand;
use zksync_os_l1_sender::commands::prove::ProofCommand;
use zksync_os_l1_watcher::CommittedBatchProvider;
use zksync_os_observability::ComponentStateReporter;
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent, SendAndRecordExt};

/// Pipeline step that waits for batches to be SNARK proved.
///
/// This component:
/// - Receives batches with FRI proofs (after they are committed to L1)
/// - Forwards them to SnarkJobManager (which makes them available via HTTP API)
/// - Receives batches with proofs from SnarkJobManager (submitted via HTTP API or fake provers)
/// - Forwards the proof commands downstream to L1 proof sender
///
/// The SnarkJobManager itself is purely reactive (no run loop), accessed/driven by:
/// - HTTP server (provers call pick_next_job, submit_proof, etc.)
/// - Fake provers pool
pub struct SnarkProvingPipelineStep {
    last_proved_batch_number: u64,
    last_committed_batch_number: u64,
    proof_storage: ProofStorage,
    committed_batch_provider: CommittedBatchProvider,
    snark_job_manager: Arc<SnarkJobManager>,
    proof_commands_receiver: mpsc::Receiver<ProofCommand>,
}

impl SnarkProvingPipelineStep {
    pub fn new(
        proof_storage: ProofStorage,
        max_fris_per_snark: usize,
        last_proved_batch_number: u64,
        last_committed_batch_number: u64,
        assignment_timeout: Duration,
        max_assigned_batch_range: usize,
        committed_batch_provider: CommittedBatchProvider,
    ) -> (Self, Arc<SnarkJobManager>) {
        let (proof_commands_sender, proof_commands_receiver) = mpsc::channel::<ProofCommand>(1);

        let snark_job_manager = Arc::new(SnarkJobManager::new(
            proof_commands_sender,
            max_fris_per_snark,
            assignment_timeout,
            max_assigned_batch_range,
        ));

        let result = Self {
            last_proved_batch_number,
            last_committed_batch_number,
            proof_storage,
            committed_batch_provider,
            snark_job_manager: snark_job_manager.clone(),
            proof_commands_receiver,
        };

        (result, snark_job_manager)
    }
}
// SYSCOIN
impl SnarkProvingPipelineStep {
    fn can_rehydrate_batch(
        committed_batch_provider: &CommittedBatchProvider,
        expected_batch_number: u64,
        batch: &SignedBatchEnvelope<FriProof>,
    ) -> bool {
        if batch.batch_number() != expected_batch_number {
            tracing::warn!(
                expected_batch_number,
                actual_batch_number = batch.batch_number(),
                "skipping SNARK rehydration due to stored proof batch number mismatch"
            );
            return false;
        }

        let local_stored_batch = batch.batch.batch_info.clone().into_stored();
        let local_hash = local_stored_batch.hash();
        let Some(committed_batch) = committed_batch_provider.get(expected_batch_number) else {
            tracing::warn!(
                batch_number = expected_batch_number,
                "skipping SNARK rehydration because canonical committed batch is missing"
            );
            return false;
        };

        let committed_hash = committed_batch.hash();
        if committed_hash != local_hash {
            tracing::warn!(
                batch_number = expected_batch_number,
                ?committed_hash,
                ?local_hash,
                "skipping SNARK rehydration due to committed/local batch hash mismatch"
            );
            return false;
        }
        // SYSCOIN
        if let FriProof::Real(real) = &batch.data {
            if let Err(err) = fri_proof_verifier::verify_real_fri_proof_bytes(
                batch.batch.previous_stored_batch_info.state_commitment,
                local_stored_batch,
                real.proof(),
            ) {
                tracing::warn!(
                    batch_number = expected_batch_number,
                    ?err,
                    "skipping SNARK rehydration due to invalid stored FRI proof"
                );
                return false;
            }
        }

        true
    }

    // SYSCOIN
    async fn rehydrate_snark_queue(
        proof_storage: &ProofStorage,
        committed_batch_provider: &CommittedBatchProvider,
        snark_job_manager: &SnarkJobManager,
        last_proved_batch_number: u64,
        last_committed_batch_number: u64,
    ) {
        // SYSCOIN On restart, rehydrate SNARK queue from stored FRI proofs that are already committed but not proved.
        let mut rehydrated_jobs = 0u64;
        for batch_number in (last_proved_batch_number + 1)..=last_committed_batch_number {
            match proof_storage.get_batch_with_proof(batch_number).await {
                Ok(Some(batch)) => {
                    if Self::can_rehydrate_batch(committed_batch_provider, batch_number, &batch) {
                        snark_job_manager.add_job(batch).await;
                        rehydrated_jobs += 1;
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(
                        batch_number,
                        ?err,
                        "failed to load stored FRI proof for SNARK queue rehydration"
                    );
                }
            }
        }
        tracing::info!(
            rehydrated_jobs,
            from = last_proved_batch_number + 1,
            to = last_committed_batch_number,
            "SNARK queue rehydration completed"
        );
    }
}

#[async_trait]
impl PipelineComponent for SnarkProvingPipelineStep {
    type Input = SignedBatchEnvelope<FriProof>;
    type Output = L1SenderCommand<ProofCommand>;

    const COMPONENT_ID: zksync_os_pipeline::ComponentId =
        zksync_os_pipeline::ComponentId::SnarkJobManager;
    const OUTPUT_CHANNEL_CAPACITY: usize = 5;

    async fn run(
        self,
        mut input: PeekableReceiver<Self::Input>,
        output: mpsc::Sender<Self::Output>,
        state_reporter: ComponentStateReporter,
    ) -> anyhow::Result<()> {
        let last_proved_batch_number = self.last_proved_batch_number;
        let last_committed_batch_number = self.last_committed_batch_number;
        let proof_storage = self.proof_storage.clone();
        let committed_batch_provider = self.committed_batch_provider.clone();
        let snark_job_manager = self.snark_job_manager.clone();
        let mut proof_commands_receiver = self.proof_commands_receiver;
        let proof_output = output.clone();
        let proof_state_reporter = state_reporter.clone();

        // SYSCOIN Keep completed SNARK proofs draining while startup rehydration may wait for job-map space.
        let mut proof_forwarder = tokio::spawn(async move {
            while let Some(proof_command) = proof_commands_receiver.recv().await {
                proof_output
                    .send_and_record(
                        L1SenderCommand::SendToL1(proof_command),
                        &proof_state_reporter,
                    )?;
            }
            Ok::<(), anyhow::Error>(())
        });

        tokio::select! {
            _ = Self::rehydrate_snark_queue(
                &proof_storage,
                &committed_batch_provider,
                &snark_job_manager,
                last_proved_batch_number,
                last_committed_batch_number,
            ) => {}
            result = &mut proof_forwarder => {
                result??;
                tracing::info!("outbound channel closed");
                return Ok(());
            }
        }

        // Forward batches: pipeline input → SnarkJobManager → pipeline output
        // Two concurrent tasks handle the bidirectional flow
        tokio::select! {
            result = async {
                while let Some(batch) = input.recv_and_record_picked(&state_reporter).await {
                    if batch.batch_number() > last_proved_batch_number {
                        snark_job_manager.add_job(batch).await;
                    } else {
                        let passthrough = L1SenderCommand::Passthrough(Box::new(batch));
                        output.send_and_record(passthrough, &state_reporter)?;
                    }
                }
                Ok::<(), anyhow::Error>(())
            } => {
                proof_forwarder.abort();
                result?;
                tracing::info!("inbound channel closed");
                return Ok(());
            },
            result = &mut proof_forwarder => {
                result??;
                tracing::info!("outbound channel closed");
                return Ok(());
            },
        }
    }
}
