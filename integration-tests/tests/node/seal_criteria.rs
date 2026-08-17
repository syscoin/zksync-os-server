use alloy::network::{ReceiptResponse, TxSigner};
use alloy::primitives::{Address, B256, U256, address};
use alloy::providers::{PendingTransactionBuilder, Provider};
use alloy::rpc::types::{TransactionReceipt, TransactionRequest};
use alloy::sol_types::SolCall;
use anyhow::Context;
use std::time::Duration;
use zksync_os_alloy_ext::provider::ZksyncApi;
use zksync_os_contract_interface::Bridgehub;
use zksync_os_contract_interface::IMailbox::NewPriorityRequest;
use zksync_os_integration_tests::assert_traits::{POLL_INTERVAL, ReceiptAssert, ReceiptsAssert};
use zksync_os_integration_tests::{CURRENT_TO_L1, TestEnvironment, Tester, test_multisetup};
use zksync_os_types::REQUIRED_L1_TO_L2_GAS_PER_PUBDATA_BYTE;

const L1_MESSENGER_ADDRESS: Address = address!("0000000000000000000000000000000000008008");

alloy::sol! {
    function sendToL1(bytes _message);
}

/// Two L1 priority txs whose combined pubdata exceeds `block_pubdata_limit_bytes` must not
/// crash the node: the block must seal on the pubdata limit and the second tx must be
/// retried in the next block (L1 priority txs are strict FIFO and cannot be skipped).
#[test_multisetup([CURRENT_TO_L1])]
async fn l1_txs_exceeding_block_pubdata_limit(env: TestEnvironment) -> anyhow::Result<()> {
    let mut config = env.default_config().await?;
    // SYSCOIN: Keep the upstream test payload below L1's per-priority-tx gas cap even though
    // production uses a larger block pubdata limit for compact Bitcoin DA.
    config.sequencer_config.block_pubdata_limit_bytes = 110_000;
    // Pubdata must be the only seal criterion that can fire: two ~55M-gas priority txs would
    // trip the default 100M block gas limit before ever reaching the pubdata check.
    config.sequencer_config.block_gas_limit = 300_000_000;
    // Wide block window so both priority txs are attempted within the same block.
    config.sequencer_config.block_time = Duration::from_secs(2);
    let tester = env.launch(config).await?;

    let pubdata_limit = tester.config().sequencer_config.block_pubdata_limit_bytes;
    // Each tx fits in an empty block on its own; two together breach the limit.
    let message_size = (pubdata_limit * 3 / 5) as usize;
    let request = prepare_priority_tx_with_pubdata(&tester, message_size).await?;
    let alice = tester.l1_wallet().default_signer().address();
    let first_nonce = tester
        .l1_provider()
        .get_transaction_count(alice)
        .pending()
        .await?;
    // Queue both deposits before awaiting either receipt so the node observes both within the
    // same block window and must exercise the pubdata sealing criterion.
    let (pending1, pending2) = tokio::try_join!(
        tester
            .l1_provider()
            .send_transaction(request.clone().nonce(first_nonce)),
        tester
            .l1_provider()
            .send_transaction(request.nonce(first_nonce + 1)),
    )?;
    let receipts = vec![pending1, pending2]
        .expect_successful_receipts()
        .await?;
    let tx1 = priority_tx_hash(&receipts[0])?;
    let tx2 = priority_tx_hash(&receipts[1])?;

    let block1 = wait_for_l2_inclusion(&tester, tx1).await?;
    let block2 = wait_for_l2_inclusion(&tester, tx2).await?;
    assert!(
        block2 > block1,
        "expected the pubdata limit to push the second priority tx into a later block, \
         but txs landed in blocks {block1} and {block2}"
    );

    Ok(())
}

/// Prepares an L1 priority tx calling `L1Messenger.sendToL1` with a `message_size`-byte
/// message, producing roughly that much pubdata on L2.
async fn prepare_priority_tx_with_pubdata(
    tester: &Tester,
    message_size: usize,
) -> anyhow::Result<TransactionRequest> {
    let alice = tester.l1_wallet().default_signer().address();
    let chain_id = tester.l2_provider.get_chain_id().await?;
    let bridgehub = Bridgehub::new(
        tester.l2_zk_provider.get_bridgehub_contract().await?,
        tester.l1_provider().clone(),
        chain_id,
    );

    let calldata = sendToL1Call {
        _message: vec![0xab; message_size].into(),
    }
    .abi_encode();
    // Pubdata is charged at `REQUIRED_L1_TO_L2_GAS_PER_PUBDATA_BYTE`; the margin covers
    // execution. Must stay under L1's `priorityTxMaxGasLimit` (72M).
    let gas_limit = message_size as u64 * REQUIRED_L1_TO_L2_GAS_PER_PUBDATA_BYTE + 10_000_000;

    let fees = tester.l1_provider().estimate_eip1559_fees().await?;
    let base_cost = bridgehub
        .l2_transaction_base_cost(
            fees.max_fee_per_gas,
            gas_limit,
            REQUIRED_L1_TO_L2_GAS_PER_PUBDATA_BYTE,
        )
        .await?;
    Ok(bridgehub
        .request_l2_transaction_direct(
            base_cost,
            L1_MESSENGER_ADDRESS,
            U256::ZERO,
            calldata,
            gas_limit,
            REQUIRED_L1_TO_L2_GAS_PER_PUBDATA_BYTE,
            alice,
        )
        .value(base_cost)
        .max_fee_per_gas(fees.max_fee_per_gas)
        .max_priority_fee_per_gas(fees.max_priority_fee_per_gas)
        .into_transaction_request())
}

fn priority_tx_hash(receipt: &TransactionReceipt) -> anyhow::Result<B256> {
    Ok(receipt
        .logs()
        .iter()
        .find_map(|log| log.log_decode::<NewPriorityRequest>().ok())
        .context("no NewPriorityRequest log in deposit receipt")?
        .inner
        .txHash)
}

/// Awaits the L2 receipt, failing fast if the node dies first — the pre-fix behavior this
/// test guards against. Returns the inclusion block number.
async fn wait_for_l2_inclusion(tester: &Tester, tx_hash: B256) -> anyhow::Result<u64> {
    let receipt_fut = PendingTransactionBuilder::new(tester.l2_zk_provider.root().clone(), tx_hash)
        .expect_successful_receipt();
    let crash_fut = async {
        while !tester.has_crashed() {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    };
    tokio::select! {
        receipt = receipt_fut => receipt?
            .block_number()
            .context("receipt is missing block number"),
        _ = crash_fut => anyhow::bail!(
            "node crashed while waiting for L1 priority tx {tx_hash} to be included"
        ),
    }
}
