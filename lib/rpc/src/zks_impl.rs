use crate::imt::{calculate_root, indexed_leaf_hash};
use crate::interop_commitment_tree::{InteropCommitmentTreeError, InteropCommitmentTreeReader};
use crate::log_proof_utils::{
    assemble_log_proof, build_gateway_proof_extension, build_message_root_proof_extension,
};
use crate::result::ToRpcResult;
use crate::{EthCallHandler, ReadRpcStorage};
use alloy::eips::BlockId;
use alloy::primitives::{Address, B256, BlockNumber, TxHash, U64, U256, keccak256};
use alloy::providers::DynProvider;
use alloy::rpc::types::Index;
use anyhow::Context;
use async_trait::async_trait;
use blake2::{Blake2s256, Digest};
use jsonrpsee::core::RpcResult;
use ruint::aliases::B160;
use std::sync::Arc;
use zk_ee::common_structs::derive_flat_storage_key;
use zksync_os_contract_interface::IBridgehub;
use zksync_os_contract_interface::settlement_layer_intervals::{
    IntervalSettlementLayer, SettlementLayerIntervals,
};
use zksync_os_genesis::{GenesisInput, GenesisInputSource};
use zksync_os_l1_watcher::{CommittedBatchProvider, util::find_l1_execute_block_by_batch_number};
use zksync_os_merkle_tree_api::flat::StorageSlotProof;
use zksync_os_mini_merkle_tree::MiniMerkleTree;
use zksync_os_rpc_api::{
    types::{
        AddressScopedKey, BatchStorageProof, BlockMetadata, ImtInclusionProof, ImtLeaf,
        L1VerificationData, L2ToL1LogProof, LogProofTarget, StateCommitmentPreimage,
    },
    zks::ZksApiServer,
};
use zksync_os_storage_api::{PersistedBatch, RepositoryError, StateError, read_multichain_root};
use zksync_os_types::{L2_TO_L1_TREE_SIZE, ProtocolSemanticVersion};

pub struct ZksNamespace<RpcStorage> {
    bridgehub_address: Address,
    bytecode_supplier_address: Address,
    storage: RpcStorage,
    genesis_input_source: Arc<dyn GenesisInputSource>,
    l2_chain_id: u64,
    /// Queries L1 MessageRoot for V32 batches that settled directly on L1.
    l1_provider: DynProvider,
    /// Present while the chain settles on Gateway; historical Gateway batches use its
    /// `L2MessageRoot`.
    gateway_provider: Option<DynProvider>,
    // SYSCOIN: A configured historical Gateway client does not imply that every batch settled
    // there; route proofs by the discovered batch interval across Gateway -> L1 migrations.
    settlement_layer_intervals: SettlementLayerIntervals,
    // SYSCOIN: Explicit operator trust gate for exposing MessageRoot proofs at the Gateway head.
    // When disabled, the RPC remains bound to the persisted finalized-batch index.
    optimistic_gateway_head: bool,
    // SYSCOIN: With the explicit gate above, MessageRoot proofs for Gateway-settled batches may
    // use the live committed-batch index after Gateway execution, without waiting for Gateway's
    // aggregate batch to finalize on L1. Default withdrawal and direct-L1 proof paths continue to
    // use persisted finalized batches.
    committed_batch_provider: CommittedBatchProvider,
    commitment_tree_reader: InteropCommitmentTreeReader<RpcStorage>,
}

// SYSCOIN: The live committed-batch fallback is intentionally narrower than the persisted RPC
// index: only Gateway MessageRoot proofs may use the optimistic Gateway execution boundary.
fn permits_optimistic_gateway_batch(
    optimistic_gateway_head: bool,
    proof_target: LogProofTarget,
    settlement_layer: &IntervalSettlementLayer,
) -> bool {
    optimistic_gateway_head
        && matches!(proof_target, LogProofTarget::MessageRoot)
        && matches!(settlement_layer, IntervalSettlementLayer::Gateway(_))
}

// SYSCOIN: A public proof request must report inconsistent transaction/batch indexing instead of
// panicking the RPC task.
fn require_batch_log_index(
    batch_index: Option<usize>,
    tx_hash: TxHash,
    block_number: BlockNumber,
    batch_number: u64,
) -> ZksResult<usize> {
    batch_index.ok_or(ZksError::TransactionNotInBatch {
        tx_hash,
        block_number,
        batch_number,
    })
}

impl<RpcStorage> ZksNamespace<RpcStorage> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        bridgehub_address: Address,
        bytecode_supplier_address: Address,
        storage: RpcStorage,
        genesis_input_source: Arc<dyn GenesisInputSource>,
        l2_chain_id: u64,
        l1_provider: DynProvider,
        gateway_provider: Option<DynProvider>,
        settlement_layer_intervals: SettlementLayerIntervals,
        optimistic_gateway_head: bool,
        committed_batch_provider: CommittedBatchProvider,
        eth_call_handler: EthCallHandler<RpcStorage>,
    ) -> Self {
        Self {
            bridgehub_address,
            bytecode_supplier_address,
            storage,
            genesis_input_source,
            l2_chain_id,
            l1_provider,
            gateway_provider,
            settlement_layer_intervals,
            optimistic_gateway_head,
            committed_batch_provider,
            commitment_tree_reader: InteropCommitmentTreeReader::new(eth_call_handler),
        }
    }
}

impl<RpcStorage: ReadRpcStorage> ZksNamespace<RpcStorage> {
    async fn get_l2_to_l1_log_proof_impl(
        &self,
        tx_hash: TxHash,
        index: Index,
        proof_target: LogProofTarget,
    ) -> ZksResult<Option<L2ToL1LogProof>> {
        let Some(tx_meta) = self.storage.repository().get_transaction_meta(tx_hash)? else {
            return Ok(None);
        };
        let block_number = tx_meta.block_number;
        let persisted_batch = self
            .storage
            .batch()
            .get_batch_by_block_number(block_number)?;
        // SYSCOIN: The finalized batch store is deliberately conservative. For interop only, a
        // Gateway execution is already a usable proof boundary: reconstruct from the live committed
        // batch index and later prove that this exact batch has executed on Gateway. Never apply this
        // fallback to withdrawals or direct-L1 MessageRoot proofs.
        let batch = match persisted_batch {
            Some(batch) => batch,
            None if self.optimistic_gateway_head
                && matches!(proof_target, LogProofTarget::MessageRoot) =>
            {
                let Some(committed_batch) = self
                    .committed_batch_provider
                    .get_batch_containing_block(block_number)
                else {
                    return Ok(None);
                };
                let committed_batch_number = committed_batch.number();
                let Some(interval) = self
                    .settlement_layer_intervals
                    .find_interval(committed_batch_number)
                else {
                    return Ok(None);
                };
                if !permits_optimistic_gateway_batch(
                    self.optimistic_gateway_head,
                    proof_target,
                    &interval.settlement_layer,
                ) {
                    return Ok(None);
                }
                PersistedBatch {
                    committed_batch,
                    execute_sl_block_number: None,
                }
            }
            None => return Ok(None),
        };

        let mut batch_index = None;
        let mut merkle_tree_leaves = vec![];
        let batch_number = batch.number();
        for block in batch.block_range.clone() {
            let Some(block) = self.storage.repository().get_block_by_number(block)? else {
                return Err(ZksError::BlockNotAvailable(block));
            };
            for block_tx_hash in block.unseal().body.transactions {
                let Some(receipt) = self
                    .storage
                    .repository()
                    .get_transaction_receipt(block_tx_hash)?
                else {
                    return Err(ZksError::TxNotAvailable(block_tx_hash));
                };
                let l2_to_l1_logs = receipt.into_l2_to_l1_logs();
                if block_tx_hash == tx_hash {
                    if index.0 >= l2_to_l1_logs.len() {
                        return Err(ZksError::IndexOutOfBounds(index.0, l2_to_l1_logs.len()));
                    }
                    batch_index.replace(merkle_tree_leaves.len() + index.0);
                }
                for l2_to_l1_log in l2_to_l1_logs {
                    merkle_tree_leaves.push(l2_to_l1_log.encode());
                }
            }
        }
        let l1_log_index =
            require_batch_log_index(batch_index, tx_hash, block_number, batch_number)?;

        let (local_root, proof) =
            MiniMerkleTree::new(merkle_tree_leaves.into_iter(), Some(L2_TO_L1_TREE_SIZE))
                .merkle_root_and_path(l1_log_index);

        let state = self.storage.state_view_at(*batch.block_range.end())?;
        let multichain_root = read_multichain_root(state);
        let root = keccak256([local_root.0, multichain_root.0].concat());
        // SYSCOIN: We need to check if the root is the same as the committed root.
        if root != batch.batch_info.l2_to_l1_logs_root_hash {
            return Err(anyhow::anyhow!(
                "reconstructed L2->L1 logs root {root:?} does not match committed root {:?} for batch #{}",
                batch.batch_info.l2_to_l1_logs_root_hash,
                batch_number
            )
            .into());
        }

        let log_leaf_proof = proof
            .into_iter()
            .chain(std::iter::once(multichain_root))
            .collect::<Vec<_>>();

        // SYSCOIN: Provider presence is not a per-batch routing signal. Resolve the recorded settlement
        // interval so Gateway and direct-L1 batches extend the MessageRoot proof correctly.
        let settlement_interval = self
            .settlement_layer_intervals
            .find_interval(batch_number)
            .ok_or_else(|| {
                ZksError::Batch(anyhow::anyhow!(
                    "no settlement-layer interval contains batch {batch_number}"
                ))
            })?;
        let batch_settles_on_gateway = matches!(
            &settlement_interval.settlement_layer,
            IntervalSettlementLayer::Gateway(_)
        );

        let (proof_extension, settlement_layer_block_number) = if batch_settles_on_gateway {
            let gateway_provider = self.gateway_provider.as_ref().ok_or_else(|| {
                ZksError::Batch(anyhow::anyhow!(
                    "batch {batch_number} settled on Gateway, but no historical Gateway provider is configured"
                ))
            })?;
            let execute_sl_block_number = if let Some(block_number) = batch.execute_sl_block_number
            {
                block_number
            } else {
                if !self.optimistic_gateway_head {
                    return Ok(None);
                }
                // SYSCOIN: The optimistic fallback is valid only after Gateway has executed the
                // source batch. Resolve the exact execution block so the MessageRoot proof is
                // anchored to the root emitted by that Gateway block, not to a later shared root.
                let latest_executed = settlement_interval
                    .proxy
                    .get_total_batches_executed(BlockId::latest())
                    .await
                    .context("read latest Gateway executed batch")?;
                if latest_executed < batch_number {
                    return Ok(None);
                }
                find_l1_execute_block_by_batch_number(
                    settlement_interval.proxy.clone(),
                    batch_number,
                )
                .await
                .context("resolve Gateway execution block")?
            };
            let extension = build_gateway_proof_extension(
                self.l2_chain_id,
                batch_number,
                execute_sl_block_number,
                matches!(proof_target, LogProofTarget::MessageRoot),
                gateway_provider,
            )
            .await
            // SYSCOIN: retain the nested V32 Gateway contract-call cause in RPC errors; the
            // generic context alone makes production proof failures indistinguishable.
            .map_err(|err| anyhow::anyhow!("build Gateway proof extension: {err:#}"))?;
            (Some(extension), Some(execute_sl_block_number))
        } else {
            match proof_target {
                LogProofTarget::MessageRoot => {
                    let last_block_number = *batch.block_range.end();
                    let last_block_replay_record = self
                        .storage
                        .replay_storage()
                        .get_replay_record(last_block_number)
                        .ok_or(ZksError::BlockNotAvailable(last_block_number))?;
                    if !last_block_replay_record
                        .protocol_version
                        .supports_l1_interop()
                    {
                        return Err(ZksError::MessageRootProofUnsupportedProtocolVersion {
                            batch_number,
                            required_protocol_version:
                                ProtocolSemanticVersion::MIN_VERSION_WITH_L1_INTEROP,
                            actual_protocol_version: last_block_replay_record.protocol_version,
                        });
                    }

                    let execute_sl_block_number =
                        batch.execute_sl_block_number.ok_or_else(|| {
                            ZksError::Batch(anyhow::anyhow!(
                                "batch {batch_number} has not been executed on L1 yet"
                            ))
                        })?;
                    let l1_message_root_address =
                        IBridgehub::new(self.bridgehub_address, &self.l1_provider)
                            .messageRoot()
                            .call()
                            .await
                            .context("bridgehub.messageRoot()")?;
                    let extension = build_message_root_proof_extension(
                        self.l2_chain_id,
                        batch_number,
                        execute_sl_block_number,
                        &self.l1_provider,
                        l1_message_root_address,
                    )
                    .await
                    .context("build MessageRoot proof extension")?;
                    (Some(extension), Some(execute_sl_block_number))
                }
                // Other targets (e.g. L1 withdrawal-finalization proofs) terminate at L1 as a
                // final node — there is no settlement layer above L1.
                _ => (None, None),
            }
        };

        let proof = assemble_log_proof(log_leaf_proof, proof_extension);

        Ok(Some(L2ToL1LogProof {
            batch_number,
            proof,
            root,
            id: l1_log_index as u32,
            settlement_layer_block_number,
        }))
    }

    fn get_block_metadata_by_number_impl(
        &self,
        block_number: u64,
    ) -> ZksResult<Option<BlockMetadata>> {
        // SYSCOIN: This metadata-only RPC must not deserialize the full replay record.
        let Some(block_context) = self.storage.replay_storage().get_context(block_number) else {
            return Ok(None);
        };

        let pubdata_price_per_byte = block_context.pubdata_price;
        let native_price = block_context.native_price;
        let execution_version = block_context.execution_version;
        Ok(Some(BlockMetadata {
            pubdata_price_per_byte,
            native_price,
            execution_version,
        }))
    }

    fn get_proof_impl(
        &self,
        address: Address,
        keys: &[B256],
        batch_number: u64,
    ) -> ZksResult<Option<BatchStorageProof>> {
        let Some(batch) = self.storage.batch().get_batch_by_number(batch_number)? else {
            return Ok(None);
        };
        let last_block_number = batch.last_block_number();

        let last_block_replay = self
            .storage
            .replay_storage()
            .get_replay_record(last_block_number)
            .with_context(|| {
                format!("missing last block {last_block_number} for batch #{batch_number}")
            })?;
        let block_hashes = last_block_replay.block_context.block_hashes;

        let last_block = self
            .storage
            .repository()
            .get_block_by_number(last_block_number)?
            .with_context(|| {
                format!("missing last block {last_block_number} for batch #{batch_number}")
            })?
            .into_inner();
        let last_block_header_for_hashing = alloy::consensus::Header {
            // `logs_bloom` must be zeroed out when computing block hashes due to how
            // block hashes are defined elsewhere in the codebase.
            logs_bloom: alloy::primitives::Bloom::default(),
            ..last_block.header
        };
        let last_block_hash = last_block_header_for_hashing.hash_slow();

        let last_256_block_hashes_blake = {
            let mut blocks_hasher = Blake2s256::new();
            for block_hash in &block_hashes.0[1..] {
                blocks_hasher.update(block_hash.to_be_bytes::<32>());
            }
            blocks_hasher.update(last_block_hash.as_slice());
            B256::from_slice(&blocks_hasher.finalize())
        };

        let address_for_keys = B160::from_be_bytes(address.into_array());
        let flat_keys: Vec<_> = keys
            .iter()
            .map(|account_key| {
                let flat_key = derive_flat_storage_key(&address_for_keys, &account_key.0.into());
                B256::new(flat_key.as_u8_array())
            })
            .collect();
        // We query tree version by the *block* number because the tree is updated on each block,
        // rather than once per batch.
        let Some((flat_proofs, tree_output)) = self
            .storage
            .tree()
            .prove_flat(last_block_number, &flat_keys)?
        else {
            return Ok(None);
        };

        // Swap flat keys in the proofs back to address-scoped keys
        let storage_proofs: Vec<_> = flat_proofs
            .into_iter()
            .zip(keys)
            .map(|(proof, &key)| StorageSlotProof {
                key: AddressScopedKey(key),
                proof: proof.proof,
            })
            .collect();

        let state_commitment_preimage = StateCommitmentPreimage {
            next_free_slot: U64::from(tree_output.leaf_count),
            block_number: U64::from(last_block_number),
            last_256_block_hashes_blake,
            last_block_timestamp: U64::from(last_block.header.timestamp),
        };

        let recovered = state_commitment_preimage.hash(tree_output.root_hash);
        if batch.batch_info.state_commitment != recovered {
            let err = anyhow::anyhow!(
                "Mismatch between stored ({stored:?}) and recovered ({recovered:?}) state commitments \
                 for batch #{batch_number}; preimage = {state_commitment_preimage:?}, tree_output = {tree_output:?}",
                stored = batch.batch_info.state_commitment
            );
            return Err(err.into());
        }

        let l1_verification_data = L1VerificationData {
            batch_number,
            number_of_layer1_txs: batch.batch_info.number_of_layer1_txs,
            priority_operations_hash: batch.batch_info.priority_operations_hash,
            dependency_roots_rolling_hash: batch.batch_info.dependency_roots_rolling_hash,
            l2_to_l1_logs_root_hash: batch.batch_info.l2_to_l1_logs_root_hash,
            commitment: batch.batch_info.commitment,
        };

        Ok(Some(BatchStorageProof {
            address,
            state_commitment_preimage,
            storage_proofs,
            l1_verification_data,
        }))
    }

    /// Index of the low-nullifier leaf for `value` (the predecessor used when inserting `value`)
    /// against the commitment tree as of `block_number`. `None` if no such leaf exists.
    fn get_imt_low_nullifier_index_impl(
        &self,
        value: U256,
        block_number: u64,
    ) -> ZksResult<Option<u64>> {
        let tree = self.commitment_tree_reader.read(block_number.into())?;
        Ok(tree.find_low_nullifier_index(value))
    }

    /// Reconstructs the IMT inclusion proof for `commit_value` at `block_number`.
    ///
    /// The response includes the stored leaf preimage, its insertion-order index, and one sibling
    /// per level in the dynamic-height tree. `None` means the requested value was absent at that
    /// historical block.
    fn get_imt_inclusion_proof_impl(
        &self,
        commit_value: U256,
        block_number: u64,
    ) -> ZksResult<Option<ImtInclusionProof>> {
        let tree = self.commitment_tree_reader.read(block_number.into())?;
        let Some(leaf_index) = tree.find_value_index(commit_value) else {
            return Ok(None);
        };
        let leaf = tree.leaves()[leaf_index as usize];
        let root = tree.root();
        let path = tree.merkle_path(leaf_index);

        // Self-verify the produced path against the root (same walk the on-chain `verifyInclusion`
        // performs) so an engine bug surfaces here instead of as an on-chain `executeAtomicBundle`
        // revert. A mismatch is an internal error, not a "leaf absent" (None) result.
        let recomputed = calculate_root(&path, leaf_index, indexed_leaf_hash(&leaf));
        if recomputed != root {
            return Err(InteropCommitmentTreeError::ProofMismatch {
                commit_value,
                leaf_index,
                recomputed_root: recomputed,
                tree_root: root,
            }
            .into());
        }

        Ok(Some(ImtInclusionProof {
            chain_imt_root: root,
            leaf: ImtLeaf {
                value: leaf.value,
                next_index: leaf.next_index,
                next_value: leaf.next_value,
            },
            imt_leaf_index: leaf_index,
            imt_proof: path,
        }))
    }
}

#[async_trait]
impl<RpcStorage: ReadRpcStorage> ZksApiServer for ZksNamespace<RpcStorage> {
    fn get_bridgehub_contract(&self) -> RpcResult<Address> {
        Ok(self.bridgehub_address)
    }

    fn get_bytecode_supplier_contract(&self) -> RpcResult<Address> {
        Ok(self.bytecode_supplier_address)
    }

    async fn get_l2_to_l1_log_proof(
        &self,
        tx_hash: TxHash,
        index: Index,
        proof_target: Option<LogProofTarget>,
    ) -> RpcResult<Option<L2ToL1LogProof>> {
        self.get_l2_to_l1_log_proof_impl(tx_hash, index, proof_target.unwrap_or_default())
            .await
            .to_rpc_result()
    }

    async fn get_genesis(&self) -> RpcResult<GenesisInput> {
        self.genesis_input_source
            .genesis_input()
            .await
            .map_err(ZksError::GenesisSource)
            .to_rpc_result()
    }

    fn get_block_metadata_by_number(&self, block_number: u64) -> RpcResult<Option<BlockMetadata>> {
        self.get_block_metadata_by_number_impl(block_number)
            .to_rpc_result()
    }

    fn get_proof(
        &self,
        account: Address,
        keys: Vec<B256>,
        batch_number: u64,
    ) -> RpcResult<Option<BatchStorageProof>> {
        self.get_proof_impl(account, &keys, batch_number)
            .to_rpc_result()
    }

    async fn get_imt_inclusion_proof(
        &self,
        commit_value: U256,
        block_number: u64,
    ) -> RpcResult<Option<ImtInclusionProof>> {
        self.get_imt_inclusion_proof_impl(commit_value, block_number)
            .to_rpc_result()
    }

    async fn get_imt_low_nullifier_index(
        &self,
        value: U256,
        block_number: u64,
    ) -> RpcResult<Option<u64>> {
        self.get_imt_low_nullifier_index_impl(value, block_number)
            .to_rpc_result()
    }
}

/// `zks` namespace result type.
pub type ZksResult<Ok> = Result<Ok, ZksError>;

/// General `zks` namespace errors
#[derive(Debug, thiserror::Error)]
pub enum ZksError {
    /// Historical block could not be found on this node (e.g., pruned).
    #[error("historical block {0} is not available")]
    BlockNotAvailable(BlockNumber),
    /// Historical transaction could not be found on this node (e.g., pruned).
    #[error("historical transaction {0} is not available")]
    TxNotAvailable(TxHash),
    /// Transaction metadata and reconstructed batch contents disagree.
    #[error(
        "transaction {tx_hash} from block {block_number} is absent from reconstructed batch {batch_number}"
    )]
    TransactionNotInBatch {
        tx_hash: TxHash,
        block_number: BlockNumber,
        batch_number: u64,
    },
    /// Historical transaction could not be found on this node (e.g., pruned).
    #[error(
        "provided L2->L1 log index ({0}) does not exist; there are only {1} L2->L1 logs in the transaction"
    )]
    IndexOutOfBounds(usize, usize),
    /// The requested batch predates the direct-L1 MessageRoot proof format.
    #[error(
        "MessageRoot proofs require protocol version {required_protocol_version}; batch \
         {batch_number} uses {actual_protocol_version}"
    )]
    MessageRootProofUnsupportedProtocolVersion {
        batch_number: u64,
        required_protocol_version: ProtocolSemanticVersion,
        actual_protocol_version: ProtocolSemanticVersion,
    },
    #[error(transparent)]
    CommitmentTree(#[from] InteropCommitmentTreeError),

    #[error(transparent)]
    Batch(#[from] anyhow::Error),
    #[error(transparent)]
    Repository(#[from] RepositoryError),
    #[error(transparent)]
    GenesisSource(anyhow::Error),
    #[error(transparent)]
    State(#[from] StateError),
}

#[cfg(test)]
mod tests {
    use super::{
        IntervalSettlementLayer, LogProofTarget, ZksError, permits_optimistic_gateway_batch,
        require_batch_log_index,
    };
    use alloy::primitives::B256;

    #[test]
    fn optimistic_batch_fallback_is_gateway_message_root_only() {
        let gateway = IntervalSettlementLayer::Gateway(506);
        let l1 = IntervalSettlementLayer::L1;

        assert!(permits_optimistic_gateway_batch(
            true,
            LogProofTarget::MessageRoot,
            &gateway,
        ));
        assert!(!permits_optimistic_gateway_batch(
            false,
            LogProofTarget::MessageRoot,
            &gateway,
        ));
        assert!(!permits_optimistic_gateway_batch(
            true,
            LogProofTarget::L1BatchRoot,
            &gateway,
        ));
        assert!(!permits_optimistic_gateway_batch(
            true,
            LogProofTarget::MessageRoot,
            &l1,
        ));
    }

    // SYSCOIN: Inconsistent RPC indexes are returned as typed errors rather than panicking.
    #[test]
    fn missing_transaction_in_reconstructed_batch_is_typed_error() {
        let tx_hash = B256::repeat_byte(0x11);
        let err = require_batch_log_index(None, tx_hash, 42, 7).unwrap_err();

        assert!(matches!(
            err,
            ZksError::TransactionNotInBatch {
                tx_hash: actual_tx_hash,
                block_number: 42,
                batch_number: 7,
            } if actual_tx_hash == tx_hash
        ));
    }
}
