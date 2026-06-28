use alloy::consensus::BlobTransactionSidecar;
use alloy::primitives::{Address, B256, BlockNumber, U256, keccak256};
use alloy::sol_types::{SolCall, SolValue};
use anyhow::ensure;
use blake2::{Blake2s256, Digest};
use serde::{Deserialize, Serialize};
use std::ops;
use std::ops::{Deref, DerefMut};
use zksync_os_contract_interface::models::{CommitBatchInfo, StoredBatchInfo};
use zksync_os_contract_interface::{IExecutor, IMultisigCommitter};
use zksync_os_merkle_tree_api::TreeBatchOutput;
use zksync_os_mini_merkle_tree::MiniMerkleTree;
use zksync_os_types::{
    BlockOutput, L2_TO_L1_TREE_SIZE, L2ToL1Log, ProtocolSemanticVersion, PubdataMode, ZkEnvelope,
    ZkTransaction,
};

const PUBDATA_SOURCE_CALLDATA: u8 = 0;

const BLOB_CHUNK_SIZE: usize = 31;
// SYSCOIN: Syscoin Bitcoin DA accepts up to 2 MiB per blob and up to 32 blobs per block.
pub const SYSCOIN_DA_BYTES_PER_BLOB: usize = 2 * 1024 * 1024;
pub const SYSCOIN_DA_MAX_BLOBS_PER_BATCH: usize = 32;
pub const SYSCOIN_DA_MAX_ENCODED_BYTES_PER_BATCH: usize =
    SYSCOIN_DA_BYTES_PER_BLOB * SYSCOIN_DA_MAX_BLOBS_PER_BATCH;
pub const SYSCOIN_DA_MAX_BLOB_PUBDATA_BYTES: usize =
    SYSCOIN_DA_MAX_ENCODED_BYTES_PER_BATCH - BLOB_CHUNK_SIZE;
// SYSCOIN: domain separator for compact edge DA references committed by Gateway.
const SYSCOIN_EDGE_DA_REFS_DOMAIN: &[u8] = b"SYSCOIN_EDGE_DA_REFS_V1";
// SYSCOIN: RelayedSLDAValidator compact-ref message version.
const SYSCOIN_RELAYED_EDGE_DA_VALIDATOR_VERSION: u8 = 1;
const ABI_WORD: usize = 32;
const SYSCOIN_EDGE_DA_REF_HEAD_BYTES: usize = ABI_WORD * 5;

// SYSCOIN: compact reference to edge-chain pubdata that was published directly to Bitcoin DA.
pub struct SyscoinEdgeDaRef<'a> {
    pub edge_chain_id: u64,
    pub edge_batch_number: u64,
    pub edge_da_commitment: B256,
    pub blob_version_hashes: &'a [u8],
}

// SYSCOIN: hash one edge DA ref with its chain/batch context so blob hashes cannot be replayed
// across edge chains or batches.
fn syscoin_edge_da_ref_hash(edge_ref: SyscoinEdgeDaRef<'_>) -> B256 {
    assert!(
        edge_ref.blob_version_hashes.len().is_multiple_of(32),
        "Syscoin edge DA refs must be a concatenation of 32-byte blob hashes"
    );

    let mut preimage = Vec::with_capacity(
        SYSCOIN_EDGE_DA_REFS_DOMAIN.len() + 32 * 4 + edge_ref.blob_version_hashes.len(),
    );
    preimage.extend(SYSCOIN_EDGE_DA_REFS_DOMAIN);
    preimage.extend(U256::from(edge_ref.edge_chain_id).to_be_bytes::<32>());
    preimage.extend(U256::from(edge_ref.edge_batch_number).to_be_bytes::<32>());
    preimage.extend(edge_ref.edge_da_commitment.as_slice());
    preimage.extend(U256::from(edge_ref.blob_version_hashes.len() / 32).to_be_bytes::<32>());
    preimage.extend(edge_ref.blob_version_hashes);
    keccak256(preimage)
}

// SYSCOIN: parse the concatenated compact edge DA ref messages carried in Gateway
// commit calldata for final-L1 DA checks.
pub fn syscoin_edge_da_refs_from_input(input: &[u8]) -> Option<Vec<SyscoinEdgeDaRef<'_>>> {
    let mut refs = Vec::new();
    let mut remaining = input;
    while !remaining.is_empty() {
        let (edge_ref, consumed) = parse_syscoin_edge_da_ref_message_prefix(remaining)?;
        refs.push(edge_ref);
        remaining = &remaining[consumed..];
    }
    Some(refs)
}

// SYSCOIN: parse abi.encode(uint8 version, uint256 chainId, uint256 batchNumber,
// bytes32 daCommitment, bytes blobHashes) emitted by the compact Gateway DA validator.
fn parse_syscoin_edge_da_ref_message(message: &[u8]) -> Option<SyscoinEdgeDaRef<'_>> {
    let (edge_ref, consumed) = parse_syscoin_edge_da_ref_message_prefix(message)?;
    if consumed != message.len() {
        return None;
    }
    Some(edge_ref)
}

fn parse_syscoin_edge_da_ref_message_prefix(
    message: &[u8],
) -> Option<(SyscoinEdgeDaRef<'_>, usize)> {
    if message.len() < SYSCOIN_EDGE_DA_REF_HEAD_BYTES + ABI_WORD {
        return None;
    }
    if message[..31] != [0u8; 31] || message[31] != SYSCOIN_RELAYED_EDGE_DA_VALIDATOR_VERSION {
        return None;
    }
    let edge_chain_id = u256_word_to_u64(&message[ABI_WORD..ABI_WORD * 2])?;
    let edge_batch_number = u256_word_to_u64(&message[ABI_WORD * 2..ABI_WORD * 3])?;
    let edge_da_commitment = B256::from_slice(&message[ABI_WORD * 3..ABI_WORD * 4]);
    let blob_hashes_offset = u256_word_to_usize(&message[ABI_WORD * 4..ABI_WORD * 5])?;
    if blob_hashes_offset != SYSCOIN_EDGE_DA_REF_HEAD_BYTES {
        return None;
    }
    let blob_hashes_len_offset = blob_hashes_offset;
    let blob_hashes_start = blob_hashes_len_offset + ABI_WORD;
    if message.len() < blob_hashes_start {
        return None;
    }
    let blob_hashes_len = u256_word_to_usize(&message[blob_hashes_len_offset..blob_hashes_start])?;
    if blob_hashes_len == 0 || blob_hashes_len % ABI_WORD != 0 {
        return None;
    }
    let blob_hashes_end = blob_hashes_start.checked_add(blob_hashes_len)?;
    if message.len() < blob_hashes_end {
        return None;
    }
    Some((
        SyscoinEdgeDaRef {
            edge_chain_id,
            edge_batch_number,
            edge_da_commitment,
            blob_version_hashes: &message[blob_hashes_start..blob_hashes_end],
        },
        blob_hashes_end,
    ))
}

fn is_compact_edge_da_commit_tx(
    tx_to: Option<Address>,
    tx_input: &[u8],
    commit_tx_target: Address,
) -> bool {
    tx_to == Some(commit_tx_target)
        && tx_input.len() >= 4
        && (tx_input[..4] == IExecutor::commitBatchesSharedBridgeCall::SELECTOR
            || tx_input[..4] == IMultisigCommitter::commitBatchesMultisigCall::SELECTOR)
}

fn u256_word_to_u64(word: &[u8]) -> Option<u64> {
    if word.len() != ABI_WORD || word[..24] != [0u8; 24] {
        return None;
    }
    Some(u64::from_be_bytes(word[24..].try_into().ok()?))
}

fn u256_word_to_usize(word: &[u8]) -> Option<usize> {
    usize::try_from(u256_word_to_u64(word)?).ok()
}

/// Returns the canonical upgrade tx hash to use for a specific batch number.
///
/// `upgrade_batch_number == 0` means an upgrade tx hash is present in contract storage and will
/// be consumed by the next committed batch.
pub fn expected_upgrade_tx_hash_for_batch(
    batch_number: u64,
    last_committed_batch: u64,
    upgrade_batch_number: u64,
    upgrade_tx_hash: Option<B256>,
) -> Option<B256> {
    let upgrade_tx_hash = upgrade_tx_hash?;
    if upgrade_batch_number == 0 {
        return (batch_number == last_committed_batch + 1).then_some(upgrade_tx_hash);
    }
    (batch_number == upgrade_batch_number).then_some(upgrade_tx_hash)
}

// SYSCOIN: Settlement-layer upgrade metadata may provide the canonical hash expected for the
// upgrade batch. Keep the batch commitment tied to the actually executed upgrade transaction.
fn checked_upgrade_tx_hash(
    expected_upgrade_tx_hash: Option<B256>,
    actual_upgrade_tx_hash: B256,
) -> anyhow::Result<B256> {
    if let Some(expected_upgrade_tx_hash) = expected_upgrade_tx_hash {
        ensure!(
            expected_upgrade_tx_hash == actual_upgrade_tx_hash,
            "canonical upgrade tx hash mismatch: expected {expected_upgrade_tx_hash}, actual {actual_upgrade_tx_hash}"
        );
    }
    Ok(actual_upgrade_tx_hash)
}

fn blob_data_id(data: &[u8]) -> [u8; 32] {
    Blake2s256::digest(data).into()
}

fn encoded_blob_chunks_from_pubdata(pubdata: &[u8]) -> anyhow::Result<Vec<Vec<u8>>> {
    ensure!(
        pubdata.len() <= SYSCOIN_DA_MAX_BLOB_PUBDATA_BYTES,
        "Syscoin DA blob pubdata exceeds 32-blob capacity: {} > {}",
        pubdata.len(),
        SYSCOIN_DA_MAX_BLOB_PUBDATA_BYTES
    );

    // Match the proving side blob commitment generator: prepend the 31-byte
    // length prefix and hash each encoded blob chunk with Blake2s.
    let mut encoded = vec![0u8; BLOB_CHUNK_SIZE];
    encoded[0..8].copy_from_slice(&(pubdata.len() as u64).to_be_bytes());
    encoded.extend_from_slice(pubdata);
    Ok(encoded
        .chunks(SYSCOIN_DA_BYTES_PER_BLOB)
        .map(|chunk| chunk.to_vec())
        .collect())
}

pub fn syscoin_blob_ids_and_chunks_from_pubdata(
    pubdata: &[u8],
) -> anyhow::Result<(Vec<u8>, Vec<Vec<u8>>)> {
    let blob_chunks = encoded_blob_chunks_from_pubdata(pubdata)?;
    let blob_ids = blob_chunks
        .iter()
        .flat_map(|chunk| blob_data_id(chunk))
        .collect();
    Ok((blob_ids, blob_chunks))
}

/// Information about a batch produced by the batcher and driven through the pipeline before it is
/// committed on-chain.
/// Contains enough data to restore `StoredBatchInfo` that got applied on-chain.
/// Contains enough data to construct public input hash (the batch commitment).
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct PendingBatchInfo {
    #[serde(flatten)]
    pub commit_info: CommitBatchInfo,
    /// L1 protocol upgrade transaction that was finalized in this batch. Missing for the vast
    /// majority of batches.
    pub upgrade_tx_hash: Option<B256>,
    pub protocol_version: ProtocolSemanticVersion,
}

impl PendingBatchInfo {
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        blocks: Vec<(&BlockOutput, &[ZkTransaction], &TreeBatchOutput)>,
        chain_id: u64,
        batch_number: u64,
        pubdata_mode: PubdataMode,
        sl_chain_id: u64,
        multichain_root: B256,
        protocol_version: &ProtocolSemanticVersion,
        expected_upgrade_tx_hash: Option<B256>,
        compact_edge_da_commit_target: Option<Address>,
        last_256_block_hashes: &[U256; 256],
    ) -> anyhow::Result<(Self, Option<BlobTransactionSidecar>)> {
        let mut priority_operations_hash = keccak256([]);
        let mut number_of_layer1_txs = 0;
        let mut number_of_layer2_txs = 0;
        let mut total_pubdata = vec![];
        let mut encoded_l2_l1_logs = vec![];

        let (first_block_output, _, _) = *blocks.first().unwrap();
        let (last_block_output, _, last_block_tree) = *blocks.last().unwrap();

        let mut upgrade_tx_hash = None;

        let mut dependency_roots_rolling_hash = B256::ZERO;
        // SYSCOIN: accumulated compact edge DA refs emitted by chains settling to Gateway.
        let mut edge_da_refs_root = B256::ZERO;
        // SYSCOIN: compact edge DA ref messages used as final-L1 root openings.
        let mut edge_da_refs_input = Vec::new();

        for (block_output, transactions, _) in blocks {
            total_pubdata.extend(block_output.pubdata.clone());

            for tx in transactions {
                match tx.envelope() {
                    ZkEnvelope::System(envelope) => {
                        number_of_layer2_txs += 1;

                        if let Some(roots) = envelope.interop_roots() {
                            for root in roots {
                                dependency_roots_rolling_hash = keccak256(
                                    (
                                        dependency_roots_rolling_hash,
                                        root.chainId,
                                        root.blockOrBatchNumber,
                                        root.sides,
                                    )
                                        .abi_encode_packed(),
                                );
                            }
                        }
                    }
                    ZkEnvelope::L2(_) => {
                        number_of_layer2_txs += 1;
                    }
                    ZkEnvelope::L1(l1_tx) => {
                        let onchain_data_hash = l1_tx.hash();
                        priority_operations_hash =
                            keccak256([priority_operations_hash.0, onchain_data_hash.0].concat());
                        number_of_layer1_txs += 1;
                    }
                    ZkEnvelope::Upgrade(_) => {
                        ensure!(
                            upgrade_tx_hash.is_none(),
                            "more than one upgrade tx in a batch: first {upgrade_tx_hash:?}, second {}",
                            tx.hash()
                        );
                        upgrade_tx_hash = Some(checked_upgrade_tx_hash(
                            expected_upgrade_tx_hash,
                            *tx.hash(),
                        )?);
                    }
                }
            }

            for (tx, tx_output) in transactions.iter().zip(&block_output.tx_results) {
                let Ok(tx_output) = tx_output else {
                    continue;
                };
                let collect_edge_da_refs = compact_edge_da_commit_target.is_some_and(|target| {
                    is_compact_edge_da_commit_tx(tx.to(), tx.input().as_ref(), target)
                });
                encoded_l2_l1_logs.extend(tx_output.l2_to_l1_logs.iter().map(
                    |log_with_preimage| {
                        if let Some(preimage) = log_with_preimage.preimage.as_deref()
                            && collect_edge_da_refs
                            && let Some(edge_ref) = parse_syscoin_edge_da_ref_message(preimage)
                        {
                            edge_da_refs_root = keccak256(
                                [edge_da_refs_root.0, syscoin_edge_da_ref_hash(edge_ref).0]
                                    .concat(),
                            );
                            edge_da_refs_input.extend_from_slice(preimage);
                        }
                        let log = L2ToL1Log {
                            l2_shard_id: log_with_preimage.log.l2_shard_id,
                            is_service: log_with_preimage.log.is_service,
                            tx_number_in_block: log_with_preimage.log.tx_number_in_block,
                            sender: log_with_preimage.log.sender,
                            key: log_with_preimage.log.key,
                            value: log_with_preimage.log.value,
                        };
                        log.encode()
                    },
                ));
            }
        }

        let last_256_block_hashes_blake = {
            let mut blocks_hasher = Blake2s256::new();
            for block_hash in &last_256_block_hashes[1..] {
                blocks_hasher.update(block_hash.to_be_bytes::<32>());
            }
            blocks_hasher.update(last_block_output.header.hash());

            blocks_hasher.finalize()
        };

        /* ---------- operator DA input ---------- */
        let da_fields = calculate_da_fields(&total_pubdata, pubdata_mode)?;

        /* ---------- new state commitment ---------- */
        // FIXME: extract to a type common batch types?
        let mut hasher = Blake2s256::new();
        hasher.update(last_block_tree.root_hash.as_slice());
        hasher.update(last_block_tree.leaf_count.to_be_bytes());
        hasher.update(last_block_output.header.number.to_be_bytes());
        hasher.update(last_256_block_hashes_blake);
        hasher.update(last_block_output.header.timestamp.to_be_bytes());
        let new_state_commitment = B256::from_slice(&hasher.finalize());

        /* ---------- root hash of l2->l1 logs ---------- */
        let l2_l1_local_root = MiniMerkleTree::new(
            encoded_l2_l1_logs.clone().into_iter(),
            Some(L2_TO_L1_TREE_SIZE),
        )
        .merkle_root();

        let l2_to_l1_logs_root_hash = if protocol_version.is_post_v31() {
            // The result should be Keccak(l2_l1_local_root, multichain_root).
            keccak256([l2_l1_local_root.0, multichain_root.0].concat())
        } else {
            // For older protocol versions, multichain root should be set to zero.
            keccak256([l2_l1_local_root.0, [0u8; 32]].concat())
        };

        let commit_info = CommitBatchInfo {
            batch_number,
            new_state_commitment,
            number_of_layer1_txs,
            number_of_layer2_txs,
            priority_operations_hash,
            dependency_roots_rolling_hash,
            l2_to_l1_logs_root_hash,
            l2_da_commitment_scheme: pubdata_mode.da_commitment_scheme(),
            da_commitment: da_fields.da_commitment,
            first_block_timestamp: first_block_output.header.timestamp,
            first_block_number: Some(first_block_output.header.number),
            last_block_timestamp: last_block_output.header.timestamp,
            last_block_number: Some(last_block_output.header.number),
            chain_id,
            operator_da_input: da_fields.operator_da_input,
            edge_da_refs_input,
            edge_da_refs_root,
            sl_chain_id,
        };
        Ok((
            Self {
                commit_info,
                protocol_version: protocol_version.clone(),
                upgrade_tx_hash,
            },
            da_fields.blob_sidecar,
        ))
    }

    /// Calculate keccak256 hash of BatchOutput part of public input (the batch commitment).
    fn public_input_hash(&self) -> B256 {
        let commit_info = &self.commit_info;
        let upgrade_tx_hash = self.upgrade_tx_hash.unwrap_or(B256::ZERO);
        match self.protocol_version.minor {
            // v30 and v31 use different packed layouts for batch output hash:
            // v31 inserts number_of_layer2_txs between L1 tx count and priority_operations_hash.
            30 => B256::from(keccak256(
                (
                    U256::from(commit_info.chain_id),
                    commit_info.first_block_timestamp,
                    commit_info.last_block_timestamp,
                    U256::from(commit_info.l2_da_commitment_scheme as u8),
                    commit_info.da_commitment,
                    U256::from(commit_info.number_of_layer1_txs),
                    commit_info.priority_operations_hash,
                    commit_info.l2_to_l1_logs_root_hash,
                    upgrade_tx_hash,
                    commit_info.dependency_roots_rolling_hash,
                )
                    .abi_encode_packed(),
            )),
            31 | 32 => B256::from(keccak256(
                (
                    U256::from(commit_info.chain_id),
                    commit_info.first_block_timestamp,
                    commit_info.last_block_timestamp,
                    U256::from(commit_info.l2_da_commitment_scheme as u8),
                    commit_info.da_commitment,
                    U256::from(commit_info.number_of_layer1_txs),
                    U256::from(commit_info.number_of_layer2_txs),
                    commit_info.priority_operations_hash,
                    commit_info.l2_to_l1_logs_root_hash,
                    upgrade_tx_hash,
                    commit_info.dependency_roots_rolling_hash,
                    U256::from(commit_info.sl_chain_id),
                    // SYSCOIN: bind compact edge DA refs into the proven Gateway batch output.
                    commit_info.edge_da_refs_root,
                )
                    .abi_encode_packed(),
            )),
            _ => panic!("Unsupported protocol version: {}", self.protocol_version),
        }
    }

    /// Computes the batch commitment and turns this into its committed form.
    pub fn into_committed(self) -> CommittedBatchInfo {
        let commitment = self.public_input_hash();
        CommittedBatchInfo {
            commit_info: self.commit_info,
            commitment,
        }
    }

    pub fn into_stored(self) -> StoredBatchInfo {
        self.into_committed().into_stored()
    }
}

impl Deref for PendingBatchInfo {
    type Target = CommitBatchInfo;

    fn deref(&self) -> &Self::Target {
        &self.commit_info
    }
}

impl DerefMut for PendingBatchInfo {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.commit_info
    }
}

/// Information about a batch that has already been committed on-chain, as discovered from L1.
/// Carries the batch `commitment` directly (e.g. read from the `BlockCommit` event) instead of
/// the data required to recompute it.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct CommittedBatchInfo {
    #[serde(flatten)]
    pub commit_info: CommitBatchInfo,
    pub commitment: B256,
}

impl CommittedBatchInfo {
    pub fn into_stored(self) -> StoredBatchInfo {
        let commit_info = self.commit_info;
        StoredBatchInfo {
            batch_number: commit_info.batch_number,
            state_commitment: commit_info.new_state_commitment,
            number_of_layer1_txs: commit_info.number_of_layer1_txs,
            priority_operations_hash: commit_info.priority_operations_hash,
            dependency_roots_rolling_hash: commit_info.dependency_roots_rolling_hash,
            l2_to_l1_logs_root_hash: commit_info.l2_to_l1_logs_root_hash,
            commitment: self.commitment,
            // unused
            last_block_timestamp: Some(0),
        }
    }
}

struct DAFields {
    pub da_commitment: B256,
    pub operator_da_input: Vec<u8>,
    pub blob_sidecar: Option<BlobTransactionSidecar>,
}

fn calculate_da_fields(pubdata: &[u8], pubdata_mode: PubdataMode) -> anyhow::Result<DAFields> {
    let (da_commitment, operator_da_input, blob_sidecar) = match pubdata_mode {
        PubdataMode::Calldata => {
            let mut operator_da_input = Vec::with_capacity(32 * 3 + 1 + pubdata.len() + 1 + 32);

            // reference for this header is taken from zk_ee: https://github.com/matter-labs/zk_ee/blob/ad-aggregation-program/aggregator/src/aggregation/da_commitment.rs#L27
            // consider reusing that code instead:
            //
            // hasher.update([0u8; 32]); // we don't have to validate state diffs hash
            // hasher.update(Keccak256::digest(&pubdata)); // full pubdata keccak
            // hasher.update([1u8]); // with calldata we should provide 1 blob
            // hasher.update([0u8; 32]); // its hash will be ignored on the settlement layer
            // Ok(hasher.finalize().into())

            operator_da_input.extend(B256::ZERO.as_slice());
            operator_da_input.extend(keccak256(pubdata));
            operator_da_input.push(1);
            operator_da_input.extend(B256::ZERO.as_slice());

            //     bytes32 daCommitment; - we compute hash of the first part of the operator_da_input (see above)
            let da_commitment = keccak256(&operator_da_input);

            operator_da_input.extend([PUBDATA_SOURCE_CALLDATA]);
            operator_da_input.extend(pubdata);
            // blob_commitment should be set to zero in ZK OS
            operator_da_input.extend(B256::ZERO.as_slice());

            (da_commitment, operator_da_input, None)
        }
        PubdataMode::Validium => (B256::ZERO, vec![0u8; 32], None),
        PubdataMode::Blobs | PubdataMode::RelayedL2Calldata => {
            // SYSCOIN: edge chains that settle to Gateway publish pubdata directly to Bitcoin
            // DA and commit only the compact ordered blob hash array to Gateway.
            let (blob_ids_from_pubdata, _blob_chunks_from_pubdata) =
                syscoin_blob_ids_and_chunks_from_pubdata(pubdata)?;
            let blob_ids = blob_ids_from_pubdata;
            let da_commitment = keccak256(&blob_ids);
            let operator_da_input = blob_ids;
            (da_commitment, operator_da_input, None)
        }
    };
    Ok(DAFields {
        da_commitment,
        operator_da_input,
        blob_sidecar,
    })
}

#[cfg(test)]
mod tests {
    use super::calculate_da_fields;
    use super::{
        SYSCOIN_DA_BYTES_PER_BLOB, SYSCOIN_DA_MAX_BLOB_PUBDATA_BYTES, SyscoinEdgeDaRef,
        blob_data_id, checked_upgrade_tx_hash, is_compact_edge_da_commit_tx,
        syscoin_edge_da_ref_hash, syscoin_edge_da_refs_from_input,
    };
    use alloy::primitives::{B256, U256, address, keccak256};
    use alloy::sol_types::SolCall;
    use zksync_os_contract_interface::IExecutor;
    use zksync_os_types::PubdataMode;

    fn expected_blob_ids(pubdata: &[u8]) -> Vec<u8> {
        let mut encoded = vec![0u8; 31];
        encoded[0..8].copy_from_slice(&(pubdata.len() as u64).to_be_bytes());
        encoded.extend_from_slice(pubdata);
        encoded
            .chunks(SYSCOIN_DA_BYTES_PER_BLOB)
            .flat_map(blob_data_id)
            .collect()
    }

    fn compact_edge_da_ref_message(
        edge_chain_id: u64,
        edge_batch_number: u64,
        edge_da_commitment: B256,
        blob_hashes: &[u8],
    ) -> Vec<u8> {
        let mut message = Vec::new();
        message.extend([0u8; 31]);
        message.push(1);
        message.extend(U256::from(edge_chain_id).to_be_bytes::<32>());
        message.extend(U256::from(edge_batch_number).to_be_bytes::<32>());
        message.extend(edge_da_commitment.as_slice());
        message.extend(U256::from(32 * 5).to_be_bytes::<32>());
        message.extend(U256::from(blob_hashes.len()).to_be_bytes::<32>());
        message.extend(blob_hashes);
        message
    }

    #[test]
    fn blob_da_fields_match_os_chunk_ids_for_single_blob() {
        let pubdata = b"hello-syscoin-da";

        let fields = calculate_da_fields(pubdata, PubdataMode::Blobs).unwrap();
        let expected_blob_ids = expected_blob_ids(pubdata);

        assert_eq!(fields.operator_da_input, expected_blob_ids);
        assert_eq!(fields.da_commitment, keccak256(&fields.operator_da_input));
        assert!(fields.blob_sidecar.is_none());
    }

    #[test]
    fn blob_da_fields_match_os_chunk_ids_for_multiple_blobs() {
        let pubdata = vec![0x42; SYSCOIN_DA_BYTES_PER_BLOB + 17];

        let fields = calculate_da_fields(&pubdata, PubdataMode::Blobs).unwrap();
        let expected_blob_ids = expected_blob_ids(&pubdata);

        assert_eq!(fields.operator_da_input, expected_blob_ids);
        assert_eq!(fields.da_commitment, keccak256(&fields.operator_da_input));
        assert!(fields.blob_sidecar.is_none());
    }

    #[test]
    fn relayed_l2_calldata_uses_compact_syscoin_da_refs() {
        let pubdata = b"edge-chain-pubdata";

        let fields = calculate_da_fields(pubdata, PubdataMode::RelayedL2Calldata).unwrap();
        let expected_blob_ids = expected_blob_ids(pubdata);

        assert_eq!(fields.operator_da_input, expected_blob_ids);
        assert_eq!(fields.da_commitment, keccak256(&fields.operator_da_input));
        assert!(fields.blob_sidecar.is_none());
    }

    #[test]
    fn blob_da_fields_reject_over_capacity_without_panicking() {
        let pubdata = vec![0u8; SYSCOIN_DA_MAX_BLOB_PUBDATA_BYTES + 1];
        let err = match calculate_da_fields(&pubdata, PubdataMode::Blobs) {
            Ok(_) => panic!("over-capacity Syscoin blob DA pubdata must be rejected"),
            Err(err) => err,
        };

        assert!(
            err.to_string()
                .contains("Syscoin DA blob pubdata exceeds 32-blob capacity"),
            "{err}"
        );
    }

    #[test]
    fn upgrade_tx_hash_uses_actual_hash_when_expected_missing() {
        let actual = B256::from([1; 32]);

        assert_eq!(checked_upgrade_tx_hash(None, actual).unwrap(), actual);
    }

    #[test]
    fn upgrade_tx_hash_accepts_matching_expected_hash() {
        let actual = B256::from([2; 32]);

        assert_eq!(
            checked_upgrade_tx_hash(Some(actual), actual).unwrap(),
            actual
        );
    }

    #[test]
    fn upgrade_tx_hash_rejects_mismatched_expected_hash_without_panicking() {
        let err = checked_upgrade_tx_hash(Some(B256::from([3; 32])), B256::from([4; 32]))
            .expect_err("mismatched upgrade tx hashes must be rejected");

        assert!(
            err.to_string()
                .contains("canonical upgrade tx hash mismatch"),
            "{err}"
        );
    }

    #[test]
    fn edge_da_refs_root_is_ordered_and_context_bound() {
        let blob_hashes = expected_blob_ids(b"edge-chain-pubdata");
        let da_commitment = keccak256(&blob_hashes);

        let first = SyscoinEdgeDaRef {
            edge_chain_id: 10,
            edge_batch_number: 1,
            edge_da_commitment: da_commitment,
            blob_version_hashes: &blob_hashes,
        };
        let second = SyscoinEdgeDaRef {
            edge_chain_id: 10,
            edge_batch_number: 2,
            edge_da_commitment: da_commitment,
            blob_version_hashes: &blob_hashes,
        };

        let first_hash = syscoin_edge_da_ref_hash(first);
        let second_hash = syscoin_edge_da_ref_hash(second);
        assert_ne!(first_hash, second_hash);

        let root = [first_hash, second_hash]
            .into_iter()
            .fold(B256::ZERO, |root, edge_ref_hash| {
                keccak256([root.0, edge_ref_hash.0].concat())
            });
        assert_ne!(root, B256::ZERO);
    }

    #[test]
    fn edge_da_refs_input_parses_concatenated_messages() {
        let first_blob_hashes = expected_blob_ids(b"first-edge-chain-pubdata");
        let second_blob_hashes = expected_blob_ids(b"second-edge-chain-pubdata");
        let first_commitment = keccak256(&first_blob_hashes);
        let second_commitment = keccak256(&second_blob_hashes);
        let first_message =
            compact_edge_da_ref_message(10, 1, first_commitment, &first_blob_hashes);
        let second_message =
            compact_edge_da_ref_message(10, 2, second_commitment, &second_blob_hashes);
        let mut input = first_message;
        input.extend(second_message);

        let refs = syscoin_edge_da_refs_from_input(&input).unwrap();

        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].edge_batch_number, 1);
        assert_eq!(refs[0].blob_version_hashes, first_blob_hashes);
        assert_eq!(refs[1].edge_batch_number, 2);
        assert_eq!(refs[1].blob_version_hashes, second_blob_hashes);
    }

    #[test]
    fn compact_edge_da_refs_are_collected_only_from_known_commit_target() {
        let commit_target = address!("0000000000000000000000000000000000001234");
        let other_target = address!("0000000000000000000000000000000000005678");
        let mut commit_input = IExecutor::commitBatchesSharedBridgeCall::SELECTOR.to_vec();
        commit_input.extend_from_slice(b"truncated calldata is enough for selector filtering");

        assert!(is_compact_edge_da_commit_tx(
            Some(commit_target),
            &commit_input,
            commit_target
        ));
        assert!(!is_compact_edge_da_commit_tx(
            Some(other_target),
            &commit_input,
            commit_target
        ));
        assert!(!is_compact_edge_da_commit_tx(
            Some(commit_target),
            b"abcd",
            commit_target
        ));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiscoveredCommittedBatch {
    /// Information about committed batch as was discovered on-chain.
    pub batch_info: StoredBatchInfo,
    /// Range of L2 blocks that belong to this batch.
    pub block_range: ops::RangeInclusive<BlockNumber>,
}

impl DiscoveredCommittedBatch {
    pub fn number(&self) -> u64 {
        self.batch_info.batch_number
    }

    pub fn hash(&self) -> B256 {
        self.batch_info.hash()
    }

    pub fn first_block_number(&self) -> BlockNumber {
        *self.block_range.start()
    }

    pub fn last_block_number(&self) -> BlockNumber {
        *self.block_range.end()
    }

    pub fn block_count(&self) -> u64 {
        self.block_range.end() - self.block_range.start() + 1
    }
}
