use alloy::consensus::{BlobTransactionSidecar, BlobTransactionSidecarVariant};
use alloy::eips::BlockId;
use alloy::primitives::{B256, U256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy::rpc::types::state::{AccountOverride, StateOverride};
use serde_json::{Value, json};
use zksync_os_integration_tests::assert_traits::EthCallAssert;
use zksync_os_integration_tests::contracts::{EventEmitter, SimpleRevert, TracingSecondary};
use zksync_os_integration_tests::{CURRENT_TO_L1, Tester, test_multisetup};

#[test_multisetup([CURRENT_TO_L1])]
async fn call_genesis(tester: Tester) -> anyhow::Result<()> {
    // Test that the node can run `eth_call` on genesis
    tester
        .l2_provider
        .call(TransactionRequest::default())
        .block(0.into())
        .await?;
    Ok(())
}

#[test_multisetup([CURRENT_TO_L1])]
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
    // TracingSecondary stores its constructor argument at slot zero.
    let initial_data = U256::from(1);
    let contract = TracingSecondary::deploy(tester.l2_provider.clone(), initial_data).await?;
    let tx_req = contract.multiply(U256::from(1)).into_transaction_request();
    let slot_override = (B256::ZERO, B256::with_last_byte(2));
    let overrides = StateOverride::from_iter([(
        *contract.address(),
        AccountOverride::default().with_state_diff([slot_override]),
    )]);

    let out = tester
        .l2_provider
        .call(tx_req.clone())
        .overrides(overrides)
        .await?;
    assert_eq!(U256::from_be_slice(&out), U256::from(2));

    for account in [
        AccountOverride::default().with_state([]),
        AccountOverride::default().with_state([slot_override]),
    ] {
        let overrides = StateOverride::from_iter([(*contract.address(), account)]);
        let params = json!([tx_req, "latest", overrides]);
        let trace_params = json!([
            tx_req,
            "latest",
            { "tracer": "callTracer", "stateOverrides": overrides }
        ]);
        let simulate_params = json!([{
            "blockStateCalls": [{ "calls": [tx_req], "stateOverrides": overrides }]
        }, "latest"]);
        for (method, params) in [
            ("eth_call", params.clone()),
            ("eth_estimateGas", params),
            ("debug_traceCall", trace_params),
            ("eth_simulateV1", simulate_params),
        ] {
            let err = tester
                .l2_provider
                .raw_request::<_, Value>(method.into(), params)
                .await
                .expect_err("full storage replacement must be rejected");
            let response = err
                .as_error_resp()
                .unwrap_or_else(|| panic!("expected a JSON-RPC error from {method}, got {err}"));
            assert_eq!(response.code, -32602, "{method}: {response:?}");
            assert!(
                response.message.contains("full storage replacement"),
                "{method}: {response:?}"
            );
        }
    }

    let out = tester.l2_provider.call(tx_req).await?;
    assert_eq!(U256::from_be_slice(&out), initial_data);

    Ok(())
}
