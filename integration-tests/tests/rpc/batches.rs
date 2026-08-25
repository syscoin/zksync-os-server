use alloy::providers::Provider;
use zksync_os_alloy_ext::provider::ZksyncApi;
use zksync_os_integration_tests::assert_traits::ReceiptAssert;
use zksync_os_integration_tests::contracts::Counter;
use zksync_os_integration_tests::{CURRENT_TO_L1, Tester, test_multisetup};
use zksync_os_storage_api::PersistedBatch;

#[test_multisetup([CURRENT_TO_L1])]
async fn enumerate_batches(tester: Tester) -> anyhow::Result<()> {
    // SYSCOIN: The stable batch RPC contract starts at canonical genesis: the reported latest
    // batch must exist, and both genesis lookup dimensions must resolve the same real record.
    let startup_latest = tester.l2_zk_provider.get_batch_number().await?;
    let latest_at_startup: Option<PersistedBatch> = tester
        .l2_zk_provider
        .client()
        .request("zks_getBatchByNumber", (startup_latest,))
        .await?;
    assert_eq!(
        latest_at_startup.as_ref().map(|batch| batch.number()),
        Some(startup_latest),
        "zks_batchNumber must identify an available persisted batch"
    );
    let genesis_by_number: Option<PersistedBatch> = tester
        .l2_zk_provider
        .client()
        .request("zks_getBatchByNumber", (0_u64,))
        .await?;
    let genesis_by_block: Option<PersistedBatch> = tester
        .l2_zk_provider
        .client()
        .request("zks_getBatchByBlockNumber", (0_u64,))
        .await?;
    let genesis = genesis_by_number.expect("canonical genesis must be available by batch number");
    assert_eq!(genesis.number(), 0);
    assert_eq!(genesis.block_range, 0..=0);
    assert_eq!(genesis_by_block, Some(genesis));

    let deploy_block_number = Counter::deploy_builder(tester.l2_provider.clone())
        .send()
        .await?
        .expect_successful_receipt()
        .await?
        .block_number
        .expect("no block for successful receipt");

    // Resolves only once the batch containing the deploy block is finalized on L1.
    let batch_number = tester
        .l2_zk_provider
        .wait_batch_number_by_block_number(deploy_block_number)
        .await?;

    let latest_batch_number = tester.l2_zk_provider.get_batch_number().await?;
    assert!(
        latest_batch_number >= batch_number,
        "latest batch {latest_batch_number} is behind finalized batch {batch_number}"
    );

    let batch: Option<PersistedBatch> = tester
        .l2_zk_provider
        .client()
        .request("zks_getBatchByNumber", (batch_number,))
        .await?;
    let batch = batch.expect("batch should be available by its number");
    assert_eq!(batch.number(), batch_number);
    assert!(
        batch.block_range.contains(&deploy_block_number),
        "batch {batch_number} block range {:?} does not contain deploy block {deploy_block_number}",
        batch.block_range
    );

    let missing: Option<PersistedBatch> = tester
        .l2_zk_provider
        .client()
        .request("zks_getBatchByNumber", (latest_batch_number + 1_000_000,))
        .await?;
    assert!(missing.is_none(), "out-of-range batch should not exist");

    Ok(())
}
