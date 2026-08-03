use crate::watcher::L1WatcherError;
use alloy::consensus::Transaction;
use alloy::primitives::{Address, B256, BlockNumber, Log, TxHash, U256};
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
use zksync_os_contract_interface::{
    Bridgehub, Error as ContractInterfaceError, IExecutor, MessageRoot, ZkChain,
};
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
    let latest_migration_number = match instance
        .migrationNumber(U256::from(chain_id))
        .block(latest.into())
        .call()
        .await
    {
        Ok(n) => n,
        // Pre-V31 `ChainAssetHandler` does not expose `migrationNumber`. No Gateway migrations can
        // exist in that era, so there is nothing to scan for — start from the latest block.
        Err(err) if is_method_missing(&err) => return Ok(latest),
        Err(err) => return Err(err.into()),
    };
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
/// Postcondition relied upon by callers: `predicate(result) == true`, and either
/// `predicate(result - 1) == false` or `result == start_block_number`.
///
/// **Caller must ensure `start_block_number >= contract.deployment_block`** — the predicate is
/// invoked without a code-presence guard, so calling it at blocks where the contract is not yet
/// deployed will produce undefined results (typically an RPC error or a `false`-returning revert).
pub async fn find_l1_block_by_predicate<Fut: Future<Output = anyhow::Result<bool>>>(
    provider: &NodeProvider,
    start_block_number: BlockNumber,
    predicate: impl Fn(BlockNumber) -> Fut,
) -> anyhow::Result<BlockNumber> {
    let latest = provider.get_block_number().await?;

    // Ensure the predicate is true by the upper bound, or bail early.
    if !predicate(latest).await? {
        anyhow::bail!(
            "Condition not satisfied up to latest block: contract not deployed yet \
             or target not reached.",
        );
    }

    find_first_true_block(start_block_number, latest, predicate).await
}

/// Core of [`find_l1_block_by_predicate`]: gallop from the tip, then binary-search the bracket.
///
/// Requires `predicate(latest) == true` (checked by the caller). Far-from-tip transitions cost up
/// to ~log2(range / initial distance) extra probes over a plain binary search.
async fn find_first_true_block<Fut: Future<Output = anyhow::Result<bool>>>(
    start_block_number: BlockNumber,
    latest: BlockNumber,
    predicate: impl Fn(BlockNumber) -> Fut,
) -> anyhow::Result<BlockNumber> {
    // Gallop: probe latest-d, latest-2d, latest-4d, ... until the predicate is false or the
    // probe clamps at `start_block_number`.
    let (mut lo, mut hi) = (start_block_number, latest);
    let mut distance = GALLOP_INITIAL_DISTANCE;
    while lo < hi {
        let probe = latest.saturating_sub(distance).max(lo);
        if predicate(probe).await? {
            hi = probe;
            if probe == lo {
                break;
            }
            distance = distance.saturating_mul(2);
        } else {
            lo = probe + 1;
            break;
        }
    }

    // Binary search on [lo, hi] for the first block where the predicate is true.
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if predicate(mid).await? {
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
    let mut logged_next_block_wait = false;

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
            Err(err)
                if latest_block.checked_add(1) == Some(start_block_number)
                    && stale_height_attempts + 1 < STALE_L1_HEIGHT_RETRY_ATTEMPTS =>
            {
                stale_height_attempts += 1;
                if !logged_next_block_wait {
                    tracing::warn!(
                        start_block_number,
                        latest_block,
                        attempt = stale_height_attempts,
                        "event scan cursor is at the next block; waiting for provider tip to advance"
                    );
                    logged_next_block_wait = true;
                } else {
                    tracing::debug!(
                        start_block_number,
                        latest_block,
                        attempt = stale_height_attempts,
                        error = %err,
                        "still waiting for provider tip to advance to event scan start"
                    );
                }
                tokio::time::sleep(STALE_L1_HEIGHT_RETRY_DELAY).await;
            }
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
/// Every commit writes `storedBatchHashes[batch_number] = hash(StoredBatchInfo)`; re-commits
/// overwrite it and reverts do not clear it — hence the guard that the batch is currently
/// committed. Under that guard, `storedBatchHash(batch_number, block) == <live hash>` is a
/// monotonic predicate whose first `true` block is the live commit block, so no `BlocksRevert`
/// scanning is needed.
///
/// Works for batch 0 too: its stored hash is written at contract initialization, so the result
/// is the deployment block (genesis has no commit event or transaction).
pub(crate) async fn find_l1_commit_block_by_batch_number(
    zk_chain: &ZkChain<NodeProvider>,
    batch_number: u64,
) -> anyhow::Result<(BlockNumber, B256)> {
    let latest = zk_chain.provider().get_block_number().await?;
    let total_committed = zk_chain.get_total_batches_committed(latest.into()).await?;
    anyhow::ensure!(
        total_committed >= batch_number,
        "batch {batch_number} is not committed on L1 \
         (batches committed as of block {latest}: {total_committed})",
    );
    let live_hash = zk_chain
        .stored_batch_hash(batch_number, latest.into())
        .await?;

    let deployment_block = zk_chain.deployment_block().await?;
    let live_commit_block = find_l1_block_by_predicate(
        zk_chain.provider(),
        deployment_block,
        move |block| async move {
            Ok(zk_chain
                .stored_batch_hash(batch_number, block.into())
                .await?
                == live_hash)
        },
    )
    .await?;
    Ok((live_commit_block, live_hash))
}

/// Finds first L1 block that contains batch execution event on L1 matching requested batch.
///
/// Returns latest L1 block is there is none.
pub async fn find_l1_execute_block_by_batch_number(
    zk_chain: &ZkChain<NodeProvider>,
    batch_number: u64,
) -> anyhow::Result<BlockNumber> {
    // Execution cannot be reverted, so a plain total-count predicate is safe here, unlike for
    // commits (see `find_l1_commit_block_by_batch_number`).
    let deployment_block = zk_chain.deployment_block().await?;
    find_l1_block_by_predicate(
        zk_chain.provider(),
        deployment_block,
        move |block| async move {
            let res = zk_chain.get_total_batches_executed(block.into()).await?;
            Ok(res >= batch_number)
        },
    )
    .await
}

/// Finds the first L1 block where `interopRootLogId >= next_interop_root_id`.
/// Uses binary search for efficiency.
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

    let latest = message_root.provider().get_block_number().await?;
    // The provider's cache resolves (and remembers) the MessageRoot deployment block, giving the
    // search a tight lower bound without a per-iteration code-existence guard.
    let deployment_block = message_root.deployment_block().await?;

    let predicate =
        async |message_root: Arc<MessageRoot<NodeProvider>>, block: u64| -> anyhow::Result<bool> {
            let res = message_root.interop_root_log_id(block.into()).await?;
            Ok(res >= next_interop_root_id)
        };
    // SYSCOIN
    let latest_result = predicate(message_root.clone(), latest).await;
    let latest_matches_target = match latest_result {
        Ok(latest_matches_target) => latest_matches_target,
        Err(err) if should_fallback_to_genesis_log_scan(&err) => {
            tracing::warn!(
                interop_root_id = next_interop_root_id,
                message_root = ?message_root_address,
                error = %err,
                "MessageRoot.totalPublishedInteropRoots is unavailable; falling back to genesis log scan"
            );
            return Ok(0);
        }
        Err(err) => return Err(err),
    };

    // SYSCOIN
    if !latest_matches_target {
        anyhow::bail!(
            "Condition not satisfied up to latest block: contract not deployed yet \
             or target not reached.",
        );
    }

    let (mut lo, mut hi) = (deployment_block, latest);
    while lo < hi {
        let mid = (lo + hi) / 2;
        // SYSCOIN
        let mid_matches_target = match predicate(message_root.clone(), mid).await {
            Ok(mid_matches_target) => mid_matches_target,
            Err(err) if should_fallback_to_genesis_log_scan(&err) => {
                tracing::warn!(
                    interop_root_id = next_interop_root_id,
                    message_root = ?message_root_address,
                    error = %err,
                    "MessageRoot.totalPublishedInteropRoots became unavailable during binary search; falling back to genesis log scan"
                );
                return Ok(0);
            }
            Err(err) => return Err(err),
        };
        if mid_matches_target {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }

    Ok(lo)
}
// SYSCOIN
fn should_fallback_to_genesis_log_scan(err: &anyhow::Error) -> bool {
    let Some(err) = err.downcast_ref::<ContractInterfaceError>() else {
        return false;
    };
    match err {
        ContractInterfaceError::Call(inner, function_name)
        | ContractInterfaceError::CallAtBlock(inner, function_name, _)
            if function_name == "totalPublishedInteropRoots" =>
        {
            inner.to_string().contains("execution reverted")
        }
        _ => false,
    }
}

/// Fetches and decodes stored batch data for batch `batch_number` that is expected to have been
/// committed in `l1_block_number`, returning it together with the commit transaction hash.
/// Returns `None` if requested batch has not been committed in the given L1 block.
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
    // Take the *last* matching log in the block: if the batch was committed, reverted and
    // re-committed within a single L1 block, only the latest commit is the live one.
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
        .retry(COMMIT_DATA_RETRY_POLICY)
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
    use super::event_scan_block_count;

    #[test]
    fn event_scan_block_count_is_inclusive() {
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
}



