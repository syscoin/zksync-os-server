use crate::batcher::bitcoin_da_status_storage::BitcoinDaStatusStorage;
use async_trait::async_trait;
use tokio::sync::mpsc;
use zksync_os_l1_sender::batcher_model::{FriProof, SignedBatchEnvelope};
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

    const NAME: &'static str = "bitcoin_da_status_cleanup";
    const OUTPUT_BUFFER_SIZE: usize = 5;

    async fn run(
        self,
        mut input: PeekableReceiver<Self::Input>,
        output: mpsc::Sender<Self::Output>,
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
