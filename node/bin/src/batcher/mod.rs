use crate::batcher::batch_deadline_policy::deadline_from_block_timestamp;
use crate::batcher::bitcoin_da_status_storage::{
    BitcoinDaBatchStatus, BitcoinDaFinalityPolicy, BitcoinDaStatusStorage,
};
use crate::batcher::seal_criteria::BatchInfoAccumulator;
use crate::config::BatcherConfig;
use alloy::hex;
use alloy::primitives::Address;
use anyhow::Context;
use async_trait::async_trait;
use bitcoin_da_client::SyscoinClient;
use secrecy::ExposeSecret;
use std::pin::Pin;
use tokio::sync::mpsc;
use tokio::time::{Instant, Sleep};
use tracing;
use zksync_os_batch_types::DiscoveredCommittedBatch;
use zksync_os_batch_types::batcher_model::{
    BatchEnvelope, BatchForSigning, MissingSignature, ProverInput, block_contains_interop_bundle,
};
use zksync_os_batch_types::syscoin_blob_ids_and_chunks_from_pubdata;
use zksync_os_batcher_metrics::BATCHER_METRICS;
use zksync_os_contract_interface::models::StoredBatchInfo;
use zksync_os_l1_watcher::CommittedBatchProvider;
use zksync_os_merkle_tree::{MerkleTree, RocksDBWrapper};
use zksync_os_observability::{ComponentStateReporter, GenericComponentState};
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent};
use zksync_os_storage_api::{ReadStateHistory, TreeBlock};
use zksync_os_types::{ProvingVersion, PubdataMode};

pub mod batch_builder;
mod batch_deadline_policy;
pub(crate) mod bitcoin_da_finality_gate;
pub(crate) mod bitcoin_da_status_cleanup;
pub(crate) mod bitcoin_da_status_storage;
mod seal_criteria;
pub mod util;
// SYSCOIN: Retry the two known Bitcoin Core ancestor-limit error spellings.
const BITCOIN_DA_ANCESTOR_LIMIT_ERRORS: &[&str] = &[
    "unconfirmed utxos are available, but spending them creates a chain of transactions that will be rejected by the mempool",
    "transaction has too long of a mempool chain",
];

/// Set of fields to define batcher's behavior on startup (when to replay, when to produce, etc.)
pub struct BatcherStartupConfig {
    pub last_committed_batch: u64,
    pub last_executed_batch: u64,
    /// Last block number already known to this node. On startup, we'll replay all blocks until and including
    /// this - in other words, there will be no arbitrary delays until this block is passed through Batcher.
    /// We do not seal batches by timeout until this block is reached.
    /// This helps to avoid premature sealing due to timeout criterion, since for every tick of the
    /// timer the `should_seal_by_timeout` will often return `true`
    /// (because those blocks were produced during the previous run of the node - maybe some time ago)
    pub last_persisted_block: u64,
}

/// Batcher component - handles batching logic, receives blocks and prepares batch data
pub struct Batcher<ReadState> {
    pub startup_config: BatcherStartupConfig,
    pub chain_id: u64,
    pub sl_chain_id: u64,
    pub chain_address_sl: Address,
    /// SYSCOIN: Guest-bound Gateway target whose successful commits yield compact edge-DA refs.
    pub compact_edge_da_commit_target: Address,
    pub pubdata_limit_bytes: u64,
    pub batcher_config: BatcherConfig,
    pub pubdata_mode: PubdataMode,
    pub committed_batch_provider: CommittedBatchProvider,
    pub read_state: ReadState,
    pub bitcoin_da_status_storage: BitcoinDaStatusStorage,
    pub merkle_tree: MerkleTree<RocksDBWrapper>,
}

fn is_bitcoin_da_ancestor_limit_error(err: &str) -> bool {
    let err = err.to_ascii_lowercase();
    BITCOIN_DA_ANCESTOR_LIMIT_ERRORS
        .iter()
        .any(|message| err.contains(message))
}

/// SYSCOIN: Advances the batch-boundary side of the replay-derived companion marker. A different
/// proving version cannot share a SNARK aggregation group; protocol/security upgrades keep their
/// absolute priority and expire the old tail instead of being delayed.
fn next_interop_companion_batch_state(
    pending: Option<ProvingVersion>,
    current: ProvingVersion,
    current_contains_bundle: bool,
) -> Option<ProvingVersion> {
    if let Some(expected) = pending
        && expected != current
    {
        tracing::warn!(
            ?expected,
            ?current,
            "expiring interop FRI companion at proving-version boundary; protocol upgrade retains priority"
        );
    }
    current_contains_bundle.then_some(current)
}

/// SYSCOIN: Names the two explicit batch boundaries that make an interop bundle and its one
/// successor become distinct FRI jobs.
fn interop_batch_seal_reason(
    force_companion: bool,
    added_block_count: usize,
    contains_bundle: bool,
) -> Option<&'static str> {
    if contains_bundle {
        Some("interop_bundle")
    } else if force_companion && added_block_count == 1 {
        Some("interop_companion")
    } else {
        None
    }
}

#[async_trait]
impl<ReadState: ReadStateHistory + Clone + Send + 'static> PipelineComponent
    for Batcher<ReadState>
{
    type Input = TreeBlock;
    type Output = BatchEnvelope<ProverInput, MissingSignature>;

    const COMPONENT_ID: zksync_os_pipeline::ComponentId = zksync_os_pipeline::ComponentId::Batcher;
    const OUTPUT_CHANNEL_CAPACITY: usize = 1;

    async fn run(
        mut self,
        mut input: PeekableReceiver<Self::Input>,
        output: mpsc::Sender<Self::Output>,
        state_reporter: ComponentStateReporter,
    ) -> anyhow::Result<()> {
        // We use last executed batch as the starting point. Next immediate batch we process will be
        // `last_executed_batch + 1`.
        let last_executed_batch = self
            .committed_batch_provider
            .wait_for_batch(self.startup_config.last_executed_batch)
            .await;
        let first_expected_block = last_executed_batch.last_block_number() + 1;
        let mut prev_batch_info = last_executed_batch.batch_info;

        // We might receive some blocks that belong to already executed batches. We can skip these
        // as there is no need to perform any L1 operations on them.
        loop {
            let Some(next_block_number) = input
                .peek_recv(|item| item.record.block_context.block_number)
                .await
            else {
                tracing::info!("inbound channel closed");
                return Ok(());
            };
            if next_block_number >= first_expected_block {
                break;
            }
            tracing::debug!(
                block_number = next_block_number,
                "skipping already executed on L1 block {next_block_number} (first unexecuted on L1 block is {first_expected_block})"
            );
            let skipped = input
                .recv_and_record_picked(&state_reporter)
                .await
                .expect("impossible: missing an already peeked batch");
            state_reporter.record_processed(
                skipped.record.block_context.block_number,
                Some(skipped.record.block_context.timestamp),
                None,
            );
        }

        // Only used for metrics/logs
        let mut last_created_batch_at: Option<Instant> = None;
        // SYSCOIN: This marker is derived solely from batch metadata rebuilt from canonical WAL
        // blocks. On restart, committed-but-unexecuted batches are recreated first and new batches
        // replay the remaining blocks, so no second durable schema or dual-write is required.
        let mut interop_companion_proving_version: Option<ProvingVersion> = None;

        loop {
            state_reporter.enter_state(GenericComponentState::Idle);

            // Peek at the next block to decide whether to recreate or create anew.
            let Some(next_block_number) = input
                .peek_recv(|item| item.record.block_context.block_number)
                .await
            else {
                tracing::info!("inbound channel closed");
                return Ok(());
            };
            state_reporter.enter_state(GenericComponentState::Active);

            let recreated;
            let batch_envelope =
                if prev_batch_info.batch_number < self.startup_config.last_committed_batch {
                    let committed_batch = self
                        .committed_batch_provider
                        .wait_for_batch(prev_batch_info.batch_number + 1)
                        .await;
                    // Validate that the existing batch's first block matches the next block in the stream
                    anyhow::ensure!(
                        committed_batch.first_block_number() == next_block_number,
                        "Existing batch first block ({}) does not match next block in stream ({})",
                        committed_batch.first_block_number(),
                        next_block_number
                    );

                    let Some(batch_envelope) = self
                        .recreate_existing_batch(
                            &mut input,
                            &prev_batch_info,
                            committed_batch,
                            &state_reporter,
                        )
                        .await?
                    else {
                        return Ok(());
                    };
                    recreated = true;
                    batch_envelope
                } else {
                    let Some(batch_envelope) = self
                        .create_batch(
                            &mut input,
                            &prev_batch_info,
                            &state_reporter,
                            interop_companion_proving_version.is_some(),
                        )
                        .await?
                    else {
                        return Ok(());
                    };
                    recreated = false;
                    batch_envelope
                };

            let time_since_last_batch =
                last_created_batch_at.map(|last_created_batch_at| last_created_batch_at.elapsed());
            if let Some(time_since_last_batch) = time_since_last_batch {
                BATCHER_METRICS
                    .time_since_last_batch
                    .observe(time_since_last_batch);
            }

            last_created_batch_at = Some(Instant::now());

            interop_companion_proving_version = next_interop_companion_batch_state(
                interop_companion_proving_version,
                batch_envelope.batch.proving_version()?,
                batch_envelope.batch.contains_interop_bundle(),
            );

            // Update prev_batch_info for the next iteration
            prev_batch_info = batch_envelope.batch.batch_info.clone().into_stored();

            BATCHER_METRICS
                .transactions_per_batch
                .observe(batch_envelope.batch.tx_count as u64);

            tracing::info!(
                batch_number = batch_envelope.batch_number(),
                batch_metadata = ?batch_envelope.batch,
                block_count = batch_envelope.batch.last_block_number - batch_envelope.batch.first_block_number + 1,
                new_state_commitment = ?batch_envelope.batch.batch_info.new_state_commitment,
                time_since_last_batch = ?time_since_last_batch,
                "Batch {}", if recreated { "recreated" } else { "created" }
            );

            tracing::debug!(
                batch_number = batch_envelope.batch_number(),
                da_commitment = ?batch_envelope.batch.batch_info.operator_da_input,
                "Batch da_input",
            );

            let last_block_number = batch_envelope.batch.last_block_number;
            let batch_number = batch_envelope.batch_number();
            output
                .send(batch_envelope)
                .await
                .context("batcher downstream channel closed")?;
            state_reporter.record_processed(last_block_number, None, Some(batch_number));
        }
    }
}

impl<ReadState: ReadStateHistory + Clone + Send + 'static> Batcher<ReadState> {
    async fn create_batch(
        &mut self,
        block_receiver: &mut PeekableReceiver<TreeBlock>,
        prev_batch_info: &StoredBatchInfo,
        state_reporter: &ComponentStateReporter,
        force_interop_companion: bool,
    ) -> anyhow::Result<Option<BatchForSigning<ProverInput>>> {
        // Armed once we reach `last_persisted_block`, using the first block's timestamp.
        let mut deadline: Option<Pin<Box<Sleep>>> = None;
        // Captured from the very first block added to the batch, even during catch-up replay.
        // This is the stable anchor for the deadline: it does not shift when the server restarts.
        let mut first_block_timestamp: Option<u64> = None;

        let batch_number = prev_batch_info.batch_number + 1;
        let mut blocks = vec![];
        let mut accumulator = BatchInfoAccumulator::new(
            self.batcher_config.blocks_per_batch_limit,
            self.batcher_config.tx_per_batch_limit,
            self.pubdata_limit_bytes,
            self.batcher_config.interop_roots_per_batch_limit,
            self.compact_edge_da_commit_target,
        );

        loop {
            state_reporter.enter_state(GenericComponentState::Idle);
            tokio::select! {
                /* ---------- check for timeout ---------- */
                _ = async {
                    if let Some(d) = &mut deadline {
                        d.as_mut().await
                    }
                }, if deadline.is_some() => {
                    BATCHER_METRICS.seal_reason[&"timeout"].inc();
                    tracing::debug!(batch_number, "Timeout reached, sealing the batch.");
                    break;
                }

                /* ---------- collect blocks ---------- */
               seal_decision = block_receiver.peek_recv(|item| {
                    // SYSCOIN: Evaluate the canonical forwarded-ref parser and aggregate cap on a
                    // cloned candidate so fallible cap errors fail closed without mutating the
                    // accepted accumulator; the match below then distinguishes seal from an
                    // intrinsically oversized first block.
                    // determine if the block fits into the current batch
                    let mut candidate = accumulator.clone();
                    candidate.add(&item.output, &item.record)?;
                    let exceeds_forwarded_da_refs_limit =
                        candidate.exceeds_forwarded_da_refs_limit();
                    let exceeds_standard_limit = candidate.should_seal();
                    let contains_interop_bundle = block_contains_interop_bundle(&item.output);
                    Ok::<_, anyhow::Error>((
                        exceeds_standard_limit,
                        exceeds_forwarded_da_refs_limit,
                        contains_interop_bundle,
                        item.record.block_context.block_number,
                    ))
                }) => {
                    state_reporter.enter_state(GenericComponentState::Active);
                    match seal_decision {
                        Some(Err(err)) => return Err(err),
                        Some(Ok((true, _, _, _))) if !blocks.is_empty() => {
                            // some of the limits was reached, start sealing the batch
                            break;
                        }
                        Some(Ok((_, true, _, block_number))) => {
                            // SYSCOIN: A block that alone forwards more than 32 openings can
                            // never fit any batch; accepting it under the generic first-block
                            // exception would create an unprovable and uncommittable batch.
                            anyhow::bail!(
                                "block {} exceeds the Gateway forwarded Bitcoin DA ref limit of {}",
                                block_number,
                                zksync_os_batch_types::SYSCOIN_DA_MAX_REFS_PER_BATCH,
                            );
                        }
                        Some(Ok((exceeds_standard_limit, _, contains_interop_bundle, _))) => {
                            // `exceeds_standard_limit` means the batch is still empty and the peeked
                            // block alone exceeds a seal limit. A batch must contain at least
                            // one block — refusing it would replay the same block forever — so
                            // accept it as a single-block batch.
                            if exceeds_standard_limit {
                                tracing::warn!(
                                    batch_number,
                                    "a single block exceeds batch seal limits; sealing it as its own batch"
                                );
                            }
                            let Some(block) = block_receiver.pop_buffer() else {
                                anyhow::bail!("No block received in buffer after peeking")
                            };

                            let block_number = block.record.block_context.block_number;

                            state_reporter.record_picked(
                                block_number,
                                Some(block.record.block_context.timestamp),
                                None,
                            );

                            tracing::debug!(
                                batch_number,
                                block_number,
                                "Adding block to a pending batch."
                            );

                            // Always record the first block's timestamp as the stable deadline
                            // anchor. This must happen before the last_persisted_block check so
                            // that restarts do not shift the reference block forward to the
                            // catch-up frontier.
                            let first_block_timestamp = first_block_timestamp
                                .get_or_insert(block.record.block_context.timestamp);

                            // Arm the timer only once catch-up replay is complete. The deadline
                            // itself is derived from first_block_timestamp — not from the block
                            // that trips this condition — so it remains stable across restarts.
                            if deadline.is_none()
                                && block_number >= self.startup_config.last_persisted_block
                            {
                                let (instant, unix_deadline) = deadline_from_block_timestamp(
                                    *first_block_timestamp,
                                    self.batcher_config.batch_timeout,
                                );
                                tracing::info!(
                                    "Armed batch deadline for batch {batch_number} from first block timestamp {first_block_timestamp}, sealing at unix={unix_deadline}"
                                );
                                deadline = Some(Box::pin(tokio::time::sleep_until(instant)));
                            }

                            // ---------- accumulate batch data ----------
                            accumulator.add(&block.output, &block.record)?;

                            blocks.push(block);

                            // SYSCOIN: An authenticated bundle must end its batch, and the first
                            // block after a bundle must end the next batch. This creates two
                            // distinct FRI jobs while preserving Airbender's stock min-two SNARK
                            // aggregation. A consecutive bundle satisfies the old tail and arms a
                            // new one through the metadata transition in `run`.
                            if let Some(reason) = interop_batch_seal_reason(
                                force_interop_companion,
                                blocks.len(),
                                contains_interop_bundle,
                            ) {
                                BATCHER_METRICS.seal_reason[&reason].inc();
                                tracing::info!(
                                    batch_number,
                                    block_number,
                                    reason,
                                    "sealing interop FRI batch boundary"
                                );
                                break;
                            }

                            if exceeds_standard_limit {
                                break;
                            }
                        }
                        None => {
                            tracing::info!("inbound channel closed");
                            return Ok(None);
                        }
                    }
                }
            }
        }
        BATCHER_METRICS
            .blocks_per_batch
            .observe(blocks.len() as u64);
        accumulator.report_accumulated_resources_to_metrics();

        let pubdata_mode = self.pubdata_mode;
        let uses_syscoin_da = matches!(
            pubdata_mode,
            PubdataMode::Blobs | PubdataMode::RelayedL2Calldata
        );
        /* ---------- seal the batch ---------- */
        let sealed_batch = self
            .seal_batch_blocking(blocks, prev_batch_info.clone(), batch_number, pubdata_mode)
            .await?;
        let batch_envelope = sealed_batch.batch;
        // SYSCOIN: `RelayedL2Calldata` is a compact edge-DA reference mode when settling to
        // Gateway; it uses the same Bitcoin DA publication and hash-array commitment as blobs.
        if uses_syscoin_da {
            let total_pubdata = sealed_batch.canonical_pubdata;
            let (blob_ids_from_pubdata, blob_chunks_from_pubdata) =
                syscoin_blob_ids_and_chunks_from_pubdata(&total_pubdata)?;
            anyhow::ensure!(
                blob_ids_from_pubdata == batch_envelope.batch.batch_info.operator_da_input,
                "canonical blob ids mismatch committed operator DA input for batch {batch_number}",
            );
            self.publish_bitcoin_da(
                batch_number,
                &blob_chunks_from_pubdata,
                &batch_envelope.batch.batch_info.operator_da_input,
            )
            .await?;
        }
        Ok(Some(batch_envelope))
    }

    /// Runs [`batch_builder::seal_batch`] on a blocking thread: sealing runs batch PIG
    /// (for V8 - a full native re-execution of the batch), which must not stall the
    /// async runtime.
    async fn seal_batch_blocking(
        &self,
        blocks: Vec<TreeBlock>,
        prev_batch_info: StoredBatchInfo,
        batch_number: u64,
        pubdata_mode: PubdataMode,
    ) -> anyhow::Result<batch_builder::SealedBatch> {
        let chain_id = self.chain_id;
        let chain_address_sl = self.chain_address_sl;
        let sl_chain_id = self.sl_chain_id;
        let compact_edge_da_commit_target = self.compact_edge_da_commit_target;
        let read_state = self.read_state.clone();
        let merkle_tree = self.merkle_tree.clone();
        tokio::task::spawn_blocking(move || {
            batch_builder::seal_batch(
                &blocks,
                prev_batch_info,
                batch_number,
                chain_id,
                chain_address_sl,
                pubdata_mode,
                sl_chain_id,
                compact_edge_da_commit_target,
                &read_state,
                &merkle_tree,
            )
        })
        .await?
    }

    async fn recreate_existing_batch(
        &mut self,
        block_receiver: &mut PeekableReceiver<TreeBlock>,
        prev_batch_info: &StoredBatchInfo,
        existing_batch: DiscoveredCommittedBatch,
        state_reporter: &ComponentStateReporter,
    ) -> anyhow::Result<Option<BatchForSigning<ProverInput>>> {
        let batch_number = existing_batch.number();

        tracing::info!(
            batch_number,
            first_block = existing_batch.first_block_number(),
            last_block = existing_batch.last_block_number(),
            "Recreating existing batch"
        );

        let mut blocks = vec![];

        let expected_block_count = existing_batch.block_count();
        // Collect all blocks in this batch
        while blocks.len() < expected_block_count as usize {
            state_reporter.enter_state(GenericComponentState::Idle);
            let Some(block) = block_receiver.recv().await else {
                tracing::info!("inbound channel closed");
                return Ok(None);
            };
            state_reporter.enter_state(GenericComponentState::Active);

            tracing::debug!(
                batch_number,
                block_number = block.record.block_context.block_number,
                "Adding block to recreated batch"
            );

            // Mirrors the record_picked call in create_batch; needed here too because
            // recreate_existing_batch is a separate code path for already-committed batches.
            state_reporter.record_picked(
                block.record.block_context.block_number,
                Some(block.record.block_context.timestamp),
                None,
            );

            blocks.push(block);
        }
        let last_block_number = blocks.last().unwrap().output.header.number;
        assert_eq!(
            last_block_number,
            existing_batch.last_block_number(),
            "Block number mismatch in last block of a rebuilt batch"
        );

        // Rebuild the batch from blocks.
        // Assume pubdata mode does not change
        let rebuilt_batch = self
            .seal_batch_blocking(
                blocks,
                prev_batch_info.clone(),
                batch_number,
                self.pubdata_mode,
            )
            .await?
            .batch;

        // Verify that the rebuilt batch matches the stored batch by comparing hashes
        if self.batcher_config.assert_rebuilt_batch_hashes {
            let rebuilt_stored_batch_info = rebuilt_batch.batch.batch_info.clone().into_stored();

            anyhow::ensure!(
                rebuilt_stored_batch_info.hash() == existing_batch.batch_info.hash(),
                "Rebuilt batch info does not match stored batch info for batch {}. \
                 Rebuilt info: {:?}, Stored info: {:?}",
                batch_number,
                rebuilt_stored_batch_info,
                existing_batch.batch_info
            );
        } else {
            tracing::warn!(
                batch_number,
                "Batch hash verification is disabled - skipping verification of rebuilt batch"
            );
        }

        Ok(Some(rebuilt_batch))
    }

    // SYSCOIN: publish each sealed batch to Syscoin Bitcoin DA. Finality is
    // enforced later, immediately before the commit transaction is sent to L1.
    async fn publish_bitcoin_da(
        &self,
        batch_number: u64,
        blob_chunks: &[Vec<u8>],
        expected_version_hashes: &[u8],
    ) -> anyhow::Result<()> {
        let rpc_url = self
            .batcher_config
            .bitcoin_da_rpc_url
            .as_deref()
            .context("`batcher.bitcoin_da_rpc_url` must be set when using blob pubdata mode")?;
        let rpc_user =
            self.batcher_config.bitcoin_da_rpc_user.as_ref().context(
                "`batcher.bitcoin_da_rpc_user` must be set when using blob pubdata mode",
            )?;
        let rpc_password = self
            .batcher_config
            .bitcoin_da_rpc_password
            .as_ref()
            .context(
                "`batcher.bitcoin_da_rpc_password` must be set when using blob pubdata mode",
            )?;

        let client = SyscoinClient::new(
            rpc_url,
            rpc_user.expose_secret(),
            rpc_password.expose_secret(),
            &self.batcher_config.bitcoin_da_poda_url,
            Some(self.batcher_config.bitcoin_da_request_timeout),
            &self.batcher_config.bitcoin_da_wallet_name,
        )
        .map_err(|err| anyhow::anyhow!("failed to create Bitcoin DA client: {err}"))?;
        let _funding_address = client
            .ensure_own_wallet_and_address(&self.batcher_config.bitcoin_da_address_label)
            .await
            .map_err(|err| {
                anyhow::anyhow!("failed to initialize Bitcoin DA wallet/address: {err}")
            })?;

        let expected_hashes: Vec<String> = expected_version_hashes
            .chunks_exact(32)
            .map(hex::encode)
            .collect();
        anyhow::ensure!(
            blob_chunks.len() == expected_hashes.len(),
            "bitcoin publication blob count mismatch: built {}, committed {}",
            blob_chunks.len(),
            expected_hashes.len()
        );
        let current_finality_policy = BitcoinDaFinalityPolicy {
            mode: self.batcher_config.bitcoin_da_finality_mode,
            confirmations: self.batcher_config.bitcoin_da_finality_confirmations,
        };
        // SYSCOIN: Resume Bitcoin DA publication only when the durable status still matches the
        // canonical blob hashes and finality policy for this batch.
        let status_storage = &self.bitcoin_da_status_storage;
        let mut status = match status_storage.load(batch_number).await? {
            Some(status)
                if status.expected_hashes == expected_hashes
                    && status.published_hashes.len() <= expected_hashes.len()
                    && (!status.finalized
                        || (status.published_hashes.len() == expected_hashes.len()
                            && status.finality_policy.as_ref()
                                == Some(&current_finality_policy))) =>
            {
                status
            }
            Some(status)
                if status.expected_hashes == expected_hashes
                    && status.published_hashes.len() == expected_hashes.len()
                    && status.finalized =>
            {
                tracing::info!(
                    batch_number,
                    stored_policy = ?status.finality_policy,
                    current_policy = ?current_finality_policy,
                    "revalidating Bitcoin DA finality under current policy"
                );
                BitcoinDaBatchStatus {
                    expected_hashes: status.expected_hashes,
                    published_hashes: status.published_hashes,
                    finalized: false,
                    finality_policy: Some(current_finality_policy.clone()),
                }
            }
            Some(status) => {
                tracing::warn!(
                    batch_number,
                    stored_expected = ?status.expected_hashes,
                    current_expected = ?expected_hashes,
                    "discarding stale Bitcoin DA publication state for batch"
                );
                BitcoinDaBatchStatus {
                    expected_hashes: expected_hashes.clone(),
                    published_hashes: Vec::new(),
                    finalized: false,
                    finality_policy: Some(current_finality_policy.clone()),
                }
            }
            None => BitcoinDaBatchStatus {
                expected_hashes: expected_hashes.clone(),
                published_hashes: Vec::new(),
                finalized: false,
                finality_policy: Some(current_finality_policy.clone()),
            },
        };
        if status.finality_policy.is_none() {
            status.finality_policy = Some(current_finality_policy.clone());
        }
        // SYSCOIN: Publish missing 2 MiB DA chunks sequentially and persist progress so a restart
        // does not republish chunks that Bitcoin DA already accepted.
        if !status.finalized {
            for (idx, (blob, expected_hash)) in blob_chunks
                .iter()
                .zip(expected_hashes.iter())
                .enumerate()
                .skip(status.published_hashes.len())
            {
                let start = Instant::now();
                let version_hash = loop {
                    match client.create_blob(blob).await {
                        Ok(version_hash) => break version_hash,
                        Err(err) => {
                            let err = err.to_string();
                            if !is_bitcoin_da_ancestor_limit_error(&err) {
                                anyhow::bail!(
                                    "failed to publish Bitcoin DA blob {idx} for batch {batch_number}: {err}"
                                );
                            }
                            if start.elapsed() >= self.batcher_config.bitcoin_da_finality_timeout {
                                anyhow::bail!(
                                    "Bitcoin DA publish for batch {batch_number}, blob {idx} remained blocked by Syscoin mempool ancestor limits for {:?}: {err}",
                                    self.batcher_config.bitcoin_da_finality_timeout
                                );
                            }

                            tracing::warn!(
                                batch_number,
                                blob_index = idx,
                                retry_in = ?self.batcher_config.bitcoin_da_finality_poll_interval,
                                "Bitcoin DA publish hit Syscoin mempool ancestor limit; waiting before retry"
                            );
                            tokio::time::sleep(
                                self.batcher_config.bitcoin_da_finality_poll_interval,
                            )
                            .await;
                        }
                    }
                };
                let normalized_hash = version_hash.strip_prefix("0x").unwrap_or(&version_hash);
                anyhow::ensure!(
                    normalized_hash.eq_ignore_ascii_case(expected_hash),
                    "Bitcoin DA version hash mismatch for batch {batch_number}, blob {idx}: expected {expected_hash}, got {normalized_hash}"
                );
                status.published_hashes.push(version_hash);
                status_storage.save(batch_number, &status).await?;
            }
        }

        tracing::info!(
            batch_number,
            version_hashes = ?status.published_hashes,
            chunk_count = blob_chunks.len(),
            "Published Bitcoin DA blobs"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        interop_batch_seal_reason, is_bitcoin_da_ancestor_limit_error,
        next_interop_companion_batch_state,
    };
    use zksync_os_types::ProvingVersion;

    #[test]
    fn detects_bitcoin_da_ancestor_limit_errors() {
        assert!(is_bitcoin_da_ancestor_limit_error(
            "Unconfirmed UTXOs are available, but spending them creates a chain of transactions that will be rejected by the mempool",
        ));
        assert!(is_bitcoin_da_ancestor_limit_error(
            "WALLET RPC `syscoincreatenevmblob` -> HTTP 500 Internal Server Error: {\"error\":{\"code\":-6,\"message\":\"Transaction has too long of a mempool chain\"}}",
        ));
        assert!(!is_bitcoin_da_ancestor_limit_error(
            "HTTP 500 Internal Server Error: insufficient funds",
        ));
    }

    #[test]
    fn interop_companion_batch_state_is_replay_deterministic() {
        let version = ProvingVersion::V8;
        let pending = next_interop_companion_batch_state(None, version, true);
        assert_eq!(pending, Some(version));
        assert_eq!(
            next_interop_companion_batch_state(pending, version, false),
            None
        );
        // A consecutive bundle is a companion for the previous batch and arms one new tail.
        assert_eq!(
            next_interop_companion_batch_state(pending, version, true),
            Some(version)
        );
    }

    #[test]
    fn interop_boundaries_seal_bundle_and_exactly_one_successor_block() {
        assert_eq!(
            interop_batch_seal_reason(false, 3, true),
            Some("interop_bundle")
        );
        assert_eq!(
            interop_batch_seal_reason(true, 1, false),
            Some("interop_companion")
        );
        assert_eq!(interop_batch_seal_reason(true, 2, false), None);
        assert_eq!(interop_batch_seal_reason(false, 1, false), None);
    }

    #[test]
    fn committed_not_executed_bundle_rearms_successor_boundary_after_restart() {
        // `run` recreates committed-but-not-settlement-executed batches before creating new
        // ones. Rebuilt durable metadata therefore restores the marker without an auxiliary DB.
        let recovered = next_interop_companion_batch_state(None, ProvingVersion::V8, true);
        assert_eq!(
            interop_batch_seal_reason(recovered.is_some(), 1, false),
            Some("interop_companion")
        );
        // Observing that isolated successor clears the recovered obligation, so a third batch is
        // not forced after another restart.
        assert_eq!(
            next_interop_companion_batch_state(recovered, ProvingVersion::V8, false),
            None
        );
    }
}
