use crate::config::RpcConfig;
use crate::result::ToRpcResult;
use crate::rpc_storage::ReadRpcStorage;
use crate::types::QueryLimits;
mod pending;
use pending::{FullTransactionsReceiver, PendingTransactionKind, PendingTransactionsReceiver};
mod registry;
use registry::{FilterKind, FilterRegistry};
mod scan;
use alloy::eips::{BlockId, BlockNumberOrTag};
use alloy::primitives::{B256, BlockNumber};
use alloy::rpc::types::{
    Filter, FilterBlockOption, FilterChanges, FilterId, Log, PendingTransactionFilterKind,
    Transaction,
};
use jsonrpsee::core::RpcResult;
use scan::scan_logs;
use zksync_os_mempool::subpools::l2::L2Subpool;
use zksync_os_rpc_api::filter::EthFilterApiServer;
use zksync_os_storage_api::RepositoryError;
use zksync_os_types::L2Envelope;

#[derive(Clone)]
pub struct EthFilterNamespace<RpcStorage, Mempool> {
    storage: RpcStorage,
    query_limits: QueryLimits,
    mempool: Mempool,
    registry: FilterRegistry,
}

impl<RpcStorage: ReadRpcStorage, Mempool: L2Subpool> EthFilterNamespace<RpcStorage, Mempool> {
    pub fn new(config: RpcConfig, storage: RpcStorage, mempool: Mempool) -> Self {
        Self {
            storage,
            query_limits: QueryLimits::new(
                config.max_blocks_per_filter,
                config.max_logs_per_response,
            ),
            mempool,
            // SYSCOIN: Namespace clones share one bounded retained-filter registry.
            registry: FilterRegistry::new(
                config.stale_filter_ttl,
                config.max_active_filters,
                config.max_filter_terms,
            ),
        }
    }
}

impl<RpcStorage: ReadRpcStorage, Mempool: L2Subpool> EthFilterNamespace<RpcStorage, Mempool> {
    fn install_filter(&self, make_kind: impl FnOnce() -> FilterKind) -> RpcResult<FilterId> {
        let latest_block = self.storage.repository().get_latest_block();
        // SYSCOIN: The kind factory runs only after the shared reservation succeeds.
        self.registry
            .install_with(make_kind, latest_block.saturating_add(1))
            .to_rpc_result()
    }

    fn filter_changes_impl(
        &self,
        id: FilterId,
    ) -> EthFilterResult<FilterChanges<Transaction<L2Envelope>>> {
        let latest_block = self.storage.repository().get_latest_block();

        self.registry.poll(id, |start_block, kind| match kind {
            FilterKind::PendingTransaction(filter) => Ok((filter.drain(), start_block)),
            FilterKind::Block => {
                if start_block > latest_block {
                    return Ok((FilterChanges::Empty, start_block));
                }
                let mut block_hashes = Vec::new();
                for block_number in start_block..=latest_block {
                    let Some(block) = self
                        .storage
                        .repository()
                        .get_block_by_number(block_number)?
                    else {
                        return Err(EthFilterError::BlockNotFound(block_number.into()));
                    };
                    block_hashes.push(B256::from(block.header.hash_slow()));
                }
                Ok((
                    FilterChanges::Hashes(block_hashes),
                    latest_block.saturating_add(1),
                ))
            }
            FilterKind::Log(filter) => {
                // SYSCOIN: The stored cursor, not the original fromBlock, bounds every later poll.
                let (from, to) =
                    log_changes_range(filter, start_block, latest_block, |block_id| {
                        self.storage
                            .resolve_block_number(block_id)?
                            .ok_or(EthFilterError::BlockNotFound(block_id))
                    })?;
                if from > to {
                    return Ok((FilterChanges::Empty, start_block));
                }
                let logs = self.get_logs_in_block_range((**filter).clone(), from, to)?;
                Ok((FilterChanges::Logs(logs), to.saturating_add(1)))
            }
        })
    }

    fn filter_logs_impl(&self, id: FilterId) -> EthFilterResult<Vec<Log>> {
        let filter = self.registry.get_log_filter(&id)?;
        self.logs_impl(filter)
    }

    fn logs_impl(&self, filter: Filter) -> EthFilterResult<Vec<Log>> {
        let (from, to) = match filter.block_option {
            FilterBlockOption::AtBlockHash(block_hash) => {
                let block_id = block_hash.into();
                let Some(block) = self.storage.resolve_block_number(block_id)? else {
                    return Err(EthFilterError::BlockNotFound(block_id));
                };
                (block, block)
            }
            FilterBlockOption::Range {
                from_block,
                to_block,
            } => self.resolve_range(from_block, to_block)?,
        };
        tracing::trace!(from, to, ?filter, "getting filtered logs");
        self.get_logs_in_block_range(filter, from, to)
    }

    fn get_logs_in_block_range(
        &self,
        filter: Filter,
        from: u64,
        to: u64,
    ) -> EthFilterResult<Vec<Log>> {
        // return empty vector if the range is invalid.
        if from > to {
            return Ok(Vec::new());
        }
        if let Some(max_blocks_per_filter) = self
            .query_limits
            .max_blocks_per_filter
            .filter(|limit| to - from > *limit)
        {
            return Err(EthFilterError::QueryExceedsMaxBlocks(max_blocks_per_filter));
        }
        scan_logs(
            self.storage.repository(),
            &filter,
            from,
            to,
            self.query_limits.max_logs_per_response,
        )
    }

    /// Endless future that evicts stale filters every `stale_filter_ttl` interval.
    pub(crate) async fn watch_and_clear_stale_filters(&self) {
        self.registry.watch_and_clear_stale().await
    }

    fn resolve_range(
        &self,
        from_block: Option<BlockNumberOrTag>,
        to_block: Option<BlockNumberOrTag>,
    ) -> EthFilterResult<(BlockNumber, BlockNumber)> {
        let from_block_id = from_block.unwrap_or_default().into();
        let Some(from) = self.storage.resolve_block_number(from_block_id)? else {
            return Err(EthFilterError::BlockNotFound(from_block_id));
        };
        let to_block_id = to_block.unwrap_or_default().into();
        let Some(to) = self.storage.resolve_block_number(to_block_id)? else {
            return Err(EthFilterError::BlockNotFound(to_block_id));
        };
        Ok((from, to))
    }
}

impl<RpcStorage: ReadRpcStorage, Mempool: L2Subpool> EthFilterApiServer
    for EthFilterNamespace<RpcStorage, Mempool>
{
    fn new_filter(&self, filter: Filter) -> RpcResult<FilterId> {
        // SYSCOIN: Bound installed criteria without changing stateless eth_getLogs queries.
        let filter = self.registry.bounded_log_filter(filter).to_rpc_result()?;
        // SYSCOIN: Resolve a dynamic lower bound once, so delayed polls cannot skip new blocks.
        let start_block = log_filter_start(
            &filter,
            self.storage.repository().get_latest_block(),
            |block_id| {
                self.storage
                    .resolve_block_number(block_id)?
                    .ok_or(EthFilterError::BlockNotFound(block_id))
            },
        )
        .to_rpc_result()?;
        self.registry
            .install_with(|| FilterKind::Log(Box::new(filter)), start_block)
            .to_rpc_result()
    }

    fn new_block_filter(&self) -> RpcResult<FilterId> {
        self.install_filter(|| FilterKind::Block)
    }

    fn new_pending_transaction_filter(
        &self,
        kind: Option<PendingTransactionFilterKind>,
    ) -> RpcResult<FilterId> {
        self.install_filter(|| match kind.unwrap_or_default() {
            PendingTransactionFilterKind::Hashes => {
                let receiver = self.mempool.pending_transactions_listener();
                let pending_txs_receiver = PendingTransactionsReceiver::new(receiver);
                FilterKind::PendingTransaction(PendingTransactionKind::Hashes(pending_txs_receiver))
            }
            PendingTransactionFilterKind::Full => {
                let stream = self.mempool.new_pending_pool_transactions_listener();
                let full_txs_receiver = FullTransactionsReceiver::new(stream);
                FilterKind::PendingTransaction(PendingTransactionKind::FullTransaction(
                    full_txs_receiver,
                ))
            }
        })
    }

    fn filter_changes(&self, id: FilterId) -> RpcResult<FilterChanges<Transaction<L2Envelope>>> {
        self.filter_changes_impl(id).to_rpc_result()
    }

    fn filter_logs(&self, id: FilterId) -> RpcResult<Vec<Log>> {
        self.filter_logs_impl(id).to_rpc_result()
    }

    fn uninstall_filter(&self, id: FilterId) -> RpcResult<bool> {
        Ok(self.registry.uninstall(&id))
    }

    fn logs(&self, filter: Filter) -> RpcResult<Vec<Log>> {
        self.logs_impl(filter).to_rpc_result()
    }
}

// SYSCOIN: Keep the original filter intact for eth_getFilterLogs; only change polling uses a cursor.
fn log_filter_start(
    filter: &Filter,
    latest_block: BlockNumber,
    resolve: impl FnOnce(BlockId) -> EthFilterResult<BlockNumber>,
) -> EthFilterResult<BlockNumber> {
    match filter.block_option {
        FilterBlockOption::Range { from_block, .. } => match from_block.unwrap_or_default() {
            BlockNumberOrTag::Latest | BlockNumberOrTag::Pending => Ok(latest_block),
            block => resolve(block.into()),
        },
        FilterBlockOption::AtBlockHash(hash) => resolve(hash.into()),
    }
}

fn log_changes_range(
    filter: &Filter,
    start_block: BlockNumber,
    latest_block: BlockNumber,
    resolve: impl FnOnce(BlockId) -> EthFilterResult<BlockNumber>,
) -> EthFilterResult<(BlockNumber, BlockNumber)> {
    match filter.block_option {
        FilterBlockOption::Range { to_block, .. } => {
            let end = match to_block.unwrap_or_default() {
                BlockNumberOrTag::Latest | BlockNumberOrTag::Pending => latest_block,
                block => resolve(block.into())?.min(latest_block),
            };
            Ok((start_block, end))
        }
        FilterBlockOption::AtBlockHash(hash) => {
            let block = resolve(hash.into())?;
            Ok((start_block.max(block), latest_block.min(block)))
        }
    }
}

type EthFilterResult<T> = Result<T, EthFilterError>;

/// Errors that can occur in the handler implementation
#[derive(Debug, thiserror::Error)]
pub enum EthFilterError {
    /// Block could not be found by its id (hash/number/tag).
    #[error("block not found")]
    BlockNotFound(BlockId),
    /// Filter not found.
    #[error("filter not found")]
    FilterNotFound(FilterId),
    /// SYSCOIN: The shared installed-filter registry is full.
    #[error("installed filter capacity reached ({max_filters}); uninstall a filter or retry later")]
    FilterCapacityReached { max_filters: u32 },
    /// SYSCOIN: Installed log filters cannot retain arbitrarily large address/topic sets.
    #[error("installed filter exceeds maximum total address/topic terms ({max_terms})")]
    FilterCriteriaTooLarge { max_terms: u32 },
    /// Query scope is too broad.
    #[error("query exceeds max block range {0}")]
    QueryExceedsMaxBlocks(u64),
    /// Query result is too large.
    #[error("query exceeds max results {max_logs}, retry with the range {from_block}-{to_block}")]
    QueryExceedsMaxResults {
        /// Maximum number of logs allowed per response
        max_logs: usize,
        /// Start block of the suggested retry range
        from_block: u64,
        /// End block of the suggested retry range (last successfully processed block)
        to_block: u64,
    },

    #[error(transparent)]
    RepositoryError(#[from] RepositoryError),
}

#[cfg(test)]
// SYSCOIN: Changes must advance independently of the original, reusable eth_getFilterLogs range.
mod tests {
    use super::*;
    use std::num::NonZeroU32;
    use std::time::Duration;

    #[test]
    fn dynamic_lower_bound_is_captured_at_installation() {
        let filter = Filter::default();
        let start = log_filter_start(&filter, 10, |_| panic!("latest is already known")).unwrap();
        assert_eq!(start, 10);
        assert_eq!(
            log_changes_range(&filter, start, 13, |_| panic!("latest is already known")).unwrap(),
            (10, 13)
        );
        assert_eq!(
            log_changes_range(&filter, 14, 17, |_| panic!("latest is already known")).unwrap(),
            (14, 17)
        );
        assert_eq!(filter, Filter::default());
    }

    #[test]
    fn historical_changes_are_not_replayed() {
        let registry = FilterRegistry::new(
            Duration::from_secs(60),
            NonZeroU32::new(1).unwrap(),
            NonZeroU32::new(4).unwrap(),
        );
        let filter = Filter::new().from_block(2u64);
        let start = log_filter_start(&filter, 4, |_| Ok(2)).unwrap();
        let id = registry
            .install_with(|| FilterKind::Log(Box::new(filter.clone())), start)
            .unwrap();
        for (head, expected) in [(4, vec![2, 3, 4]), (4, vec![]), (5, vec![5])] {
            let changes = registry
                .poll(id.clone(), |cursor, _| {
                    let (from, to) = log_changes_range(&filter, cursor, head, |_| {
                        panic!("latest is already known")
                    })?;
                    if from > to {
                        return Ok((Vec::new(), cursor));
                    }
                    Ok(((from..=to).collect::<Vec<_>>(), to + 1))
                })
                .unwrap();
            assert_eq!(changes, expected);
        }
        assert_eq!(registry.get_log_filter(&id).unwrap(), filter);
    }

    #[test]
    fn finality_upper_bound_does_not_consume_unfinalized_blocks() {
        let filter = Filter::new()
            .from_block(2u64)
            .to_block(BlockNumberOrTag::Finalized);
        assert_eq!(
            log_changes_range(&filter, 2, 10, |_| Ok(4)).unwrap(),
            (2, 4)
        );
        assert_eq!(
            log_changes_range(&filter, 5, 10, |_| Ok(4)).unwrap(),
            (5, 4)
        );
        assert_eq!(
            log_changes_range(&filter, 5, 12, |_| Ok(6)).unwrap(),
            (5, 6)
        );
    }

    #[test]
    fn future_and_fixed_upper_bounds_do_not_replay() {
        let filter = Filter::new().from_block(10u64).to_block(12u64);
        assert_eq!(log_filter_start(&filter, 5, |_| Ok(10)).unwrap(), 10);
        assert_eq!(
            log_changes_range(&filter, 10, 5, |_| Ok(12)).unwrap(),
            (10, 5)
        );
        assert_eq!(
            log_changes_range(&filter, 10, 11, |_| Ok(12)).unwrap(),
            (10, 11)
        );
        assert_eq!(
            log_changes_range(&filter, 13, 20, |_| Ok(12)).unwrap(),
            (13, 12)
        );
    }

    #[test]
    fn block_hash_filter_includes_its_block_only_once() {
        let filter = Filter::new().at_block_hash(B256::repeat_byte(1));
        assert_eq!(log_filter_start(&filter, 10, |_| Ok(3)).unwrap(), 3);
        assert_eq!(
            log_changes_range(&filter, 3, 10, |_| Ok(3)).unwrap(),
            (3, 3)
        );
        assert_eq!(
            log_changes_range(&filter, 4, 11, |_| Ok(3)).unwrap(),
            (4, 3)
        );
    }
}
