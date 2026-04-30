use crate::eth_impl::build_api_receipt;
use crate::metrics::{TX_SUBMISSION, TxRejectionReason};
use crate::{ReadRpcStorage, RpcConfig};
use alloy::consensus::Transaction;
use alloy::consensus::transaction::SignerRecoverable;
use alloy::eips::Decodable2718;
use alloy::hex;
use alloy::primitives::{B256, Bytes, U256, keccak256};
use alloy::providers::{DynProvider, Provider};
use alloy::sol_types::SolCall;
use alloy::transports::{RpcError, TransportErrorKind};
use bitcoin_da_client::SyscoinClient;
use std::time::{Duration, Instant};
use tokio::sync::watch;
use zksync_os_contract_interface::IExecutor;
use zksync_os_contract_interface::calldata::CommitCalldata;
use zksync_os_contract_interface::models::DACommitmentScheme;
use zksync_os_mempool::PoolError;
use zksync_os_mempool::subpools::l2::L2Subpool;
use zksync_os_mempool::{InvalidPoolTransactionError, PoolErrorKind};
use zksync_os_rpc_api::types::ZkTransactionReceipt;
use zksync_os_types::{L2Envelope, L2Transaction, NotAcceptingReason, TransactionAcceptanceState};

/// Maximum user provided timeout for `eth_sendRawTransactionSync`. Chosen liberally as waiting is
/// inexpensive.
const SEND_RAW_TRANSACTION_SYNC_MAX_TIMEOUT: Duration = Duration::from_secs(30);

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
        if let TransactionAcceptanceState::NotAccepting(reason) = &*self.acceptance_state.borrow() {
            return Err(EthSendRawTransactionError::NotAcceptingTransactions(
                *reason,
            ));
        }

        let transaction = L2Envelope::decode_2718(&mut tx_bytes.as_ref())
            .map_err(|_| EthSendRawTransactionError::FailedToDecodeSignedTransaction)?;
        let l2_tx: L2Transaction = transaction
            .try_into_recovered()
            .map_err(|_| EthSendRawTransactionError::InvalidTransactionSignature)?;
        let hash = *l2_tx.hash();
        if self.config.l2_signer_blacklist.contains(&l2_tx.signer()) {
            return Err(EthSendRawTransactionError::BlacklistedSigner);
        }
        // SYSCOIN
        self.verify_compact_edge_da_refs(&l2_tx).await?;
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
                // Remove previously added transaction from local mempool
                self.mempool.remove_transactions(vec![hash]);
                return Err(err.into());
            }
        }

        Ok(hash)
    }
    // SYSCOIN: verify compact edge DA refs emitted by chains settling to Gateway.
    async fn verify_compact_edge_da_refs(
        &self,
        tx: &L2Transaction,
    ) -> Result<(), EthSendRawTransactionError> {
        let Some(config) = self.config.edge_da_finality.as_ref() else {
            return Ok(());
        };
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
            EthSendRawTransactionError::EdgeDaFinalityCheckFailed(format!(
                "failed to create Bitcoin DA client: {err}"
            ))
        })?;
        for (idx, version_hash) in version_hashes.iter().enumerate() {
            let finalized = client
                .check_blob_finality_with_mode(
                    version_hash,
                    config.finality_mode,
                    config.confirmations,
                )
                .await
                .map_err(|err| {
                    EthSendRawTransactionError::EdgeDaFinalityCheckFailed(format!(
                        "failed to check Bitcoin DA finality for compact edge ref {idx} ({version_hash}): {err}"
                    ))
                })?;
            if !finalized {
                return Err(EthSendRawTransactionError::EdgeDaFinalityCheckFailed(
                    format!("compact edge DA ref {idx} ({version_hash}) is not finalized"),
                ));
            }
        }

        Ok(())
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

                if let Some(stored_tx) = block.transactions.get(&tx_hash) {
                    return Ok(build_api_receipt(
                        tx_hash,
                        stored_tx.receipt.clone(),
                        &stored_tx.tx,
                        &stored_tx.meta,
                    ));
                }
            }
        })
        .await
        .map_err(|_| EthSendRawTransactionSyncError::Timeout(timeout_duration))?
    }
}

// SYSCOIN: parse Gateway child-chain commit calldata and return compact Bitcoin DA refs
// that must be finalized before admitting the tx to the Gateway mempool.
fn compact_edge_da_refs_from_commit_calldata(
    input: &[u8],
) -> Result<Option<Vec<String>>, EthSendRawTransactionError> {
    if input.len() < 4 || input[..4] != IExecutor::commitBatchesSharedBridgeCall::SELECTOR {
        return Ok(None);
    }

    let commit = CommitCalldata::decode(input).map_err(|err| {
        EthSendRawTransactionError::EdgeDaFinalityCheckFailed(format!(
            "failed to decode compact edge DA commit calldata: {err}"
        ))
    })?;
    match commit.commit_batch_info.l2_da_commitment_scheme {
        DACommitmentScheme::BlobsZKsyncOS => {}
        // SYSCOIN: Validium child chains have no Bitcoin DA refs to check at Gateway admission.
        // Their scheme-specific validity remains enforced by the configured onchain DA validator.
        DACommitmentScheme::EmptyNoDA => return Ok(None),
        scheme => {
            return Err(EthSendRawTransactionError::EdgeDaFinalityCheckFailed(
                format!("unsupported child-chain DA commitment scheme for Gateway: {scheme:?}"),
            ));
        }
    }

    let operator_da_input = &commit.commit_batch_info.operator_da_input;
    if operator_da_input.is_empty() || operator_da_input.len() % 32 != 0 {
        return Err(EthSendRawTransactionError::EdgeDaFinalityCheckFailed(
            "compact edge DA operator input must be a non-empty 32-byte hash array".into(),
        ));
    }
    let actual_commitment = keccak256(operator_da_input);
    if actual_commitment != commit.commit_batch_info.da_commitment {
        return Err(EthSendRawTransactionError::EdgeDaFinalityCheckFailed(
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
    #[error(transparent)]
    NotAcceptingTransactions(NotAcceptingReason),
    /// Errors related to the transaction pool
    #[error(transparent)]
    PoolError(#[from] PoolError),
    /// Error forwarded from main node
    #[error(transparent)]
    ForwardError(#[from] RpcError<TransportErrorKind>),
    #[error("Signer is blacklisted")]
    BlacklistedSigner,
    #[error("compact edge DA finality check failed: {0}")]
    EdgeDaFinalityCheckFailed(String),
}

impl From<&EthSendRawTransactionError> for TxRejectionReason {
    fn from(err: &EthSendRawTransactionError) -> Self {
        match err {
            EthSendRawTransactionError::FailedToDecodeSignedTransaction => Self::DecodeFailed,
            EthSendRawTransactionError::InvalidTransactionSignature => Self::InvalidSignature,
            EthSendRawTransactionError::NotAcceptingTransactions(_) => Self::NotAccepting,
            EthSendRawTransactionError::BlacklistedSigner => Self::BlacklistedSigner,
            EthSendRawTransactionError::EdgeDaFinalityCheckFailed(_) => Self::PoolOther,
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
    use alloy::primitives::Address;
    use zksync_os_contract_interface::calldata::encode_commit_batch_data;
    use zksync_os_contract_interface::models::{CommitBatchInfo, StoredBatchInfo};

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
}
