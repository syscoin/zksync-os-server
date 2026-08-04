use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use std::time::Duration;
use zksync_os_integration_tests::assert_traits::{DEFAULT_TIMEOUT, ReceiptAssert};
use zksync_os_integration_tests::l1_helpers::fetch_l1_state;
use zksync_os_integration_tests::provider::ZksyncTestingProvider;
use zksync_os_integration_tests::{CURRENT_TO_L1, TestEnvironment, test_multisetup};

const TRANSACTIONS_TO_SEND_BEFORE_RESTART: usize = 5;

/// Verifies that a node with delayed batch sealing can be restarted in normal mode and will
/// commit all previously-accumulated blocks to L1.
///
/// Scenario:
///   1. Start with a long batch timeout — blocks execute and are stored locally but nothing is
///      sealed or submitted to L1.
///   2. Mine several blocks and confirm that L1 commitment count did not move.
///   3. Restart with the normal short batch timeout.
///   4. Wait for the last pre-restart block to be finalized (= executed on L1), proving the
///      node settled all pending blocks after restarting the batch pipeline.
#[test_multisetup([CURRENT_TO_L1])]
#[test_runtime(flavor = "multi_thread")]
async fn uncommitted_blocks_are_settled_after_restart(env: TestEnvironment) -> anyhow::Result<()> {
    let mut config = env.default_config().await?;
    config.batcher_config.batch_timeout = Duration::from_secs(60 * 60);
    config.sequencer_config.block_time = Duration::from_millis(50);
    let tester = env.launch(config).await?;

    let initial_committed = fetch_l1_state(&tester).await?.last_committed_batch;

    // Mine several blocks while the batcher is off.
    for _ in 0..TRANSACTIONS_TO_SEND_BEFORE_RESTART {
        tester
            .l2_provider
            .send_transaction(
                TransactionRequest::default()
                    .with_to(Address::random())
                    .with_value(U256::from(1u64)),
            )
            .await?
            .expect_successful_receipt()
            .await?;
    }
    let last_pre_restart_block = tester.l2_provider.get_block_number().await?;

    // No batch reached a seal criterion — the committed batch count must not have changed.
    let committed_before_restart = fetch_l1_state(&tester).await?.last_committed_batch;
    assert_eq!(
        committed_before_restart, initial_committed,
        "no new batches should be committed before the delayed batch is sealed"
    );

    // Plain restart preserves config, so explicitly restore a short seal timeout.
    let mut restarted_config = tester.config().clone();
    restarted_config.batcher_config.batch_timeout = Duration::from_millis(100);
    let restarted = tester.restart_with_config(restarted_config).await?;

    // The restarted node must pick up all pending uncommitted blocks and settle them on L1.
    // Wait until the last pre-restart block is finalized (= executed on L1).
    restarted
        .l2_zk_provider
        .wait_finalized_with_timeout(last_pre_restart_block, DEFAULT_TIMEOUT)
        .await?;

    // Confirm via L1 state that new batches were actually committed and executed.
    let l1_state_after = fetch_l1_state(&restarted).await?;
    assert!(
        l1_state_after.last_committed_batch > initial_committed,
        "expected new batches to be committed after restarting the batch pipeline, \
         but committed batch count did not increase ({initial_committed} -> {})",
        l1_state_after.last_committed_batch,
    );

    Ok(())
}
