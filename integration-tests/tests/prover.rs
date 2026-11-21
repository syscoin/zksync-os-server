#![cfg(feature = "prover-tests")]

use alloy::network::{ReceiptResponse, TransactionBuilder};
use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use anyhow::{Context, ensure};
use std::time::{Duration, Instant};
use zksync_os_alloy_ext::provider::ZksyncApi;
use zksync_os_integration_tests::Tester;
use zksync_os_integration_tests::assert_traits::ReceiptAssert;
use zksync_os_integration_tests::canonical_restore::CanonicalTestStack;

struct SealedProbe {
    block: u64,
    sealed_through_batch: u64,
}

async fn seal_probe_batch(tester: &Tester) -> anyhow::Result<SealedProbe> {
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
    let block = receipt
        .block_number()
        .context("probe receipt has no block number")?;
    // zks_getBatchByBlockNumber reads the settlement persistence watcher, which cannot report
    // new work before a prover is attached. Use the actual local handoff into the proof pipeline.
    loop {
        let status = tester.pipeline_status().await?;
        let batcher = status
            .as_array()
            .and_then(|parts| parts.iter().find(|part| part["name"] == "batcher"))
            .context("missing canonical batcher pipeline status")?;
        let progress = &batcher["processed"];
        if progress["block_number"]
            .as_u64()
            .is_some_and(|processed| processed >= block)
        {
            let batch = progress["batch_number"]
                .as_u64()
                .context("missing sealed batch number")?;
            return Ok(SealedProbe {
                block,
                sealed_through_batch: batch,
            });
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[test_log::test(tokio::test)]
async fn canonical_restore_smoke() -> anyhow::Result<()> {
    let mut stack = CanonicalTestStack::start().await?;
    let probe = tokio::time::timeout(Duration::from_secs(180), async {
        let gateway = stack.gateway();
        let baseline = gateway.l2_zk_provider.get_batch_number().await?;
        let probe = seal_probe_batch(gateway).await?;
        ensure!(
            probe.sealed_through_batch > baseline,
            "CPU probe did not seal a fresh Gateway batch"
        );
        stack.verify_gateway_da(probe.sealed_through_batch).await?;
        stack.ensure_running()
    })
    .await
    .context("canonical CPU probe or native DA confirmation timed out")
    .and_then(|result| result);
    let cleanup = stack.shutdown().await;
    probe.and(cleanup)
}

#[test_log::test(tokio::test)]
async fn prover() -> anyhow::Result<()> {
    let proving_timeout = std::env::var("SYSCOIN_V8_PROVING_TIMEOUT_SECS")
        .ok()
        .map(|value| value.parse::<u64>())
        .transpose()?
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(4 * 3600));
    let mut stack = CanonicalTestStack::start().await?;
    let result = prove_fresh_gateway_batches(&stack, proving_timeout).await;
    let liveness = stack.ensure_running();
    let cleanup = stack.shutdown().await;
    result.and(liveness).and(cleanup)
}

async fn prove_fresh_gateway_batches(
    stack: &CanonicalTestStack,
    proving_timeout: Duration,
) -> anyhow::Result<()> {
    let tester = stack.gateway();
    let deadline = Instant::now()
        .checked_add(proving_timeout)
        .context("SYSCOIN_V8_PROVING_TIMEOUT_SECS exceeds the monotonic clock range")?;
    // Probe batches must postdate both the local and on-chain proof frontiers restored from disk.
    let (first_settlement_block, baseline, first_probe, second_probe) =
        tokio::time::timeout_at(deadline.into(), async {
            let first_settlement_block = tester
                .l1_provider()
                .get_block_number()
                .await?
                .checked_add(1)
                .context("NEVM block number overflow")?;
            let baseline = tester
                .l2_zk_provider
                .get_batch_number()
                .await?
                .max(tester.prover_tester.last_proven_batch().await?);
            let first_probe = seal_probe_batch(tester).await?;
            ensure!(
                first_probe.sealed_through_batch > baseline,
                "first probe was not in a fresh Gateway batch"
            );
            let second_probe = seal_probe_batch(tester).await?;
            ensure!(
                second_probe.block > first_probe.block
                    && second_probe.sealed_through_batch > first_probe.sealed_through_batch,
                "second probe was not in a new Gateway batch"
            );
            Ok::<_, anyhow::Error>((first_settlement_block, baseline, first_probe, second_probe))
        })
        .await
        .context("timed out sealing fresh canonical Gateway batches")??;
    let mut worker = stack.start_gateway_worker()?;
    let target_batch = second_probe.sealed_through_batch;

    let proving = tokio::time::timeout_at(deadline.into(), async {
        let remaining = deadline.saturating_duration_since(Instant::now());
        tester
            .prover_tester
            .wait_for_batch_proven_from_block(target_batch, first_settlement_block, remaining)
            .await?;
        let first_batch = tester
            .l2_zk_provider
            .wait_batch_number_by_block_number(first_probe.block)
            .await?;
        let second_batch = tester
            .l2_zk_provider
            .wait_batch_number_by_block_number(second_probe.block)
            .await?;
        ensure!(
            first_batch > baseline && second_batch > first_batch && second_batch <= target_batch,
            "the submitted proof did not cover two distinct fresh probe batches"
        );
        Ok::<_, anyhow::Error>(())
    });
    tokio::pin!(proving);
    tokio::select! {
        biased;
        result = &mut proving => {
            let cleanup = stop_worker(&mut worker).await;
            result.with_context(|| format!("full V8 proving pipeline exceeded {proving_timeout:?}"))?
                .and(cleanup)?;
        }
        status = worker.wait() => {
            let status = status.context("failed to wait for the proof worker")?;
            anyhow::bail!("combined prover exited before the fresh Gateway proof was observed: {status}");
        }
    }
    tracing::info!(
        target_batch,
        "fresh app-bound Gateway V8 proof settled on the active NEVM verifier"
    );
    Ok(())
}

async fn stop_worker(worker: &mut tokio::process::Child) -> anyhow::Result<()> {
    if let Some(lifetime) = worker.stdin.take() {
        // The SSH launcher reaps its remote worker before acknowledging local EOF.
        drop(lifetime);
        let stopped = tokio::time::timeout(Duration::from_secs(45), worker.wait()).await;
        if let Ok(status) = stopped {
            let status = status.context("failed to reap the CUDA worker launcher")?;
            ensure!(
                status.success(),
                "CUDA worker launcher shutdown failed: {status}"
            );
            return Ok(());
        }
        worker
            .kill()
            .await
            .context("could not stop the CUDA worker launcher after timeout")?;
        anyhow::bail!("CUDA worker launcher did not acknowledge shutdown");
    }
    worker
        .kill()
        .await
        .context("failed to stop and reap the proof worker")
}
