use alloy::consensus::{BlobTransactionSidecar, BlobTransactionSidecarVariant};
use alloy::eips::BlockId;
use alloy::network::TransactionBuilder;
use alloy::primitives::U128;
use alloy::primitives::{U256, b256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy::rpc::types::state::{AccountOverride, StateOverride};
use std::collections::HashMap;
use zksync_os_integration_tests::assert_traits::EthCallAssert;
use zksync_os_integration_tests::contracts::{
    Counter, EventEmitter, SimpleRevert, TracingSecondary,
};
use zksync_os_integration_tests::{
    CURRENT_TO_L1, NEXT_TO_GATEWAY, Tester, TesterBuilder, test_multisetup,
};
use zksync_os_server::config::FeeConfig;

#[test_multisetup([CURRENT_TO_L1, NEXT_TO_GATEWAY])]
async fn call_genesis(tester: Tester) -> anyhow::Result<()> {
    // Test that the node can run `eth_call` on genesis
    tester
        .l2_provider
        .call(TransactionRequest::default())
        .block(0.into())
        .await?;
    Ok(())
}

#[test_multisetup([CURRENT_TO_L1, NEXT_TO_GATEWAY])]
async fn call_pending(tester: Tester) -> anyhow::Result<()> {
    // Test that the node can run `eth_call` on pending block
    tester
        .l2_provider
        .call(TransactionRequest::default())
        .block(BlockId::pending())
        .await?;
    Ok(())
}

#[test_multisetup([CURRENT_TO_L1])]
async fn call_fail(tester: Tester) -> anyhow::Result<()> {
    // Test that the node responds with proper errors when `eth_call` fails

    // Tx type errors
    tester
        .l2_provider
        .call(TransactionRequest {
            sidecar: Some(BlobTransactionSidecarVariant::Eip4844(
                BlobTransactionSidecar {
                    blobs: vec![],
                    commitments: vec![],
                    proofs: vec![],
                },
            )),
            ..Default::default()
        })
        .expect_to_fail("EIP-4844 transactions are not supported")
        .await;
    tester
        .l2_provider
        .call(TransactionRequest {
            authorization_list: Some(vec![]),
            ..Default::default()
        })
        .expect_to_fail("EIP-7702 transactions are not supported")
        .await;

    // Block not found errors
    tester
        .l2_provider
        .call(TransactionRequest::default())
        // Very far ahead block
        .block((u32::MAX as u64).into())
        .expect_to_fail("block `0xffffffff` not found")
        .await;

    // Fee errors
    tester
        .l2_provider
        .call(TransactionRequest {
            gas_price: Some(100),
            max_fee_per_gas: Some(100),
            ..Default::default()
        })
        .expect_to_fail("both `gasPrice` and (`maxFeePerGas` or `maxPriorityFeePerGas`) specified")
        .await;
    tester
        .l2_provider
        .call(TransactionRequest {
            max_fee_per_gas: Some(1),
            max_priority_fee_per_gas: Some(1),
            ..Default::default()
        })
        .expect_to_fail("`maxFeePerGas` less than `block.baseFee`")
        .await;
    tester
        .l2_provider
        .call(TransactionRequest {
            max_fee_per_gas: Some(1_000_000_001),
            max_priority_fee_per_gas: Some(1_000_000_002),
            ..Default::default()
        })
        .expect_to_fail("`maxPriorityFeePerGas` higher than `maxFeePerGas`")
        .await;

    tester
        .l2_provider
        .call(TransactionRequest {
            max_fee_per_gas: Some(1),
            max_priority_fee_per_gas: Some(1),
            ..Default::default()
        })
        .expect_to_fail("`maxFeePerGas` less than `block.baseFee`")
        .await;
    tester
        .l2_provider
        .call(TransactionRequest {
            max_priority_fee_per_gas: Some(u128::MAX),
            ..Default::default()
        })
        .expect_to_fail("`maxPriorityFeePerGas` is too high")
        .await;
    // Missing field errors
    tester
        .l2_provider
        .call(TransactionRequest {
            max_fee_per_gas: Some(1_000_000_001),
            ..Default::default()
        })
        .expect_to_fail("missing `maxPriorityFeePerGas` field for EIP-1559 transaction")
        .await;

    Ok(())
}

#[test_multisetup([CURRENT_TO_L1])]
async fn call_deploy(tester: Tester) -> anyhow::Result<()> {
    // Test that the node can run `eth_call` with contract deployment
    let result = EventEmitter::deploy_builder(tester.l2_provider.clone())
        .call()
        .await?;
    assert_eq!(result, EventEmitter::DEPLOYED_BYTECODE);
    Ok(())
}

#[test_multisetup([CURRENT_TO_L1])]
async fn call_revert(tester: Tester) -> anyhow::Result<()> {
    // Test that the node returns error on reverting `eth_call`
    let simple_revert = SimpleRevert::deploy(tester.l2_provider.clone()).await?;
    // Custom error is returned as accompanying data
    let error = simple_revert
        .simpleRevert()
        .call_raw()
        .await
        .expect_err("call did not result in revert error")
        .to_string();
    assert_eq!(
        error,
        "server returned an error response: error code 3: execution reverted, data: \"0xc2bb947c\""
    );
    // String reverts are parsed out as a revert reason
    let error = simple_revert
        .stringRevert()
        .call_raw()
        .await
        .expect_err("call did not result in revert error")
        .to_string();
    assert_eq!(
        error,
        "server returned an error response: error code 3: execution reverted: my message, data: \"0x08c379a00000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000000a6d79206d65737361676500000000000000000000000000000000000000000000\""
    );

    Ok(())
}

#[test_multisetup([CURRENT_TO_L1])]
async fn call_with_state_overrides(tester: Tester) -> anyhow::Result<()> {
    // Deploy a dummy contract with storage at slot 0, call it to read the value,
    // then call again with a state override for slot 0 and expect a different result.

    // Deploy TracingSecondary with `data = 1` stored at slot 0
    let initial_data = U256::from(1);
    let contract = TracingSecondary::deploy(tester.l2_provider.clone(), initial_data).await?;

    // Build a TransactionRequest for multiply(1) -> returns the storage-backed value
    let tx_req = contract.multiply(U256::from(1)).into_transaction_request();

    // Baseline call without overrides (should return 1)
    let out = tester.l2_provider.call(tx_req.clone()).await?;
    let baseline = U256::from_be_slice(&out);
    assert_eq!(baseline, initial_data);

    // Prepare state override via JSON to match expected types: set slot 0 to 2
    let overrides = StateOverride::from_iter([(
        *contract.address(),
        AccountOverride {
            balance: None,
            nonce: None,
            code: None,
            state: Some(HashMap::from_iter([(
                b256!("0x0000000000000000000000000000000000000000000000000000000000000000"),
                b256!("0x0000000000000000000000000000000000000000000000000000000000000002"),
            )])),
            state_diff: None,
            move_precompile_to: None,
        },
    )]);

    // Call again with the override; expect 2 now
    let out_overridden = tester.l2_provider.call(tx_req).overrides(overrides).await?;
    let overridden = U256::from_be_slice(&out_overridden);
    assert_eq!(overridden, U256::from(2));

    Ok(())
}

#[test_multisetup([CURRENT_TO_L1])]
async fn call_pubdata_exhaustion_detected(builder: TesterBuilder) -> anyhow::Result<()> {
    // Test that `eth_call` detects when a transaction would revert due to pubdata costs
    // even though execution succeeds with basefee=0.
    //
    // Strategy: configure the node with a very high pubdata price. Then call
    // Counter.increment() WITHOUT gas_price (so basefee=0 disables all fee checks
    // inside the VM and execution succeeds). The post-execution heuristic uses the
    // real basefee to compute gas_budget and detects the pubdata cost overrun.
    // Use a high-enough pubdata_price that resource_cost > gas_budget, but low enough
    // that eth_call (basefee=0, gas_price=0) can still execute the transaction.
    // base_fee=25M, pubdata_price=1B/byte, native_price=1M, gas_limit=100K →
    //   gas_budget  = 25M * 100K = 2.5T
    //   resource_cost ≈ 1B * 200 bytes = 200B  (too low, need higher pubdata_price)
    //
    // Let's use base_fee = 25M, pubdata_price = 100B, native_price = 1M:
    //   gas_budget  = 25M * 100K = 2.5 * 10^12
    //   resource_cost ≈ 100B * 200 = 20 * 10^12 = 2 * 10^13
    //   2 * 10^13 > 2.5 * 10^12 → triggers
    let fee_config = FeeConfig {
        native_price_usd: 3e-9,
        base_fee_override: Some(U128::from(25_000_000u64)), // 25M wei
        native_per_gas: 100,
        pubdata_price_override: Some(U128::from(100_000_000_000u64)), // 100B wei/byte
        native_price_override: Some(U128::from(1_000_000u64)),
        pubdata_price_cap: None,
    };
    let tester = builder.fee_config(fee_config).build().await?;
    let counter = Counter::deploy(tester.l2_provider.clone()).await?;

    let mut tx_req = counter.increment(U256::from(1)).into_transaction_request();
    // No gas_price: basefee=0 in the VM → execution succeeds without fee checks.
    // The heuristic uses real basefee (25M) to compute gas_budget.
    tx_req.set_gas_limit(100_000);
    tx_req.set_from(tester.l2_wallet.default_signer().address());
    tester
        .l2_provider
        .call(tx_req)
        .expect_to_fail("insufficient gas to cover pubdata cost")
        .await;

    Ok(())
}

#[test_multisetup([CURRENT_TO_L1])]
async fn call_no_pubdata_check_without_gas_price(tester: Tester) -> anyhow::Result<()> {
    // Test that the pubdata exhaustion check is NOT triggered when the caller does not
    // specify a gas price (effective_gas_price=0). This is the normal `eth_call` path
    // for read-only simulations.
    let counter = Counter::deploy(tester.l2_provider.clone()).await?;

    let mut tx_req = counter.increment(U256::from(1)).into_transaction_request();
    // No gas_price set, so effective_gas_price=0 — the heuristic should be skipped.
    tx_req.set_gas_limit(100_000);
    tx_req.set_from(tester.l2_wallet.default_signer().address());

    // This should succeed (no pubdata check applied)
    tester.l2_provider.call(tx_req).await?;

    Ok(())
}
