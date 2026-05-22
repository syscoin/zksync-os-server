use crate::batcher::bitcoin_da_status_storage::{
    BitcoinDaBatchStatus, BitcoinDaFinalityPolicy, BitcoinDaStatusStorage,
};
use crate::config::{BatcherConfig, BitcoinDaFinalityMode};
use alloy::hex;
use anyhow::Context;
use async_trait::async_trait;
use bitcoin_da_client::{
    BitcoinDaFinalityMode as ClientBitcoinDaFinalityMode, BlobFinalityState, SyscoinClient,
};
use secrecy::ExposeSecret;
use tokio::sync::mpsc;
use tokio::time::Instant;
use zksync_os_batch_types::{SYSCOIN_DA_MAX_BLOBS_PER_BATCH, syscoin_edge_da_refs_from_input};
use zksync_os_contract_interface::models::DACommitmentScheme;
use zksync_os_l1_sender::commands::{L1SenderCommand, commit::CommitCommand};
use zksync_os_observability::ComponentStateReporter;
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent, SendAndRecordExt};

pub struct BitcoinDaFinalityGate {
    config: BatcherConfig,
    storage: BitcoinDaStatusStorage,
    settling_on_gateway: bool,
}

#[derive(Clone, Copy)]
enum BlobFinalityWaitContext {
    OwnBatch { batch_number: u64 },
    GatewayEdgeRef,
}

impl BitcoinDaFinalityGate {
    pub fn new(
        config: BatcherConfig,
        storage: BitcoinDaStatusStorage,
        settling_on_gateway: bool,
    ) -> Self {
        Self {
            config,
            storage,
            settling_on_gateway,
        }
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

    async fn verify_batch_da_before_commit(
        &self,
        batch_number: u64,
        expected_version_hashes: &[u8],
    ) -> anyhow::Result<()> {
        let expected_hashes: Vec<String> = expected_version_hashes
            .chunks_exact(32)
            .map(hex::encode)
            .collect();
        anyhow::ensure!(
            expected_hashes.len() * 32 == expected_version_hashes.len(),
            "Bitcoin DA operator input for batch {batch_number} is not a 32-byte hash array"
        );
        anyhow::ensure!(
            expected_hashes.len() <= SYSCOIN_DA_MAX_BLOBS_PER_BATCH,
            "Bitcoin DA batch {batch_number} has {} blobs, max is {}",
            expected_hashes.len(),
            SYSCOIN_DA_MAX_BLOBS_PER_BATCH
        );

        let mut status = self.storage.load(batch_number).await?.with_context(|| {
            format!("missing Bitcoin DA publication status for batch {batch_number}")
        })?;
        anyhow::ensure!(
            status.expected_hashes == expected_hashes,
            "Bitcoin DA publication status mismatch for batch {batch_number}: stored expected {:?}, command expected {:?}",
            status.expected_hashes,
            expected_hashes
        );
        if self.settling_on_gateway {
            self.verify_published_batch_status(batch_number, &status)
                .await?;
            return Ok(());
        }

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
        anyhow::ensure!(
            status.published_hashes.len() <= SYSCOIN_DA_MAX_BLOBS_PER_BATCH,
            "Bitcoin DA batch {batch_number} has {} blobs, max is {}",
            status.published_hashes.len(),
            SYSCOIN_DA_MAX_BLOBS_PER_BATCH
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
        for version_hash in &status.published_hashes {
            self.wait_for_blob_finality(
                &client,
                version_hash,
                BlobFinalityWaitContext::OwnBatch { batch_number },
            )
            .await?;
        }

        status.finalized = true;
        status.finality_policy = Some(current_policy);
        self.storage.save(batch_number, &status).await?;
        Ok(())
    }

    async fn verify_published_batch_status(
        &self,
        batch_number: u64,
        status: &BitcoinDaBatchStatus,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            status.published_hashes.len() == status.expected_hashes.len(),
            "Bitcoin DA publication incomplete for batch {batch_number}: published {} of {} blobs",
            status.published_hashes.len(),
            status.expected_hashes.len(),
        );
        anyhow::ensure!(
            status.published_hashes.len() <= SYSCOIN_DA_MAX_BLOBS_PER_BATCH,
            "Bitcoin DA batch {batch_number} has {} blobs, max is {}",
            status.published_hashes.len(),
            SYSCOIN_DA_MAX_BLOBS_PER_BATCH
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

        tracing::info!(
            batch_number,
            blob_count = status.published_hashes.len(),
            "Bitcoin DA publication status verified before Gateway commit"
        );
        Ok(())
    }

    async fn wait_for_edge_ref_finality(
        &self,
        client: &SyscoinClient,
        version_hash: &str,
    ) -> anyhow::Result<()> {
        self.wait_for_blob_finality(
            client,
            version_hash,
            BlobFinalityWaitContext::GatewayEdgeRef,
        )
        .await
    }

    async fn wait_for_blob_finality(
        &self,
        client: &SyscoinClient,
        version_hash: &str,
        context: BlobFinalityWaitContext,
    ) -> anyhow::Result<()> {
        let mut start = Instant::now();
        loop {
            let finality_state = self.blob_finality_state(client, version_hash).await?;
            if finality_state.is_final() {
                match context {
                    BlobFinalityWaitContext::OwnBatch { batch_number } => {
                        tracing::info!(batch_number, version_hash, "Bitcoin DA blob finalized");
                    }
                    BlobFinalityWaitContext::GatewayEdgeRef => {
                        tracing::info!(version_hash, "Gateway edge DA ref finalized");
                    }
                }
                return Ok(());
            }
            if matches!(finality_state, BlobFinalityState::Confirmed { .. }) {
                tokio::time::sleep(self.config.bitcoin_da_finality_poll_interval).await;
                continue;
            }

            if start.elapsed() >= self.config.bitcoin_da_finality_timeout {
                self.republish_blob_after_timeout(client, version_hash, context)
                    .await?;
                start = Instant::now();
            }

            tokio::time::sleep(self.config.bitcoin_da_finality_poll_interval).await;
        }
    }

    async fn republish_blob_after_timeout(
        &self,
        client: &SyscoinClient,
        version_hash: &str,
        context: BlobFinalityWaitContext,
    ) -> anyhow::Result<()> {
        match context {
            BlobFinalityWaitContext::OwnBatch { batch_number } => {
                tracing::warn!(
                    batch_number,
                    version_hash,
                    "Bitcoin DA blob did not make confirmation progress before timeout; fetching and republishing"
                );
            }
            BlobFinalityWaitContext::GatewayEdgeRef => {
                anyhow::ensure!(
                    self.config.bitcoin_da_gateway_l1_republish_enabled,
                    "Gateway edge DA ref {version_hash} did not finalize within {:?} and republish is disabled",
                    self.config.bitcoin_da_finality_timeout
                );
                tracing::warn!(
                    version_hash,
                    "Gateway edge DA ref did not finalize before timeout; fetching and republishing"
                );
            }
        }

        let blob = client
            .get_blob(version_hash)
            .await
            .map_err(|err| match context {
                BlobFinalityWaitContext::OwnBatch { batch_number } => anyhow::anyhow!(
                    "failed to fetch Bitcoin DA blob for batch {batch_number}, ref {version_hash}: {err}"
                ),
                BlobFinalityWaitContext::GatewayEdgeRef => anyhow::anyhow!(
                    "failed to fetch Bitcoin DA blob for Gateway edge ref {version_hash}: {err}"
                ),
            })?;
        let republished_hash = client.force_create_blob(&blob).await.map_err(|err| {
            match context {
                BlobFinalityWaitContext::OwnBatch { batch_number } => anyhow::anyhow!(
                    "failed to republish Bitcoin DA blob for batch {batch_number}, ref {version_hash}: {err}"
                ),
                BlobFinalityWaitContext::GatewayEdgeRef => anyhow::anyhow!(
                    "failed to republish Bitcoin DA blob for Gateway edge ref {version_hash}: {err}"
                ),
            }
        })?;
        let normalized_republished = republished_hash
            .strip_prefix("0x")
            .unwrap_or(&republished_hash);
        let normalized_expected = version_hash.strip_prefix("0x").unwrap_or(version_hash);
        anyhow::ensure!(
            normalized_republished.eq_ignore_ascii_case(normalized_expected),
            "{}",
            match context {
                BlobFinalityWaitContext::OwnBatch { batch_number } => format!(
                    "republished Bitcoin DA hash mismatch for batch {batch_number}: expected {normalized_expected}, got {normalized_republished}"
                ),
                BlobFinalityWaitContext::GatewayEdgeRef => format!(
                    "republished Bitcoin DA hash mismatch for Gateway edge ref: expected {normalized_expected}, got {normalized_republished}"
                ),
            }
        );
        Ok(())
    }

    async fn blob_finality_state(
        &self,
        client: &SyscoinClient,
        version_hash: &str,
    ) -> anyhow::Result<BlobFinalityState> {
        client
            .blob_finality_state_with_mode(
                version_hash,
                self.finality_mode(),
                self.config.bitcoin_da_finality_confirmations,
            )
            .await
            .map_err(|err| {
                anyhow::anyhow!("failed to check Bitcoin DA finality for {version_hash}: {err}")
            })
    }

    async fn wait_for_gateway_edge_refs_finality(
        &self,
        command: &CommitCommand,
    ) -> anyhow::Result<()> {
        let mut client = None;
        for batch in command.as_ref() {
            let input = &batch.batch.batch_info.commit_info.edge_da_refs_input;
            if input.is_empty() {
                continue;
            }
            // SYSCOIN: calldata-only batches do not require a Bitcoin DA RPC. Create the client
            // lazily only once we see Gateway edge DA refs that must be finalized on L1.
            if client.is_none() {
                client = Some(self.client()?);
            }
            let client = client
                .as_ref()
                .expect("Bitcoin DA client was initialized before use");
            let edge_refs = syscoin_edge_da_refs_from_input(input).with_context(|| {
                format!(
                    "failed to parse Gateway edge DA refs for batch {}",
                    batch.batch_number()
                )
            })?;
            for edge_ref in edge_refs {
                for version_hash in edge_ref.blob_version_hashes.chunks_exact(32) {
                    let version_hash = hex::encode(version_hash);
                    self.wait_for_edge_ref_finality(&client, &version_hash)
                        .await?;
                }
            }
        }
        Ok(())
    }

    async fn verify_command_da_before_commit(&self, command: &CommitCommand) -> anyhow::Result<()> {
        for batch in command.as_ref() {
            if batch.batch.batch_info.commit_info.l2_da_commitment_scheme
                == DACommitmentScheme::BlobsZKsyncOS
            {
                self.verify_batch_da_before_commit(
                    batch.batch_number(),
                    &batch.batch.batch_info.commit_info.operator_da_input,
                )
                .await?;
            }
        }
        if !self.settling_on_gateway {
            self.wait_for_gateway_edge_refs_finality(command).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl PipelineComponent for BitcoinDaFinalityGate {
    type Input = L1SenderCommand<CommitCommand>;
    type Output = L1SenderCommand<CommitCommand>;

    const COMPONENT_ID: zksync_os_pipeline::ComponentId =
        zksync_os_pipeline::ComponentId::BitcoinDaFinalityGate;
    const OUTPUT_CHANNEL_CAPACITY: usize = 5;

    async fn run(
        self,
        mut input: PeekableReceiver<Self::Input>,
        output: mpsc::Sender<Self::Output>,
        state_reporter: ComponentStateReporter,
    ) -> anyhow::Result<()> {
        while let Some(command) = input.recv().await {
            if let L1SenderCommand::SendToL1(commit_command) = &command {
                self.verify_command_da_before_commit(commit_command).await?;
            }
            output.send_and_record(command, &state_reporter)?;
        }
        tracing::info!("inbound channel closed");
        Ok(())
    }
}
