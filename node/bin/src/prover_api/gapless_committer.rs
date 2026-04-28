use crate::prover_api::proof_storage::{ProofStorage, ProvenBatch, StoredBatch};
use anyhow::Context;
use async_trait::async_trait;
use std::collections::BTreeMap;
use tokio::sync::mpsc;
use zksync_os_contract_interface::l1_discovery::BatchVerificationSL;
use zksync_os_l1_sender::batcher_metrics::BatchExecutionStage;
use zksync_os_l1_sender::commands::L1SenderCommand;
use zksync_os_l1_sender::commands::commit::CommitCommand;
use zksync_os_observability::{ComponentStateReporter, GenericComponentState};
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent};

/// Receives Batches with proofs - potentially out of order;
/// * Fixes the order (by filling in the `buffer` field);
/// * Saves to the `proof_storage`
/// * Sends downstream:
///    * For already committed batches: `L1SenderCommand::Passthrough`
///    * For batches that are not yet committed: `L1SenderCommand::SendToL1`
///
pub struct GaplessCommitter {
    pub next_expected_batch_number: u64,
    pub last_committed_batch_number: u64,
    pub proof_storage: ProofStorage,
    pub batch_verification_l1_config: BatchVerificationSL,
}

#[async_trait]
impl PipelineComponent for GaplessCommitter {
    type Input = ProvenBatch;
    type Output = L1SenderCommand<CommitCommand>;

    const NAME: &'static str = "gapless_committer";
    const OUTPUT_BUFFER_SIZE: usize = 5;

    async fn run(
        self,
        mut input: PeekableReceiver<Self::Input>,
        output: mpsc::Sender<Self::Output>,
    ) -> anyhow::Result<()> {
        let latency_tracker = ComponentStateReporter::global()
            .handle_for("gapless_committer", GenericComponentState::WaitingRecv);

        let mut buffer: BTreeMap<u64, ProvenBatch> = BTreeMap::new();
        let mut next_expected_batch_number = self.next_expected_batch_number;

        loop {
            latency_tracker.enter_state(GenericComponentState::WaitingRecv);
            match input.recv().await {
                Some(batch) => {
                    latency_tracker.enter_state(GenericComponentState::Processing);
                    buffer.insert(batch.batch.batch_number(), batch);

                    // SYSCOIN Flush ready batches
                    let mut ready: Vec<ProvenBatch> = Vec::new();
                    while let Some(next_batch) = buffer.remove(&next_expected_batch_number) {
                        ready.push(next_batch);
                        next_expected_batch_number += 1;
                    }

                    if !ready.is_empty() {
                        tracing::info!(
                            buffer_size = buffer.len(),
                            "Saving {} (batches {}-{}) to proof_storage",
                            ready.len(),
                            ready[0].batch.batch_number(),
                            ready.last().unwrap().batch.batch_number()
                        );
                        for proven_batch in ready {
                            let pending_proof_key = proven_batch.pending_proof_key;
                            let batch = proven_batch
                                .batch
                                .with_stage(BatchExecutionStage::FriProofStored);
                            let stored_batch = StoredBatch::V1(batch);
                            self.proof_storage
                                .save_batch_with_proof(&stored_batch)
                                .await?;
                            if let Some(pending_proof_key) = pending_proof_key {
                                self.proof_storage
                                    .release_pending_batch_with_proof(&pending_proof_key)
                                    .await;
                            }
                            let result = if stored_batch.batch_number()
                                <= self.last_committed_batch_number
                            {
                                L1SenderCommand::Passthrough(Box::new(
                                    stored_batch.batch_envelope(),
                                ))
                            } else {
                                CommitCommand::try_new(
                                    &self.batch_verification_l1_config,
                                    stored_batch.batch_envelope(),
                                )
                                .map(L1SenderCommand::SendToL1)
                                .context("Committer batch signature failure")?
                            };
                            latency_tracker.enter_state(GenericComponentState::WaitingSend);
                            output.send(result).await?;
                            latency_tracker.enter_state(GenericComponentState::Processing);
                        }
                    }
                }
                None => {
                    tracing::info!("inbound channel closed");
                    return Ok(());
                }
            }
        }
    }
}
