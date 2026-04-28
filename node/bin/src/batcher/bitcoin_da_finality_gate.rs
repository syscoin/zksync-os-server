use crate::batcher::bitcoin_da_status_storage::{BitcoinDaFinalityPolicy, BitcoinDaStatusStorage};
use crate::config::{BatcherConfig, BitcoinDaFinalityMode};
use anyhow::Context;
use async_trait::async_trait;
use bitcoin_da_client::{BitcoinDaFinalityMode as ClientBitcoinDaFinalityMode, SyscoinClient};
use secrecy::ExposeSecret;
use tokio::sync::mpsc;
use tokio::time::Instant;
use zksync_os_contract_interface::models::DACommitmentScheme;
use zksync_os_l1_sender::commands::{L1SenderCommand, commit::CommitCommand};
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent};

pub struct BitcoinDaFinalityGate {
    config: BatcherConfig,
    storage: BitcoinDaStatusStorage,
}

impl BitcoinDaFinalityGate {
    pub fn new(config: BatcherConfig, storage: BitcoinDaStatusStorage) -> Self {
        Self { config, storage }
    }

    fn current_finality_policy(&self) -> BitcoinDaFinalityPolicy {
        BitcoinDaFinalityPolicy {
            mode: self.config.bitcoin_da_finality_mode,
            confirmations: self.config.bitcoin_da_finality_confirmations,
        }
    }

    fn finality_mode(&self) -> ClientBitcoinDaFinalityMode {
        match self.config.bitcoin_da_finality_mode {
            BitcoinDaFinalityMode::Chainlock => ClientBitcoinDaFinalityMode::Chainlock,
            BitcoinDaFinalityMode::Confirmations => ClientBitcoinDaFinalityMode::Confirmations,
        }
    }

    fn client(&self) -> anyhow::Result<SyscoinClient> {
        let rpc_url = self
            .config
            .bitcoin_da_rpc_url
            .as_deref()
            .context("`batcher.bitcoin_da_rpc_url` must be set when using blob pubdata mode")?;
        let rpc_user =
            self.config.bitcoin_da_rpc_user.as_ref().context(
                "`batcher.bitcoin_da_rpc_user` must be set when using blob pubdata mode",
            )?;
        let rpc_password = self.config.bitcoin_da_rpc_password.as_ref().context(
            "`batcher.bitcoin_da_rpc_password` must be set when using blob pubdata mode",
        )?;

        SyscoinClient::new(
            rpc_url,
            rpc_user.expose_secret(),
            rpc_password.expose_secret(),
            &self.config.bitcoin_da_poda_url,
            Some(self.config.bitcoin_da_request_timeout),
            &self.config.bitcoin_da_wallet_name,
        )
        .map_err(|err| anyhow::anyhow!("failed to create Bitcoin DA client: {err}"))
    }

    async fn wait_for_batch_finality(&self, batch_number: u64) -> anyhow::Result<()> {
        let mut status = self.storage.load(batch_number).await?.with_context(|| {
            format!("missing Bitcoin DA publication status for batch {batch_number}")
        })?;
        let current_policy = self.current_finality_policy();
        if status.finalized && status.finality_policy.as_ref() == Some(&current_policy) {
            tracing::info!(batch_number, "Bitcoin DA already finalized");
            return Ok(());
        }

        anyhow::ensure!(
            status.published_hashes.len() == status.expected_hashes.len(),
            "Bitcoin DA publication incomplete for batch {batch_number}: published {} of {} blobs",
            status.published_hashes.len(),
            status.expected_hashes.len(),
        );
        for (idx, (published_hash, expected_hash)) in status
            .published_hashes
            .iter()
            .zip(status.expected_hashes.iter())
            .enumerate()
        {
            let normalized_hash = published_hash.strip_prefix("0x").unwrap_or(published_hash);
            anyhow::ensure!(
                normalized_hash.eq_ignore_ascii_case(expected_hash),
                "Bitcoin DA version hash mismatch for batch {batch_number}, blob {idx}: expected {expected_hash}, got {normalized_hash}"
            );
        }

        let client = self.client()?;
        let finality_mode = self.finality_mode();
        for version_hash in &status.published_hashes {
            let start = Instant::now();
            loop {
                let is_final = client
                    .check_blob_finality_with_mode(
                        version_hash,
                        finality_mode,
                        self.config.bitcoin_da_finality_confirmations,
                    )
                    .await
                    .map_err(|err| {
                        anyhow::anyhow!(
                            "failed to check Bitcoin DA finality for batch {batch_number}: {err}"
                        )
                    })?;
                if is_final {
                    tracing::info!(batch_number, version_hash, "Bitcoin DA blob finalized");
                    break;
                }

                if start.elapsed() >= self.config.bitcoin_da_finality_timeout {
                    anyhow::bail!(
                        "Bitcoin DA blob for batch {batch_number} did not finalize within {:?}",
                        self.config.bitcoin_da_finality_timeout
                    );
                }

                tokio::time::sleep(self.config.bitcoin_da_finality_poll_interval).await;
            }
        }

        status.finalized = true;
        status.finality_policy = Some(current_policy);
        self.storage.save(batch_number, &status).await?;
        Ok(())
    }

    async fn wait_for_command_finality(&self, command: &CommitCommand) -> anyhow::Result<()> {
        for batch in command.as_ref() {
            if batch.batch.batch_info.commit_info.l2_da_commitment_scheme
                == DACommitmentScheme::BlobsZKsyncOS
            {
                self.wait_for_batch_finality(batch.batch_number()).await?;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl PipelineComponent for BitcoinDaFinalityGate {
    type Input = L1SenderCommand<CommitCommand>;
    type Output = L1SenderCommand<CommitCommand>;

    const NAME: &'static str = "bitcoin_da_finality_gate";
    const OUTPUT_BUFFER_SIZE: usize = 5;

    async fn run(
        self,
        mut input: PeekableReceiver<Self::Input>,
        output: mpsc::Sender<Self::Output>,
    ) -> anyhow::Result<()> {
        while let Some(command) = input.recv().await {
            if let L1SenderCommand::SendToL1(commit_command) = &command {
                self.wait_for_command_finality(commit_command).await?;
            }
            if output.send(command).await.is_err() {
                tracing::info!("outbound channel closed");
                return Ok(());
            }
        }
        tracing::info!("inbound channel closed");
        Ok(())
    }
}
