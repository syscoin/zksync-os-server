use alloy::eips::Encodable2718;
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use std::time::Duration;
use zksync_os_integration_tests::Tester;

/// After `max_blocks_to_produce` blocks are produced, the node signals `BlockProductionDisabled`.
/// `eth_sendRawTransaction` must then return `-32003` with a structured `data` field containing
/// `{"reason":"block_production_disabled"}` — NOT `-32603` (internal error).
///
/// The block executor only advances when mineable transactions are present, so we submit
/// confirmed transactions to consume all allowed Produce commands, then verify the rejection.
#[test_log::test(tokio::test)]
async fn block_production_disabled_returns_32003_with_structured_data() -> anyhow::Result<()> {
    // 5 Produce commands allowed. L1 priority transactions in build() will consume 1-2 of them;
    // we fill the rest with explicit setup transfers.
    const LIMIT: u64 = 5;

    let tester = Tester::builder()
        .max_blocks_to_produce(LIMIT)
        .block_time(Duration::from_millis(200))
        .build()
        .await?;

    let alice = tester.l2_wallet.default_signer().address();

    // Keep submitting single-tx-per-block transfers until the node enters NotAccepting.
    // We submit one at a time (waiting for confirmation) so each tx drives exactly one
    // Produce command. Once we see a rejection, we know NotAccepting is active.
    let mut nonce = tester.l2_provider.get_transaction_count(alice).await?;
    let gas_price = tester.l2_provider.get_gas_price().await?;
    let chain_id = tester.l2_provider.get_chain_id().await?;

    let build_tx = |nonce: u64| {
        TransactionRequest::default()
            .with_to(Address::random())
            .with_value(U256::from(1))
            .with_nonce(nonce)
            .with_gas_price(gas_price)
            .with_gas_limit(21_000)
            .with_chain_id(chain_id)
    };

    // Submit setup txs one at a time. Each one mines a block and increments the counter.
    // Once the limit is reached the executor hangs and the next send returns -32003.
    let max_setup_txs = LIMIT + 2; // extra headroom in case L1 deposits didn't consume any slots
    let rejection = 'drain: {
        for _ in 0..max_setup_txs {
            let encoded = build_tx(nonce)
                .build(&tester.l2_wallet)
                .await?
                .encoded_2718();

            match tester.l2_provider.send_raw_transaction(&encoded).await {
                Ok(pending) => {
                    // Accepted — mine this block, then loop.
                    pending.get_receipt().await?;
                    nonce += 1;
                }
                Err(e) => {
                    // Node is now rejecting — capture the error and break.
                    break 'drain e;
                }
            }
        }

        // If we exhausted the loop without a rejection, one more send should get it.
        // The limit fires on the (LIMIT+1)-th Produce command, which is triggered by this tx.
        // Wait one block-time for the state to propagate after the last mined block.
        tokio::time::sleep(Duration::from_millis(400)).await;

        let encoded = build_tx(nonce)
            .build(&tester.l2_wallet)
            .await?
            .encoded_2718();
        tester
            .l2_provider
            .send_raw_transaction(&encoded)
            .await
            .expect_err("node should be in NotAccepting state after exhausting the block limit")
    };

    let payload = rejection
        .as_error_resp()
        .expect("expected a JSON-RPC error response, not a transport failure");

    assert_eq!(
        payload.code, -32003,
        "expected -32003 TransactionRejected, got {}",
        payload.code
    );

    let data_raw = payload
        .data
        .as_ref()
        .expect("expected structured data field in -32003 response");
    let data: serde_json::Value =
        serde_json::from_str(data_raw.get()).expect("data field should be valid JSON");

    assert_eq!(
        data["reason"], "block_production_disabled",
        "unexpected reason: {}",
        data["reason"]
    );
    assert!(
        data.get("retry_after_ms").is_none(),
        "block_production_disabled should carry no retry hint, got: {:?}",
        data.get("retry_after_ms")
    );

    tracing::info!(%data, "confirmed: -32003 with structured data after block limit");

    Ok(())
}
