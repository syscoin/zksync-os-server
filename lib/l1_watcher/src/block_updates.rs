use alloy::eips::BlockId;
use alloy::primitives::BlockNumber;
use alloy::providers::{DynProvider, Provider};
use reth_tasks::Runtime;
use std::time::Duration;
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockBoundary {
    Confirmed { confirmations: BlockNumber },
    Finalized,
}

/// Used to track changes & notify watchers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockUpdates {
    pub latest_block: BlockNumber,
    // SYSCOIN: some Syscoin/Gateway startup windows may not expose a finalized
    // block yet; finalized watchers should wait instead of crashing the poller.
    pub finalized_block: Option<BlockNumber>,
}

pub fn run(
    provider: DynProvider,
    runtime: &Runtime,
    task_name: &'static str,
    poll_interval: Duration,
    finalized_poll_interval: Duration,
) -> watch::Receiver<BlockUpdates> {
    let (l1_head, receiver) = watch::channel(BlockUpdates::default());
    runtime.spawn_critical_task(task_name, async move {
        let mut latest_timer = tokio::time::interval(poll_interval);
        let mut finalized_timer = tokio::time::interval(finalized_poll_interval);
        loop {
            if l1_head.receiver_count() == 0 {
                tracing::info!("block updates have no subscribers; stopping");
                return;
            }

            let result = tokio::select! {
                _ = latest_timer.tick() => poll_latest(&provider, &l1_head).await,
                _ = finalized_timer.tick() => poll_finalized(&provider, &l1_head).await,
            };
            if let Err(err) = result {
                // SYSCOIN: Preserve the old watcher behavior where transient provider
                // transport errors were retried instead of taking down all L1 watchers.
                tracing::warn!(?err, "block updates transport error; retrying on next poll");
            }
        }
    });
    receiver
}

async fn poll_latest(
    provider: &DynProvider,
    l1_head: &watch::Sender<BlockUpdates>,
) -> alloy::transports::TransportResult<()> {
    let latest_block = provider.get_block_number().await?;
    l1_head.send_if_modified(|current| {
        if current.latest_block == latest_block {
            false
        } else {
            current.latest_block = latest_block;
            true
        }
    });
    Ok(())
}

async fn poll_finalized(
    provider: &DynProvider,
    l1_head: &watch::Sender<BlockUpdates>,
) -> alloy::transports::TransportResult<()> {
    let Some(finalized_block) = provider
        .get_block_number_by_id(BlockId::finalized())
        .await?
    else {
        // SYSCOIN: some Syscoin/Gateway startup windows may not expose a finalized
        // block yet; finalized watchers should wait instead of crashing the poller.
        tracing::debug!("no finalized L1 block available yet");
        return Ok(());
    };
    l1_head.send_if_modified(|current| {
        if current.finalized_block == Some(finalized_block) {
            false
        } else {
            current.finalized_block = Some(finalized_block);
            true
        }
    });
    Ok(())
}

impl BlockUpdates {
    pub(crate) fn get_block_number(&self, boundary: BlockBoundary) -> Option<BlockNumber> {
        match boundary {
            BlockBoundary::Confirmed { confirmations } => {
                Some(self.latest_block.saturating_sub(confirmations))
            }
            BlockBoundary::Finalized => self.finalized_block,
        }
    }
}
