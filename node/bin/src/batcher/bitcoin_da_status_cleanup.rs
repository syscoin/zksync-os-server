use crate::batcher::bitcoin_da_status_storage::BitcoinDaStatusStorage;
use async_trait::async_trait;
use tokio::sync::mpsc;
use zksync_os_batch_types::batcher_model::{FriProof, SignedBatchEnvelope};
use zksync_os_observability::ComponentStateReporter;
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent};

pub struct BitcoinDaStatusCleanup {
    storage: BitcoinDaStatusStorage,
}

impl BitcoinDaStatusCleanup {
    pub fn new(storage: BitcoinDaStatusStorage) -> Self {
        Self { storage }
    }
}

#[async_trait]
impl PipelineComponent for BitcoinDaStatusCleanup {
    type Input = SignedBatchEnvelope<FriProof>;
    type Output = SignedBatchEnvelope<FriProof>;

    const COMPONENT_ID: zksync_os_pipeline::ComponentId =
        zksync_os_pipeline::ComponentId::BitcoinDaStatusCleanup;
    const OUTPUT_CHANNEL_CAPACITY: usize = 5;

    async fn run(
        self,
        mut input: PeekableReceiver<Self::Input>,
        output: mpsc::Sender<Self::Output>,
        _state_reporter: ComponentStateReporter,
    ) -> anyhow::Result<()> {
        while let Some(batch) = input.recv().await {
            self.storage.delete(batch.batch_number()).await?;
            if output.send(batch).await.is_err() {
                tracing::info!("outbound channel closed");
                return Ok(());
            }
        }
        tracing::info!("inbound channel closed");
        Ok(())
    }
}
