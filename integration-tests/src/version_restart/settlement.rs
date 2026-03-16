use crate::assert_traits::{DEFAULT_TIMEOUT, ReceiptAssert};
use crate::dyn_wallet_provider::EthDynProvider;
use crate::provider::{ZksyncApi, ZksyncTestingProvider};
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use anyhow::Context;
use backon::{ConstantBuilder, Retryable};
use std::time::Duration;
use zksync_os_contract_interface::l1_discovery::L1State;

use super::{server::ConnectedProviders, MAX_PHASE_TXS};

pub(crate) async fn settle_new_batches(
    l1_provider: &EthDynProvider,
    providers: &ConnectedProviders,
    start_batch: u64,
    new_batches: u64,
) -> anyhow::Result<u64> {
    let target_batch = start_batch + new_batches;
    for _ in 0..MAX_PHASE_TXS {
        let current_batch = latest_executed_batch(l1_provider, providers).await?;
        if current_batch >= target_batch {
            return Ok(current_batch);
        }

        let to = Address::random();
        let receipt = providers
            .ethereum
            .send_transaction(
                TransactionRequest::default()
                    .with_to(to)
                    .with_value(U256::from(1u64)),
            )
            .await?
            .expect_to_execute()
            .await?;
        let block_number = receipt
            .block_number
            .context("receipt is missing block number after execution")?;
        providers
            .zksync
            .wait_finalized_with_timeout(block_number, DEFAULT_TIMEOUT)
            .await?;
    }

    let current_batch = latest_executed_batch(l1_provider, providers).await?;
    anyhow::ensure!(
        current_batch >= target_batch,
        "expected finalized batch to reach {target_batch}, got {current_batch}"
    );
    Ok(current_batch)
}

pub(crate) async fn wait_for_rich_l2_balance(
    providers: &ConnectedProviders,
    address: Address,
) -> anyhow::Result<()> {
    (|| async {
        let balance = providers.ethereum.get_balance(address).await?;
        if balance == U256::ZERO {
            anyhow::bail!("L2 rich wallet balance is zero")
        }
        Ok(())
    })
    .retry(
        ConstantBuilder::default()
            .with_delay(Duration::from_millis(200))
            .with_max_times(50),
    )
    .notify(|err: &anyhow::Error, dur: Duration| {
        tracing::info!(%err, ?dur, %address, "waiting for L2 account to become rich");
    })
    .await
}

async fn latest_executed_batch(
    l1_provider: &EthDynProvider,
    providers: &ConnectedProviders,
) -> anyhow::Result<u64> {
    let bridgehub = providers.zksync.get_bridgehub_contract().await?;
    let chain_id = providers.ethereum.get_chain_id().await?;
    let l1_state = L1State::fetch(
        l1_provider.clone().erased(),
        l1_provider.clone().erased(),
        bridgehub,
        chain_id,
    )
    .await?;
    Ok(l1_state.last_executed_batch)
}
