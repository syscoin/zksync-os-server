//! Settlement-layer `MessageRoot` event ingestion for interop-root system transactions.
//!
//! Each `NewInteropRoot` carries a shared root that chains must import before they can verify
//! cross-chain proofs against it. SYSCOIN: The active settlement layer is the root source: edge chains read
//! Gateway's `L2MessageRoot`, while a V32 chain currently settling on L1 reads L1 `MessageRoot`.
//! This watcher resumes near the persisted interop cursor, drops roots that were already imported,
//! and forwards new roots to the mempool sink.

use alloy::primitives::ruint::FromUintError;
use alloy::rpc::types::{Log, Topic};
use alloy::sol_types::SolEvent;
use anyhow::Context;
use std::collections::HashMap;
use zksync_os_contract_interface::Bridgehub;
use zksync_os_contract_interface::IMessageRoot::NewInteropRoot;
use zksync_os_contract_interface::InteropRoot;
use zksync_os_provider::NodeProvider;
use zksync_os_types::IndexedInteropRoot;

use crate::util::find_l1_block_by_interop_root_id;
use crate::watcher::{L1WatcherError, StartResolver};
use crate::{EventSink, L1WatcherConfig, ProcessRawEvents};

/// Decodes confirmed `NewInteropRoot` logs for the shared [`L1Watcher`](crate::L1Watcher).
pub struct InteropWatcher {
    starting_interop_root_id: u64,
    sink: Box<dyn EventSink<IndexedInteropRoot>>,
}

impl InteropWatcher {
    /// SYSCOIN: Creates a resolver for the currently active settlement layer.
    ///
    /// `interop_root_id` is local to one `MessageRoot` contract, so this supports only a fresh/static
    /// settlement-layer identity. Live settlement-layer migration remains unsupported until the
    /// persisted cursor source is namespaced or reset for the new contract.
    pub async fn create_watcher(
        config: L1WatcherConfig,
        active_bridgehub: Bridgehub<NodeProvider>,
        active_sl_chain_id: u64,
        sink: impl EventSink<IndexedInteropRoot>,
    ) -> anyhow::Result<StartResolver<u64, Self>> {
        let message_root = active_bridgehub
            .message_root_address()
            .await
            .context("failed to fetch active settlement-layer MessageRoot address")?;
        let provider = active_bridgehub.provider().clone();

        let resolve_start = move |starting_interop_root_id: u64| async move {
            let start_block = find_l1_block_by_interop_root_id(
                active_bridgehub.clone(),
                starting_interop_root_id,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to resolve interop_root_id={starting_interop_root_id} on the active settlement layer"
                )
            })?;
            let processor = Self {
                starting_interop_root_id,
                sink: Box::new(sink),
            };
            Ok((start_block, processor))
        };

        // SYSCOIN: Validate the active settlement-layer provider (Gateway for an edge chain) against
        // the chain ID discovered at startup instead of assuming that the configured client matches.
        StartResolver::new(
            config,
            provider,
            message_root.into(),
            None,
            active_sl_chain_id,
            resolve_start,
        )
        .await
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::network::EthereumWallet;
    use alloy::primitives::{Bytes, U64, address};
    use alloy::providers::ProviderBuilder;
    use alloy::rpc::types::Header;
    use alloy::sol_types::SolValue;
    use alloy::transports::mock::Asserter;
    use std::time::Duration;

    const ACTIVE_SL_CHAIN_ID: u64 = 270;
    const L2_CHAIN_ID: u64 = 506;

    struct NoopSink;

    #[async_trait::async_trait]
    impl EventSink<IndexedInteropRoot> for NoopSink {
        async fn push(&mut self, _item: IndexedInteropRoot) {}
    }

    fn header_with_number(number: u64) -> Header<alloy::consensus::Header> {
        let mut header: Header<alloy::consensus::Header> = Header::default();
        header.inner.number = number;
        header
    }

    async fn mocked_node_provider(asserter: &Asserter) -> NodeProvider {
        // NodeProvider capability probes: latest header and finalized header.
        asserter.push_success(&header_with_number(1));
        asserter.push_success(&header_with_number(1));
        let provider = ProviderBuilder::new()
            .disable_recommended_fillers()
            .wallet(EthereumWallet::default())
            .connect_mocked_client(asserter.clone());
        NodeProvider::new(provider)
            .await
            .expect("mocked provider construction should succeed")
    }

    fn config() -> L1WatcherConfig {
        L1WatcherConfig {
            max_blocks_to_process: 100,
            confirmations: 2,
            poll_interval: Duration::from_millis(10),
            finalized_poll_interval: Duration::from_millis(10),
            logs_cache_capacity: 0,
        }
    }

    // SYSCOIN: The active interop source must be bound to the settlement-layer chain discovered
    // during startup, including when that source is Gateway rather than L1.
    #[tokio::test]
    async fn validates_active_settlement_layer_provider_chain_id() {
        let asserter = Asserter::new();
        let provider = mocked_node_provider(&asserter).await;
        let message_root = address!("0x0000000000000000000000000000000000001009");
        asserter.push_success(&Bytes::from(message_root.abi_encode()));
        asserter.push_success(&U64::from(ACTIVE_SL_CHAIN_ID + 1));

        let bridgehub = Bridgehub::new(
            address!("0x0000000000000000000000000000000000001002"),
            provider,
            L2_CHAIN_ID,
        );
        let err = InteropWatcher::create_watcher(config(), bridgehub, ACTIVE_SL_CHAIN_ID, NoopSink)
            .await
            .err()
            .expect("mismatched active settlement-layer provider must be rejected");

        assert!(
            err.to_string().contains("provider chain ID mismatch"),
            "unexpected error: {err:#}"
        );
        assert!(asserter.read_q().is_empty(), "all responses consumed");
    }

    #[tokio::test]
    async fn creates_confirmed_resolver_for_matching_active_provider() {
        let asserter = Asserter::new();
        let provider = mocked_node_provider(&asserter).await;
        let message_root = address!("0x0000000000000000000000000000000000001009");
        asserter.push_success(&Bytes::from(message_root.abi_encode()));
        asserter.push_success(&U64::from(ACTIVE_SL_CHAIN_ID));

        let bridgehub = Bridgehub::new(
            address!("0x0000000000000000000000000000000000001002"),
            provider,
            L2_CHAIN_ID,
        );
        let _resolver: StartResolver<u64, InteropWatcher> =
            InteropWatcher::create_watcher(config(), bridgehub, ACTIVE_SL_CHAIN_ID, NoopSink)
                .await
                .expect("matching active settlement-layer provider should construct a watcher");

        assert!(asserter.read_q().is_empty(), "all responses consumed");
    }
}
