use crate::batcher::bitcoin_da_status_storage::{
    BitcoinDaBatchStatus, BitcoinDaFinalityPolicy, BitcoinDaStatusStorage,
};
use crate::config::{BatcherConfig, BitcoinDaFinalityMode};
use alloy::hex;
use anyhow::Context;
use async_trait::async_trait;
use bitcoin_da_client::{
    BitcoinDaFinalityMode as ClientBitcoinDaFinalityMode, BlobFinalityState, MAX_BLOB_SIZE,
    SyscoinClient,
};
use blake2::{Blake2s256, Digest};
use secrecy::ExposeSecret;
use std::collections::BTreeSet;
use std::time::Duration;
use tokio::sync::mpsc;
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

#[cfg(test)]
fn recovery_now_unix_ms() -> anyhow::Result<u64> {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("Bitcoin DA recovery clock is before UNIX epoch")?
        .as_millis()
        .try_into()
        .context("Bitcoin DA recovery clock exceeds u64 milliseconds")
}

fn authenticate_recovered_blob(
    blob: Vec<u8>,
    version_hash: &str,
) -> anyhow::Result<(Vec<u8>, String)> {
    anyhow::ensure!(
        !blob.is_empty() && blob.len() <= 2 * MAX_BLOB_SIZE,
        "recovered Bitcoin DA blob {version_hash} has invalid size {}, expected raw data up to {MAX_BLOB_SIZE} bytes or bounded bare hex",
        blob.len()
    );
    let normalized_expected = version_hash.strip_prefix("0x").unwrap_or(version_hash);
    // SYSCOIN: The RPC returns decoded data, but PoDA may return bare hex. Authenticate
    // raw bytes first so valid hex-looking blobs are not silently decoded into other data.
    if blob.len() <= MAX_BLOB_SIZE {
        let recovered_hash = hex::encode(Blake2s256::digest(&blob));
        if recovered_hash.eq_ignore_ascii_case(normalized_expected) {
            return Ok((blob, recovered_hash));
        }
    }
    let is_bare_hex = blob.len().is_multiple_of(2) && blob.iter().all(u8::is_ascii_hexdigit);
    anyhow::ensure!(
        blob.len() <= MAX_BLOB_SIZE || is_bare_hex,
        "recovered Bitcoin DA blob {version_hash} has invalid size {}, expected raw data up to {MAX_BLOB_SIZE} bytes or bounded bare hex",
        blob.len()
    );
    anyhow::ensure!(
        is_bare_hex,
        "recovered Bitcoin DA hash mismatch: expected {normalized_expected}; recovery bytes are not authenticated raw data or strict bare hex"
    );
    // SYSCOIN: Bound the wire representation before allocating, decode once, and
    // require the same committed hash before reserving an attempt or spending funds.
    let blob = hex::decode(blob).context("invalid recovered Bitcoin DA bare hex")?;
    anyhow::ensure!(
        !blob.is_empty() && blob.len() <= MAX_BLOB_SIZE,
        "recovered Bitcoin DA blob {version_hash} has invalid decoded size {}",
        blob.len()
    );
    let recovered_hash = hex::encode(Blake2s256::digest(&blob));
    anyhow::ensure!(
        recovered_hash.eq_ignore_ascii_case(normalized_expected),
        "recovered Bitcoin DA hash mismatch: expected {normalized_expected}, got {recovered_hash}"
    );
    Ok((blob, recovered_hash))
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
        // SYSCOIN: Keep the wallet budget guard effective for callers that bypass startup config.
        anyhow::ensure!(
            self.config.bitcoin_da_max_republish_attempts == 0
                || !self.config.bitcoin_da_finality_timeout.is_zero(),
            "Bitcoin DA finality timeout must be positive when republication is enabled"
        );
        anyhow::ensure!(
            self.config.bitcoin_da_max_republish_attempts == 0
                || self
                    .config
                    .bitcoin_da_missing_blob_recovery_timeout
                    .is_none_or(|timeout| !timeout.is_zero()),
            "Bitcoin DA missing blob recovery timeout must be positive when republication is enabled"
        );
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

            let initial_delay = if matches!(finality_state, BlobFinalityState::Missing) {
                self.config
                    .bitcoin_da_missing_blob_recovery_timeout
                    .unwrap_or(self.config.bitcoin_da_finality_timeout)
            } else {
                self.config.bitcoin_da_finality_timeout
            };
            if self
                .storage
                .recovery_due(
                    version_hash,
                    initial_delay,
                    self.config.bitcoin_da_finality_timeout,
                )
                .await?
            {
                self.republish_blob_after_timeout(client, version_hash, context, initial_delay)
                    .await?;
            }

            tokio::time::sleep(self.config.bitcoin_da_finality_poll_interval).await;
        }
    }

    async fn republish_blob_after_timeout(
        &self,
        client: &SyscoinClient,
        version_hash: &str,
        context: BlobFinalityWaitContext,
        initial_delay: Duration,
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
        // SYSCOIN: Availability is not authentication; normalize only hash-bound recovery
        // bytes before the durable attempt reservation and wallet publication boundary.
        let (blob, recovered_hash) = authenticate_recovered_blob(blob, version_hash)?;
        let normalized_expected = version_hash.strip_prefix("0x").unwrap_or(version_hash);
        let Some(claim) = self
            .storage
            .claim_republication(
                &recovered_hash,
                initial_delay,
                self.config.bitcoin_da_finality_timeout,
                self.config.bitcoin_da_max_republish_attempts,
            )
            .await?
        else {
            return Ok(());
        };
        let attempt = claim.attempt;
        tracing::warn!(
            version_hash,
            attempt,
            "Reserved Bitcoin DA republication attempt"
        );
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
        // Keep the per-hash OS lock until the wallet call and response check finish, even
        // when an operator deliberately configures a cooldown shorter than the RPC timeout.
        drop(claim);
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
            // SYSCOIN: Parse and enforce the canonical per-message and aggregate 32-ref bound
            // before constructing a client or issuing any finality/republish work.
            let edge_refs = syscoin_edge_da_refs_from_input(input).with_context(|| {
                format!(
                    "failed to parse Gateway edge DA refs for batch {}",
                    batch.batch_number()
                )
            })?;
            // SYSCOIN: This pass verifies only compact edge-chain references forwarded by a Gateway
            // batch; the batch's own Bitcoin DA chunks are checked separately. Create the
            // client lazily once there is at least one forwarded reference to verify.
            if client.is_none() {
                client = Some(self.client()?);
            }
            let client = client
                .as_ref()
                .expect("Bitcoin DA client was initialized before use");
            for edge_ref in edge_refs {
                for version_hash in edge_ref.blob_version_hashes.chunks_exact(32) {
                    let version_hash = hex::encode(version_hash);
                    self.wait_for_edge_ref_finality(client, &version_hash)
                        .await?;
                }
            }
        }
        Ok(())
    }

    async fn verify_command_da_before_commit(&self, command: &CommitCommand) -> anyhow::Result<()> {
        self.prime_command_recovery_clocks(command).await?;
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

    async fn prime_command_recovery_clocks(&self, command: &CommitCommand) -> anyhow::Result<()> {
        if self.settling_on_gateway {
            return Ok(());
        }
        let mut hashes = BTreeSet::new();
        for batch in command.as_ref() {
            let info = &batch.batch.batch_info.commit_info;
            if info.l2_da_commitment_scheme == DACommitmentScheme::BlobsZKsyncOS {
                anyhow::ensure!(
                    info.operator_da_input.len().is_multiple_of(32)
                        && info.operator_da_input.len() / 32 <= SYSCOIN_DA_MAX_BLOBS_PER_BATCH,
                    "invalid Bitcoin DA operator hash array for batch {}",
                    batch.batch_number()
                );
                let status = self
                    .storage
                    .load(batch.batch_number())
                    .await?
                    .with_context(|| {
                        format!(
                            "missing Bitcoin DA publication status for batch {}",
                            batch.batch_number()
                        )
                    })?;
                let expected: Vec<String> = info
                    .operator_da_input
                    .chunks_exact(32)
                    .map(hex::encode)
                    .collect();
                anyhow::ensure!(
                    status.expected_hashes == expected,
                    "Bitcoin DA publication status mismatch for batch {}",
                    batch.batch_number()
                );
                self.verify_published_batch_status(batch.batch_number(), &status)
                    .await?;
                if !(status.finalized
                    && status.finality_policy.as_ref() == Some(&self.current_finality_policy()))
                {
                    hashes.extend(expected);
                }
            }
            if !info.edge_da_refs_input.is_empty() {
                let refs = syscoin_edge_da_refs_from_input(&info.edge_da_refs_input).with_context(
                    || {
                        format!(
                            "failed to parse Gateway edge DA refs for batch {}",
                            batch.batch_number()
                        )
                    },
                )?;
                for edge_ref in refs {
                    hashes.extend(
                        edge_ref
                            .blob_version_hashes
                            .chunks_exact(32)
                            .map(hex::encode),
                    );
                }
            }
        }
        if hashes.is_empty() {
            return Ok(());
        }
        let client = self.client()?;
        // Only this already-bounded command is primed, not the queued stream. Later refs
        // age while earlier refs settle, instead of starting another full recovery delay.
        for hash in hashes {
            let state = self.blob_finality_state(&client, &hash).await?;
            if matches!(
                state,
                BlobFinalityState::Missing | BlobFinalityState::Unconfirmed
            ) {
                self.storage.observe_recovery(&hash).await?;
            } else {
                // A previously republished ref can prune while earlier refs settle. Start
                // its legacy cooldown now, not another full window when it becomes missing.
                self.storage.prime_legacy_recovery(&hash).await?;
            }
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
            output.send_and_record(command, &state_reporter).await?;
        }
        tracing::info!("inbound channel closed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, routing::get};
    use serde_json::{Value, json};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    const ABC_HASH: &str = "508c5e8c327c14e2e1a72ba34eeb452f37458b209ed63a294d999b4c86675982";
    const METADATA_POLL_BUDGET_ERROR: &str = "test metadata poll budget exhausted";
    const HEX_LOOKING_HASH: &str =
        "d6ffd191d5851bd88ef4f0fffd051050a9e91c15346096a0e1f0a9afa50f34e5";

    fn recovery_test_command(own_hashes: &[String], edge_input: Vec<u8>) -> CommitCommand {
        use alloy::primitives::{Address, B256};
        use zksync_os_batch_types::{
            PendingBatchInfo,
            batcher_model::{BatchForSigning, BatchMetadata, BatchSignatureData, FriProof},
        };
        use zksync_os_contract_interface::{
            l1_discovery::BatchVerificationSL,
            models::{CommitBatchInfo, StoredBatchInfo},
        };
        use zksync_os_types::{ProtocolSemanticVersion, PubdataMode};
        let batch_number = 7;
        let metadata = BatchMetadata {
            previous_stored_batch_info: StoredBatchInfo {
                batch_number: batch_number - 1,
                state_commitment: B256::ZERO,
                number_of_layer1_txs: 0,
                priority_operations_hash: B256::ZERO,
                dependency_roots_rolling_hash: B256::ZERO,
                l2_to_l1_logs_root_hash: B256::ZERO,
                commitment: B256::ZERO,
                last_block_timestamp: Some(0),
            },
            batch_info: PendingBatchInfo {
                commit_info: CommitBatchInfo {
                    batch_number,
                    new_state_commitment: B256::ZERO,
                    number_of_layer1_txs: 0,
                    number_of_layer2_txs: 0,
                    priority_operations_hash: B256::ZERO,
                    dependency_roots_rolling_hash: B256::ZERO,
                    l2_to_l1_logs_root_hash: B256::ZERO,
                    l2_da_commitment_scheme: DACommitmentScheme::BlobsZKsyncOS,
                    da_commitment: B256::ZERO,
                    first_block_timestamp: 0,
                    first_block_number: Some(batch_number),
                    last_block_timestamp: 0,
                    last_block_number: Some(batch_number),
                    chain_id: 1,
                    operator_da_input: own_hashes
                        .iter()
                        .flat_map(|hash| hex::decode(hash).unwrap())
                        .collect(),
                    edge_da_refs_input: edge_input,
                    edge_da_refs_root: B256::ZERO,
                    sl_chain_id: 2,
                },
                protocol_version: ProtocolSemanticVersion::new(0, 32, 0),
                upgrade_tx_hash: None,
            },
            chain_address: Address::ZERO,
            first_block_number: batch_number,
            last_block_number: batch_number,
            last_block_hash: None,
            pubdata_mode: PubdataMode::Blobs,
            tx_count: 0,
            computational_native_used: None,
            logs: vec![],
            messages: vec![],
            multichain_root: B256::ZERO,
            set_sl_chain_id_migration_number: None,
        };
        let batch = BatchForSigning::new(metadata, FriProof::Fake)
            .with_signatures(BatchSignatureData::NotNeeded);
        CommitCommand::try_new(&BatchVerificationSL::Disabled, batch).unwrap()
    }

    fn recovery_test_edge_message(chain: u64, batch: u64, hashes: &[String]) -> Vec<u8> {
        use alloy::primitives::U256;
        let bytes: Vec<u8> = hashes
            .iter()
            .flat_map(|hash| hex::decode(hash).unwrap())
            .collect();
        let mut message = vec![0; 31];
        message.push(1);
        message.extend(U256::from(chain).to_be_bytes::<32>());
        message.extend(U256::from(batch).to_be_bytes::<32>());
        message.extend([0; 32]);
        message.extend(U256::from(160).to_be_bytes::<32>());
        message.extend(U256::from(bytes.len()).to_be_bytes::<32>());
        message.extend(bytes);
        message
    }

    #[tokio::test]
    async fn current_confirmed_legacy_refs_age_before_pruning_without_expanding_recovery_scope() {
        use std::sync::atomic::AtomicBool;

        let hashes: Vec<String> = ["legacy-own", "legacy-edge", "normal-final", "future-legacy"]
            .into_iter()
            .map(|value| hex::encode(Blake2s256::digest(value.as_bytes())))
            .collect();
        let own = vec![hashes[0].clone(), hashes[2].clone()];
        let command = recovery_test_command(&own, recovery_test_edge_message(10, 1, &hashes[1..3]));
        let pruned = Arc::new(AtomicBool::new(false));
        let matured = Arc::new(AtomicBool::new(false));
        let wallet_calls = Arc::new(AtomicUsize::new(0));
        let handler = {
            let pruned = pruned.clone();
            let matured = matured.clone();
            let wallet_calls = wallet_calls.clone();
            let current = hashes[..3].to_vec();
            move |Json(request): Json<Value>| {
                let mut body = match request["method"].as_str().unwrap() {
                    "getblockcount" => json!({"result": 10}),
                    "getnevmblobdata" => {
                        let hash = request["params"][0]
                            .as_str()
                            .unwrap()
                            .trim_start_matches("0x");
                        assert!(current.iter().any(|value| value == hash));
                        assert_eq!(
                            request["params"],
                            json!([hash]),
                            "priming uses Core's metadata-only default"
                        );
                        if pruned.load(Ordering::SeqCst) && hash != current[2] {
                            json!({"error": {"code": -32602, "message": "missing blob"}})
                        } else {
                            let height = if hash == current[0] && !matured.load(Ordering::SeqCst) {
                                10
                            } else {
                                9
                            };
                            json!({"result": {"height": height, "chainlock": false}})
                        }
                    }
                    "syscoincreatenevmblob" => {
                        wallet_calls.fetch_add(1, Ordering::SeqCst);
                        panic!("priming or a still-final ref must never publish")
                    }
                    method => panic!("unexpected method: {method}"),
                };
                body["id"] = request["id"].clone();
                async move { Json(body) }
            }
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().fallback(axum::routing::post(handler));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = tempfile::tempdir().unwrap();
        let storage = BitcoinDaStatusStorage::new(dir.path()).unwrap();
        let status = BitcoinDaBatchStatus {
            expected_hashes: own.clone(),
            published_hashes: own,
            ..Default::default()
        };
        storage.save(7, &status).await.unwrap();
        for index in [0, 1, 3] {
            assert_eq!(
                storage
                    .reserve_republication(&hashes[index], 3)
                    .await
                    .unwrap(),
                1
            );
        }
        let cooldown = Duration::from_secs(5_400);
        let missing = Duration::from_secs(300);
        let config = BatcherConfig {
            bitcoin_da_rpc_url: Some(url.clone()),
            bitcoin_da_rpc_user: Some("user".into()),
            bitcoin_da_rpc_password: Some("password".into()),
            bitcoin_da_poda_url: url,
            bitcoin_da_finality_mode: BitcoinDaFinalityMode::Confirmations,
            bitcoin_da_finality_confirmations: 2,
            bitcoin_da_finality_timeout: cooldown,
            bitcoin_da_missing_blob_recovery_timeout: Some(missing),
            ..Default::default()
        };
        let gate = BitcoinDaFinalityGate::new(config.clone(), storage.clone(), false);
        let client = gate.client().unwrap();
        assert!(matches!(
            gate.blob_finality_state(&client, &hashes[0]).await.unwrap(),
            BlobFinalityState::Confirmed { .. }
        ));
        assert!(
            gate.blob_finality_state(&client, &hashes[1])
                .await
                .unwrap()
                .is_final()
        );
        gate.prime_command_recovery_clocks(&command).await.unwrap();
        let mut primed = Vec::new();
        for hash in &hashes[..2] {
            let path = dir.path().join(format!("recovery_{hash}.json"));
            let bytes = std::fs::read(path).unwrap();
            let clock: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                clock["first_observed_unix_ms"],
                clock["last_attempt_unix_ms"]
            );
            let due_at = clock["last_attempt_unix_ms"].as_u64().unwrap() + 5_400_000;
            assert!(
                !storage
                    .recovery_due_at(hash, due_at - 1, missing, cooldown)
                    .await
                    .unwrap()
            );
            primed.push((hash.clone(), bytes, due_at));
        }
        // Normal final refs and even a retained legacy reservation outside this command
        // must not acquire a new recovery clock or lock.
        for index in [2, 3] {
            for extension in ["json", "lock"] {
                assert!(
                    !dir.path()
                        .join(format!("recovery_{}.{extension}", hashes[index]))
                        .exists()
                );
            }
        }
        pruned.store(true, Ordering::SeqCst);
        for (hash, original, due_at) in primed {
            assert!(matches!(
                gate.blob_finality_state(&client, &hash).await.unwrap(),
                BlobFinalityState::Missing
            ));
            storage.observe_recovery_at(&hash, due_at).await.unwrap();
            assert!(
                storage
                    .recovery_due_at(&hash, due_at, missing, cooldown)
                    .await
                    .unwrap()
            );
            assert_eq!(
                std::fs::read(dir.path().join(format!("recovery_{hash}.json"))).unwrap(),
                original
            );
        }

        // The same legacy clocks being overdue must not change finality acceptance or
        // cause a wallet call when a fresh chain read still satisfies the policy.
        pruned.store(false, Ordering::SeqCst);
        matured.store(true, Ordering::SeqCst);
        let due_dir = tempfile::tempdir().unwrap();
        let due_storage = BitcoinDaStatusStorage::new(due_dir.path()).unwrap();
        due_storage.save(7, &status).await.unwrap();
        let now_ms = recovery_now_unix_ms().unwrap();
        for hash in &hashes[..2] {
            due_storage.reserve_republication(hash, 3).await.unwrap();
            due_storage
                .prime_legacy_recovery_at(hash, now_ms - 5_400_001)
                .await
                .unwrap();
        }
        let due_gate = BitcoinDaFinalityGate::new(config.clone(), due_storage.clone(), false);
        due_gate
            .verify_command_da_before_commit(&command)
            .await
            .unwrap();
        assert!(due_storage.load(7).await.unwrap().unwrap().finalized);
        assert_eq!(wallet_calls.load(Ordering::SeqCst), 0);
        for hash in &hashes[..2] {
            let clock: Value = serde_json::from_slice(
                &std::fs::read(due_dir.path().join(format!("recovery_{hash}.json"))).unwrap(),
            )
            .unwrap();
            assert_eq!(clock["last_attempt_unix_ms"], now_ms - 5_400_001);
            assert!(!due_dir.path().join(format!("republish_{hash}_2")).exists());
        }

        let cached_dir = tempfile::tempdir().unwrap();
        let cached_storage = BitcoinDaStatusStorage::new(cached_dir.path()).unwrap();
        cached_storage
            .reserve_republication(&hashes[0], 3)
            .await
            .unwrap();
        cached_storage
            .save(
                7,
                &BitcoinDaBatchStatus {
                    finalized: true,
                    finality_policy: Some(BitcoinDaFinalityPolicy {
                        mode: BitcoinDaFinalityMode::Confirmations,
                        confirmations: 2,
                    }),
                    ..status
                },
            )
            .await
            .unwrap();
        let cached_gate = BitcoinDaFinalityGate::new(config, cached_storage, false);
        cached_gate
            .prime_command_recovery_clocks(&command)
            .await
            .unwrap();
        for extension in ["json", "lock"] {
            assert!(
                !cached_dir
                    .path()
                    .join(format!("recovery_{}.{extension}", hashes[0]))
                    .exists()
            );
        }
        server.abort();
    }

    #[tokio::test]
    async fn actual_command_primes_unique_own_and_parsed_edge_refs_before_sequential_waits() {
        use std::collections::{HashMap, HashSet};
        use std::sync::{Mutex, atomic::AtomicU64};
        use tokio::sync::Notify;
        let blobs: Vec<Vec<u8>> = [
            b"own-a".as_slice(),
            b"own-b",
            b"already-confirmed",
            b"already-finalized",
            b"edge-a",
            b"edge-b",
        ]
        .into_iter()
        .map(<[u8]>::to_vec)
        .collect();
        let hashes: Vec<String> = blobs
            .iter()
            .map(|blob| hex::encode(Blake2s256::digest(blob)))
            .collect();
        let own_hashes = hashes[..4].to_vec();
        let edge_input = [
            recovery_test_edge_message(
                10,
                1,
                &[hashes[4].clone(), hashes[0].clone(), hashes[2].clone()],
            ),
            recovery_test_edge_message(
                11,
                2,
                &[hashes[5].clone(), hashes[3].clone(), hashes[4].clone()],
            ),
        ]
        .concat();
        let command = recovery_test_command(&own_hashes, edge_input);
        let payloads = Arc::new(hashes.iter().cloned().zip(blobs).collect::<HashMap<_, _>>());
        let published = Arc::new(Mutex::new(HashSet::<String>::new()));
        let height = Arc::new(AtomicU64::new(10));
        let wallet_calls = Arc::new(AtomicUsize::new(0));
        let wallet_entered = Arc::new(Notify::new());
        let wallet_release = Arc::new(Notify::new());
        let canonical_confirmation_seen = Arc::new(Notify::new());
        let canonical_confirmation_polls = Arc::new(AtomicUsize::new(0));
        let handler = {
            let payloads = payloads.clone();
            let published = published.clone();
            let height = height.clone();
            let wallet_calls = wallet_calls.clone();
            let wallet_entered = wallet_entered.clone();
            let wallet_release = wallet_release.clone();
            let canonical_confirmation_seen = canonical_confirmation_seen.clone();
            let canonical_confirmation_polls = canonical_confirmation_polls.clone();
            let hashes = hashes.clone();
            move |Json(request): Json<Value>| {
                let payloads = payloads.clone();
                let published = published.clone();
                let height = height.clone();
                let wallet_calls = wallet_calls.clone();
                let wallet_entered = wallet_entered.clone();
                let wallet_release = wallet_release.clone();
                let canonical_confirmation_seen = canonical_confirmation_seen.clone();
                let canonical_confirmation_polls = canonical_confirmation_polls.clone();
                let hashes = hashes.clone();
                async move {
                    let mut body = match request["method"].as_str().unwrap() {
                        "getblockcount" => json!({"result": height.load(Ordering::SeqCst)}),
                        "getnevmblobdata" => {
                            let hash = request["params"][0]
                                .as_str()
                                .unwrap()
                                .trim_start_matches("0x");
                            assert!(payloads.contains_key(hash));
                            if request["params"][1] == true {
                                json!({"result": {"data": hex::encode(&payloads[hash])}})
                            } else if hash == hashes[3] {
                                json!({"result": {"height": 9, "chainlock": false}})
                            } else if hash == hashes[2] || published.lock().unwrap().contains(hash)
                            {
                                // A second metadata poll proves the gate processed the first
                                // one-confirmation response and kept waiting for its policy.
                                if hash == hashes[0]
                                    && height.load(Ordering::SeqCst) == 10
                                    && canonical_confirmation_polls.fetch_add(1, Ordering::SeqCst)
                                        >= 1
                                {
                                    canonical_confirmation_seen.notify_one();
                                }
                                json!({"result": {"height": 10, "chainlock": false}})
                            } else {
                                json!({"error": {"code": -32602, "message": "missing blob"}})
                            }
                        }
                        "syscoincreatenevmblob" => {
                            let bytes =
                                hex::decode(request["params"][0].as_str().unwrap()).unwrap();
                            let hash = hex::encode(Blake2s256::digest(&bytes));
                            assert_eq!(payloads[&hash], bytes);
                            wallet_calls.fetch_add(1, Ordering::SeqCst);
                            if hash == hashes[0] {
                                wallet_entered.notify_one();
                                wallet_release.notified().await;
                            }
                            published.lock().unwrap().insert(hash.clone());
                            json!({"result": {"versionhash": hash}})
                        }
                        method => panic!("unexpected method: {method}"),
                    };
                    body["id"] = request["id"].clone();
                    Json(body)
                }
            }
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().fallback(axum::routing::post(handler));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = tempfile::tempdir().unwrap();
        let storage = BitcoinDaStatusStorage::new(dir.path()).unwrap();
        storage
            .save(
                7,
                &BitcoinDaBatchStatus {
                    expected_hashes: own_hashes.clone(),
                    published_hashes: own_hashes,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let config = BatcherConfig {
            bitcoin_da_rpc_url: Some(url.clone()),
            bitcoin_da_rpc_user: Some("user".into()),
            bitcoin_da_rpc_password: Some("password".into()),
            bitcoin_da_poda_url: url,
            bitcoin_da_finality_mode: BitcoinDaFinalityMode::Confirmations,
            bitcoin_da_finality_confirmations: 2,
            bitcoin_da_finality_timeout: Duration::from_secs(5_400),
            bitcoin_da_missing_blob_recovery_timeout: Some(Duration::from_millis(1)),
            bitcoin_da_finality_poll_interval: Duration::from_millis(1),
            ..Default::default()
        };
        let gate = BitcoinDaFinalityGate::new(config, storage.clone(), false);
        let task =
            tokio::spawn(async move { gate.verify_command_da_before_commit(&command).await });
        tokio::time::timeout(Duration::from_secs(5), wallet_entered.notified())
            .await
            .unwrap();
        let mut first_observations = HashMap::new();
        for index in [0, 1, 4, 5] {
            let clock: Value = serde_json::from_slice(
                &std::fs::read(dir.path().join(format!("recovery_{}.json", hashes[index])))
                    .unwrap(),
            )
            .unwrap();
            first_observations.insert(
                hashes[index].clone(),
                clock["first_observed_unix_ms"].clone(),
            );
            if index != 0 {
                assert!(clock["last_attempt_unix_ms"].is_null());
            }
        }
        for index in [2, 3] {
            assert!(
                !dir.path()
                    .join(format!("recovery_{}.json", hashes[index]))
                    .exists()
            );
            assert!(
                !dir.path()
                    .join(format!("recovery_{}.lock", hashes[index]))
                    .exists()
            );
        }
        assert_eq!(wallet_calls.load(Ordering::SeqCst), 1);
        wallet_release.notify_one();
        tokio::time::timeout(
            Duration::from_secs(5),
            canonical_confirmation_seen.notified(),
        )
        .await
        .unwrap();
        assert!(
            !task.is_finished(),
            "one confirmation cannot release a two-confirmation policy"
        );
        height.store(11, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(wallet_calls.load(Ordering::SeqCst), 4);
        for (hash, first) in first_observations {
            let clock: Value = serde_json::from_slice(
                &std::fs::read(dir.path().join(format!("recovery_{hash}.json"))).unwrap(),
            )
            .unwrap();
            assert_eq!(
                clock["first_observed_unix_ms"], first,
                "later waits must reuse primed clocks"
            );
            assert!(dir.path().join(format!("republish_{hash}_1")).exists());
            assert!(!dir.path().join(format!("republish_{hash}_2")).exists());
        }
        let status = storage.load(7).await.unwrap().unwrap();
        assert!(status.finalized);
        assert_eq!(
            status.finality_policy,
            Some(BitcoinDaFinalityPolicy {
                mode: BitcoinDaFinalityMode::Confirmations,
                confirmations: 2
            })
        );
        server.abort();
    }

    #[test]
    fn recovery_authentication_preserves_raw_and_decodes_only_matching_hex() {
        for raw in [b"abc".to_vec(), b"616263".to_vec(), vec![0, 255, 17]] {
            let expected_hash = hex::encode(Blake2s256::digest(&raw));
            let (recovered, hash) =
                authenticate_recovered_blob(raw.clone(), &expected_hash).unwrap();
            assert_eq!(recovered, raw);
            assert_eq!(hash, expected_hash);
        }
        let raw = vec![0xde, 0xad, 0xbe, 0xef];
        let expected_hash = hex::encode(Blake2s256::digest(&raw));
        for wire in [
            b"deadbeef".to_vec(),
            b"DEADBEEF".to_vec(),
            b"DeAdBeEf".to_vec(),
        ] {
            let (recovered, hash) =
                authenticate_recovered_blob(wire, &format!("0x{}", expected_hash.to_uppercase()))
                    .unwrap();
            assert_eq!(recovered, raw);
            assert_eq!(hash, expected_hash);
        }
    }

    #[test]
    fn recovery_authentication_preserves_decoded_limit_and_bounds_wire() {
        let raw = vec![42; MAX_BLOB_SIZE];
        let expected_hash = hex::encode(Blake2s256::digest(&raw));
        for input in [raw.clone(), hex::encode(&raw).into_bytes()] {
            let (recovered, hash) = authenticate_recovered_blob(input, &expected_hash).unwrap();
            assert_eq!(recovered, raw);
            assert_eq!(hash, expected_hash);
        }
        let oversized = vec![42; MAX_BLOB_SIZE + 1];
        let oversized_hash = hex::encode(Blake2s256::digest(&oversized));
        for input in [oversized.clone(), hex::encode(oversized).into_bytes()] {
            assert!(authenticate_recovered_blob(input, &oversized_hash).is_err());
        }
    }

    #[test]
    fn recovery_authentication_refuses_malformed_mismatched_and_repeated_encoding() {
        for input in [
            Vec::new(),
            b"corrupt".to_vec(),
            b"61626".to_vec(),
            b"0x616263".to_vec(),
            b"616263\n".to_vec(),
            b" 616263".to_vec(),
            b"\"616263\"".to_vec(),
            br#"{"data":"616263"}"#.to_vec(),
            vec![255],
            b"deadbeef".to_vec(),
            b"363136323633".to_vec(),
        ] {
            assert!(authenticate_recovered_blob(input, ABC_HASH).is_err());
        }
        assert!(authenticate_recovered_blob(b"abc".to_vec(), "1234").is_err());
        assert!(authenticate_recovered_blob(b"616263".to_vec(), "not-a-hash").is_err());
        assert!(authenticate_recovered_blob(b"616263".to_vec(), &format!("0X{ABC_HASH}")).is_err());
    }

    async fn recovery_rpc(
        finality: Option<Value>,
        wallet_error: bool,
    ) -> (SyscoinClient, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let (client, wallet_calls, _, server) =
            recovery_rpc_with_metadata_budget(finality, wallet_error, None).await;
        (client, wallet_calls, server)
    }

    async fn recovery_rpc_with_metadata_budget(
        finality: Option<Value>,
        wallet_error: bool,
        metadata_budget: Option<usize>,
    ) -> (
        SyscoinClient,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        let wallet_calls = Arc::new(AtomicUsize::new(0));
        let calls = wallet_calls.clone();
        let metadata_reads = Arc::new(AtomicUsize::new(0));
        let reads = metadata_reads.clone();
        let app = Router::new().fallback(
            get(|| async { b"abc".to_vec() }).post(move |Json(request): Json<Value>| {
                let calls = calls.clone();
                let reads = reads.clone();
                let finality = finality.clone();
                async move {
                    let body = match request["method"].as_str().unwrap() {
                        "syscoincreatenevmblob" => {
                            calls.fetch_add(1, Ordering::SeqCst);
                            if wallet_error {
                                json!({"error": {"code": -1, "message": "ambiguous publication failure"}})
                            } else {
                                json!({"result": {"versionhash": ABC_HASH}})
                            }
                        }
                        "getnevmblobdata" if request["params"][1] == true => {
                            json!({"result": {"data": "616263"}})
                        }
                        "getblockcount" => json!({"result": 10}),
                        "getnevmblobdata" => {
                            let read = reads.fetch_add(1, Ordering::SeqCst);
                            if metadata_budget.is_some_and(|budget| read >= budget) {
                                json!({"error": {"code": -32099, "message": METADATA_POLL_BUDGET_ERROR}})
                            } else {
                                match finality {
                                    Some(state) => json!({"result": state}),
                                    None => json!({"error": {"code": -32602, "message": "missing blob"}}),
                                }
                            }
                        }
                        method => panic!("unexpected method: {method}"),
                    };
                    let mut body = body;
                    body["id"] = request["id"].clone();
                    Json(body)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = SyscoinClient::new(
            &url,
            "user",
            "password",
            &url,
            Some(std::time::Duration::from_secs(2)),
            "test",
        )
        .unwrap();
        (client, wallet_calls, metadata_reads, server)
    }

    #[tokio::test]
    async fn zero_recovery_window_cannot_consume_wallet_budget() {
        let (client, calls, server) = recovery_rpc(None, false).await;
        for config in [
            BatcherConfig {
                bitcoin_da_max_republish_attempts: 2,
                bitcoin_da_finality_timeout: Duration::ZERO,
                ..Default::default()
            },
            BatcherConfig {
                bitcoin_da_max_republish_attempts: 2,
                bitcoin_da_missing_blob_recovery_timeout: Some(Duration::ZERO),
                ..Default::default()
            },
        ] {
            let dir = tempfile::tempdir().unwrap();
            let gate = BitcoinDaFinalityGate::new(
                config,
                BitcoinDaStatusStorage::new(dir.path()).unwrap(),
                false,
            );
            let error = gate
                .wait_for_blob_finality(&client, ABC_HASH, BlobFinalityWaitContext::GatewayEdgeRef)
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains("must be positive"), "{error}");
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        }
        server.abort();
    }

    #[tokio::test]
    async fn missing_override_recovers_without_shortening_unconfirmed_timeout() {
        for state in [None, Some(json!({"chainlock": false}))] {
            let missing = state.is_none();
            let (client, calls, metadata_reads, server) =
                recovery_rpc_with_metadata_budget(state, true, missing.then_some(1)).await;
            let dir = tempfile::tempdir().unwrap();
            let config = BatcherConfig {
                bitcoin_da_finality_timeout: Duration::from_secs(5_400),
                bitcoin_da_missing_blob_recovery_timeout: Some(Duration::from_millis(1)),
                bitcoin_da_finality_poll_interval: Duration::from_millis(1),
                ..Default::default()
            };
            let gate = BitcoinDaFinalityGate::new(
                config,
                BitcoinDaStatusStorage::new(dir.path()).unwrap(),
                false,
            );
            let first_observed = recovery_now_unix_ms().unwrap() - 1_000;
            gate.storage
                .observe_recovery_at(ABC_HASH, first_observed)
                .await
                .unwrap();
            let wait = gate.wait_for_blob_finality(
                &client,
                ABC_HASH,
                BlobFinalityWaitContext::GatewayEdgeRef,
            );
            if missing {
                // A second metadata poll fails distinctly, so recovery must occur on the
                // first eligible poll without imposing a filesystem/network timing SLA.
                let error = wait.await.unwrap_err().to_string();
                assert!(error.contains("failed to republish"), "{error}");
                assert_eq!(metadata_reads.load(Ordering::SeqCst), 1);
                assert_eq!(calls.load(Ordering::SeqCst), 1);
                assert!(dir.path().join(format!("republish_{ABC_HASH}_1")).is_file());
                assert!(!dir.path().join(format!("republish_{ABC_HASH}_2")).exists());
                let clock: Value = serde_json::from_slice(
                    &std::fs::read(dir.path().join(format!("recovery_{ABC_HASH}.json"))).unwrap(),
                )
                .unwrap();
                assert_eq!(clock["first_observed_unix_ms"], json!(first_observed));
            } else {
                let result = tokio::time::timeout(Duration::from_millis(100), wait).await;
                assert!(result.is_err());
                assert_eq!(calls.load(Ordering::SeqCst), 0);
            }
            server.abort();
        }
    }

    #[tokio::test]
    async fn restart_does_not_reset_an_aged_missing_blob_clock() {
        let (client, calls, metadata_reads, server) =
            recovery_rpc_with_metadata_budget(None, true, Some(1)).await;
        let dir = tempfile::tempdir().unwrap();
        let storage = BitcoinDaStatusStorage::new(dir.path()).unwrap();
        let first_observed = recovery_now_unix_ms().unwrap() - 1_000;
        storage
            .observe_recovery_at(ABC_HASH, first_observed)
            .await
            .unwrap();
        drop(storage);
        let config = BatcherConfig {
            bitcoin_da_missing_blob_recovery_timeout: Some(Duration::from_millis(1)),
            bitcoin_da_finality_poll_interval: Duration::from_millis(1),
            ..Default::default()
        };
        let gate = BitcoinDaFinalityGate::new(
            config,
            BitcoinDaStatusStorage::new(dir.path()).unwrap(),
            false,
        );
        let error = gate
            .wait_for_blob_finality(&client, ABC_HASH, BlobFinalityWaitContext::GatewayEdgeRef)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("failed to republish"), "{error}");
        assert_eq!(metadata_reads.load(Ordering::SeqCst), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(dir.path().join(format!("republish_{ABC_HASH}_1")).is_file());
        assert!(!dir.path().join(format!("republish_{ABC_HASH}_2")).exists());
        let clock: Value = serde_json::from_slice(
            &std::fs::read(dir.path().join(format!("recovery_{ABC_HASH}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(clock["first_observed_unix_ms"], json!(first_observed));
        server.abort();
    }

    #[tokio::test]
    async fn finite_metadata_budget_rejects_a_second_poll_without_wallet() {
        let (client, calls, metadata_reads, server) =
            recovery_rpc_with_metadata_budget(None, true, Some(1)).await;
        let dir = tempfile::tempdir().unwrap();
        let config = BatcherConfig {
            bitcoin_da_finality_timeout: Duration::from_secs(5_400),
            bitcoin_da_missing_blob_recovery_timeout: None,
            bitcoin_da_finality_poll_interval: Duration::from_millis(1),
            ..Default::default()
        };
        let gate = BitcoinDaFinalityGate::new(
            config,
            BitcoinDaStatusStorage::new(dir.path()).unwrap(),
            false,
        );
        let first_observed = recovery_now_unix_ms().unwrap() - 1_000;
        gate.storage
            .observe_recovery_at(ABC_HASH, first_observed)
            .await
            .unwrap();
        let error = gate
            .wait_for_blob_finality(&client, ABC_HASH, BlobFinalityWaitContext::GatewayEdgeRef)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("failed to check Bitcoin DA finality"),
            "{error}"
        );
        assert!(error.contains(METADATA_POLL_BUDGET_ERROR), "{error}");
        assert!(!error.contains("failed to republish"), "{error}");
        assert_eq!(metadata_reads.load(Ordering::SeqCst), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(!dir.path().join(format!("republish_{ABC_HASH}_1")).exists());
        assert!(!dir.path().join(format!("republish_{ABC_HASH}_2")).exists());
        let clock: Value = serde_json::from_slice(
            &std::fs::read(dir.path().join(format!("recovery_{ABC_HASH}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(clock["first_observed_unix_ms"], json!(first_observed));
        assert!(clock["last_attempt_unix_ms"].is_null());
        server.abort();
    }

    #[tokio::test]
    async fn nonfinal_recovery_stops_at_the_wallet_attempt_budget() {
        for state in [None, Some(json!({"chainlock": false}))] {
            let (client, calls, server) = recovery_rpc(state, false).await;
            let dir = tempfile::tempdir().unwrap();
            let config = BatcherConfig {
                bitcoin_da_max_republish_attempts: 2,
                bitcoin_da_finality_timeout: std::time::Duration::from_millis(1),
                bitcoin_da_finality_poll_interval: std::time::Duration::from_millis(1),
                ..Default::default()
            };
            let gate = BitcoinDaFinalityGate::new(
                config,
                BitcoinDaStatusStorage::new(dir.path()).unwrap(),
                false,
            );
            let result = gate
                .wait_for_blob_finality(&client, ABC_HASH, BlobFinalityWaitContext::GatewayEdgeRef)
                .await;
            assert!(result.unwrap_err().to_string().contains("limit exhausted"));
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            server.abort();
        }
    }

    #[tokio::test]
    async fn failed_wallet_calls_consume_budget_across_gate_restarts_and_contexts() {
        let (client, calls, server) = recovery_rpc(None, true).await;
        let dir = tempfile::tempdir().unwrap();
        for attempt in 0..3 {
            let config = BatcherConfig {
                bitcoin_da_max_republish_attempts: 2,
                ..Default::default()
            };
            let gate = BitcoinDaFinalityGate::new(
                config,
                BitcoinDaStatusStorage::new(dir.path()).unwrap(),
                false,
            );
            let context = if attempt == 0 {
                BlobFinalityWaitContext::OwnBatch { batch_number: 1 }
            } else {
                BlobFinalityWaitContext::GatewayEdgeRef
            };
            let hash = if attempt == 0 {
                ABC_HASH.to_owned()
            } else {
                format!("0x{}", ABC_HASH.to_uppercase())
            };
            let error = gate
                .republish_blob_after_timeout(&client, &hash, context, Duration::ZERO)
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(if attempt < 2 {
                "failed to republish"
            } else {
                "limit exhausted"
            }));
            gate.republish_blob_after_timeout(&client, &hash, context, Duration::ZERO)
                .await
                .unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), (attempt + 1).min(2));
            // Simulate the durable cooldown elapsing without making this real-RPC test
            // depend on wall-clock sleeps or consuming the production 90-minute window.
            std::fs::write(
                dir.path().join(format!("recovery_{ABC_HASH}.json")),
                br#"{"version":1,"first_observed_unix_ms":1,"last_attempt_unix_ms":1}"#,
            )
            .unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    async fn confirmed_wait_and_finalized_success_need_no_republication_budget() {
        for finalized in [false, true] {
            let (client, calls, server) =
                recovery_rpc(Some(json!({"height": 10, "chainlock": finalized})), false).await;
            let dir = tempfile::tempdir().unwrap();
            let config = BatcherConfig {
                bitcoin_da_max_republish_attempts: 0,
                bitcoin_da_finality_timeout: std::time::Duration::ZERO,
                bitcoin_da_finality_poll_interval: std::time::Duration::from_millis(1),
                ..Default::default()
            };
            let gate = BitcoinDaFinalityGate::new(
                config,
                BitcoinDaStatusStorage::new(dir.path()).unwrap(),
                false,
            );
            let result = tokio::time::timeout(
                std::time::Duration::from_millis(100),
                gate.wait_for_blob_finality(
                    &client,
                    ABC_HASH,
                    BlobFinalityWaitContext::GatewayEdgeRef,
                ),
            )
            .await;
            if finalized {
                result.unwrap().unwrap();
            } else {
                assert!(result.is_err());
            }
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
            server.abort();
        }
    }

    #[tokio::test]
    async fn confirmation_policy_does_not_wait_for_chainlocks_or_republish_confirmed_blobs() {
        for required in [1, 2] {
            let (client, calls, server) =
                recovery_rpc(Some(json!({"height": 10, "chainlock": false})), false).await;
            let dir = tempfile::tempdir().unwrap();
            let config = BatcherConfig {
                bitcoin_da_finality_mode: BitcoinDaFinalityMode::Confirmations,
                bitcoin_da_finality_confirmations: required,
                bitcoin_da_missing_blob_recovery_timeout: Some(Duration::from_millis(1)),
                bitcoin_da_finality_timeout: Duration::from_millis(1),
                bitcoin_da_finality_poll_interval: Duration::from_millis(1),
                ..Default::default()
            };
            let gate = BitcoinDaFinalityGate::new(
                config,
                BitcoinDaStatusStorage::new(dir.path()).unwrap(),
                false,
            );
            let result = tokio::time::timeout(
                Duration::from_millis(100),
                gate.wait_for_blob_finality(
                    &client,
                    ABC_HASH,
                    BlobFinalityWaitContext::GatewayEdgeRef,
                ),
            )
            .await;
            if required == 1 {
                result.unwrap().unwrap();
            } else {
                assert!(result.is_err());
            }
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
            server.abort();
        }
    }

    #[tokio::test]
    async fn unknown_finality_rpc_errors_never_start_missing_recovery() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let app = Router::new().fallback(axum::routing::post(move |Json(request): Json<Value>| {
            let counted = counted.clone();
            async move {
                if request["method"] == "syscoincreatenevmblob" { counted.fetch_add(1, Ordering::SeqCst); }
                Json(json!({"id": request["id"], "error": {"code": -32603, "message": "upstream unavailable"}}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = SyscoinClient::new(
            &url,
            "user",
            "password",
            &url,
            Some(Duration::from_secs(2)),
            "test",
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let config = BatcherConfig {
            bitcoin_da_missing_blob_recovery_timeout: Some(Duration::from_millis(1)),
            ..Default::default()
        };
        let gate = BitcoinDaFinalityGate::new(
            config,
            BitcoinDaStatusStorage::new(dir.path()).unwrap(),
            false,
        );
        assert!(
            gate.wait_for_blob_finality(&client, ABC_HASH, BlobFinalityWaitContext::GatewayEdgeRef)
                .await
                .unwrap_err()
                .to_string()
                .contains("failed to check")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        server.abort();
    }

    #[tokio::test]
    async fn disabled_gateway_recovery_and_storage_failure_cannot_call_wallet() {
        let (client, calls, server) = recovery_rpc(None, false).await;
        let dir = tempfile::tempdir().unwrap();
        for disabled in [true, false] {
            let config = BatcherConfig {
                bitcoin_da_gateway_l1_republish_enabled: !disabled,
                ..Default::default()
            };
            let storage_path = dir
                .path()
                .join(if disabled { "disabled" } else { "broken" });
            let storage = BitcoinDaStatusStorage::new(&storage_path).unwrap();
            if !disabled {
                std::fs::remove_dir(&storage_path).unwrap();
                std::fs::write(&storage_path, b"not a directory").unwrap();
            }
            let gate = BitcoinDaFinalityGate::new(config, storage, false);
            assert!(
                gate.republish_blob_after_timeout(
                    &client,
                    ABC_HASH,
                    BlobFinalityWaitContext::GatewayEdgeRef,
                    Duration::ZERO,
                )
                .await
                .is_err()
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        server.abort();
    }

    // SYSCOIN: Exercise the wallet boundary through the real client, including its archive
    // fallback, so a check moved after publication cannot silently regress this protection.
    #[tokio::test]
    async fn recovery_authenticates_blob_before_wallet_publication() {
        let mut cases = vec![
            (b"abc".to_vec(), ABC_HASH.to_owned(), ABC_HASH, None, 1),
            (b"616263".to_vec(), ABC_HASH.to_owned(), ABC_HASH, None, 1),
            (
                b"616263".to_vec(),
                HEX_LOOKING_HASH.to_owned(),
                HEX_LOOKING_HASH,
                None,
                1,
            ),
            (
                b"abc".to_vec(),
                format!("0x{}", ABC_HASH.to_uppercase()),
                ABC_HASH,
                None,
                1,
            ),
            (
                b"corrupt".to_vec(),
                ABC_HASH.to_owned(),
                ABC_HASH,
                Some("recovered Bitcoin DA hash mismatch"),
                0,
            ),
            (
                b"363136323633".to_vec(),
                ABC_HASH.to_owned(),
                ABC_HASH,
                Some("recovered Bitcoin DA hash mismatch"),
                0,
            ),
            (
                Vec::new(),
                hex::encode(Blake2s256::digest([])),
                ABC_HASH,
                Some("invalid size"),
                0,
            ),
            (
                vec![0; MAX_BLOB_SIZE + 1],
                ABC_HASH.to_owned(),
                ABC_HASH,
                Some("invalid size"),
                0,
            ),
            (
                vec![b'0'; 2 * MAX_BLOB_SIZE + 1],
                ABC_HASH.to_owned(),
                ABC_HASH,
                Some("invalid size"),
                0,
            ),
            (
                b"abc".to_vec(),
                "1234".to_owned(),
                ABC_HASH,
                Some("recovered Bitcoin DA hash mismatch"),
                0,
            ),
            (
                b"abc".to_vec(),
                ABC_HASH.to_owned(),
                "wrong-wallet-result",
                Some("republished Bitcoin DA hash mismatch"),
                1,
            ),
        ];
        cases.extend(
            [
                b"61626".to_vec(),
                b"0x616263".to_vec(),
                b"616263\n".to_vec(),
                b" 616263".to_vec(),
                b"\"616263\"".to_vec(),
                br#"{"data":"616263"}"#.to_vec(),
                vec![255],
                b"deadbeef".to_vec(),
            ]
            .into_iter()
            .map(|blob| {
                (
                    blob,
                    ABC_HASH.to_owned(),
                    ABC_HASH,
                    Some("recovered Bitcoin DA hash mismatch"),
                    0,
                )
            }),
        );
        for (blob, expected_hash, wallet_hash, expected_error, expected_calls) in cases {
            let wallet_calls = Arc::new(AtomicUsize::new(0));
            let calls = wallet_calls.clone();
            let expected_rpc_data = if expected_hash == HEX_LOOKING_HASH {
                "363136323633"
            } else {
                "616263"
            };
            let app = Router::new().fallback(
                get(move || {
                    let blob = blob.clone();
                    async move {
                        ([(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], blob)
                    }
                })
                .post(move |Json(request): Json<Value>| {
                    let calls = calls.clone();
                    async move {
                        if request["method"] == "syscoincreatenevmblob" {
                            calls.fetch_add(1, Ordering::SeqCst);
                            assert_eq!(request["params"], json!([expected_rpc_data, true, "blake2s"]));
                            Json(json!({"id": 1, "result": {"versionhash": wallet_hash}}))
                        } else {
                            assert_eq!(request["method"], "getnevmblobdata");
                            Json(json!({"id": 1, "error": {"code": -32602, "message": "missing blob"}}))
                        }
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let client = SyscoinClient::new(
                &url,
                "user",
                "password",
                &url,
                Some(std::time::Duration::from_secs(2)),
                "test",
            )
            .unwrap();
            let storage_dir = tempfile::tempdir().unwrap();
            let gate = BitcoinDaFinalityGate::new(
                BatcherConfig::default(),
                BitcoinDaStatusStorage::new(storage_dir.path()).unwrap(),
                false,
            );
            let result = gate
                .republish_blob_after_timeout(
                    &client,
                    &expected_hash,
                    BlobFinalityWaitContext::OwnBatch { batch_number: 1 },
                    Duration::ZERO,
                )
                .await;
            server.abort();
            match expected_error {
                Some(message) => assert!(result.unwrap_err().to_string().contains(message)),
                None => result.unwrap(),
            }
            assert_eq!(wallet_calls.load(Ordering::SeqCst), expected_calls);
            assert_eq!(
                std::fs::read_dir(storage_dir.path())
                    .unwrap()
                    .filter(|entry| {
                        entry
                            .as_ref()
                            .unwrap()
                            .file_name()
                            .to_string_lossy()
                            .starts_with("republish_")
                    })
                    .count(),
                expected_calls,
                "invalid recovery must not consume a durable attempt"
            );
            if expected_calls == 0 {
                assert_eq!(std::fs::read_dir(storage_dir.path()).unwrap().count(), 0);
            }
            if expected_calls != 0 {
                let canonical_hash = expected_hash
                    .strip_prefix("0x")
                    .unwrap_or(&expected_hash)
                    .to_ascii_lowercase();
                let reservation = storage_dir
                    .path()
                    .join(format!("republish_{canonical_hash}_1"));
                let metadata = std::fs::symlink_metadata(reservation).unwrap();
                assert!(metadata.is_file());
                assert_eq!(metadata.len(), 0);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::{MetadataExt, PermissionsExt};
                    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
                    assert_eq!(metadata.nlink(), 1);
                }
            }
        }
    }
}
