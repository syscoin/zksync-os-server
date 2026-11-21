#![cfg(feature = "prover-tests")]

use alloy::network::{ReceiptResponse, TransactionBuilder};
use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use anyhow::Context;
use std::time::{Duration, Instant};
use zksync_os_alloy_ext::provider::ZksyncApi;
use zksync_os_integration_tests::assert_traits::ReceiptAssert;
use zksync_os_integration_tests::{CURRENT_TO_L1, TestEnvironment, Tester, test_multisetup};
use zksync_os_server::config::Config;

async fn seal_probe_batch(tester: &Tester) -> anyhow::Result<u64> {
    let receipt = tester
        .l2_provider
        .send_transaction(
            TransactionRequest::default()
                .with_to(Address::random())
                .with_value(U256::from(1)),
        )
        .await?
        .expect_successful_receipt()
        .await?;
    let block_number = receipt
        .block_number()
        .expect("successful probe transaction has no block number");
    Ok(tester
        .l2_zk_provider
        .wait_batch_number_by_block_number(block_number)
        .await?)
}

async fn configure_prover_test(env: &TestEnvironment) -> anyhow::Result<Config> {
    let mut config = env.default_config().await?;
    anyhow::ensure!(
        config.batcher_config.bitcoin_da_rpc_url.is_some(),
        "canonical prover tests require the restored Syscoin Core DA endpoint"
    );
    // SYSCOIN: Production V32 proving is a single app-bound real FRI+SNARK lane; opt into the
    // external prover API explicitly instead of constructing the prohibited real/fake hybrid.
    config.prover_api_config.enabled = true;
    config.prover_api_config.fake_fri_provers.enabled = false;
    config.prover_api_config.fake_snark_provers.enabled = false;
    config
        .prover_api_config
        .proof_storage
        .batch_with_proof_capacity
        .0 = 8 * 1024 * 1024 * 1024;
    config.prover_api_config.max_fris_per_snark = 2;
    config.prover_api_config.target_fris_per_snark = 2;
    config.prover_api_config.fri_job_timeout = Duration::from_secs(48 * 3600);
    config.prover_api_config.snark_job_timeout = Duration::from_secs(48 * 3600);
    config.batcher_config.batch_timeout = Duration::from_millis(100);
    Ok(config)
}

#[test_multisetup([CURRENT_TO_L1])]
async fn canonical_restore_smoke(env: TestEnvironment) -> anyhow::Result<()> {
    let config = configure_prover_test(&env).await?;
    let tester = env.launch_without_prover(config).await?;
    let probe = tokio::time::timeout(Duration::from_secs(60), async {
        anyhow::ensure!(
            tester.l1_provider().get_chain_id().await? == 31337
                && tester.l2_provider.get_chain_id().await? == 57001,
            "canonical smoke requires Gateway 57001 settling on NEVM 31337"
        );
        seal_probe_batch(&tester).await?;
        Ok::<_, anyhow::Error>(())
    })
    .await;
    tester.shutdown().await?;
    probe.context("canonical CPU startup probe timed out")?
}

// SYSCOIN: A fresh production deployment proves only the canonical V32/V8 lane; the retired
// V30 fixture must not silently reintroduce a legacy proving graph into this launch gate.
#[test_multisetup([CURRENT_TO_L1])]
async fn prover(env: TestEnvironment) -> anyhow::Result<()> {
    // Test that the prover can aggregate and settle the minimum production range.
    let config = configure_prover_test(&env).await?;
    let mut tester = env.launch(config).await?;
    let mut prover_service_task = tester
        .take_prover_service_task()
        .expect("enabled real prover API did not launch the combined prover service");

    let proving_timeout = std::env::var("SYSCOIN_V8_PROVING_TIMEOUT_SECS")
        .ok()
        .map(|value| value.parse::<u64>())
        .transpose()?
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(4 * 3600));
    let deadline = Instant::now().checked_add(proving_timeout).ok_or_else(|| {
        anyhow::anyhow!(
            "SYSCOIN_V8_PROVING_TIMEOUT_SECS exceeds the supported monotonic clock range"
        )
    })?;

    // SYSCOIN: A production SNARK job contains at least two consecutive FRI proofs. Bound batch
    // creation and proving under one deadline, then fail immediately if the combined prover exits
    // unsuccessfully instead of masking an app/VK/CRS/CLI failure behind a multi-hour poll.
    let proving_pipeline = tokio::time::timeout_at(deadline.into(), async {
        let first_settlement_block = tester
            .l1_provider()
            .get_block_number()
            .await?
            .checked_add(1)
            .context("NEVM block number overflow")?;
        let last_proven_batch = tester.prover_tester.last_proven_batch().await?;
        let latest_batch = tester.l2_zk_provider.get_batch_number().await?;
        // Restored batches may already have commitments or queued proof work.
        let first_probe = seal_probe_batch(&tester).await?;
        anyhow::ensure!(
            first_probe > latest_batch.max(last_proven_batch),
            "first probe must land in a new batch beyond the restored frontier"
        );
        let target_batch = seal_probe_batch(&tester).await?;
        anyhow::ensure!(
            target_batch > first_probe,
            "second probe must land in a separate newly sealed batch"
        );

        let remaining = deadline.saturating_duration_since(Instant::now());
        tester
            .prover_tester
            .wait_for_batch_proven_from_block(target_batch, first_settlement_block, remaining)
            .await?;
        Ok::<_, anyhow::Error>(target_batch)
    });
    tokio::pin!(proving_pipeline);

    let result = tokio::select! {
        result = &mut proving_pipeline => {
            // SYSCOIN: A dropped JoinHandle detaches its task. Abort and join the monitor so it
            // drops the owned child and triggers kill_on_drop before the test/tempdir can exit.
            prover_service_task.abort();
            match prover_service_task.await {
                Err(err) if err.is_cancelled() => {}
                Ok(Ok(())) => {}
                Ok(Err(err)) => return Err(err.context("combined prover service failed")),
                Err(err) => {
                    return Err(anyhow::Error::new(err).context("combined prover monitor failed"));
                }
            }
            result
        },
        service_result = &mut prover_service_task => {
            match service_result {
                Ok(Ok(())) => proving_pipeline.await,
                Ok(Err(err)) => return Err(err.context("combined prover service failed")),
                Err(err) => {
                    return Err(anyhow::Error::new(err).context("combined prover monitor failed"));
                }
            }
        }
    };
    let target_batch = result
        .map_err(|_| anyhow::anyhow!("full V8 proving pipeline exceeded {proving_timeout:?}"))??;
    tracing::info!(target_batch, "full app-bound V8 proving pipeline settled");

    Ok(())
}
