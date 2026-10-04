use crate::Tester;
use crate::assert_traits::{DEFAULT_TIMEOUT, POLL_INTERVAL};
use alloy::eips::BlockId;
use alloy::providers::Provider;
use anyhow::Context;
use zksync_os_alloy_ext::provider::ZksyncApi;
use zksync_os_contract_interface::l1_discovery::L1State;
use zksync_os_l1_watcher::fetch_live_committed_batch;

/// Fetches the current L1 state from the given tester.
pub async fn fetch_l1_state(tester: &Tester) -> anyhow::Result<L1State> {
    let chain_id = tester.l2_provider.get_chain_id().await?;
    let bridgehub_address = tester.l2_zk_provider.get_bridgehub_contract().await?;
    L1State::fetch(
        tester.l1_provider().clone(),
        tester.gateway_eth_provider(),
        bridgehub_address,
        chain_id,
    )
    .await
}

/// Drains already-proved fixture batches before tests freeze commit-only settlement frontiers.
/// Delaying fake SNARKs prevents new proofs, but does not prevent execution or finalization of
/// batches proved before launch. The RPC watcher must also catch up before its tag is snapshotted.
pub async fn wait_for_commit_only_baseline(tester: &Tester) -> anyhow::Result<L1State> {
    tokio::time::timeout(DEFAULT_TIMEOUT, async {
        let initial_state = fetch_l1_state(tester).await?;
        let proved_frontier = initial_state.last_proved_batch;
        let expected_finalized_block = if proved_frontier == 0 {
            0
        } else {
            fetch_live_committed_batch(
                &initial_state.diamond_proxy_sl,
                proved_frontier,
                tester.config().l1_watcher_config.max_blocks_to_process,
            )
            .await?
            .0
            .last_block_number()
        };

        loop {
            anyhow::ensure!(
                !tester.has_crashed(),
                "node crashed while draining commit-only fixture settlement",
            );
            let state = fetch_l1_state(tester).await?;
            if commit_only_frontiers_ready(
                proved_frontier,
                state.last_proved_batch,
                state.last_executed_batch,
                state.last_finalized_executed_batch,
            )? {
                let rpc_finalized_block = tester
                    .l2_provider
                    .get_block_number_by_id(BlockId::finalized())
                    .await?;
                if commit_only_rpc_finalized_ready(expected_finalized_block, rpc_finalized_block)? {
                    return Ok(state);
                }
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .context("timed out draining commit-only fixture execution, finality, and RPC finalized tag")?
}

fn commit_only_frontiers_ready(
    proved_frontier: u64,
    last_proved_batch: u64,
    last_executed_batch: u64,
    last_finalized_executed_batch: u64,
) -> anyhow::Result<bool> {
    anyhow::ensure!(
        last_proved_batch == proved_frontier,
        "commit-only proved frontier changed while draining fixture settlement: expected {proved_frontier}, got {last_proved_batch}",
    );
    anyhow::ensure!(
        last_executed_batch <= proved_frontier,
        "commit-only execution exceeded the original proved frontier: {last_executed_batch} > {proved_frontier}",
    );
    anyhow::ensure!(
        last_finalized_executed_batch <= last_executed_batch,
        "finalized execution exceeded the executed frontier: {last_finalized_executed_batch} > {last_executed_batch}",
    );
    Ok(last_executed_batch == proved_frontier && last_finalized_executed_batch == proved_frontier)
}

fn commit_only_rpc_finalized_ready(
    expected_block: u64,
    actual_block: Option<u64>,
) -> anyhow::Result<bool> {
    anyhow::ensure!(
        actual_block.is_none_or(|block| block <= expected_block),
        "RPC finalized block exceeded the commit-only fixture frontier: expected {expected_block}, got {actual_block:?}",
    );
    Ok(actual_block == Some(expected_block))
}

/// Polls the L1 state until a predicate is satisfied or timeout is reached.
///
/// Uses the global `DEFAULT_TIMEOUT` and `POLL_INTERVAL` for polling parameters.
pub async fn wait_for_l1_state(
    tester: &Tester,
    description: &str,
    predicate: impl Fn(&L1State) -> bool,
) -> anyhow::Result<L1State> {
    let deadline = std::time::Instant::now() + DEFAULT_TIMEOUT;
    let mut last_err: Option<anyhow::Error> = None;
    loop {
        // The L1 state lives on anvil, so a dead node would otherwise burn the whole timeout
        // and report an unhelpful "waiting for ..." error; fail fast with the real cause.
        anyhow::ensure!(
            !tester.has_crashed(),
            "node crashed while waiting for L1 state: {description}",
        );
        match fetch_l1_state(tester).await {
            Ok(state) if predicate(&state) => return Ok(state),
            Ok(_) => {}
            Err(err) => last_err = Some(err),
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "timed out waiting for L1 state: {description} (last fetch error: {last_err:?})",
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{commit_only_frontiers_ready, commit_only_rpc_finalized_ready};

    #[test]
    fn commit_only_baseline_waits_for_fixture_execution_and_finality() {
        for (executed, finalized, ready) in [(71, 70, false), (72, 71, false), (72, 72, true)] {
            assert_eq!(
                commit_only_frontiers_ready(72, 72, executed, finalized).unwrap(),
                ready,
            );
        }
        assert!(commit_only_frontiers_ready(0, 0, 0, 0).unwrap());
    }

    #[test]
    fn commit_only_baseline_never_chases_a_changed_proved_frontier() {
        for proved in [71, 73] {
            let err = commit_only_frontiers_ready(72, proved, proved, proved).unwrap_err();
            assert!(err.to_string().contains("proved frontier changed"));
        }
        assert!(commit_only_frontiers_ready(72, 72, 73, 72).is_err());
        assert!(commit_only_frontiers_ready(72, 72, 71, 72).is_err());
    }

    #[test]
    fn commit_only_baseline_waits_for_the_exact_rpc_finalized_block() {
        assert!(!commit_only_rpc_finalized_ready(139, None).unwrap());
        assert!(!commit_only_rpc_finalized_ready(139, Some(137)).unwrap());
        assert!(commit_only_rpc_finalized_ready(139, Some(139)).unwrap());
        assert!(commit_only_rpc_finalized_ready(139, Some(140)).is_err());
        assert!(!commit_only_rpc_finalized_ready(0, None).unwrap());
        assert!(commit_only_rpc_finalized_ready(0, Some(0)).unwrap());
    }
}
