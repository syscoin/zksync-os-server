use crate::util;
use alloy::primitives::BlockNumber;
use alloy::providers::Provider;
use anyhow::Context;
use futures::stream::{self, StreamExt};
use rangemap::RangeInclusiveMap;
use reth_tasks::Runtime;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::time::sleep;
use zksync_os_batch_types::DiscoveredCommittedBatch;
use zksync_os_contract_interface::ZkChain;
use zksync_os_contract_interface::l1_discovery::L1State;
use zksync_os_contract_interface::models::StoredBatchInfo;
use zksync_os_contract_interface::settlement_layer_intervals::{
    IntervalSettlementLayer, SettlementLayerIntervals,
};
use zksync_os_provider::NodeProvider;
use zksync_os_storage_api::ReadBatch;

const INIT_MAX_PARALLEL_BATCH_FETCHES: usize = 10;
const WAIT_FOR_BATCH_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// In-memory store of committed batches discovered on startup and by the live commit watcher.
///
/// This component provides a single lookup / wait API for committed batch metadata regardless of
/// whether the batch came from startup catch-up or from the live `L1CommitWatcher`.
///
/// Depended on by:
/// - `L1ExecuteWatcher`, which waits for a committed batch before marking it executed;
/// - `Batcher`, which replays existing L1 batches before creating new ones;
/// - `PriorityTreeManager`, which reconstructs / advances the priority tree using committed batch
///   boundaries.
///
/// Construct it with [`Self::new`], which eagerly loads the startup frontier batches needed by
/// startup bookkeeping. Then run [`Self::init`] in a background task to populate the remaining
/// historical committed range while consumers use [`Self::wait_for_batch`] to block until a
/// specific batch becomes available.
#[derive(Debug, Clone)]
pub struct CommittedBatchProvider {
    inner: Arc<RwLock<Inner>>,
    /// Intervals used to route batch lookups to the diamond proxy of the SL the batch was
    /// committed to.
    intervals: SettlementLayerIntervals,
}

#[derive(Debug, Default)]
struct Inner {
    batches: HashMap<u64, DiscoveredCommittedBatch>,
    block_range_index: RangeInclusiveMap<BlockNumber, u64>,
}

impl CommittedBatchProvider {
    /// Creates a provider, inserts the genesis batch if needed, and eagerly loads the startup
    /// frontier batches used by startup bookkeeping.
    // SYSCOIN: Thread persisted batch storage through startup loading so restarts
    // use local committed-batch data before falling back to archive L1 calls.
    pub async fn new<BatchStorage>(
        runtime: &Runtime,
        l1_state: &L1State,
        max_l1_blocks_to_scan: u64,
        batch_storage: BatchStorage,
        archive_l1_provider: Option<NodeProvider>,
        load_genesis_batch_info: impl AsyncFnOnce() -> StoredBatchInfo,
    ) -> anyhow::Result<Self>
    where
        BatchStorage: ReadBatch + Clone,
    {
        let provider = Self {
            inner: Arc::new(RwLock::new(Inner::default())),
            intervals: l1_state.settlement_layer_intervals.clone(),
        };
        // Special case for genesis
        if l1_state.last_executed_batch == 0 {
            let batch_info = load_genesis_batch_info().await;
            let batch_hash_l1 = l1_state.diamond_proxy_l1.stored_batch_hash(0).await?;
            anyhow::ensure!(
                batch_hash_l1 == batch_info.hash(),
                "genesis batch hash mismatch: L1 {}, local {}",
                batch_hash_l1,
                batch_info.hash(),
            );
            provider.insert(DiscoveredCommittedBatch {
                batch_info,
                block_range: 0..=0,
            });
        }

        let (prioritized_batch_numbers, _) = startup_batch_numbers(
            l1_state.last_committed_batch,
            l1_state.last_proved_batch,
            l1_state.last_executed_batch,
            l1_state.last_finalized_executed_batch,
        );
        provider
            .load_batch_numbers(
                max_l1_blocks_to_scan,
                &batch_storage,
                archive_l1_provider.as_ref(),
                prioritized_batch_numbers,
            )
            .await?;

        let provider_for_init = provider.clone();
        let last_committed = l1_state.last_committed_batch;
        let last_proved = l1_state.last_proved_batch;
        let last_executed = l1_state.last_executed_batch;
        let last_finalized_executed = l1_state.last_finalized_executed_batch;
        let batch_storage_for_init = batch_storage.clone();
        let archive_l1_provider_for_init = archive_l1_provider.clone();
        runtime.spawn_critical_task("committed batch provider init", async move {
            provider_for_init
                .init(
                    last_committed,
                    last_proved,
                    last_executed,
                    last_finalized_executed,
                    max_l1_blocks_to_scan,
                    batch_storage_for_init,
                    archive_l1_provider_for_init,
                )
                .await
                .expect("failed to initialize CommittedBatchProvider");
        });

        Ok(provider)
    }

    /// Loads the remaining historical committed batches discovered on startup.
    // SYSCOIN: Keep background startup catch-up cache-first too.
    async fn init<BatchStorage>(
        &self,
        last_committed_batch: u64,
        last_proved_batch: u64,
        last_executed_batch: u64,
        last_finalized_executed_batch: u64,
        max_l1_blocks_to_scan: u64,
        batch_storage: BatchStorage,
        archive_l1_provider: Option<NodeProvider>,
    ) -> anyhow::Result<()>
    where
        BatchStorage: ReadBatch,
    {
        let (_, remaining_batch_numbers) = startup_batch_numbers(
            last_committed_batch,
            last_proved_batch,
            last_executed_batch,
            last_finalized_executed_batch,
        );
        self.load_batch_numbers(
            max_l1_blocks_to_scan,
            &batch_storage,
            archive_l1_provider.as_ref(),
            remaining_batch_numbers,
        )
        .await?;
        Ok(())
    }

    pub(crate) fn insert(&self, batch: DiscoveredCommittedBatch) {
        let mut inner = self.inner.write().expect("lock poisoned");
        inner.insert(batch);
    }

    /// Waits until the requested batch is available in memory.
    ///
    /// Startup initialization and live L1 watchers both populate this provider, so callers can use
    /// a single API regardless of whether the batch is historical or just arrived from L1.
    pub async fn wait_for_batch(&self, batch_number: u64) -> DiscoveredCommittedBatch {
        let mut logged_wait = false;
        loop {
            let batch = {
                let inner = self.inner.read().expect("lock poisoned");
                inner.batches.get(&batch_number).cloned()
            };
            if let Some(batch) = batch {
                tracing::info!("returning batch {batch_number} from CommittedBatchProvider");
                return batch;
            }
            if !logged_wait {
                tracing::info!("waiting for committed batch {batch_number} to load");
                logged_wait = true;
            }
            sleep(WAIT_FOR_BATCH_POLL_INTERVAL).await;
        }
    }

    /// SYSCOIN: returns the committed batch containing the requested L2 block if it is already
    /// indexed in memory.
    pub fn get_batch_containing_block(
        &self,
        block_number: BlockNumber,
    ) -> Option<DiscoveredCommittedBatch> {
        let inner = self.inner.read().expect("lock poisoned");
        inner
            .block_range_index
            .get(&block_number)
            .and_then(|batch_number| inner.batches.get(batch_number))
            .cloned()
    }

    /// Returns `DiscoveredCommittedBatch` from in-memory map if available.
    pub fn get(&self, batch_number: u64) -> Option<DiscoveredCommittedBatch> {
        let inner = self.inner.read().expect("lock poisoned");
        inner.batches.get(&batch_number).cloned()
    }

    /// Fetches a batch set with bounded concurrency to reduce startup latency without issuing an
    /// unbounded number of L1 requests.
    // SYSCOIN: Check persisted executed batch storage before issuing historical L1 calls.
    async fn load_batch_numbers<BatchStorage>(
        &self,
        max_l1_blocks_to_scan: u64,
        batch_storage: &BatchStorage,
        archive_l1_provider: Option<&NodeProvider>,
        batch_numbers: Vec<u64>,
    ) -> anyhow::Result<()>
    where
        BatchStorage: ReadBatch,
    {
        stream::iter(batch_numbers)
            .map(|batch_number| async move {
                if let Some(committed_batch) = load_persisted_batch(batch_storage, batch_number)? {
                    tracing::info!(
                        batch_number,
                        "loaded committed batch from persisted batch storage on startup",
                    );
                    self.insert(committed_batch);
                    return anyhow::Ok(());
                }

                let interval = self
                    .intervals
                    .find_interval(batch_number)
                    .with_context(|| format!("batch {batch_number} does not belong to any known settlement layer interval"))?;
                let discovered_batch =
                    match (&interval.settlement_layer, archive_l1_provider.as_ref()) {
                        (IntervalSettlementLayer::L1, Some(provider)) => {
                            // SYSCOIN: Accept archive-backed batch metadata only after
                            // verifying archive freshness against the live provider.
                            fetch_batch_with_archive_fallback(
                                &interval.proxy,
                                provider,
                                batch_number,
                                max_l1_blocks_to_scan,
                            )
                            .await?
                        }
                        _ => fetch_batch(&interval.proxy, batch_number, max_l1_blocks_to_scan)
                            .await?,
                    };
                tracing::info!(
                    batch_number = discovered_batch.number(),
                    "discovered committed batch {} on startup",
                    discovered_batch.number()
                );
                self.insert(discovered_batch);
                anyhow::Ok(())
            })
            .buffer_unordered(INIT_MAX_PARALLEL_BATCH_FETCHES)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(())
    }
}

impl Inner {
    fn insert(&mut self, batch: DiscoveredCommittedBatch) {
        self.block_range_index
            .insert(batch.block_range.clone(), batch.number());
        self.batches.insert(batch.number(), batch);
    }
}

/// Returns startup frontier batches first, then the remaining committed startup range.
///
/// The prioritized vector contains every batch needed for immediate startup bookkeeping:
/// committed, proved, operational executed, and finalized executed.
fn startup_batch_numbers(
    last_committed_batch: u64,
    last_proved_batch: u64,
    last_executed_batch: u64,
    last_finalized_executed_batch: u64,
) -> (Vec<u64>, Vec<u64>) {
    let prioritized = [
        last_committed_batch,
        last_proved_batch,
        last_executed_batch,
        last_finalized_executed_batch,
    ];
    let (prioritized_in_range, remaining_batch_numbers): (Vec<_>, Vec<_>) =
        (last_finalized_executed_batch.max(1)..=last_committed_batch)
            .partition(|batch_number| prioritized.contains(batch_number));
    (prioritized_in_range, remaining_batch_numbers)
}

// SYSCOIN: Rehydrate committed batch metadata that was already persisted by the L1 watcher.
fn load_persisted_batch<BatchStorage>(
    batch_storage: &BatchStorage,
    batch_number: u64,
) -> anyhow::Result<Option<DiscoveredCommittedBatch>>
where
    BatchStorage: ReadBatch,
{
    Ok(batch_storage
        .get_batch_by_number(batch_number)
        .with_context(|| format!("failed to read persisted batch {batch_number} during startup"))?
        .map(|batch| batch.committed_batch))
}

/// Resolves a committed batch from L1 by first finding the block that committed it and then
/// decoding the corresponding stored batch data.
async fn fetch_batch(
    diamond_proxy_sl: &ZkChain<NodeProvider>,
    batch_number: u64,
    max_l1_blocks_to_scan: u64,
) -> anyhow::Result<DiscoveredCommittedBatch> {
    let sl_block_with_commit = util::find_l1_commit_block_by_batch_number(
        diamond_proxy_sl.clone(),
        batch_number,
        max_l1_blocks_to_scan,
    )
    .await?;
    util::fetch_stored_batch_data(diamond_proxy_sl, sl_block_with_commit, batch_number)
        .await?
        .with_context(|| format!("failed to find committed batch {batch_number} on L1"))
}

// SYSCOIN: Archive-backed batch metadata is only safe if the archive endpoint
// has caught up to the live endpoint. The live storedBatchHash authenticates
// StoredBatchInfo, but not the L2 block range decoded from the commit log.
async fn fetch_batch_with_archive_fallback(
    live_proxy: &ZkChain<NodeProvider>,
    archive_provider: &NodeProvider,
    batch_number: u64,
    max_l1_blocks_to_scan: u64,
) -> anyhow::Result<DiscoveredCommittedBatch> {
    let live_tip = live_proxy
        .provider()
        .get_block_number()
        .await
        .with_context(|| {
            format!("failed to fetch live provider tip before batch {batch_number} lookup")
        })?;
    let archive_tip = match archive_provider.get_block_number().await {
        Ok(archive_tip) => archive_tip,
        Err(archive_tip_err) => {
            let archive_tip_err = format!("{archive_tip_err:#}");
            tracing::warn!(
                batch_number,
                live_tip,
                archive_error = archive_tip_err,
                "archive provider tip lookup failed; retrying committed batch lookup on live provider",
            );
            return fetch_batch(live_proxy, batch_number, max_l1_blocks_to_scan)
                .await
                .with_context(|| {
                    format!(
                        "archive provider tip lookup failed before committed batch {batch_number}: {archive_tip_err}; \
                         live provider fallback also failed"
                    )
                });
        }
    };

    let archive_proxy = ZkChain::new(*live_proxy.address(), archive_provider.clone());
    match fetch_batch(&archive_proxy, batch_number, max_l1_blocks_to_scan).await {
        Ok(batch) => {
            let live_tip_after_fetch = live_proxy.provider().get_block_number().await.with_context(|| {
                format!("failed to fetch live provider tip after archive batch {batch_number} lookup")
            })?;
            let archive_tip_after_fetch =
                archive_provider.get_block_number().await.with_context(|| {
                    format!(
                        "failed to fetch archive provider tip after batch {batch_number} lookup"
                    )
                })?;
            if archive_tip_after_fetch < live_tip_after_fetch {
                tracing::warn!(
                    batch_number,
                    live_tip,
                    archive_tip,
                    live_tip_after_fetch,
                    archive_tip_after_fetch,
                    "archive provider is behind live provider; retrying committed batch lookup on live provider",
                );
                return fetch_batch(live_proxy, batch_number, max_l1_blocks_to_scan)
                    .await
                    .with_context(|| {
                        format!(
                            "archive provider is behind live provider for committed batch {batch_number} \
                             (archive tip {archive_tip_after_fetch}, live tip {live_tip_after_fetch}); live provider fallback also failed"
                        )
                    });
            }

            match validate_archive_batch_against_live(live_proxy, &batch).await {
                Ok(()) => {
                    tracing::warn!(
                        batch_number,
                        live_tip = live_tip_after_fetch,
                        archive_tip = archive_tip_after_fetch,
                        "archive committed batch hash matches live state",
                    );
                    Ok(batch)
                }
                Err(validation_err) => {
                    let validation_err = format!("{validation_err:#}");
                    tracing::warn!(
                        batch_number,
                        live_tip = live_tip_after_fetch,
                        archive_tip = archive_tip_after_fetch,
                        validation_error = validation_err,
                        "archive batch metadata could not be validated against live state; retrying live provider",
                    );
                    fetch_batch(live_proxy, batch_number, max_l1_blocks_to_scan)
                        .await
                        .with_context(|| {
                            format!(
                                "archive committed batch {batch_number} failed live hash validation \
                                 (archive tip {archive_tip_after_fetch}, live tip {live_tip_after_fetch}): {validation_err}; \
                                 live provider fallback also failed"
                            )
                        })
                }
            }
        }
        Err(archive_err) => {
            let archive_err = format!("{archive_err:#}");
            tracing::warn!(
                batch_number,
                live_tip,
                archive_tip,
                archive_error = archive_err,
                "archive provider failed to fetch committed batch; retrying live provider",
            );
            fetch_batch(live_proxy, batch_number, max_l1_blocks_to_scan)
                .await
                .with_context(|| {
                    format!(
                        "archive provider failed to fetch committed batch {batch_number}: {archive_err}; \
                         live provider fallback also failed"
                    )
                })
        }
    }
}

// SYSCOIN: A behind archive can still serve valid historical commit calldata.
// Validate the decoded batch against live latest state before accepting it.
async fn validate_archive_batch_against_live(
    live_proxy: &ZkChain<NodeProvider>,
    batch: &DiscoveredCommittedBatch,
) -> anyhow::Result<()> {
    let live_batch_hash = live_proxy
        .stored_batch_hash(batch.number())
        .await
        .with_context(|| {
            format!(
                "failed to fetch live stored batch hash for batch {}",
                batch.number()
            )
        })?;
    let archive_batch_hash = batch.batch_info.hash();
    anyhow::ensure!(
        live_batch_hash == archive_batch_hash,
        "archive batch hash {archive_batch_hash} does not match live stored batch hash {live_batch_hash}",
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{load_persisted_batch, startup_batch_numbers};
    use alloy::primitives::B256;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use zksync_os_batch_types::DiscoveredCommittedBatch;
    use zksync_os_contract_interface::models::StoredBatchInfo;
    use zksync_os_storage_api::{PersistedBatch, ReadBatch};

    #[test]
    fn prioritizes_frontier_batches_once() {
        assert_eq!(startup_batch_numbers(10, 8, 8, 8), (vec![8, 10], vec![9]));
    }

    #[test]
    fn excludes_prioritized_batches_from_remaining_range() {
        assert_eq!(
            startup_batch_numbers(10, 8, 6, 4),
            (vec![4, 6, 8, 10], vec![5, 7, 9])
        );
    }

    #[test]
    fn loads_committed_batch_from_persisted_storage() {
        let storage = MockBatchStorage::default();
        let committed_batch = discovered_batch(7, 70, 79);
        storage.insert(PersistedBatch {
            committed_batch: committed_batch.clone(),
            execute_sl_block_number: Some(100),
        });

        assert_eq!(
            load_persisted_batch(&storage, 7).unwrap(),
            Some(committed_batch)
        );
        assert!(load_persisted_batch(&storage, 8).unwrap().is_none());
    }

    #[derive(Clone, Default)]
    struct MockBatchStorage {
        batches: Arc<Mutex<HashMap<u64, PersistedBatch>>>,
    }

    impl MockBatchStorage {
        fn insert(&self, batch: PersistedBatch) {
            self.batches.lock().unwrap().insert(batch.number(), batch);
        }
    }

    impl ReadBatch for MockBatchStorage {
        fn get_batch_by_block_number(
            &self,
            block_number: u64,
        ) -> anyhow::Result<Option<PersistedBatch>> {
            Ok(self
                .batches
                .lock()
                .unwrap()
                .values()
                .find(|batch| batch.block_range.contains(&block_number))
                .cloned())
        }

        fn get_batch_by_number(&self, batch_number: u64) -> anyhow::Result<Option<PersistedBatch>> {
            Ok(self.batches.lock().unwrap().get(&batch_number).cloned())
        }

        fn latest_batch(&self) -> u64 {
            self.batches
                .lock()
                .unwrap()
                .keys()
                .max()
                .copied()
                .unwrap_or_default()
        }
    }

    fn discovered_batch(
        batch_number: u64,
        first_block: u64,
        last_block: u64,
    ) -> DiscoveredCommittedBatch {
        DiscoveredCommittedBatch {
            batch_info: StoredBatchInfo {
                batch_number,
                state_commitment: B256::with_last_byte(1),
                number_of_layer1_txs: 0,
                priority_operations_hash: B256::with_last_byte(2),
                dependency_roots_rolling_hash: B256::with_last_byte(3),
                l2_to_l1_logs_root_hash: B256::with_last_byte(4),
                commitment: B256::with_last_byte(5),
                last_block_timestamp: Some(0),
            },
            block_range: first_block..=last_block,
        }
    }
}
