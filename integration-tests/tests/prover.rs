#![cfg(feature = "prover-tests")]

use zksync_os_integration_tests::{
    CURRENT_TO_L1, NEXT_TO_GATEWAY, SettlementLayer, TestCase, TesterBuilder, test_multisetup,
};

// todo: add gateway test once v31 is fully ready.
#[test_multisetup([CURRENT_TO_L1, NEXT_TO_GATEWAY])]
async fn prover(builder: TesterBuilder, test_case: TestCase) -> anyhow::Result<()> {
    // Test that prover can successfully prove at least one batch
    let tester = builder.enable_prover().build().await?;

    if test_case.settlement_layer == SettlementLayer::Gateway {
        // Gateway comes with a pre-baked state and some batches are already fake-proven there.
        // So we expect the next batch to be proven with real flow.
        let last_proven_batch = tester.prover_tester.last_proven_batch().await?;
        // We expect that first supporting node is gateway node.
        // Wait for the first batch to be proven on gateway node as well.
        tester.supporting_nodes()[0]
            .prover_tester
            .wait_for_batch_proven(last_proven_batch + 1)
            .await?;
    }

    // Test environment comes with some L1 transactions by default, so one batch should be provable
    // without any new transactions inside the test.
    tester.prover_tester.wait_for_batch_proven(1).await?;

    // todo: consider expanding this test to prove multiple batches on top of the first batch
    //       also to test L2 transactions are provable too

    Ok(())
}
