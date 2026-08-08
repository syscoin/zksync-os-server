//! L1 `MessageRoot` event ingestion for interop-root system transactions.
//!
//! Each `NewInteropRoot` carries a shared root that chains must import before they can verify
//! cross-chain proofs against it. This watcher resumes near the persisted interop cursor, drops
//! roots that were already imported, and forwards new roots to the mempool sink.

use alloy::primitives::ruint::FromUintError;
use alloy::rpc::types::{Log, Topic};
use alloy::sol_types::SolEvent;
use anyhow::Context;
use std::collections::HashMap;
use zksync_os_contract_interface::Bridgehub;
use zksync_os_contract_interface::IMessageRoot::NewInteropRoot;
use zksync_os_contract_interface::InteropRoot;
use zksync_os_contract_interface::l1_discovery::L2_BRIDGEHUB_ADDRESS;
use zksync_os_contract_interface::settlement_layer_intervals::{
    IntervalSettlementLayer, SettlementLayerIntervals,
};
use zksync_os_provider::NodeProvider;
use zksync_os_types::IndexedInteropRoot;

use crate::sl_aware_watcher::{SegmentResolver, SegmentSpec};
use crate::util::{find_l1_block_by_interop_root_id, find_l1_execute_block_by_batch_number};
use crate::watcher::L1WatcherError;
use crate::{EventSink, L1WatcherConfig, ProcessRawEvents};

/// Decodes confirmed `NewInteropRoot` logs for the shared [`L1Watcher`](crate::L1Watcher).
pub struct InteropWatcher {
    starting_interop_root_id: u64,
    sink: Box<dyn EventSink<IndexedInteropRoot>>,
}

impl InteropWatcher {
    /// Creates a settlement-layer-aware resolver. V31 Gateway intervals and V32+ direct-L1
    /// intervals use the same persisted cursor, so every historical segment is scanned in order.
    pub fn create_watcher(
        intervals: SettlementLayerIntervals,
        config: L1WatcherConfig,
        l1_bridgehub: Bridgehub<NodeProvider>,
        l2_chain_id: u64,
        sink: impl EventSink<IndexedInteropRoot>,
    ) -> SegmentResolver<u64, Self> {
        let resolve_segments = move |starting_interop_root_id: u64| async move {
            let mut segments = Vec::new();
            for interval in intervals.intervals() {
                if interval
                    .last_batch
                    .is_some_and(|last_batch| interval.first_batch > last_batch)
                {
                    continue;
                }

                let bridgehub = match interval.settlement_layer {
                    IntervalSettlementLayer::L1 => l1_bridgehub.clone(),
                    IntervalSettlementLayer::Gateway(_) => Bridgehub::new(
                        L2_BRIDGEHUB_ADDRESS,
                        interval.proxy.provider().clone(),
                        l2_chain_id,
                    ),
                };
                let message_root = bridgehub.message_root_address().await.with_context(|| {
                    format!("failed to fetch MessageRoot address for interval {interval}")
                })?;
                let start_block = find_l1_block_by_interop_root_id(
                    bridgehub.clone(),
                    starting_interop_root_id,
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to resolve interop_root_id={starting_interop_root_id} in interval {interval}"
                    )
                })?;
                let end_block = match interval.last_batch {
                    Some(last_batch) => Some(
                        find_l1_execute_block_by_batch_number(interval.proxy.clone(), last_batch)
                            .await
                            .with_context(|| {
                                format!(
                                    "failed to find settlement-layer execute block for batch #{last_batch} in interval {interval}"
                                )
                            })?,
                    ),
                    None => None,
                };
                segments.push(SegmentSpec {
                    provider: bridgehub.provider().clone(),
                    address: message_root.into(),
                    start_block,
                    end_block,
                });
            }

            let processor = Self {
                starting_interop_root_id,
                sink: Box::new(sink),
            };
            Ok((segments, processor))
        };

        SegmentResolver::new(config, resolve_segments)
    }
}

#[async_trait::async_trait]
impl ProcessRawEvents for InteropWatcher {
    fn name(&self) -> &'static str {
        "interop_root"
    }

    fn event_signatures(&self) -> Topic {
        NewInteropRoot::SIGNATURE_HASH.into()
    }

    fn filter_events(&self, logs: Vec<Log>) -> Vec<Log> {
        // A polling range may contain repeated updates for one log id. Only its latest root should
        // reach the subpool.
        let mut indexes = HashMap::new();

        for log in logs {
            let event = match NewInteropRoot::decode_log(&log.inner) {
                Ok(event) => event.data,
                Err(err) => {
                    tracing::error!(?log, error = ?err, "failed to decode interop root log");
                    continue;
                }
            };
            indexes.insert(event.logId, log);
        }

        indexes.into_values().collect()
    }

    async fn process_raw_event(
        &mut self,
        _provider: &NodeProvider,
        log: Log,
    ) -> Result<(), L1WatcherError> {
        let event = NewInteropRoot::decode_log(&log.inner)?.data;

        let log_id: u64 = event
            .logId
            .try_into()
            .map_err(|e: FromUintError<u64>| L1WatcherError::Other(e.into()))?;

        // Because startup rescans the block containing the cursor, only that first scanned L1 block
        // can contain roots that were already imported.
        if log_id < self.starting_interop_root_id {
            tracing::debug!(
                log_id,
                starting_interop_root_id = self.starting_interop_root_id,
                "skipping interop root event before starting id",
            );
            return Ok(());
        }
        let interop_root = InteropRoot {
            chainId: event.chainId,
            blockOrBatchNumber: event.blockNumber,
            sides: event.sides.clone(),
        };

        self.sink
            .push(IndexedInteropRoot {
                log_id,
                root: interop_root,
            })
            .await;
        Ok(())
    }
}
