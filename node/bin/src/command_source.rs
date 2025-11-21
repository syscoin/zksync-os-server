use crate::replay_transport::replay_receiver;
use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use std::collections::HashSet;
use std::time::Duration;
use tokio::sync::mpsc;
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent};
use zksync_os_sequencer::model::blocks::{BlockCommand, ProduceCommand, RebuildCommand};
use zksync_os_storage_api::{ReadReplay, ReadReplayExt};

/// Main node command source
#[derive(Debug)]
pub struct MainNodeCommandSource<Replay> {
    pub block_replay_storage: Replay,
    pub starting_block: u64,
    pub rebuild_options: Option<RebuildOptions>,
    pub block_time: Duration,
    pub max_transactions_in_block: usize,
    pub drop_blocks_from_height: Option<u64>,
}

#[derive(Debug)]
pub struct RebuildOptions {
    pub rebuild_from_block: u64,
    pub blocks_to_empty: HashSet<u64>,
}

/// External node command source
#[derive(Debug)]
pub struct ExternalNodeCommandSource {
    pub starting_block: u64,
    pub replay_download_address: String,
}

#[async_trait]
impl<Replay: ReadReplay> PipelineComponent for MainNodeCommandSource<Replay> {
    type Input = ();
    type Output = BlockCommand;

    const NAME: &'static str = "command_source";
    const OUTPUT_BUFFER_SIZE: usize = 5;

    async fn run(
        self,
        _input: PeekableReceiver<()>,
        output: mpsc::Sender<BlockCommand>,
    ) -> anyhow::Result<()> {
        // TODO: no need for a Stream in `command_source` - just send to channel right away instead
        let mut stream = command_source(
            &self.block_replay_storage,
            self.starting_block,
            self.block_time,
            self.max_transactions_in_block,
            self.rebuild_options,
            self.drop_blocks_from_height,
        );

        while let Some(command) = stream.next().await {
            tracing::debug!(?command, "Sending block command");
            if output.send(command).await.is_err() {
                tracing::warn!("Command output channel closed, stopping source");
                break;
            }
        }

        Ok(())
    }
}

#[async_trait]
impl PipelineComponent for ExternalNodeCommandSource {
    type Input = ();
    type Output = BlockCommand;

    const NAME: &'static str = "external_node_command_source";
    const OUTPUT_BUFFER_SIZE: usize = 5;

    async fn run(
        self,
        _input: PeekableReceiver<()>,
        output: mpsc::Sender<BlockCommand>,
    ) -> anyhow::Result<()> {
        // TODO: no need for a Stream in `replay_receiver` - just send to channel right away instead
        let mut stream = replay_receiver(self.starting_block, self.replay_download_address.clone())
            .await
            .map_err(|err| {
                tracing::error!(?err, "Failed to connect to main node to receive blocks");
                err
            })?;

        while let Some(command) = stream.next().await {
            tracing::debug!(?command, "Received block command from main node");
            if output.send(command).await.is_err() {
                tracing::warn!("Command output channel closed, stopping source");
                break;
            }
        }

        Ok(())
    }
}

fn command_source(
    block_replay_wal: &impl ReadReplay,
    block_to_start: u64,
    block_time: Duration,
    max_transactions_in_block: usize,
    rebuild_options: Option<RebuildOptions>,
    drop_blocks_from_height: Option<u64>,
) -> BoxStream<BlockCommand> {
    let last_block_in_wal = block_replay_wal.latest_record();

    // Determine the effective last block to replay, considering drop_blocks_from_height
    let effective_last_block_in_wal = if let Some(drop_height) = drop_blocks_from_height {
        assert!(
            drop_height > block_to_start,
            "drop_blocks_from_height must be > block_to_start, got {drop_height} <= {block_to_start}"
        );
        assert!(
            drop_height <= last_block_in_wal + 1,
            "drop_blocks_from_height must be <= last_block_in_wal + 1, got {drop_height} > {}",
            last_block_in_wal + 1
        );
        drop_height - 1
    } else {
        last_block_in_wal
    };

    tracing::info!(
        last_block_in_wal,
        effective_last_block_in_wal,
        block_to_start,
        ?rebuild_options,
        ?drop_blocks_from_height,
        "starting command source"
    );

    let (replay_end, rebuild_stream): (u64, BoxStream<BlockCommand>) =
        if let Some(rebuild_options) = rebuild_options {
            let rebuild_from = rebuild_options.rebuild_from_block;
            assert!(
                rebuild_from >= block_to_start,
                "rebuild_from_block must be >= block_to_start, got {rebuild_from} < {block_to_start}"
            );

            assert!(
                rebuild_from <= effective_last_block_in_wal,
                "rebuild_from_block must be <= effective_last_block_in_wal, got {rebuild_from} > {effective_last_block_in_wal}"
            );

            let command_iterator = (rebuild_options.rebuild_from_block
                ..=effective_last_block_in_wal)
                .map(move |block_number| {
                    let replay_record = block_replay_wal
                        .get_replay_record(block_number)
                        .expect("Replay record must exist for rebuild");
                    let make_empty = rebuild_options.blocks_to_empty.contains(&block_number);
                    BlockCommand::Rebuild(Box::new(RebuildCommand {
                        replay_record,
                        make_empty,
                    }))
                });
            (
                rebuild_options.rebuild_from_block - 1,
                futures::stream::iter(command_iterator).boxed(),
            )
        } else {
            (
                effective_last_block_in_wal,
                futures::stream::empty().boxed(),
            )
        };

    // Stream of replay commands from WAL
    // Guaranteed to stream exactly `[block_to_start; replay_end]`.
    let replay_wal_stream = block_replay_wal
        .stream(block_to_start, replay_end)
        .map(|record| BlockCommand::Replay(Box::new(record)));

    // Start producing from drop_blocks_from_height if set, otherwise from last_block_in_wal + 1
    let produce_start_block = drop_blocks_from_height.unwrap_or(last_block_in_wal + 1);
    let produce_stream: BoxStream<BlockCommand> =
        futures::stream::unfold(produce_start_block, move |block_number| async move {
            Some((
                BlockCommand::Produce(ProduceCommand {
                    block_number,
                    block_time,
                    max_transactions_in_block,
                }),
                block_number + 1,
            ))
        })
        .boxed();
    // Combined source: run WAL replay first, then rebuild (normally empty), then produce blocks from mempool
    let stream = replay_wal_stream
        .chain(rebuild_stream)
        .chain(produce_stream);
    stream.boxed()
}
