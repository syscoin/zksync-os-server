use crate::tree::{EfficientTreeAdapter, RawLeafProof, TREE_DEPTH, VersionedMerkleTree};
use crate::{NativeBatchBlock, NativeBatchRunOutput};
use alloy::primitives::{Address, B256, ruint::aliases::B160};
use anyhow::Context as _;
use blake2::{Blake2s256, Digest};
use std::collections::VecDeque;
use zk_ee::common_structs::{ProofData, da_commitment_scheme::DACommitmentScheme};
use zk_ee::system::metadata::chain_config::{ChainConfig, DEFAULT_MAX_TX_GAS_LIMIT};
use zk_ee::system::metadata::zk_metadata::{BlockHashes, BlockMetadataFromOracle};
use zk_ee::utils::Bytes32;
use zk_os_basic_system::system_implementation::flat_storage_model::FlatStorageLeaf;
use zk_os_forward_system::run::{
    BatchBlockInput, BatchState as ForwardBatchState, LeafProof,
    PreimageSource as ForwardPreimageSource, ReadStorage as ForwardReadStorage, ReadStorageTree,
    StorageCommitment, generate_batch_proof_input,
};
use zksync_os_batch_types::syscoin_edge_da_refs_for_blocks;
use zksync_os_interface::traits::TxListSource;
use zksync_os_merkle_tree::{MerkleTree, RocksDBWrapper};
use zksync_os_storage_api::{BlockContext, ReadStateHistory, ReplayRecord, ViewState};
use zksync_os_types::{
    ProtocolSemanticVersion, PubdataMode, SYSCOIN_MAX_TX_GAS_LIMIT, ZksyncOsEncode,
    block_output_hash, state_commitment_hash, syscoin_chain_config_hash,
};

/// SYSCOIN: The fixed chain config all canonical V32 native batch runs use. Its hash is part of
/// the batch public input, so proof verification must reconstruct it identically.
pub(crate) fn chain_config(chain_id: u64) -> anyhow::Result<ChainConfig> {
    anyhow::ensure!(
        DEFAULT_MAX_TX_GAS_LIMIT == SYSCOIN_MAX_TX_GAS_LIMIT,
        "zksync-os default max transaction gas limit changed: expected {}, found {}",
        SYSCOIN_MAX_TX_GAS_LIMIT,
        DEFAULT_MAX_TX_GAS_LIMIT,
    );
    ChainConfig::new(chain_id, false, SYSCOIN_MAX_TX_GAS_LIMIT)
        .map_err(|err| anyhow::anyhow!("invalid chain config: {err:?}"))
}

pub(crate) fn chain_config_hash(chain_id: u64) -> anyhow::Result<B256> {
    let native_hash = B256::from(chain_config(chain_id)?.hash());
    let canonical_hash = syscoin_chain_config_hash(chain_id);
    anyhow::ensure!(
        native_hash == canonical_hash,
        "zksync-os chain config hash drift: native {native_hash}, canonical {canonical_hash}",
    );
    Ok(canonical_hash)
}

pub(crate) fn generate_batch_run<ReadState: ReadStateHistory>(
    blocks: &[NativeBatchBlock<'_>],
    read_state: &ReadState,
    merkle_tree: MerkleTree<RocksDBWrapper>,
    pubdata_mode: PubdataMode,
    compact_edge_da_commit_target: Address,
) -> anyhow::Result<NativeBatchRunOutput> {
    anyhow::ensure!(
        !blocks.is_empty(),
        "batch prover input requires at least one block",
    );

    let first_replay_record = blocks[0].replay_record;
    // The chain config is frozen for the whole batch; chain id lives there now rather than in
    // per-block metadata. All blocks in a batch share the same chain.
    let chain_id = first_replay_record.block_context.chain_id;
    // SYSCOIN: Reject metadata drift that final-v0.4 no longer repeats in each block input.
    validate_batch_replay_identity(
        chain_id,
        &first_replay_record.protocol_version,
        blocks.iter().enumerate().map(|(index, block)| {
            (
                index,
                block.replay_record.block_context.chain_id,
                &block.replay_record.protocol_version,
            )
        }),
    )?;
    let chain_config = chain_config(chain_id)?;
    let first_state_version = first_replay_record
        .block_context
        .block_number
        .checked_sub(1)
        .context("batch prover input requires a parent state version")?;
    let (root_hash, leaf_count) = merkle_tree
        .root_info(first_state_version)?
        .context("missing Merkle tree state for the first V32 batch block")?;

    let initial_proof_data = ProofData {
        state_root_view: StorageCommitment {
            root: bytes32_from_b256(root_hash),
            next_free_slot: leaf_count,
        },
        last_block_timestamp: first_replay_record.previous_block_timestamp,
    };

    let state_views = blocks
        .iter()
        .map(|block| {
            let state_version = block
                .replay_record
                .block_context
                .block_number
                .checked_sub(1)
                .context("batch prover input requires a parent state version")?;
            read_state
                .state_view_at(state_version)
                .map_err(anyhow::Error::from)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    // Serve tree queries from each block's batch update proof (zero I/O), falling back to
    // Merkle proof queries against the tree DB only for data the proof doesn't cover.
    let trees = blocks
        .iter()
        .map(|block| {
            let tree_version = block
                .replay_record
                .block_context
                .block_number
                .checked_sub(1)
                .context("batch prover input requires a parent tree version")?;
            Ok(EfficientTreeAdapter::new(
                block.tree_data.clone(),
                VersionedMerkleTree::new(merkle_tree.clone(), tree_version),
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let batch_state = HistoricalBatchState::new(state_views, trees);

    let block_inputs = blocks
        .iter()
        .map(|block| batch_block_input(block.replay_record))
        .collect::<Vec<_>>();

    let batch_run = generate_batch_proof_input(
        initial_proof_data,
        batch_state,
        block_inputs,
        da_commitment_scheme(pubdata_mode)?,
        chain_config,
    )
    .map_err(|err| anyhow::anyhow!("native batch run failed: {err:?}"))?;

    // SYSCOIN: HistoricalBatchState advances through canonical snapshots, not guest writes.
    // Refuse any divergent native block before accepting its witness or batch commitment.
    anyhow::ensure!(
        batch_run.block_outputs.len() == blocks.len(),
        "native batch output count mismatch"
    );
    for (block, native) in blocks.iter().zip(&batch_run.block_outputs) {
        anyhow::ensure!(
            native_output_matches(
                native,
                block.block_output,
                block.replay_record.block_output_hash
            ),
            "native batch block {} output mismatch with canonical replay",
            block.replay_record.block_context.block_number,
        );
    }
    let last = blocks.last().expect("nonempty batch checked above");
    let (final_root, final_leaves) = merkle_tree
        .root_info(last.replay_record.block_context.block_number)?
        .context("missing canonical Merkle tree state for the last V32 batch block")?;
    let expected_state_after = canonical_state_after(
        &last.replay_record.block_context,
        last.block_output.header.hash(),
        final_root,
        final_leaves,
    );
    anyhow::ensure!(
        b256_from_bytes32(batch_run.batch_public_input.state_after) == expected_state_after,
        "native batch final state commitment mismatch with canonical Merkle tree",
    );

    let batch_output = batch_run.batch_output;
    let batch_public_input = batch_run.batch_public_input;
    let upgrade_tx_hash = b256_from_bytes32(batch_output.upgrade_tx_hash);
    let upgrade_tx_hash = (upgrade_tx_hash != B256::ZERO).then_some(upgrade_tx_hash);
    let (edge_da_refs_input, edge_da_refs_root) = syscoin_edge_da_refs_for_blocks(
        blocks.iter().map(|block| {
            (
                block.block_output,
                block.replay_record.transactions.as_slice(),
            )
        }),
        compact_edge_da_commit_target,
        chain_id,
    )?;
    let proven_edge_da_refs_root = b256_from_bytes32(batch_output.edge_da_refs_root);
    anyhow::ensure!(
        proven_edge_da_refs_root == edge_da_refs_root,
        "native batch edge-DA root mismatch: batch program produced {proven_edge_da_refs_root}, replay reconstruction produced {edge_da_refs_root}",
    );

    Ok(NativeBatchRunOutput {
        prover_input: batch_run.prover_input,
        pubdata: batch_run.pubdata,
        previous_state_commitment: b256_from_bytes32(batch_public_input.state_before),
        batch_public_input_hash: B256::from(batch_public_input.hash()),
        new_state_commitment: b256_from_bytes32(batch_public_input.state_after),
        da_commitment: b256_from_bytes32(batch_output.pubdata_commitment),
        number_of_layer1_txs: u256_to_u64(
            "batch_output.number_of_layer_1_txs",
            batch_output.number_of_layer_1_txs,
        )?,
        number_of_layer2_txs: u256_to_u64(
            "batch_output.number_of_layer_2_txs",
            batch_output.number_of_layer_2_txs,
        )?,
        priority_operations_hash: b256_from_bytes32(batch_output.priority_operations_hash),
        dependency_roots_rolling_hash: b256_from_bytes32(batch_output.interop_roots_rolling_hash),
        l2_to_l1_logs_root_hash: b256_from_bytes32(batch_output.l2_logs_tree_root),
        first_block_timestamp: batch_output.first_block_timestamp,
        last_block_timestamp: batch_output.last_block_timestamp,
        chain_id,
        sl_chain_id: u256_to_u64(
            "batch_output.settlement_layer_chain_id",
            batch_output.settlement_layer_chain_id,
        )?,
        upgrade_tx_hash,
        edge_da_refs_input,
        edge_da_refs_root,
    })
}

fn native_output_matches(
    native: &zk_os_forward_system::run::output::BlockOutput,
    canonical: &zksync_os_types::BlockOutput,
    replay_hash: B256,
) -> bool {
    block_output_hash(
        native.header.hash(),
        &native.tx_results,
        &native.storage_writes,
    ) == replay_hash
        && block_output_hash(
            canonical.header.hash(),
            &canonical.tx_results,
            &canonical.storage_writes,
        ) == replay_hash
}

// SYSCOIN: Use only the persisted final tree and replay/header metadata, never the guest's root.
fn canonical_state_after(
    last_context: &BlockContext,
    last_block_hash: B256,
    tree_root: B256,
    leaf_count: u64,
) -> B256 {
    let mut blocks_hasher = Blake2s256::new();
    for block_hash in &last_context.block_hashes.0[1..] {
        blocks_hasher.update(block_hash.to_be_bytes::<32>());
    }
    blocks_hasher.update(last_block_hash);
    state_commitment_hash(
        tree_root,
        leaf_count,
        last_context.block_number,
        B256::from_slice(&blocks_hasher.finalize()),
        last_context.timestamp,
    )
}

fn validate_batch_replay_identity<'a>(
    chain_id: u64,
    protocol_version: &ProtocolSemanticVersion,
    identities: impl IntoIterator<Item = (usize, u64, &'a ProtocolSemanticVersion)>,
) -> anyhow::Result<()> {
    for (index, block_chain_id, block_protocol_version) in identities {
        anyhow::ensure!(
            block_chain_id == chain_id,
            "batch block {index} chain id mismatch: expected {chain_id}, found {block_chain_id}",
        );
        anyhow::ensure!(
            block_protocol_version == protocol_version,
            "batch block {index} protocol version mismatch: expected {protocol_version}, found {block_protocol_version}",
        );
    }
    Ok(())
}

fn batch_block_input(replay_record: &ReplayRecord) -> BatchBlockInput<TxListSource> {
    BatchBlockInput {
        block_context: BlockMetadataFromOracle {
            block_number: replay_record.block_context.block_number,
            block_hashes: BlockHashes(replay_record.block_context.block_hashes.0),
            timestamp: replay_record.block_context.timestamp,
            eip1559_basefee: replay_record.block_context.eip1559_basefee,
            pubdata_price: replay_record.block_context.pubdata_price,
            native_price: replay_record.block_context.native_price,
            coinbase: B160::from_be_bytes(replay_record.block_context.coinbase.into_array()),
            gas_limit: replay_record.block_context.gas_limit,
            pubdata_limit: replay_record.block_context.pubdata_limit,
            mix_hash: replay_record.block_context.mix_hash,
            blob_fee: replay_record.block_context.blob_fee,
        },
        tx_source: TxListSource {
            transactions: replay_record
                .transactions
                .iter()
                .cloned()
                .map(|tx| tx.encode())
                .collect::<VecDeque<_>>(),
        },
    }
}

fn da_commitment_scheme(pubdata_mode: PubdataMode) -> anyhow::Result<DACommitmentScheme> {
    (pubdata_mode.da_commitment_scheme() as u8)
        .try_into()
        .map_err(|_| anyhow::anyhow!("failed to convert DA commitment scheme"))
}

fn u256_to_u64<T>(label: &str, value: T) -> anyhow::Result<u64>
where
    T: TryInto<u64> + Copy + std::fmt::Display,
{
    value
        .try_into()
        .map_err(|_| anyhow::anyhow!("{label} does not fit into u64: {value}"))
}

fn bytes32_from_b256(value: B256) -> Bytes32 {
    Bytes32::from(value.0)
}

fn b256_from_bytes32(value: Bytes32) -> B256 {
    B256::from(value.as_u8_array())
}

#[derive(Debug)]
struct HistoricalBatchState<SV> {
    state_views: Vec<SV>,
    trees: Vec<EfficientTreeAdapter>,
    cursor: usize,
}

impl<SV> HistoricalBatchState<SV> {
    fn new(state_views: Vec<SV>, trees: Vec<EfficientTreeAdapter>) -> Self {
        assert_eq!(state_views.len(), trees.len());
        Self {
            state_views,
            trees,
            cursor: 0,
        }
    }
}

impl<SV: ViewState> ForwardReadStorage for HistoricalBatchState<SV> {
    fn read(&mut self, key: Bytes32) -> Option<Bytes32> {
        self.state_views[self.cursor]
            .read(b256_from_bytes32(key))
            .map(bytes32_from_b256)
    }
}

impl<SV: ViewState> ForwardPreimageSource for HistoricalBatchState<SV> {
    fn get_preimage(&mut self, hash: Bytes32) -> Option<Vec<u8>> {
        self.state_views[self.cursor].get_preimage(b256_from_bytes32(hash))
    }
}

impl<SV: ViewState> ReadStorageTree for HistoricalBatchState<SV> {
    fn tree_index(&mut self, key: Bytes32) -> Option<u64> {
        self.trees[self.cursor].tree_index(b256_from_bytes32(key))
    }

    fn merkle_proof(&mut self, tree_index: u64) -> LeafProof {
        map_leaf_proof(self.trees[self.cursor].merkle_proof(tree_index))
    }

    fn prev_tree_index(&mut self, key: Bytes32) -> u64 {
        self.trees[self.cursor].prev_tree_index(b256_from_bytes32(key))
    }
}

fn map_leaf_proof(proof: RawLeafProof) -> LeafProof {
    let leaf = FlatStorageLeaf {
        key: bytes32_from_b256(proof.key),
        value: bytes32_from_b256(proof.value),
        next: proof.next_index,
    };
    let mut merkle_path = Box::new([Bytes32::default(); TREE_DEPTH as usize]);
    for (slot, hash) in merkle_path.iter_mut().zip(proof.path.iter()) {
        *slot = bytes32_from_b256(*hash);
    }
    LeafProof::new(proof.index, leaf, merkle_path)
}

impl<SV: ViewState> ForwardBatchState for HistoricalBatchState<SV> {
    fn apply_block_output(
        &mut self,
        _block_output: &zk_os_forward_system::run::output::BlockOutput,
    ) {
        if self.cursor + 1 < self.state_views.len() {
            self.cursor += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        canonical_state_after, chain_config_hash, native_output_matches,
        validate_batch_replay_identity,
    };
    use alloy::consensus::{Header, Sealable};
    use alloy::primitives::{Address, B256, U256, b256, keccak256};
    use zksync_os_interface::types::{ExecutionOutput, ExecutionResult, StorageWrite, TxOutput};
    use zksync_os_storage_api::BlockContext;
    use zksync_os_types::{BlockOutput, BlockPubdata};
    use zksync_os_types::{ProtocolSemanticVersion, syscoin_chain_config_hash};

    #[test]
    fn native_chain_config_matches_the_canonical_server_commitment() {
        let chain_id = 57;
        assert_eq!(
            chain_config_hash(chain_id).unwrap(),
            syscoin_chain_config_hash(chain_id)
        );
    }

    #[test]
    fn batch_replay_identity_rejects_later_chain_or_protocol_drift() {
        let canonical = ProtocolSemanticVersion::canonical_genesis_version();
        let other_protocol = ProtocolSemanticVersion::new(0, 31, 1);

        validate_batch_replay_identity(57, &canonical, [(0, 57, &canonical), (1, 57, &canonical)])
            .unwrap();

        let chain_err = validate_batch_replay_identity(
            57,
            &canonical,
            [(0, 57, &canonical), (1, 58, &canonical)],
        )
        .unwrap_err();
        assert!(chain_err.to_string().contains("chain id mismatch"));

        let protocol_err = validate_batch_replay_identity(
            57,
            &canonical,
            [(0, 57, &canonical), (1, 57, &other_protocol)],
        )
        .unwrap_err();
        assert!(
            protocol_err
                .to_string()
                .contains("protocol version mismatch")
        );
    }

    #[test]
    fn native_output_check_preserves_replay_encoding_and_rejects_drift() {
        let canonical = BlockOutput {
            header: Header::default().seal_slow(),
            tx_results: vec![Ok(TxOutput {
                execution_result: ExecutionResult::Success(ExecutionOutput::Call(vec![])),
                gas_used: 21_000,
                gas_refunded: 0,
                computational_native_used: 0,
                native_used: 0,
                pubdata_used: 0,
                contract_address: None,
                logs: vec![],
                l2_to_l1_logs: vec![],
                storage_writes: vec![],
            })],
            storage_writes: vec![StorageWrite {
                key: B256::repeat_byte(0x11),
                value: B256::repeat_byte(0x22),
                account: Address::ZERO,
                account_key: B256::ZERO,
            }],
            account_diffs: vec![],
            published_preimages: vec![],
            pubdata: BlockPubdata::new(0),
            computational_native_used: 0,
        };
        // SYSCOIN: Fixed concatenation from the pre-existing sequencer codec; no wire change.
        let replay_hash = keccak256(
            [
                canonical.header.hash().as_slice(),
                &[1],
                &21_000u64.to_be_bytes(),
                &[0x11; 32],
                &[0x22; 32],
            ]
            .concat(),
        );
        let mut native = zk_os_forward_system::run::output::BlockOutput {
            header: canonical.header.clone(),
            tx_results: canonical.tx_results.clone(),
            storage_writes: canonical.storage_writes.clone(),
            account_diffs: vec![],
            published_preimages: vec![],
            pubdata_used: 0,
            computational_native_used: 0,
        };
        assert!(native_output_matches(&native, &canonical, replay_hash));
        assert!(!native_output_matches(&native, &canonical, B256::ZERO));
        native.tx_results[0].as_mut().unwrap().gas_used += 1;
        assert!(!native_output_matches(&native, &canonical, replay_hash));
        native.tx_results = canonical.tx_results.clone();
        native.tx_results[0].as_mut().unwrap().execution_result = ExecutionResult::Revert(vec![]);
        assert!(!native_output_matches(&native, &canonical, replay_hash));
        native.tx_results = canonical.tx_results.clone();
        native.storage_writes[0].value = B256::ZERO;
        assert!(!native_output_matches(&native, &canonical, replay_hash));
        native.storage_writes = canonical.storage_writes.clone();
        let mut wrong_canonical = canonical.clone();
        wrong_canonical.storage_writes[0].key = B256::ZERO;
        assert!(!native_output_matches(
            &native,
            &wrong_canonical,
            replay_hash
        ));
        native.header = Header {
            number: 1,
            ..Default::default()
        }
        .seal_slow();
        assert!(!native_output_matches(&native, &canonical, replay_hash));
    }

    #[test]
    fn canonical_final_state_binds_tree_and_advances_blockhash_window() {
        let mut context = BlockContext {
            block_number: 300,
            timestamp: 1_700_000_000,
            ..Default::default()
        };
        for (index, hash) in context.block_hashes.0.iter_mut().enumerate() {
            *hash = U256::from(index + 1);
        }
        let root = B256::repeat_byte(0x11);
        let block_hash = B256::repeat_byte(0x22);
        // SYSCOIN: Independently generated with OpenSSL's BLAKE2s via Node crypto.
        let expected = b256!("518c50a780a1fb7c1b2529fff507344a9d17ddb656af853966e77cefb35c5836");
        assert_eq!(
            canonical_state_after(&context, block_hash, root, 17),
            expected
        );
        assert_ne!(
            canonical_state_after(&context, block_hash, B256::ZERO, 17),
            expected
        );
        assert_ne!(
            canonical_state_after(&context, block_hash, root, 18),
            expected
        );
        assert_ne!(
            canonical_state_after(&context, B256::ZERO, root, 17),
            expected
        );
        context.block_hashes.0[0] = U256::ZERO;
        assert_eq!(
            canonical_state_after(&context, block_hash, root, 17),
            expected
        );
        context.block_hashes.0[255] += U256::ONE;
        assert_ne!(
            canonical_state_after(&context, block_hash, root, 17),
            expected
        );
        context.block_hashes.0[255] -= U256::ONE;
        context.timestamp += 1;
        assert_ne!(
            canonical_state_after(&context, block_hash, root, 17),
            expected
        );
        context.timestamp -= 1;
        context.block_number += 1;
        assert_ne!(
            canonical_state_after(&context, block_hash, root, 17),
            expected
        );
    }
}
