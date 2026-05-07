use crate::prover_api::proof_storage::{ProofStorage, ProvenBatch, StoredBatch};
use anyhow::Context;
use async_trait::async_trait;
use std::collections::BTreeMap;
use tokio::sync::mpsc;
use zksync_os_batcher_metrics::BatchExecutionStage;
use zksync_os_contract_interface::l1_discovery::BatchVerificationSL;
use zksync_os_l1_sender::commands::L1SenderCommand;
use zksync_os_l1_sender::commands::commit::CommitCommand;
use zksync_os_observability::{ComponentStateReporter, GenericComponentState};
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent, SendAndRecordExt};

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

    const COMPONENT_ID: zksync_os_pipeline::ComponentId =
        zksync_os_pipeline::ComponentId::GaplessCommitter;

    async fn run(
        self,
        mut input: PeekableReceiver<Self::Input>,
        output: mpsc::Sender<Self::Output>,
        state_reporter: ComponentStateReporter,
    ) -> anyhow::Result<()> {
        let mut buffer: BTreeMap<u64, ProvenBatch> = BTreeMap::new();
        let mut next_expected_batch_number = self.next_expected_batch_number;

        loop {
            state_reporter.enter_state(GenericComponentState::Idle);
            match input.recv_and_record_picked(&state_reporter).await {
                Some(proven_batch) => {
                    state_reporter.enter_state(GenericComponentState::Active);
                    let batch_number = proven_batch.batch.batch_number();
                    // SYSCOIN
                    if batch_number < next_expected_batch_number {
                        if let Some(pending_proof_key) = proven_batch.pending_proof_key {
                            tracing::warn!(
                                batch_number,
                                pending_proof_key = ?pending_proof_key,
                                next_expected_batch_number,
                                "dropping stale pending FRI proof replay"
                            );
                            self.proof_storage
                                .release_pending_batch_with_proof(&pending_proof_key)
                                .await;
                        } else {
                            tracing::warn!(
                                batch_number,
                                next_expected_batch_number,
                                "dropping stale FRI proof"
                            );
                        }
                        continue;
                    }

                    // SYSCOIN Multiple pending files for the same batch may be replayed after
                    // restart. Keep the latest proof in the gap buffer and release the replaced
                    // pending file so it does not remain protected forever.
                    if let Some(replaced_batch) = buffer.insert(batch_number, proven_batch)
                        && let Some(pending_proof_key) = replaced_batch.pending_proof_key
                    {
                        self.proof_storage
                            .release_pending_batch_with_proof(&pending_proof_key)
                            .await;
                    }

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
                            if let Some(pending_proof_key) = pending_proof_key {
                                self.proof_storage
                                    .promote_pending_batch_with_proof(&pending_proof_key)
                                    .await?;
                                self.proof_storage
                                    .release_pending_batch_with_proof(&pending_proof_key)
                                    .await;
                            } else {
                                self.proof_storage
                                    .save_batch_with_proof(&stored_batch)
                                    .await?;
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
                            output.send_and_record(result, &state_reporter)?;
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
