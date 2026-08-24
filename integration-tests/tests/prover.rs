#![cfg(feature = "prover-tests")]

use zksync_os_integration_tests::{CURRENT_TO_L1, TestEnvironment, test_multisetup};

// SYSCOIN: A fresh production deployment proves only the canonical V32/V8 lane; the retired
// V30 fixture must not silently reintroduce a legacy proving graph into this launch gate.
#[test_multisetup([CURRENT_TO_L1])]
async fn prover(env: TestEnvironment) -> anyhow::Result<()> {
    // Test that prover can successfully prove at least one batch
    let mut config = env.default_config().await?;
    config.prover_api_config.fake_fri_provers.enabled = false;
    config.prover_api_config.fake_snark_provers.enabled = false;
    let tester = env.launch(config).await?;

    // Test environment comes with some L1 transactions by default, so one batch should be provable
    // without any new transactions inside the test.
    tester.prover_tester.wait_for_batch_proven(1).await?;

    // todo: consider expanding this test to prove multiple batches on top of the first batch
    //       also to test L2 transactions are provable too

    Ok(())
}
