use std::collections::HashSet;
use zk_ee::{common_structs::MAX_NUMBER_OF_LOGS, system::MAX_NATIVE_COMPUTATIONAL};
use zksync_os_interface::types::BlockOutput;
use zksync_os_l1_sender::batcher_metrics::BATCHER_METRICS;
use zksync_os_storage_api::ReplayRecord;
use zksync_os_types::{ProtocolSemanticVersion, SystemTxType, ZkTxType};

/// SYSCOIN Reserved headroom (in bytes) between the batch's accumulated raw pubdata and
/// the configured `batch_pubdata_limit_bytes`, used to guarantee that the
/// commit transaction carrying the batch still fits within a settlement-layer
/// block configured with the same pubdata limit.
///
/// When a `RelayedL2Calldata` edge commits to a gateway, the gateway's
/// `RelayedSLDAValidator` re-emits the full edge pubdata as an L2→L1 message
/// via `L2_TO_L1_MESSENGER_SYSTEM_CONTRACT.sendToL1(abi.encode(version, chainId,
/// batchNumber, pubdata))`. That message payload is then physically written
/// into the gateway block's own pubdata buffer by `LogsStorage::apply_pubdata`
/// (see `zk_ee::common_structs::logs_storage`). The gateway block pubdata
/// therefore equals the edge pubdata plus a per-commit fixed expansion, which
/// we upper-bound below.
///
/// Breakdown of the gateway-side expansion (conservative upper bound):
///   * Gateway block header in pubdata: 1B version + 32B block_hash + 8B
///     timestamp = 41B.
///   * Gateway state diffs emitted by executing `commitBatches` on the
///     diamond proxy (batch hash tracking, last-committed pointers, message
///     root bookkeeping): ≲ 400B.
///   * First-pass L2→L1 logs section: 4B count prefix + up to ~6 service logs
///     (RelayedSLDAValidator user message + Executor.sol service logs) ×
///     88B each = ≲ 532B.
///   * Second-pass messages section: 4B count + per-user-message header
///     (4B length prefix + 160B `abi.encode` header for the
///     `(uint8,uint256,uint256,bytes)` tuple + up to 31B tail padding of the
///     embedded `pubdata` bytes) + room for auxiliary service payloads:
///     ≲ 800B.
///   * Safety margin for future Executor/DAValidator additions.
///
/// 2048B comfortably covers this envelope while costing only ~0.2% of the
/// typical ~1 MB block pubdata budget.
///
/// The check that uses this constant is only applied once the batch already
/// contains at least one block: the batcher's invariant is that the first
/// block of a batch must always be includable, otherwise a single block near
/// the per-block pubdata cap would cause an infinite peek-reject loop. For
/// single-block batches, inclusion still depends on the settlement layer
/// accepting the commit tx, which requires the settlement layer's
/// `block_pubdata_limit_bytes` to exceed the edge's by at least this margin.
const COMMIT_TX_PUBDATA_OVERHEAD: u64 = 2048;

#[derive(Default, Clone)]
pub(crate) struct BatchInfoAccumulator {
    // Accumulated values
    pub native_cycles: u64,
    pub pubdata_bytes: u64,
    pub l2_to_l1_logs_count: u64,
    pub block_count: u64,
    pub tx_count: u64,
    pub has_upgrade_tx: bool,
    pub interop_roots_count: u64,
    pub should_seal_for_gateway_migration: bool,

    pub protocol_versions: HashSet<ProtocolSemanticVersion>,
    pub execution_versions: HashSet<u32>,

    // Limits
    pub tx_per_batch_limit: u64,
    pub batch_pubdata_limit_bytes: u64,
    pub interop_roots_per_batch_limit: u64,
}

impl BatchInfoAccumulator {
    pub fn new(
        tx_per_batch_limit: u64,
        batch_pubdata_limit_bytes: u64,
        interop_roots_per_batch_limit: u64,
    ) -> Self {
        Self {
            tx_per_batch_limit,
            batch_pubdata_limit_bytes,
            interop_roots_per_batch_limit,
            ..Default::default()
        }
    }

    pub fn add(&mut self, block_output: &BlockOutput, replay_record: &ReplayRecord) -> &Self {
        self.native_cycles += block_output.computational_native_used;
        self.pubdata_bytes += block_output.pubdata.len() as u64;
        self.l2_to_l1_logs_count += block_output
            .tx_results
            .iter()
            .map(|tx_result| tx_result.as_ref().map_or(0, |tx| tx.l2_to_l1_logs.len()))
            .sum::<usize>() as u64;
        self.block_count += 1;
        self.tx_count += replay_record.transactions.len() as u64;
        self.execution_versions
            .insert(replay_record.block_context.execution_version);
        self.protocol_versions
            .insert(replay_record.protocol_version.clone());
        self.interop_roots_count += replay_record
            .transactions
            .iter()
            .map(|tx| {
                if let Some(SystemTxType::ImportInteropRoots(roots_count)) = tx.as_system_tx_type()
                {
                    *roots_count
                } else {
                    0
                }
            })
            .sum::<u64>();

        // If there is a chain id update transaction not in the first block(note `self.block_count > 1`), we need to seal the batch for gateway migration(so it goes in the first block of the next batch)
        if replay_record
            .transactions
            .iter()
            .any(|tx| matches!(tx.as_system_tx_type(), Some(SystemTxType::SetSLChainId(_))))
            && self.block_count > 1
        {
            self.should_seal_for_gateway_migration = true;
        }

        if !self.has_upgrade_tx
            && replay_record
                .transactions
                .iter()
                .any(|tx| tx.tx_type() == ZkTxType::Upgrade)
        {
            // Sanity check: upgrade tx must be either the only tx in the block,
            // or followed by exactly one SetSLChainId tx (only for the v31 upgrade).
            assert!(
                replay_record.transactions.len() == 1
                    || (replay_record.transactions.len() == 2
                        && replay_record.protocol_version.minor == 31
                        && matches!(
                            replay_record.transactions[1].as_system_tx_type(),
                            Some(SystemTxType::SetSLChainId(u64::MAX))
                        )),
                "upgrade tx must be the only tx in the block (or followed by a single SetSLChainId tx for v31): {replay_record:?}"
            );
            self.has_upgrade_tx = true;
        }

        self
    }

    /// Checks if the batch should be sealed based on the content of the blocks.
    /// e.g. due to the block count limit, tx count limit, or pubdata size limit.
    pub fn should_seal(&self) -> bool {
        // With current implementation, sealer assumes that the first block in the batch
        // can always be included, so we shouldn't return `true` until we add one more block here.
        // Otherwise, we will end up in a situation where the first block is never included in any batch.
        if self.has_upgrade_tx && self.block_count > 1 {
            BATCHER_METRICS.seal_reason[&"upgrade_tx"].inc();
            tracing::debug!("Batcher: sealing batch due to upgrade transaction");
            return true;
        }

        // If patch upgrade was executed, then we will not have an upgrade tx, but we still need to seal the previous
        // batch to make sure that all the blocks within a batch have the same protocol version.
        if self.protocol_versions.len() > 1 {
            BATCHER_METRICS.seal_reason[&"protocol_version_change"].inc();
            tracing::debug!("Batcher: protocol version changed within the batch");
            return true;
        }

        if self.tx_count > self.tx_per_batch_limit {
            BATCHER_METRICS.seal_reason[&"tx_per_batch"].inc();
            tracing::debug!("Batcher: reached tx per batch limit");
            return true;
        }

        if self.native_cycles > MAX_NATIVE_COMPUTATIONAL {
            BATCHER_METRICS.seal_reason[&"native_cycles"].inc();
            tracing::debug!("Batcher: reached native cycles limit for the batch");
            return true;
        }

        if self.pubdata_bytes > self.batch_pubdata_limit_bytes {
            BATCHER_METRICS.seal_reason[&"pubdata"].inc();
            tracing::debug!("Batcher: reached pubdata bytes limit for the batch");
            return true;
        }

        // SYSCOIN Seal early to reserve space for the commit-transaction framing overhead so the
        // settlement layer (L1 or gateway) never rejects the commit tx as exceeding its
        // block pubdata limit when it is configured identically to this limit. Only
        // enforced once the batch already has more than one block so that a single block
        // near the per-block pubdata cap is still guaranteed to fit in some batch.
        if self.block_count > 1
            && self
                .pubdata_bytes
                .saturating_add(COMMIT_TX_PUBDATA_OVERHEAD)
                > self.batch_pubdata_limit_bytes
        {
            BATCHER_METRICS.seal_reason[&"pubdata"].inc();
            tracing::debug!(
                "Batcher: sealing batch to reserve commit-tx pubdata overhead \
                 (pubdata_bytes={}, overhead={}, limit={})",
                self.pubdata_bytes,
                COMMIT_TX_PUBDATA_OVERHEAD,
                self.batch_pubdata_limit_bytes
            );
            return true;
        }

        if self.l2_to_l1_logs_count > MAX_NUMBER_OF_LOGS {
            BATCHER_METRICS.seal_reason[&"l2_l1_logs"].inc();
            tracing::debug!("Batcher: reached max number of L2 to L1 logs");
            return true;
        }

        if self.interop_roots_count > self.interop_roots_per_batch_limit {
            BATCHER_METRICS.seal_reason[&"interop_roots"].inc();
            tracing::debug!("Batcher: reached max number of interop roots per batch");
            return true;
        }

        // In case SL chain id update tx is present but not in the first block, we need to seal and
        // exclude. It will then go in the first block of the next batch.
        if self.should_seal_for_gateway_migration {
            BATCHER_METRICS.seal_reason[&"chain_id_update_tx"].inc();
            tracing::debug!(
                "Batcher: sealing batch due to chain id update transaction, which should go in the first block of the next batch"
            );
            return true;
        }

        // TODO: once upgrade functionality is implemented in the sequencer, this check will be equivalent
        // to the `protocol_versions` one above, so we can remove this logic.
        if self.execution_versions.len() > 1 {
            BATCHER_METRICS.seal_reason[&"execution_version_change"].inc();
            tracing::debug!("Batcher: ZKsync OS version changed within the batch");
            return true;
        }

        false
    }

    pub fn report_accumulated_resources_to_metrics(&self) {
        BATCHER_METRICS
            .computational_native_used_per_batch
            .observe(self.native_cycles);
        BATCHER_METRICS
            .pubdata_per_batch
            .observe(self.pubdata_bytes);
    }
}
