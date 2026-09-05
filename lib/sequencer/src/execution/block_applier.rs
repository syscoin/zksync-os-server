use crate::config::SequencerConfig;
use crate::execution::metrics::BlockApplierState;
use crate::model::blocks::{AppliedBlock, BlockCommandType, BlockPayload};
use alloy::consensus::Sealed;
use alloy::primitives::BlockNumber;
use anyhow::Context as _;
use async_trait::async_trait;
use tokio::sync::{mpsc, watch};
use zksync_os_observability::ComponentStateReporter;
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent, SendAndRecordExt};
use zksync_os_storage_api::{WriteReplay, WriteRepository, WriteState};

/// Persists blocks in various local storages.
/// Used to be part of the Sequencer - was split into `BlockExecutor` and `BlockApplier`.
pub struct BlockApplier<State, Replay, Repo>
where
    State: WriteState + Clone + Send + 'static,
    Replay: WriteReplay + Send + 'static,
    Repo: WriteRepository + Send + 'static,
{
    pub state: State,
    pub replay: Replay,
    pub repositories: Repo,
    pub config: SequencerConfig,
    pub applied_block_number_sender: watch::Sender<Option<BlockNumber>>,
}

#[async_trait]
impl<State, Replay, Repo> PipelineComponent for BlockApplier<State, Replay, Repo>
where
    State: WriteState + Clone + Send + 'static,
    Replay: WriteReplay + Send + 'static,
    Repo: WriteRepository + Send + 'static,
{
    type Input = BlockPayload;
    type Output = AppliedBlock;

    const COMPONENT_ID: zksync_os_pipeline::ComponentId =
        zksync_os_pipeline::ComponentId::BlockApplier;
    const OUTPUT_CHANNEL_CAPACITY: usize = 5;

    async fn run(
        mut self,
        mut input: PeekableReceiver<Self::Input>,
        output: mpsc::Sender<Self::Output>,
        state_reporter: ComponentStateReporter,
    ) -> anyhow::Result<()> {
        loop {
            state_reporter.enter_state(BlockApplierState::Idle);
            let Some(BlockPayload {
                output: block_output_with_reads,
                record: executed_replay,
                command_type: cmd_type,
                failed_transactions,
            }) = input.recv_and_record_picked(&state_reporter).await
            else {
                tracing::info!("inbound channel closed");
                return Ok(());
            };
            let block_output = block_output_with_reads.as_ref();

            let block_number = executed_replay.block_context.block_number;
            let block_hash = block_output.header.hash();
            let override_allowed = match cmd_type {
                // SYSCOIN: Main-node followers need the same historical replacement permission
                // as the leader, but ordinary startup/consensus Replay stays non-overwriting.
                BlockCommandType::Rebuild | BlockCommandType::CanonizedRebuild => true,
                _ if self.config.node_role.is_external() => true,
                _ => false,
            };

            state_reporter.enter_state(BlockApplierState::AddingToStorage);
            tracing::info!(block_number, "Persisting block {block_number}");
            // SYSCOIN: WAL validation failure is a critical pipeline error, not a clean EOF.
            // Do not publish state, repositories or applied progress for a rejected record.
            let replay_written = self
                .replay
                .write(
                    Sealed::new_unchecked(executed_replay.clone(), block_hash),
                    override_allowed,
                )
                .await
                .with_context(|| {
                    format!("failed to persist replay record for block {block_number}")
                })?;

            // SYSCOIN: A crash may leave a rebuilt WAL row durable before its derived state.
            // `Ok(false)` authenticates the exact existing canonical header and replay input;
            // repair state from that output without granting permission to rewrite the WAL.
            let state_override_allowed = override_allowed || !replay_written;
            self.state.add_block_result(
                block_number,
                block_output.storage_writes.clone(),
                block_output
                    .published_preimages
                    .iter()
                    .map(|(k, v)| (*k, v)),
                state_override_allowed,
            )?;

            state_reporter.enter_state(BlockApplierState::PopulatingRepos);
            self.repositories
                .populate(
                    block_output.clone(),
                    executed_replay.transactions.clone(),
                    failed_transactions,
                )
                .await?;

            self.applied_block_number_sender
                .send_replace(Some(block_number));

            output
                .send_and_record(
                    AppliedBlock {
                        output: block_output_with_reads,
                        record: executed_replay,
                    },
                    &state_reporter,
                )
                .await?;
        }
    }
}
