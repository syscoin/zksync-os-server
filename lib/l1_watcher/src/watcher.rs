use crate::metrics::METRICS;
use crate::{L1WatcherConfig, ProcessRawEvents};
use alloy::primitives::{Address, B256, BlockNumber};
use alloy::providers::Provider;
use alloy::rpc::types::{Filter, Log, ValueOrArray};
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::time::Duration;
use zksync_os_provider::NodeProvider;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockBoundary {
    Confirmed { confirmations: BlockNumber },
    Finalized,
}

/// Boxed async closure that turns a starting point `S` into a concrete start block and the
/// processor `P` that consumes it.
type ResolveStartFn<S, P> =
    Box<dyn FnOnce(S) -> BoxFuture<'static, anyhow::Result<(BlockNumber, P)>> + Send + Sync>;

/// Resolves the confirmation depth for a confirmed-boundary watcher.
async fn resolve_confirmations(
    provider: &NodeProvider,
    expected_chain_id: u64,
    config: &L1WatcherConfig,
) -> anyhow::Result<BlockNumber> {
    // SYSCOIN: the confirmation lag must apply to the chain being watched. Callers
    // pass the expected provider chain ID so gateway/SL watchers keep the same reorg
    // protection instead of silently falling back to latest-block processing.
    let provider_chain_id = provider.get_chain_id().await?;
    anyhow::ensure!(
        provider_chain_id == expected_chain_id,
        "L1 watcher provider chain ID mismatch: expected {expected_chain_id}, got {provider_chain_id}"
    );
    Ok(config.confirmations)
}

/// Deferred constructor for an [`L1Watcher`]: holds the watcher's static dependencies and turns
/// a starting point `S` into a ready-to-run watcher once that starting point is finally known.
///
/// Constructing a resolver only requires static dependencies; the provider-dependent binary
/// search that turns a starting point (a priority id, batch number, protocol version, …) into a
/// concrete `next_block` — together with the processor `P` that consumes the resolved start
/// point — is deferred into the `resolve_start` closure and invoked by
/// [`resolve`](Self::resolve). This lets watchers be created in one place and started in
/// another, once the first replayed block is known.
pub struct StartResolver<S, P> {
    provider: NodeProvider,
    address: ValueOrArray<Address>,
    /// `Some(eb)` makes the watcher exit once the cursor passes `eb`. `None` runs forever.
    end_block: Option<BlockNumber>,
    max_blocks_to_process: u64,
    block_boundary: BlockBoundary,
    poll_interval: Duration,
    resolve_start: ResolveStartFn<S, P>,
}

impl<S, P: ProcessRawEvents> StartResolver<S, P> {
    pub(crate) async fn new<Fut>(
        config: L1WatcherConfig,
        provider: NodeProvider,
        address: ValueOrArray<Address>,
        end_block: Option<BlockNumber>,
        expected_chain_id: u64,
        resolve_start: impl FnOnce(S) -> Fut + Send + Sync + 'static,
    ) -> anyhow::Result<Self>
    where
        Fut: Future<Output = anyhow::Result<(BlockNumber, P)>> + Send + 'static,
    {
        let confirmations = resolve_confirmations(&provider, expected_chain_id, &config).await?;

        Ok(Self {
            provider,
            address,
            end_block,
            max_blocks_to_process: config.max_blocks_to_process,
            block_boundary: BlockBoundary::Confirmed { confirmations },
            poll_interval: config.poll_interval,
            resolve_start: Box::new(move |start| Box::pin(resolve_start(start))),
        })
    }

    /// Like [`new`](Self::new), but tails the finalized boundary so the produced watcher only
    /// reacts to irreversibly observed events.
    pub(crate) fn new_finalized<Fut>(
        config: L1WatcherConfig,
        provider: NodeProvider,
        address: ValueOrArray<Address>,
        end_block: Option<BlockNumber>,
        resolve_start: impl FnOnce(S) -> Fut + Send + Sync + 'static,
    ) -> anyhow::Result<Self>
    where
        Fut: Future<Output = anyhow::Result<(BlockNumber, P)>> + Send + 'static,
    {
        // SYSCOIN: unbounded finalized watchers must not be constructed when the provider
        // cannot actually query finalized/safe tags; otherwise startup would fail later in the
        // critical watcher task.
        anyhow::ensure!(
            end_block.is_some() || provider.supports_finalized_tag(),
            "provider lacks finalized/safe block tags; refusing to treat latest as finalized"
        );

        Ok(Self {
            provider,
            address,
            end_block,
            max_blocks_to_process: config.max_blocks_to_process,
            block_boundary: BlockBoundary::Finalized,
            poll_interval: config.poll_interval,
            resolve_start: Box::new(move |start| Box::pin(resolve_start(start))),
        })
    }

    /// Resolves the starting point into a concrete start block and processor, producing a
    /// ready-to-run [`L1Watcher`].
    pub async fn resolve(self, start: S) -> anyhow::Result<L1Watcher<P>> {
        let Self {
            provider,
            address,
            end_block,
            max_blocks_to_process,
            block_boundary,
            poll_interval,
            resolve_start,
        } = self;
        let (next_block, processor) = resolve_start(start).await?;
        Ok(L1Watcher {
            provider,
            address,
            next_block,
            end_block,
            max_blocks_to_process,
            block_boundary,
            poll_interval,
            processor,
            observed_range: None,
        })
    }

    /// Resolves the starting point and runs the produced watcher. A failure to resolve the
    /// start block is fatal (panics), matching the previous behavior where resolution happened
    /// at construction.
    pub async fn run(self, start: S) {
        self.resolve(start)
            .await
            .expect("failed to resolve L1 watcher start block")
            .run()
            .await;
    }
}

/// An abstract watcher for events.
/// Handles polling for new blocks and extracting logs,
/// while delegating the actual event processing to the processor `P`.
///
/// Produced by [`StartResolver::resolve`] once the starting point has been resolved into a
/// concrete `next_block` and processor. May be run unbounded (live tail) or bounded by
/// `end_block` (used by [`SlAwareL1Watcher`](crate::SlAwareL1Watcher) to scan a closed segment
/// to completion).
pub struct L1Watcher<P> {
    provider: NodeProvider,
    address: ValueOrArray<Address>,
    next_block: BlockNumber,
    /// `Some(eb)` makes the watcher exit `run` once `next_block > eb`. `None` runs forever.
    end_block: Option<BlockNumber>,
    max_blocks_to_process: u64,
    block_boundary: BlockBoundary,
    poll_interval: Duration,
    // This pins a range before its first side effect, including partially processed retries.
    // It is deliberately in-memory; restart safety still depends on finalized/trusted inputs.
    observed_range: Option<(BlockNumber, B256)>,
    pub(crate) processor: P,
}

impl<P: ProcessRawEvents> L1Watcher<P> {
    /// Builds a watcher for a single pre-resolved segment, tailing the finalized boundary
    /// (closed segments are dominated by `end_block`, so the boundary mode only matters for the
    /// open-ended segment).
    pub(crate) fn new_finalized(
        config: L1WatcherConfig,
        provider: NodeProvider,
        address: ValueOrArray<Address>,
        next_block: BlockNumber,
        end_block: Option<BlockNumber>,
        processor: P,
    ) -> Self {
        Self {
            provider,
            address,
            next_block,
            end_block,
            max_blocks_to_process: config.max_blocks_to_process,
            block_boundary: BlockBoundary::Finalized,
            poll_interval: config.poll_interval,
            processor,
            observed_range: None,
        }
    }

    /// Builds a watcher for a single pre-resolved segment, tailing the confirmed boundary
    /// (`latest - confirmations`). Unlike [`new_finalized`](Self::new_finalized), it reacts to an
    /// event within `confirmations` blocks instead of waiting out finality.
    pub(crate) async fn new_confirmed(
        config: L1WatcherConfig,
        provider: NodeProvider,
        address: ValueOrArray<Address>,
        next_block: BlockNumber,
        end_block: Option<BlockNumber>,
        expected_chain_id: u64,
        processor: P,
    ) -> anyhow::Result<Self> {
        let confirmations = resolve_confirmations(&provider, expected_chain_id, &config).await?;
        Ok(Self {
            provider,
            address,
            next_block,
            end_block,
            max_blocks_to_process: config.max_blocks_to_process,
            block_boundary: BlockBoundary::Confirmed { confirmations },
            poll_interval: config.poll_interval,
            processor,
            observed_range: None,
        })
    }

    /// Polls for new events.
    ///
    /// For unbounded watchers (`end_block = None`) this never returns; for bounded watchers
    /// it returns once the cursor passes `end_block`.
    pub async fn run(mut self) {
        self.run_inner().await;
    }

    /// Non-consuming version of `run`, intended for internal usage in this crate.
    pub(crate) async fn run_inner(&mut self) {
        // SYSCOIN: closed segments already have a pre-resolved cap, so do not initialize a
        // finalized/latest header watcher that can block startup before the segment is scanned.
        let mut headers = if self.end_block.is_none() {
            Some(match self.block_boundary {
                BlockBoundary::Confirmed { .. } => self.provider.latest_header_watcher().await,
                BlockBoundary::Finalized => match self.provider.finalized_header_watcher().await {
                    Ok(headers) => headers,
                    Err(err) => {
                        tracing::error!(
                            %err,
                            "failed to initialize finalized L1 watcher header subscription"
                        );
                        return;
                    }
                },
            })
        } else {
            None
        };

        loop {
            let cap = match self.end_block {
                // Closed segment: `end_block` was already resolved against a finalized/executed
                // batch, so the confirmation/finalization window doesn't apply and we don't need
                // an additional RPC.
                Some(end_block) => end_block,
                None => {
                    let number = headers
                        .as_mut()
                        .expect("unbounded watcher must have a header subscription")
                        .borrow_and_update()
                        .number;
                    match self.block_boundary {
                        BlockBoundary::Confirmed { confirmations } => {
                            number.saturating_sub(confirmations)
                        }
                        BlockBoundary::Finalized => number,
                    }
                }
            };

            match self.poll(cap).await {
                Ok(()) => {}
                // SYSCOIN: Treat transient settlement-layer transport failures as retryable.
                Err(L1WatcherError::Transport(err)) => {
                    tracing::warn!(?err, "watcher transport error; retrying on next poll");
                    // SYSCOIN: retry the same block range even if the chain is idle and the
                    // shared header watcher does not publish a new head.
                    tokio::time::sleep(self.poll_interval).await;
                    continue;
                }
                Err(err) => panic!("watcher failed: {err}"),
            }

            if let Some(end_block) = self.end_block
                && self.next_block > end_block
            {
                return;
            }

            let headers = headers
                .as_mut()
                .expect("unbounded watcher must have a header subscription");
            // A provider can replace consumed history without advancing its reported head.
            tokio::select! {
                result = headers.changed() => {
                    if let Err(e) = result {
                        panic!("l1 watcher header watcher closed unexpectedly: {e}");
                    }
                }
                _ = tokio::time::sleep(self.poll_interval) => {}
            }
        }
    }

    async fn poll(&mut self, cap: BlockNumber) -> Result<(), L1WatcherError> {
        self.check_observed_range().await?;
        if self.observed_range.is_some_and(|(number, _)| cap < number) {
            // A partial attempt may have published events above the temporarily regressed
            // boundary. Keep that full anchor until the boundary catches up again.
            return Ok(());
        }
        while self.next_block <= cap {
            let from_block = self.next_block;
            // Inspect up to `self.max_blocks_to_process` blocks at a time
            let to_block = cap.min(from_block + self.max_blocks_to_process - 1);
            let range_hash = self.canonical_hash(to_block).await?;

            let events = self
                .extract_logs_from_l1_blocks(from_block, to_block)
                .await?;

            self.validate_log_blocks(&events, from_block, to_block, range_hash)
                .await?;
            self.check_hash(to_block, range_hash).await?;
            self.check_observed_range().await?;

            let events = self.processor.filter_events(events);

            // Processors may publish some events before returning a retryable error. Retain
            // the authenticated anchor even on that path, without advancing the scan cursor.
            if self
                .observed_range
                .is_none_or(|(number, _)| to_block >= number)
            {
                self.observed_range = Some((to_block, range_hash));
            }

            METRICS.events_loaded[&self.processor.name()].inc_by(events.len() as u64);
            METRICS.most_recently_scanned_l1_block[&self.processor.name()].set(to_block);

            for event in events {
                self.processor
                    .process_raw_event(&self.provider, event)
                    .await?;
            }

            // All effects completed successfully. A transport failure in the final anchor
            // check must retry that check without delivering the completed range again.
            self.next_block = to_block + 1;
            self.check_observed_range().await?;
        }

        Ok(())
    }

    async fn canonical_hash(&self, number: BlockNumber) -> Result<B256, L1WatcherError> {
        let block = self
            .provider
            .get_block_by_number(number.into())
            .await?
            .ok_or(L1WatcherError::CanonicalBlockUnavailable(number))?;
        let header = block.header;
        if header.number != number {
            return Err(L1WatcherError::InvalidLogRange(
                "provider returned the wrong header number",
            ));
        }
        Ok(header.hash)
    }

    async fn check_hash(&self, number: BlockNumber, expected: B256) -> Result<(), L1WatcherError> {
        let actual = self.canonical_hash(number).await?;
        if actual != expected {
            return Err(L1WatcherError::CanonicalChainChanged {
                number,
                expected,
                actual,
            });
        }
        Ok(())
    }

    async fn check_observed_range(&self) -> Result<(), L1WatcherError> {
        if let Some((number, hash)) = self.observed_range {
            self.check_hash(number, hash).await?;
        }
        Ok(())
    }

    async fn validate_log_blocks(
        &self,
        events: &[Log],
        from: BlockNumber,
        to: BlockNumber,
        range_hash: B256,
    ) -> Result<(), L1WatcherError> {
        let mut hashes = HashMap::from([(to, range_hash)]);
        for event in events {
            let number = event.block_number.ok_or(L1WatcherError::InvalidLogRange(
                "log is missing its block number",
            ))?;
            if event.removed || number < from || number > to {
                return Err(L1WatcherError::InvalidLogRange(
                    "removed or out-of-range log in canonical response",
                ));
            }
            let expected = match hashes.get(&number) {
                Some(hash) => *hash,
                None => {
                    let hash = self.canonical_hash(number).await?;
                    hashes.insert(number, hash);
                    hash
                }
            };
            if event.block_hash != Some(expected) {
                return Err(L1WatcherError::InvalidLogRange(
                    "log block hash does not match canonical header",
                ));
            }
        }
        Ok(())
    }

    /// Processes a range of L1 blocks for new events.
    ///
    /// Returns a list of new events as extracted from the L1 blocks.
    async fn extract_logs_from_l1_blocks(
        &self,
        from: BlockNumber,
        to: BlockNumber,
    ) -> Result<Vec<Log>, L1WatcherError> {
        let mut filter = Filter::new()
            .from_block(from)
            .to_block(to)
            .event_signature(self.processor.event_signatures())
            .address(self.address.clone());
        if let Some(topic1) = self.processor.topic1_filter() {
            filter = filter.topic1(topic1);
        }
        // The shared numeric-range log cache can lag a reorg while its head poller catches
        // up. Security-sensitive ingestion must authenticate a fresh provider response.
        let new_logs = self.provider.root().get_logs(&filter).await?;

        if new_logs.is_empty() {
            tracing::trace!(
                event_name = self.processor.name(),
                l1_block_from = from,
                l1_block_to = to,
                "no new events"
            );
        } else {
            tracing::info!(
                event_name = self.processor.name(),
                event_count = new_logs.len(),
                l1_block_from = from,
                l1_block_to = to,
                "received new events"
            );
        }

        Ok(new_logs)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum L1WatcherError {
    #[error("canonical block {0} is unavailable while authenticating an L1 watcher range")]
    CanonicalBlockUnavailable(BlockNumber),
    #[error("canonical history changed at block {number}: expected {expected}, got {actual}")]
    CanonicalChainChanged {
        number: BlockNumber,
        expected: B256,
        actual: B256,
    },
    #[error("invalid L1 watcher response: {0}")]
    InvalidLogRange(&'static str),
    #[error("L1 does not have any blocks")]
    NoL1Blocks,
    #[error(transparent)]
    Sol(#[from] alloy::sol_types::Error),
    #[error(transparent)]
    Transport(#[from] alloy::transports::TransportError),
    #[error(transparent)]
    Batch(anyhow::Error),
    #[error(transparent)]
    Convert(anyhow::Error),
    #[error(transparent)]
    Contract(#[from] zksync_os_contract_interface::Error),
    #[error(transparent)]
    Other(anyhow::Error),
    #[error(
        "batch {0} was committed on L1 but not submitted by this session; likely a pending tx from a prior crash"
    )]
    UnexpectedCommit(u64),
    #[error(
        "L1 batches were reverted on the settlement layer (new committed batch count = {0}); restarting to re-sync from the main node"
    )]
    L1Reverted(u64),
    #[error("output has been closed")]
    OutputClosed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::network::EthereumWallet;
    use alloy::providers::ProviderBuilder;
    use alloy::rpc::json_rpc::ErrorPayload;
    use alloy::rpc::types::{Block, Topic};
    use alloy::transports::mock::Asserter;
    use std::borrow::Cow;
    use std::sync::{Arc, Mutex};

    struct RecordingProcessor {
        events: Arc<Mutex<Vec<Log>>>,
        fail_at: Option<usize>,
    }

    #[async_trait::async_trait]
    impl ProcessRawEvents for RecordingProcessor {
        fn name(&self) -> &'static str {
            "authenticated_test"
        }

        fn event_signatures(&self) -> Topic {
            B256::ZERO.into()
        }

        fn filter_events(&self, logs: Vec<Log>) -> Vec<Log> {
            logs
        }

        async fn process_raw_event(
            &mut self,
            _: &NodeProvider,
            event: Log,
        ) -> Result<(), L1WatcherError> {
            let mut events = self.events.lock().unwrap();
            if self.fail_at == Some(events.len()) {
                self.fail_at = None;
                return Err(alloy::transports::TransportErrorKind::custom_str(
                    "temporary processor RPC failure",
                )
                .into());
            }
            events.push(event);
            Ok(())
        }
    }

    fn header(number: u64, hash: B256) -> Block {
        let mut block: Block = Block::default();
        block.header.inner.number = number;
        block.header.hash = hash;
        block
    }

    fn log(number: u64, hash: B256) -> Log {
        Log {
            block_number: Some(number),
            block_hash: Some(hash),
            ..Log::default()
        }
    }

    async fn watcher(asserter: &Asserter) -> L1Watcher<RecordingProcessor> {
        watcher_with_header_support(asserter, true).await
    }

    async fn watcher_with_header_support(
        asserter: &Asserter,
        supports_header: bool,
    ) -> L1Watcher<RecordingProcessor> {
        if supports_header {
            asserter.push_success(&header(1, B256::repeat_byte(1)).header);
            asserter.push_success(&header(1, B256::repeat_byte(1)).header);
        } else {
            asserter.push_failure(ErrorPayload {
                code: -32601,
                message: Cow::Borrowed("method not found"),
                data: None,
            });
            asserter.push_success(&header(1, B256::repeat_byte(1)));
        }
        asserter.push_failure(ErrorPayload {
            code: -32601,
            message: Cow::Borrowed("method not found"),
            data: None,
        });
        asserter.push_success(&"anvil/test");
        let provider = NodeProvider::new(
            ProviderBuilder::new()
                .disable_recommended_fillers()
                .wallet(EthereumWallet::default())
                .connect_mocked_client(asserter.clone()),
        )
        .await
        .unwrap();
        L1Watcher::new_finalized(
            L1WatcherConfig {
                max_blocks_to_process: 100,
                confirmations: 2,
                poll_interval: Duration::from_millis(10),
                finalized_poll_interval: Duration::from_millis(10),
                logs_cache_capacity: 0,
            },
            provider,
            Address::ZERO.into(),
            10,
            None,
            RecordingProcessor {
                events: Arc::default(),
                fail_at: None,
            },
        )
    }

    fn successful_range(asserter: &Asserter, number: u64, hash: B256, events: Vec<Log>) {
        asserter.push_success(&header(number, hash));
        asserter.push_success(&events);
        asserter.push_success(&header(number, hash));
        asserter.push_success(&header(number, hash));
    }

    #[tokio::test]
    async fn authenticates_ranges_without_the_optional_header_rpc() {
        let asserter = Asserter::new();
        let mut watcher = watcher_with_header_support(&asserter, false).await;
        successful_range(&asserter, 10, B256::repeat_byte(10), vec![]);
        watcher.poll(10).await.unwrap();
        assert_eq!(watcher.next_block, 11);
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test]
    async fn pins_successfully_processed_and_empty_ranges() {
        for events in [vec![], vec![log(10, B256::repeat_byte(10))]] {
            let asserter = Asserter::new();
            let mut watcher = watcher(&asserter).await;
            successful_range(&asserter, 10, B256::repeat_byte(10), events.clone());
            watcher.poll(10).await.unwrap();
            assert_eq!(watcher.next_block, 11);
            assert_eq!(watcher.observed_range, Some((10, B256::repeat_byte(10))));
            assert_eq!(*watcher.processor.events.lock().unwrap(), events);
            assert!(asserter.read_q().is_empty());
        }
    }

    #[tokio::test]
    async fn detects_replaced_consumed_history_even_without_a_new_scan() {
        for cap in [9, 10, 11] {
            let asserter = Asserter::new();
            let mut watcher = watcher(&asserter).await;
            successful_range(&asserter, 10, B256::repeat_byte(10), vec![]);
            watcher.poll(10).await.unwrap();
            asserter.push_success(&header(10, B256::repeat_byte(99)));
            assert!(matches!(
                watcher.poll(cap).await,
                Err(L1WatcherError::CanonicalChainChanged { number: 10, .. })
            ));
            assert_eq!(watcher.next_block, 11);
            assert!(asserter.read_q().is_empty());
        }
    }

    #[tokio::test]
    async fn rejects_a_range_reorg_before_publishing_any_event() {
        let asserter = Asserter::new();
        let mut watcher = watcher(&asserter).await;
        let hash = B256::repeat_byte(10);
        asserter.push_success(&header(10, hash));
        asserter.push_success(&vec![log(10, hash)]);
        asserter.push_success(&header(10, B256::repeat_byte(99)));
        assert!(matches!(
            watcher.poll(10).await,
            Err(L1WatcherError::CanonicalChainChanged { .. })
        ));
        assert!(watcher.processor.events.lock().unwrap().is_empty());
        assert_eq!(watcher.next_block, 10);
        assert_eq!(watcher.observed_range, None);
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test]
    async fn rejects_unanchored_log_metadata_before_any_side_effect() {
        let hash = B256::repeat_byte(10);
        let mut removed = log(10, hash);
        removed.removed = true;
        let mut missing_hash = log(10, hash);
        missing_hash.block_hash = None;
        let mut missing_number = log(10, hash);
        missing_number.block_number = None;
        for event in [
            removed,
            missing_hash,
            missing_number,
            log(10, B256::repeat_byte(99)),
            log(9, hash),
            log(11, hash),
        ] {
            let asserter = Asserter::new();
            let mut watcher = watcher(&asserter).await;
            asserter.push_success(&header(10, hash));
            asserter.push_success(&vec![event]);
            assert!(matches!(
                watcher.poll(10).await,
                Err(L1WatcherError::InvalidLogRange(_))
            ));
            assert!(watcher.processor.events.lock().unwrap().is_empty());
            assert_eq!(watcher.next_block, 10);
            assert!(asserter.read_q().is_empty());
        }
    }

    #[tokio::test]
    async fn verifies_event_blocks_within_a_multiblock_range() {
        let asserter = Asserter::new();
        let mut watcher = watcher(&asserter).await;
        asserter.push_success(&header(11, B256::repeat_byte(11)));
        asserter.push_success(&vec![log(10, B256::repeat_byte(99))]);
        asserter.push_success(&header(10, B256::repeat_byte(10)));
        assert!(matches!(
            watcher.poll(11).await,
            Err(L1WatcherError::InvalidLogRange(_))
        ));
        assert!(watcher.processor.events.lock().unwrap().is_empty());
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test]
    async fn partial_processing_pins_the_range_across_a_retry() {
        let asserter = Asserter::new();
        let mut watcher = watcher(&asserter).await;
        watcher.processor.fail_at = Some(1);
        let hash = B256::repeat_byte(10);
        asserter.push_success(&header(10, hash));
        asserter.push_success(&vec![log(10, hash), log(10, hash)]);
        asserter.push_success(&header(10, hash));
        assert!(matches!(
            watcher.poll(10).await,
            Err(L1WatcherError::Transport(_))
        ));
        assert_eq!(watcher.processor.events.lock().unwrap().len(), 1);
        assert_eq!(watcher.next_block, 10);
        assert_eq!(watcher.observed_range, Some((10, hash)));

        asserter.push_success(&header(10, B256::repeat_byte(99)));
        assert!(matches!(
            watcher.poll(10).await,
            Err(L1WatcherError::CanonicalChainChanged { .. })
        ));
        assert_eq!(watcher.processor.events.lock().unwrap().len(), 1);
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test]
    async fn regressed_boundary_cannot_lower_a_partially_consumed_anchor() {
        let asserter = Asserter::new();
        let mut watcher = watcher(&asserter).await;
        watcher.processor.fail_at = Some(1);
        let hash = B256::repeat_byte(20);
        asserter.push_success(&header(20, hash));
        asserter.push_success(&vec![log(20, hash), log(20, hash)]);
        asserter.push_success(&header(20, hash));
        assert!(matches!(
            watcher.poll(20).await,
            Err(L1WatcherError::Transport(_))
        ));
        asserter.push_success(&header(20, hash));
        watcher.poll(15).await.unwrap();
        assert_eq!(watcher.observed_range, Some((20, hash)));
        assert_eq!(watcher.next_block, 10);
        assert_eq!(watcher.processor.events.lock().unwrap().len(), 1);

        asserter.push_success(&header(20, B256::repeat_byte(99)));
        assert!(matches!(
            watcher.poll(15).await,
            Err(L1WatcherError::CanonicalChainChanged { .. })
        ));
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test]
    async fn reorg_during_processing_stops_before_further_scan() {
        let asserter = Asserter::new();
        let mut watcher = watcher(&asserter).await;
        let hash = B256::repeat_byte(10);
        asserter.push_success(&header(10, hash));
        asserter.push_success(&vec![log(10, hash)]);
        asserter.push_success(&header(10, hash));
        asserter.push_success(&header(10, B256::repeat_byte(99)));
        assert!(matches!(
            watcher.poll(10).await,
            Err(L1WatcherError::CanonicalChainChanged { .. })
        ));
        assert_eq!(watcher.processor.events.lock().unwrap().len(), 1);
        assert_eq!(watcher.next_block, 11);
        assert_eq!(watcher.observed_range, Some((10, hash)));
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test]
    async fn post_processing_transport_retry_does_not_redeliver_events() {
        let asserter = Asserter::new();
        let mut watcher = watcher(&asserter).await;
        let hash = B256::repeat_byte(10);
        let events = vec![log(10, hash)];
        asserter.push_success(&header(10, hash));
        asserter.push_success(&events);
        asserter.push_success(&header(10, hash));
        asserter.push_failure(ErrorPayload {
            code: -32000,
            message: Cow::Borrowed("temporary upstream failure"),
            data: None,
        });
        assert!(matches!(
            watcher.poll(10).await,
            Err(L1WatcherError::Transport(_))
        ));
        assert_eq!(*watcher.processor.events.lock().unwrap(), events);
        assert_eq!(watcher.next_block, 11);
        assert_eq!(watcher.observed_range, Some((10, hash)));

        asserter.push_success(&header(10, hash));
        watcher.poll(10).await.unwrap();
        assert_eq!(*watcher.processor.events.lock().unwrap(), events);
        assert_eq!(watcher.next_block, 11);
        assert_eq!(watcher.observed_range, Some((10, hash)));
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test]
    async fn unavailable_checkpoint_stops_and_transport_failure_preserves_it() {
        let asserter = Asserter::new();
        let mut watcher = watcher(&asserter).await;
        let hash = B256::repeat_byte(10);
        successful_range(&asserter, 10, hash, vec![]);
        watcher.poll(10).await.unwrap();
        asserter.push_failure(ErrorPayload {
            code: -32000,
            message: Cow::Borrowed("temporary upstream failure"),
            data: None,
        });
        assert!(matches!(
            watcher.poll(11).await,
            Err(L1WatcherError::Transport(_))
        ));
        assert_eq!(watcher.next_block, 11);
        assert_eq!(watcher.observed_range, Some((10, hash)));

        asserter.push_success(&Option::<Block>::None);
        assert!(matches!(
            watcher.poll(11).await,
            Err(L1WatcherError::CanonicalBlockUnavailable(10))
        ));
        assert_eq!(watcher.next_block, 11);
        assert!(asserter.read_q().is_empty());
    }
}
