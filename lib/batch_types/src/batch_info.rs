use alloy::consensus::BlobTransactionSidecar;
use alloy::primitives::{Address, B256, BlockNumber, U256, keccak256};
use alloy::sol_types::SolValue;
use blake2::{Blake2s256, Digest};
use serde::{Deserialize, Serialize};
use std::ops;
use std::ops::{Deref, DerefMut};
use zksync_os_contract_interface::models::{CommitBatchInfo, StoredBatchInfo};
use zksync_os_interface::types::{BlockContext, BlockOutput};
use zksync_os_mini_merkle_tree::MiniMerkleTree;
use zksync_os_types::{
    L2_TO_L1_TREE_SIZE, L2ToL1Log, ProtocolSemanticVersion, PubdataMode, ZkEnvelope, ZkTransaction,
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
pub fn syscoin_edge_da_ref_hash(edge_ref: SyscoinEdgeDaRef<'_>) -> B256 {
    assert!(
        edge_ref.blob_version_hashes.len() % 32 == 0,
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

// SYSCOIN: ordered rolling root over edge DA refs checked by the Gateway node and later bound
// to settlement. Empty means no edge DA references were included.
pub fn syscoin_edge_da_refs_root<'a>(
    edge_refs: impl IntoIterator<Item = SyscoinEdgeDaRef<'a>>,
) -> B256 {
    edge_refs.into_iter().fold(B256::ZERO, |root, edge_ref| {
        keccak256([root.0, syscoin_edge_da_ref_hash(edge_ref).0].concat())
    })
}

// SYSCOIN: derive the ordered edge DA refs root from compact ref messages emitted during
// Gateway execution. Non-Syscoin L2->L1 messages are ignored.
pub fn syscoin_edge_da_refs_root_from_messages<'a>(
    messages: impl IntoIterator<Item = &'a [u8]>,
) -> B256 {
    messages.into_iter().fold(B256::ZERO, |root, message| {
        let Some(edge_ref) = parse_syscoin_edge_da_ref_message(message) else {
            return root;
        };
        keccak256([root.0, syscoin_edge_da_ref_hash(edge_ref).0].concat())
    })
}

// SYSCOIN: parse abi.encode(uint8 version, uint256 chainId, uint256 batchNumber,
// bytes32 daCommitment, bytes blobHashes) emitted by the compact Gateway DA validator.
fn parse_syscoin_edge_da_ref_message(message: &[u8]) -> Option<SyscoinEdgeDaRef<'_>> {
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
    if message.len() != blob_hashes_end {
        return None;
    }
    Some(SyscoinEdgeDaRef {
        edge_chain_id,
        edge_batch_number,
        edge_da_commitment,
        blob_version_hashes: &message[blob_hashes_start..blob_hashes_end],
    })
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

fn blob_data_id(data: &[u8]) -> [u8; 32] {
    keccak256(data).0
}

fn encoded_blob_chunks_from_pubdata(pubdata: &[u8]) -> Vec<Vec<u8>> {
    assert!(
        pubdata.len() <= SYSCOIN_DA_MAX_BLOB_PUBDATA_BYTES,
        "Syscoin DA blob pubdata exceeds 32-blob capacity: {} > {}",
        pubdata.len(),
        SYSCOIN_DA_MAX_BLOB_PUBDATA_BYTES
    );

    // Match the proving side blob commitment generator:
    // prepend 31-byte length field prefix and hash each encoded blob chunk.
    let mut encoded = vec![0u8; BLOB_CHUNK_SIZE];
    encoded[0..8].copy_from_slice(&(pubdata.len() as u64).to_be_bytes());
    encoded.extend_from_slice(pubdata);
    encoded
        .chunks(SYSCOIN_DA_BYTES_PER_BLOB)
        .map(|chunk| chunk.to_vec())
        .collect()
}

fn syscoin_da_blob_count_for_pubdata(pubdata_len: usize) -> usize {
    let blob_count = pubdata_len
        .saturating_add(BLOB_CHUNK_SIZE)
        .div_ceil(SYSCOIN_DA_BYTES_PER_BLOB)
        .max(1);
    assert!(
        blob_count <= SYSCOIN_DA_MAX_BLOBS_PER_BATCH,
        "Syscoin DA pubdata exceeds 32-blob capacity: {} blobs > {}",
        blob_count,
        SYSCOIN_DA_MAX_BLOBS_PER_BATCH
    );
    blob_count
}

pub fn syscoin_blob_ids_and_chunks_from_pubdata(pubdata: &[u8]) -> (Vec<u8>, Vec<Vec<u8>>) {
    let blob_chunks = encoded_blob_chunks_from_pubdata(pubdata);
    let blob_ids = blob_chunks
        .iter()
        .flat_map(|chunk| blob_data_id(chunk))
        .collect();
    (blob_ids, blob_chunks)
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct BatchInfo {
    #[serde(flatten)]
    pub commit_info: CommitBatchInfo,
    /// Chain's diamond proxy address on L1.
    // todo: this should not be a part of this struct as this is static information for the entire chain
    //       but we cannot remove it without breaking backwards compatibility
    pub chain_address: Address,
    /// L1 protocol upgrade transaction that was finalized in this batch. Missing for the vast
    /// majority of batches.
    pub upgrade_tx_hash: Option<B256>,
    /// Blobs sidecar that should be sent with commit operation.
    pub blob_sidecar: Option<BlobTransactionSidecar>,
}

impl BatchInfo {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        blocks: Vec<(
            &BlockOutput,
            &BlockContext,
            &[ZkTransaction],
            &zksync_os_merkle_tree::TreeBatchOutput,
        )>,
        chain_id: u64,
        chain_address: Address,
        batch_number: u64,
        pubdata_mode: PubdataMode,
        sl_chain_id: u64,
        multichain_root: B256,
        protocol_version: &ProtocolSemanticVersion,
        expected_upgrade_tx_hash: Option<B256>,
    ) -> Self {
        let mut priority_operations_hash = keccak256([]);
        let mut number_of_layer1_txs = 0;
        let mut number_of_layer2_txs = 0;
        let mut total_pubdata = vec![];
        let mut encoded_l2_l1_logs = vec![];

        let (first_block_output, _, _, _) = *blocks.first().unwrap();
        let (last_block_output, last_block_context, _, last_block_tree) = *blocks.last().unwrap();

        let mut upgrade_tx_hash = None;

        let mut dependency_roots_rolling_hash = B256::ZERO;
        // SYSCOIN: accumulated compact edge DA refs emitted by chains settling to Gateway.
        let mut edge_da_refs_root = B256::ZERO;
        // SYSCOIN: compact edge DA ref messages used as final-L1 root openings.
        let mut edge_da_refs_input = Vec::new();

        for (block_output, _, transactions, _) in blocks {
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
                        assert!(
                            upgrade_tx_hash.is_none(),
                            "more than one upgrade tx in a batch: first {upgrade_tx_hash:?}, second {}",
                            tx.hash()
                        );
                        upgrade_tx_hash = Some(expected_upgrade_tx_hash.unwrap_or(*tx.hash()));
                    }
                }
            }

            for tx_output in block_output.tx_results.clone().into_iter().flatten() {
                encoded_l2_l1_logs.extend(tx_output.l2_to_l1_logs.into_iter().map(
                    |log_with_preimage| {
                        if let Some(preimage) = log_with_preimage.preimage.as_deref()
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
            for block_hash in &last_block_context.block_hashes.0[1..] {
                blocks_hasher.update(block_hash.to_be_bytes::<32>());
            }
            blocks_hasher.update(last_block_output.header.hash());

            blocks_hasher.finalize()
        };

        /* ---------- operator DA input ---------- */
        let da_fields = calculate_da_fields(
            &total_pubdata,
            pubdata_mode,
            last_block_context.execution_version,
        );

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
        Self {
            commit_info,
            chain_address,
            upgrade_tx_hash,
            blob_sidecar: da_fields.blob_sidecar,
        }
    }

    /// Calculate keccak256 hash of BatchOutput part of public input
    pub fn public_input_hash(&self, protocol_version: &ProtocolSemanticVersion) -> B256 {
        let commit_info = &self.commit_info;
        let upgrade_tx_hash = self.upgrade_tx_hash.unwrap_or(B256::ZERO);
        match protocol_version.minor {
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
            _ => panic!("Unsupported protocol version: {protocol_version}"),
        }
    }

    pub fn into_stored(self, protocol_version: &ProtocolSemanticVersion) -> StoredBatchInfo {
        let commitment = self.public_input_hash(protocol_version);
        let commit_info = self.commit_info;
        StoredBatchInfo {
            batch_number: commit_info.batch_number,
            state_commitment: commit_info.new_state_commitment,
            number_of_layer1_txs: commit_info.number_of_layer1_txs,
            priority_operations_hash: commit_info.priority_operations_hash,
            dependency_roots_rolling_hash: commit_info.dependency_roots_rolling_hash,
            l2_to_l1_logs_root_hash: commit_info.l2_to_l1_logs_root_hash,
            commitment,
            // unused
            last_block_timestamp: Some(0),
        }
    }
}

impl Deref for BatchInfo {
    type Target = CommitBatchInfo;

    fn deref(&self) -> &Self::Target {
        &self.commit_info
    }
}

impl DerefMut for BatchInfo {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.commit_info
    }
}

struct DAFields {
    pub da_commitment: B256,
    pub operator_da_input: Vec<u8>,
    pub blob_sidecar: Option<BlobTransactionSidecar>,
}

fn calculate_da_fields(
    pubdata: &[u8],
    pubdata_mode: PubdataMode,
    batch_execution_version: u32,
) -> DAFields {
    let (da_commitment, operator_da_input, blob_sidecar) =
        match (pubdata_mode, batch_execution_version) {
            (PubdataMode::Calldata, _) | (PubdataMode::Validium, 4) => {
                let blobs_provided = syscoin_da_blob_count_for_pubdata(pubdata.len());
                let mut operator_da_input = Vec::with_capacity(
                    32 * 2 + 1 + 32 * blobs_provided + 1 + pubdata.len() + 32 * blobs_provided,
                );

                // reference for this header is taken from zk_ee: https://github.com/matter-labs/zk_ee/blob/ad-aggregation-program/aggregator/src/aggregation/da_commitment.rs#L27
                // consider reusing that code instead:
                //
                // hasher.update([0u8; 32]); // we don't have to validate state diffs hash
                // hasher.update(Keccak256::digest(&pubdata)); // full pubdata keccak
                // hasher.update([blobs_provided as u8]); // Syscoin DA supports multiple 2 MiB blobs
                // hasher.update([0u8; 32] * blobs_provided); // ignored on the settlement layer
                // Ok(hasher.finalize().into())

                operator_da_input.extend(B256::ZERO.as_slice());
                operator_da_input.extend(keccak256(pubdata));
                operator_da_input.push(
                    blobs_provided
                        .try_into()
                        .expect("Syscoin DA blob count must fit into u8"),
                );
                for _ in 0..blobs_provided {
                    operator_da_input.extend(B256::ZERO.as_slice());
                }

                //     bytes32 daCommitment; - we compute hash of the first part of the operator_da_input (see above)
                let da_commitment = keccak256(&operator_da_input);

                operator_da_input.extend([PUBDATA_SOURCE_CALLDATA]);
                operator_da_input.extend(pubdata);
                // blob_commitment should be set to zero in ZK OS
                for _ in 0..blobs_provided {
                    operator_da_input.extend(B256::ZERO.as_slice());
                }

                if pubdata_mode == PubdataMode::Validium {
                    operator_da_input = U256::ZERO.to_be_bytes_vec();
                }

                (da_commitment, operator_da_input, None)
            }
            (PubdataMode::Validium, _) => (B256::ZERO, vec![0u8; 32], None),
            (PubdataMode::Blobs | PubdataMode::RelayedL2Calldata, _) => {
                // SYSCOIN: edge chains that settle to Gateway publish pubdata directly to Bitcoin
                // DA and commit only the compact ordered blob hash array to Gateway.
                let (blob_ids_from_pubdata, _blob_chunks_from_pubdata) =
                    syscoin_blob_ids_and_chunks_from_pubdata(pubdata);
                let blob_ids = blob_ids_from_pubdata;
                let da_commitment = keccak256(&blob_ids);
                let operator_da_input = blob_ids;
                (da_commitment, operator_da_input, None)
            }
        };
    DAFields {
        da_commitment,
        operator_da_input,
        blob_sidecar,
    }
}

#[cfg(test)]
mod tests {
    use super::calculate_da_fields;
    use super::{
        SYSCOIN_DA_BYTES_PER_BLOB, SyscoinEdgeDaRef, blob_data_id, syscoin_edge_da_ref_hash,
        syscoin_edge_da_refs_root, syscoin_edge_da_refs_root_from_messages,
    };
    use alloy::primitives::{B256, U256, keccak256};
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

        let fields = calculate_da_fields(pubdata, PubdataMode::Blobs, 6);
        let expected_blob_ids = expected_blob_ids(pubdata);

        assert_eq!(fields.operator_da_input, expected_blob_ids);
        assert_eq!(fields.da_commitment, keccak256(&fields.operator_da_input));
        assert!(fields.blob_sidecar.is_none());
    }

    #[test]
    fn blob_da_fields_match_os_chunk_ids_for_multiple_blobs() {
        let pubdata = vec![0x42; SYSCOIN_DA_BYTES_PER_BLOB + 17];

        let fields = calculate_da_fields(&pubdata, PubdataMode::Blobs, 6);
        let expected_blob_ids = expected_blob_ids(&pubdata);

        assert_eq!(fields.operator_da_input, expected_blob_ids);
        assert_eq!(fields.da_commitment, keccak256(&fields.operator_da_input));
        assert!(fields.blob_sidecar.is_none());
    }

    #[test]
    fn relayed_l2_calldata_uses_compact_syscoin_da_refs() {
        let pubdata = b"edge-chain-pubdata";

        let fields = calculate_da_fields(pubdata, PubdataMode::RelayedL2Calldata, 6);
        let expected_blob_ids = expected_blob_ids(pubdata);

        assert_eq!(fields.operator_da_input, expected_blob_ids);
        assert_eq!(fields.da_commitment, keccak256(&fields.operator_da_input));
        assert!(fields.blob_sidecar.is_none());
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

        let root = syscoin_edge_da_refs_root([
            SyscoinEdgeDaRef {
                edge_chain_id: 10,
                edge_batch_number: 1,
                edge_da_commitment: da_commitment,
                blob_version_hashes: &blob_hashes,
            },
            SyscoinEdgeDaRef {
                edge_chain_id: 10,
                edge_batch_number: 2,
                edge_da_commitment: da_commitment,
                blob_version_hashes: &blob_hashes,
            },
        ]);
        assert_ne!(root, B256::ZERO);
    }

    #[test]
    fn edge_da_refs_root_from_messages_matches_canonical_refs() {
        let blob_hashes = expected_blob_ids(b"edge-chain-pubdata");
        let da_commitment = keccak256(&blob_hashes);
        let message = compact_edge_da_ref_message(10, 1, da_commitment, &blob_hashes);

        let root_from_messages = syscoin_edge_da_refs_root_from_messages([
            b"not-a-syscoin-edge-ref".as_slice(),
            message.as_slice(),
        ]);
        let expected_root = syscoin_edge_da_refs_root([SyscoinEdgeDaRef {
            edge_chain_id: 10,
            edge_batch_number: 1,
            edge_da_commitment: da_commitment,
            blob_version_hashes: &blob_hashes,
        }]);

        assert_eq!(root_from_messages, expected_root);
    }

    #[test]
    fn edge_da_refs_root_rejects_messages_with_trailing_bytes() {
        let blob_hashes = expected_blob_ids(b"edge-chain-pubdata");
        let da_commitment = keccak256(&blob_hashes);
        let mut message = compact_edge_da_ref_message(10, 1, da_commitment, &blob_hashes);
        message.extend([0xff; 32]);

        let root_from_messages = syscoin_edge_da_refs_root_from_messages([message.as_slice()]);

        assert_eq!(root_from_messages, B256::ZERO);
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
