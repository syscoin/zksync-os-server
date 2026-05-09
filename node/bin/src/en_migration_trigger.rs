use async_trait::async_trait;
use tokio::sync::{mpsc, watch};
use tokio::time::{Duration, MissedTickBehavior, interval};
use zksync_os_l1_watcher::CommittedBatchProvider;
use zksync_os_observability::ComponentStateReporter;
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent, SendAndRecordExt};
use zksync_os_sequencer::model::blocks::AppliedBlock;
use zksync_os_storage_api::ReadFinality;
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
    type Input = AppliedBlock;
    type Output = AppliedBlock;

    const COMPONENT_ID: zksync_os_pipeline::ComponentId =
        zksync_os_pipeline::ComponentId::EnMigrationTrigger;
    const OUTPUT_CHANNEL_CAPACITY: usize = 5;

    async fn run(
        self,
        mut input: PeekableReceiver<Self::Input>,
        output: mpsc::Sender<Self::Output>,
        state_reporter: ComponentStateReporter,
    ) -> anyhow::Result<()> {
        let mut pending_trigger: Option<(u64, u64, Option<u64>)> = None;
        let mut lookup_interval = interval(MIGRATION_BATCH_LOOKUP_POLL_INTERVAL);
        lookup_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut input_closed = false;
        loop {
            tokio::select! {
                maybe_item = input.recv_and_record_picked(&state_reporter), if !input_closed => {
                    let Some(applied_block) = maybe_item else {
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
                    let replay_record = &applied_block.record;

                    let pending_migration_number = replay_record
                        .transactions
                        .iter()
                        .find_map(|tx| {
                            let Some(SystemTxType::SetSLChainId(_, migration_number)) =
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
                            Some((pending_migration_number, pending_block_number, _))
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
                            Some((
                                pending_migration_number,
                                pending_block_number,
                                ref mut fallback_block_number,
                            )) if migration_number == pending_migration_number => {
                                if fallback_block_number
                                    .as_ref()
                                    .is_some_and(|fallback| block_number <= *fallback)
                                {
                                    tracing::warn!(
                                        migration_number,
                                        block_number,
                                        pending_block_number,
                                        ?fallback_block_number,
                                        "ignoring duplicate SetSLChainId while an external-node migration trigger lookup is pending"
                                    );
                                } else {
                                    tracing::warn!(
                                        migration_number,
                                        block_number,
                                        pending_block_number,
                                        "recording fallback block for pending external-node migration trigger lookup"
                                    );
                                    *fallback_block_number = Some(block_number);
                                }
                            }
                            Some((pending_migration_number, pending_block_number, _)) => {
                                tracing::warn!(
                                    migration_number,
                                    block_number,
                                    pending_migration_number,
                                    pending_block_number,
                                    "replacing pending external-node migration trigger lookup"
                                );
                                pending_trigger = Some((migration_number, block_number, None));
                            }
                            None => {
                                pending_trigger = Some((migration_number, block_number, None));
                            }
                        }
                    }

                    output.send_and_record(applied_block, &state_reporter).await?;
                }

                // SYSCOIN: L1 commit indexing can lag EN replay; notify asynchronously so replay
                // does not stall while waiting for the batch containing this block to be indexed.
                _ = lookup_interval.tick(), if pending_trigger.is_some() => {
                    let Some((migration_number, block_number, fallback_block_number)) = pending_trigger else {
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

                    if let Some(fallback_block_number) = fallback_block_number {
                        if let Some(trigger_batch) = self
                            .committed_batch_provider
                            .get_batch_containing_block(fallback_block_number)
                        {
                            let trigger_batch_number = trigger_batch.number();
                            tracing::info!(
                                migration_number,
                                block_number = fallback_block_number,
                                trigger_batch_number,
                                "fallback SetSLChainId block replayed on external node; signalling settlement layer watcher"
                            );
                            let _ = self.migration_triggered.send(Some(trigger_batch_number));
                            pending_trigger = None;
                            if input_closed {
                                tracing::info!("pending external-node migration trigger drained");
                                return Ok(());
                            }
                            continue;
                        }
                    }

                    let status = self.finality.get_finality_status();
                    let finalized_trigger_block =
                        if status.last_finalized_executed_block >= block_number {
                            Some(block_number)
                        } else if fallback_block_number.is_some_and(|fallback_block_number| {
                            status.last_finalized_executed_block >= fallback_block_number
                        }) {
                            fallback_block_number
                        } else {
                            None
                        };
                    if let Some(finalized_trigger_block) = finalized_trigger_block {
                        tracing::info!(
                            migration_number,
                            block_number = finalized_trigger_block,
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
