use crate::watcher::L1WatcherError;
use alloy::consensus::Transaction;
use alloy::eips::{BlockId, BlockNumberOrTag};
use alloy::primitives::{Address, BlockNumber, Log, TxHash, U256};
use alloy::providers::Provider;
use alloy::rpc::types::Filter;
use alloy::sol_types::SolEvent;
use anyhow::Context as _;
use backon::{ConstantBuilder, Retryable};
use std::fmt::Debug;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use zksync_os_batch_types::{CommittedBatchInfo, DiscoveredCommittedBatch};
use zksync_os_contract_interface::IChainAssetHandler;
use zksync_os_contract_interface::IExecutor::ReportCommittedBatchRangeZKsyncOS;
use zksync_os_contract_interface::calldata::CommitCalldata;
use zksync_os_contract_interface::is_method_missing;
use zksync_os_contract_interface::{Bridgehub, IExecutor, MessageRoot, ZkChain};
use zksync_os_provider::NodeProvider;

// SYSCOIN: Startup cursor resolution may need historical L1 state that a live
// pruned provider cannot serve. Prefer an archive provider for the lookup, but
// retry the live provider so recent cursors still work if the archive endpoint
// is unavailable or lagging.
pub async fn find_startup_block_with_archive_fallback<F, Fut>(
    live_zk_chain: ZkChain<NodeProvider>,
    archive_zk_chain: Option<ZkChain<NodeProvider>>,
    operation: &'static str,
    find: F,
) -> anyhow::Result<BlockNumber>
where
    F: Fn(ZkChain<NodeProvider>) -> Fut,
    Fut: Future<Output = anyhow::Result<BlockNumber>>,
{
    if let Some(archive_zk_chain) = archive_zk_chain {
        match find(archive_zk_chain).await {
            Ok(block) => return Ok(block),
            Err(archive_err) => {
                let archive_err = format!("{archive_err:#}");
                tracing::warn!(
                    operation,
                    archive_error = archive_err,
                    "archive provider failed startup cursor lookup; retrying live provider",
                );
                return find(live_zk_chain).await.with_context(|| {
                    format!(
                        "archive provider failed startup cursor lookup for {operation}: {archive_err}; \
                         live provider fallback also failed"
                    )
                });
            }
        }
    }

    find(live_zk_chain).await
}

// SYSCOIN: `find_block_by_migration_number` returns the provider's latest block
// when the target migration has not happened yet. If an archive provider is
// lagging behind the target migration, only start from the live tip when live
// latest state also proves the target has not happened yet; otherwise fail
// closed until archive catches up enough to resolve the historical cursor.
pub async fn find_startup_migration_block_with_archive_fallback(
    live_zk_chain: ZkChain<NodeProvider>,
    archive_zk_chain: Option<ZkChain<NodeProvider>>,
    chain_asset_handler: Address,
    chain_id: u64,
    migration_number: u64,
    operation: &'static str,
) -> anyhow::Result<BlockNumber> {
    if let Some(archive_zk_chain) = archive_zk_chain {
        match latest_migration_number(&archive_zk_chain, chain_asset_handler, chain_id).await {
            Ok((_archive_latest, archive_latest_migration_number))
                if archive_latest_migration_number >= U256::from(migration_number) =>
            {
                match find_block_by_migration_number(
                    archive_zk_chain,
                    chain_asset_handler,
                    chain_id,
                    migration_number,
                )
                .await
                {
                    Ok(block) => return Ok(block),
                    Err(archive_err) => {
                        let archive_err = format!("{archive_err:#}");
                        tracing::warn!(
                            operation,
                            archive_error = archive_err,
                            "archive provider failed startup migration lookup; retrying live provider",
                        );
                        return find_block_by_migration_number(
                            live_zk_chain,
                            chain_asset_handler,
                            chain_id,
                            migration_number,
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "archive provider failed startup migration lookup for {operation}: {archive_err}; \
                                 live provider fallback also failed"
                            )
                        });
                    }
                }
            }
            Ok((archive_latest, archive_latest_migration_number)) => {
                return match latest_migration_number(&live_zk_chain, chain_asset_handler, chain_id)
                    .await
                {
                    Ok((live_latest, live_latest_migration_number))
                        if live_latest_migration_number < U256::from(migration_number) =>
                    {
                        tracing::warn!(
                            operation,
                            archive_latest,
                            %archive_latest_migration_number,
                            live_latest,
                            %live_latest_migration_number,
                            migration_number,
                            "archive provider has not reached startup migration target; using live tip because target is not reached on live either",
                        );
                        Ok(live_latest)
                    }
                    Ok((live_latest, live_latest_migration_number)) => {
                        anyhow::bail!(
                            "archive provider has not reached startup migration target for {operation}: \
                             archive latest block {archive_latest} has migration number {archive_latest_migration_number}, \
                             but live latest block {live_latest} has migration number {live_latest_migration_number} \
                             for target migration {migration_number}; archive must catch up or live provider must support historical reads"
                        )
                    }
                    Err(live_err) => {
                        let live_err = format!("{live_err:#}");
                        anyhow::bail!(
                            "archive provider has not reached startup migration target for {operation}: \
                             archive latest block {archive_latest} has migration number {archive_latest_migration_number}, \
                             and live provider latest migration check failed: {live_err}"
                        )
                    }
                };
            }
            Err(archive_err) => {
                let archive_err = format!("{archive_err:#}");
                tracing::warn!(
                    operation,
                    archive_error = archive_err,
                    "archive provider failed startup migration tip lookup; retrying live provider",
                );
                return find_block_by_migration_number(
                    live_zk_chain,
                    chain_asset_handler,
                    chain_id,
                    migration_number,
                )
                .await
                .with_context(|| {
                    format!(
                        "archive provider failed startup migration tip lookup for {operation}: {archive_err}; \
                         live provider fallback also failed"
                    )
                });
            }
        }
    }

    find_block_by_migration_number(
        live_zk_chain,
        chain_asset_handler,
        chain_id,
        migration_number,
    )
    .await
}

async fn latest_migration_number(
    zk_chain: &ZkChain<NodeProvider>,
    chain_asset_handler: Address,
    chain_id: u64,
) -> anyhow::Result<(BlockNumber, U256)> {
    let instance = IChainAssetHandler::new(chain_asset_handler, zk_chain.provider().clone());
    let latest = instance.provider().get_block_number().await?;
    let latest_migration_number = instance
        .migrationNumber(U256::from(chain_id))
        .block(latest.into())
        .call()
        .await?;
    Ok((latest, latest_migration_number))
}

/// Finds the first block where `IChainAssetHandler::migrationNumber(chain_id) >= migration_number`
/// using binary search. Returns latest block if migration number is not reached yet.
///
/// Used by both [`GatewayMigrationWatcher`][crate::GatewayMigrationWatcher] (on L1) and
/// [`MigrationCompleteWatcher`][crate::MigrationCompleteWatcher] (on the current settlement layer)
/// to determine the block from which to start scanning for migration events.
pub async fn find_block_by_migration_number(
    zk_chain: ZkChain<NodeProvider>,
    chain_asset_handler: Address,
    chain_id: u64,
    migration_number: u64,
) -> anyhow::Result<BlockNumber> {
    let instance = Arc::new(IChainAssetHandler::new(
        chain_asset_handler,
        zk_chain.provider().clone(),
    ));
    let target = U256::from(migration_number);
    let latest = instance.provider().get_block_number().await?;
    // SYSCOIN: The current pinned V32 ChainAssetHandler must answer this lookup. Only the binary
    // search below may treat method-missing at historical pre-deployment blocks as `false`.
    let latest_migration_number = instance
        .migrationNumber(U256::from(chain_id))
        .block(latest.into())
        .call()
        .await?;
    // If this migration has not been reached yet, return the latest block.
    if latest_migration_number < target {
        return Ok(latest);
    }

    // The chain's diamond proxy deployment block is a safe lower bound for CAH searches: the proxy
    // can only exist when the bridgehub ecosystem (including CAH, when present) is at least
    // partially up. The predicate still guards against CAH being absent for the V30→V31 migration
    // window where the proxy existed before CAH was deployed.
    let start_block = zk_chain.deployment_block().await?;
    find_l1_block_by_predicate(Arc::new(zk_chain), start_block, move |zk, block| {
        let instance = instance.clone();
        async move {
            let code = zk
                .provider()
                .get_code_at(*instance.address())
                .block_id(block.into())
                .await?;
            if code.0.is_empty() {
                return Ok(false);
            }
            // At this block the address may have code but not yet be the ChainAssetHandler
            // (e.g. a proxy upgraded to it only later), so `migrationNumber` reverts. Treat a
            // revert as "not deployed yet" (false); real RPC errors still propagate.
            let res = match instance
                .migrationNumber(U256::from(chain_id))
                .block(block.into())
                .call()
                .await
            {
                Ok(res) => res,
                Err(err) if is_method_missing(&err) => return Ok(false),
                Err(err) => return Err(err.into()),
            };
            Ok(res >= target)
        }
    })
    .await
}

/// Maximum number of L1 blocks that we can scan in a reasonable amount of time.
///
/// Rough calculations: 10min * 10 req/s * 1000 blocks/req = 600 * 10 * 1000 = 6_000_000
const MAX_L1_BLOCKS_TO_SCAN_LINEARLY: u64 = 6_000_000;
// SYSCOIN: a stale RPC height must not make a revert scan succeed as "no events".
const STALE_L1_HEIGHT_RETRY_DELAY: Duration = Duration::from_millis(200);
// SYSCOIN: keep startup fail-closed while allowing brief load-balanced RPC lag to recover.
const STALE_L1_HEIGHT_RETRY_ATTEMPTS: usize = 10;

/// Binary-searches `[start_block_number, latest]` for the first block at which `predicate` returns
/// `true`. The predicate must be monotonic over the search range (caller's responsibility).
///
/// **Caller must ensure `start_block_number >= contract.deployment_block`** — the predicate is
/// invoked without a code-presence guard, so calling it at blocks where the contract is not yet
/// deployed will produce undefined results (typically an RPC error or a `false`-returning revert).
pub async fn find_l1_block_by_predicate<Fut: Future<Output = anyhow::Result<bool>>>(
    zk_chain: Arc<ZkChain<NodeProvider>>,
    start_block_number: BlockNumber,
    predicate: impl Fn(Arc<ZkChain<NodeProvider>>, u64) -> Fut,
) -> anyhow::Result<BlockNumber> {
    let latest = zk_chain.provider().get_block_number().await?;

    // Ensure the predicate is true by the upper bound, or bail early.
    if !predicate(zk_chain.clone(), latest).await? {
        anyhow::bail!(
            "Condition not satisfied up to latest block: contract not deployed yet \
             or target not reached.",
        );
    }

    // Binary search on [start_block_number, latest] for the first block where predicate is true.
    let (mut lo, mut hi) = (start_block_number, latest);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if predicate(zk_chain.clone(), mid).await? {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }

    Ok(lo)
}

/// Looks for an L1 event that happened in block range `[start_block_number; latest_block]`
/// and matching provided predicate. Returns latest L1 block that contains such an event or `None`
/// if there is not any.
async fn find_last_matching_event<E: SolEvent + Debug>(
    address: Address,
    provider: &NodeProvider,
    start_block_number: BlockNumber,
    max_blocks_to_scan: u64,
    predicate: impl Fn(&E) -> bool,
) -> anyhow::Result<Option<BlockNumber>> {
    let mut current_block = start_block_number;
    let latest_block = latest_block_for_event_scan(provider, start_block_number).await?;

    tracing::debug!(
        %address,
        start_block_number,
        latest_block,
        max_blocks_to_scan,
        signature = E::SIGNATURE,
        "looking for last matching event"
    );

    let blocks_to_scan = event_scan_block_count(start_block_number, latest_block)?;
    if blocks_to_scan > MAX_L1_BLOCKS_TO_SCAN_LINEARLY {
        tracing::warn!(blocks_to_scan, "scanning a lot of L1 blocks");
    }
    // SYSCOIN: avoid a non-advancing event scan; callers rely on this helper to prove the range.
    anyhow::ensure!(
        max_blocks_to_scan > 0,
        "max_blocks_to_scan must be non-zero"
    );

    let mut filter = Filter::new()
        .address(address)
        .event_signature(E::SIGNATURE_HASH);
    let mut last_block_with_event = None;
    // SYSCOIN: scan the documented inclusive range `[start_block_number; latest_block]`.
    while current_block <= latest_block {
        // Inspect up to `max_blocks_to_scan` L1 blocks at a time
        let filter_to_block =
            latest_block.min(current_block.saturating_add(max_blocks_to_scan - 1));
        filter = filter.from_block(current_block).to_block(filter_to_block);
        let logs = provider.get_logs(&filter).await?;
        tracing::trace!(
            from_block = current_block,
            to_block = filter_to_block,
            log_count = logs.len(),
            "fetched logs"
        );
        for log in logs {
            let event = E::decode_log(&log.inner)?.data;
            if predicate(&event) {
                let l1_block = log
                    .block_number
                    .expect("indexed event log without block number");
                tracing::debug!(
                    %address,
                    ?event,
                    "found new matching event on L1"
                );
                last_block_with_event = Some(l1_block);
            }
        }
        let Some(next_block) = filter_to_block.checked_add(1) else {
            break;
        };
        current_block = next_block;
    }
    Ok(last_block_with_event)
}

// SYSCOIN: retry transient stale heights, but fail closed if the scan range still cannot be
// proven against the provider tip.
async fn latest_block_for_event_scan(
    provider: &NodeProvider,
    start_block_number: BlockNumber,
) -> anyhow::Result<BlockNumber> {
    let mut stale_height_attempts = 0;

    loop {
        let latest_block = match provider.get_block_number().await {
            Ok(latest_block) => latest_block,
            Err(err) if stale_height_attempts + 1 < STALE_L1_HEIGHT_RETRY_ATTEMPTS => {
                stale_height_attempts += 1;
                tracing::debug!(
                    start_block_number,
                    attempt = stale_height_attempts,
                    error = %err,
                    "retrying provider tip fetch before failing event scan"
                );
                tokio::time::sleep(STALE_L1_HEIGHT_RETRY_DELAY).await;
                continue;
            }
            Err(err) => return Err(err.into()),
        };
        match event_scan_block_count(start_block_number, latest_block) {
            Ok(_) => return Ok(latest_block),
            Err(err) if stale_height_attempts + 1 < STALE_L1_HEIGHT_RETRY_ATTEMPTS => {
                stale_height_attempts += 1;
                tracing::debug!(
                    start_block_number,
                    latest_block,
                    attempt = stale_height_attempts,
                    error = %err,
                    "retrying stale provider height before failing closed"
                );
                tokio::time::sleep(STALE_L1_HEIGHT_RETRY_DELAY).await;
            }
            Err(err) => return Err(err),
        }
    }
}

// SYSCOIN: keep the stale-height check separate so regressions are unit-tested without a live RPC.
fn event_scan_block_count(
    start_block_number: BlockNumber,
    latest_block: BlockNumber,
) -> anyhow::Result<u64> {
    // SYSCOIN: a cursor exactly one past the tip is a proven empty inclusive range. This occurs
    // when a commit is at the current tip and its subsequent revert scan starts at the next block.
    if latest_block.checked_add(1) == Some(start_block_number) {
        return Ok(0);
    }

    latest_block
        .checked_sub(start_block_number)
        .and_then(|span| span.checked_add(1))
        .with_context(|| {
            format!(
                "L1 RPC latest block {latest_block} is behind event scan start block \
                 {start_block_number}; refusing to treat the unscanned range as empty"
            )
        })
}

/// Looks for an L1 batch revert event that happened in block range `[start_block_number; latest_block]`
/// and has affected batch `batch_number`. Returns latest L1 block that contains such an event or `None`
/// if there is not any.
///
/// Batch `batch_number` MUST have been committed before `start_block_number`.
async fn find_latest_l1_revert(
    zk_chain: &ZkChain<NodeProvider>,
    batch_number: u64,
    start_block_number: BlockNumber,
    max_blocks_to_scan: u64,
) -> anyhow::Result<Option<BlockNumber>> {
    find_last_matching_event::<IExecutor::BlocksRevert>(
        *zk_chain.address(),
        zk_chain.provider(),
        start_block_number,
        max_blocks_to_scan,
        |e| e.totalBatchesCommitted < batch_number,
    )
    .await
}

/// Finds first L1 block that contains **non-reverted** batch commitment event on L1 matching
/// requested batch.
///
/// Returns latest L1 block is there is none.
///
/// For any batch `B` that was reverted in tx `T` belonging to L1 block `b` the following MUST hold:
/// `b` CAN contain commit event for `B` that happened either before `T` or after `T` but MUST NOT
/// contain both. See comments inside the implementation for more details.
pub async fn find_l1_commit_block_by_batch_number(
    zk_chain: ZkChain<NodeProvider>,
    batch_number: u64,
    max_l1_blocks_to_scan: u64,
) -> anyhow::Result<BlockNumber> {
    let is_batch_committed = move |zk: Arc<ZkChain<NodeProvider>>, block: BlockNumber| async move {
        let res = zk.get_total_batches_committed(block.into()).await?;
        Ok(res >= batch_number)
    };
    let deployment_block = zk_chain.deployment_block().await?;
    // This predicate is not monotonic because committed batches can be reverted. Even then, this
    // binary search will find **some** L1 block that commits our batch. If revert and another commit
    // happen after the found L1 block, then we will find them as handled by logic in the rest of the
    // function. If there are none, then we will not find anything and return this L1 block as a
    // result.
    let l1_block_with_commit = find_l1_block_by_predicate(
        Arc::new(zk_chain.clone()),
        deployment_block,
        is_batch_committed,
    )
    .await?;
    tracing::debug!(
        batch_number,
        l1_block_with_commit,
        "found first L1 block containing batch commitment"
    );

    let last_l1_block_with_revert = find_latest_l1_revert(
        &zk_chain,
        batch_number,
        // Start from next block as current block might contain unrelated reverts. Note that our
        // batch was observed as committed at the END of block `l1_block_with_commit` so any
        // preceding reverts are irrelevant.
        l1_block_with_commit + 1,
        max_l1_blocks_to_scan,
    )
    .await?;
    match last_l1_block_with_revert {
        Some(last_l1_block_with_revert) => {
            tracing::info!(
                batch_number,
                last_l1_block_with_revert,
                "looking for batch commitment after last revert"
            );
            // Run binary search one more time but start from `last_l1_block_with_revert` now.
            // `last_l1_block_with_revert` might contain EITHER commit event for our batch that
            // happened BEFORE revert or AFTER revert. But it cannot contain both, otherwise L1
            // Watcher will index reverted commit first. To mitigate this, we can make L1 Watcher
            // interactively resistant to reverts that happened in the same block (it would watch
            // for both `BlockCommit` and `BlocksRevert`). This scenario should not happen in the
            // current implementation, however, and hence can be safely ignored for now.
            let l1_block_with_commit = find_l1_block_by_predicate(
                Arc::new(zk_chain),
                last_l1_block_with_revert,
                is_batch_committed,
            )
            .await?;
            tracing::info!(
                batch_number,
                l1_block_with_commit,
                "found non-reverted batch commitment on L1"
            );
            Ok(l1_block_with_commit)
        }
        None => {
            tracing::info!(
                batch_number,
                l1_block_with_commit,
                "no batch reverts found on L1"
            );
            Ok(l1_block_with_commit)
        }
    }
}

/// Finds first L1 block that contains batch execution event on L1 matching requested batch.
///
/// Returns latest L1 block is there is none.
pub async fn find_l1_execute_block_by_batch_number(
    zk_chain: ZkChain<NodeProvider>,
    batch_number: u64,
) -> anyhow::Result<BlockNumber> {
    // Execution cannot be reverted, so unlike in `find_l1_commit_block_by_batch_number`, we do not need
    // to take L1 reverts into account here.
    let deployment_block = zk_chain.deployment_block().await?;
    find_l1_block_by_predicate(
        Arc::new(zk_chain),
        deployment_block,
        move |zk, block| async move {
            let res = zk.get_total_batches_executed(block.into()).await?;
            Ok(res >= batch_number)
        },
    )
    .await
}

/// Finds a safe rescan block for the next MessageRoot ID the chain has not imported.
///
/// If the next ID has already published, this is the first block whose counter reached it. If the
/// local cursor is exactly one beyond the current counter, the chain is caught up and the canonical
/// tip is returned so the watcher can rescan it, skip old IDs, and tail future logs.
pub async fn find_l1_block_by_interop_root_id(
    bridgehub: Bridgehub<NodeProvider>,
    next_interop_root_id: u64,
) -> anyhow::Result<BlockNumber> {
    if next_interop_root_id == 0 {
        return Ok(0);
    }
    let message_root_address = bridgehub.message_root_address().await?;
    let message_root = Arc::new(MessageRoot::new(
        message_root_address,
        bridgehub.provider().clone(),
    ));
    let latest_block = message_root
        .provider()
        .get_block_by_number(BlockNumberOrTag::Latest)
        .await?
        .context("active MessageRoot provider returned no latest block")?;
    let latest = latest_block.header.inner.number;
    let latest_hash = latest_block.header.hash;
    // The provider's cache resolves (and remembers) the MessageRoot deployment block, giving the
    // search a tight lower bound without a per-iteration code-existence guard.
    let deployment_block = message_root.deployment_block().await?;

    let predicate =
        async |message_root: Arc<MessageRoot<NodeProvider>>, block: u64| -> anyhow::Result<bool> {
            let res = message_root.interop_root_log_id(block.into()).await?;
            Ok(res >= next_interop_root_id)
        };
    // SYSCOIN: V32 pins the counter getter, so a missing/reverting method is a topology error, not
    // a legacy signal. Anchor latest state by canonical hash and distinguish a normal caught-up
    // next cursor from a corrupt cursor that is more than one ID ahead.
    let latest_counter = message_root
        .interop_root_log_id(BlockId::hash_canonical(latest_hash))
        .await?;
    if interop_cursor_is_caught_up(next_interop_root_id, latest_counter)? {
        return Ok(latest);
    }

    let (mut lo, mut hi) = (deployment_block, latest);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let mid_matches_target = predicate(message_root.clone(), mid).await?;
        if mid_matches_target {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }

    // SYSCOIN: A concurrent reorg invalidates the numeric binary-search snapshot. Restart startup
    // discovery instead of returning a block resolved across two canonical histories.
    let canonical_latest = message_root
        .provider()
        .get_block_by_number(BlockNumberOrTag::Number(latest))
        .await?
        .context("active MessageRoot latest block disappeared during cursor resolution")?;
    anyhow::ensure!(
        canonical_latest.header.hash == latest_hash,
        "active MessageRoot chain reorged during interop cursor resolution"
    );
    Ok(lo)
}

// SYSCOIN: The persisted value is a next-ID cursor. Exactly counter+1 is normal caught-up state;
// any larger gap would skip roots and must fail startup before the watcher begins polling.
fn interop_cursor_is_caught_up(next_id: u64, latest_counter: u64) -> anyhow::Result<bool> {
    if next_id <= latest_counter {
        return Ok(false);
    }
    anyhow::ensure!(
        latest_counter.checked_add(1) == Some(next_id),
        "persisted next interop root ID {next_id} is more than one ahead of active MessageRoot counter {latest_counter}"
    );
    Ok(true)
}

/// Fetches and decodes stored batch data for batch `batch_number` that is expected to have been
/// committed in `l1_block_number`, together with the transaction that committed it. Returns
/// `None` if the requested batch was not committed in that block.
pub async fn fetch_stored_batch_data(
    zk_chain: &ZkChain<NodeProvider>,
    l1_block_number: BlockNumber,
    batch_number: u64,
) -> anyhow::Result<Option<(DiscoveredCommittedBatch, TxHash)>> {
    let Some((commit_log, tx_hash)) =
        find_commit_log(zk_chain, l1_block_number, batch_number).await?
    else {
        return Ok(None);
    };
    let batch_info = fetch_committed_batch_data(zk_chain, tx_hash, l1_block_number, batch_number)
        .await?
        .into_stored();

    Ok(Some((
        DiscoveredCommittedBatch {
            batch_info,
            block_range: commit_log.firstBlockNumber..=commit_log.lastBlockNumber,
        },
        tx_hash,
    )))
}

/// Finds the `ReportCommittedBatchRangeZKsyncOS` commit event for `batch_number` in
/// `l1_block_number`, returning the decoded event together with the hash of the transaction that
/// emitted it, or `None` if no matching event is present in that block.
pub(crate) async fn find_commit_log(
    zk_chain: &ZkChain<NodeProvider>,
    l1_block_number: BlockNumber,
    batch_number: u64,
) -> anyhow::Result<Option<(Log<ReportCommittedBatchRangeZKsyncOS>, TxHash)>> {
    let logs = zk_chain
        .provider()
        .get_logs(
            &Filter::new()
                .address(*zk_chain.address())
                .event_signature(ReportCommittedBatchRangeZKsyncOS::SIGNATURE_HASH)
                .from_block(l1_block_number)
                .to_block(l1_block_number),
        )
        .await?;
    // Take the last matching log: a batch may be committed, reverted, and recommitted in one L1
    // block, in which case only the final commit can match the block's resulting stored batch hash.
    Ok(logs
        .into_iter()
        .filter_map(|log| {
            let batch_log = ReportCommittedBatchRangeZKsyncOS::decode_log(&log.inner)
                .expect("unable to decode `ReportCommittedBatchRangeZKsyncOS` log");
            (batch_log.batchNumber == batch_number).then(|| {
                (
                    batch_log,
                    log.transaction_hash.expect("indexed log without tx hash"),
                )
            })
        })
        .next_back())
}

/// Fetches batch commit transaction and extra data from L1 required to construct `CommitedBatch`.
/// Retries if the transaction is pending (exists but has no block number yet) or not yet visible.
pub async fn fetch_committed_batch_data(
    zk_chain: &ZkChain<NodeProvider>,
    tx_hash: TxHash,
    l1_block_number: BlockNumber,
    batch_number: u64,
) -> Result<CommittedBatchInfo, L1WatcherError> {
    // The commit transaction carries the `CommitBatchInfo` calldata, while the `BlockCommit` event
    // from that same transaction carries the commitment. Both can transiently lag right after the
    // commit is observed when hitting a load-balanced RPC, so each is retried.
    let retry_policy = || {
        ConstantBuilder::default()
            .with_delay(Duration::from_millis(200))
            .with_max_times(50)
    };

    let tx_fut = async {
        (|| async {
            let tx = zk_chain
                .provider()
                .get_transaction_by_hash(tx_hash)
                .await
                .map_err(|e| L1WatcherError::Other(e.into()))?
                .ok_or_else(|| {
                    L1WatcherError::Other(anyhow::anyhow!("commit tx {tx_hash} not found"))
                })?;
            tx.block_number.ok_or_else(|| {
                L1WatcherError::Other(anyhow::anyhow!(
                    "commit tx {tx_hash} has no block number (still pending)"
                ))
            })?;
            Ok::<_, L1WatcherError>(tx)
        })
        .retry(retry_policy())
        .await
    };

    // SYSCOIN: The batch commitment is emitted in the `BlockCommit` event of the commit transaction.
    // Reading it from L1 directly is safe and accurate, unlike deriving it from the current
    // protocol version / upgrade transaction hash, which reflect the latest chain state rather than
    // the state at the moment this batch was committed. Bind the lookup to `tx_hash`: same-block
    // commit/revert/re-commit sequences can otherwise expose more than one `BlockCommit` for the
    // same batch number.
    let log_fut = async {
        (|| async {
            zk_chain
                .provider()
                .get_logs(
                    &Filter::new()
                        .address(*zk_chain.address())
                        .event_signature(IExecutor::BlockCommit::SIGNATURE_HASH)
                        .topic1(U256::from(batch_number))
                        .from_block(l1_block_number)
                        .to_block(l1_block_number),
                )
                .await
                .map_err(|e| L1WatcherError::Other(e.into()))?
                .into_iter()
                .find(|log| log.transaction_hash == Some(tx_hash))
                .ok_or_else(|| {
                    L1WatcherError::Other(anyhow::anyhow!(
                        "`BlockCommit` event for batch {batch_number} from commit tx {tx_hash} not found in L1 block {l1_block_number}"
                    ))
                })
        })
        .retry(retry_policy())
        .await
    };

    let (tx, log) = tokio::try_join!(tx_fut, log_fut)?;
    if tx.block_number != Some(l1_block_number) {
        return Err(L1WatcherError::Other(anyhow::anyhow!(
            "commit tx {tx_hash} belongs to L1 block {:?}, but block {l1_block_number} was expected",
            tx.block_number
        )));
    }

    let CommitCalldata {
        commit_batch_info, ..
    } = CommitCalldata::decode(tx.input()).map_err(L1WatcherError::Other)?;
    if commit_batch_info.batch_number != batch_number {
        return Err(L1WatcherError::Other(anyhow::anyhow!(
            "commit tx {tx_hash} encodes batch {} but batch {batch_number} was expected",
            commit_batch_info.batch_number
        )));
    }

    let block_commit = IExecutor::BlockCommit::decode_log(&log.inner).map_err(|e| {
        L1WatcherError::Other(anyhow::anyhow!(
            "failed to decode `BlockCommit` event for batch {batch_number}: {e}"
        ))
    })?;
    if block_commit.batchHash != commit_batch_info.new_state_commitment {
        return Err(L1WatcherError::Other(anyhow::anyhow!(
            "`BlockCommit` event for batch {batch_number} from commit tx {tx_hash} emitted batchHash {}, but commit calldata has newStateCommitment {}",
            block_commit.batchHash,
            commit_batch_info.new_state_commitment
        )));
    }
    let commitment = block_commit.commitment;

    Ok(CommittedBatchInfo {
        commit_info: commit_batch_info,
        commitment,
    })
}

#[cfg(test)]
mod tests {
    use super::{event_scan_block_count, interop_cursor_is_caught_up};

    #[test]
    fn event_scan_block_count_is_inclusive() {
        assert_eq!(event_scan_block_count(11, 10).unwrap(), 0);
        assert_eq!(event_scan_block_count(10, 10).unwrap(), 1);
        assert_eq!(event_scan_block_count(10, 12).unwrap(), 3);
    }

    #[test]
    fn event_scan_block_count_rejects_stale_latest_height() {
        let err = event_scan_block_count(11, 5).unwrap_err();
        assert!(
            err.to_string().contains("behind event scan start block"),
            "{err}"
        );
    }

    #[test]
    fn next_interop_cursor_accepts_caught_up_tip_and_rejects_skipped_ids() {
        assert!(!interop_cursor_is_caught_up(7, 7).unwrap());
        assert!(interop_cursor_is_caught_up(8, 7).unwrap());
        assert!(
            interop_cursor_is_caught_up(9, 7)
                .unwrap_err()
                .to_string()
                .contains("more than one ahead")
        );
        assert!(!interop_cursor_is_caught_up(u64::MAX, u64::MAX).unwrap());
    }
}
