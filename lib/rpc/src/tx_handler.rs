use crate::eth_impl::build_api_receipt;
use crate::metrics::{TX_SUBMISSION, TxRejectionReason};
use crate::{ReadRpcStorage, RpcConfig};
use alloy::consensus::Transaction;
use alloy::consensus::transaction::SignerRecoverable;
use alloy::eips::Decodable2718;
use alloy::hex;
use alloy::primitives::{Address, B256, Bytes, U256, keccak256};
use alloy::providers::{DynProvider, Provider};
use alloy::sol_types::SolCall;
use alloy::transports::{RpcError, TransportErrorKind};
use bitcoin_da_client::SyscoinClient;
use std::time::{Duration, Instant};
use tokio::sync::watch;
use zksync_os_contract_interface::calldata::CommitCalldata;
use zksync_os_contract_interface::models::DACommitmentScheme;
use zksync_os_contract_interface::{IExecutor, IMultisigCommitter};
use zksync_os_mempool::PoolError;
use zksync_os_mempool::subpools::l2::L2Subpool;
use zksync_os_mempool::{InvalidPoolTransactionError, PoolErrorKind};
use zksync_os_rpc_api::types::ZkTransactionReceipt;
use zksync_os_types::{L2Envelope, L2Transaction, NotAcceptingReason, TransactionAcceptanceState};

/// Maximum user provided timeout for `eth_sendRawTransactionSync`. Chosen liberally as waiting is
/// inexpensive.
const SEND_RAW_TRANSACTION_SYNC_MAX_TIMEOUT: Duration = Duration::from_secs(30);
// SYSCOIN: Bitcoin DA supports up to 32 compact blob hashes per edge batch.
const SYSCOIN_DA_MAX_BLOBS_PER_BATCH: usize = 32;

/// Handles transactions received in API
pub struct TxHandler<RpcStorage, Mempool> {
    config: RpcConfig,
    storage: RpcStorage,
    mempool: Mempool,
    acceptance_state: watch::Receiver<TransactionAcceptanceState>,
    tx_forwarder: Option<DynProvider>,
}

impl<RpcStorage: ReadRpcStorage, Mempool: L2Subpool> TxHandler<RpcStorage, Mempool> {
    pub fn new(
        config: RpcConfig,
        storage: RpcStorage,
        mempool: Mempool,
        acceptance_state: watch::Receiver<TransactionAcceptanceState>,
        tx_forwarder: Option<DynProvider>,
    ) -> Self {
        Self {
            config,
            storage,
            mempool,
            acceptance_state,
            tx_forwarder,
        }
    }

    pub async fn send_raw_transaction_impl(
        &self,
        tx_bytes: Bytes,
    ) -> Result<B256, EthSendRawTransactionError> {
        if let TransactionAcceptanceState::NotAccepting(reasons) = &*self.acceptance_state.borrow()
        {
            return Err(EthSendRawTransactionError::NotAcceptingTransactions(
                reasons.clone(),
            ));
        }

        let transaction = L2Envelope::decode_2718(&mut tx_bytes.as_ref())
            .map_err(|_| EthSendRawTransactionError::FailedToDecodeSignedTransaction)?;
        let l2_tx: L2Transaction = transaction
            .try_into_recovered()
            .map_err(|_| EthSendRawTransactionError::InvalidTransactionSignature)?;
        let hash = *l2_tx.hash();
        if self.config.l2_tx_blacklist.contains(&hash) {
            return Err(EthSendRawTransactionError::BlacklistedTransaction);
        }
        if self.config.l2_signer_blacklist.contains(&l2_tx.signer()) {
            return Err(EthSendRawTransactionError::BlacklistedSigner);
        }
        // SYSCOIN: run local mempool validation before external DA admission checks, but only for
        // compact-edge-DA commit candidates. Ordinary txs keep the single validation performed by
        // insertion below.
        if self.is_compact_edge_da_admission_candidate(&l2_tx) {
            if self.mempool.contains(&hash) {
                return Err(PoolError::new(hash, PoolErrorKind::AlreadyImported).into());
            }
            self.mempool.validate_l2_transaction(l2_tx.clone()).await?;
            self.verify_compact_edge_da_refs_available(&l2_tx).await?;
        }
        {
            let _guard = MempoolLatencyGuard::new();
            self.mempool.add_l2_transaction(l2_tx).await?;
        }

        if let Some(tx_forwarder) = self.tx_forwarder.as_ref() {
            let forwarding_result = {
                let _guard = ForwardingLatencyGuard::new();
                tx_forwarder.send_raw_transaction(&tx_bytes).await
            };
            // We do not need to wait for pending transaction here, so it's safe to forget about it
            if let Err(err) = forwarding_result {
                tracing::debug!(%err, "forwarding error from main node back to user");
                // SYSCOIN: keep EN pending state consistent when the main node already knows the
                // transaction or when the forwarding result is ambiguous after local acceptance.
                if forwarding_error_indicates_main_node_already_knows_tx(&err) {
                    tracing::debug!(%err, %hash, "main node already knows forwarded transaction");
                    return Ok(hash);
                }
                if forwarding_error_should_rollback_local_tx(&err) {
                    self.mempool
                        .remove_forwarding_rollback_transactions(vec![hash]);
                }
                return Err(err.into());
            }
        }

        Ok(hash)
    }
    // SYSCOIN: verify compact edge DA refs emitted by chains settling to Gateway.
    async fn verify_compact_edge_da_refs_available(
        &self,
        tx: &L2Transaction,
    ) -> Result<(), EthSendRawTransactionError> {
        let Some(config) = self.config.edge_da_admission.as_ref() else {
            return Ok(());
        };
        if !is_compact_edge_da_commit_target(tx.to(), config.commit_tx_target) {
            return Ok(());
        }
        let Some(version_hashes) = compact_edge_da_refs_from_commit_calldata(tx.input())? else {
            return Ok(());
        };

        let client = SyscoinClient::new(
            &config.rpc_url,
            &config.rpc_user,
            &config.rpc_password,
            &config.poda_url,
            Some(config.request_timeout),
            &config.wallet_name,
        )
        .map_err(|err| {
            EthSendRawTransactionError::EdgeDaAdmissionCheckFailed(format!(
                "failed to create Bitcoin DA client: {err}"
            ))
        })?;
        for (idx, version_hash) in version_hashes.iter().enumerate() {
            let exists = client
                .blob_exists(version_hash)
                .await
                .map_err(|err| {
                    EthSendRawTransactionError::EdgeDaAdmissionCheckFailed(format!(
                        "failed to check Bitcoin DA availability for compact edge ref {idx} ({version_hash}): {err}"
                    ))
                })?;
            if !exists {
                return Err(EthSendRawTransactionError::EdgeDaAdmissionCheckFailed(
                    format!("compact edge DA ref {idx} ({version_hash}) is not retrievable"),
                ));
            }
        }

        Ok(())
    }

    // SYSCOIN: use only cheap structural checks here so invalid raw txs cannot force DA parsing or
    // external Bitcoin DA calls before the mempool validator rejects them.
    fn is_compact_edge_da_admission_candidate(&self, tx: &L2Transaction) -> bool {
        let Some(config) = self.config.edge_da_admission.as_ref() else {
            return false;
        };
        is_compact_edge_da_commit_target(tx.to(), config.commit_tx_target)
            && has_compact_edge_da_commit_selector(tx.input())
    }

    pub async fn send_raw_transaction_sync_impl(
        &self,
        bytes: Bytes,
        max_wait_ms: Option<U256>,
    ) -> Result<ZkTransactionReceipt, EthSendRawTransactionSyncError> {
        let timeout_duration = if let Some(timeout_ms) = max_wait_ms {
            match timeout_ms.try_into() {
                Ok(timeout_u64) => {
                    let requested_timeout = Duration::from_millis(timeout_u64);
                    if requested_timeout > SEND_RAW_TRANSACTION_SYNC_MAX_TIMEOUT {
                        // Per EIP-7966 MUST use default timeout if user provided timeout is invalid
                        self.config.send_raw_transaction_sync_timeout
                    } else {
                        requested_timeout
                    }
                }
                Err(_) => {
                    // Per EIP-7966 MUST use default timeout if user provided timeout is invalid
                    self.config.send_raw_transaction_sync_timeout
                }
            }
        } else {
            self.config.send_raw_transaction_sync_timeout
        };

        // Create block subscription
        let mut block_rx = self.storage.block_subscriptions().subscribe_to_blocks();

        let tx_hash = self.send_raw_transaction_impl(bytes).await?;

        // Wait for the transaction to appear in a block or timeout
        tokio::time::timeout(timeout_duration, async {
            loop {
                // Wait for the next block notification
                let Ok(block) = block_rx.recv().await else {
                    // Channel closed or is lagging, this shouldn't happen in normal operation
                    tracing::warn!("block subscription closed while waiting for tx receipt");
                    return Err(EthSendRawTransactionSyncError::Timeout(timeout_duration));
                };

                let mut l2_to_l1_logs_before_this_tx = 0;
                for block_tx_hash in &block.block.body.transactions {
                    let Some(stored_tx) = block.transactions.get(block_tx_hash) else {
                        continue;
                    };
                    if *block_tx_hash == tx_hash {
                        return Ok(build_api_receipt(
                            tx_hash,
                            stored_tx.receipt.clone(),
                            &stored_tx.tx,
                            &stored_tx.meta,
                            l2_to_l1_logs_before_this_tx,
                        ));
                    }
                    l2_to_l1_logs_before_this_tx += stored_tx.receipt.l2_to_l1_logs().len() as u64;
                }
            }
        })
        .await
        .map_err(|_| EthSendRawTransactionSyncError::Timeout(timeout_duration))?
    }
}

// SYSCOIN: only Gateway validator timelock commit transactions can carry compact edge DA refs.
fn is_compact_edge_da_commit_target(tx_to: Option<Address>, commit_tx_target: Address) -> bool {
    tx_to == Some(commit_tx_target)
}

// SYSCOIN: keep this deliberately cheap; full calldata decoding happens only after mempool
// validation for commit candidates.
fn has_compact_edge_da_commit_selector(input: &[u8]) -> bool {
    input.len() >= 4
        && (input[..4] == IExecutor::commitBatchesSharedBridgeCall::SELECTOR
            || input[..4] == IMultisigCommitter::commitBatchesMultisigCall::SELECTOR)
}

// SYSCOIN: parse Gateway child-chain commit calldata and return compact Bitcoin DA refs
// that must be finalized before admitting the tx to the Gateway mempool.
fn compact_edge_da_refs_from_commit_calldata(
    input: &[u8],
) -> Result<Option<Vec<String>>, EthSendRawTransactionError> {
    if !has_compact_edge_da_commit_selector(input) {
        return Ok(None);
    }

    let commit = CommitCalldata::decode(input).map_err(|err| {
        EthSendRawTransactionError::EdgeDaAdmissionCheckFailed(format!(
            "failed to decode compact edge DA commit calldata: {err}"
        ))
    })?;
    match commit.commit_batch_info.l2_da_commitment_scheme {
        DACommitmentScheme::BlobsZKsyncOS => {}
        // SYSCOIN: Validium child chains have no Bitcoin DA refs to check at Gateway admission.
        // Their scheme-specific validity remains enforced by the configured onchain DA validator.
        DACommitmentScheme::EmptyNoDA => return Ok(None),
        scheme => {
            return Err(EthSendRawTransactionError::EdgeDaAdmissionCheckFailed(
                format!("unsupported child-chain DA commitment scheme for Gateway: {scheme:?}"),
            ));
        }
    }

    let operator_da_input = &commit.commit_batch_info.operator_da_input;
    let blob_hash_count = operator_da_input.len() / 32;
    if operator_da_input.is_empty()
        || operator_da_input.len() % 32 != 0
        || blob_hash_count > SYSCOIN_DA_MAX_BLOBS_PER_BATCH
    {
        return Err(EthSendRawTransactionError::EdgeDaAdmissionCheckFailed(
            format!(
                "compact edge DA operator input must be a non-empty array of at most {SYSCOIN_DA_MAX_BLOBS_PER_BATCH} 32-byte hashes"
            ),
        ));
    }
    let actual_commitment = keccak256(operator_da_input);
    if actual_commitment != commit.commit_batch_info.da_commitment {
        return Err(EthSendRawTransactionError::EdgeDaAdmissionCheckFailed(
            format!(
                "compact edge DA commitment mismatch: expected {}, got {}",
                commit.commit_batch_info.da_commitment, actual_commitment
            ),
        ));
    }

    Ok(Some(
        operator_da_input
            .chunks_exact(32)
            .map(hex::encode)
            .collect(),
    ))
}

// SYSCOIN: forwarding can fail after local mempool insertion. Only roll back errors that are
// definitely local/pre-send failures or explicit main-node rejections; keep the tx for ambiguous
// transport/response failures so pending RPC state does not drop a tx the main node may include.
fn forwarding_error_should_rollback_local_tx(err: &RpcError<TransportErrorKind>) -> bool {
    match err {
        RpcError::ErrorResp(_) => !forwarding_error_indicates_main_node_already_knows_tx(err),
        RpcError::SerError(_) | RpcError::UnsupportedFeature(_) | RpcError::LocalUsageError(_) => {
            true
        }
        RpcError::Transport(_) | RpcError::NullResp | RpcError::DeserError { .. } => false,
    }
}

fn forwarding_error_indicates_main_node_already_knows_tx(
    err: &RpcError<TransportErrorKind>,
) -> bool {
    let Some(payload) = err.as_error_resp() else {
        return false;
    };
    contains_known_transaction_error(payload.message.as_ref())
        || payload
            .data
            .as_ref()
            .is_some_and(|data| contains_known_transaction_error(data.get()))
}

fn contains_known_transaction_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("already known")
        || message.contains("already imported")
        || message.contains("already in the pool")
        || message.contains("transaction already")
        || message.trim_start().starts_with("known transaction")
}

/// Error types returned by `eth_sendRawTransaction` implementation
#[derive(Debug, thiserror::Error)]
pub enum EthSendRawTransactionError {
    /// When decoding a signed transaction fails
    #[error("failed to decode signed transaction")]
    FailedToDecodeSignedTransaction,
    /// When the transaction signature is invalid
    #[error("invalid transaction signature")]
    InvalidTransactionSignature,
    /// When the node is not accepting new transactions
    #[error("{}", .0.iter().map(|r| r.to_string()).collect::<Vec<_>>().join("; "))]
    NotAcceptingTransactions(Vec<NotAcceptingReason>),
    /// Errors related to the transaction pool
    #[error(transparent)]
    PoolError(#[from] PoolError),
    /// Error forwarded from main node
    #[error(transparent)]
    ForwardError(#[from] RpcError<TransportErrorKind>),
    #[error("Signer is blacklisted")]
    BlacklistedSigner,
    #[error("Transaction is blacklisted")]
    BlacklistedTransaction,
    #[error("compact edge DA admission check failed: {0}")]
    EdgeDaAdmissionCheckFailed(String),
}

impl From<&EthSendRawTransactionError> for TxRejectionReason {
    fn from(err: &EthSendRawTransactionError) -> Self {
        match err {
            EthSendRawTransactionError::FailedToDecodeSignedTransaction => Self::DecodeFailed,
            EthSendRawTransactionError::InvalidTransactionSignature => Self::InvalidSignature,
            EthSendRawTransactionError::NotAcceptingTransactions(_) => Self::NotAccepting,
            EthSendRawTransactionError::BlacklistedSigner => Self::BlacklistedSigner,
            EthSendRawTransactionError::BlacklistedTransaction => Self::BlacklistedTransaction,
            EthSendRawTransactionError::EdgeDaAdmissionCheckFailed(_) => Self::PoolOther,
            EthSendRawTransactionError::ForwardError(rpc_err) => match rpc_err {
                RpcError::ErrorResp(_) => Self::ForwardRejected,
                _ => Self::ForwardTransportError,
            },
            EthSendRawTransactionError::PoolError(pool_err) => Self::from(&pool_err.kind),
        }
    }
}

impl From<&PoolErrorKind> for TxRejectionReason {
    fn from(kind: &PoolErrorKind) -> Self {
        match kind {
            PoolErrorKind::AlreadyImported => Self::PoolAlreadyImported,
            PoolErrorKind::ReplacementUnderpriced => Self::PoolReplacementUnderpriced,
            PoolErrorKind::FeeCapBelowMinimumProtocolFeeCap(_) => Self::PoolFeeCapBelowMinimum,
            PoolErrorKind::SpammerExceededCapacity(_) => Self::PoolSpammerExceededCapacity,
            PoolErrorKind::DiscardedOnInsert => Self::PoolDiscardedOnInsert,
            PoolErrorKind::ExistingConflictingTransactionType(_, _) => Self::PoolConflictingTxType,
            PoolErrorKind::InvalidTransaction(invalid) => Self::from(invalid),
            PoolErrorKind::Other(_) => Self::PoolOther,
        }
    }
}

impl From<&InvalidPoolTransactionError> for TxRejectionReason {
    fn from(err: &InvalidPoolTransactionError) -> Self {
        match err {
            InvalidPoolTransactionError::Consensus(_) => Self::PoolConsensusError,
            InvalidPoolTransactionError::ExceedsGasLimit(_, _) => Self::PoolExceedsGasLimit,
            InvalidPoolTransactionError::MaxTxGasLimitExceeded(_, _) => {
                Self::PoolMaxTxGasLimitExceeded
            }
            InvalidPoolTransactionError::ExceedsFeeCap { .. } => Self::PoolExceedsFeeCap,
            InvalidPoolTransactionError::ExceedsMaxInitCodeSize(_, _) => {
                Self::PoolExceedsMaxInitCodeSize
            }
            InvalidPoolTransactionError::OversizedData { .. } => Self::PoolOversizedData,
            InvalidPoolTransactionError::Underpriced => Self::PoolUnderpriced,
            InvalidPoolTransactionError::Overdraft { .. } => Self::PoolOverdraft,
            InvalidPoolTransactionError::Eip2681 => Self::PoolNonceOverflow,
            InvalidPoolTransactionError::Eip4844(_) => Self::PoolEip4844Error,
            InvalidPoolTransactionError::Eip7702(_) => Self::PoolEip7702Error,
            InvalidPoolTransactionError::Other(_) => Self::PoolOther,
            InvalidPoolTransactionError::IntrinsicGasTooLow => Self::PoolIntrinsicGasTooLow,
            InvalidPoolTransactionError::PriorityFeeBelowMinimum { .. } => {
                Self::PoolPriorityFeeBelowMinimum
            }
        }
    }
}

/// Error types returned by `eth_sendRawTransactionSync` implementation
#[derive(Debug, thiserror::Error)]
pub enum EthSendRawTransactionSyncError {
    /// Regular `eth_sendRawTransaction` errors
    #[error(transparent)]
    Regular(#[from] EthSendRawTransactionError),
    /// Timeout while waiting for transaction receipt.
    #[error("The transaction was added to the mempool but wasn't processed within {0:?}.")]
    Timeout(Duration),
}

/// Records mempool insertion latency on drop, capturing errors and async cancellations.
struct MempoolLatencyGuard(Instant);

impl MempoolLatencyGuard {
    fn new() -> Self {
        Self(Instant::now())
    }
}

impl Drop for MempoolLatencyGuard {
    fn drop(&mut self) {
        TX_SUBMISSION.mempool_latency.observe(self.0.elapsed());
    }
}

/// Records forwarding latency on drop, capturing errors and async cancellations.
struct ForwardingLatencyGuard(Instant);

impl ForwardingLatencyGuard {
    fn new() -> Self {
        Self(Instant::now())
    }
}

impl Drop for ForwardingLatencyGuard {
    fn drop(&mut self) {
        TX_SUBMISSION.forwarding_latency.observe(self.0.elapsed());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RpcStorage;
    use alloy::consensus::{Block, BlockBody, Header, Sealed};
    use alloy::eips::Encodable2718;
    use alloy::network::{EthereumWallet, TransactionBuilder};
    use alloy::primitives::{BlockHash, BlockNumber, TxHash, TxNonce};
    use alloy::rpc::types::{TransactionInput, TransactionRequest};
    use alloy::signers::local::PrivateKeySigner;
    use httpmock::Method::POST;
    use httpmock::MockServer;
    use serde_json::json;
    use std::collections::HashSet;
    use std::fmt;
    use std::ops::RangeInclusive;
    use std::sync::Arc;
    use tokio::sync::{broadcast, watch};
    use zksync_os_contract_interface::calldata::encode_commit_batch_data;
    use zksync_os_contract_interface::models::{CommitBatchInfo, StoredBatchInfo};
    use zksync_os_interface::traits::{PreimageSource, ReadStorage};
    use zksync_os_mempool::{PoolConfig, TxValidatorConfig};
    use zksync_os_merkle_tree_api::{
        BatchTreeProof, MAX_TREE_DEPTH, MerkleTreeProver, TreeBatchOutput,
    };
    use zksync_os_reth_compat::provider::ZkProviderFactory;
    use zksync_os_storage_api::notifications::{BlockNotification, SubscribeToBlocks};
    use zksync_os_storage_api::{
        FinalityStatus, LogIndex, PersistedBatch, ReadBatch, ReadFinality, ReadReplay,
        ReadRepository, ReadStateHistory, RepositoryBlock, RepositoryResult, StateResult,
        StoredTxData, TxMeta, ViewState,
    };
    use zksync_os_types::{ZkReceiptEnvelope, ZkTransaction};

    #[derive(Clone, Debug)]
    struct EmptyState;

    #[derive(Clone, Debug)]
    struct EmptyStateView;

    impl ReadStorage for EmptyStateView {
        fn read(&mut self, _key: B256) -> Option<B256> {
            None
        }
    }

    impl PreimageSource for EmptyStateView {
        fn get_preimage(&mut self, _hash: B256) -> Option<Vec<u8>> {
            None
        }
    }

    impl ReadStateHistory for EmptyState {
        fn state_view_at(&self, _block_number: BlockNumber) -> StateResult<impl ViewState> {
            Ok(EmptyStateView)
        }

        fn block_range_available(&self) -> RangeInclusive<u64> {
            0..=0
        }
    }

    #[derive(Clone)]
    struct TestRepository {
        genesis: RepositoryBlock,
        blocks: broadcast::Sender<BlockNotification>,
    }

    impl fmt::Debug for TestRepository {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("TestRepository").finish()
        }
    }

    impl TestRepository {
        fn new() -> Self {
            let header = Header {
                gas_limit: 30_000_000,
                base_fee_per_gas: Some(0),
                ..Default::default()
            };
            let genesis = Sealed::new_unchecked(
                Block::new(header, BlockBody::<TxHash>::default()),
                B256::ZERO,
            );
            let (blocks, _) = broadcast::channel(1);
            Self { genesis, blocks }
        }
    }

    impl LogIndex for TestRepository {}

    impl SubscribeToBlocks for TestRepository {
        fn subscribe_to_blocks(&self) -> broadcast::Receiver<BlockNotification> {
            self.blocks.subscribe()
        }
    }

    impl ReadRepository for TestRepository {
        fn get_block_by_number(
            &self,
            number: BlockNumber,
        ) -> RepositoryResult<Option<RepositoryBlock>> {
            Ok((number == 0).then(|| self.genesis.clone()))
        }

        fn get_block_by_hash(&self, hash: BlockHash) -> RepositoryResult<Option<RepositoryBlock>> {
            Ok((hash == self.genesis.hash()).then(|| self.genesis.clone()))
        }

        fn get_raw_transaction(&self, _hash: TxHash) -> RepositoryResult<Option<Vec<u8>>> {
            Ok(None)
        }

        fn get_transaction(&self, _hash: TxHash) -> RepositoryResult<Option<ZkTransaction>> {
            Ok(None)
        }

        fn get_transaction_receipt(
            &self,
            _hash: TxHash,
        ) -> RepositoryResult<Option<ZkReceiptEnvelope>> {
            Ok(None)
        }

        fn get_transaction_meta(&self, _hash: TxHash) -> RepositoryResult<Option<TxMeta>> {
            Ok(None)
        }

        fn get_transaction_hash_by_sender_nonce(
            &self,
            _sender: Address,
            _nonce: TxNonce,
        ) -> RepositoryResult<Option<TxHash>> {
            Ok(None)
        }

        fn get_stored_transaction(&self, _hash: TxHash) -> RepositoryResult<Option<StoredTxData>> {
            Ok(None)
        }

        fn get_latest_block(&self) -> u64 {
            0
        }
    }

    #[derive(Clone, Debug)]
    struct EmptyReplay;

    impl ReadReplay for EmptyReplay {
        fn get_context(
            &self,
            _block_number: BlockNumber,
        ) -> Option<zksync_os_interface::types::BlockContext> {
            None
        }

        fn get_replay_record_by_key(
            &self,
            _block_number: BlockNumber,
            _db_key: Option<Vec<u8>>,
        ) -> Option<zksync_os_storage_api::ReplayRecord> {
            None
        }

        fn get_canonical_block_hash(&self, _block_number: BlockNumber) -> Option<BlockHash> {
            None
        }

        fn latest_record(&self) -> BlockNumber {
            0
        }
    }

    #[derive(Clone)]
    struct EmptyFinality {
        status: watch::Sender<FinalityStatus>,
    }

    impl EmptyFinality {
        fn new() -> Self {
            let status = FinalityStatus {
                last_committed_block: 0,
                last_committed_batch: 0,
                last_executed_block: 0,
                last_executed_batch: 0,
                last_finalized_executed_block: 0,
                last_finalized_executed_batch: 0,
            };
            Self {
                status: watch::channel(status).0,
            }
        }
    }

    impl ReadFinality for EmptyFinality {
        fn get_finality_status(&self) -> FinalityStatus {
            self.status.borrow().clone()
        }

        fn subscribe(&self) -> watch::Receiver<FinalityStatus> {
            self.status.subscribe()
        }
    }

    #[derive(Clone, Debug)]
    struct EmptyBatch;

    impl ReadBatch for EmptyBatch {
        fn get_batch_by_block_number(
            &self,
            _block_number: BlockNumber,
        ) -> anyhow::Result<Option<PersistedBatch>> {
            Ok(None)
        }

        fn get_batch_by_number(
            &self,
            _batch_number: u64,
        ) -> anyhow::Result<Option<PersistedBatch>> {
            Ok(None)
        }

        fn latest_batch(&self) -> u64 {
            0
        }
    }

    #[derive(Debug)]
    struct EmptyTree;

    impl MerkleTreeProver for EmptyTree {
        fn tree_depth(&self) -> u8 {
            MAX_TREE_DEPTH
        }

        fn prove(
            &self,
            _version: u64,
            _keys: &[B256],
        ) -> anyhow::Result<Option<(BatchTreeProof, TreeBatchOutput)>> {
            Ok(None)
        }
    }

    fn dummy_stored_batch_info() -> StoredBatchInfo {
        StoredBatchInfo {
            batch_number: 0,
            state_commitment: B256::ZERO,
            number_of_layer1_txs: 0,
            priority_operations_hash: B256::ZERO,
            dependency_roots_rolling_hash: B256::ZERO,
            l2_to_l1_logs_root_hash: B256::ZERO,
            commitment: B256::ZERO,
            last_block_timestamp: Some(0),
        }
    }

    fn dummy_commit_batch_info(
        scheme: DACommitmentScheme,
        da_commitment: B256,
        operator_da_input: Vec<u8>,
    ) -> CommitBatchInfo {
        CommitBatchInfo {
            batch_number: 1,
            new_state_commitment: B256::ZERO,
            number_of_layer1_txs: 0,
            number_of_layer2_txs: 0,
            priority_operations_hash: B256::ZERO,
            dependency_roots_rolling_hash: B256::ZERO,
            l2_to_l1_logs_root_hash: B256::ZERO,
            l2_da_commitment_scheme: scheme,
            da_commitment,
            first_block_timestamp: 1,
            first_block_number: Some(1),
            last_block_timestamp: 1,
            last_block_number: Some(1),
            chain_id: 1,
            operator_da_input,
            edge_da_refs_input: Vec::new(),
            edge_da_refs_root: B256::ZERO,
            sl_chain_id: 1,
        }
    }

    fn commit_call_data(commit_info: CommitBatchInfo) -> Vec<u8> {
        let commit_data = encode_commit_batch_data(&dummy_stored_batch_info(), commit_info, 31);
        IExecutor::commitBatchesSharedBridgeCall {
            _chainAddress: Address::ZERO,
            _processFrom: U256::ZERO,
            _processTo: U256::from(1),
            _commitData: Bytes::from(commit_data),
        }
        .abi_encode()
    }

    fn multisig_commit_call_data(commit_info: CommitBatchInfo) -> Vec<u8> {
        let commit_data = encode_commit_batch_data(&dummy_stored_batch_info(), commit_info, 31);
        IMultisigCommitter::commitBatchesMultisigCall {
            chainAddress: Address::ZERO,
            _processBatchFrom: U256::ZERO,
            _processBatchTo: U256::from(1),
            _batchData: Bytes::from(commit_data),
            signers: Vec::new(),
            signatures: Vec::new(),
        }
        .abi_encode()
    }

    fn rpc_config(commit_tx_target: Address, rpc_url: String) -> RpcConfig {
        RpcConfig {
            address: "127.0.0.1:0".to_string(),
            eth_call_gas: 50_000_000,
            eth_simulate_block_gas_limit: 50_000_000,
            max_connections: 128,
            max_concurrent_blocking_rpcs: 8,
            max_subscriptions_per_connection: 1024,
            max_request_size: 10,
            max_response_size: 10,
            max_blocks_per_filter: 10_000,
            max_logs_per_response: 10_000,
            stale_filter_ttl: Duration::from_secs(300),
            l2_signer_blacklist: HashSet::new(),
            l2_tx_blacklist: HashSet::new(),
            send_raw_transaction_sync_timeout: Duration::from_secs(1),
            gas_price_scale_factor: 1.0,
            estimate_gas_pubdata_price_factor: 1.0,
            enable_debug_namespace: false,
            enable_txpool_namespace: false,
            edge_da_admission: Some(crate::EdgeDaAdmissionConfig {
                commit_tx_target,
                rpc_url: rpc_url.clone(),
                rpc_user: "user".to_string(),
                rpc_password: "password".to_string(),
                poda_url: rpc_url,
                wallet_name: "zksync-os".to_string(),
                request_timeout: Duration::from_secs(2),
            }),
        }
    }

    fn operator_da_input(blob_count: usize) -> Vec<u8> {
        let mut input = Vec::with_capacity(32 * blob_count);
        for idx in 0..blob_count {
            input.extend([idx as u8 + 1; 32]);
        }
        input
    }

    fn tx_handler(
        commit_tx_target: Address,
        rpc_url: String,
    ) -> TxHandler<
        RpcStorage<TestRepository, EmptyReplay, EmptyFinality, EmptyBatch, EmptyState>,
        impl L2Subpool,
    > {
        let repository = TestRepository::new();
        let state = EmptyState;
        let storage = RpcStorage::new(
            repository.clone(),
            EmptyReplay,
            EmptyFinality::new(),
            EmptyBatch,
            state.clone(),
            Arc::new(EmptyTree),
        );
        let mempool = zksync_os_mempool::subpools::l2::in_memory(
            ZkProviderFactory::new(state, repository, 270),
            PoolConfig::default().with_disabled_protocol_base_fee(),
            TxValidatorConfig {
                max_input_bytes: usize::MAX,
                tx_fee_cap: 0,
            },
        );
        let (_, acceptance_state) = watch::channel(TransactionAcceptanceState::Accepting);
        TxHandler::new(
            rpc_config(commit_tx_target, rpc_url),
            storage,
            mempool,
            acceptance_state,
            None,
        )
    }

    #[tokio::test]
    async fn duplicate_compact_edge_da_tx_is_rejected_before_da_lookup() {
        let syscoin_da = MockServer::start_async().await;
        let get_blob_data = syscoin_da
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/")
                    .body_matches(r#""method"\s*:\s*"getnevmblobdata""#);
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(json!({
                        "result": {"data": "00"},
                        "error": null,
                        "id": 1
                    }));
            })
            .await;

        let commit_tx_target = Address::repeat_byte(0x77);
        let handler = tx_handler(commit_tx_target, syscoin_da.base_url());

        let blobs = operator_da_input(SYSCOIN_DA_MAX_BLOBS_PER_BATCH);
        let calldata = commit_call_data(dummy_commit_batch_info(
            DACommitmentScheme::BlobsZKsyncOS,
            keccak256(&blobs),
            blobs,
        ));
        let wallet = EthereumWallet::new(PrivateKeySigner::random());
        let tx = TransactionRequest::default()
            .with_to(commit_tx_target)
            .with_nonce(0)
            .with_gas_limit(1_000_000)
            .with_max_fee_per_gas(0)
            .with_max_priority_fee_per_gas(0)
            .with_chain_id(270)
            .input(TransactionInput::new(Bytes::from(calldata)));
        let encoded = Bytes::from(tx.build(&wallet).await.unwrap().encoded_2718());

        handler
            .send_raw_transaction_impl(encoded.clone())
            .await
            .expect("initial compact edge DA transaction must be accepted");
        assert_eq!(
            get_blob_data.calls_async().await,
            SYSCOIN_DA_MAX_BLOBS_PER_BATCH
        );

        let replay_rejection = handler
            .send_raw_transaction_impl(encoded)
            .await
            .expect_err("duplicate transaction must be rejected");

        assert!(matches!(
            replay_rejection,
            EthSendRawTransactionError::PoolError(_)
        ));
        assert!(
            replay_rejection.to_string().contains("already imported"),
            "unexpected replay rejection: {replay_rejection}"
        );
        assert_eq!(
            get_blob_data.calls_async().await,
            SYSCOIN_DA_MAX_BLOBS_PER_BATCH,
            "duplicate replay must not perform additional DA lookups"
        );
    }

    #[test]
    fn compact_edge_da_guard_only_routes_target_validator_timelock() {
        let validator_timelock = Address::repeat_byte(0x11);
        assert!(is_compact_edge_da_commit_target(
            Some(validator_timelock),
            validator_timelock
        ));
        assert!(!is_compact_edge_da_commit_target(
            Some(Address::repeat_byte(0x22)),
            validator_timelock
        ));
        assert!(!is_compact_edge_da_commit_target(None, validator_timelock));
    }

    #[test]
    fn compact_edge_da_refs_extracts_blob_hashes() {
        let mut operator_da_input = vec![0x11; 32];
        operator_da_input.extend([0x22; 32]);
        let expected = vec![hex::encode([0x11; 32]), hex::encode([0x22; 32])];
        let input = commit_call_data(dummy_commit_batch_info(
            DACommitmentScheme::BlobsZKsyncOS,
            keccak256(&operator_da_input),
            operator_da_input,
        ));

        let refs = compact_edge_da_refs_from_commit_calldata(&input)
            .unwrap()
            .unwrap();

        assert_eq!(refs, expected);
    }

    #[test]
    fn compact_edge_da_refs_extracts_multisig_commit_blob_hashes() {
        let operator_da_input = vec![0x33; 32];
        let expected = vec![hex::encode([0x33; 32])];
        let input = multisig_commit_call_data(dummy_commit_batch_info(
            DACommitmentScheme::BlobsZKsyncOS,
            keccak256(&operator_da_input),
            operator_da_input,
        ));

        let refs = compact_edge_da_refs_from_commit_calldata(&input)
            .unwrap()
            .unwrap();

        assert_eq!(refs, expected);
    }

    #[test]
    fn compact_edge_da_refs_rejects_empty_multisig_batch_data() {
        let input = IMultisigCommitter::commitBatchesMultisigCall {
            chainAddress: Address::ZERO,
            _processBatchFrom: U256::ZERO,
            _processBatchTo: U256::from(1),
            _batchData: Bytes::new(),
            signers: Vec::new(),
            signatures: Vec::new(),
        }
        .abi_encode();

        let err = compact_edge_da_refs_from_commit_calldata(&input).unwrap_err();

        assert!(err.to_string().contains("commit data is empty"));
    }

    #[test]
    fn compact_edge_da_refs_allows_validium_without_refs() {
        let input = commit_call_data(dummy_commit_batch_info(
            DACommitmentScheme::EmptyNoDA,
            B256::ZERO,
            vec![0; 32],
        ));

        let refs = compact_edge_da_refs_from_commit_calldata(&input).unwrap();

        assert!(refs.is_none());
    }

    #[test]
    fn compact_edge_da_refs_rejects_unsupported_scheme() {
        let input = commit_call_data(dummy_commit_batch_info(
            DACommitmentScheme::BlobsAndPubdataKeccak256,
            B256::ZERO,
            Vec::new(),
        ));

        let err = compact_edge_da_refs_from_commit_calldata(&input).unwrap_err();

        assert!(
            err.to_string()
                .contains("unsupported child-chain DA commitment scheme")
        );
    }

    #[test]
    fn compact_edge_da_refs_rejects_commitment_mismatch() {
        let input = commit_call_data(dummy_commit_batch_info(
            DACommitmentScheme::BlobsZKsyncOS,
            B256::ZERO,
            vec![0x11; 32],
        ));

        let err = compact_edge_da_refs_from_commit_calldata(&input).unwrap_err();

        assert!(
            err.to_string()
                .contains("compact edge DA commitment mismatch")
        );
    }

    #[test]
    fn compact_edge_da_refs_rejects_oversized_hash_array() {
        let operator_da_input = vec![0x11; 32 * (SYSCOIN_DA_MAX_BLOBS_PER_BATCH + 1)];
        let input = commit_call_data(dummy_commit_batch_info(
            DACommitmentScheme::BlobsZKsyncOS,
            keccak256(&operator_da_input),
            operator_da_input,
        ));

        let err = compact_edge_da_refs_from_commit_calldata(&input).unwrap_err();

        assert!(err.to_string().contains("at most 32 32-byte hashes"));
    }

    #[test]
    fn forwarding_duplicate_errors_are_known_transaction_errors() {
        assert!(contains_known_transaction_error("already known"));
        assert!(contains_known_transaction_error(
            "transaction already imported"
        ));
        assert!(contains_known_transaction_error(
            "transaction already in the pool"
        ));
        assert!(contains_known_transaction_error(
            "transaction already exists"
        ));
        assert!(contains_known_transaction_error(
            "known transaction: 0x1234"
        ));
        assert!(!contains_known_transaction_error("nonce too low"));
        assert!(!contains_known_transaction_error(
            "unknown transaction type"
        ));
    }

    #[test]
    fn forwarding_ambiguous_errors_do_not_rollback_local_tx() {
        let null_response = RpcError::<TransportErrorKind>::NullResp;

        assert!(!forwarding_error_should_rollback_local_tx(&null_response));
    }
}
