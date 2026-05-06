use async_trait::async_trait;
use tokio::sync::{mpsc, watch};
use tokio::time::{Duration, MissedTickBehavior, interval};
use zksync_os_interface::types::BlockOutput;
use zksync_os_l1_watcher::CommittedBatchProvider;
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent};
use zksync_os_storage_api::{ReadFinality, ReplayRecord};
use zksync_os_types::SystemTxType;

const MIGRATION_BATCH_LOOKUP_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// SYSCOIN: mirrors the main-node MigrationGate trigger for external nodes.
pub(crate) struct EnMigrationTrigger<Finality> {
    pub committed_batch_provider: CommittedBatchProvider,
    pub finality: Finality,
    pub last_finalized_migration: watch::Receiver<u64>,
    pub migration_triggered: watch::Sender<Option<u64>>,
}

#[async_trait]
impl<Finality> PipelineComponent for EnMigrationTrigger<Finality>
where
    Finality: ReadFinality + Clone,
{
    type Input = (BlockOutput, ReplayRecord);
    type Output = (BlockOutput, ReplayRecord);

    const NAME: &'static str = "en_migration_trigger";
    const OUTPUT_BUFFER_SIZE: usize = 5;

    async fn run(
        self,
        mut input: PeekableReceiver<Self::Input>,
        output: mpsc::Sender<Self::Output>,
    ) -> anyhow::Result<()> {
        let mut pending_trigger = None;
        let mut lookup_interval = interval(MIGRATION_BATCH_LOOKUP_POLL_INTERVAL);
        lookup_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut input_closed = false;
        loop {
            tokio::select! {
                maybe_item = input.recv(), if !input_closed => {
                    let Some((block_output, replay_record)) = maybe_item else {
                        if pending_trigger.is_some() {
                            tracing::info!(
                                ?pending_trigger,
                                "inbound channel closed; draining pending external-node migration trigger lookup"
                            );
                            input_closed = true;
                            continue;
                        }
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
                            let migration_number = *migration_number;
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

                        // SYSCOIN: keep at most one lookup alive; replay input must not be able
                        // to create unbounded polling work.
                        match pending_trigger {
                            Some((pending_migration_number, pending_block_number))
                                if migration_number == pending_migration_number =>
                            {
                                tracing::warn!(
                                    migration_number,
                                    pending_migration_number,
                                    pending_block_number,
                                    "ignoring duplicate SetSLChainId while an external-node migration trigger lookup is pending"
                                );
                            }
                            Some((pending_migration_number, pending_block_number))
                                if block_number <= pending_block_number =>
                            {
                                tracing::warn!(
                                    migration_number,
                                    block_number,
                                    pending_migration_number,
                                    pending_block_number,
                                    "ignoring stale SetSLChainId while an external-node migration trigger lookup is pending"
                                );
                            }
                            Some((pending_migration_number, pending_block_number)) => {
                                tracing::warn!(
                                    migration_number,
                                    block_number,
                                    pending_migration_number,
                                    pending_block_number,
                                    "replacing pending external-node migration trigger lookup"
                                );
                                pending_trigger = Some((migration_number, block_number));
                            }
                            None => {
                                pending_trigger = Some((migration_number, block_number));
                            }
                        }
                    }

                    if output.send((block_output, replay_record)).await.is_err() {
                        tracing::info!("outbound channel closed");
                        return Ok(());
                    }
                }

                // SYSCOIN: L1 commit indexing can lag EN replay; notify asynchronously so replay
                // does not stall while waiting for the batch containing this block to be indexed.
                _ = lookup_interval.tick(), if pending_trigger.is_some() => {
                    let Some((migration_number, block_number)) = pending_trigger else {
                        continue;
                    };

                    if let Some(trigger_batch) =
                        self.committed_batch_provider.get_batch_containing_block(block_number)
                    {
                        let trigger_batch_number = trigger_batch.number();
                        tracing::info!(
                            migration_number,
                            block_number,
                            trigger_batch_number,
                            "SetSLChainId block replayed on external node; signalling settlement layer watcher"
                        );
                        let _ = self.migration_triggered.send(Some(trigger_batch_number));
                        pending_trigger = None;
                        if input_closed {
                            tracing::info!("pending external-node migration trigger drained");
                            return Ok(());
                        }
                        continue;
                    }

                    let status = self.finality.get_finality_status();
                    if status.last_finalized_executed_block >= block_number {
                        tracing::info!(
                            migration_number,
                            block_number,
                            last_finalized_executed_batch =
                                status.last_finalized_executed_batch,
                            "SetSLChainId block already finalized on external node; signalling settlement layer watcher"
                        );
                        let _ = self
                            .migration_triggered
                            .send(Some(status.last_finalized_executed_batch));
                        pending_trigger = None;
                        if input_closed {
                            tracing::info!("pending external-node migration trigger drained");
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}
