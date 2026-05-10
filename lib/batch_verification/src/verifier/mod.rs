use crate::config::SyscoinDaVerificationConfig;
use crate::verifier::metrics::BATCH_VERIFICATION_RESPONDER_METRICS;
use crate::verify_batch_wire::{VerificationRequest, normalized_commit_data};
use alloy::eips::BlockId;
use alloy::primitives::{Address, keccak256};
use alloy::signers::local::PrivateKeySigner;
use async_trait::async_trait;
use bitcoin_da_client::SyscoinClient;
use block_cache::BlockCache;
use secrecy::{ExposeSecret, SecretString};
use std::str::FromStr;
use tokio::sync::{broadcast, mpsc};
use zksync_os_batch_types::{
    BatchSignature, ExtendedCommitBatchInfo, SYSCOIN_DA_MAX_BLOBS_PER_BATCH,
    expected_upgrade_tx_hash_for_batch, syscoin_edge_da_refs_from_input,
};
use zksync_os_contract_interface::l1_discovery::{BatchVerificationSL, L1State};
use zksync_os_contract_interface::models::DACommitmentScheme;
use zksync_os_interface::types::BlockOutput;
use zksync_os_merkle_tree::TreeBatchOutput;
use zksync_os_network::{
    PeerVerifyBatch, PeerVerifyBatchResult, VerifyBatch, VerifyBatchOutcome, VerifyBatchResult,
};
use zksync_os_observability::{ComponentStateReporter, GenericComponentState};
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent};
use zksync_os_storage_api::{ReadFinality, ReadStateHistory};
use zksync_os_storage_api::{ReplayRecord, StateError, TreeBlock, read_multichain_root};

mod block_cache;
mod metrics;

type VerificationInput = TreeBlock;

/// Batch verification responder that consumes requests from the network.
pub struct BatchVerificationResponder<Finality, ReadState> {
    chain_id: u64,
    diamond_proxy_sl: Address,
    l1_state: L1State,
    signer: PrivateKeySigner,
    syscoin_da_verification: Option<SyscoinDaVerificationConfig>,
    block_cache: BlockCache<Finality, TreeBlock>,
    read_state: ReadState,
    verify_request_rx: mpsc::Receiver<PeerVerifyBatch>,
    outgoing_verify_results: broadcast::Sender<PeerVerifyBatchResult>,
}

#[derive(Debug, thiserror::Error)]
enum BatchVerificationError {
    #[error("Missing records for block {0}")]
    MissingBlock(u64),
    #[error("Tree error")]
    TreeError,
    #[error("Batch data mismatch")]
    BatchDataMismatch,
    #[error("Batch build error: {0}")]
    BatchBuild(String),
    #[error("State error: {0}")]
    State(#[from] StateError),
    // SYSCOIN
    #[error("Missing Syscoin DA verification config")]
    MissingSyscoinDaVerificationConfig,
    // SYSCOIN
    #[error("Invalid Syscoin DA commitment: {0}")]
    InvalidSyscoinDaCommitment(String),
    // SYSCOIN
    #[error("Syscoin DA verification failed: {0}")]
    SyscoinDaVerificationFailed(String),
}

impl<Finality: ReadFinality, ReadState: ReadStateHistory>
    BatchVerificationResponder<Finality, ReadState>
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        chain_id: u64,
        diamond_proxy_sl: Address,
        private_key: SecretString,
        syscoin_da_verification: Option<SyscoinDaVerificationConfig>,
        finality: Finality,
        l1_state: L1State,
        read_state: ReadState,
        verify_request_rx: mpsc::Receiver<PeerVerifyBatch>,
        outgoing_verify_results: broadcast::Sender<PeerVerifyBatchResult>,
    ) -> Self {
        let signer = PrivateKeySigner::from_str(private_key.expose_secret())
            .expect("Invalid batch verification private key");
        if let BatchVerificationSL::Enabled(l1_config) = l1_state.batch_verification.clone()
            && !l1_config.validators.contains(&signer.address())
        {
            tracing::warn!(
                address = %signer.address(),
                "Your address is not authorized to verify batches on L1",
            );
        }

        Self {
            chain_id,
            diamond_proxy_sl,
            l1_state,
            signer,
            syscoin_da_verification,
            block_cache: BlockCache::new(finality),
            read_state,
            verify_request_rx,
            outgoing_verify_results,
        }
    }

    async fn handle_verification_request(
        &self,
        request: VerificationRequest,
    ) -> Result<BatchSignature, BatchVerificationError> {
        tracing::info!(
            batch_number = request.batch_number,
            request_id = request.request_id,
            "Handling batch verification request (blocks {}-{})",
            request.first_block_number,
            request.last_block_number,
        );

        let blocks: Vec<(&BlockOutput, &ReplayRecord, TreeBatchOutput)> =
            (request.first_block_number..=request.last_block_number)
                .map(|block_number| {
                    let cached = self
                        .block_cache
                        .get(block_number)
                        .ok_or(BatchVerificationError::MissingBlock(block_number))?;
                    let (block_output, replay_record, tree_data) =
                        (&cached.output, &cached.record, &cached.tree);

                    let (root_hash, leaf_count) = tree_data
                        .block_end
                        .clone()
                        .root_info()
                        .map_err(|_| BatchVerificationError::TreeError)?;

                    let tree_output = TreeBatchOutput {
                        root_hash,
                        leaf_count,
                    };
                    Ok((block_output, replay_record, tree_output))
                })
                .collect::<Result<Vec<_>, BatchVerificationError>>()?;

        let state_view = self.read_state.state_view_at(request.last_block_number)?;
        let multichain_root = read_multichain_root(state_view);
        let latest = BlockId::latest();
        let upgrade_batch_number = self
            .l1_state
            .diamond_proxy_sl
            .get_upgrade_batch_number(latest)
            .await
            .unwrap_or(0);
        let upgrade_tx_hash = self
            .l1_state
            .diamond_proxy_sl
            .get_upgrade_tx_hash(latest)
            .await
            .ok()
            .filter(|hash| !hash.is_zero());
        let last_committed_batch = self
            .l1_state
            .diamond_proxy_sl
            .get_total_batches_committed(latest)
            .await
            .unwrap_or(self.l1_state.last_committed_batch);
        let expected_upgrade_tx_hash = expected_upgrade_tx_hash_for_batch(
            request.batch_number,
            last_committed_batch,
            upgrade_batch_number,
            upgrade_tx_hash,
        );

        let (batch_info, _) = ExtendedCommitBatchInfo::build(
            blocks
                .iter()
                .map(|(block_output, replay_record, tree)| {
                    (
                        *block_output,
                        &replay_record.block_context,
                        replay_record.transactions.as_slice(),
                        tree,
                    )
                })
                .collect(),
            self.chain_id,
            request.batch_number,
            request.pubdata_mode,
            self.l1_state.sl_chain_id,
            multichain_root,
            &blocks.first().unwrap().1.protocol_version,
            expected_upgrade_tx_hash,
            Some(self.l1_state.validator_timelock_sl),
        )
        .map_err(|err| BatchVerificationError::BatchBuild(err.to_string()))?;

        let expected_commit_data = normalized_commit_data(
            batch_info.commit_info.clone(),
            request.execution_protocol_version,
        );
        if expected_commit_data != request.commit_data {
            return Err(BatchVerificationError::BatchDataMismatch);
        }
        self.verify_syscoin_da_before_signing(&expected_commit_data)
            .await?;

        let signature = BatchSignature::sign_batch(
            &request.prev_commit_data,
            &batch_info.commit_info,
            self.diamond_proxy_sl,
            self.l1_state.sl_chain_id,
            self.l1_state.validator_timelock_sl,
            &blocks.first().unwrap().1.protocol_version,
            &self.signer,
        )
        .await;

        Ok(signature)
    }

    // SYSCOIN: batch-verifier signatures should not attest to a Syscoin DA batch
    // unless the batch DA blobs and compact edge DA refs are independently
    // retrievable on the DA layer.
    async fn verify_syscoin_da_before_signing(
        &self,
        commit_data: &zksync_os_contract_interface::models::CommitBatchInfo,
    ) -> Result<(), BatchVerificationError> {
        let has_batch_da = commit_data.l2_da_commitment_scheme == DACommitmentScheme::BlobsZKsyncOS;
        let has_edge_da_refs = !commit_data.edge_da_refs_input.is_empty();
        if !has_batch_da && !has_edge_da_refs {
            return Ok(());
        }

        if has_batch_da {
            if commit_data.operator_da_input.is_empty()
                || commit_data.operator_da_input.len() % 32 != 0
            {
                return Err(BatchVerificationError::InvalidSyscoinDaCommitment(
                    "operator DA input must be a non-empty array of 32-byte blob hashes"
                        .to_string(),
                ));
            }
            let blob_count = commit_data.operator_da_input.len() / 32;
            if blob_count > SYSCOIN_DA_MAX_BLOBS_PER_BATCH {
                return Err(BatchVerificationError::InvalidSyscoinDaCommitment(format!(
                    "operator DA input has {blob_count} blobs, max is {SYSCOIN_DA_MAX_BLOBS_PER_BATCH}"
                )));
            }
            let actual_commitment = keccak256(&commit_data.operator_da_input);
            if actual_commitment != commit_data.da_commitment {
                return Err(BatchVerificationError::InvalidSyscoinDaCommitment(format!(
                    "commitment mismatch: expected {}, got {}",
                    commit_data.da_commitment, actual_commitment
                )));
            }
        }

        let config = self
            .syscoin_da_verification
            .as_ref()
            .ok_or(BatchVerificationError::MissingSyscoinDaVerificationConfig)?;
        let client = SyscoinClient::new(
            &config.rpc_url,
            config.rpc_user.expose_secret(),
            config.rpc_password.expose_secret(),
            &config.poda_url,
            Some(config.request_timeout),
            &config.wallet_name,
        )
        .map_err(|err| {
            BatchVerificationError::SyscoinDaVerificationFailed(format!(
                "failed to create Bitcoin DA client: {err}"
            ))
        })?;

        if has_batch_da {
            for (idx, version_hash) in commit_data.operator_da_input.chunks_exact(32).enumerate() {
                let version_hash = alloy::hex::encode(version_hash);
                Self::verify_syscoin_blob_available(
                    &client,
                    &version_hash,
                    &format!("batch DA blob {idx}"),
                )
                .await?;
            }
        }

        if has_edge_da_refs {
            let edge_refs = syscoin_edge_da_refs_from_input(&commit_data.edge_da_refs_input)
                .ok_or_else(|| {
                    BatchVerificationError::InvalidSyscoinDaCommitment(
                        "failed to parse compact edge DA refs".to_string(),
                    )
                })?;
            for edge_ref in edge_refs {
                for (idx, version_hash) in edge_ref.blob_version_hashes.chunks_exact(32).enumerate()
                {
                    let version_hash = alloy::hex::encode(version_hash);
                    Self::verify_syscoin_blob_available(
                        &client,
                        &version_hash,
                        &format!(
                            "edge DA ref chain {}, batch {}, blob {}",
                            edge_ref.edge_chain_id, edge_ref.edge_batch_number, idx
                        ),
                    )
                    .await?;
                }
            }
        }

        Ok(())
    }

    // SYSCOIN
    async fn verify_syscoin_blob_available(
        client: &SyscoinClient,
        version_hash: &str,
        context: &str,
    ) -> Result<(), BatchVerificationError> {
        let exists = client.blob_exists(version_hash).await.map_err(|err| {
            BatchVerificationError::SyscoinDaVerificationFailed(format!(
                "failed to check {context} ({version_hash}) availability: {err}"
            ))
        })?;
        if !exists {
            return Err(BatchVerificationError::SyscoinDaVerificationFailed(
                format!("{context} ({version_hash}) is not retrievable"),
            ));
        }
        tracing::info!(
            version_hash,
            context,
            "Syscoin DA blob retrievable before batch signing"
        );
        Ok(())
    }

    async fn handle_verification_message(
        &self,
        request: VerifyBatch,
    ) -> Result<VerifyBatchResult, anyhow::Error> {
        let request_id = request.request_id;
        let batch_number = request.batch_number;
        let request = VerificationRequest::try_from(request)?;
        let result = match self.handle_verification_request(request).await {
            Ok(signature) => {
                BATCH_VERIFICATION_RESPONDER_METRICS
                    .record_request_success(request_id, batch_number);
                VerifyBatchOutcome::Approved(signature.into_raw().to_vec().into())
            }
            Err(reason) => {
                BATCH_VERIFICATION_RESPONDER_METRICS
                    .record_request_failure(request_id, batch_number);
                VerifyBatchOutcome::Refused(reason.to_string())
            }
        };
        Ok(VerifyBatchResult {
            request_id,
            batch_number,
            result,
        })
    }
}

#[async_trait]
impl<Finality: ReadFinality, ReadState: ReadStateHistory> PipelineComponent
    for BatchVerificationResponder<Finality, ReadState>
{
    type Input = VerificationInput;
    type Output = ();

    const COMPONENT_ID: zksync_os_pipeline::ComponentId =
        zksync_os_pipeline::ComponentId::BatchVerificationResponder;
    const OUTPUT_CHANNEL_CAPACITY: usize = 5;

    async fn run(
        mut self,
        mut input: PeekableReceiver<Self::Input>,
        _output: mpsc::Sender<Self::Output>,
        state_reporter: ComponentStateReporter,
    ) -> anyhow::Result<()> {
        tracing::info!("starting batch verification responder");
        loop {
            state_reporter.enter_state(GenericComponentState::Idle);
            tokio::select! {
                block = input.recv() => {
                    match block {
                        Some(tree_block) => {
                            state_reporter.enter_state(GenericComponentState::Active);
                            let block_number = tree_block.record.block_context.block_number;
                            let block_timestamp = tree_block.record.block_context.timestamp;
                            self.block_cache.insert(block_number, tree_block)?;
                            state_reporter.record_processed(block_number, Some(block_timestamp), None);
                        }
                        None => return Ok(()),
                    }
                }
                request = self.verify_request_rx.recv() => {
                    let Some(request) = request else {
                        return Ok(());
                    };
                    state_reporter.enter_state(GenericComponentState::Active);
                    let peer_id = request.peer_id;
                    let request_id = request.message.request_id;
                    let batch_number = request.message.batch_number;
                    let result = self.handle_verification_message(request.message).await?;
                    tracing::info!(%peer_id, request_id, batch_number, "handled batch verification request");
                    let _ = self.outgoing_verify_results.send(PeerVerifyBatchResult {
                        peer_id,
                        message: result,
                    });
                }
            }
        }
    }
}
