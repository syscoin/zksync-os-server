//! Builds the settlement-layer extension of an L2-to-L1 log proof.
//!
//! SYSCOIN: The base proof ends at the source chain's batch root. For a Gateway-settled source
//! batch, the extension continues through Gateway's `L2MessageRoot`. A `MessageRoot` target stops at
//! the exact Gateway execution-block root used by interop; an `L1BatchRoot` target recursively
//! reaches the containing Gateway batch root and authenticates that root against L1. Direct-L1
//! batches remain final at their source batch root and never use this extension.

use alloy::eips::{BlockId, BlockNumberOrTag};
use alloy::primitives::{Address, B256, U256, address, b256, keccak256};
use alloy::providers::{DynProvider, Provider};
use alloy::rpc::types::{Filter, Log};
use alloy::sol_types::SolEvent;
use anyhow::Context;
use futures::TryFutureExt;
use std::collections::HashMap;
use std::future::Future;
use std::ops;
use std::time::Duration;
use zksync_os_contract_interface::IMessageRoot::AppendedChainBatchRoot;
use zksync_os_contract_interface::{Bytes32PushTree, IBridgehub, IMessageRoot};
use zksync_os_storage_api::PersistedBatch;

const LOG_PROOF_SUPPORTED_METADATA_VERSION: u8 = 1;
const L2_MESSAGE_ROOT_ADDRESS: Address = address!("0x0000000000000000000000000000000000010005");
// SYSCOIN: Match the pinned V32 `CHAIN_TREE_EMPTY_ENTRY_HASH`; provider-returned compact trees
// must derive every higher zero subtree from this consensus contract constant.
const CHAIN_TREE_EMPTY_ENTRY_HASH: B256 =
    b256!("0x46700b4d40ac5c35af2c22dda2787a91eb567b06c924a8fb8ae9a05b20c08c21");
// SYSCOIN: Bound the only multi-block provider scan used to reconstruct a Gateway batch. Normal
// V32 batches contain far fewer blocks; this generous ceiling prevents malicious metadata from
// triggering unbounded ancestry walks, log scans, or compact-tree replay.
const MAX_GATEWAY_PROOF_BLOCK_SPAN: u64 = 4_096;
// SYSCOIN: Bound provider-controlled event CPU/secondary allocations after transport decoding.
// A normal V32 settlement range is far below this defensive ceiling.
const MAX_BATCH_ROOT_EVENTS_PER_PROOF: usize = 65_536;
// SYSCOIN: Error text is public RPC output; cap provider-controlled diagnostics independently of
// the accepted event ceiling so a missing target cannot amplify a response.
const MAX_DIAGNOSTIC_BATCH_EVENTS: usize = 32;
// SYSCOIN: One cumulative deadline covers ancestry, logs, historical contract reads, Gateway
// metadata revalidation, and any L1 authentication rather than resetting per sub-call.
pub(crate) const SETTLEMENT_PROOF_TIMEOUT: Duration = Duration::from_secs(300);

// SYSCOIN: Keep proof-only V32 getters local to the RPC hardening boundary. Every call is pinned to
// the same canonical block hash as the corresponding tree/path reconstruction.
alloy::sol! {
    #[sol(rpc)]
    interface IMessageRootProofView {
        function getAggregatedRoot() external view returns (bytes32);
        function historicalRoot(uint256 blockNumber) external view returns (bytes32);
        function chainBatchRoots(uint256 chainId, uint256 batchNumber) external view returns (bytes32);
    }
}

// SYSCOIN: Apply one wall-clock bound to the complete settlement proof, including retries hidden
// inside providers. The duration parameter keeps the boundary directly regression-testable.
pub(crate) async fn with_settlement_proof_deadline<T, F>(
    source: &'static str,
    deadline: Duration,
    future: F,
) -> anyhow::Result<T>
where
    F: Future<Output = anyhow::Result<T>>,
{
    tokio::time::timeout(deadline, future)
        .await
        .with_context(|| format!("{source} settlement proof exceeded {deadline:?}"))?
}

// SYSCOIN: Reject provider identity drift from startup-discovered canonical settlement topology.
fn ensure_chain_identity(source: &'static str, expected: u64, observed: u64) -> anyhow::Result<()> {
    anyhow::ensure!(
        observed == expected,
        "{source} RPC chain ID {observed} does not match startup-discovered chain ID {expected}"
    );
    Ok(())
}

// SYSCOIN: Zero roots are contract sentinels, never valid authenticated proof targets.
fn ensure_nonzero_root(name: &str, root: B256) -> anyhow::Result<()> {
    anyhow::ensure!(root != B256::ZERO, "{name} is zero");
    Ok(())
}

// SYSCOIN: Every proof read is tied to a canonical execution-chain header rather than a mutable
// block number. Parent hashes are retained so multi-block Gateway ranges can prove one ancestry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CanonicalBlockAnchor {
    number: u64,
    hash: B256,
    parent_hash: B256,
}

impl CanonicalBlockAnchor {
    const fn block_id(self) -> BlockId {
        BlockId::hash_canonical(self.hash)
    }
}

// SYSCOIN: An ascending, contiguous ancestry ending at a block fetched canonically by number.
// If that end anchor remains canonical, every parent in this range is the same canonical view.
#[derive(Clone, Debug)]
struct CanonicalBlockRange {
    blocks: Vec<CanonicalBlockAnchor>,
}

impl CanonicalBlockRange {
    fn new(blocks: Vec<CanonicalBlockAnchor>) -> anyhow::Result<Self> {
        anyhow::ensure!(!blocks.is_empty(), "canonical block range is empty");
        for pair in blocks.windows(2) {
            let expected_number = pair[0]
                .number
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("canonical block number overflow"))?;
            anyhow::ensure!(
                pair[1].number == expected_number,
                "canonical block range jumps from {} to {}",
                pair[0].number,
                pair[1].number
            );
            anyhow::ensure!(
                pair[1].parent_hash == pair[0].hash,
                "block #{} parent {} does not match anchored block #{} hash {}",
                pair[1].number,
                pair[1].parent_hash,
                pair[0].number,
                pair[0].hash
            );
        }
        Ok(Self { blocks })
    }

    fn anchor(&self, number: u64) -> anyhow::Result<CanonicalBlockAnchor> {
        let first = self.blocks[0].number;
        let offset = number
            .checked_sub(first)
            .and_then(|offset| usize::try_from(offset).ok())
            .with_context(|| {
                format!("block #{number} is before anchored range beginning at #{first}")
            })?;
        self.blocks.get(offset).copied().with_context(|| {
            format!(
                "block #{number} is outside anchored range #{}..=#{}",
                self.blocks[0].number,
                self.blocks[self.blocks.len() - 1].number
            )
        })
    }

    fn end(&self) -> CanonicalBlockAnchor {
        self.blocks[self.blocks.len() - 1]
    }
}

// SYSCOIN: Resolve a bounded canonical ancestry by anchoring its end by number, then walking parent
// hashes backwards. This avoids mixing independently-numbered block reads across a reorg.
async fn canonical_block_range(
    provider: &DynProvider,
    start: u64,
    end: u64,
    source: &'static str,
) -> anyhow::Result<CanonicalBlockRange> {
    anyhow::ensure!(
        start <= end,
        "{source} canonical range {start}..={end} is inverted"
    );
    let end_block = provider
        .get_block_by_number(BlockNumberOrTag::Number(end))
        .await
        .with_context(|| format!("get {source} block #{end}"))?
        .with_context(|| format!("{source} block #{end} is unavailable"))?;
    anyhow::ensure!(
        end_block.header.inner.number == end,
        "{source} block-number lookup for #{end} returned #{}",
        end_block.header.inner.number
    );
    let mut descending = vec![CanonicalBlockAnchor {
        number: end,
        hash: end_block.header.hash,
        parent_hash: end_block.header.inner.parent_hash,
    }];

    loop {
        let child = *descending
            .last()
            .context("canonical ancestry unexpectedly became empty")?;
        if child.number <= start {
            break;
        }
        let expected_number = child
            .number
            .checked_sub(1)
            .ok_or_else(|| anyhow::anyhow!("{source} ancestry underflow"))?;
        let parent = provider
            .get_block_by_hash(child.parent_hash)
            .await
            .with_context(|| format!("get {source} parent block {:#x}", child.parent_hash))?
            .with_context(|| {
                format!(
                    "{source} parent {} of block #{} is unavailable",
                    child.parent_hash, child.number
                )
            })?;
        anyhow::ensure!(
            parent.header.hash == child.parent_hash,
            "{source} parent lookup returned hash {}; expected {}",
            parent.header.hash,
            child.parent_hash
        );
        anyhow::ensure!(
            parent.header.inner.number == expected_number,
            "{source} parent of block #{} is numbered #{}; expected #{}",
            child.number,
            parent.header.inner.number,
            expected_number
        );
        descending.push(CanonicalBlockAnchor {
            number: expected_number,
            hash: parent.header.hash,
            parent_hash: parent.header.inner.parent_hash,
        });
    }
    descending.reverse();
    CanonicalBlockRange::new(descending)
}

// SYSCOIN: Re-read the numbered tip after every dependent RPC call. A changed hash means a reorg
// crossed the proof build and all hash-pinned data must be discarded.
async fn revalidate_canonical_tip(
    provider: &DynProvider,
    expected: CanonicalBlockAnchor,
    source: &'static str,
) -> anyhow::Result<()> {
    let block = provider
        .get_block_by_number(BlockNumberOrTag::Number(expected.number))
        .await
        .with_context(|| format!("revalidate {source} block #{}", expected.number))?
        .with_context(|| format!("{source} block #{} disappeared", expected.number))?;
    anyhow::ensure!(
        block.header.inner.number == expected.number && block.header.hash == expected.hash,
        "{source} canonical block #{} changed from {} to {}",
        expected.number,
        expected.hash,
        block.header.hash
    );
    Ok(())
}

// SYSCOIN: The pinned Era proof metadata stores each sibling-path length in one byte. Reject
// provider-supplied paths that cannot be represented instead of truncating them into calldata.
fn checked_proof_path_len(name: &str, len: usize) -> anyhow::Result<u8> {
    u8::try_from(len).with_context(|| format!("{name} length {len} exceeds the 255-word ABI limit"))
}

// SYSCOIN: A malicious provider cannot choose an alternate compact-tree zero basis or recurrence
// while preserving only the vector shape. This mirrors `DynamicIncrementalMerkle` exactly.
fn validate_canonical_zero_hashes(zeros: &[B256]) -> anyhow::Result<()> {
    let first = zeros
        .first()
        .copied()
        .context("MessageRoot compact tree has no zero hashes")?;
    anyhow::ensure!(
        first == CHAIN_TREE_EMPTY_ENTRY_HASH,
        "MessageRoot compact tree zero base {first} does not match pinned V32 constant {CHAIN_TREE_EMPTY_ENTRY_HASH}"
    );
    for (level, pair) in zeros.windows(2).enumerate() {
        let expected = keccak256([pair[0].0, pair[0].0].concat());
        anyhow::ensure!(
            pair[1] == expected,
            "MessageRoot compact tree zero at level {} is {}; expected {}",
            level + 1,
            pair[1],
            expected
        );
    }
    Ok(())
}

// SYSCOIN: Validate the untrusted compact-tree shape and index before proof reconstruction indexes
// vectors or performs native-width conversions.
fn checked_tree_leaf_index(tree: &Bytes32PushTree, appended_leaves: usize) -> anyhow::Result<u64> {
    anyhow::ensure!(
        !tree._zeros.is_empty(),
        "MessageRoot compact tree has no zero hashes"
    );
    // SYSCOIN: Shape checks alone do not authenticate the Merkle domain; pin all zero subtrees.
    validate_canonical_zero_hashes(&tree._zeros)?;
    anyhow::ensure!(
        tree._sides.len() == tree._zeros.len(),
        "MessageRoot compact tree has {} sides but {} zero hashes",
        tree._sides.len(),
        tree._zeros.len()
    );
    let levels = tree._zeros.len() - 1;
    checked_proof_path_len("MessageRoot batch-tree sibling path", levels)?;
    anyhow::ensure!(
        levels <= u64::BITS as usize,
        "MessageRoot compact tree height {levels} exceeds the u64 reconstruction limit"
    );
    let capacity = U256::ONE.checked_shl(levels).ok_or_else(|| {
        anyhow::anyhow!("MessageRoot compact tree height {levels} is unsupported")
    })?;
    anyhow::ensure!(
        tree._nextLeafIndex <= capacity,
        "MessageRoot next leaf index {} exceeds height-{levels} capacity {capacity}",
        tree._nextLeafIndex
    );
    let appended_leaves = u64::try_from(appended_leaves)
        .context("MessageRoot appended-leaf count does not fit u64")?;
    let final_leaf_index = tree
        ._nextLeafIndex
        .checked_add(U256::from(appended_leaves))
        .ok_or_else(|| anyhow::anyhow!("MessageRoot final leaf index overflow"))?;
    anyhow::ensure!(
        final_leaf_index <= U256::from(u64::MAX),
        "MessageRoot final leaf index {final_leaf_index} exceeds the u64 reconstruction limit"
    );
    u64::try_from(tree._nextLeafIndex)
        .map_err(|_| anyhow::anyhow!("MessageRoot next leaf index does not fit u64"))
}

/// Mirrors `MessageHashing.batchLeafHash` in the pinned Era contracts.
fn message_root_batch_leaf_hash(batch_root: B256, batch_number: B256) -> B256 {
    keccak256(
        [
            keccak256(b"zkSync:BatchLeaf").0,
            batch_root.0,
            batch_number.0,
        ]
        .concat(),
    )
}

/// SYSCOIN: Reconstructs the sibling path for one newly appended batch leaf with fallible,
/// checked arithmetic over provider-supplied compact-tree state.
///
/// `tree` is the state before the relevant block range and `new_hashes` are the leaves appended in
/// that range. Leaves through `proof_for_idx` update the stored left sides; later leaves are used to
/// calculate right-side siblings without mutating the historical tree snapshot.
fn calculate_batch_tree_proof(
    mut tree: Bytes32PushTree,
    new_hashes: Vec<B256>,
    proof_for_idx: usize,
) -> anyhow::Result<Vec<B256>> {
    anyhow::ensure!(
        proof_for_idx < new_hashes.len(),
        "batch proof target index {proof_for_idx} is outside {} new leaves",
        new_hashes.len()
    );
    checked_tree_leaf_index(&tree, new_hashes.len())?;

    for hash in new_hashes.iter().take(proof_for_idx + 1) {
        push_to_tree(&mut tree, *hash)?;
    }

    // The returned proof targets the tree after the whole range, even though the compact tree only
    // needs to be mutated through the proven leaf.
    let levels_with_all_leaves = {
        let leaves_after_target = new_hashes
            .len()
            .checked_sub(proof_for_idx + 1)
            .context("batch proof target accounting underflow")?;
        let final_next_leaf_index = u64::try_from(tree._nextLeafIndex)
            .context("MessageRoot next leaf index does not fit u64")?
            .checked_add(
                u64::try_from(leaves_after_target)
                    .context("remaining MessageRoot leaf count does not fit u64")?,
            )
            .context("MessageRoot final next-leaf index overflow")?;
        let last_index = final_next_leaf_index
            .checked_sub(1)
            .context("MessageRoot proof has no final leaf")?;
        match last_index {
            0 => 0,
            last_index => (last_index.ilog2() + 1) as usize,
        }
    };

    // Grow zero subtrees to that final height before calculating right-side siblings.
    let mut zeros = tree._zeros;
    while zeros.len() <= levels_with_all_leaves {
        let zero = *zeros
            .last()
            .context("MessageRoot compact tree has no zero hash while growing proof")?;
        let new_zero = keccak256([zero.0, zero.0].concat());
        zeros.push(new_zero);
    }

    let mut current_index = u64::try_from(tree._nextLeafIndex)
        .context("MessageRoot next leaf index does not fit u64")?
        .checked_sub(1)
        .context("MessageRoot proof target index underflow")?;
    let levels = zeros
        .len()
        .checked_sub(1)
        .context("MessageRoot compact tree has no zero hashes")?;

    let mut node_hash_calculator = NodeHashCalculator::new(
        new_hashes[(proof_for_idx + 1)..].to_vec(),
        current_index
            .checked_add(1)
            .context("MessageRoot right-sibling start index overflow")?,
        zeros,
    )?;
    let mut proof = Vec::new();

    for i in 0..levels {
        let is_left = current_index.is_multiple_of(2);
        if is_left {
            let sibling_index = current_index
                .checked_add(1)
                .context("MessageRoot sibling index overflow")?;
            proof.push(node_hash_calculator.node_hash(i, sibling_index)?);
        } else {
            proof.push(*tree._sides.get(i).with_context(|| {
                format!("MessageRoot compact tree is missing side at level {i}")
            })?);
        }
        current_index /= 2;
    }

    Ok(proof)
}

/// SYSCOIN: Calculates right-side nodes contributed by leaves appended after the proven leaf
/// without native-index overflow or unchecked provider-controlled vector access.
///
/// A proof can ask for the same subtree more than once while walking levels, so computed hashes are
/// cached by `(level, index)`.
#[derive(Debug)]
struct NodeHashCalculator {
    cache: HashMap<(usize, u64), B256>,
    leaves: Vec<B256>,
    first_leaf_index: u64,
    last_non_zero_indices: Vec<u64>,
    zeros: Vec<B256>,
}

impl NodeHashCalculator {
    fn new(leaves: Vec<B256>, first_leaf_index: u64, zeros: Vec<B256>) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !zeros.is_empty(),
            "MessageRoot node calculator has no zero hashes"
        );
        let levels = zeros.len();
        let mut last_non_zero_indices = vec![0; levels];

        let leaves_len = u64::try_from(leaves.len())
            .context("MessageRoot right-sibling leaf count does not fit u64")?;
        let mut last_index = first_leaf_index
            .checked_add(leaves_len)
            .context("MessageRoot right-sibling range overflow")?
            .checked_sub(1)
            .context("MessageRoot empty right-sibling range starts at zero")?;
        for last_non_zero_index in &mut last_non_zero_indices {
            *last_non_zero_index = last_index;
            last_index /= 2;
        }

        Ok(Self {
            cache: HashMap::new(),
            leaves,
            first_leaf_index,
            last_non_zero_indices,
            zeros,
        })
    }

    fn node_hash(&mut self, level: usize, index: u64) -> anyhow::Result<B256> {
        if let Some(cached) = self.cache.get(&(level, index)) {
            return Ok(*cached);
        }

        let hash = self.node_hash_internal(level, index)?;

        self.cache.insert((level, index), hash);
        Ok(hash)
    }

    fn node_hash_internal(&mut self, level: usize, index: u64) -> anyhow::Result<B256> {
        anyhow::ensure!(
            level < self.zeros.len(),
            "MessageRoot node level {level} exceeds {} zero hashes",
            self.zeros.len()
        );

        if index > self.last_non_zero_indices[level] {
            return Ok(self.zeros[level]);
        }

        if level == 0 {
            let range_end = self
                .first_leaf_index
                .checked_add(
                    u64::try_from(self.leaves.len())
                        .context("MessageRoot leaf count does not fit u64")?,
                )
                .context("MessageRoot leaf range overflow")?;
            anyhow::ensure!(
                (self.first_leaf_index..range_end).contains(&index),
                "MessageRoot leaf index {index} is outside {}..{range_end}",
                self.first_leaf_index
            );
            let offset = usize::try_from(index - self.first_leaf_index)
                .context("MessageRoot leaf offset does not fit usize")?;

            return self
                .leaves
                .get(offset)
                .copied()
                .context("MessageRoot leaf offset is unavailable");
        }

        let left_child_index = index
            .checked_mul(2)
            .context("MessageRoot left-child index overflow")?;
        let right_child_index = left_child_index
            .checked_add(1)
            .context("MessageRoot right-child index overflow")?;
        let left_child_hash = self.node_hash(level - 1, left_child_index)?;
        let right_child_hash = self.node_hash(level - 1, right_child_index)?;

        Ok(keccak256([left_child_hash.0, right_child_hash.0].concat()))
    }
}

/// SYSCOIN: Mirrors a `Bytes32PushTree` append while keeping its compact `_sides` representation
/// and rejecting malformed shapes/capacity overflow before mutation.
fn push_to_tree(tree: &mut Bytes32PushTree, leaf: B256) -> anyhow::Result<()> {
    checked_tree_leaf_index(tree, 1)?;
    let mut levels = tree
        ._zeros
        .len()
        .checked_sub(1)
        .context("MessageRoot compact tree has no zero hashes")?;
    let index = tree._nextLeafIndex;
    tree._nextLeafIndex = tree
        ._nextLeafIndex
        .checked_add(U256::ONE)
        .context("MessageRoot next leaf index overflow")?;

    let capacity = U256::ONE
        .checked_shl(levels)
        .context("MessageRoot compact tree height is unsupported")?;
    anyhow::ensure!(
        index <= capacity,
        "MessageRoot next leaf index {index} exceeds height-{levels} capacity {capacity}"
    );
    if index == capacity {
        let zero = *tree
            ._zeros
            .get(levels)
            .context("MessageRoot compact tree is missing its top zero hash")?;
        let new_zero = keccak256([zero.0, zero.0].concat());
        tree._zeros.push(new_zero);
        tree._sides.push(B256::ZERO);
        levels = levels
            .checked_add(1)
            .context("MessageRoot compact tree height overflow")?;
    }

    let mut current_index = index;
    let mut current_level_hash = leaf;
    let mut updated_sides = false;
    for i in 0..levels {
        // A left child becomes the stored side used when a later right child arrives.
        let is_left = current_index % U256::from(2u32) == U256::ZERO;

        if is_left && !updated_sides {
            *tree._sides.get_mut(i).with_context(|| {
                format!("MessageRoot compact tree is missing mutable side at level {i}")
            })? = current_level_hash;
            updated_sides = true;
        }

        // Missing right children use the zero subtree; right children use the remembered left side.
        current_level_hash = if is_left {
            let zero = tree._zeros.get(i).with_context(|| {
                format!("MessageRoot compact tree is missing zero at level {i}")
            })?;
            keccak256([current_level_hash.0, zero.0].concat())
        } else {
            let side = tree._sides.get(i).with_context(|| {
                format!("MessageRoot compact tree is missing side at level {i}")
            })?;
            keccak256([side.0, current_level_hash.0].concat())
        };

        current_index /= U256::from(2u32);
    }

    *tree
        ._sides
        .get_mut(levels)
        .context("MessageRoot compact tree is missing its root side")? = current_level_hash;
    Ok(())
}

// SYSCOIN: Prove `eth_getLogs` completeness by replaying every canonical append into the pinned
// pre-range tree and requiring the exact pinned post-range compact state, including its exposed root.
fn replay_and_validate_compact_tree(
    pre_tree: &Bytes32PushTree,
    post_tree: &Bytes32PushTree,
    new_hashes: &[B256],
    source: &'static str,
) -> anyhow::Result<()> {
    checked_tree_leaf_index(pre_tree, new_hashes.len())?;
    checked_tree_leaf_index(post_tree, 0)?;
    let mut replayed = pre_tree.clone();
    for leaf in new_hashes {
        push_to_tree(&mut replayed, *leaf)?;
    }
    let replayed_root = replayed
        ._sides
        .last()
        .copied()
        .context("replayed MessageRoot tree has no exposed root")?;
    let post_root = post_tree
        ._sides
        .last()
        .copied()
        .context("post-range MessageRoot tree has no exposed root")?;
    anyhow::ensure!(
        replayed._nextLeafIndex == post_tree._nextLeafIndex,
        "{source} MessageRoot replay next index {} does not match pinned post-range index {}",
        replayed._nextLeafIndex,
        post_tree._nextLeafIndex
    );
    anyhow::ensure!(
        replayed._zeros == post_tree._zeros,
        "{source} MessageRoot replay zero subtrees do not match pinned post-range tree"
    );
    anyhow::ensure!(
        replayed._sides == post_tree._sides,
        "{source} MessageRoot replay sides do not match pinned post-range tree"
    );
    anyhow::ensure!(
        replayed_root == post_root,
        "{source} MessageRoot replay root {replayed_root} does not match pinned post-range root {post_root}"
    );
    Ok(())
}

#[derive(Debug, Clone)]
struct ChainAggProof {
    chain_id_leaf_proof: Vec<B256>,
    chain_id_leaf_proof_mask: U256,
    // SYSCOIN: Bind the provider-returned path and mask to independently pinned MessageRoot state.
    pinned_aggregate_root: B256,
}

/// SYSCOIN: Mirrors `MessageHashing.chainIdLeafHash` in the pinned Era V32 contracts.
fn message_root_chain_leaf_hash(chain_root: B256, chain_id: u64) -> B256 {
    keccak256(
        [
            keccak256(b"zkSync:ChainIdLeaf").0,
            chain_root.0,
            U256::from(chain_id).to_be_bytes::<32>(),
        ]
        .concat(),
    )
}

// SYSCOIN: Validate the path/mask against both the Merkle path width and the verifier's packed
// low-128-bit direction-mask field before hashing or encoding it.
fn checked_chain_proof_path_len(chain_proof: &ChainAggProof) -> anyhow::Result<u8> {
    let path_len = checked_proof_path_len(
        "MessageRoot chain sibling path",
        chain_proof.chain_id_leaf_proof.len(),
    )?;
    anyhow::ensure!(
        path_len <= 128,
        "MessageRoot chain sibling path length {path_len} overlaps the batch/block field at bit 128"
    );
    let path_mask_limit = U256::ONE
        .checked_shl(path_len as usize)
        .ok_or_else(|| anyhow::anyhow!("MessageRoot chain path mask width is unsupported"))?;
    anyhow::ensure!(
        chain_proof.chain_id_leaf_proof_mask < path_mask_limit,
        "MessageRoot chain path mask {} has bits outside its {path_len}-word sibling path",
        chain_proof.chain_id_leaf_proof_mask
    );
    Ok(path_len)
}

// SYSCOIN: Reconstruct the shared-tree root from the authenticated post-range chain root and reject
// forged provider paths/masks that do not reach the same hash-pinned aggregate root.
fn authenticate_chain_aggregate_root(
    chain_root: B256,
    chain_id: u64,
    chain_proof: &ChainAggProof,
) -> anyhow::Result<B256> {
    ensure_nonzero_root("MessageRoot post-range chain root", chain_root)?;
    ensure_nonzero_root(
        "MessageRoot pinned aggregate root",
        chain_proof.pinned_aggregate_root,
    )?;
    checked_chain_proof_path_len(chain_proof)?;

    let mut current = message_root_chain_leaf_hash(chain_root, chain_id);
    for (level, sibling) in chain_proof.chain_id_leaf_proof.iter().enumerate() {
        let mask_bit = U256::ONE
            .checked_shl(level)
            .ok_or_else(|| anyhow::anyhow!("MessageRoot chain path level {level} exceeds U256"))?;
        current = if chain_proof.chain_id_leaf_proof_mask & mask_bit != U256::ZERO {
            keccak256([sibling.0, current.0].concat())
        } else {
            keccak256([current.0, sibling.0].concat())
        };
    }
    anyhow::ensure!(
        current == chain_proof.pinned_aggregate_root,
        "MessageRoot chain path reconstructs aggregate root {current}; pinned root is {}",
        chain_proof.pinned_aggregate_root
    );
    Ok(current)
}

// SYSCOIN: Gateway-to-L1 proofs append the Gateway local root as one extra sibling. Use U256
// shifting and check the final metadata width before mutating the proof; native `1u64 << len`
// panics for valid paths of 64 words or more.
fn append_gateway_local_root(
    chain_proof: &mut ChainAggProof,
    gateway_local_root: B256,
) -> anyhow::Result<()> {
    let sibling_index = chain_proof.chain_id_leaf_proof.len();
    let final_len = sibling_index
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("Gateway chain proof length overflow"))?;
    checked_proof_path_len("Gateway chain proof with local root", final_len)?;
    let local_root_mask = U256::ONE.checked_shl(sibling_index).ok_or_else(|| {
        anyhow::anyhow!("Gateway local-root sibling index {sibling_index} exceeds U256 mask width")
    })?;
    chain_proof.chain_id_leaf_proof_mask |= local_root_mask;
    chain_proof.chain_id_leaf_proof.push(gateway_local_root);
    Ok(())
}

/// SYSCOIN: Reads a source chain's path in MessageRoot at one canonical hash-pinned settlement
/// block and bounds the provider-controlled path before returning it.
///
/// The path is historical because a later chain append would authenticate a different shared root.
async fn get_chain_log_proof(
    l2_chain_id: u64,
    settlement_block: CanonicalBlockAnchor,
    l1_provider: &DynProvider,
    message_root_address: Address,
    historical_root_block: Option<u64>,
) -> anyhow::Result<ChainAggProof> {
    let message_root = IMessageRoot::new(message_root_address, l1_provider.clone());
    // SYSCOIN: Aggregate/historical getters are proof anchors, not optional diagnostics.
    let proof_view = IMessageRootProofView::new(message_root_address, l1_provider.clone());
    let merkle_path_builder = message_root
        .getMerklePathForChain(U256::from(l2_chain_id))
        .block(settlement_block.block_id());
    let merkle_path_fut = merkle_path_builder
        .call()
        .into_future()
        .map_err(|e| anyhow::Error::from(e).context("getMerklePathForChain"));
    let chain_index_builder = message_root
        .chainIndex(U256::from(l2_chain_id))
        .block(settlement_block.block_id());
    let chain_index_fut = chain_index_builder
        .call()
        .into_future()
        .map_err(|e| anyhow::Error::from(e).context("chainIndex"));
    let aggregate_root_builder = proof_view
        .getAggregatedRoot()
        .block(settlement_block.block_id());
    let aggregate_root_fut = aggregate_root_builder
        .call()
        .into_future()
        .map_err(|e| anyhow::Error::from(e).context("getAggregatedRoot"));
    let historical_root_fut = async {
        match historical_root_block {
            Some(block_number) => {
                let call = proof_view
                    .historicalRoot(U256::from(block_number))
                    .block(settlement_block.block_id());
                call.call()
                    .await
                    .map(Some)
                    .map_err(anyhow::Error::from)
                    .context("historicalRoot")
            }
            None => Ok(None),
        }
    };
    let (merkle_path, chain_index, aggregate_root, historical_root) = futures::try_join!(
        merkle_path_fut,
        chain_index_fut,
        aggregate_root_fut,
        historical_root_fut
    )?;
    // SYSCOIN: Provider-returned path lengths feed fixed-width proof metadata; validate them before
    // retaining any accompanying direction mask.
    checked_proof_path_len("MessageRoot chain sibling path", merkle_path.len())?;
    // SYSCOIN: Gateway block-target proofs must authenticate the exact persisted historical root;
    // recursive Gateway-batch proofs bind to the hash-pinned aggregate state instead.
    ensure_nonzero_root("MessageRoot aggregate root", aggregate_root)?;
    if let (Some(block_number), Some(historical_root)) = (historical_root_block, historical_root) {
        ensure_nonzero_root("Gateway MessageRoot historical root", historical_root)?;
        anyhow::ensure!(
            historical_root == aggregate_root,
            "Gateway historical root {historical_root} at block {} does not match pinned aggregate root {aggregate_root}",
            block_number
        );
    }
    Ok(ChainAggProof {
        chain_id_leaf_proof: merkle_path,
        chain_id_leaf_proof_mask: chain_index,
        pinned_aggregate_root: aggregate_root,
    })
}

/// SYSCOIN: Encodes and validates a chain-tree proof in the flat `bytes32[]` layout consumed by the
/// on-chain verifier.
///
/// The prefix is `[block-or-batch number + path mask, settlement-layer chain id, metadata]`,
/// followed by the chain-tree siblings. The metadata marks this as the final proof segment.
fn chain_proof_vector(
    batch_or_block_number: u64,
    chain_agg_proof: ChainAggProof,
    sl_chain_id: u64,
) -> anyhow::Result<Vec<B256>> {
    // SYSCOIN: Keep validation shared with aggregate-root reconstruction so encoded proof words
    // cannot accept a shape that the authentication step rejected (or vice versa).
    checked_chain_proof_path_len(&chain_agg_proof)?;
    let encoded_mask_limit = U256::ONE << 128;
    anyhow::ensure!(
        chain_agg_proof.chain_id_leaf_proof_mask < encoded_mask_limit,
        "MessageRoot chain path mask {} overlaps the batch/block field at bit 128",
        chain_agg_proof.chain_id_leaf_proof_mask
    );
    let sl_encoded_data = (U256::from(batch_or_block_number) << U256::from(128u32))
        + chain_agg_proof.chain_id_leaf_proof_mask;

    let mut chain_proof_vector = vec![
        B256::from(sl_encoded_data.to_be_bytes()),
        B256::from(U256::from(sl_chain_id).to_be_bytes()),
        proof_metadata(chain_agg_proof.chain_id_leaf_proof.len(), 0, true)?,
    ];
    chain_proof_vector.extend(chain_agg_proof.chain_id_leaf_proof);

    Ok(chain_proof_vector)
}

/// SYSCOIN: Builds the batch-tree segment for `batch_number` from hash-pinned MessageRoot state
/// and the complete canonical append-event sequence.
///
/// The tree is read immediately before the settlement block range; matching
/// `AppendedChainBatchRoot` events from the range are replayed in order and checked against the
/// independently pinned post-range tree. The returned words are `[absolute leaf index, sibling
/// path...]`; the separate length counts only the sibling path because outer proof metadata needs it.
// SYSCOIN: Keep every independently authenticated identity/range/root input explicit at this
// security boundary rather than hiding them in mutable provider state.
#[allow(clippy::too_many_arguments)]
async fn batch_tree_proof(
    settlement_block_range: ops::RangeInclusive<u64>,
    canonical_blocks: &CanonicalBlockRange,
    l2_chain_id: u64,
    batch_number: u64,
    expected_chain_batch_root: B256,
    provider: &DynProvider,
    message_root_address: Address,
    source: &'static str,
) -> anyhow::Result<(Vec<B256>, u8, B256)> {
    // SYSCOIN: The caller reconstructed this root from local batch data; never let provider logs
    // substitute a different nonzero root for the requested batch.
    ensure_nonzero_root(
        "requested local chain batch root",
        expected_chain_batch_root,
    )?;
    let range_start = *settlement_block_range.start();
    let range_end = *settlement_block_range.end();
    anyhow::ensure!(
        range_start > 0 && range_start <= range_end,
        "cannot reconstruct {source} MessageRoot batch proof over range {settlement_block_range:?}"
    );
    let pre_block = canonical_blocks.anchor(range_start - 1)?;
    let post_block = canonical_blocks.anchor(range_end)?;

    let message_root = IMessageRoot::new(message_root_address, provider.clone());
    let pre_tree_call = message_root
        .getChainTree(U256::from(l2_chain_id))
        .block(pre_block.block_id());
    let pre_tree_future = pre_tree_call
        .call()
        .into_future()
        .map_err(|e| anyhow::Error::from(e).context("get pre-range getChainTree"));
    let post_tree_call = message_root
        .getChainTree(U256::from(l2_chain_id))
        .block(post_block.block_id());
    let post_tree_future = post_tree_call
        .call()
        .into_future()
        .map_err(|e| anyhow::Error::from(e).context("get post-range getChainTree"));

    let filter = Filter::new()
        .event_signature(AppendedChainBatchRoot::SIGNATURE_HASH)
        .topic1(U256::from(l2_chain_id))
        .address(message_root_address);
    // SYSCOIN: EIP-234 permits an exact block-hash log filter for a single block. Multi-block
    // Gateway scans have no hash-range RPC form, so every returned log is checked against the
    // anchored ancestry and the canonical tip is revalidated by the outer builder.
    let filter = if range_start == range_end {
        filter.at_block_hash(post_block.hash)
    } else {
        filter.from_block(range_start).to_block(range_end)
    };
    let logs_future = provider
        .get_logs(&filter)
        .map_err(|e| anyhow::Error::from(e).context("get_logs for AppendedChainBatchRoot"));

    let (pre_tree, post_tree, logs) =
        futures::future::try_join3(pre_tree_future, post_tree_future, logs_future).await?;

    // SYSCOIN: Do not trust an RPC server to honor the Gateway log filter. The strict canonical
    // decoder rejects malformed identity, metadata, range, and ordering before reconstructing
    // consensus-facing proof words.
    let (events, batch_idx) = decode_batch_root_logs(
        logs,
        message_root_address,
        l2_chain_id,
        &settlement_block_range,
        canonical_blocks,
        batch_number,
        expected_chain_batch_root,
        source,
    )?;
    // SYSCOIN: Validate the provider-returned compact tree against the complete append set before
    // entering the native-width reconstruction algorithm.
    let absolute_batch_idx = checked_tree_leaf_index(&pre_tree, events.len())?
        .checked_add(u64::try_from(batch_idx).context("batch event index does not fit u64")?)
        .ok_or_else(|| anyhow::anyhow!("MessageRoot absolute batch leaf index overflow"))?;

    let new_hashes: Vec<B256> = events
        .into_iter()
        .map(|event| {
            message_root_batch_leaf_hash(
                event.chain_batch_root,
                B256::from(U256::from(event.batch_number).to_be_bytes::<32>()),
            )
        })
        .collect();

    // SYSCOIN: A filtered log set is complete only if replaying every append yields the separately
    // hash-pinned post-range compact tree. This detects omitted, injected, or reordered events.
    replay_and_validate_compact_tree(&pre_tree, &post_tree, &new_hashes, source)?;
    // SYSCOIN: This independently pinned post-range chain root is the only valid input to the
    // provider-supplied shared-tree path authentication.
    let post_chain_root = post_tree
        ._sides
        .last()
        .copied()
        .context("post-range MessageRoot tree has no exposed root")?;
    ensure_nonzero_root("post-range MessageRoot chain root", post_chain_root)?;
    let batch_proof = calculate_batch_tree_proof(pre_tree, new_hashes, batch_idx)?;
    let batch_proof_len =
        checked_proof_path_len("MessageRoot batch sibling path", batch_proof.len())?;

    // `_getProofData` reads the leaf index before consuming the sibling path.
    let mut proof = vec![B256::from(U256::from(absolute_batch_idx).to_be_bytes())];
    proof.extend(batch_proof);

    Ok((proof, batch_proof_len, post_chain_root))
}

/// Opaque MessageRoot extension appended after a source batch's log proof.
pub(crate) struct MessageRootProofExtension {
    batch_proof_len: u8,
    words: Vec<B256>,
}

// SYSCOIN: Retain only canonically decoded event fields used to reconstruct either settlement tree.
#[derive(Debug)]
struct DecodedBatchRoot {
    batch_number: u64,
    chain_batch_root: B256,
    block_number: u64,
}

// SYSCOIN: Cap decoder work even if a provider ignores the filter and returns an excessive array.
fn validate_batch_root_log_count(count: usize) -> anyhow::Result<()> {
    anyhow::ensure!(
        count <= MAX_BATCH_ROOT_EVENTS_PER_PROOF,
        "MessageRoot proof returned {count} batch-root logs; maximum is {MAX_BATCH_ROOT_EVENTS_PER_PROOF}"
    );
    Ok(())
}

// SYSCOIN: Keep provider-controlled missing-target diagnostics bounded and state omitted count.
fn observed_batch_root_diagnostic(events: &[DecodedBatchRoot]) -> String {
    let mut observed = events
        .iter()
        .take(MAX_DIAGNOSTIC_BATCH_EVENTS)
        .map(|event| format!("{}@{}", event.batch_number, event.block_number))
        .collect::<Vec<_>>()
        .join(", ");
    let omitted = events.len().saturating_sub(MAX_DIAGNOSTIC_BATCH_EVENTS);
    if omitted > 0 {
        if !observed.is_empty() {
            observed.push_str(", ");
        }
        observed.push_str(&format!("... {omitted} additional event(s) omitted"));
    }
    observed
}

// SYSCOIN: An L1 or Gateway RPC is a configured trust dependency, not a license to panic or forge
// public proof data. Decode the exact pinned Era event ABI and independently enforce every filter
// property because a faulty or Byzantine provider can return arbitrary logs.
#[allow(clippy::too_many_arguments)]
fn decode_batch_root_logs(
    logs: Vec<Log>,
    expected_address: Address,
    expected_chain_id: u64,
    expected_block_range: &ops::RangeInclusive<u64>,
    canonical_blocks: &CanonicalBlockRange,
    target_batch_number: u64,
    expected_target_root: B256,
    source: &'static str,
) -> anyhow::Result<(Vec<DecodedBatchRoot>, usize)> {
    // SYSCOIN: Reject over-cap responses before iteration, formatting, or proof allocations.
    validate_batch_root_log_count(logs.len())?;
    ensure_nonzero_root("requested local chain batch root", expected_target_root)?;
    let mut previous_position = None;
    // SYSCOIN: Keep checked consecutive arithmetic explicitly in the contract's u64 proof domain.
    let mut previous_batch_number: Option<u64> = None;
    let mut target_index = None;
    let events = logs
        .into_iter()
        .map(|log| {
            anyhow::ensure!(
                log.inner.address == expected_address,
                "{source} batch-root log has unexpected address {}; expected {}",
                log.inner.address,
                expected_address
            );
            anyhow::ensure!(!log.removed, "{source} batch-root log is marked removed");
            let block_number = log
                .block_number
                .with_context(|| format!("{source} batch-root log is missing its block number"))?;
            anyhow::ensure!(
                expected_block_range.contains(&block_number),
                "{source} batch-root log block {block_number} is outside requested range {expected_block_range:?}"
            );
            let block_hash = log
                .block_hash
                .with_context(|| format!("{source} batch-root log is missing its block hash"))?;
            let canonical_block = canonical_blocks.anchor(block_number)?;
            anyhow::ensure!(
                block_hash == canonical_block.hash,
                "{source} batch-root log at block #{block_number} has hash {block_hash}; expected anchored hash {}",
                canonical_block.hash
            );
            let log_index = log
                .log_index
                .with_context(|| format!("{source} batch-root log is missing its log index"))?;
            let position = (block_number, log_index);
            if let Some(previous_position) = previous_position {
                anyhow::ensure!(
                    position > previous_position,
                    "{source} batch-root logs are not in canonical (block, log-index) order: \
                     {position:?} follows {previous_position:?}"
                );
            }
            previous_position = Some(position);
            let event = AppendedChainBatchRoot::decode_log(&log.inner)
                .map_err(anyhow::Error::from)
                .with_context(|| {
                    format!("decode {source} AppendedChainBatchRoot log at block {block_number}")
                })?
                .data;
            anyhow::ensure!(
                event.chainId == U256::from(expected_chain_id),
                "{source} batch-root log at block {block_number} has chain ID {}; expected {expected_chain_id}",
                event.chainId
            );
            let batch_number = u64::try_from(event.batchNumber).with_context(|| {
                format!(
                    "{source} batch-root log at block {block_number} has non-u64 batch number {}",
                    event.batchNumber
                )
            })?;
            // SYSCOIN: V32 `addChainBatchRoot` rejects zero roots and advances exactly one batch;
            // enforce both invariants independently of provider filtering/event fidelity.
            ensure_nonzero_root("AppendedChainBatchRoot.chainBatchRoot", event.chainBatchRoot)?;
            if let Some(previous_batch_number) = previous_batch_number {
                let expected_batch_number = previous_batch_number.checked_add(1).ok_or_else(|| {
                    anyhow::anyhow!("{source} batch-root number overflow after {previous_batch_number}")
                })?;
                anyhow::ensure!(
                    batch_number == expected_batch_number,
                    "{source} batch-root numbers are not consecutive: {batch_number} follows {previous_batch_number}"
                );
            }
            previous_batch_number = Some(batch_number);
            Ok(DecodedBatchRoot {
                batch_number,
                chain_batch_root: event.chainBatchRoot,
                block_number,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    for (index, event) in events.iter().enumerate() {
        if event.batch_number == target_batch_number {
            anyhow::ensure!(
                target_index.replace(index).is_none(),
                "{source} target batch #{target_batch_number} appears more than once"
            );
        }
    }
    let target_index = target_index.with_context(|| {
        let observed = observed_batch_root_diagnostic(&events);
        format!(
            "target batch #{target_batch_number} not found in {source} MessageRoot logs for blocks \
             {expected_block_range:?}; observed [{observed}]"
        )
    })?;
    // SYSCOIN: Bind the exact target event to the root reconstructed from local batch logs/state.
    let target_event = events
        .get(target_index)
        .context("validated target batch index is unavailable")?;
    anyhow::ensure!(
        target_event.chain_batch_root == expected_target_root,
        "{source} target batch #{target_batch_number} root {} does not match locally reconstructed root {expected_target_root}",
        target_event.chain_batch_root
    );
    Ok((events, target_index))
}

// SYSCOIN: `unstable_getBatchByBlockNumber` is provider metadata, so validate its claimed range
// before using it to drive ancestry walks or log scans.
fn validate_gateway_block_range(
    block_range: &ops::RangeInclusive<u64>,
    requested_block: u64,
) -> anyhow::Result<()> {
    let start = *block_range.start();
    let end = *block_range.end();
    anyhow::ensure!(
        start > 0 && start <= end,
        "Gateway batch metadata returned invalid block range {block_range:?}"
    );
    anyhow::ensure!(
        block_range.contains(&requested_block),
        "Gateway batch range {block_range:?} does not contain requested execution block #{requested_block}"
    );
    let span = end
        .checked_sub(start)
        .and_then(|delta| delta.checked_add(1))
        .context("Gateway batch block-range span overflow")?;
    anyhow::ensure!(
        span <= MAX_GATEWAY_PROOF_BLOCK_SPAN,
        "Gateway batch range {block_range:?} spans {span} blocks; maximum is {MAX_GATEWAY_PROOF_BLOCK_SPAN}"
    );
    Ok(())
}

// SYSCOIN: A Gateway batch commits `keccak(local L2-to-L1 root, MessageRoot aggregate root)`.
// Authenticate that exact composition before adding the local root as the final Merkle sibling.
fn authenticate_gateway_batch_root(
    gateway_local_root: B256,
    gateway_aggregate_root: B256,
    stored_gateway_batch_root: B256,
) -> anyhow::Result<B256> {
    ensure_nonzero_root("Gateway local L2-to-L1 root", gateway_local_root)?;
    ensure_nonzero_root("Gateway MessageRoot aggregate root", gateway_aggregate_root)?;
    ensure_nonzero_root(
        "stored Gateway batch L2-to-L1 root",
        stored_gateway_batch_root,
    )?;
    let reconstructed = keccak256([gateway_local_root.0, gateway_aggregate_root.0].concat());
    anyhow::ensure!(
        reconstructed == stored_gateway_batch_root,
        "Gateway batch root reconstructed as {reconstructed}; stored batch commits {stored_gateway_batch_root}"
    );
    Ok(reconstructed)
}

// SYSCOIN: The final L1 mapping is the recursive verifier's source of truth for a Gateway batch.
fn authenticate_l1_gateway_batch_root(
    l1_stored_root: B256,
    reconstructed_gateway_batch_root: B256,
) -> anyhow::Result<()> {
    ensure_nonzero_root("L1-stored Gateway chain batch root", l1_stored_root)?;
    ensure_nonzero_root(
        "reconstructed Gateway batch root",
        reconstructed_gateway_batch_root,
    )?;
    anyhow::ensure!(
        l1_stored_root == reconstructed_gateway_batch_root,
        "L1 MessageRoot stores Gateway batch root {l1_stored_root}; reconstructed root is {reconstructed_gateway_batch_root}"
    );
    Ok(())
}

// SYSCOIN: For a full Gateway-to-L1 proof, bind the reconstructed Gateway batch root to the exact
// L1 MessageRoot mapping at the Gateway batch's recorded L1 execution block.
async fn authenticate_gateway_batch_on_l1(
    gateway_chain_id: u64,
    gateway_batch_number: u64,
    gateway_batch_root: B256,
    l1_execution_block_number: u64,
    expected_l1_chain_id: u64,
    l1_provider: &DynProvider,
    bridgehub_address: Address,
) -> anyhow::Result<()> {
    ensure_nonzero_root("reconstructed Gateway batch root", gateway_batch_root)?;
    anyhow::ensure!(
        l1_execution_block_number > 0,
        "Gateway batch #{gateway_batch_number} has invalid L1 execution block 0"
    );
    let canonical_l1 = canonical_block_range(
        l1_provider,
        l1_execution_block_number,
        l1_execution_block_number,
        "L1",
    )
    .await?;
    let l1_anchor = canonical_l1.end();
    let bridgehub = IBridgehub::new(bridgehub_address, l1_provider.clone());
    let message_root_call = bridgehub.messageRoot().block(l1_anchor.block_id());
    let message_root_fut = message_root_call
        .call()
        .into_future()
        .map_err(|err| anyhow::Error::from(err).context("L1 bridgehub.messageRoot()"));
    let l1_chain_id_fut = l1_provider
        .get_chain_id()
        .map_err(|err| anyhow::Error::from(err).context("get_chain_id (L1)"));
    let (l1_message_root_address, observed_l1_chain_id) =
        futures::future::try_join(message_root_fut, l1_chain_id_fut).await?;
    ensure_chain_identity("L1", expected_l1_chain_id, observed_l1_chain_id)?;
    anyhow::ensure!(
        l1_message_root_address != Address::ZERO,
        "Bridgehub returned zero MessageRoot address at Gateway L1 execution block"
    );

    let proof_view = IMessageRootProofView::new(l1_message_root_address, l1_provider.clone());
    let stored_root = proof_view
        .chainBatchRoots(
            U256::from(gateway_chain_id),
            U256::from(gateway_batch_number),
        )
        .block(l1_anchor.block_id())
        .call()
        .await
        .context("L1 MessageRoot.chainBatchRoots(Gateway, batch)")?;
    // SYSCOIN: Reject zero/mismatched L1 mapping values before returning recursive proof words.
    authenticate_l1_gateway_batch_root(stored_root, gateway_batch_root)?;
    revalidate_canonical_tip(l1_provider, l1_anchor, "L1").await
}

/// SYSCOIN: Extends a V32 source-chain proof through its Gateway settlement segment. The caller
/// owns the single cumulative deadline so optimistic execution discovery and this complete
/// cross-layer reconstruction cannot reset independent timeout budgets.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn build_gateway_proof_extension(
    l2_chain_id: u64,
    batch_number: u64,
    expected_chain_batch_root: B256,
    execute_gateway_block_number: u64,
    stop_at_gateway_message_root: bool,
    expected_gateway_chain_id: u64,
    gateway_provider: &DynProvider,
    expected_l1_chain_id: u64,
    l1_provider: &DynProvider,
    bridgehub_address: Address,
) -> anyhow::Result<MessageRootProofExtension> {
    let (block_range, chain_proof_number, gateway_batch_metadata) = if stop_at_gateway_message_root
    {
        (
            execute_gateway_block_number..=execute_gateway_block_number,
            execute_gateway_block_number,
            None,
        )
    } else {
        let gateway_batch: PersistedBatch = gateway_provider
            .raw_request(
                "unstable_getBatchByBlockNumber".into(),
                (execute_gateway_block_number,),
            )
            .await
            .context("unstable_getBatchByBlockNumber")?;
        validate_gateway_block_range(&gateway_batch.block_range, execute_gateway_block_number)?;
        (
            gateway_batch.block_range.clone(),
            gateway_batch.number(),
            Some(gateway_batch),
        )
    };
    validate_gateway_block_range(&block_range, execute_gateway_block_number)?;
    let range_start = *block_range.start();
    let canonical_blocks = canonical_block_range(
        gateway_provider,
        range_start - 1,
        *block_range.end(),
        "Gateway",
    )
    .await?;
    let chain_proof_block = canonical_blocks.end();

    let chain_proof_fut = get_chain_log_proof(
        l2_chain_id,
        chain_proof_block,
        gateway_provider,
        L2_MESSAGE_ROOT_ADDRESS,
        stop_at_gateway_message_root.then_some(chain_proof_number),
    );
    let batch_proof_fut = batch_tree_proof(
        block_range,
        &canonical_blocks,
        l2_chain_id,
        batch_number,
        expected_chain_batch_root,
        gateway_provider,
        L2_MESSAGE_ROOT_ADDRESS,
        "Gateway",
    );
    let gateway_chain_id_fut = gateway_provider
        .get_chain_id()
        .map_err(|err| anyhow::Error::from(err).context("get_chain_id (Gateway)"));
    let gateway_local_root_fut = async {
        if gateway_batch_metadata.is_some() {
            gateway_provider
                .raw_request("unstable_getLocalRoot".into(), (chain_proof_number,))
                .await
                .map(Some)
                .map_err(anyhow::Error::from)
                .context("unstable_getLocalRoot")
        } else {
            Ok(None)
        }
    };

    let (
        mut chain_proof,
        (mut words, batch_proof_len, post_chain_root),
        observed_gateway_chain_id,
        gateway_local_root,
    ) = futures::try_join!(
        chain_proof_fut,
        batch_proof_fut,
        gateway_chain_id_fut,
        gateway_local_root_fut
    )?;
    // SYSCOIN: Bind interval-discovered Gateway identity and authenticate its child-chain path
    // against the same hash-pinned post-range aggregate root before any recursive extension.
    ensure_chain_identity(
        "Gateway",
        expected_gateway_chain_id,
        observed_gateway_chain_id,
    )?;
    let gateway_aggregate_root =
        authenticate_chain_aggregate_root(post_chain_root, l2_chain_id, &chain_proof)?;
    // SYSCOIN: Gateway's unstable metadata/local-root RPCs do not accept EIP-1898 block IDs.
    // Double-read them around all hash-pinned proof work and require byte-for-byte stability, then
    // revalidate the canonical range tip. This is the strongest binding their current API permits.
    let gateway_l1_auth = if let Some(initial_gateway_batch) = gateway_batch_metadata {
        let final_gateway_batch: PersistedBatch = gateway_provider
            .raw_request(
                "unstable_getBatchByBlockNumber".into(),
                (execute_gateway_block_number,),
            )
            .await
            .context("revalidate unstable_getBatchByBlockNumber")?;
        anyhow::ensure!(
            final_gateway_batch == initial_gateway_batch,
            "Gateway batch metadata changed while building the proof"
        );
        let initial_local_root = gateway_local_root
            .context("Gateway local root is missing for a Gateway-batch proof")?;
        let final_local_root: B256 = gateway_provider
            .raw_request("unstable_getLocalRoot".into(), (chain_proof_number,))
            .await
            .context("revalidate unstable_getLocalRoot")?;
        anyhow::ensure!(
            final_local_root == initial_local_root,
            "Gateway local root changed while building the proof"
        );
        // SYSCOIN: The appended local-root sibling must reconstruct the exact stored Gateway batch
        // L2-to-L1 root, not merely a self-consistent provider path.
        let gateway_batch_root = authenticate_gateway_batch_root(
            initial_local_root,
            gateway_aggregate_root,
            initial_gateway_batch.batch_info.l2_to_l1_logs_root_hash,
        )?;
        let l1_execution_block_number = initial_gateway_batch
            .execute_sl_block_number
            .with_context(|| {
                format!(
                    "Gateway batch #{} has not been executed on L1",
                    initial_gateway_batch.number()
                )
            })?;
        append_gateway_local_root(&mut chain_proof, initial_local_root)?;
        Some((
            initial_gateway_batch.number(),
            gateway_batch_root,
            l1_execution_block_number,
        ))
    } else {
        anyhow::ensure!(
            gateway_local_root.is_none(),
            "unexpected Gateway local root for MessageRoot-target proof"
        );
        None
    };
    revalidate_canonical_tip(gateway_provider, chain_proof_block, "Gateway").await?;
    // SYSCOIN: A full recursive proof is not returned until the reconstructed Gateway batch root
    // is found under the startup-discovered L1 topology at its recorded execution block.
    if let Some((gateway_batch_number, gateway_batch_root, l1_execution_block_number)) =
        gateway_l1_auth
    {
        authenticate_gateway_batch_on_l1(
            expected_gateway_chain_id,
            gateway_batch_number,
            gateway_batch_root,
            l1_execution_block_number,
            expected_l1_chain_id,
            l1_provider,
            bridgehub_address,
        )
        .await?;
    }
    words.extend(chain_proof_vector(
        chain_proof_number,
        chain_proof,
        expected_gateway_chain_id,
    )?);
    Ok(MessageRootProofExtension {
        batch_proof_len,
        words,
    })
}

/// SYSCOIN: Encodes the full contract-facing proof while validating every fixed-width segment in
/// one place, so final direct-L1 and recursive Gateway proofs cannot truncate path lengths.
pub(crate) fn assemble_log_proof(
    mut log_leaf_proof: Vec<B256>,
    extension: Option<MessageRootProofExtension>,
) -> anyhow::Result<Vec<B256>> {
    let (batch_proof_len, is_final_node, extension_words) = match extension {
        Some(extension) => (extension.batch_proof_len, false, extension.words),
        None => (0, true, Vec::new()),
    };
    let metadata = proof_metadata(log_leaf_proof.len(), batch_proof_len, is_final_node)?;

    let mut proof = Vec::with_capacity(1 + log_leaf_proof.len() + extension_words.len());
    proof.push(metadata);
    proof.append(&mut log_leaf_proof);
    proof.extend(extension_words);
    Ok(proof)
}

// SYSCOIN: The pinned verifier ABI allocates one byte to each path length; keep the narrowing check
// inseparable from metadata construction.
fn proof_metadata(
    log_proof_len: usize,
    batch_proof_len: u8,
    is_final_node: bool,
) -> anyhow::Result<B256> {
    let mut metadata = [0u8; 32];
    metadata[0] = LOG_PROOF_SUPPORTED_METADATA_VERSION;
    metadata[1] = checked_proof_path_len("L2-to-L1 log sibling path", log_proof_len)?;
    metadata[2] = batch_proof_len;
    metadata[3] = u8::from(is_final_node);
    Ok(metadata.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{Bytes, IntoLogData, LogData, b256};

    // SYSCOIN: Deterministic contiguous anchors let decoder/tree tests exercise the same block-hash
    // checks as production without an RPC transport.
    fn test_block_hash(number: u64) -> B256 {
        B256::from(U256::from(number).to_be_bytes::<32>())
    }

    fn test_canonical_blocks(start: u64, end: u64) -> CanonicalBlockRange {
        CanonicalBlockRange::new(
            (start..=end)
                .map(|number| CanonicalBlockAnchor {
                    number,
                    hash: test_block_hash(number),
                    parent_hash: test_block_hash(number.saturating_sub(1)),
                })
                .collect(),
        )
        .unwrap()
    }

    fn decode_test_batch_root_logs(
        logs: Vec<Log>,
        expected_address: Address,
        expected_chain_id: u64,
        expected_block_range: &ops::RangeInclusive<u64>,
        source: &'static str,
    ) -> anyhow::Result<(Vec<DecodedBatchRoot>, usize)> {
        let canonical_blocks = test_canonical_blocks(
            expected_block_range.start().saturating_sub(1),
            *expected_block_range.end(),
        );
        decode_batch_root_logs(
            logs,
            expected_address,
            expected_chain_id,
            expected_block_range,
            &canonical_blocks,
            99,
            B256::repeat_byte(0x42),
            source,
        )
    }

    // SYSCOIN: Tests that exercise target-root binding pass an explicit locally reconstructed root.
    fn decode_test_batch_root_logs_for_root(
        logs: Vec<Log>,
        expected_address: Address,
        expected_chain_id: u64,
        expected_block_range: &ops::RangeInclusive<u64>,
        expected_target_root: B256,
        source: &'static str,
    ) -> anyhow::Result<(Vec<DecodedBatchRoot>, usize)> {
        let canonical_blocks = test_canonical_blocks(
            expected_block_range.start().saturating_sub(1),
            *expected_block_range.end(),
        );
        decode_batch_root_logs(
            logs,
            expected_address,
            expected_chain_id,
            expected_block_range,
            &canonical_blocks,
            99,
            expected_target_root,
            source,
        )
    }

    // SYSCOIN: Produce the exact compact-tree zero recurrence used by the pinned V32 contract.
    fn canonical_zero_hashes(len: usize) -> Vec<B256> {
        let mut zeros = Vec::with_capacity(len);
        if len == 0 {
            return zeros;
        }
        zeros.push(CHAIN_TREE_EMPTY_ENTRY_HASH);
        while zeros.len() < len {
            let previous = zeros[zeros.len() - 1];
            zeros.push(keccak256([previous.0, previous.0].concat()));
        }
        zeros
    }

    // SYSCOIN: Build canonical pinned-Era event logs for strict Gateway decoder tests.
    fn batch_root_log(
        address: Address,
        chain_id: u64,
        batch_number: u64,
        chain_batch_root: B256,
    ) -> Log {
        batch_root_log_with_number(
            address,
            chain_id,
            U256::from(batch_number),
            chain_batch_root,
        )
    }

    // SYSCOIN: Preserve arbitrary untrusted ABI values so overflow regressions can exercise the
    // production u64 narrowing boundary before reconstruction.
    fn batch_root_log_with_number(
        address: Address,
        chain_id: u64,
        batch_number: U256,
        chain_batch_root: B256,
    ) -> Log {
        let event = AppendedChainBatchRoot {
            chainId: U256::from(chain_id),
            batchNumber: batch_number,
            chainBatchRoot: chain_batch_root,
        };
        Log {
            inner: alloy::primitives::Log {
                address,
                data: event.into_log_data(),
            },
            block_hash: Some(test_block_hash(100)),
            block_number: Some(100),
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: Some(0),
            removed: false,
        }
    }

    #[test]
    fn canonical_batch_root_event_signature_is_immutable() {
        // This is the exact signature emitted by the pinned Era IMessageRoot. Adding a timestamp
        // changes topic0 and hides every canonical event.
        assert_eq!(
            AppendedChainBatchRoot::SIGNATURE_HASH,
            b256!("0x4f7fd9ed016150a623d5a2cf43053fe313a56293a77e060a05db49ed22579520")
        );
    }

    #[test]
    fn canonical_batch_leaf_hash_matches_pinned_era_contract() {
        assert_eq!(
            message_root_batch_leaf_hash(
                B256::repeat_byte(0x11),
                B256::from(U256::from(42).to_be_bytes()),
            ),
            b256!("0x74b7992b6908f6e7c58378abc88a8344a960ebeb389c885900c2127036d501b2")
        );
    }

    // SYSCOIN: Freeze the packed `CHAIN_ID_LEAF_PADDING || chainRoot || uint256(chainId)` domain.
    #[test]
    fn canonical_chain_leaf_hash_matches_pinned_era_contract() {
        assert_eq!(
            message_root_chain_leaf_hash(B256::repeat_byte(0x11), 42),
            b256!("0xff5f51c6770c4b9511060d6f71d90b65b747b5c7f8fe08eda142b74eb35a1294")
        );
    }

    // SYSCOIN: Shared-tree authentication binds both sibling ordering and the pinned aggregate.
    #[test]
    fn chain_path_authenticates_post_tree_root_and_rejects_forgery() {
        let chain_id = 57;
        let chain_root = B256::repeat_byte(0x11);
        let siblings = vec![B256::repeat_byte(0x22), B256::repeat_byte(0x33)];
        let mask = U256::ONE;
        let leaf = message_root_chain_leaf_hash(chain_root, chain_id);
        let parent = keccak256([siblings[0].0, leaf.0].concat());
        let aggregate_root = keccak256([parent.0, siblings[1].0].concat());
        let proof = ChainAggProof {
            chain_id_leaf_proof: siblings,
            chain_id_leaf_proof_mask: mask,
            pinned_aggregate_root: aggregate_root,
        };

        assert_eq!(
            authenticate_chain_aggregate_root(chain_root, chain_id, &proof).unwrap(),
            aggregate_root
        );

        let mut forged_sibling = proof.clone();
        forged_sibling.chain_id_leaf_proof[0] = B256::repeat_byte(0x44);
        assert!(authenticate_chain_aggregate_root(chain_root, chain_id, &forged_sibling).is_err());

        let mut forged_mask = proof.clone();
        forged_mask.chain_id_leaf_proof_mask = U256::ZERO;
        assert!(authenticate_chain_aggregate_root(chain_root, chain_id, &forged_mask).is_err());

        let mut forged_anchor = proof;
        forged_anchor.pinned_aggregate_root = B256::repeat_byte(0x55);
        assert!(authenticate_chain_aggregate_root(chain_root, chain_id, &forged_anchor).is_err());
    }

    // SYSCOIN: Gateway recursion is valid only when local+aggregate reconstruction equals the
    // stored Gateway batch root that will be authenticated on L1.
    #[test]
    fn gateway_batch_root_composition_is_exact_and_nonzero() {
        let local_root = B256::repeat_byte(0x11);
        let aggregate_root = B256::repeat_byte(0x22);
        let stored_root = keccak256([local_root.0, aggregate_root.0].concat());
        assert_eq!(
            authenticate_gateway_batch_root(local_root, aggregate_root, stored_root).unwrap(),
            stored_root
        );
        assert!(
            authenticate_gateway_batch_root(local_root, aggregate_root, B256::repeat_byte(0x33))
                .is_err()
        );
        assert!(authenticate_gateway_batch_root(B256::ZERO, aggregate_root, stored_root).is_err());
        assert!(authenticate_l1_gateway_batch_root(stored_root, stored_root).is_ok());
        assert!(authenticate_l1_gateway_batch_root(B256::repeat_byte(0x44), stored_root).is_err());
        assert!(authenticate_l1_gateway_batch_root(B256::ZERO, stored_root).is_err());
    }

    // SYSCOIN: Live chain IDs validate startup topology; they never select proof metadata.
    #[test]
    fn settlement_identity_must_match_startup_discovery() {
        assert!(ensure_chain_identity("L1", 1, 1).is_ok());
        assert!(ensure_chain_identity("L1", 1, 9).is_err());
        assert!(ensure_chain_identity("Gateway", 506, 507).is_err());
    }

    // SYSCOIN: The whole proof future, not each individual provider call, has one deadline.
    #[tokio::test]
    async fn settlement_proof_deadline_is_cumulative_and_testable() {
        let ready = with_settlement_proof_deadline("test", Duration::from_secs(1), async {
            Ok::<_, anyhow::Error>(42_u8)
        })
        .await
        .unwrap();
        assert_eq!(ready, 42);

        let pending = futures::future::pending::<anyhow::Result<()>>();
        assert!(
            with_settlement_proof_deadline("test", Duration::from_millis(1), pending)
                .await
                .is_err()
        );
    }

    #[test]
    fn assembles_final_log_proof_metadata() {
        let log_proof = vec![B256::repeat_byte(1), B256::repeat_byte(2)];

        let proof = assemble_log_proof(log_proof.clone(), None).unwrap();

        let mut expected_metadata = [0_u8; 32];
        expected_metadata[0] = LOG_PROOF_SUPPORTED_METADATA_VERSION;
        expected_metadata[1] = log_proof.len() as u8;
        expected_metadata[3] = 1;
        assert_eq!(proof, [vec![expected_metadata.into()], log_proof].concat());
    }

    #[test]
    fn assembles_message_root_extension_metadata() {
        let log_proof = vec![B256::repeat_byte(1)];
        let extension_words = vec![B256::repeat_byte(2), B256::repeat_byte(3)];
        let extension = MessageRootProofExtension {
            batch_proof_len: 4,
            words: extension_words.clone(),
        };

        let proof = assemble_log_proof(log_proof.clone(), Some(extension)).unwrap();

        let mut expected_metadata = [0_u8; 32];
        expected_metadata[0] = LOG_PROOF_SUPPORTED_METADATA_VERSION;
        expected_metadata[1] = log_proof.len() as u8;
        expected_metadata[2] = 4;
        assert_eq!(
            proof,
            [vec![expected_metadata.into()], log_proof, extension_words].concat()
        );
    }

    // SYSCOIN: Canonical Gateway logs decode through the pinned Era event ABI before reconstruction.
    #[test]
    fn gateway_batch_root_log_uses_canonical_event_decoder() {
        let root = B256::repeat_byte(0x42);
        let (events, target_index) = decode_test_batch_root_logs(
            vec![batch_root_log(L2_MESSAGE_ROOT_ADDRESS, 57, 99, root)],
            L2_MESSAGE_ROOT_ADDRESS,
            57,
            &(99..=101),
            "Gateway",
        )
        .unwrap();

        assert_eq!(target_index, 0);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].batch_number, 99);
        assert_eq!(events[0].chain_batch_root, root);
        assert_eq!(events[0].block_number, 100);
    }

    // SYSCOIN: The requested batch event must expose the exact locally reconstructed root.
    #[test]
    fn batch_root_log_binds_local_target_root_and_rejects_zero() {
        let provider_root = B256::repeat_byte(0x42);
        let logs = vec![batch_root_log(
            L2_MESSAGE_ROOT_ADDRESS,
            57,
            99,
            provider_root,
        )];
        assert!(
            decode_test_batch_root_logs_for_root(
                logs,
                L2_MESSAGE_ROOT_ADDRESS,
                57,
                &(99..=101),
                B256::repeat_byte(0x43),
                "Gateway",
            )
            .is_err()
        );

        let zero_log = batch_root_log(L2_MESSAGE_ROOT_ADDRESS, 57, 99, B256::ZERO);
        assert!(
            decode_test_batch_root_logs_for_root(
                vec![zero_log],
                L2_MESSAGE_ROOT_ADDRESS,
                57,
                &(99..=101),
                provider_root,
                "Gateway",
            )
            .is_err()
        );
    }

    // SYSCOIN: V32 advances `currentChainBatchNumber` by exactly one for every append; a gap means
    // a provider omitted an event even if the post tree were also forged.
    #[test]
    fn batch_root_logs_must_be_consecutive() {
        let first = batch_root_log(L2_MESSAGE_ROOT_ADDRESS, 57, 99, B256::repeat_byte(0x42));
        let mut after_gap =
            batch_root_log(L2_MESSAGE_ROOT_ADDRESS, 57, 101, B256::repeat_byte(0x44));
        after_gap.log_index = Some(1);
        assert!(
            decode_test_batch_root_logs(
                vec![first, after_gap],
                L2_MESSAGE_ROOT_ADDRESS,
                57,
                &(99..=101),
                "Gateway",
            )
            .is_err()
        );
    }

    // SYSCOIN: Event processing and provider-controlled missing-target diagnostics have separate
    // deterministic ceilings.
    #[test]
    fn batch_root_event_and_diagnostic_caps_are_enforced() {
        assert!(validate_batch_root_log_count(MAX_BATCH_ROOT_EVENTS_PER_PROOF).is_ok());
        assert!(validate_batch_root_log_count(MAX_BATCH_ROOT_EVENTS_PER_PROOF + 1).is_err());

        let events = (0..MAX_DIAGNOSTIC_BATCH_EVENTS + 7)
            .map(|index| DecodedBatchRoot {
                batch_number: index as u64,
                chain_batch_root: B256::repeat_byte(0x42),
                block_number: 100,
            })
            .collect::<Vec<_>>();
        let diagnostic = observed_batch_root_diagnostic(&events);
        assert!(diagnostic.contains("7 additional event(s) omitted"));
        assert!(!diagnostic.contains(&format!("{}@100", events.len() - 1)));
    }

    // SYSCOIN: Truncated indexed topics from an RPC response fail without direct indexing or panic.
    #[test]
    fn gateway_batch_root_log_rejects_short_topics_without_panicking() {
        let malformed = Log {
            inner: alloy::primitives::Log {
                address: L2_MESSAGE_ROOT_ADDRESS,
                data: LogData::new_unchecked(
                    vec![AppendedChainBatchRoot::SIGNATURE_HASH, B256::ZERO],
                    Bytes::from(vec![0_u8; 32]),
                ),
            },
            block_hash: Some(test_block_hash(100)),
            block_number: Some(100),
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: Some(0),
            removed: false,
        };

        assert!(
            decode_test_batch_root_logs(
                vec![malformed],
                L2_MESSAGE_ROOT_ADDRESS,
                57,
                &(99..=101),
                "Gateway"
            )
            .is_err()
        );
    }

    // SYSCOIN: Malformed event data from an RPC response fails canonical ABI decoding without panic.
    #[test]
    fn gateway_batch_root_log_rejects_malformed_data_without_panicking() {
        let canonical = batch_root_log(L2_MESSAGE_ROOT_ADDRESS, 57, 99, B256::repeat_byte(0x42));
        let malformed = Log {
            inner: alloy::primitives::Log {
                address: L2_MESSAGE_ROOT_ADDRESS,
                data: LogData::new_unchecked(
                    canonical.inner.topics().to_vec(),
                    Bytes::from(vec![0_u8; 31]),
                ),
            },
            ..canonical
        };

        assert!(
            decode_test_batch_root_logs(
                vec![malformed],
                L2_MESSAGE_ROOT_ADDRESS,
                57,
                &(99..=101),
                "Gateway"
            )
            .is_err()
        );
    }

    // SYSCOIN: Re-check provider-returned address and chain identity independently of the filter.
    #[test]
    fn gateway_batch_root_log_rejects_wrong_filter_identity() {
        let mut wrong_chain =
            batch_root_log(L2_MESSAGE_ROOT_ADDRESS, 58, 99, B256::repeat_byte(0x42));
        assert!(
            decode_test_batch_root_logs(
                vec![wrong_chain.clone()],
                L2_MESSAGE_ROOT_ADDRESS,
                57,
                &(99..=101),
                "Gateway"
            )
            .is_err()
        );

        wrong_chain.inner.address = Address::repeat_byte(0x11);
        assert!(
            decode_test_batch_root_logs(
                vec![wrong_chain],
                L2_MESSAGE_ROOT_ADDRESS,
                58,
                &(99..=101),
                "Gateway"
            )
            .is_err()
        );
    }

    // SYSCOIN: Removed, incomplete, out-of-range, or noncanonical log sequences fail closed.
    #[test]
    fn gateway_batch_root_logs_require_canonical_metadata_and_order() {
        let mut removed = batch_root_log(L2_MESSAGE_ROOT_ADDRESS, 57, 99, B256::repeat_byte(0x11));
        removed.removed = true;
        assert!(
            decode_test_batch_root_logs(
                vec![removed],
                L2_MESSAGE_ROOT_ADDRESS,
                57,
                &(99..=101),
                "Gateway"
            )
            .is_err()
        );

        let mut missing_block =
            batch_root_log(L2_MESSAGE_ROOT_ADDRESS, 57, 99, B256::repeat_byte(0x11));
        missing_block.block_number = None;
        assert!(
            decode_test_batch_root_logs(
                vec![missing_block],
                L2_MESSAGE_ROOT_ADDRESS,
                57,
                &(99..=101),
                "Gateway"
            )
            .is_err()
        );

        let mut missing_hash =
            batch_root_log(L2_MESSAGE_ROOT_ADDRESS, 57, 99, B256::repeat_byte(0x11));
        missing_hash.block_hash = None;
        assert!(
            decode_test_batch_root_logs(
                vec![missing_hash],
                L2_MESSAGE_ROOT_ADDRESS,
                57,
                &(99..=101),
                "Gateway"
            )
            .is_err()
        );

        let mut noncanonical_hash =
            batch_root_log(L2_MESSAGE_ROOT_ADDRESS, 57, 99, B256::repeat_byte(0x11));
        noncanonical_hash.block_hash = Some(B256::repeat_byte(0xaa));
        assert!(
            decode_test_batch_root_logs(
                vec![noncanonical_hash],
                L2_MESSAGE_ROOT_ADDRESS,
                57,
                &(99..=101),
                "Gateway"
            )
            .is_err()
        );

        let mut out_of_range =
            batch_root_log(L2_MESSAGE_ROOT_ADDRESS, 57, 99, B256::repeat_byte(0x11));
        out_of_range.block_number = Some(102);
        assert!(
            decode_test_batch_root_logs(
                vec![out_of_range],
                L2_MESSAGE_ROOT_ADDRESS,
                57,
                &(99..=101),
                "Gateway"
            )
            .is_err()
        );

        let mut missing_log_index =
            batch_root_log(L2_MESSAGE_ROOT_ADDRESS, 57, 99, B256::repeat_byte(0x11));
        missing_log_index.log_index = None;
        assert!(
            decode_test_batch_root_logs(
                vec![missing_log_index],
                L2_MESSAGE_ROOT_ADDRESS,
                57,
                &(99..=101),
                "Gateway"
            )
            .is_err()
        );

        let mut later = batch_root_log(L2_MESSAGE_ROOT_ADDRESS, 57, 100, B256::repeat_byte(0x22));
        later.log_index = Some(1);
        let earlier = batch_root_log(L2_MESSAGE_ROOT_ADDRESS, 57, 99, B256::repeat_byte(0x11));
        assert!(
            decode_test_batch_root_logs(
                vec![later, earlier],
                L2_MESSAGE_ROOT_ADDRESS,
                57,
                &(99..=101),
                "Gateway"
            )
            .is_err()
        );

        let first = batch_root_log(L2_MESSAGE_ROOT_ADDRESS, 57, 100, B256::repeat_byte(0x11));
        let mut second = batch_root_log(L2_MESSAGE_ROOT_ADDRESS, 57, 99, B256::repeat_byte(0x22));
        second.log_index = Some(1);
        assert!(
            decode_test_batch_root_logs(
                vec![first, second],
                L2_MESSAGE_ROOT_ADDRESS,
                57,
                &(99..=101),
                "Gateway"
            )
            .is_err()
        );
    }

    // SYSCOIN: Batch numbers are encoded as U256 in the event but the pinned proof layout and
    // reconstruction indices are u64; reject non-representable or non-unique targets explicitly.
    #[test]
    fn batch_root_logs_require_u64_batches_and_exact_target() {
        let oversized_batch = batch_root_log_with_number(
            L2_MESSAGE_ROOT_ADDRESS,
            57,
            U256::from(u64::MAX) + U256::ONE,
            B256::repeat_byte(0x11),
        );
        assert!(
            decode_test_batch_root_logs(
                vec![oversized_batch],
                L2_MESSAGE_ROOT_ADDRESS,
                57,
                &(99..=101),
                "Gateway"
            )
            .is_err()
        );

        let missing_target =
            batch_root_log(L2_MESSAGE_ROOT_ADDRESS, 57, 100, B256::repeat_byte(0x22));
        assert!(
            decode_test_batch_root_logs(
                vec![missing_target],
                L2_MESSAGE_ROOT_ADDRESS,
                57,
                &(99..=101),
                "Gateway"
            )
            .is_err()
        );

        let first = batch_root_log(L2_MESSAGE_ROOT_ADDRESS, 57, 99, B256::repeat_byte(0x33));
        let mut duplicate = first.clone();
        duplicate.log_index = Some(1);
        assert!(
            decode_test_batch_root_logs(
                vec![first, duplicate],
                L2_MESSAGE_ROOT_ADDRESS,
                57,
                &(99..=101),
                "Gateway"
            )
            .is_err()
        );
    }

    // SYSCOIN: A stable end anchor authenticates every parent in the range; malformed ancestry is
    // rejected before hash-pinned contract reads or multi-block log reconstruction begins.
    #[test]
    fn canonical_block_range_requires_contiguous_ancestry() {
        assert!(CanonicalBlockRange::new(Vec::new()).is_err());

        let canonical = vec![
            CanonicalBlockAnchor {
                number: 99,
                hash: test_block_hash(99),
                parent_hash: test_block_hash(98),
            },
            CanonicalBlockAnchor {
                number: 100,
                hash: test_block_hash(100),
                parent_hash: test_block_hash(99),
            },
        ];
        assert!(CanonicalBlockRange::new(canonical.clone()).is_ok());

        let mut wrong_parent = canonical.clone();
        wrong_parent[1].parent_hash = B256::repeat_byte(0xaa);
        assert!(CanonicalBlockRange::new(wrong_parent).is_err());

        let mut skipped_number = canonical;
        skipped_number[1].number = 101;
        assert!(CanonicalBlockRange::new(skipped_number).is_err());
    }

    // SYSCOIN: The decoder must enforce its caller-supplied MessageRoot identity, metadata, range,
    // and ordering boundary rather than trusting `eth_getLogs` filter compliance.
    #[test]
    fn batch_root_logs_enforce_supplied_contract_identity() {
        let expected_message_root = Address::repeat_byte(0x77);
        let canonical = batch_root_log(expected_message_root, 57, 99, B256::repeat_byte(0x42));
        let decode = |logs| {
            decode_test_batch_root_logs(logs, expected_message_root, 57, &(100..=100), "test")
        };
        assert_eq!(decode(vec![canonical.clone()]).unwrap().0.len(), 1);

        let mut wrong_address = canonical.clone();
        wrong_address.inner.address = L2_MESSAGE_ROOT_ADDRESS;
        assert!(decode(vec![wrong_address]).is_err());

        let wrong_chain = batch_root_log(expected_message_root, 58, 99, B256::repeat_byte(0x42));
        assert!(decode(vec![wrong_chain]).is_err());

        let mut removed = canonical.clone();
        removed.removed = true;
        assert!(decode(vec![removed]).is_err());

        let mut wrong_block = canonical.clone();
        wrong_block.block_number = Some(101);
        assert!(decode(vec![wrong_block]).is_err());

        let mut missing_block = canonical.clone();
        missing_block.block_number = None;
        assert!(decode(vec![missing_block]).is_err());

        let mut missing_log_index = canonical.clone();
        missing_log_index.log_index = None;
        assert!(decode(vec![missing_log_index]).is_err());

        let mut later = batch_root_log(expected_message_root, 57, 100, B256::repeat_byte(0x22));
        later.log_index = Some(1);
        assert!(decode(vec![later, canonical]).is_err());
    }

    // SYSCOIN: Unstable Gateway metadata may not select an empty, unrelated, or unbounded block
    // range; the accepted maximum remains deterministic at both boundaries.
    #[test]
    fn gateway_batch_metadata_range_is_nonempty_containing_and_bounded() {
        assert!(validate_gateway_block_range(&(1..=1), 1).is_ok());
        assert!(validate_gateway_block_range(&(1..=MAX_GATEWAY_PROOF_BLOCK_SPAN), 2_048).is_ok());
        assert!(validate_gateway_block_range(&(0..=0), 0).is_err());
        let inverted = ops::RangeInclusive::new(5, 4);
        assert!(validate_gateway_block_range(&inverted, 5).is_err());
        assert!(validate_gateway_block_range(&(10..=20), 9).is_err());
        assert!(validate_gateway_block_range(&(10..=20), 21).is_err());
        assert!(validate_gateway_block_range(&(1..=MAX_GATEWAY_PROOF_BLOCK_SPAN + 1), 1).is_err());
    }

    // SYSCOIN: Gateway local-root extension uses the complete U256 mask and bounded path width.
    #[test]
    fn gateway_local_root_uses_full_u256_mask_and_checks_path_width() {
        let mut proof = ChainAggProof {
            chain_id_leaf_proof: vec![B256::ZERO; 64],
            chain_id_leaf_proof_mask: U256::ZERO,
            pinned_aggregate_root: B256::ZERO,
        };
        append_gateway_local_root(&mut proof, B256::repeat_byte(0x42)).unwrap();
        assert_eq!(proof.chain_id_leaf_proof.len(), 65);
        assert_eq!(proof.chain_id_leaf_proof_mask, U256::ONE << 64);

        let mut oversized = ChainAggProof {
            chain_id_leaf_proof: vec![B256::ZERO; 255],
            chain_id_leaf_proof_mask: U256::ZERO,
            pinned_aggregate_root: B256::ZERO,
        };
        assert!(append_gateway_local_root(&mut oversized, B256::ZERO).is_err());
        assert_eq!(oversized.chain_id_leaf_proof.len(), 255);
        assert_eq!(oversized.chain_id_leaf_proof_mask, U256::ZERO);
    }

    // SYSCOIN: Pinned one-byte proof metadata lengths must never truncate oversized paths.
    #[test]
    fn proof_metadata_rejects_unrepresentable_path_lengths() {
        assert!(proof_metadata(256, 0, true).is_err());
        assert!(assemble_log_proof(vec![B256::ZERO; 256], None).is_err());
    }

    // SYSCOIN: Byzantine compact-tree heights/indices cannot overflow native reconstruction math.
    #[test]
    fn compact_tree_rejects_indices_that_can_overflow_u64_reconstruction() {
        let excessive_height = Bytes32PushTree {
            _nextLeafIndex: U256::ZERO,
            _zeros: canonical_zero_hashes(66),
            _sides: vec![B256::ZERO; 66],
        };
        assert!(checked_tree_leaf_index(&excessive_height, 1).is_err());

        let overflowing_append = Bytes32PushTree {
            _nextLeafIndex: U256::from(u64::MAX),
            _zeros: canonical_zero_hashes(65),
            _sides: vec![B256::ZERO; 65],
        };
        assert!(checked_tree_leaf_index(&overflowing_append, 1).is_err());
    }

    // SYSCOIN: Compact-tree zero hashes are consensus-domain inputs, not arbitrary provider state.
    #[test]
    fn compact_tree_requires_pinned_zero_base_and_recurrence() {
        let canonical = canonical_zero_hashes(4);
        assert!(validate_canonical_zero_hashes(&canonical).is_ok());

        let mut wrong_base = canonical.clone();
        wrong_base[0] = B256::repeat_byte(0x11);
        assert!(validate_canonical_zero_hashes(&wrong_base).is_err());

        let mut wrong_recurrence = canonical;
        wrong_recurrence[2] = B256::repeat_byte(0x22);
        assert!(validate_canonical_zero_hashes(&wrong_recurrence).is_err());
    }

    // SYSCOIN: Replay must authenticate the full compact state, not merely find the requested log;
    // omitted, injected, or reordered leaves all fail against the independent post-range snapshot.
    #[test]
    fn compact_tree_replay_proves_complete_canonical_append_set() {
        let mut pre_tree = Bytes32PushTree {
            _nextLeafIndex: U256::ZERO,
            _zeros: vec![CHAIN_TREE_EMPTY_ENTRY_HASH],
            _sides: vec![B256::ZERO],
        };
        push_to_tree(&mut pre_tree, B256::repeat_byte(0x01)).unwrap();

        let canonical = vec![B256::repeat_byte(0x02), B256::repeat_byte(0x03)];
        let mut post_tree = pre_tree.clone();
        for leaf in &canonical {
            push_to_tree(&mut post_tree, *leaf).unwrap();
        }
        replay_and_validate_compact_tree(&pre_tree, &post_tree, &canonical, "test").unwrap();

        assert!(
            replay_and_validate_compact_tree(&pre_tree, &post_tree, &canonical[..1], "omitted")
                .is_err()
        );
        let mut injected = canonical.clone();
        injected.push(B256::repeat_byte(0x04));
        assert!(
            replay_and_validate_compact_tree(&pre_tree, &post_tree, &injected, "injected").is_err()
        );
        let mut reordered = canonical;
        reordered.reverse();
        assert!(
            replay_and_validate_compact_tree(&pre_tree, &post_tree, &reordered, "reordered")
                .is_err()
        );
    }

    // SYSCOIN: Checked reconstruction remains fallible at the native index ceiling and never
    // mutates an invalid tree before reporting overflow.
    #[test]
    fn compact_tree_near_u64_max_is_checked_without_panics() {
        let mut near_max = Bytes32PushTree {
            _nextLeafIndex: U256::from(u64::MAX - 1),
            _zeros: canonical_zero_hashes(65),
            _sides: vec![B256::ZERO; 65],
        };
        let leaf = B256::repeat_byte(0x42);
        let proof = calculate_batch_tree_proof(near_max.clone(), vec![leaf], 0).unwrap();
        assert_eq!(proof.len(), 64);

        push_to_tree(&mut near_max, leaf).unwrap();
        assert_eq!(near_max._nextLeafIndex, U256::from(u64::MAX));
        let next_index = near_max._nextLeafIndex;
        let sides = near_max._sides.clone();
        assert!(push_to_tree(&mut near_max, B256::repeat_byte(0x43)).is_err());
        assert_eq!(near_max._nextLeafIndex, next_index);
        assert_eq!(near_max._sides, sides);
    }

    // SYSCOIN: Chain masks must fit both their sibling path and the low half of encoded proof data.
    #[test]
    fn chain_proof_rejects_masks_outside_path_or_encoding_field() {
        let outside_path = ChainAggProof {
            chain_id_leaf_proof: vec![B256::ZERO],
            chain_id_leaf_proof_mask: U256::from(2),
            pinned_aggregate_root: B256::ZERO,
        };
        assert!(chain_proof_vector(1, outside_path, 57).is_err());

        let overlaps_batch_field = ChainAggProof {
            chain_id_leaf_proof: vec![B256::ZERO; 129],
            chain_id_leaf_proof_mask: U256::ONE << 128,
            pinned_aggregate_root: B256::ZERO,
        };
        assert!(chain_proof_vector(1, overlaps_batch_field, 57).is_err());

        let unrepresentable_zero_mask = ChainAggProof {
            chain_id_leaf_proof: vec![B256::ZERO; 129],
            chain_id_leaf_proof_mask: U256::ZERO,
            pinned_aggregate_root: B256::ZERO,
        };
        assert!(chain_proof_vector(1, unrepresentable_zero_mask, 57).is_err());
    }

    // SYSCOIN: Exhaustively sample prefilled/new-leaf/target combinations and authenticate every
    // fallibly reconstructed sibling path against the independently replayed compact-tree root.
    #[test]
    fn test_calculate_batch_tree_proof() {
        let empty_tree = Bytes32PushTree {
            _nextLeafIndex: U256::ZERO,
            _zeros: vec![CHAIN_TREE_EMPTY_ENTRY_HASH],
            _sides: vec![B256::ZERO],
        };
        let mut hashes = Vec::new();
        for i in 0..20 {
            hashes.push(keccak256([i as u8; 32]));
        }

        for prefilled in 0..hashes.len() {
            let mut tree_with_prefilled = empty_tree.clone();
            for h in &hashes[0..prefilled] {
                push_to_tree(&mut tree_with_prefilled, *h).unwrap();
            }

            for new_len in 1..(hashes.len() - prefilled) {
                let new_hashes = hashes[prefilled..(prefilled + new_len)].to_vec();
                let mut tree = tree_with_prefilled.clone();
                for h in &new_hashes {
                    push_to_tree(&mut tree, *h).unwrap();
                }
                for i in 0..new_hashes.len() {
                    let proof = calculate_batch_tree_proof(
                        tree_with_prefilled.clone(),
                        new_hashes.clone(),
                        i,
                    )
                    .unwrap();

                    // The reconstructed siblings must reach the same root as replaying every new
                    // leaf into the compact tree.
                    let mut current_hash = new_hashes[i];
                    let mut current_index: u64 =
                        tree_with_prefilled._nextLeafIndex.to::<u64>() + i as u64;
                    for sibling_hash in proof.iter() {
                        let is_left = current_index.is_multiple_of(2);
                        current_hash = if is_left {
                            keccak256([current_hash.0, sibling_hash.0].concat())
                        } else {
                            keccak256([sibling_hash.0, current_hash.0].concat())
                        };
                        current_index /= 2;
                    }

                    assert_eq!(current_hash, tree._sides[tree._zeros.len() - 1]);
                }
            }
        }
    }
}
