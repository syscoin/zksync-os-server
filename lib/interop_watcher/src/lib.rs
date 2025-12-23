use std::time::Duration;

use alloy::rpc::types::Filter;
use alloy::sol_types::SolEvent;
use alloy::{
    primitives::Address,
    providers::{DynProvider, Provider},
};
use tokio::sync::mpsc;
use zksync_os_contract_interface::IMessageRoot::AppendedChainRoot;
use zksync_os_contract_interface::{Bridgehub, InteropRoot};
use zksync_os_types::InteropRootsEnvelope;

pub const INTEROP_ROOTS_PER_IMPORT: u64 = 100;
const LOOKBEHIND_BLOCKS: u64 = 1000;

pub struct L1InteropRootsWatcher {
    contract_address: Address,

    provider: DynProvider,
    // first number is block number, second is log index
    next_log_to_scan_from: Option<(u64, u64)>,

    poll_interval: Duration,

    output: mpsc::Sender<InteropRootsEnvelope>,
}

impl L1InteropRootsWatcher {
    pub async fn new(
        bridgehub: Bridgehub<DynProvider>,
        poll_interval: Duration,
        output: mpsc::Sender<InteropRootsEnvelope>,
    ) -> anyhow::Result<Self> {
        let provider = bridgehub.provider().clone();
        let contract_address = bridgehub
            .message_root()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get message root: {}", e))?;

        Ok(Self {
            provider,
            contract_address,
            next_log_to_scan_from: None,
            poll_interval,
            output,
        })
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        let mut timer = tokio::time::interval(self.poll_interval);
        loop {
            timer.tick().await;
            self.poll().await?;
        }
    }

    async fn fetch_events(
        &mut self,
        from_block: u64,
        to_block: u64,
        start_log_index: u64,
    ) -> anyhow::Result<Vec<InteropRoot>> {
        let filter = Filter::new()
            .from_block(from_block)
            .to_block(to_block)
            .address(self.contract_address)
            .event_signature(AppendedChainRoot::SIGNATURE_HASH);
        let logs = self.provider.get_logs(&filter).await?;

        let mut interop_roots = Vec::new();
        for log in logs {
            let log_block_number = log.block_number.unwrap();
            let log_index_in_block = log.log_index.unwrap();

            if log_block_number == from_block && log_index_in_block <= start_log_index {
                continue;
            }
            let interop_root_event = AppendedChainRoot::decode_log(&log.inner)?.data;

            let interop_root = InteropRoot {
                chainId: interop_root_event.chainId,
                blockOrBatchNumber: interop_root_event.batchNumber,
                sides: vec![interop_root_event.chainRoot],
            };
            interop_roots.push(interop_root);

            self.next_log_to_scan_from = Some((log_block_number, log_index_in_block + 1));

            if interop_roots.len() >= INTEROP_ROOTS_PER_IMPORT as usize {
                break;
            }
        }

        // if we didn't get enough interop roots, it should be safe to continue from the last block we already scanned
        // edge case would be if the last root we included was already in the last block, then we should leave the value as is(it was updated before)
        if interop_roots.len() < INTEROP_ROOTS_PER_IMPORT as usize
            && self.next_log_to_scan_from.map(|(from_block, _)| from_block) < Some(to_block)
        {
            self.next_log_to_scan_from = Some((to_block, 0));
        }

        Ok(interop_roots)
    }

    async fn poll(&mut self) -> anyhow::Result<()> {
        let latest_block = self.provider.get_block_number().await?;

        if let Some((from_block, _)) = self.next_log_to_scan_from {
            if from_block + LOOKBEHIND_BLOCKS < latest_block {
                tracing::warn!(
                    from_block,
                    latest_block,
                    "From block is found to be behind the latest block by more than {}, it shouldn't happen normally",
                    LOOKBEHIND_BLOCKS
                );
            }
        }

        let (from_block, start_log_index) = match self.next_log_to_scan_from {
            Some((from_block, start_log_index)) => (from_block, start_log_index),
            None => (latest_block.saturating_sub(LOOKBEHIND_BLOCKS), 0),
        };

        let interop_roots = self
            .fetch_events(from_block, latest_block, start_log_index)
            .await?;

        // let interop_roots_envelope = InteropRootsEnvelope::from_interop_roots(interop_roots);
        // self.output.send(interop_roots_envelope).await?;

        // temporary implementation where we send each interop root separately
        for interop_root in interop_roots {
            let interop_root_envelope = InteropRootsEnvelope::from_interop_root(interop_root);
            self.output.send(interop_root_envelope).await?;
        }

        Ok(())
    }
}
