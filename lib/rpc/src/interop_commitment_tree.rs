//! Reads the atomic-interop commitment tree from historical local L2 state.
//!
//! The authoritative leaf preimages live in the fixed-address `L2InteropCommitmentTree` system
//! contract; the server does not maintain a separate historical copy of the tree. This reader
//! reuses the node's local `eth_call` path to execute `leafCount()` and `leafAt(i)` against the
//! requested block's state. The contract exposes leaves but no Merkle paths, so [`crate::imt`]
//! reconstructs the internal nodes and paths from those leaves.

use crate::eth_call_handler::{EthCallError, EthCallHandler};
use crate::imt::{ImtLeaf, IndexedMerkleTree};
use crate::rpc_storage::ReadRpcStorage;
use alloy::eips::BlockId;
use alloy::primitives::{B256, Bytes, U256};
use alloy::rpc::types::TransactionRequest;
use alloy::sol_types::SolCall;
use std::num::NonZeroU64;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{Semaphore, TryAcquireError};
use zksync_os_contract_interface::IL2InteropCommitmentTree;
use zksync_os_types::L2_INTEROP_COMMITMENT_TREE_ADDRESS;

// SYSCOIN: Historical IMT reads replay all leaves through the local VM and rebuild the tree in
// memory. Keep exactly one reconstruction active per RPC server without occupying a Tokio worker.
const MAX_CONCURRENT_RECONSTRUCTIONS: usize = 1;

/// Failure while reconstructing the atomic-interop commitment tree.
#[derive(Debug, thiserror::Error)]
pub enum InteropCommitmentTreeError {
    #[error("commitment-tree `{method}` call failed")]
    Call {
        method: &'static str,
        #[source]
        source: EthCallError,
    },
    #[error("failed to decode commitment-tree `{method}` response")]
    Decode {
        method: &'static str,
        #[source]
        source: alloy::sol_types::Error,
    },
    #[error("commitment-tree reconstruction admission is closed")]
    ReconstructionAdmissionClosed,
    #[error("another commitment-tree reconstruction is already running")]
    ReconstructionBusy,
    #[error("commitment-tree reconstruction task failed")]
    ReconstructionTask(#[source] tokio::task::JoinError),
    #[error(
        "commitment tree has {leaf_count} leaves, exceeding the configured reconstruction limit of {max_leaves}"
    )]
    ReconstructionLimitExceeded { leaf_count: U256, max_leaves: u64 },
    #[error("commitment tree has no sentinel leaf")]
    MissingSentinel,
    #[error(
        "IMT inclusion proof failed self-verification for commit value {commit_value} at leaf \
         {leaf_index}: recomputed root {recomputed_root} != tree root {tree_root}"
    )]
    ProofMismatch {
        commit_value: U256,
        leaf_index: u64,
        recomputed_root: B256,
        tree_root: B256,
    },
}

/// Loads index-ordered commitment-tree leaves through this node's local `eth_call` implementation.
#[derive(Clone)]
pub(crate) struct InteropCommitmentTreeReader<RpcStorage> {
    eth_call_handler: EthCallHandler<RpcStorage>,
    // SYSCOIN: The permit moves into the blocking task, so cancelling an RPC cannot release the
    // gate while its non-cancellable reconstruction continues in the background.
    reconstruction_permits: Arc<Semaphore>,
    // SYSCOIN: Bound one admitted reconstruction before allocation and cumulative local VM work.
    max_reconstruction_leaves: u64,
    // SYSCOIN: Emit the operator capacity alarm once per process, not once per public request.
    reconstruction_limit_alarm_emitted: Arc<AtomicBool>,
}

impl<RpcStorage> InteropCommitmentTreeReader<RpcStorage> {
    pub(crate) fn new(
        eth_call_handler: EthCallHandler<RpcStorage>,
        max_reconstruction_leaves: NonZeroU64,
    ) -> Self {
        Self {
            eth_call_handler,
            reconstruction_permits: Arc::new(Semaphore::new(MAX_CONCURRENT_RECONSTRUCTIONS)),
            max_reconstruction_leaves: max_reconstruction_leaves.get(),
            reconstruction_limit_alarm_emitted: Arc::new(AtomicBool::new(false)),
        }
    }
}

// SYSCOIN: `spawn_blocking` work cannot be stopped once it starts. Acquiring first and moving the
// owned permit into that task keeps admission correct across request cancellation and disconnects.
async fn run_bounded_reconstruction<T, F>(
    permits: Arc<Semaphore>,
    work: F,
) -> Result<T, InteropCommitmentTreeError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, InteropCommitmentTreeError> + Send + 'static,
{
    let permit = permits.try_acquire_owned().map_err(|err| match err {
        TryAcquireError::NoPermits => InteropCommitmentTreeError::ReconstructionBusy,
        TryAcquireError::Closed => InteropCommitmentTreeError::ReconstructionAdmissionClosed,
    })?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(InteropCommitmentTreeError::ReconstructionTask)?
}

// SYSCOIN: Validate the full-width contract value before narrowing it or reserving memory.
fn checked_reconstruction_leaf_count(
    leaf_count: U256,
    max_leaves: u64,
) -> Result<u64, InteropCommitmentTreeError> {
    if leaf_count == U256::ZERO {
        return Err(InteropCommitmentTreeError::MissingSentinel);
    }
    if leaf_count > U256::from(max_leaves) {
        return Err(InteropCommitmentTreeError::ReconstructionLimitExceeded {
            leaf_count,
            max_leaves,
        });
    }
    Ok(leaf_count.to::<u64>())
}

impl<RpcStorage: ReadRpcStorage> InteropCommitmentTreeReader<RpcStorage> {
    fn call(
        &self,
        method: &'static str,
        calldata: Bytes,
        block: BlockId,
    ) -> Result<Bytes, InteropCommitmentTreeError> {
        let request = TransactionRequest::default()
            .to(L2_INTEROP_COMMITMENT_TREE_ADDRESS)
            .input(calldata.into());
        self.eth_call_handler
            .call_impl(request, Some(block), None, None)
            .map_err(|source| InteropCommitmentTreeError::Call { method, source })
    }

    /// Rebuilds the tree and consumes it inside the same bounded blocking task.
    ///
    /// Keeping `use_tree` inside the task means lookup, path construction, and self-verification
    /// remain covered by the same permit as the cumulative historical-state replay.
    pub(crate) async fn with_tree<T, F>(
        &self,
        block: BlockId,
        use_tree: F,
    ) -> Result<T, InteropCommitmentTreeError>
    where
        T: Send + 'static,
        F: FnOnce(IndexedMerkleTree) -> Result<T, InteropCommitmentTreeError> + Send + 'static,
    {
        let reader = self.clone();
        run_bounded_reconstruction(self.reconstruction_permits.clone(), move || {
            let tree = reader.read_sync(block)?;
            use_tree(tree)
        })
        .await
    }

    /// Rebuilds the tree because the contract exposes historical leaves, but not Merkle paths.
    fn read_sync(&self, block: BlockId) -> Result<IndexedMerkleTree, InteropCommitmentTreeError> {
        let count_bytes = self.call(
            "leafCount",
            IL2InteropCommitmentTree::leafCountCall {}
                .abi_encode()
                .into(),
            block,
        )?;
        let leaf_count = IL2InteropCommitmentTree::leafCountCall::abi_decode_returns(&count_bytes)
            .map_err(|source| InteropCommitmentTreeError::Decode {
                method: "leafCount",
                source,
            })?;
        let leaf_count = checked_reconstruction_leaf_count(
            leaf_count,
            self.max_reconstruction_leaves,
        )
        .inspect_err(|err| {
            // SYSCOIN: This is an operator-actionable capacity alarm. Raise the configured limit
            // only after benchmarking the full replay, or replace it with an indexed proof source.
            if matches!(
                err,
                InteropCommitmentTreeError::ReconstructionLimitExceeded { .. }
            ) && !self
                .reconstruction_limit_alarm_emitted
                .swap(true, Ordering::Relaxed)
            {
                tracing::error!(
                    max_leaves = self.max_reconstruction_leaves,
                    "historical IMT reconstruction reached its configured work limit; indexed proof storage or an operator-reviewed capacity increase is required"
                );
            }
        })?;

        let mut leaves = Vec::with_capacity(leaf_count as usize);
        for index in 0..leaf_count {
            let leaf_bytes = self.call(
                "leafAt",
                IL2InteropCommitmentTree::leafAtCall {
                    index: U256::from(index),
                }
                .abi_encode()
                .into(),
                block,
            )?;
            let leaf = IL2InteropCommitmentTree::leafAtCall::abi_decode_returns(&leaf_bytes)
                .map_err(|source| InteropCommitmentTreeError::Decode {
                    method: "leafAt",
                    source,
                })?;
            leaves.push(ImtLeaf {
                value: leaf.value,
                next_index: leaf.nextIndex,
                next_value: leaf.nextValue,
            });
        }

        Ok(IndexedMerkleTree::new(leaves))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        InteropCommitmentTreeError, checked_reconstruction_leaf_count, run_bounded_reconstruction,
    };
    use alloy::primitives::U256;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    use tokio::sync::{Semaphore, oneshot};

    // SYSCOIN: This reproduces the cancellation shape that defeats an outer RPC-middleware gate:
    // once blocking work has started, aborting its request must not admit a second reconstruction.
    #[tokio::test]
    async fn cancelled_request_keeps_permit_with_running_reconstruction() {
        let permits = Arc::new(Semaphore::new(1));
        let (first_started_tx, first_started_rx) = oneshot::channel();
        let (release_first_tx, release_first_rx) = mpsc::channel();
        let first_permits = permits.clone();
        let first = tokio::spawn(async move {
            run_bounded_reconstruction(first_permits, move || {
                first_started_tx.send(()).unwrap();
                release_first_rx.recv().unwrap();
                Ok(())
            })
            .await
        });

        first_started_rx.await.unwrap();
        first.abort();
        let _ = first.await;
        assert_eq!(permits.available_permits(), 0);

        let second_started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let second_started_in_work = second_started.clone();
        let second_permits = permits.clone();
        let err = run_bounded_reconstruction(second_permits, move || {
            second_started_in_work.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(7_u8)
        })
        .await
        .unwrap_err();
        assert!(
            matches!(err, super::InteropCommitmentTreeError::ReconstructionBusy),
            "a concurrent reconstruction was not rejected: {err}"
        );
        assert!(!second_started.load(std::sync::atomic::Ordering::SeqCst));

        release_first_tx.send(()).unwrap();
        let returned_permit = tokio::time::timeout(Duration::from_secs(1), permits.acquire())
            .await
            .expect("the cancelled reconstruction did not return its permit after completing")
            .unwrap();
        drop(returned_permit);

        let result = run_bounded_reconstruction(permits.clone(), || Ok(7_u8))
            .await
            .unwrap();
        assert_eq!(result, 7);
        assert_eq!(permits.available_permits(), 1);
    }

    // SYSCOIN: Ordinary completion returns both the work result and the sole admission permit.
    #[tokio::test]
    async fn completed_reconstruction_returns_result_and_permit() {
        let permits = Arc::new(Semaphore::new(1));
        let result = run_bounded_reconstruction(permits.clone(), || Ok(11_u8))
            .await
            .unwrap();
        assert_eq!(result, 11);
        assert_eq!(permits.available_permits(), 1);
    }

    // SYSCOIN: The work ceiling is inclusive and full-width, so a huge contract value cannot
    // truncate into a small allocation/loop bound. A zero count still reports missing genesis.
    #[test]
    fn leaf_count_is_checked_before_narrowing_or_allocation() {
        assert_eq!(
            checked_reconstruction_leaf_count(U256::from(8), 8).unwrap(),
            8
        );
        assert!(matches!(
            checked_reconstruction_leaf_count(U256::from(9), 8),
            Err(InteropCommitmentTreeError::ReconstructionLimitExceeded { .. })
        ));
        assert!(matches!(
            checked_reconstruction_leaf_count(U256::MAX, u64::MAX),
            Err(InteropCommitmentTreeError::ReconstructionLimitExceeded { .. })
        ));
        assert!(matches!(
            checked_reconstruction_leaf_count(U256::ZERO, 8),
            Err(InteropCommitmentTreeError::MissingSentinel)
        ));
    }
}
