use crate::execution::fee_provider::{FeeParams, FeeProvider};
use crate::execution::metrics::EXECUTION_METRICS;
use crate::model::blocks::{
    BlockCommand, InvalidTxPolicy, PreparedBlockCommand, RebuildCommand, SealPolicy,
};
use alloy::primitives::{Address, B256, BlockHash, TxHash, U256};
use anyhow::Context as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::{sync::watch, time::Instant};
use zksync_os_contract_interface::settlement_layer_intervals::{
    IntervalSettlementLayer, SettlementLayerIntervals,
};
use zksync_os_genesis::genesis_header;
use zksync_os_mempool::subpools::l2::L2Subpool;
use zksync_os_mempool::{MarkingTxStream, Pool};
use zksync_os_storage_api::{BlockContext, BlockHashes, ReplayRecord};
use zksync_os_types::{
    BlockOutput, BlockStartCursors, ExecutionVersion, ProtocolSemanticVersion, SystemTxEnvelope,
    SystemTxType, UpgradeMetadata, ZkEnvelope, ZkTransaction,
};

/// Component that turns `BlockCommand`s into `PreparedBlockCommand`s.
/// Last step in the stream where `Produce` and `Replay` are differentiated.
///
///  * Tracks L1 priority ID and 256 previous block hashes.
///  * Combines the L1 and L2 transactions
///  * Cross-checks L1 transactions in Replay blocks against L1 (important for ENs) todo: not implemented yet
///
/// Note: unlike other components, this one doesn't tolerate replaying blocks -
///  it doesn't tolerate jumps in L1 priority IDs.
///  this is easily fixable if needed.
pub struct BlockContextProvider<Subpool> {
    fee_provider: FeeProvider,
    pool: Pool<Subpool>,
    config: Config,
    last_block: Option<LastBlock>,
    next_interop_tx_allowed_after: Instant,
    /// L2 chain id of the chain's currently-active settlement layer. Can change in runtime if there
    /// is a migration in the process.
    current_sl_chain_id: u64,
    last_constructed_block_ctx_sender: watch::Sender<Option<BlockContext>>,
}

pub struct Config {
    pub l2_chain_id: u64,
    pub l1_chain_id: u64,
    pub gas_limit: u64,
    pub pubdata_limit: u64,
    pub fee_collector_address: Address,
    pub block_time: Duration,
    pub service_block_delay: Duration,
    pub max_transactions_in_block: usize,
    pub interop_roots_per_block: u64,
}

struct LastBlock {
    record: ReplayRecord,
    hash: BlockHash,
    next_cursors: BlockStartCursors,
}

pub struct LastBlockSeed {
    pub record: ReplayRecord,
    pub hash: BlockHash,
    pub next_cursors: BlockStartCursors,
}

impl<Subpool: L2Subpool> BlockContextProvider<Subpool> {
    pub fn new(
        fee_provider: FeeProvider,
        pool: Pool<Subpool>,
        config: Config,
        intervals: &SettlementLayerIntervals,
        last_constructed_block_ctx_sender: watch::Sender<Option<BlockContext>>,
        last_block_seed: Option<LastBlockSeed>,
    ) -> Self {
        let current_sl_chain_id = match intervals.current_settlement_layer() {
            IntervalSettlementLayer::L1 => config.l1_chain_id,
            IntervalSettlementLayer::Gateway(gw_chain_id) => *gw_chain_id,
        };
        let last_block = last_block_seed.map(|seed| LastBlock {
            record: seed.record,
            hash: seed.hash,
            next_cursors: seed.next_cursors,
        });
        Self {
            fee_provider,
            pool,
            config,
            last_block,
            next_interop_tx_allowed_after: Instant::now(),
            current_sl_chain_id,
            last_constructed_block_ctx_sender,
        }
    }

    /// `true` when the chain currently settles on a Gateway (i.e. its tracked SL chain id
    /// differs from L1's).
    fn settles_on_gateway(&self) -> bool {
        self.current_sl_chain_id != self.config.l1_chain_id
    }

    pub fn last_block_number(&self) -> Option<u64> {
        self.last_block
            .as_ref()
            .map(|b| b.record.block_context.block_number)
    }

    pub async fn prepare_command(
        &mut self,
        block_command: BlockCommand,
    ) -> anyhow::Result<Option<PreparedBlockCommand<'_>>> {
        match block_command {
            BlockCommand::Produce(_) => self.produce().await,
            BlockCommand::Replay(record) => self.replay(record).await,
            BlockCommand::Rebuild(rebuild) => self.rebuild(rebuild).await,
        }
    }

    async fn produce(&mut self) -> anyhow::Result<Option<PreparedBlockCommand<'_>>> {
        let LastBlock {
            record: previous_record,
            hash: previous_block_hash,
            next_cursors,
        } = self
            .last_block
            .take()
            .expect("tried to produce a block without replaying at least one record");
        let block_number = previous_record.block_context.block_number + 1;
        let (fee_params, best_txs) = loop {
            let pending_fee_params = self.fee_provider.produce_fee_params().await?;
            self.pool.update_pending_block_fees(
                pending_fee_params.eip1559_basefee.saturating_to(),
                None,
            );

            // Create stream:
            // - If available, upgrade tx goes first (expected to be the only tx in the block, enforced by sequencer).
            // - L1 transactions first, then L2 transactions.
            let best_txs = self
                .pool
                .best_transactions_stream(
                    self.next_interop_tx_allowed_after,
                    self.settles_on_gateway(),
                )
                .await
                .context("mempool is closed")?;

            // SYSCOIN: `best_transactions_stream` can wait indefinitely while the mempool
            // is empty. If fee inputs changed during that wait, discard the selected stream
            // and reselect under fresh pending fees so tx selection and BlockContext use the
            // same fee snapshot.
            let fee_params = self.fee_provider.produce_fee_params().await?;
            if fee_params == pending_fee_params {
                break (fee_params, best_txs);
            }

            tracing::info!(
                ?pending_fee_params,
                ?fee_params,
                "fee params changed while waiting for transactions; reselecting tx stream"
            );
            drop(best_txs);
        };

        let timestamp = (millis_since_epoch() / 1000) as u64;

        // SYSCOIN: Check if we peeked upgrade metadata.
        // Patch-only upgrades with version <= the previous record's version can be safely
        // skipped, but equal-version full genesis upgrades must keep their forced preimages.
        let (protocol_version, force_preimages, canonical_upgrade_tx_hash) = if let Some(
            upgrade_metadata,
        ) =
            best_txs.upgrade_metadata
            && should_apply_upgrade_metadata(
                &upgrade_metadata,
                &previous_record.protocol_version,
                best_txs.stream_contains_upgrade_tx,
            ) {
            tracing::info!(
                block_number,
                ?upgrade_metadata,
                "including protocol upgrade transaction in the block"
            );
            // Invariant: transactions sent through this stream must be ready for execution, e.g.
            // transaction should not be sent until timestamp is reached.
            // We add some margin of error for timestamp comparison.
            let current_timestamp = timestamp.saturating_add(5);
            anyhow::ensure!(
                upgrade_metadata.timestamp <= current_timestamp,
                "upgrade transaction with timestamp {} received too early at {}; tx: {upgrade_metadata:?}",
                upgrade_metadata.timestamp,
                current_timestamp
            );
            (
                upgrade_metadata.protocol_version,
                upgrade_metadata.force_preimages.clone(),
                upgrade_metadata.canonical_tx_hash,
            )
        } else {
            (
                previous_record.protocol_version.clone(),
                Vec::new(),
                B256::ZERO,
            )
        };

        let execution_version: ExecutionVersion = (&protocol_version)
            .try_into()
            .context("Cannot instantiate a block for unsupported execution version")?;

        // Insert a SetSLChainId system transaction exactly once: when the protocol
        // version is v31 (either via upgrade from v30, or on the first block of a
        // fresh v31 chain). After it fires once, the condition can never trigger again.
        let (tx_source, expect_sl_chain_id_tx_after_upgrade) = if protocol_version.minor == 31
            && (previous_record.protocol_version.minor < 31
                || previous_record.block_context.block_number == 0)
        {
            let sl_chain_id_tx = SystemTxEnvelope::set_sl_chain_id(
                self.current_sl_chain_id,
                // We use `u64::MAX` as a placeholder, since it is not an actual migration
                u64::MAX,
            );
            // SYSCOIN: Keep upgrade blocks ordered as upgrade -> SetSLChainId, but
            // prepend for non-upgrade streams so live L2 traffic cannot starve the v31
            // SetSLChainId tx. Both helpers preserve the L2 marker for invalid tx
            // rejection when the stream is markable.
            let sl_chain_id_tx = ZkTransaction::from(sl_chain_id_tx);
            let tx_source = if best_txs.stream_contains_upgrade_tx {
                best_txs.stream.append_tx(sl_chain_id_tx)
            } else {
                best_txs.stream.prepend_tx(sl_chain_id_tx)
            };
            (tx_source, true)
        } else {
            (best_txs.stream, false)
        };

        let FeeParams {
            eip1559_basefee,
            native_price,
            pubdata_price,
        } = fee_params;
        let block_context = BlockContext {
            eip1559_basefee,
            native_price,
            pubdata_price,
            block_number,
            timestamp,
            chain_id: self.config.l2_chain_id,
            coinbase: self.config.fee_collector_address,
            block_hashes: previous_record
                .block_context
                .block_hashes
                .push(previous_block_hash),
            gas_limit: self.config.gas_limit,
            pubdata_limit: self.config.pubdata_limit,
            // todo: initialize as source of randomness, i.e. the value of prevRandao
            mix_hash: Default::default(),
            execution_version: execution_version as u32,
            blob_fee: U256::ONE,
        };
        self.last_constructed_block_ctx_sender
            .send_replace(Some(block_context));
        Ok(Some(PreparedBlockCommand {
            block_context,
            tx_source,
            seal_policy: SealPolicy::Decide(
                self.config.block_time,
                self.config.max_transactions_in_block,
            ),
            invalid_tx_policy: InvalidTxPolicy::RejectAndContinue {
                mark_in_source: true,
            },
            metrics_label: "produce",
            protocol_version,
            expected_block_output_hash: None,
            previous_block_timestamp: previous_record.block_context.timestamp,
            force_preimages,
            canonical_upgrade_tx_hash,
            expect_sl_chain_id_tx_after_upgrade,
            starting_cursors: next_cursors.clone(),
            interop_roots_per_block: self.config.interop_roots_per_block,
            strict_subpool_cleanup: true,
        }))
    }

    async fn replay(
        &mut self,
        record: Box<ReplayRecord>,
    ) -> anyhow::Result<Option<PreparedBlockCommand<'_>>> {
        validate_replay_record_context(
            self.last_block
                .as_ref()
                .map(|last_block| (&last_block.record, last_block.hash)),
            &record,
        )?;
        if record.block_context.block_number == 0 {
            let genesis_header = genesis_header();
            self.last_block = Some(LastBlock {
                record: *record,
                hash: genesis_header.hash(),
                next_cursors: Default::default(),
            });
            return Ok(None);
        }

        let expect_sl_chain_id_tx_after_upgrade = record
            .transactions
            .windows(2)
            .find(|window| {
                matches!(window[0].envelope(), ZkEnvelope::Upgrade(_))
                    && matches!(
                        window[1].as_system_tx_type(),
                        Some(SystemTxType::SetSLChainId(_, _))
                    )
            })
            .is_some();

        Ok(Some(PreparedBlockCommand {
            block_context: record.block_context,
            seal_policy: SealPolicy::UntilExhausted {
                allowed_to_finish_early: false,
            },
            invalid_tx_policy: InvalidTxPolicy::Abort,
            tx_source: MarkingTxStream::unmarkable(futures::stream::iter(record.transactions)),
            metrics_label: "replay",
            protocol_version: record.protocol_version,
            expected_block_output_hash: Some(record.block_output_hash),
            previous_block_timestamp: record.previous_block_timestamp,
            force_preimages: record.force_preimages,
            canonical_upgrade_tx_hash: record.canonical_upgrade_tx_hash,
            expect_sl_chain_id_tx_after_upgrade,
            starting_cursors: record.starting_cursors,
            interop_roots_per_block: self.config.interop_roots_per_block,
            strict_subpool_cleanup: false,
        }))
    }

    async fn rebuild(
        &mut self,
        rebuild: Box<RebuildCommand>,
    ) -> anyhow::Result<Option<PreparedBlockCommand<'_>>> {
        let (previous_block_timestamp, next_cursors, block_hashes) =
            if let Some(last_block) = self.last_block.as_ref() {
                (
                    last_block.record.block_context.timestamp,
                    last_block.next_cursors.clone(),
                    last_block
                        .record
                        .block_context
                        .block_hashes
                        .push(last_block.hash),
                )
            } else {
                (
                    rebuild.replay_record.previous_block_timestamp,
                    rebuild.replay_record.starting_cursors,
                    rebuild.replay_record.block_context.block_hashes,
                )
            };

        let block_number = rebuild.replay_record.block_context.block_number;
        let (execution_version, protocol_version) = (
            rebuild.replay_record.block_context.execution_version,
            rebuild.replay_record.protocol_version,
        );

        if rebuild.make_empty
            && rebuild
                .replay_record
                .transactions
                .iter()
                .any(|tx| matches!(tx.envelope(), ZkEnvelope::Upgrade(_)))
        {
            anyhow::bail!(
                "Cannot make an empty block when there is an upgrade transaction in the replay record for block {}",
                block_number
            );
        }

        let timestamp = if rebuild.reset_timestamp {
            (millis_since_epoch() / 1000) as u64
        } else {
            rebuild.replay_record.block_context.timestamp
        };
        let block_context = BlockContext {
            eip1559_basefee: rebuild.replay_record.block_context.eip1559_basefee,
            native_price: rebuild.replay_record.block_context.native_price,
            pubdata_price: rebuild.replay_record.block_context.pubdata_price,
            block_number,
            timestamp,
            blob_fee: rebuild.replay_record.block_context.blob_fee,
            chain_id: self.config.l2_chain_id,
            coinbase: self.config.fee_collector_address,
            block_hashes,
            gas_limit: self.config.gas_limit,
            pubdata_limit: self.config.pubdata_limit,
            // todo: initialize as source of randomness, i.e. the value of prevRandao
            mix_hash: Default::default(),
            execution_version,
        };
        let txs = if rebuild.make_empty {
            Vec::new()
        } else {
            let first_l1_tx = rebuild
                .replay_record
                .transactions
                .iter()
                .find(|tx| matches!(tx.envelope(), ZkEnvelope::L1(_)));
            // It's possible that we haven't processed some L1 transaction from previous blocks when rebuilding.
            // In that case we shouldn't consider next L1 txs when rebuilding.
            let filter_l1_txs =
                if let Some(ZkEnvelope::L1(l1_tx)) = first_l1_tx.map(|tx| tx.envelope()) {
                    l1_tx.priority_id() != next_cursors.l1_priority_id
                } else {
                    false
                };
            if filter_l1_txs {
                rebuild
                    .replay_record
                    .transactions
                    .into_iter()
                    .filter(|tx| !matches!(tx.envelope(), ZkEnvelope::L1(_)))
                    .collect()
            } else {
                rebuild.replay_record.transactions
            }
        };

        let expect_sl_chain_id_tx_after_upgrade = txs
            .windows(2)
            .find(|window| {
                matches!(window[0].envelope(), ZkEnvelope::Upgrade(_))
                    && matches!(
                        window[1].as_system_tx_type(),
                        Some(SystemTxType::SetSLChainId(_, _))
                    )
            })
            .is_some();

        Ok(Some(PreparedBlockCommand {
            expect_sl_chain_id_tx_after_upgrade,
            block_context,
            tx_source: MarkingTxStream::unmarkable(futures::stream::iter(txs)),
            seal_policy: SealPolicy::UntilExhausted {
                allowed_to_finish_early: true,
            },
            invalid_tx_policy: InvalidTxPolicy::RejectAndContinue {
                mark_in_source: false,
            },
            metrics_label: "rebuild",
            protocol_version,
            expected_block_output_hash: None,
            previous_block_timestamp,
            force_preimages: rebuild.replay_record.force_preimages,
            canonical_upgrade_tx_hash: rebuild.replay_record.canonical_upgrade_tx_hash,
            starting_cursors: next_cursors,
            interop_roots_per_block: self.config.interop_roots_per_block,
            strict_subpool_cleanup: false,
        }))
    }

    pub fn purge_transactions(&self, tx_hashes: Vec<TxHash>) {
        self.pool.purge_transactions(tx_hashes);
    }

    pub async fn on_canonical_state_change(
        &mut self,
        block_output: &BlockOutput,
        replay_record: &ReplayRecord,
        strict_subpool_cleanup: bool,
    ) {
        let mut next_cursors = replay_record.starting_cursors.clone();
        let outcome = self
            .pool
            .on_canonical_state_change(
                block_output.header.clone(),
                &block_output.account_diffs,
                replay_record,
                strict_subpool_cleanup,
            )
            .await;
        if let Some(last_l1_priority_id) = outcome.last_l1_priority_id {
            next_cursors.l1_priority_id = last_l1_priority_id + 1;
            EXECUTION_METRICS
                .next_l1_priority_id
                .set(next_cursors.l1_priority_id);
        }
        if let Some(last_interop_log_id) = outcome.last_interop_log_id {
            self.next_interop_tx_allowed_after = Instant::now() + self.config.service_block_delay;
            next_cursors.interop_root_id = last_interop_log_id + 1;
        }

        if let Some(last_migration_number) = outcome.last_migration_number {
            next_cursors.migration_number = last_migration_number + 1;
        }
        if let Some(target_sl_chain_id) = outcome.last_sl_chain_id_target {
            // Subsequent produced blocks will gate interop traffic on the new value (in particular:
            // stop including interop-root / interop-fee txs once we've migrated back to L1).
            // Otherwise, we will end up with blocks/batches that must be committed to L1 but
            // include interop txs which leads to `CommitBasedInteropNotSupported` revert.
            if self.current_sl_chain_id != target_sl_chain_id {
                tracing::info!(
                    previous_sl_chain_id = self.current_sl_chain_id,
                    new_sl_chain_id = target_sl_chain_id,
                    "applied SetSLChainId tx; updating runtime settlement layer pointer"
                );
                self.current_sl_chain_id = target_sl_chain_id;
            }
        }
        if let Some(last_interop_fee_number) = outcome.last_interop_fee_number {
            next_cursors.interop_fee_number = last_interop_fee_number + 1;
        }

        self.fee_provider.on_canonical_state_change(replay_record);
        self.last_block = Some(LastBlock {
            record: replay_record.clone(),
            hash: block_output.header.hash(),
            next_cursors,
        })
    }
}

pub fn millis_since_epoch() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Incorrect system time")
        .as_millis()
}

// SYSCOIN: full upgrade transactions can be valid at the current protocol version on a fresh
// v31 genesis start; patch-only equal/lower metadata should remain skippable.
fn should_apply_upgrade_metadata(
    upgrade_metadata: &UpgradeMetadata,
    current_protocol_version: &ProtocolSemanticVersion,
    stream_contains_upgrade_tx: bool,
) -> bool {
    upgrade_metadata.protocol_version > *current_protocol_version
        || (stream_contains_upgrade_tx
            && upgrade_metadata.protocol_version == *current_protocol_version)
}

fn validate_genesis_replay_record(record: &ReplayRecord) -> anyhow::Result<()> {
    let genesis_header = genesis_header();
    let expected_block_hashes = BlockHashes::default();
    anyhow::ensure!(
        record.block_context.block_hashes == expected_block_hashes,
        "inconsistent genesis block hashes: expected {:?}, but received {:?}",
        expected_block_hashes,
        record.block_context.block_hashes
    );
    anyhow::ensure!(
        record.block_context.timestamp == genesis_header.timestamp,
        "inconsistent genesis timestamp: expected {}, but received {}",
        genesis_header.timestamp,
        record.block_context.timestamp
    );
    Ok(())
}

fn validate_replay_record_context(
    last_block: Option<(&ReplayRecord, BlockHash)>,
    record: &ReplayRecord,
) -> anyhow::Result<()> {
    if record.block_context.block_number == 0 {
        validate_genesis_replay_record(record)
    } else if let Some((last_record, last_hash)) = last_block {
        validate_next_replay_record(last_record, last_hash, record)
    } else {
        anyhow::bail!(
            "cannot validate replay block {} without previous local block",
            record.block_context.block_number
        )
    }
}

fn validate_next_replay_record(
    last_record: &ReplayRecord,
    last_hash: BlockHash,
    record: &ReplayRecord,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        last_record.block_context.block_number + 1 == record.block_context.block_number,
        "blocks received our of order: last block was {}, but received {}",
        last_record.block_context.block_number,
        record.block_context.block_number
    );
    anyhow::ensure!(
        last_record.block_context.timestamp == record.previous_block_timestamp,
        "inconsistent previous block timestamp: last block was {}, but received {}",
        last_record.block_context.timestamp,
        record.previous_block_timestamp
    );
    let expected_block_hashes = last_record.block_context.block_hashes.push(last_hash);
    anyhow::ensure!(
        expected_block_hashes == record.block_context.block_hashes,
        "inconsistent previous block hashes: last block's (#{}) expected next {:?}, but received new block's {:?}",
        last_record.block_context.block_number,
        expected_block_hashes,
        record.block_context.block_hashes
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_context(block_number: u64, timestamp: u64, block_hashes: BlockHashes) -> BlockContext {
        BlockContext {
            eip1559_basefee: U256::ZERO,
            native_price: U256::ZERO,
            pubdata_price: U256::ZERO,
            block_number,
            timestamp,
            chain_id: 1,
            coinbase: Address::ZERO,
            block_hashes,
            gas_limit: 0,
            pubdata_limit: 0,
            mix_hash: U256::ZERO,
            execution_version: 0,
            blob_fee: U256::ONE,
        }
    }

    fn replay_record(
        block_number: u64,
        timestamp: u64,
        previous_block_timestamp: u64,
        block_hashes: BlockHashes,
    ) -> ReplayRecord {
        ReplayRecord::new(
            block_context(block_number, timestamp, block_hashes),
            Vec::new(),
            previous_block_timestamp,
            semver::Version::new(0, 0, 0),
            ProtocolSemanticVersion::new(0, 31, 0),
            B256::ZERO,
            Vec::new(),
            B256::ZERO,
            Default::default(),
        )
    }

    fn upgrade_metadata(protocol_version: ProtocolSemanticVersion) -> UpgradeMetadata {
        UpgradeMetadata {
            timestamp: 0,
            protocol_version,
            force_preimages: Vec::new(),
            canonical_tx_hash: B256::ZERO,
        }
    }

    #[test]
    fn applies_equal_version_full_upgrade_metadata() {
        let current_protocol_version = ProtocolSemanticVersion::new(0, 31, 0);
        let upgrade_metadata = upgrade_metadata(current_protocol_version.clone());

        assert!(should_apply_upgrade_metadata(
            &upgrade_metadata,
            &current_protocol_version,
            true,
        ));
    }

    #[test]
    fn skips_equal_version_patch_upgrade_metadata() {
        let current_protocol_version = ProtocolSemanticVersion::new(0, 31, 0);
        let upgrade_metadata = upgrade_metadata(current_protocol_version.clone());

        assert!(!should_apply_upgrade_metadata(
            &upgrade_metadata,
            &current_protocol_version,
            false,
        ));
    }

    #[test]
    fn applies_newer_patch_upgrade_metadata() {
        let current_protocol_version = ProtocolSemanticVersion::new(0, 31, 0);
        let upgrade_metadata = upgrade_metadata(ProtocolSemanticVersion::new(0, 31, 1));

        assert!(should_apply_upgrade_metadata(
            &upgrade_metadata,
            &current_protocol_version,
            false,
        ));
    }

    #[test]
    fn replay_record_requires_canonical_newest_previous_block_hash() {
        let mut previous_hashes = BlockHashes::default();
        for (idx, hash) in previous_hashes.0.iter_mut().enumerate() {
            *hash = U256::from(idx as u64);
        }
        let canonical_previous_hash = B256::repeat_byte(0x42);
        let last_record = replay_record(10, 100, 99, previous_hashes);

        let expected_hashes = previous_hashes.push(canonical_previous_hash);
        let mut crafted_hashes = expected_hashes;
        crafted_hashes.0[255] = U256::from(0x9999_u64);

        // This is the incomplete predicate from the vulnerable replay path: older entries shift,
        // but the newest previous-block hash slot is attacker-controlled.
        assert_eq!(previous_hashes.0[1..], crafted_hashes.0[..255]);
        assert_ne!(expected_hashes, crafted_hashes);

        let crafted_record = replay_record(11, 101, 100, crafted_hashes);
        let err =
            validate_next_replay_record(&last_record, canonical_previous_hash, &crafted_record)
                .expect_err("replay must reject corrupted newest previous-block hash");
        assert!(
            err.to_string()
                .contains("inconsistent previous block hashes"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn non_genesis_replay_requires_previous_local_seed() {
        let record = replay_record(11, 101, 100, BlockHashes::default());

        let err = validate_replay_record_context(None, &record)
            .expect_err("first non-genesis replay must require previous local block");
        assert!(
            err.to_string().contains("without previous local block"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn genesis_replay_record_requires_local_genesis_context() {
        let mut invalid_hashes = BlockHashes::default();
        invalid_hashes.0[255] = U256::from(1);
        let record = replay_record(0, genesis_header().timestamp, 0, invalid_hashes);

        let err = validate_genesis_replay_record(&record)
            .expect_err("genesis replay must reject non-empty block hashes");
        assert!(
            err.to_string()
                .contains("inconsistent genesis block hashes"),
            "unexpected error: {err}"
        );
    }
}
