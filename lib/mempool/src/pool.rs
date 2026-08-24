use crate::interop_fee_updater::{InteropFeeUpdater, InteropFeeUpdaterConfig, LocalEthCall};
use crate::metrics::TRANSACTION_POOL_METRICS;
use crate::subpools::interop_fee::InteropFeeSubpool;
use crate::subpools::interop_roots::InteropRootsSubpool;
use crate::subpools::l1::L1Subpool;
use crate::subpools::l2::{L2Subpool, L2TransactionsStreamMarker};
use crate::subpools::sl_chain_id::SlChainIdSubpool;
use crate::subpools::upgrade::{UpgradeSubpool, UpgradeTransactionsStream};
use alloy::consensus::{Header, Sealed};
use alloy::primitives::{Address, ChainId, TxHash};
use alloy::providers::Provider;
use anyhow::Context;
use futures::stream::{BoxStream, PollNext};
use futures::{Stream, StreamExt};
use reth_ethereum_primitives::{Block, BlockBody};
use reth_execution_types::ChangedAccount;
use reth_primitives_traits::SealedBlock;
use reth_tasks::Runtime;
use reth_transaction_pool::{CanonicalStateUpdate, PoolUpdateKind};
use std::num::NonZeroUsize;
use tokio::time::Instant;
use zksync_os_base_token_adjuster::BaseTokenPriceHandle;
use zksync_os_contract_interface::ZkChain;
use zksync_os_contract_interface::l1_discovery::L1State;
use zksync_os_genesis::Genesis;
use zksync_os_interface::types::AccountDiff;
use zksync_os_l1_watcher::{
    InteropWatcher, L1TxWatcher, L1UpgradeTxWatcher, L1WatcherConfig, StartResolver,
};
use zksync_os_provider::NodeProvider;
use zksync_os_storage_api::ReplayRecord;
use zksync_os_types::{
    FeeParams, L1TxSerialId, NodeRole, ProtocolSemanticVersion, SystemTxType, UpgradeInfo,
    UpgradeMetadata, ZkEnvelope, ZkTransaction,
};

/// SYSCOIN: Selection constraint while an authenticated interop bundle still needs a distinct
/// FRI companion under Airbender's stock minimum-two SNARK aggregation rule.
#[derive(Debug, Clone, Copy)]
pub struct InteropCompanionRequest {
    /// Empty zero/zero batches are not commit-valid in Era priority mode. This is `Some` only
    /// when the caller can rule priority mode out (currently: an edge settling on Gateway), and
    /// gives ordinary/system traffic a short grace period before the empty fallback becomes ready.
    pub empty_after: Option<Instant>,
}

/// General pool that provides unified access to all transaction sources in the system.
///
/// Consists of multiple smaller subpools, see [`crate::subpools`] for more information.
pub struct Pool<T> {
    runtime: Runtime,
    genesis: Genesis,
    upgrade_subpool: UpgradeSubpool,
    sl_chain_id_subpool: SlChainIdSubpool,
    interop_fee_subpool: InteropFeeSubpool,
    interop_roots_subpool: InteropRootsSubpool,
    l1_subpool: L1Subpool,
    l2_subpool: T,
    subcomponents: Subcomponents,
}

struct Subcomponents {
    upgrade_watcher: Option<StartResolver<ProtocolSemanticVersion, L1UpgradeTxWatcher>>,
    l1_tx_watcher: Option<StartResolver<u64, L1TxWatcher>>,
    interop_watcher: Option<StartResolver<u64, InteropWatcher>>,
    /// Polls local + gateway state and enqueues interop-fee-update system txs into
    /// `interop_fee_subpool`.
    /// `None` unless this node is responsible for interop fee updates (main node settling on
    /// Gateway) - see [`Pool::new`].
    interop_fee_updater: Option<InteropFeeUpdater>,
}

pub struct Config {
    pub node_role: NodeRole,
    pub chain_id: ChainId,
    pub gateway_chain_id: ChainId,
    pub interop_roots_per_tx: usize,
    pub bytecode_supplier_address: Address,
    // SYSCOIN: startup cursor resolution may need an archive L1 provider while live watchers keep
    // polling the live provider.
    pub archive_lookup_diamond_proxy_l1: Option<ZkChain<NodeProvider>>,
    pub l1_watcher_config: L1WatcherConfig,
    /// SYSCOIN: Explicitly trust the active Gateway head for interop-root ingestion. This is
    /// topology-scoped: it has no effect while the chain settles directly on L1.
    pub optimistic_gateway_head: bool,
    pub interop_fee_updater_config: InteropFeeUpdaterConfig,
}

// SYSCOIN: Keep the irreversible Gateway-head trust decision explicit and scoped to Gateway.
// Direct-L1 topology creates no aggregation-root watcher under the pinned V32 contracts.
fn interop_watcher_config(
    mut config: L1WatcherConfig,
    settles_on_gateway: bool,
    optimistic_gateway_head: bool,
) -> L1WatcherConfig {
    if settles_on_gateway && optimistic_gateway_head {
        config.confirmations = 0;
    }
    config
}

// SYSCOIN: Pinned V32 L1 MessageRoot does not update the shared aggregation trees. Only a chain
// currently settling on Gateway has a meaningful interop-root stream to import.
fn should_start_interop_watcher(settles_on_gateway: bool) -> bool {
    settles_on_gateway
}

impl<T: L2Subpool> Pool<T> {
    pub async fn new(
        runtime: Runtime,
        genesis: Genesis,
        l1_state: &L1State,
        config: Config,
        eth_call: Box<dyn LocalEthCall>,
        base_token_price: BaseTokenPriceHandle,
        l2_subpool: T,
    ) -> anyhow::Result<Self> {
        let upgrade_subpool = UpgradeSubpool::default();
        let sl_chain_id_subpool = SlChainIdSubpool::default();
        let interop_fee_subpool = InteropFeeSubpool::default();
        // SYSCOIN: Keep the library boundary fail-closed even if a caller bypasses node config
        // validation; zero-sized chunks would panic without consuming the FIFO head.
        let interop_roots_per_tx = NonZeroUsize::new(config.interop_roots_per_tx)
            .context("interop_roots_per_tx must be greater than zero")?;
        let interop_roots_subpool = InteropRootsSubpool::new(interop_roots_per_tx);
        let l1_subpool = L1Subpool::new(10);

        // The interop fee updater only runs on the main node and only when it is settling on Gateway.
        let interop_fee_updater = if config.node_role.is_main() && l1_state.settles_on_gateway() {
            Some(InteropFeeUpdater::new(
                eth_call,
                l1_state.diamond_proxy_sl.provider().clone().erased(),
                base_token_price,
                interop_fee_subpool.clone(),
                config.interop_fee_updater_config.clone(),
            ))
        } else {
            None
        };

        let upgrade_watcher = L1UpgradeTxWatcher::create_watcher(
            config.l1_watcher_config.clone(),
            config.chain_id,
            l1_state.bridgehub_l1.clone(),
            l1_state.bridgehub_sl.clone(),
            l1_state.diamond_proxy_l1.clone(),
            config.archive_lookup_diamond_proxy_l1.clone(),
            l1_state.diamond_proxy_sl.clone(),
            config.bytecode_supplier_address,
            upgrade_subpool.clone(),
        )
        .await
        .context("failed to start L1 upgrade transaction watcher")?;

        // SYSCOIN: Pinned V32 has an aggregation-root stream only while this chain settles on
        // Gateway; direct-L1 startup must not create a watcher against L1 MessageRoot.
        let interop_watcher = if should_start_interop_watcher(l1_state.settles_on_gateway()) {
            let interop_watcher_config = interop_watcher_config(
                config.l1_watcher_config.clone(),
                true,
                config.optimistic_gateway_head,
            );
            Some(
                InteropWatcher::create_watcher(
                    interop_watcher_config,
                    l1_state.bridgehub_sl.clone(),
                    l1_state.sl_chain_id,
                    interop_roots_subpool.clone(),
                )
                .await
                .context("failed to create Gateway interop root watcher")?,
            )
        } else {
            None
        };

        let l1_tx_watcher = L1TxWatcher::create_watcher(
            config.l1_watcher_config.clone(),
            l1_state.diamond_proxy_l1.clone(),
            config.archive_lookup_diamond_proxy_l1.clone(),
            l1_state.diamond_proxy_sl.clone(),
            l1_subpool.clone(),
        )
        .await
        .context("failed to create L1 transaction watcher")?;

        let subcomponents = Subcomponents {
            upgrade_watcher: Some(upgrade_watcher),
            l1_tx_watcher: Some(l1_tx_watcher),
            interop_watcher,
            interop_fee_updater,
        };

        Ok(Self {
            runtime,
            genesis,
            upgrade_subpool,
            sl_chain_id_subpool,
            interop_fee_subpool,
            interop_roots_subpool,
            l1_subpool,
            l2_subpool,
            subcomponents,
        })
    }

    /// Initializes mempool with the starting block, expects to be called exactly once during the
    /// node's lifetime.
    pub async fn init(&mut self, replay: &ReplayRecord) {
        let current_protocol_version = &replay.protocol_version;
        self.upgrade_subpool
            .init(current_protocol_version.clone())
            .await;

        // If we start from genesis, we should start by sending upgrade tx for genesis. Same thing
        // for block #1 as it contains this upgrade tx required during replay.
        if replay.block_context.block_number <= 1 {
            let genesis_upgrade = self.genesis.genesis_upgrade_tx().await;
            let upgrade_tx = UpgradeInfo {
                tx: Some(genesis_upgrade.tx.clone()),
                metadata: UpgradeMetadata {
                    protocol_version: genesis_upgrade.protocol_version.clone(),
                    timestamp: 0, // No restrictions on timestamp.
                    force_preimages: genesis_upgrade.force_deploy_preimages.clone(),
                },
            };
            self.upgrade_subpool.insert(upgrade_tx).await;
        }

        self.interop_fee_subpool
            .init(replay.starting_cursors.interop_fee_number)
            .await;
        // SYSCOIN: Canonical replay cleanup must know the exact source cursor even when direct-L1
        // topology intentionally has no Gateway watcher available to refill historical roots.
        self.interop_roots_subpool
            .init(replay.starting_cursors.interop_root_id);

        if let Some(upgrade_watcher) = self.subcomponents.upgrade_watcher.take() {
            self.runtime.spawn_critical_task(
                "L1 upgrade transaction watcher",
                upgrade_watcher.run(current_protocol_version.clone()),
            );
        }
        if let Some(l1_tx_watcher) = self.subcomponents.l1_tx_watcher.take() {
            self.runtime.spawn_critical_task(
                "L1 transaction watcher",
                l1_tx_watcher.run(replay.starting_cursors.l1_priority_id),
            );
        }
        // SYSCOIN: Only Gateway topology owns a V32 interop-root watcher.
        if let Some(interop_watcher) = self.subcomponents.interop_watcher.take() {
            self.runtime.spawn_critical_task(
                "interop roots watcher",
                interop_watcher.run(replay.starting_cursors.interop_root_id),
            );
        }
        if let Some(interop_fee_updater) = self.subcomponents.interop_fee_updater.take() {
            self.runtime
                .spawn_critical_task("interop fee updater", interop_fee_updater.run());
        }
    }

    /// Picks the best source of transactions out of currently available ones. If there are none,
    /// then waits for one to become available.
    ///
    /// Also provides upgrade information is there is one (which is not necessarily accompanied by
    /// an upgrade transaction).
    ///
    /// SYSCOIN: `include_interop_traffic` gates both interop-root and interop-fee system
    /// transactions. Pinned V32 consumes aggregation roots only from Gateway's L2MessageRoot.
    ///
    /// Returns `None` if all transaction sources are closed.
    // SYSCOIN: Stream selection only needs shared access. Keeping this borrow immutable lets the
    // sequencer refresh pending fees after the wait without rebuilding unrelated pool state.
    pub async fn best_transactions_stream<'a>(
        &'a self,
        next_interop_tx_allowed_after: Instant,
        include_interop_traffic: bool,
        interop_companion: Option<InteropCompanionRequest>,
    ) -> Option<StreamOutcome<'a>> {
        let mut upgrade_info_stream = self.upgrade_subpool.upgrade_info_stream().await;

        let interop_root_stream = tokio_stream::StreamExt::peekable(
            self.interop_roots_subpool
                .interop_transactions_with_delay(next_interop_tx_allowed_after)
                .await,
        );

        let mut sl_chain_id_stream = tokio_stream::StreamExt::peekable(
            self.sl_chain_id_subpool.best_transactions_stream().await,
        );
        let interop_fee_stream = tokio_stream::StreamExt::peekable(
            self.interop_fee_subpool.best_transactions_stream().await,
        );

        let l1_stream = self.l1_subpool.best_transactions_stream().await;
        let l2_stream = self.l2_subpool.best_transactions_stream();
        let l2_marker = l2_stream.marker();
        fn prio_left(_: &mut ()) -> PollNext {
            PollNext::Left
        }
        let l1_l2_stream = futures::stream::select_with_strategy(l1_stream, l2_stream, prio_left);
        let mut l1_l2_stream = tokio_stream::StreamExt::peekable(l1_l2_stream);

        let interop_related_stream = futures::stream::select_with_strategy(
            interop_fee_stream,
            interop_root_stream,
            prio_left,
        );
        let mut interop_related_stream = tokio_stream::StreamExt::peekable(interop_related_stream);

        let mut upgrade_metadata = None;
        loop {
            tokio::select! {
                // This select is biased on purpose, meaning `tokio::select!` branches are checked
                // sequentially top to bottom. Transaction types must be ordered by priority -
                // otherwise, if there is some frequent transaction type in the top, under load
                // we might never poll and pick a rarer but important transaction type.
                biased;

                // Upgrade branch is a bit special as it does not always produce a stream of
                // transactions. Sometimes it only sets `upgrade_metadata` and some other stream
                // needs to provide transactions. This is the reason behind `loop` above (which can
                // iterate twice at max).
                Some(upgrade) = tokio_stream::StreamExt::next(&mut upgrade_info_stream) => {
                    // SYSCOIN: Security/protocol upgrades retain absolute upstream priority. If
                    // this crosses a proving-version boundary, the canonical-state transition
                    // expires the old companion request rather than blocking the upgrade.
                    if let Some(upgrade_tx) = &upgrade.tx {
                        tracing::info!(
                            protocol_version = %upgrade.metadata.protocol_version,
                            tx_hash = %upgrade_tx.hash(),
                            "L1 upgrade transaction found for protocol version {}",
                            upgrade.metadata.protocol_version,
                        )
                    } else {
                        tracing::info!(
                            protocol_version = %upgrade.metadata.protocol_version,
                            "L1 patch upgrade (no tx) found for protocol version {}",
                            upgrade.metadata.protocol_version,
                        )
                    }
                    upgrade_metadata = Some(upgrade.metadata);
                    if let Some(tx) = upgrade.tx {
                        return Some(StreamOutcome {
                            kind: StreamOutcomeKind::Transactions,
                            upgrade_metadata,
                            // SYSCOIN: distinguish full upgrades from patch-only metadata so
                            // equal-version genesis upgrades keep their forced preimages.
                            stream_contains_upgrade_tx: true,
                            stream: MarkingTxStream::unmarkable(UpgradeTransactionsStream::one(tx)),
                        });
                    }
                }
                Some(_) = sl_chain_id_stream.peek() => {
                    // todo: this will make sure that SL chain ID transaction is in its own block.
                    //       But we only need to ensure that, if present, it is the first transaction
                    //       in the block. In other words, we could chain it with `l1_l2_stream` as
                    //       a micro-optimization. Given how rare it is, likely not worth the trouble.
                    return Some(StreamOutcome {
                        kind: StreamOutcomeKind::Transactions,
                        upgrade_metadata,
                        stream_contains_upgrade_tx: false,
                        stream: MarkingTxStream::unmarkable(sl_chain_id_stream),
                    });
                }
                Some(_) = interop_related_stream.peek(), if include_interop_traffic => {
                    return Some(StreamOutcome {
                        kind: StreamOutcomeKind::Transactions,
                        upgrade_metadata,
                        stream_contains_upgrade_tx: false,
                        stream: MarkingTxStream::unmarkable(interop_related_stream),
                    });
                }
                Some(_) = l1_l2_stream.peek() => {
                    return Some(StreamOutcome {
                        kind: StreamOutcomeKind::Transactions,
                        upgrade_metadata,
                        stream_contains_upgrade_tx: false,
                        stream: MarkingTxStream::markable(l1_l2_stream, l2_marker),
                    });
                }

                // SYSCOIN: A Gateway-settled edge cannot activate Era priority mode
                // (`activatePriorityMode` is L1-only), so an empty zero/zero block is a valid,
                // deterministic last resort there after a short grace period. Direct-L1 callers
                // leave `empty_after` unset: priority-mode activation can race the sequencer, and
                // Committer rejects zero/zero batches while active. Ready real transactions and
                // upgrades always win above until the timer expires.
                _ = async {
                    if let Some(deadline) = interop_companion.and_then(|request| request.empty_after) {
                        tokio::time::sleep_until(deadline).await;
                    }
                }, if interop_companion.and_then(|request| request.empty_after).is_some() => {
                    return Some(StreamOutcome {
                        kind: StreamOutcomeKind::InteropCompanionEmpty,
                        upgrade_metadata,
                        stream_contains_upgrade_tx: false,
                        stream: MarkingTxStream::unmarkable(futures::stream::empty()),
                    });
                }

                else => {
                    return None;
                }
            }
        }
    }

    /// Removes transactions from the local pool when forwarding to the main node fails after
    /// local insertion. Records them in the `forwarding_rollback_transactions` metric.
    pub fn remove_transactions(&self, tx_hashes: Vec<TxHash>) {
        TRANSACTION_POOL_METRICS
            .forwarding_rollback_transactions
            .inc_by(tx_hashes.len() as u64);
        self.l2_subpool.remove_transactions(tx_hashes);
    }

    /// Removes transactions that were rejected by the ZK VM during block execution and
    /// records them in the `purged_transactions` metric.
    pub fn purge_transactions(&self, tx_hashes: Vec<TxHash>) {
        TRANSACTION_POOL_METRICS
            .purged_transactions
            .inc_by(tx_hashes.len() as u64);
        self.l2_subpool.remove_transactions(tx_hashes);
    }

    pub fn update_pending_block_fees(
        &self,
        fee_params: FeeParams,
        pending_block_blob_fee: Option<u128>,
    ) {
        let mut block_info = self.l2_subpool.block_info();
        block_info.pending_basefee = fee_params.eip1559_basefee.saturating_to();
        block_info.pending_blob_fee = pending_block_blob_fee;
        self.l2_subpool.set_block_info(block_info);
        self.l2_subpool.update_pending_fee_params(fee_params);
    }

    pub async fn on_canonical_state_change(
        &self,
        header: Sealed<Header>,
        account_diffs: &[AccountDiff],
        replay_record: &ReplayRecord,
        strict_subpool_cleanup: bool,
    ) -> anyhow::Result<StateChangeOutcome> {
        let mut upgrade_txs = Vec::new();
        let mut interop_txs = Vec::new();
        let mut interop_fee_txs = Vec::new();
        let mut sl_chain_id_txs = Vec::new();
        let mut l1_transactions = Vec::new();
        let mut l2_transactions = Vec::new();
        for tx in &replay_record.transactions {
            match tx.envelope() {
                ZkEnvelope::System(system_tx) => match system_tx.system_subtype() {
                    SystemTxType::ImportInteropRoots(_) => {
                        interop_txs.push(system_tx);
                    }
                    SystemTxType::SetInteropFee(_) => {
                        interop_fee_txs.push(system_tx);
                    }
                    SystemTxType::SetSLChainId(_, _) => {
                        sl_chain_id_txs.push(system_tx);
                    }
                },
                ZkEnvelope::L1(l1_tx) => {
                    l1_transactions.push(l1_tx);
                }
                ZkEnvelope::L2(l2_tx) => {
                    l2_transactions.push(*l2_tx.hash());
                }
                ZkEnvelope::Upgrade(upgrade) => {
                    upgrade_txs.push(upgrade);
                }
            }
        }
        self.upgrade_subpool
            .on_canonical_state_change(&replay_record.protocol_version, upgrade_txs)
            .await;
        // SYSCOIN: Replay/rebuild must advance from canonical tx data without waiting for a
        // topology-specific watcher; newly produced blocks retain exact live-queue comparison.
        let last_interop_log_id = self
            .interop_roots_subpool
            .on_canonical_state_change(interop_txs, strict_subpool_cleanup)
            .await?;
        let last_interop_fee_number = self
            .interop_fee_subpool
            .on_canonical_state_change(interop_fee_txs, strict_subpool_cleanup)
            .await;
        let sl_chain_id_outcome = self
            .sl_chain_id_subpool
            .on_canonical_state_change(sl_chain_id_txs)
            .await?;
        let last_l1_priority_id = self
            .l1_subpool
            .on_canonical_state_change(l1_transactions)
            .await;

        let (header, hash) = header.into_parts();
        let body = BlockBody::default();
        let block = Block::new(header, body);
        let sealed_block = SealedBlock::new_unchecked(block, hash);
        let changed_accounts = account_diffs
            .iter()
            .map(|diff| ChangedAccount {
                address: diff.address,
                nonce: diff.nonce,
                balance: diff.balance,
            })
            .collect();
        self.l2_subpool
            .on_canonical_state_change(CanonicalStateUpdate {
                new_tip: &sealed_block,
                // pending block fees will be set later in `update_pending_block_fees`
                pending_block_base_fee: 0,
                pending_block_blob_fee: None,
                changed_accounts,
                mined_transactions: l2_transactions,
                update_kind: PoolUpdateKind::Commit,
            });

        // Propagate the just-finalized protocol version to the L2 validator so version-gated
        // stateless checks use the correct version for incoming transactions.
        self.l2_subpool
            .update_pending_protocol_version(replay_record.protocol_version.clone());
        // Refresh the validator's fee params from the executed block's context. This is the only
        // fee source on nodes that don't produce blocks (external nodes never call
        // `update_pending_block_fees`); on the main node these values are overwritten with the
        // pending block's params at the start of each `produce()`.
        self.l2_subpool.update_pending_fee_params(FeeParams {
            eip1559_basefee: replay_record.block_context.eip1559_basefee,
            native_price: replay_record.block_context.native_price,
            pubdata_price: replay_record.block_context.pubdata_price,
        });

        Ok(StateChangeOutcome {
            last_interop_log_id,
            last_l1_priority_id,
            last_migration_number: sl_chain_id_outcome.map(|o| o.last_migration_number),
            last_sl_chain_id_target: sl_chain_id_outcome.map(|o| o.last_sl_chain_id_target),
            last_interop_fee_number,
        })
    }
}

pub struct StreamOutcome<'a> {
    pub kind: StreamOutcomeKind,
    /// Optional upgrade metadata to be applied with transactions in `stream`. Note that even if
    /// this is `Some`, `stream` is not guaranteed to contain an upgrade transaction. The stream may
    /// contain other transaction types if the upgrade is a patch upgrade.
    pub upgrade_metadata: Option<UpgradeMetadata>,
    /// SYSCOIN: whether `stream` contains the full upgrade transaction associated with
    /// `upgrade_metadata`, as opposed to patch-only metadata consumed alongside another tx stream.
    pub stream_contains_upgrade_tx: bool,
    /// Stream of transactions. This is empty only for the explicitly tagged interop companion.
    pub stream: MarkingTxStream<'a>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamOutcomeKind {
    Transactions,
    /// SYSCOIN: One-shot, normal-mode-compatible empty block used to prevent an authenticated
    /// interop bundle from remaining a single FRI forever on an idle Gateway-settled edge.
    InteropCompanionEmpty,
}

#[derive(Debug, Default)]
pub struct StateChangeOutcome {
    /// Last interop log_id that was imported after canonical state change.
    pub last_interop_log_id: Option<u64>,
    /// Last L1 priority ID that was executed after canonical state change.
    pub last_l1_priority_id: Option<L1TxSerialId>,
    /// Last migration number that was executed after canonical state change.
    pub last_migration_number: Option<u64>,
    /// Target settlement-layer chain id of the last `SetSLChainId` system tx applied in the
    /// block (excluding the `u64::MAX` upgrade placeholder).
    pub last_sl_chain_id_target: Option<ChainId>,
    /// Last interop fee update number that was executed after canonical state change.
    pub last_interop_fee_number: Option<u64>,
}

/// Transaction stream that is capable of marking last L2 transaction as invalid.
pub struct MarkingTxStream<'a> {
    pub stream: BoxStream<'a, ZkTransaction>,
    marker: Option<L2TransactionsStreamMarker>,
}

impl<'a> MarkingTxStream<'a> {
    pub fn unmarkable(stream: impl Stream<Item = ZkTransaction> + Send + 'a) -> Self {
        Self {
            stream: stream.boxed(),
            marker: None,
        }
    }

    fn markable(
        stream: impl Stream<Item = ZkTransaction> + Send + 'a,
        marker: L2TransactionsStreamMarker,
    ) -> Self {
        Self {
            stream: stream.boxed(),
            marker: Some(marker),
        }
    }

    pub fn mark_last_l2_tx_as_invalid(&self) {
        let Some(marker) = self.marker.as_ref() else {
            panic!(
                "tried to mark last L2 transaction as invalid but this stream does not serve L2 transactions"
            )
        };
        marker.mark_last_tx_as_invalid()
    }

    // SYSCOIN: Allows sequencer-injected transactions to run before a live L2 stream without
    // dropping the stream marker used to reject VM-invalid L2 transactions from the mempool.
    pub fn prepend_tx(self, tx: ZkTransaction) -> Self {
        Self {
            stream: futures::stream::once(async move { tx })
                .chain(self.stream)
                .boxed(),
            marker: self.marker,
        }
    }

    // SYSCOIN: Preserve stream metadata while allowing finite system streams, such as a protocol
    // upgrade transaction, to be followed by a sequencer-injected transaction.
    pub fn append_tx(self, tx: ZkTransaction) -> Self {
        Self {
            stream: self
                .stream
                .chain(futures::stream::once(async move { tx }))
                .boxed(),
            marker: self.marker,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{L1WatcherConfig, interop_watcher_config, should_start_interop_watcher};
    use std::time::Duration;

    fn watcher_config(confirmations: u64) -> L1WatcherConfig {
        L1WatcherConfig {
            max_blocks_to_process: 100,
            confirmations,
            poll_interval: Duration::from_secs(1),
            finalized_poll_interval: Duration::from_secs(60),
            logs_cache_capacity: 128,
        }
    }

    // SYSCOIN: Gateway-head ingestion is an explicit trust mode, never a topology-only default.
    #[test]
    fn gateway_head_trust_is_explicit() {
        let config = interop_watcher_config(watcher_config(7), true, false);

        assert_eq!(config.confirmations, 7);
    }

    #[test]
    fn direct_l1_topology_has_no_interop_root_watcher() {
        assert!(should_start_interop_watcher(true));
        assert!(!should_start_interop_watcher(false));
    }

    // SYSCOIN: The trust mode removes lag only for the active Gateway settlement topology.
    #[test]
    fn gateway_head_trust_topology_truth_table() {
        for (settles_on_gateway, optimistic_gateway_head, expected_confirmations) in [
            (false, false, 7),
            (false, true, 7),
            (true, false, 7),
            (true, true, 0),
        ] {
            let config = interop_watcher_config(
                watcher_config(7),
                settles_on_gateway,
                optimistic_gateway_head,
            );
            assert_eq!(
                config.confirmations, expected_confirmations,
                "unexpected confirmation depth for settles_on_gateway={settles_on_gateway}, optimistic_gateway_head={optimistic_gateway_head}"
            );
        }
    }
}
