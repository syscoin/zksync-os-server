use alloy::primitives::{Address, B256, BlockNumber, Bytes, U256, keccak256};
use alloy::sol_types::{SolCall, SolValue};
use anyhow::ensure;
use blake2::{Blake2s256, Digest};
use serde::{Deserialize, Serialize};
use std::ops;
use std::ops::{Deref, DerefMut};
use zksync_os_contract_interface::calldata::CommitCalldata;
use zksync_os_contract_interface::models::{CommitBatchInfo, DACommitmentScheme, StoredBatchInfo};
use zksync_os_contract_interface::{IExecutor, IMultisigCommitter};
use zksync_os_interface::types::TxOutput;
use zksync_os_types::{BlockOutput, ProtocolSemanticVersion, PubdataMode, ZkTransaction};

const BLOB_CHUNK_SIZE: usize = 31;
// SYSCOIN: Syscoin Bitcoin DA accepts up to 2 MiB per blob and up to 32 blobs per block.
pub const SYSCOIN_DA_BYTES_PER_BLOB: usize = 2 * 1024 * 1024;
pub const SYSCOIN_DA_MAX_BLOBS_PER_BATCH: usize = 32;
// SYSCOIN: Final-L1 settlement performs one Bitcoin-DA opening per forwarded hash, so a
// Gateway batch uses the same 32-opening ceiling for each message and for their aggregate.
pub const SYSCOIN_DA_MAX_REFS_PER_BATCH: usize = SYSCOIN_DA_MAX_BLOBS_PER_BATCH;
pub const SYSCOIN_DA_MAX_ENCODED_BYTES_PER_BATCH: usize =
    SYSCOIN_DA_BYTES_PER_BLOB * SYSCOIN_DA_MAX_BLOBS_PER_BATCH;
pub const SYSCOIN_DA_MAX_BLOB_PUBDATA_BYTES: usize =
    SYSCOIN_DA_MAX_ENCODED_BYTES_PER_BATCH - BLOB_CHUNK_SIZE;
// SYSCOIN: domain separator for compact edge DA references committed by Gateway.
const SYSCOIN_EDGE_DA_REFS_DOMAIN: &[u8] = b"SYSCOIN_EDGE_DA_REFS_V1";
// SYSCOIN: RelayedSLDAValidator compact-ref message version.
const SYSCOIN_RELAYED_EDGE_DA_VALIDATOR_VERSION: u8 = 1;
// SYSCOIN: V32 commit encoding decoded by the compact-ref replay fallback.
const SYSCOIN_COMPACT_EDGE_COMMIT_ENCODING_VERSION: u8 = 4;
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
    assert!(
        edge_ref.blob_version_hashes.len() / ABI_WORD <= SYSCOIN_DA_MAX_REFS_PER_BATCH,
        "Syscoin edge DA ref exceeds the per-message opening limit"
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
    let mut total_refs = 0usize;
    while !remaining.is_empty() {
        let (edge_ref, consumed) = parse_syscoin_edge_da_ref_message_prefix(remaining)?;
        let message_refs = edge_ref.blob_version_hashes.len() / ABI_WORD;
        total_refs = total_refs.checked_add(message_refs)?;
        if total_refs > SYSCOIN_DA_MAX_REFS_PER_BATCH {
            return None;
        }
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
    if blob_hashes_len == 0
        || blob_hashes_len % ABI_WORD != 0
        || blob_hashes_len / ABI_WORD > SYSCOIN_DA_MAX_REFS_PER_BATCH
    {
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

/// SYSCOIN: The zkOS guest collects compact edge-DA references only for successful calls.
/// Native replay must apply the same status gate because reverted outputs may retain diagnostic
/// L2-to-L1 logs in the host execution result.
fn collects_syscoin_edge_da_refs(tx_output: &TxOutput) -> bool {
    tx_output.is_success()
}

// SYSCOIN: Replay execution retains transaction calldata and the emitted message hash, but the
// messenger output does not retain its preimage. Recreate the validator's canonical message from
// the child-chain commit calldata so Gateway batch commitments remain stable across restarts.
fn compact_edge_da_ref_message_from_commit_calldata(
    input: &[u8],
) -> anyhow::Result<Option<Vec<u8>>> {
    let commit_encoding_version = if input
        .starts_with(IExecutor::commitBatchesSharedBridgeCall::SELECTOR.as_slice())
    {
        let call = <IExecutor::commitBatchesSharedBridgeCall as SolCall>::abi_decode(input)?;
        call._commitData.first().copied()
    } else if input.starts_with(IMultisigCommitter::commitBatchesMultisigCall::SELECTOR.as_slice())
    {
        let call = <IMultisigCommitter::commitBatchesMultisigCall as SolCall>::abi_decode(input)?;
        call._batchData.first().copied()
    } else {
        None
    };
    if commit_encoding_version != Some(SYSCOIN_COMPACT_EDGE_COMMIT_ENCODING_VERSION) {
        return Ok(None);
    }

    let commit = CommitCalldata::decode(input)?;
    let commit_info = commit.commit_batch_info;
    match commit_info.l2_da_commitment_scheme {
        DACommitmentScheme::BlobsZKsyncOS => {}
        scheme => anyhow::bail!(
            "unsupported compact edge DA commitment scheme: {scheme:?}; expected BlobsZKsyncOS"
        ),
    }

    let blob_hashes = commit_info.operator_da_input;
    ensure!(
        !blob_hashes.is_empty()
            && blob_hashes.len().is_multiple_of(ABI_WORD)
            && blob_hashes.len() / ABI_WORD <= SYSCOIN_DA_MAX_BLOBS_PER_BATCH,
        "compact edge DA operator input must contain 1..={SYSCOIN_DA_MAX_BLOBS_PER_BATCH} blob hashes"
    );
    ensure!(
        keccak256(&blob_hashes) == commit_info.da_commitment,
        "compact edge DA commitment does not match operator input"
    );

    Ok(Some(
        (
            U256::from(SYSCOIN_RELAYED_EDGE_DA_VALIDATOR_VERSION),
            U256::from(commit_info.chain_id),
            U256::from(commit_info.batch_number),
            commit_info.da_commitment,
            Bytes::from(blob_hashes),
        )
            .abi_encode_params(),
    ))
}

/// SYSCOIN: Reconstructs compact edge-DA messages and their ordered, context-bound root from
/// successfully executed calls to the configured Gateway commit target.
///
/// Native batch replay uses this function so restart cannot disagree about which messages are
/// opened to L1.
pub fn syscoin_edge_da_refs_for_blocks<'a>(
    blocks: impl IntoIterator<Item = (&'a BlockOutput, &'a [ZkTransaction])>,
    compact_edge_da_commit_target: Address,
) -> anyhow::Result<(Vec<u8>, B256)> {
    let mut edge_da_refs_input = Vec::new();
    let mut edge_da_refs_root = B256::ZERO;
    let mut total_refs = 0usize;

    for (block_output, transactions) in blocks {
        ensure!(
            transactions.len() == block_output.tx_results.len(),
            "transaction/output count mismatch while reconstructing compact edge-DA refs: {} transactions, {} outputs",
            transactions.len(),
            block_output.tx_results.len(),
        );
        for (tx, tx_output) in transactions.iter().zip(&block_output.tx_results) {
            let Ok(tx_output) = tx_output else {
                continue;
            };
            if !collects_syscoin_edge_da_refs(tx_output) {
                continue;
            }
            if !is_compact_edge_da_commit_tx(
                tx.to(),
                tx.input().as_ref(),
                compact_edge_da_commit_target,
            ) {
                continue;
            }

            let mut collected_from_preimage = false;
            for log in &tx_output.l2_to_l1_logs {
                if let Some(preimage) = log.preimage.as_deref()
                    && let Some(edge_ref) = parse_syscoin_edge_da_ref_message(preimage)
                {
                    let message_refs = edge_ref.blob_version_hashes.len() / ABI_WORD;
                    total_refs = total_refs
                        .checked_add(message_refs)
                        .ok_or_else(|| anyhow::anyhow!("compact edge DA ref count overflow"))?;
                    ensure!(
                        total_refs <= SYSCOIN_DA_MAX_REFS_PER_BATCH,
                        "compact edge DA refs exceed the per-Gateway-batch limit of {SYSCOIN_DA_MAX_REFS_PER_BATCH}"
                    );
                    edge_da_refs_root = keccak256(
                        [edge_da_refs_root.0, syscoin_edge_da_ref_hash(edge_ref).0].concat(),
                    );
                    edge_da_refs_input.extend_from_slice(preimage);
                    collected_from_preimage = true;
                }
            }

            if !collected_from_preimage
                && let Some(message) =
                    compact_edge_da_ref_message_from_commit_calldata(tx.input().as_ref())?
            {
                let message_hash = keccak256(&message);
                ensure!(
                    tx_output
                        .l2_to_l1_logs
                        .iter()
                        .any(|log| log.log.value == message_hash),
                    "compact edge DA commit did not emit its canonical message"
                );
                let edge_ref = parse_syscoin_edge_da_ref_message(&message)
                    .expect("locally encoded compact edge DA ref must parse");
                let message_refs = edge_ref.blob_version_hashes.len() / ABI_WORD;
                total_refs = total_refs
                    .checked_add(message_refs)
                    .ok_or_else(|| anyhow::anyhow!("compact edge DA ref count overflow"))?;
                ensure!(
                    total_refs <= SYSCOIN_DA_MAX_REFS_PER_BATCH,
                    "compact edge DA refs exceed the per-Gateway-batch limit of {SYSCOIN_DA_MAX_REFS_PER_BATCH}"
                );
                edge_da_refs_root =
                    keccak256([edge_da_refs_root.0, syscoin_edge_da_ref_hash(edge_ref).0].concat());
                edge_da_refs_input.extend_from_slice(&message);
            }
        }
    }

    Ok((edge_da_refs_input, edge_da_refs_root))
}

// SYSCOIN: Batcher peeking must use the exact same successful-target-call semantics as native
// replay, while avoiding an oversized-first-block exception for settlement-critical openings.
pub fn syscoin_edge_da_ref_count_for_blocks<'a>(
    blocks: impl IntoIterator<Item = (&'a BlockOutput, &'a [ZkTransaction])>,
    compact_edge_da_commit_target: Address,
) -> anyhow::Result<usize> {
    let (input, _) = syscoin_edge_da_refs_for_blocks(blocks, compact_edge_da_commit_target)?;
    let refs = syscoin_edge_da_refs_from_input(&input)
        .ok_or_else(|| anyhow::anyhow!("canonical compact edge DA input is malformed"))?;
    Ok(refs
        .iter()
        .map(|edge_ref| edge_ref.blob_version_hashes.len() / ABI_WORD)
        .sum())
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

/// Batch-level commit values produced canonically by the V8 native batch run. The batch program
/// itself computes pubdata, DA/state commitments and L1/L2 tx counters, so
/// [`PendingBatchInfo::build_from_canonical_output`] consumes this instead of the server
/// re-accumulating per-block outputs ([`PendingBatchInfo::build`]).
#[derive(Debug, Clone)]
pub struct CanonicalBatchCommitData {
    pub first_block_number: u64,
    pub last_block_number: u64,
    pub first_block_timestamp: u64,
    pub last_block_timestamp: u64,
    pub new_state_commitment: B256,
    pub da_commitment: B256,
    pub number_of_layer1_txs: u64,
    pub number_of_layer2_txs: u64,
    pub priority_operations_hash: B256,
    pub dependency_roots_rolling_hash: B256,
    pub l2_to_l1_logs_root_hash: B256,
    pub upgrade_tx_hash: Option<B256>,
    pub chain_id: u64,
    pub sl_chain_id: u64,
    pub pubdata: Vec<u8>,
    /// SYSCOIN: Canonical compact edge-DA messages that open [`Self::edge_da_refs_root`].
    pub edge_da_refs_input: Vec<u8>,
    /// SYSCOIN: Ordered, context-bound root emitted by the patched V8 batch program.
    pub edge_da_refs_root: B256,
}

impl PendingBatchInfo {
    pub fn build_from_canonical_output(
        batch_number: u64,
        pubdata_mode: PubdataMode,
        protocol_version: &ProtocolSemanticVersion,
        batch: CanonicalBatchCommitData,
    ) -> anyhow::Result<Self> {
        let da_fields = calculate_da_fields(&batch.pubdata, pubdata_mode)?;
        anyhow::ensure!(
            da_fields.da_commitment == batch.da_commitment,
            "canonical batch DA commitment mismatch: expected {}, got {}",
            batch.da_commitment,
            da_fields.da_commitment,
        );
        let edge_refs = syscoin_edge_da_refs_from_input(&batch.edge_da_refs_input)
            .ok_or_else(|| anyhow::anyhow!("canonical batch edge-DA input is malformed"))?;
        let reconstructed_edge_root = edge_refs.into_iter().fold(B256::ZERO, |root, edge_ref| {
            keccak256([root.0, syscoin_edge_da_ref_hash(edge_ref).0].concat())
        });
        anyhow::ensure!(
            reconstructed_edge_root == batch.edge_da_refs_root,
            "canonical batch edge-DA root mismatch: expected {}, got {}",
            batch.edge_da_refs_root,
            reconstructed_edge_root,
        );

        let commit_info = CommitBatchInfo {
            batch_number,
            new_state_commitment: batch.new_state_commitment,
            number_of_layer1_txs: batch.number_of_layer1_txs,
            number_of_layer2_txs: batch.number_of_layer2_txs,
            priority_operations_hash: batch.priority_operations_hash,
            dependency_roots_rolling_hash: batch.dependency_roots_rolling_hash,
            l2_to_l1_logs_root_hash: batch.l2_to_l1_logs_root_hash,
            l2_da_commitment_scheme: pubdata_mode.da_commitment_scheme(),
            da_commitment: batch.da_commitment,
            first_block_timestamp: batch.first_block_timestamp,
            first_block_number: Some(batch.first_block_number),
            last_block_timestamp: batch.last_block_timestamp,
            last_block_number: Some(batch.last_block_number),
            chain_id: batch.chain_id,
            operator_da_input: da_fields.operator_da_input,
            edge_da_refs_input: batch.edge_da_refs_input,
            edge_da_refs_root: batch.edge_da_refs_root,
            sl_chain_id: batch.sl_chain_id,
        };

        Ok(Self {
            commit_info,
            upgrade_tx_hash: batch.upgrade_tx_hash,
            protocol_version: protocol_version.clone(),
        })
    }

    /// SYSCOIN: Canonical batch-output hash computed by the patched final-v0.4 guest. The chain id is
    /// committed through the outer chain-config hash; Syscoin additionally binds the compact
    /// edge-DA root as the final field.
    pub fn batch_output_hash(&self) -> B256 {
        let commit_info = &self.commit_info;
        let upgrade_tx_hash = self.upgrade_tx_hash.unwrap_or(B256::ZERO);
        B256::from(keccak256(
            (
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
                commit_info.edge_da_refs_root,
            )
                .abi_encode_packed(),
        ))
    }

    /// Computes the batch commitment and turns this into its committed form.
    pub fn into_committed(self) -> CommittedBatchInfo {
        let commitment = self.batch_output_hash();
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
}

fn calculate_da_fields(pubdata: &[u8], pubdata_mode: PubdataMode) -> anyhow::Result<DAFields> {
    let (da_commitment, operator_da_input) = match pubdata_mode {
        PubdataMode::Blobs | PubdataMode::RelayedL2Calldata => {
            // SYSCOIN: edge chains that settle to Gateway publish pubdata directly to Bitcoin
            // DA and commit only the compact ordered blob hash array to Gateway.
            let (blob_ids_from_pubdata, _blob_chunks_from_pubdata) =
                syscoin_blob_ids_and_chunks_from_pubdata(pubdata)?;
            let blob_ids = blob_ids_from_pubdata;
            let da_commitment = keccak256(&blob_ids);
            let operator_da_input = blob_ids;
            (da_commitment, operator_da_input)
        }
    };
    Ok(DAFields {
        da_commitment,
        operator_da_input,
    })
}

#[cfg(test)]
mod canonical_output_tests {
    use super::calculate_da_fields;
    use super::{
        SYSCOIN_DA_BYTES_PER_BLOB, SYSCOIN_DA_MAX_BLOB_PUBDATA_BYTES,
        SYSCOIN_DA_MAX_REFS_PER_BATCH, SyscoinEdgeDaRef, blob_data_id,
        collects_syscoin_edge_da_refs, compact_edge_da_ref_message_from_commit_calldata,
        is_compact_edge_da_commit_tx, syscoin_edge_da_ref_hash, syscoin_edge_da_refs_from_input,
    };
    use alloy::primitives::{Address, B256, Bytes, U256, address, keccak256};
    use alloy::sol_types::SolCall;
    use zksync_os_contract_interface::IExecutor;
    use zksync_os_contract_interface::calldata::encode_commit_batch_data;
    use zksync_os_contract_interface::models::{
        CommitBatchInfo, DACommitmentScheme, StoredBatchInfo,
    };
    use zksync_os_interface::types::{ExecutionOutput, ExecutionResult, TxOutput};
    use zksync_os_types::PubdataMode;

    fn output_with_result(execution_result: ExecutionResult) -> TxOutput {
        TxOutput {
            execution_result,
            gas_used: 0,
            gas_refunded: 0,
            computational_native_used: 0,
            native_used: 0,
            pubdata_used: 0,
            contract_address: None,
            logs: Vec::new(),
            l2_to_l1_logs: Vec::new(),
            storage_writes: Vec::new(),
        }
    }

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

    fn compact_edge_commit_call_data(
        chain_id: u64,
        batch_number: u64,
        scheme: DACommitmentScheme,
        blob_hashes: Vec<u8>,
        protocol_version_minor: u64,
    ) -> Vec<u8> {
        let previous = StoredBatchInfo {
            batch_number: batch_number - 1,
            state_commitment: B256::ZERO,
            number_of_layer1_txs: 0,
            priority_operations_hash: B256::ZERO,
            dependency_roots_rolling_hash: B256::ZERO,
            l2_to_l1_logs_root_hash: B256::ZERO,
            commitment: B256::ZERO,
            last_block_timestamp: Some(0),
        };
        let commit = CommitBatchInfo {
            batch_number,
            new_state_commitment: B256::ZERO,
            number_of_layer1_txs: 0,
            number_of_layer2_txs: 1,
            priority_operations_hash: B256::ZERO,
            dependency_roots_rolling_hash: B256::ZERO,
            l2_to_l1_logs_root_hash: B256::ZERO,
            l2_da_commitment_scheme: scheme,
            da_commitment: keccak256(&blob_hashes),
            first_block_timestamp: 1,
            first_block_number: Some(1),
            last_block_timestamp: 1,
            last_block_number: Some(1),
            chain_id,
            operator_da_input: blob_hashes,
            edge_da_refs_input: Vec::new(),
            edge_da_refs_root: B256::ZERO,
            sl_chain_id: 57001,
        };
        let commit_data = encode_commit_batch_data(&previous, commit, protocol_version_minor);
        IExecutor::commitBatchesSharedBridgeCall {
            _chainAddress: Address::ZERO,
            _processFrom: U256::from(batch_number),
            _processTo: U256::from(batch_number),
            _commitData: Bytes::from(commit_data),
        }
        .abi_encode()
    }

    #[test]
    fn blob_da_fields_match_os_chunk_ids_for_single_blob() {
        let pubdata = b"hello-syscoin-da";

        let fields = calculate_da_fields(pubdata, PubdataMode::Blobs).unwrap();
        let expected_blob_ids = expected_blob_ids(pubdata);

        assert_eq!(fields.operator_da_input, expected_blob_ids);
        assert_eq!(fields.da_commitment, keccak256(&fields.operator_da_input));
    }

    #[test]
    fn blob_da_fields_match_os_chunk_ids_for_multiple_blobs() {
        let pubdata = vec![0x42; SYSCOIN_DA_BYTES_PER_BLOB + 17];

        let fields = calculate_da_fields(&pubdata, PubdataMode::Blobs).unwrap();
        let expected_blob_ids = expected_blob_ids(&pubdata);

        assert_eq!(fields.operator_da_input, expected_blob_ids);
        assert_eq!(fields.da_commitment, keccak256(&fields.operator_da_input));
    }

    #[test]
    fn relayed_l2_calldata_uses_compact_syscoin_da_refs() {
        let pubdata = b"edge-chain-pubdata";

        let fields = calculate_da_fields(pubdata, PubdataMode::RelayedL2Calldata).unwrap();
        let expected_blob_ids = expected_blob_ids(pubdata);

        assert_eq!(fields.operator_da_input, expected_blob_ids);
        assert_eq!(fields.da_commitment, keccak256(&fields.operator_da_input));
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
    fn edge_da_refs_accept_exact_aggregate_limit_split_across_messages() {
        let first_hashes = vec![0x11; 16 * 32];
        let second_hashes = vec![0x22; 16 * 32];
        let first = compact_edge_da_ref_message(10, 1, keccak256(&first_hashes), &first_hashes);
        let second = compact_edge_da_ref_message(11, 2, keccak256(&second_hashes), &second_hashes);
        let input = [first, second].concat();

        let refs = syscoin_edge_da_refs_from_input(&input).unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(
            refs.iter()
                .map(|edge_ref| edge_ref.blob_version_hashes.len() / 32)
                .sum::<usize>(),
            SYSCOIN_DA_MAX_REFS_PER_BATCH
        );
        let root = refs.into_iter().fold(B256::ZERO, |root, edge_ref| {
            keccak256([root.0, syscoin_edge_da_ref_hash(edge_ref).0].concat())
        });
        assert_ne!(root, B256::ZERO);
    }

    #[test]
    fn edge_da_refs_reject_aggregate_limit_across_messages() {
        let first_hashes = vec![0x11; 16 * 32];
        let second_hashes = vec![0x22; 17 * 32];
        let first = compact_edge_da_ref_message(10, 1, keccak256(&first_hashes), &first_hashes);
        let second = compact_edge_da_ref_message(11, 2, keccak256(&second_hashes), &second_hashes);

        assert!(syscoin_edge_da_refs_from_input(&[first, second].concat()).is_none());
    }

    #[test]
    fn edge_da_refs_reject_single_message_above_limit() {
        let hashes = vec![0x33; (SYSCOIN_DA_MAX_REFS_PER_BATCH + 1) * 32];
        let input = compact_edge_da_ref_message(10, 1, keccak256(&hashes), &hashes);

        assert!(syscoin_edge_da_refs_from_input(&input).is_none());
    }

    #[test]
    fn empty_edge_da_refs_remain_the_canonical_zero_root() {
        let refs = syscoin_edge_da_refs_from_input(&[]).unwrap();
        let root = refs.into_iter().fold(B256::ZERO, |root, edge_ref| {
            keccak256([root.0, syscoin_edge_da_ref_hash(edge_ref).0].concat())
        });

        assert_eq!(root, B256::ZERO);
    }

    #[test]
    fn edge_da_ref_message_is_recreated_from_commit_calldata() {
        let chain_id = 57_057;
        let batch_number = 2_839;
        let mut blob_hashes = vec![0x11; 32];
        blob_hashes.extend([0x22; 32]);
        let input = compact_edge_commit_call_data(
            chain_id,
            batch_number,
            DACommitmentScheme::BlobsZKsyncOS,
            blob_hashes.clone(),
            32,
        );

        let message = compact_edge_da_ref_message_from_commit_calldata(&input)
            .unwrap()
            .unwrap();

        assert_eq!(
            message,
            compact_edge_da_ref_message(
                chain_id,
                batch_number,
                keccak256(&blob_hashes),
                &blob_hashes,
            )
        );
    }

    #[test]
    fn validium_commit_is_rejected_as_compact_edge_ref_message() {
        let input = compact_edge_commit_call_data(
            57_057,
            2_839,
            DACommitmentScheme::EmptyNoDA,
            vec![0; 32],
            32,
        );

        let err = compact_edge_da_ref_message_from_commit_calldata(&input).unwrap_err();

        assert!(
            err.to_string()
                .contains("unsupported compact edge DA commitment scheme")
        );
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

    #[test]
    fn reverted_commit_output_cannot_contribute_compact_edge_da_refs() {
        let successful =
            output_with_result(ExecutionResult::Success(ExecutionOutput::Call(Vec::new())));
        let reverted = output_with_result(ExecutionResult::Revert(Vec::new()));

        assert!(collects_syscoin_edge_da_refs(&successful));
        assert!(!collects_syscoin_edge_da_refs(&reverted));
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

#[cfg(test)]
mod tests {
    use super::{CanonicalBatchCommitData, PendingBatchInfo, calculate_da_fields};
    use alloy::primitives::B256;
    use zksync_os_types::{ProtocolSemanticVersion, PubdataMode};

    fn canonical_batch_data(pubdata_mode: PubdataMode) -> CanonicalBatchCommitData {
        let pubdata = vec![1, 2, 3, 4, 5, 6];
        let da_fields = calculate_da_fields(&pubdata, pubdata_mode).unwrap();
        CanonicalBatchCommitData {
            first_block_number: 11,
            last_block_number: 13,
            first_block_timestamp: 100,
            last_block_timestamp: 120,
            new_state_commitment: B256::repeat_byte(0x11),
            da_commitment: da_fields.da_commitment,
            number_of_layer1_txs: 3,
            number_of_layer2_txs: 8,
            priority_operations_hash: B256::repeat_byte(0x22),
            dependency_roots_rolling_hash: B256::repeat_byte(0x33),
            l2_to_l1_logs_root_hash: B256::repeat_byte(0x44),
            upgrade_tx_hash: Some(B256::repeat_byte(0x55)),
            chain_id: 270,
            sl_chain_id: 123,
            pubdata,
            edge_da_refs_input: Vec::new(),
            edge_da_refs_root: B256::ZERO,
        }
    }

    #[test]
    fn builds_commit_info_from_canonical_batch_output() {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let batch = canonical_batch_data(PubdataMode::Blobs);
        let expected_da_fields = calculate_da_fields(&batch.pubdata, PubdataMode::Blobs).unwrap();

        let batch_info = PendingBatchInfo::build_from_canonical_output(
            42,
            PubdataMode::Blobs,
            &protocol_version,
            batch,
        )
        .unwrap();

        assert_eq!(batch_info.batch_number, 42);
        assert_eq!(batch_info.new_state_commitment, B256::repeat_byte(0x11));
        assert_eq!(batch_info.number_of_layer1_txs, 3);
        assert_eq!(batch_info.number_of_layer2_txs, 8);
        assert_eq!(batch_info.priority_operations_hash, B256::repeat_byte(0x22));
        assert_eq!(
            batch_info.dependency_roots_rolling_hash,
            B256::repeat_byte(0x33)
        );
        assert_eq!(batch_info.l2_to_l1_logs_root_hash, B256::repeat_byte(0x44));
        assert_eq!(batch_info.upgrade_tx_hash, Some(B256::repeat_byte(0x55)));
        assert_eq!(batch_info.first_block_number, Some(11));
        assert_eq!(batch_info.last_block_number, Some(13));
        assert_eq!(batch_info.first_block_timestamp, 100);
        assert_eq!(batch_info.last_block_timestamp, 120);
        assert_eq!(batch_info.chain_id, 270);
        assert_eq!(batch_info.sl_chain_id, 123);
        assert!(batch_info.edge_da_refs_input.is_empty());
        assert_eq!(batch_info.edge_da_refs_root, B256::ZERO);
        assert_eq!(batch_info.da_commitment, expected_da_fields.da_commitment);
        assert_eq!(
            batch_info.operator_da_input,
            expected_da_fields.operator_da_input
        );
    }

    #[test]
    fn detects_canonical_da_commitment_mismatch() {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let mut batch = canonical_batch_data(PubdataMode::Blobs);
        batch.da_commitment = B256::ZERO;

        let err = PendingBatchInfo::build_from_canonical_output(
            42,
            PubdataMode::Blobs,
            &protocol_version,
            batch,
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("canonical batch DA commitment mismatch")
        );
    }

    #[test]
    fn detects_canonical_edge_da_root_mismatch() {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let mut batch = canonical_batch_data(PubdataMode::Blobs);
        batch.edge_da_refs_root = B256::repeat_byte(0x42);

        let err = PendingBatchInfo::build_from_canonical_output(
            42,
            PubdataMode::Blobs,
            &protocol_version,
            batch,
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("canonical batch edge-DA root mismatch")
        );
    }
}
