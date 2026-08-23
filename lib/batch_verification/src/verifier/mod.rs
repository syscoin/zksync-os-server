use crate::config::SyscoinDaVerificationConfig;
use crate::verifier::metrics::BATCH_VERIFICATION_RESPONDER_METRICS;
use crate::verify_batch_wire::VerificationRequest;
use alloy::primitives::{Address, keccak256};
use alloy::signers::local::PrivateKeySigner;
use async_trait::async_trait;
use bitcoin_da_client::SyscoinClient;
use block_cache::BlockCache;
use secrecy::{ExposeSecret, SecretString};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use zksync_os_batch_types::{
    BatchSignature, SYSCOIN_DA_MAX_BLOBS_PER_BATCH, syscoin_edge_da_refs_from_input,
};
use zksync_os_contract_interface::l1_discovery::{BatchVerificationSL, L1State};
use zksync_os_contract_interface::models::{BatchDaInputMode, DACommitmentScheme};
use zksync_os_merkle_tree::{MerkleTree, RocksDBWrapper};
use zksync_os_native_pig::{NativeBatchBlock, generate_batch_run};
use zksync_os_network::{
    PeerVerifyBatch, PeerVerifyBatchResult, VerifyBatch, VerifyBatchOutcome, VerifyBatchResult,
};
use zksync_os_observability::{ComponentStateReporter, GenericComponentState};
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent};
use zksync_os_storage_api::{ReadFinality, ReadStateHistory};
use zksync_os_storage_api::{StateError, TreeBlock};
use zksync_os_types::{ProvingVersion, PubdataMode};

mod block_cache;
mod metrics;

type VerificationInput = TreeBlock;

/// Batch verification responder that consumes requests from the network.
pub struct BatchVerificationResponder<Finality, ReadState> {
    chain_id: u64,
    diamond_proxy_sl: Address,
    l1_state: L1State,
    syscoin_edge_da_commit_target: Address,
    signer: PrivateKeySigner,
    syscoin_da_verification: Option<SyscoinDaVerificationConfig>,
    // `Arc` so verification requests can hand the blocks to a blocking task without
    // deep-cloning replay records and tree data.
    block_cache: BlockCache<Finality, Arc<TreeBlock>>,
    read_state: ReadState,
    merkle_tree: MerkleTree<RocksDBWrapper>,
    verify_request_rx: mpsc::Receiver<PeerVerifyBatch>,
    outgoing_verify_results: broadcast::Sender<PeerVerifyBatchResult>,
}

#[derive(Debug, thiserror::Error)]
enum BatchVerificationError {
    #[error("Missing records for block {0}")]
    MissingBlock(u64),
    #[error("Batch data mismatch")]
    BatchDataMismatch,
    #[error("Non-canonical settlement DA input mode: {0:?}")]
    NonCanonicalDaInputMode(BatchDaInputMode),
    #[error(
        "Non-canonical pubdata mode for settlement topology: expected {expected:?}, got {actual:?}"
    )]
    NonCanonicalPubdataMode {
        expected: PubdataMode,
        actual: PubdataMode,
    },
    #[error("State error: {0}")]
    State(#[from] StateError),
    // SYSCOIN: Batch-verifier attestations require configured Bitcoin DA availability checks.
    #[error("Missing Syscoin DA verification config")]
    MissingSyscoinDaVerificationConfig,
    // SYSCOIN: Reject malformed or noncanonical Bitcoin DA commitments before signing.
    #[error("Invalid Syscoin DA commitment: {0}")]
    InvalidSyscoinDaCommitment(String),
    // SYSCOIN: Surface Bitcoin DA lookup failures separately from native replay failures.
    #[error("Syscoin DA verification failed: {0}")]
    SyscoinDaVerificationFailed(String),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

// SYSCOIN: Bind the accepted DA mode to direct-L1 versus Gateway settlement topology.
fn canonical_pubdata_mode_for_settlement(
    da_input_mode: BatchDaInputMode,
    settlement_layer_address: Address,
) -> Result<PubdataMode, BatchVerificationError> {
    match da_input_mode {
        BatchDaInputMode::Rollup if settlement_layer_address.is_zero() => Ok(PubdataMode::Blobs),
        BatchDaInputMode::Rollup => Ok(PubdataMode::RelayedL2Calldata),
        mode @ BatchDaInputMode::Validium => {
            Err(BatchVerificationError::NonCanonicalDaInputMode(mode))
        }
    }
}

// SYSCOIN: Validate and enumerate every direct or compact-reference Bitcoin DA opening.
fn syscoin_da_availability_checks(
    commit_data: &zksync_os_contract_interface::models::CommitBatchInfo,
) -> Result<Vec<(String, String)>, BatchVerificationError> {
    if commit_data.l2_da_commitment_scheme != DACommitmentScheme::BlobsZKsyncOS {
        return Err(BatchVerificationError::InvalidSyscoinDaCommitment(format!(
            "expected BlobsZKsyncOS commitment scheme, got {:?}",
            commit_data.l2_da_commitment_scheme
        )));
    }
    if commit_data.operator_da_input.is_empty()
        || !commit_data.operator_da_input.len().is_multiple_of(32)
    {
        return Err(BatchVerificationError::InvalidSyscoinDaCommitment(
            "operator DA input must be a non-empty array of 32-byte blob hashes".to_string(),
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

    let mut availability_checks = commit_data
        .operator_da_input
        .chunks_exact(32)
        .enumerate()
        .map(|(idx, version_hash)| {
            (
                alloy::hex::encode(version_hash),
                format!("batch DA blob {idx}"),
            )
        })
        .collect::<Vec<_>>();

    if !commit_data.edge_da_refs_input.is_empty() {
        let edge_refs = syscoin_edge_da_refs_from_input(&commit_data.edge_da_refs_input)
            .ok_or_else(|| {
                BatchVerificationError::InvalidSyscoinDaCommitment(
                    "failed to parse compact edge DA refs".to_string(),
                )
            })?;
        for edge_ref in edge_refs {
            for (idx, version_hash) in edge_ref.blob_version_hashes.chunks_exact(32).enumerate() {
                availability_checks.push((
                    alloy::hex::encode(version_hash),
                    format!(
                        "edge DA ref chain {}, batch {}, blob {}",
                        edge_ref.edge_chain_id, edge_ref.edge_batch_number, idx
                    ),
                ));
            }
        }
    }

    Ok(availability_checks)
}

impl<Finality: ReadFinality, ReadState: ReadStateHistory + Clone>
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
        syscoin_edge_da_commit_target: Address,
        read_state: ReadState,
        merkle_tree: MerkleTree<RocksDBWrapper>,
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
            syscoin_edge_da_commit_target,
            signer,
            syscoin_da_verification,
            block_cache: BlockCache::new(finality),
            read_state,
            merkle_tree,
            verify_request_rx,
            outgoing_verify_results,
        }
    }

    async fn handle_verification_request(
        &self,
        request: VerificationRequest,
    ) -> Result<BatchSignature, BatchVerificationError> {
        let expected_pubdata_mode = self.canonical_pubdata_mode()?;
        if request.pubdata_mode != expected_pubdata_mode {
            return Err(BatchVerificationError::NonCanonicalPubdataMode {
                expected: expected_pubdata_mode,
                actual: request.pubdata_mode,
            });
        }
        tracing::info!(
            batch_number = request.batch_number,
            request_id = request.request_id,
            "Handling batch verification request (blocks {}-{})",
            request.first_block_number,
            request.last_block_number,
        );

        let blocks = (request.first_block_number..=request.last_block_number)
            .map(|block_number| {
                self.block_cache
                    .get(block_number)
                    .cloned()
                    .ok_or(BatchVerificationError::MissingBlock(block_number))
            })
            .collect::<Result<Vec<_>, BatchVerificationError>>()?;

        let protocol_version = blocks
            .first()
            .ok_or_else(|| {
                BatchVerificationError::Internal(anyhow::anyhow!("empty batch verification range"))
            })?
            .record
            .protocol_version
            .clone();
        let proving_version =
            ProvingVersion::try_from(protocol_version.clone()).map_err(anyhow::Error::from)?;
        // Native batch PIG re-executes the whole batch - run it on a blocking thread to avoid
        // stalling the async runtime.
        let native_run_blocks = blocks.clone();
        let read_state = self.read_state.clone();
        let merkle_tree = self.merkle_tree.clone();
        let pubdata_mode = request.pubdata_mode;
        let compact_edge_da_commit_target = self.syscoin_edge_da_commit_target;
        let native_batch_run = tokio::task::spawn_blocking(move || {
            let native_blocks = native_run_blocks
                .iter()
                .map(|block| NativeBatchBlock {
                    replay_record: &block.record,
                    tree_data: &block.tree,
                    block_output: &block.output,
                })
                .collect::<Vec<_>>();
            generate_batch_run(
                &native_blocks,
                &read_state,
                merkle_tree,
                pubdata_mode,
                compact_edge_da_commit_target,
            )
        })
        .await
        .map_err(anyhow::Error::from)??;
        tracing::info!(
            batch_number = request.batch_number,
            request_id = request.request_id,
            first_block_number = request.first_block_number,
            last_block_number = request.last_block_number,
            block_count = blocks.len(),
            ?protocol_version,
            ?proving_version,
            pubdata_mode = ?request.pubdata_mode,
            prover_input_words = native_batch_run.prover_input.len(),
            canonical_pubdata_bytes = native_batch_run.pubdata.len(),
            "Using native batch PIG for batch verification",
        );
        let batch_info = native_batch_run.build_batch_info(
            request.batch_number,
            request.first_block_number,
            request.last_block_number,
            request.pubdata_mode,
            &protocol_version,
            self.chain_id,
            native_batch_sl_chain_id(&self.l1_state),
        )?;

        let expected_commit_data = batch_info.commit_info.clone();
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
            &blocks.first().unwrap().record.protocol_version,
            &self.signer,
        )
        .await;

        Ok(signature)
    }

    fn canonical_pubdata_mode(&self) -> Result<PubdataMode, BatchVerificationError> {
        canonical_pubdata_mode_for_settlement(
            self.l1_state.da_input_mode,
            self.l1_state.settlement_layer_address,
        )
    }

    // SYSCOIN: batch-verifier signatures should not attest to a Syscoin DA batch
    // unless the batch DA blobs and compact edge DA refs are independently
    // retrievable on the DA layer.
    async fn verify_syscoin_da_before_signing(
        &self,
        commit_data: &zksync_os_contract_interface::models::CommitBatchInfo,
    ) -> Result<(), BatchVerificationError> {
        let availability_checks = syscoin_da_availability_checks(commit_data)?;

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

        Self::verify_syscoin_blobs_available(&client, &availability_checks).await?;

        Ok(())
    }

    // SYSCOIN: Query Bitcoin DA in bounded groups matching the per-batch blob limit.
    async fn verify_syscoin_blobs_available(
        client: &SyscoinClient,
        availability_checks: &[(String, String)],
    ) -> Result<(), BatchVerificationError> {
        for chunk in availability_checks.chunks(SYSCOIN_DA_MAX_BLOBS_PER_BATCH) {
            let version_hashes = chunk.iter().map(|(version_hash, _)| version_hash);
            let existence = client.blobs_exist(version_hashes).await.map_err(|err| {
                BatchVerificationError::SyscoinDaVerificationFailed(format!(
                    "failed to check Syscoin DA availability: {err}"
                ))
            })?;
            if existence.len() != chunk.len() {
                return Err(BatchVerificationError::SyscoinDaVerificationFailed(
                    format!(
                        "Syscoin DA availability response length mismatch: requested {}, got {}",
                        chunk.len(),
                        existence.len()
                    ),
                ));
            }
            for ((version_hash, context), exists) in chunk.iter().zip(existence) {
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
            }
        }
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

/// SYSCOIN: Returns the chain ID that the batch program binds as its settlement layer.
///
/// On a Gateway topology this intentionally differs from the L1 chain ID. Keep this helper at
/// the native-batch call site so verification cannot accidentally replay a Gateway batch using
/// the parent L1 identity while signing it with the settlement-layer identity.
fn native_batch_sl_chain_id(l1_state: &L1State) -> u64 {
    l1_state.sl_chain_id
}

#[async_trait]
impl<Finality: ReadFinality, ReadState: ReadStateHistory + Clone> PipelineComponent
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
                            self.block_cache.insert(block_number, Arc::new(tree_block))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{DummyFinality, dummy_commit_batch_info};
    use crate::verify_batch_wire::encode_verify_batch_request;
    use alloy::consensus::{Header, Sealable};
    use alloy::eips::eip1559::INITIAL_BASE_FEE;
    use alloy::network::EthereumWallet;
    use alloy::primitives::{Address, B256, U256, address, keccak256};
    use alloy::providers::ProviderBuilder;
    use alloy::transports::mock::Asserter;
    use blake2::{Blake2s256, Digest};
    use httpmock::Method::POST;
    use httpmock::{HttpMockRequest, HttpMockResponse, MockServer};
    use serde_json::{Value, json};
    use std::collections::{BTreeMap, HashMap};
    use std::ops::RangeInclusive;
    use std::path::PathBuf;
    use std::str::FromStr;
    use std::sync::Arc;
    use zksync_os_batch_types::BlockMerkleTreeData;
    use zksync_os_batch_types::PendingBatchInfo;
    use zksync_os_batch_types::batcher_model::{BatchEnvelope, BatchMetadata, ProverInput};
    use zksync_os_contract_interface::models::{BatchDaInputMode, StoredBatchInfo};
    use zksync_os_contract_interface::settlement_layer_intervals::SettlementLayerIntervals;
    use zksync_os_contract_interface::{Bridgehub, ZkChain};
    use zksync_os_genesis::{FileGenesisInputSource, GenesisState, build_genesis};
    use zksync_os_interface::traits::{PreimageSource, ReadStorage};
    use zksync_os_merkle_tree::{MerkleTree, RocksDBWrapper, TreeBatchOutput, TreeEntry};
    use zksync_os_merkle_tree_api::BatchTreeProof;
    use zksync_os_provider::NodeProvider;
    use zksync_os_storage_api::{
        BlockContext, BlockHashes, ReplayRecord, StateError, read_multichain_root,
    };
    use zksync_os_types::{
        BlockOutput, BlockPubdata, BlockStartCursors, ExecutionVersion, ProtocolSemanticVersion,
        PubdataMode, SystemTxEnvelope, ZkTransaction,
    };

    const CHAIN_ID: u64 = 270;
    const SL_CHAIN_ID: u64 = 9;
    const BATCH_NUMBER: u64 = 1;
    const REQUEST_ID: u64 = 4242;
    const PRIVATE_KEY: &str = "0x7726827caac94a7f9e1b160f7ea819f172f7b6f9d2a97f992c38edeab82d4110";
    const DIAMOND_PROXY_SL: Address = address!("0x00000000000000000000000000000000000000d1");
    const VALIDATOR_TIMELOCK: Address = address!("0x00000000000000000000000000000000000000e1");

    #[derive(Clone, Debug)]
    struct MemoryStateView {
        storage: Arc<HashMap<B256, B256>>,
        preimages: Arc<HashMap<B256, Vec<u8>>>,
    }

    impl ReadStorage for MemoryStateView {
        fn read(&mut self, key: B256) -> Option<B256> {
            self.storage.get(&key).copied()
        }
    }

    impl PreimageSource for MemoryStateView {
        fn get_preimage(&mut self, hash: B256) -> Option<Vec<u8>> {
            self.preimages.get(&hash).cloned()
        }
    }

    #[derive(Clone, Debug)]
    struct MemoryStateHistory {
        view: MemoryStateView,
        block_range: RangeInclusive<u64>,
    }

    impl MemoryStateHistory {
        fn from_genesis_state(genesis_state: &GenesisState) -> Self {
            let storage = genesis_state
                .storage_logs
                .iter()
                .copied()
                .collect::<HashMap<_, _>>();
            let preimages = genesis_state
                .preimages
                .iter()
                .cloned()
                .collect::<HashMap<_, _>>();

            Self {
                view: MemoryStateView {
                    storage: Arc::new(storage),
                    preimages: Arc::new(preimages),
                },
                block_range: 0..=1,
            }
        }
    }

    impl ReadStateHistory for MemoryStateHistory {
        fn state_view_at(
            &self,
            block_number: u64,
        ) -> Result<impl zksync_os_storage_api::ViewState, StateError> {
            if self.block_range.contains(&block_number) {
                Ok(self.view.clone())
            } else {
                Err(StateError::NotFound(block_number))
            }
        }

        fn block_range_available(&self) -> RangeInclusive<u64> {
            self.block_range.clone()
        }
    }

    #[test]
    fn verifier_pubdata_mode_is_topology_bound() {
        assert_eq!(
            canonical_pubdata_mode_for_settlement(BatchDaInputMode::Rollup, Address::ZERO).unwrap(),
            PubdataMode::Blobs
        );
        assert_eq!(
            canonical_pubdata_mode_for_settlement(
                BatchDaInputMode::Rollup,
                address!("0x0000000000000000000000000000000000000001"),
            )
            .unwrap(),
            PubdataMode::RelayedL2Calldata
        );
        assert!(matches!(
            canonical_pubdata_mode_for_settlement(BatchDaInputMode::Validium, Address::ZERO),
            Err(BatchVerificationError::NonCanonicalDaInputMode(
                BatchDaInputMode::Validium
            ))
        ));
    }

    #[tokio::test]
    async fn native_batch_binding_uses_settlement_layer_chain_id() {
        let l1_state = test_l1_state().await;
        assert_ne!(l1_state.l1_chain_id, l1_state.sl_chain_id);
        assert_eq!(native_batch_sl_chain_id(&l1_state), SL_CHAIN_ID);
    }

    #[test]
    fn verifier_rejects_noncanonical_syscoin_da_commitments() {
        let canonical = dummy_commit_batch_info(BATCH_NUMBER, 1, 1);
        assert_eq!(syscoin_da_availability_checks(&canonical).unwrap().len(), 1);

        let mut commit = canonical.clone();
        commit.l2_da_commitment_scheme = DACommitmentScheme::EmptyNoDA;
        assert!(matches!(
            syscoin_da_availability_checks(&commit),
            Err(BatchVerificationError::InvalidSyscoinDaCommitment(_))
        ));

        let mut commit = canonical.clone();
        commit.operator_da_input.clear();
        assert!(matches!(
            syscoin_da_availability_checks(&commit),
            Err(BatchVerificationError::InvalidSyscoinDaCommitment(_))
        ));

        let mut commit = canonical.clone();
        commit.operator_da_input = vec![0; 31];
        assert!(matches!(
            syscoin_da_availability_checks(&commit),
            Err(BatchVerificationError::InvalidSyscoinDaCommitment(_))
        ));

        let mut commit = canonical.clone();
        commit.operator_da_input = vec![0; (SYSCOIN_DA_MAX_BLOBS_PER_BATCH + 1) * 32];
        commit.da_commitment = keccak256(&commit.operator_da_input);
        assert!(matches!(
            syscoin_da_availability_checks(&commit),
            Err(BatchVerificationError::InvalidSyscoinDaCommitment(_))
        ));

        let mut commit = canonical.clone();
        commit.da_commitment = B256::ZERO;
        assert!(matches!(
            syscoin_da_availability_checks(&commit),
            Err(BatchVerificationError::InvalidSyscoinDaCommitment(_))
        ));

        let mut commit = canonical;
        commit.edge_da_refs_input = vec![1];
        assert!(matches!(
            syscoin_da_availability_checks(&commit),
            Err(BatchVerificationError::InvalidSyscoinDaCommitment(_))
        ));
    }

    #[tokio::test]
    #[ignore = "requires the regenerated canonical v32.0/V8 genesis state"]
    async fn v8_verifier_approves_batch_built_from_native_run() {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let genesis_state = build_genesis_state_for_test(&protocol_version).await;
        let read_state = MemoryStateHistory::from_genesis_state(&genesis_state);

        let temp_dir = tempfile::tempdir().unwrap();
        let tree = genesis_tree(&genesis_state, temp_dir.path());
        let prev_batch_info = genesis_stored_batch_info(&genesis_state, &tree);
        let tree_block = empty_tree_block(&tree, protocol_version.clone());

        let batch_envelope = v8_batch_for_signing(
            &tree_block,
            prev_batch_info,
            &read_state,
            &tree,
            protocol_version.clone(),
        );
        let request = encode_verify_batch_request(&batch_envelope, REQUEST_ID).unwrap();

        let bitcoin_da_mock = MockServer::start();
        bitcoin_da_mock.mock(|when, then| {
            when.method(POST);
            then.respond_with(|req: &HttpMockRequest| {
                HttpMockResponse::builder()
                    .status(200)
                    .header("content-type", "application/json")
                    .body(handle_bitcoin_da_rpc(&req.body_string()))
                    .build()
            });
        });
        let da_url = bitcoin_da_mock.base_url();
        let da_config = SyscoinDaVerificationConfig {
            rpc_url: da_url.clone(),
            rpc_user: SecretString::from("user".to_owned()),
            rpc_password: SecretString::from("password".to_owned()),
            poda_url: da_url,
            wallet_name: "zksync-os".to_owned(),
            request_timeout: std::time::Duration::from_secs(2),
        };

        let (_verify_request_tx, verify_request_rx) = mpsc::channel(1);
        let (outgoing_verify_results, _) = broadcast::channel(1);
        let mut responder = BatchVerificationResponder::new(
            CHAIN_ID,
            DIAMOND_PROXY_SL,
            SecretString::from(PRIVATE_KEY.to_owned()),
            Some(da_config),
            DummyFinality::zero(),
            test_l1_state().await,
            Address::ZERO,
            read_state.clone(),
            tree.clone(),
            verify_request_rx,
            outgoing_verify_results,
        );
        responder
            .block_cache
            .insert(1, Arc::new(tree_block))
            .unwrap();

        let mut wrong_topology_request = request.clone();
        wrong_topology_request.pubdata_mode = PubdataMode::RelayedL2Calldata.to_u8();
        let wrong_topology_result = responder
            .handle_verification_message(wrong_topology_request)
            .await
            .unwrap();
        assert!(matches!(
            wrong_topology_result.result,
            VerifyBatchOutcome::Refused(reason)
                if reason.contains("Non-canonical pubdata mode for settlement topology")
        ));

        responder.l1_state.da_input_mode = BatchDaInputMode::Validium;
        let validium_result = responder
            .handle_verification_message(request.clone())
            .await
            .unwrap();
        assert!(matches!(
            validium_result.result,
            VerifyBatchOutcome::Refused(reason)
                if reason.contains("Non-canonical settlement DA input mode")
        ));
        responder.l1_state.da_input_mode = BatchDaInputMode::Rollup;

        let result = responder
            .handle_verification_message(request)
            .await
            .unwrap();

        assert_eq!(result.request_id, REQUEST_ID);
        assert_eq!(result.batch_number, BATCH_NUMBER);

        let signature = match result.result {
            VerifyBatchOutcome::Approved(signature) => {
                let signature: [u8; 65] = signature.as_ref().try_into().unwrap();
                BatchSignature::from_raw_array(&signature).unwrap()
            }
            VerifyBatchOutcome::Refused(reason) => panic!("verification refused: {reason}"),
        };

        let validated = signature
            .verify_signature(
                &batch_envelope.batch.previous_stored_batch_info,
                &batch_envelope.batch.batch_info.commit_info,
                DIAMOND_PROXY_SL,
                SL_CHAIN_ID,
                VALIDATOR_TIMELOCK,
                &protocol_version,
            )
            .unwrap();
        let expected_signer = PrivateKeySigner::from_str(PRIVATE_KEY).unwrap().address();
        assert_eq!(*validated.signer(), expected_signer);
    }

    fn handle_bitcoin_da_rpc(body: &str) -> String {
        let request: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let response = if let Some(calls) = request.as_array() {
            Value::Array(calls.iter().map(handle_bitcoin_da_call).collect())
        } else {
            handle_bitcoin_da_call(&request)
        };
        response.to_string()
    }

    fn handle_bitcoin_da_call(call: &Value) -> Value {
        let id = call.get("id").cloned().unwrap_or(Value::Null);
        let version_hash = call
            .get("params")
            .and_then(Value::as_array)
            .and_then(|params| params.first())
            .and_then(Value::as_str)
            .unwrap_or_default();
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "versionhash": version_hash,
                "txid": "syscoin-da-test-txid"
            },
            "error": null
        })
    }

    /// The server-side V8 batch public-input reconstruction (used to verify V8 FRI proofs in
    /// `fri_proof_verifier::verify_fri_proof_v8`) must match the public input the zksync-os
    /// 0.4.0 batch program computes natively:
    /// `keccak(state_before || state_after || chain_config_hash || batch_output)`.
    #[tokio::test]
    #[ignore = "requires the regenerated canonical v32.0/V8 genesis state"]
    async fn v8_public_input_reconstruction_matches_native_run() {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let genesis_state = build_genesis_state_for_test(&protocol_version).await;
        let read_state = MemoryStateHistory::from_genesis_state(&genesis_state);

        let temp_dir = tempfile::tempdir().unwrap();
        let tree = genesis_tree(&genesis_state, temp_dir.path());
        let tree_block = empty_tree_block(&tree, protocol_version.clone());

        let native_batch_run = generate_batch_run(
            &[NativeBatchBlock {
                replay_record: &tree_block.record,
                tree_data: &tree_block.tree,
                block_output: &tree_block.output,
            }],
            &read_state,
            tree.clone(),
            PubdataMode::Blobs,
            Address::ZERO,
        )
        .unwrap();

        let batch_info = PendingBatchInfo::build_from_canonical_output(
            BATCH_NUMBER,
            PubdataMode::Blobs,
            &protocol_version,
            native_batch_run.canonical_commit_data(1, 1),
        )
        .unwrap();

        let chain_config_hash =
            zksync_os_native_pig::chain_config_hash(batch_info.commit_info.chain_id).unwrap();
        let reconstructed = keccak256(
            [
                native_batch_run.previous_state_commitment.0,
                batch_info.commit_info.new_state_commitment.0,
                chain_config_hash.0,
                batch_info.batch_output_hash().0,
            ]
            .concat(),
        );

        assert_eq!(
            reconstructed, native_batch_run.batch_public_input_hash,
            "server-side V8 public input reconstruction diverges from the batch program"
        );
    }

    /// Utility (not a real test): runs the V8 native batch PIG for the simplest possible batch
    /// (a single empty block at canonical protocol v32.0) and dumps the resulting prover input in the
    /// formats the `zksync-airbender` CLI understands, so it can be proven/verified on CPU
    /// elsewhere (e.g. `cli prove --bin multiblock_batch.bin --input-file <hex> --backend cpu`).
    ///
    /// Run with:
    ///   V8_PROVER_INPUT_OUT=/tmp/v8-prover-input \
    ///   cargo test -p zksync_os_batch_verification dump_v8_simplest_batch_prover_input \
    ///     -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "utility: dumps the V8 simplest-batch prover input to files"]
    async fn dump_v8_simplest_batch_prover_input() {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let genesis_state = build_genesis_state_for_test(&protocol_version).await;
        let read_state = MemoryStateHistory::from_genesis_state(&genesis_state);

        let temp_dir = tempfile::tempdir().unwrap();
        let tree = genesis_tree(&genesis_state, temp_dir.path());
        let tree_block = empty_tree_block(&tree, protocol_version.clone());

        let native_batch_run = generate_batch_run(
            &[NativeBatchBlock {
                replay_record: &tree_block.record,
                tree_data: &tree_block.tree,
                block_output: &tree_block.output,
            }],
            &read_state,
            tree.clone(),
            PubdataMode::Blobs,
            Address::ZERO,
        )
        .expect("V8 native batch run failed");

        let words = native_batch_run.prover_input;

        let out_dir = std::env::var("V8_PROVER_INPUT_OUT")
            .expect("set V8_PROVER_INPUT_OUT to the output directory for the dumped files");
        std::fs::create_dir_all(&out_dir).unwrap();

        // `--input-type hex` (the CLI default): each u32 word as 8 lowercase hex chars, concatenated.
        let hex: String = words.iter().map(|w| format!("{w:08x}")).collect();
        let hex_path = format!("{out_dir}/v8_simplest_prover_input.hex");
        std::fs::write(&hex_path, &hex).unwrap();

        // Raw little-endian words (useful for other tooling / re-encoding as base64 prover-input-json).
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let bin_path = format!("{out_dir}/v8_simplest_prover_input.le.bin");
        std::fs::write(&bin_path, &bytes).unwrap();

        println!("=== V8 simplest-batch prover input ===");
        println!("protocol_version: v32.0  proving_version: V8  pubdata_mode: Blobs");
        println!(
            "prover_input words: {}  ({} bytes)",
            words.len(),
            bytes.len()
        );
        println!("first words: {:?}", &words[..words.len().min(8)]);
        println!(
            "new_state_commitment: {:?}",
            native_batch_run.new_state_commitment
        );
        println!("da_commitment:        {:?}", native_batch_run.da_commitment);
        println!("wrote hex : {hex_path}");
        println!("wrote bin : {bin_path}");
    }

    fn v8_batch_for_signing<ReadState: ReadStateHistory>(
        tree_block: &TreeBlock,
        prev_batch_info: StoredBatchInfo,
        read_state: &ReadState,
        tree: &MerkleTree<RocksDBWrapper>,
        protocol_version: ProtocolSemanticVersion,
    ) -> zksync_os_batch_types::batcher_model::BatchForSigning<ProverInput> {
        let native_batch_run = generate_batch_run(
            &[NativeBatchBlock {
                replay_record: &tree_block.record,
                tree_data: &tree_block.tree,
                block_output: &tree_block.output,
            }],
            read_state,
            tree.clone(),
            PubdataMode::Blobs,
            Address::ZERO,
        )
        .unwrap();
        let batch_info = PendingBatchInfo::build_from_canonical_output(
            BATCH_NUMBER,
            PubdataMode::Blobs,
            &protocol_version,
            native_batch_run.canonical_commit_data(1, 1),
        )
        .unwrap();

        let multichain_root = read_multichain_root(read_state.state_view_at(1).unwrap());

        BatchEnvelope::new(
            BatchMetadata {
                previous_stored_batch_info: prev_batch_info,
                batch_info,
                chain_address: DIAMOND_PROXY_SL,
                first_block_number: 1,
                last_block_number: 1,
                last_block_hash: Some(tree_block.output.header.hash()),
                pubdata_mode: PubdataMode::Blobs,
                tx_count: tree_block.output.tx_results.len(),
                computational_native_used: Some(tree_block.output.computational_native_used),
                logs: vec![],
                messages: vec![],
                multichain_root,
                set_sl_chain_id_migration_number: None,
            },
            ProverInput::Real(native_batch_run.prover_input),
        )
    }

    fn empty_tree_block(
        tree: &MerkleTree<RocksDBWrapper>,
        protocol_version: ProtocolSemanticVersion,
    ) -> TreeBlock {
        let (root_hash, leaf_count) = tree.root_info(0).unwrap().unwrap();
        let tree_output = TreeBatchOutput {
            root_hash,
            leaf_count,
        };

        TreeBlock {
            output: empty_block_output(),
            record: empty_replay_record(protocol_version),
            tree: BlockMerkleTreeData {
                input: tree_output,
                output: TreeBatchOutput {
                    root_hash,
                    leaf_count,
                },
                written_keys: vec![],
                read_keys: vec![],
                proof: BatchTreeProof {
                    operations: vec![],
                    read_operations: vec![],
                    sorted_leaves: BTreeMap::new(),
                    hashes: vec![],
                },
            },
        }
    }

    fn empty_block_output() -> BlockOutput {
        let header = Header {
            number: 1,
            timestamp: 1,
            ..Default::default()
        }
        .seal_slow();

        BlockOutput {
            header,
            tx_results: vec![],
            storage_writes: vec![],
            account_diffs: vec![],
            published_preimages: vec![],
            pubdata: BlockPubdata::new(0),
            computational_native_used: 0,
        }
    }

    fn empty_replay_record(protocol_version: ProtocolSemanticVersion) -> ReplayRecord {
        ReplayRecord::new(
            BlockContext {
                chain_id: CHAIN_ID,
                block_number: 1,
                block_hashes: BlockHashes::default(),
                timestamp: 1,
                eip1559_basefee: U256::from(INITIAL_BASE_FEE),
                pubdata_price: U256::ZERO,
                native_price: U256::ONE,
                coinbase: Address::ZERO,
                gas_limit: 100_000_000,
                pubdata_limit: 100_000_000,
                mix_hash: U256::ZERO,
                execution_version: ExecutionVersion::V7 as u32,
                blob_fee: U256::ONE,
            },
            // A fresh chain establishes the settlement-layer chain id in its first block
            // (see the sequencer's SetSLChainId injection); the V8 batch program reads it
            // from state, and batch-info construction cross-checks it against the node.
            vec![ZkTransaction::from(SystemTxEnvelope::set_sl_chain_id(
                SL_CHAIN_ID,
                u64::MAX,
            ))],
            0,
            semver::Version::new(0, 0, 0),
            protocol_version,
            B256::ZERO,
            vec![],
            BlockStartCursors::default(),
        )
    }

    async fn test_l1_state() -> L1State {
        let asserter = Asserter::new();
        let provider = ProviderBuilder::new()
            .disable_recommended_fillers()
            .wallet(EthereumWallet::default())
            .connect_mocked_client(asserter.clone());
        let provider = NodeProvider::new(provider).await.unwrap();

        let diamond_proxy_l1 = ZkChain::new(
            address!("0x00000000000000000000000000000000000000c1"),
            provider.clone(),
        );
        let bridgehub_l1 = Bridgehub::new(
            address!("0x00000000000000000000000000000000000000a1"),
            provider.clone(),
            CHAIN_ID,
        );

        L1State {
            bridgehub_l1: bridgehub_l1.clone(),
            bridgehub_sl: bridgehub_l1,
            diamond_proxy_l1: diamond_proxy_l1.clone(),
            diamond_proxy_sl: diamond_proxy_l1.clone(),
            validator_timelock_sl: VALIDATOR_TIMELOCK,
            batch_verification: BatchVerificationSL::Disabled,
            last_committed_batch: 0,
            last_proved_batch: 0,
            last_executed_batch: 0,
            last_finalized_executed_batch: 0,
            sl_block_number: 0,
            finalized_sl_block_number: 0,
            da_input_mode: BatchDaInputMode::Rollup,
            l1_chain_id: CHAIN_ID,
            sl_chain_id: SL_CHAIN_ID,
            settlement_layer_address: Address::ZERO,
            settlement_layer_intervals: SettlementLayerIntervals::direct_l1(diamond_proxy_l1),
        }
    }

    async fn build_genesis_state_for_test(
        protocol_version: &ProtocolSemanticVersion,
    ) -> GenesisState {
        let genesis_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../local-chains/v32.0/genesis.json");
        let source = FileGenesisInputSource::new(genesis_path);
        build_genesis(&source, CHAIN_ID, protocol_version)
            .await
            .unwrap()
    }

    fn genesis_tree(
        genesis_state: &GenesisState,
        path: &std::path::Path,
    ) -> MerkleTree<RocksDBWrapper> {
        let db = RocksDBWrapper::new(path).unwrap();
        let mut tree = MerkleTree::new(db).unwrap();
        let tree_entries = genesis_state
            .storage_logs
            .iter()
            .map(|(key, value)| TreeEntry {
                key: *key,
                value: *value,
            })
            .collect::<Vec<_>>();
        tree.extend(&tree_entries).unwrap();
        tree
    }

    fn genesis_stored_batch_info(
        genesis_state: &GenesisState,
        tree: &MerkleTree<RocksDBWrapper>,
    ) -> StoredBatchInfo {
        let (genesis_root_hash, genesis_root_leaves) = tree.root_info(0).unwrap().unwrap();

        let last_256_block_hashes_blake = {
            let mut blocks_hasher = Blake2s256::new();
            for _ in 0..255 {
                blocks_hasher.update([0u8; 32]);
            }
            blocks_hasher.update(genesis_state.header.hash());
            blocks_hasher.finalize()
        };

        let mut hasher = Blake2s256::new();
        hasher.update(genesis_root_hash.as_slice());
        hasher.update(genesis_root_leaves.to_be_bytes());
        hasher.update(0u64.to_be_bytes());
        hasher.update(last_256_block_hashes_blake);
        hasher.update(0u64.to_be_bytes());
        let state_commitment = B256::from_slice(&hasher.finalize());

        assert_eq!(genesis_state.expected_genesis_root, state_commitment);

        StoredBatchInfo {
            batch_number: 0,
            state_commitment,
            number_of_layer1_txs: 0,
            priority_operations_hash: keccak256([]),
            dependency_roots_rolling_hash: B256::ZERO,
            l2_to_l1_logs_root_hash: B256::ZERO,
            commitment: B256::from(U256::ONE.to_be_bytes()),
            last_block_timestamp: Some(0),
        }
    }
}
