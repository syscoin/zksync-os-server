use crate::committed_batch_provider::CommittedBatchProvider;
use crate::watcher::{L1WatcherError, StartResolver};
use crate::{L1WatcherConfig, ProcessRawEvents, util};
use alloy::rpc::types::{Log, Topic};
use alloy::sol_types::SolEvent;
use tokio::sync::watch;
use zksync_os_batch_types::DiscoveredCommittedBatch;
use zksync_os_contract_interface::IExecutor::{BlocksRevert, ReportCommittedBatchRangeZKsyncOS};
use zksync_os_contract_interface::ZkChain;
use zksync_os_provider::NodeProvider;
use zksync_os_storage_api::WriteFinality;

/// Watches settlement-layer commit events and advances the committed finality frontier.
///
/// This component reads `ReportCommittedBatchRangeZKsyncOS` events, resolves the committed batch
/// payload from L1 calldata, updates `WriteFinality`, and inserts the discovered batch into
/// `CommittedBatchProvider`. Live `BlocksRevert` events stop ingestion before a fetched range
/// can publish any commits, so restart can rebuild the frontier from settlement state.
///
/// Depended on by:
/// - `L1ExecuteWatcher`, which waits on the committed batches this watcher publishes;
/// - `Batcher` and `PriorityTreeManager`, which consume the same committed batch data during
///   startup replay and live operation;
/// - node startup / recovery logic, which relies on the committed frontier stored in finality.
pub struct L1CommitWatcher<Finality> {
    next_batch_number: u64,
    // SL tip used for finality initialization. Used to identify historical events during catch-up.
    sl_block_initial_finality_init_at: u64,
    // Last committed batch as of startup. Historical commits above this value are stale.
    startup_last_committed_batch: u64,
    committed_batch_provider: CommittedBatchProvider,
    finality: Finality,
    commit_submitted_rx: Option<watch::Receiver<u64>>,
}

impl<Finality: WriteFinality> L1CommitWatcher<Finality> {
    #[allow(clippy::too_many_arguments)]
    pub async fn create_watcher(
        config: L1WatcherConfig,
        zk_chain: ZkChain<NodeProvider>,
        archive_lookup_zk_chain: Option<ZkChain<NodeProvider>>,
        committed_batch_provider: CommittedBatchProvider,
        finality: Finality,
        sl_block_initial_finality_init_at: u64,
        sl_chain_id: u64,
        commit_submitted_rx: Option<watch::Receiver<u64>>,
    ) -> anyhow::Result<StartResolver<(), Self>> {
        tracing::info!(
            sl_block_initial_finality_init_at,
            config.max_blocks_to_process,
            ?config.poll_interval,
            zk_chain_address = ?zk_chain.address(),
            "initializing L1 commit watcher"
        );

        let provider = zk_chain.provider().clone();
        let address = (*zk_chain.address()).into();
        let max_blocks_to_process = config.max_blocks_to_process;

        let resolve_start = move |()| async move {
            let last_committed_batch = finality.get_finality_status().last_committed_batch;
            // SYSCOIN: Resolve the startup cursor through the archive-capable
            // provider while keeping live polling on `zk_chain`.
            let last_l1_block = util::find_startup_block_with_archive_fallback(
                zk_chain,
                archive_lookup_zk_chain,
                "commit watcher",
                |zk_chain| {
                    util::find_l1_commit_block_by_batch_number(
                        zk_chain,
                        last_committed_batch,
                        max_blocks_to_process,
                    )
                },
            )
            .await?;
            tracing::info!(last_committed_batch, last_l1_block, "resolved on L1");

            let processor = Self {
                next_batch_number: last_committed_batch
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("committed batch cursor overflow"))?,
                sl_block_initial_finality_init_at,
                startup_last_committed_batch: last_committed_batch,
                committed_batch_provider,
                finality,
                commit_submitted_rx,
            };
            // Discovery can find a replacement commit after a revert during startup. Include
            // every block after the snapshot so that such a revert cannot be skipped.
            Ok((
                commit_scan_start(last_l1_block, sl_block_initial_finality_init_at)?,
                processor,
            ))
        };
        // SYSCOIN: this watcher follows the active settlement layer, so validate against the
        // SL provider chain ID and preserve the configured confirmations.
        StartResolver::new(config, provider, address, None, sl_chain_id, resolve_start).await
    }

    async fn process_commit(
        &mut self,
        provider: &NodeProvider,
        report: ReportCommittedBatchRangeZKsyncOS,
        log: Log,
    ) -> Result<(), L1WatcherError> {
        let batch_number = report.batchNumber;
        // Startup-only guard: skip historical commits that are above the startup committed frontier.
        // This handles batches that were committed and reverted before the node started.
        if should_skip_historical_commit(
            self.sl_block_initial_finality_init_at,
            self.startup_last_committed_batch,
            batch_number,
            log.block_number,
        ) {
            tracing::warn!(
                batch_number,
                log_block_number = ?log.block_number,
                sl_block_initial_finality_init_at = self.sl_block_initial_finality_init_at,
                startup_last_committed_batch = self.startup_last_committed_batch,
                "skipping historical committed batch above startup frontier; likely reverted before startup",
            );
        } else if batch_number < self.next_batch_number {
            tracing::debug!(batch_number, "skipping already processed committed batch");
        } else {
            // Fast-fail if this batch was committed by a prior crashed session's pending tx.
            if should_restart_for_unexpected_commit(batch_number, self.commit_submitted_rx.as_ref())
            {
                return Err(L1WatcherError::UnexpectedCommit(batch_number));
            }

            tracing::debug!(batch_number, "discovered committed batch");
            let tx_hash = log.transaction_hash.expect("indexed log without tx hash");
            let l1_block_number = log.block_number.expect("indexed log without block number");
            let zk_chain = ZkChain::new(log.address(), provider.clone());
            let batch_info =
                util::fetch_committed_batch_data(&zk_chain, tx_hash, l1_block_number, batch_number)
                    .await?
                    .into_stored();
            let committed_batch = DiscoveredCommittedBatch {
                batch_info,
                block_range: report.firstBlockNumber..=report.lastBlockNumber,
            };

            self.publish_committed_batch(committed_batch)?;
        }
        Ok(())
    }

    fn publish_committed_batch(
        &mut self,
        committed_batch: DiscoveredCommittedBatch,
    ) -> Result<(), L1WatcherError> {
        let batch_number = committed_batch.number();
        let next_batch_number =
            batch_number
                .checked_add(1)
                .ok_or(L1WatcherError::InvalidLogRange(
                    "committed batch cursor overflow",
                ))?;
        let last_committed_block = committed_batch.last_block_number();
        self.finality.update_finality_status(|finality| {
            assert!(
                batch_number > finality.last_committed_batch,
                "non-monotonous committed batch"
            );
            assert!(
                last_committed_block > finality.last_committed_block,
                "non-monotonous committed block"
            );
            finality.last_committed_batch = batch_number;
            finality.last_committed_block = last_committed_block;
        });
        self.committed_batch_provider.insert(committed_batch);
        // A later processor error retries the range, including already published commits.
        self.next_batch_number = next_batch_number;
        Ok(())
    }

    fn validate_revert(&self, log: &Log) -> Result<(), L1WatcherError> {
        let revert = BlocksRevert::decode_log(&log.inner)?.data;
        let block_number = log.block_number.ok_or(L1WatcherError::InvalidLogRange(
            "revert log is missing its block number",
        ))?;
        if block_number > self.sl_block_initial_finality_init_at {
            let total_batches_committed = revert
                .totalBatchesCommitted
                .try_into()
                .map_err(|_| L1WatcherError::InvalidLogRange("reverted batch count exceeds u64"))?;
            return Err(L1WatcherError::L1Reverted(total_batches_committed));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl<Finality: WriteFinality> ProcessRawEvents for L1CommitWatcher<Finality> {
    fn name(&self) -> &'static str {
        "block_commit"
    }

    fn event_signatures(&self) -> Topic {
        Topic::default()
            .extend(ReportCommittedBatchRangeZKsyncOS::SIGNATURE_HASH)
            .extend(BlocksRevert::SIGNATURE_HASH)
    }

    fn filter_events(&self, logs: Vec<Log>) -> Vec<Log> {
        logs
    }

    fn validate_events(&self, logs: &[Log]) -> Result<(), L1WatcherError> {
        // A revert can invalidate an earlier commit in the same block or fetched range.
        // Reject it before publishing any commit, regardless of task scheduling or log order.
        for log in logs {
            if log.topic0() == Some(&BlocksRevert::SIGNATURE_HASH) {
                self.validate_revert(log)?;
            }
        }
        Ok(())
    }

    async fn process_raw_event(
        &mut self,
        provider: &NodeProvider,
        log: Log,
    ) -> Result<(), L1WatcherError> {
        match log.topic0() {
            Some(signature) if *signature == ReportCommittedBatchRangeZKsyncOS::SIGNATURE_HASH => {
                let report = ReportCommittedBatchRangeZKsyncOS::decode_log(&log.inner)?.data;
                self.process_commit(provider, report, log).await
            }
            Some(signature) if *signature == BlocksRevert::SIGNATURE_HASH => {
                self.validate_revert(&log)
            }
            _ => Err(L1WatcherError::InvalidLogRange(
                "unexpected commit watcher event topic",
            )),
        }
    }
}

fn commit_scan_start(discovered_commit_block: u64, startup_sl_block: u64) -> anyhow::Result<u64> {
    let first_live_block = startup_sl_block
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("startup settlement block cursor overflow"))?;
    Ok(discovered_commit_block.min(first_live_block))
}

/// Returns true if the commit event is for a batch that this session's pipeline has not yet
/// submitted to L1 — indicating a pending tx from a prior crashed session just landed.
fn should_restart_for_unexpected_commit(
    batch_number: u64,
    commit_submitted_rx: Option<&watch::Receiver<u64>>,
) -> bool {
    commit_submitted_rx.is_some_and(|rx| batch_number > *rx.borrow())
}

/// Returns true if the commit event belongs to startup catch-up range and is above the startup
/// committed frontier.
fn should_skip_historical_commit(
    sl_block_initial_finality_init_at: u64,
    startup_last_committed_batch: u64,
    batch_number: u64,
    log_block_number: Option<u64>,
) -> bool {
    log_block_number.is_some_and(|log_block_number| {
        log_block_number <= sl_block_initial_finality_init_at
            && batch_number > startup_last_committed_batch
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{Address, B256, U256};
    use alloy::transports::mock::Asserter;
    use zksync_os_contract_interface::models::StoredBatchInfo;
    use zksync_os_storage_api::{FinalityStatus, ReadFinality};

    struct TestFinality(watch::Sender<FinalityStatus>);

    impl ReadFinality for TestFinality {
        fn get_finality_status(&self) -> FinalityStatus {
            self.0.borrow().clone()
        }

        fn subscribe(&self) -> watch::Receiver<FinalityStatus> {
            self.0.subscribe()
        }
    }

    impl WriteFinality for TestFinality {
        fn update_finality_status(&self, f: impl FnOnce(&mut FinalityStatus)) {
            self.0.send_modify(f);
        }
    }

    async fn processor(asserter: &Asserter) -> (L1CommitWatcher<TestFinality>, NodeProvider) {
        let provider = crate::watcher::tests::mock_provider(asserter, true).await;
        let (finality, _) = watch::channel(FinalityStatus {
            last_committed_batch: 10,
            last_committed_block: 99,
            last_executed_batch: 0,
            last_executed_block: 0,
            last_finalized_executed_batch: 0,
            last_finalized_executed_block: 0,
        });
        (
            L1CommitWatcher {
                next_batch_number: 11,
                sl_block_initial_finality_init_at: 100,
                startup_last_committed_batch: 10,
                committed_batch_provider: CommittedBatchProvider::for_test(ZkChain::new(
                    Address::ZERO,
                    provider.clone(),
                )),
                finality: TestFinality(finality),
                commit_submitted_rx: None,
            },
            provider,
        )
    }

    fn event_log(event: impl SolEvent, block_number: u64) -> Log {
        Log {
            inner: alloy::primitives::Log {
                address: Address::ZERO,
                data: event.encode_log_data(),
            },
            block_number: Some(block_number),
            ..Log::default()
        }
    }

    fn commit_log(batch_number: u64, block_number: u64) -> Log {
        event_log(
            ReportCommittedBatchRangeZKsyncOS {
                batchNumber: batch_number,
                firstBlockNumber: 100,
                lastBlockNumber: 109,
            },
            block_number,
        )
    }

    fn revert_log(block_number: u64) -> Log {
        event_log(
            BlocksRevert {
                totalBatchesCommitted: U256::from(10),
                totalBatchesVerified: U256::ZERO,
                totalBatchesExecuted: U256::ZERO,
            },
            block_number,
        )
    }

    #[test]
    fn startup_cursor_includes_reverts_before_a_replacement_commit() {
        assert_eq!(commit_scan_start(90, 100).unwrap(), 90);
        assert_eq!(commit_scan_start(100, 100).unwrap(), 100);
        assert_eq!(commit_scan_start(101, 100).unwrap(), 101);
        assert_eq!(commit_scan_start(105, 100).unwrap(), 101);
        assert!(commit_scan_start(0, u64::MAX).is_err());
    }

    #[tokio::test]
    async fn live_revert_rejects_commit_revert_recommit_ranges() {
        let asserter = Asserter::new();
        let (processor, _) = processor(&asserter).await;
        for (revert_block, recommit_block) in [(101, 101), (102, 102)] {
            let logs = vec![
                commit_log(11, 101),
                revert_log(revert_block),
                commit_log(11, recommit_block),
            ];
            assert!(matches!(
                processor.validate_events(&logs),
                Err(L1WatcherError::L1Reverted(10))
            ));
        }
        assert_eq!(
            processor
                .finality
                .get_finality_status()
                .last_committed_batch,
            10
        );
        assert!(processor.committed_batch_provider.get(11).is_none());
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test]
    async fn startup_reverts_and_stale_commits_do_not_change_finality() {
        let asserter = Asserter::new();
        let (mut processor, provider) = processor(&asserter).await;
        for block_number in [99, 100] {
            let logs = vec![
                commit_log(11, block_number),
                revert_log(block_number),
                commit_log(10, block_number),
            ];
            processor.validate_events(&logs).unwrap();
            for log in logs {
                processor.process_raw_event(&provider, log).await.unwrap();
            }
        }
        processor.validate_events(&[commit_log(11, 101)]).unwrap();
        assert_eq!(processor.next_batch_number, 11);
        assert_eq!(
            processor
                .finality
                .get_finality_status()
                .last_committed_batch,
            10
        );
        assert!(processor.committed_batch_provider.get(11).is_none());
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test]
    async fn revert_requires_block_metadata_and_a_representable_batch_count() {
        let asserter = Asserter::new();
        let (processor, _) = processor(&asserter).await;
        let mut missing_block = revert_log(101);
        missing_block.block_number = None;
        let overflowing_batch = event_log(
            BlocksRevert {
                totalBatchesCommitted: U256::MAX,
                totalBatchesVerified: U256::ZERO,
                totalBatchesExecuted: U256::ZERO,
            },
            101,
        );
        for log in [missing_block, overflowing_batch] {
            assert!(matches!(
                processor.validate_events(&[log]),
                Err(L1WatcherError::InvalidLogRange(_))
            ));
        }
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test]
    async fn published_commit_is_not_republished_when_a_range_retries() {
        let asserter = Asserter::new();
        let (mut processor, provider) = processor(&asserter).await;
        let mut finality_rx = processor.finality.subscribe();
        let batch = DiscoveredCommittedBatch {
            batch_info: StoredBatchInfo {
                batch_number: 11,
                state_commitment: B256::ZERO,
                number_of_layer1_txs: 0,
                priority_operations_hash: B256::ZERO,
                dependency_roots_rolling_hash: B256::ZERO,
                l2_to_l1_logs_root_hash: B256::ZERO,
                commitment: B256::ZERO,
                last_block_timestamp: Some(0),
            },
            block_range: 100..=109,
        };
        processor.publish_committed_batch(batch.clone()).unwrap();
        assert_eq!(processor.next_batch_number, 12);
        assert_eq!(finality_rx.borrow_and_update().last_committed_batch, 11);
        processor
            .process_raw_event(&provider, commit_log(11, 101))
            .await
            .unwrap();
        assert!(!finality_rx.has_changed().unwrap());
        assert_eq!(processor.committed_batch_provider.get(11), Some(batch));
        assert!(asserter.read_q().is_empty());
    }

    #[test]
    fn skips_historical_batch_above_startup_frontier() {
        assert!(should_skip_historical_commit(100, 10, 11, Some(99)));
        assert!(should_skip_historical_commit(100, 10, 11, Some(100)));
    }

    #[test]
    fn does_not_skip_batch_after_startup_block() {
        assert!(!should_skip_historical_commit(100, 10, 11, Some(101)));
    }

    #[test]
    fn does_not_skip_batch_within_startup_committed_frontier() {
        assert!(!should_skip_historical_commit(100, 10, 10, Some(50)));
        assert!(!should_skip_historical_commit(100, 10, 9, Some(50)));
    }

    #[test]
    fn does_not_skip_when_log_has_no_block_number() {
        assert!(!should_skip_historical_commit(100, 10, 11, None));
    }

    #[test]
    fn restarts_when_batch_exceeds_submitted() {
        let (_tx, rx) = watch::channel(5u64);
        assert!(should_restart_for_unexpected_commit(6, Some(&rx)));
    }

    #[test]
    fn no_restart_when_batch_equals_submitted() {
        let (_tx, rx) = watch::channel(5u64);
        assert!(!should_restart_for_unexpected_commit(5, Some(&rx)));
    }

    #[test]
    fn no_restart_when_batch_below_submitted() {
        let (_tx, rx) = watch::channel(5u64);
        assert!(!should_restart_for_unexpected_commit(4, Some(&rx)));
    }

    #[test]
    fn no_restart_when_rx_is_none() {
        assert!(!should_restart_for_unexpected_commit(100, None));
    }

    #[test]
    fn no_restart_after_pipeline_updates_submitted() {
        let (tx, rx) = watch::channel(5u64);
        tx.send(6).unwrap();
        assert!(!should_restart_for_unexpected_commit(6, Some(&rx)));
    }
}
