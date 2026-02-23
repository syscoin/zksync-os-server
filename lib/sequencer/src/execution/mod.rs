use crate::config::SequencerConfig;
use crate::execution::block_context_provider::BlockContextProvider;
use crate::execution::execute_block_in_vm::execute_block_in_vm;
use crate::execution::metrics::{EXECUTION_METRICS, SequencerState};
use crate::execution::utils::save_dump;
use crate::model::blocks::{BlockCommand, BlockCommandType};
use anyhow::Context;
use async_trait::async_trait;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use zksync_os_interface::types::BlockOutput;
use zksync_os_mempool::L2TransactionPool;
use zksync_os_observability::{ComponentStateHandle, ComponentStateReporter};
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent};
use zksync_os_storage_api::{OverlayBuffer, ReadStateHistory, ReplayRecord, WriteState};
use zksync_os_types::{NotAcceptingReason, TransactionAcceptanceState};

pub use fee_provider::{FeeConfig, FeeParams, FeeProvider};

pub mod block_applier;
pub mod block_canonizer;
pub mod block_context_provider;
pub mod execute_block_in_vm;
mod fee_provider;
pub(crate) mod metrics;
pub(crate) mod utils;
pub mod vm_wrapper;

pub use block_applier::BlockApplier;
pub use block_canonizer::{BlockCanonizer, ConsensusInterface, LoopbackConsensus};
/// Executes blocks, while only updating local in-memory state (mempool, block context).
/// Does not persist anything to disk.
/// Does not track the node role - reacts on the ordered inbound commands instead (`Produce` vs `Replay`)
pub struct BlockExecutor<Mempool, State>
where
    Mempool: L2TransactionPool + Send + 'static,
    State: ReadStateHistory + WriteState + Clone + Send + 'static,
{
    pub block_context_provider: BlockContextProvider<Mempool>,
    pub state: State,
    pub config: SequencerConfig,
    /// Controls transaction acceptance state.
    /// When max_blocks_to_produce limit is reached, sequencer sends NotAccepting to stop RPC from accepting new txs.
    pub tx_acceptance_state_sender: watch::Sender<TransactionAcceptanceState>,
}

#[async_trait]
impl<Mempool, State> PipelineComponent for BlockExecutor<Mempool, State>
where
    Mempool: L2TransactionPool + Send + 'static,
    State: ReadStateHistory + WriteState + Clone + Send + 'static,
{
    type Input = BlockCommand;
    /// Outputs executed blocks. Passes along information whether it's a replayed or new block -
    ///  new blocks need to be canonized by network (enforced by `BlockCanonizer`)
    type Output = (BlockOutput, ReplayRecord, BlockCommandType);

    const NAME: &'static str = "block_executor";
    const OUTPUT_BUFFER_SIZE: usize = 1;

    async fn run(
        mut self,
        mut input: PeekableReceiver<Self::Input>, // PeekableReceiver<BlockCommand>
        output: mpsc::Sender<Self::Output>, // Sender<(BlockOutput, ReplayRecord, BlockCommandType)>
    ) -> anyhow::Result<()> {
        let latency_tracker = ComponentStateReporter::global()
            .handle_for("block_executor", SequencerState::WaitingForCommand);

        // Track how many Produce commands we've processed (for `sequencer_max_blocks_to_produce` config)
        let mut produced_blocks_count = 0u64;

        // Only used for metrics/logs
        let mut last_processed_block_at: Option<Instant> = None;
        // `BlockExecutor` doesn't persist/update state after block execution.
        // Instead, we keep the diff in memory - and apply it on top of the last persisted block
        let mut state_overlay_buffer = OverlayBuffer::default();

        loop {
            latency_tracker.enter_state(SequencerState::WaitingForCommand);

            let Some(cmd) = input.recv().await else {
                anyhow::bail!("inbound channel closed");
            };
            let cmd_type = cmd.command_type();

            // For Produce commands: check limit (will await indefinitely if limit reached) and increment counter
            if matches!(cmd, BlockCommand::Produce(_))
                && let Some(limit) = self.config.max_blocks_to_produce
            {
                check_block_production_limit(
                    limit,
                    produced_blocks_count,
                    &self.tx_acceptance_state_sender,
                    &latency_tracker,
                )
                .await;
                produced_blocks_count += 1;
            }
            tracing::debug!(
                cmd = cmd.to_string(),
                "starting command. Turning into PreparedCommand.."
            );
            latency_tracker.enter_state(SequencerState::BlockContextTxs);

            let prepared_command = self.block_context_provider.prepare_command(cmd).await?;

            let block_number = prepared_command.block_context.block_number;
            tracing::info!(
                block_number,
                "Prepared context for block {block_number}. expected_block_output_hash: {:?}, starting_l1_priority_id: {}, timestamp: {}, execution_version: {}. Executing..",
                prepared_command.expected_block_output_hash,
                prepared_command.starting_l1_priority_id,
                prepared_command.block_context.timestamp,
                prepared_command.block_context.execution_version,
            );

            let exec_view = state_overlay_buffer
                .sync_with_base_and_build_view_for_block(&self.state, block_number)?;

            let (block_output, replay_record, purged_txs) =
                execute_block_in_vm(prepared_command, exec_view, &latency_tracker)
                    .await
                    .map_err(|dump| {
                        let error = anyhow::anyhow!("{}", dump.error);
                        tracing::info!("Saving dump..");
                        if let Err(err) = save_dump(self.config.block_dump_path.clone(), dump) {
                            tracing::error!(?err, "Failed to write block dump");
                        }
                        error
                    })
                    .context("execute_block")?;

            let time_since_last_block = last_processed_block_at
                .map(|last_processed_block_at| last_processed_block_at.elapsed());
            if let Some(time_since_last_block) = time_since_last_block {
                EXECUTION_METRICS
                    .time_since_last_block
                    .observe(time_since_last_block);
            }
            last_processed_block_at = Some(Instant::now());

            tracing::debug!(block_number, "Executed. Updating mempools...");
            latency_tracker.enter_state(SequencerState::UpdatingMempool);

            self.block_context_provider
                .on_canonical_state_change(&block_output, &replay_record, cmd_type)
                .await;
            let purged_txs_hashes = purged_txs.into_iter().map(|(hash, _)| hash).collect();
            self.block_context_provider.remove_txs(purged_txs_hashes);

            state_overlay_buffer.add_block(
                block_number,
                block_output.storage_writes.clone(),
                block_output.published_preimages.clone(),
            )?;

            tracing::debug!(
                block_number,
                time_since_last_block = ?time_since_last_block,
                "Block processed in `BlockExecutor`. Sending downstream..."
            );
            EXECUTION_METRICS.block_number.set(block_number);
            EXECUTION_METRICS
                .last_execution_version
                .set(replay_record.block_context.execution_version as u64);

            latency_tracker.enter_state(SequencerState::WaitingSend);
            if output
                .send((block_output.clone(), replay_record.clone(), cmd_type))
                .await
                .is_err()
            {
                anyhow::bail!("Outbound channel closed");
            }

            tracing::debug!(block_number, "Block fully processed");
        }
    }
}

/// Checks if block production limit has been reached.
/// If limit is reached, signals to stop accepting transactions and awaits indefinitely (never returns).
/// Should only be called for Produce commands.
async fn check_block_production_limit(
    limit: u64,
    already_produced_blocks_count: u64,
    tx_acceptance_state_sender: &watch::Sender<TransactionAcceptanceState>,
    latency_tracker: &ComponentStateHandle<SequencerState>,
) {
    if already_produced_blocks_count >= limit {
        tracing::warn!(
            already_produced_blocks_count,
            limit,
            "Reached max_blocks_to_produce limit, stopping transaction acceptance"
        );

        // Signal to RPC that we're no longer accepting transactions
        let _ = tx_acceptance_state_sender.send(TransactionAcceptanceState::NotAccepting(
            NotAcceptingReason::BlockProductionDisabled,
        ));

        latency_tracker.enter_state(SequencerState::ConfiguredBlockLimitReached);
        std::future::pending::<()>().await;
    }
}
