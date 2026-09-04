use alloy::primitives::B256;
use blake2::{Blake2s256, Digest};

/// SYSCOIN: Shared V32 state-commitment encoding for RPC openings and native batch validation.
pub fn state_commitment_hash(
    tree_root_hash: B256,
    next_free_slot: u64,
    block_number: u64,
    last_256_block_hashes_blake: B256,
    last_block_timestamp: u64,
) -> B256 {
    let mut hasher = Blake2s256::new();
    hasher.update(tree_root_hash.as_slice());
    hasher.update(next_free_slot.to_be_bytes());
    hasher.update(block_number.to_be_bytes());
    hasher.update(last_256_block_hashes_blake);
    hasher.update(last_block_timestamp.to_be_bytes());
    B256::from_slice(&hasher.finalize())
}
