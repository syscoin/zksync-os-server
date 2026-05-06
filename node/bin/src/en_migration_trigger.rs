use async_trait::async_trait;
use tokio::sync::{mpsc, watch};
use zksync_os_interface::types::BlockOutput;
use zksync_os_l1_watcher::CommittedBatchProvider;
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent};
use zksync_os_storage_api::ReplayRecord;
use zksync_os_types::SystemTxType;

/// SYSCOIN: mirrors the main-node MigrationGate trigger for external nodes.
pub(crate) struct EnMigrationTrigger {
    pub committed_batch_provider: CommittedBatchProvider,
    pub last_finalized_migration: watch::Receiver<u64>,
    pub migration_triggered: watch::Sender<Option<u64>>,
}

#[async_trait]
impl PipelineComponent for EnMigrationTrigger {
    type Input = (BlockOutput, ReplayRecord);
    type Output = (BlockOutput, ReplayRecord);

    const NAME: &'static str = "en_migration_trigger";
    const OUTPUT_BUFFER_SIZE: usize = 5;

    async fn run(
        self,
        mut input: PeekableReceiver<Self::Input>,
        output: mpsc::Sender<Self::Output>,
    ) -> anyhow::Result<()> {
        loop {
            let Some((block_output, replay_record)) = input.recv().await else {
                tracing::info!("inbound channel closed");
                return Ok(());
            };

            let pending_migration_number = replay_record
                .transactions
                .iter()
                .find_map(|tx| {
                    let Some(SystemTxType::SetSLChainId(migration_number)) =
                        tx.as_system_tx_type()
                    else {
                        return None;
                    };
                    if migration_number != u64::MAX
                        && migration_number > *self.last_finalized_migration.borrow()
                    {
                        Some(migration_number)
                    } else {
                        None
                    }
                });

            if let Some(migration_number) = pending_migration_number {
                let block_number = replay_record.block_context.block_number;
                let committed_batch_provider = self.committed_batch_provider.clone();
                let migration_triggered = self.migration_triggered.clone();

                // SYSCOIN: L1 commit indexing can lag EN replay; notify asynchronously so replay
                // does not stall while waiting for the batch containing this block to be indexed.
                tokio::spawn(async move {
                    let trigger_batch = committed_batch_provider
                        .wait_for_batch_containing_block(block_number)
                        .await;
                    let trigger_batch_number = trigger_batch.number();

                    tracing::info!(
                        migration_number,
                        block_number,
                        trigger_batch_number,
                        "SetSLChainId block replayed on external node; signalling settlement layer watcher"
                    );
                    let _ = migration_triggered.send(Some(trigger_batch_number));
                });
            }

            if output.send((block_output, replay_record)).await.is_err() {
                tracing::info!("outbound channel closed");
                return Ok(());
            }
        }
    }
}
