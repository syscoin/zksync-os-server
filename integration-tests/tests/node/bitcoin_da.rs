// SYSCOIN: end-to-end Bitcoin DA publication/finality regression test.
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use httpmock::Method::POST;
use httpmock::MockServer;
use serde_json::json;
use smart_config::value::SecretString;
use std::time::Duration;
use zksync_os_integration_tests::assert_traits::ReceiptAssert;
use zksync_os_integration_tests::{SettlementLayer, TesterBuilder};
use zksync_os_types::PubdataMode;

#[tokio::test]
async fn publishes_bitcoin_da_blob_for_gateway_settling_chain() -> anyhow::Result<()> {
    let server = MockServer::start_async().await;

    let loadwallet = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/")
                .body_contains("\"method\":\"loadwallet\"");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({"result": true, "error": null, "id": 1}));
        })
        .await;
    let getaddressesbylabel = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/wallet/zksync-os")
                .body_contains("\"method\":\"getaddressesbylabel\"");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({"result": {}, "error": null, "id": 1}));
        })
        .await;
    let getnewaddress = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/wallet/zksync-os")
                .body_contains("\"method\":\"getnewaddress\"");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({"result": "sys-mock-address", "error": null, "id": 1}));
        })
        .await;
    let create_blob = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/wallet/zksync-os")
                .body_contains("\"method\":\"syscoincreatenevmblob\"");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({"result": {"versionhash": "0xdeadbeef"}, "error": null, "id": 1}));
        })
        .await;
    let check_finality = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/")
                .body_contains("\"method\":\"getnevmblobdata\"");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({"result": {"chainlock": true}, "error": null, "id": 1}));
        })
        .await;

    let server_url = server.base_url();
    let tester = TesterBuilder::default()
        .settlement_layer(SettlementLayer::Gateway)
        .block_time(Duration::from_millis(50))
        .config_overrides(move |config| {
            config.l1_sender_config.pubdata_mode = Some(PubdataMode::Bitcoin);
            config.batcher_config.batch_timeout = Duration::from_millis(100);
            config.batcher_config.bitcoin_da_rpc_url = Some(server_url.clone());
            config.batcher_config.bitcoin_da_rpc_user =
                Some(SecretString::new("user".into()));
            config.batcher_config.bitcoin_da_rpc_password =
                Some(SecretString::new("password".into()));
            config.batcher_config.bitcoin_da_poda_url = server_url.clone();
            config.batcher_config.bitcoin_da_wallet_name = "zksync-os".into();
            config.batcher_config.bitcoin_da_address_label = "zksync-os-batcher".into();
            config.batcher_config.bitcoin_da_request_timeout = Duration::from_secs(2);
            config.batcher_config.bitcoin_da_finality_poll_interval = Duration::from_millis(20);
            config.batcher_config.bitcoin_da_finality_timeout = Duration::from_secs(5);
        })
        .build()
        .await?;

    let from = tester.l2_wallet.default_signer().address();
    tester
        .l2_provider
        .send_transaction(
            TransactionRequest::default()
                .with_from(from)
                .with_to(Address::random())
                .with_value(U256::from(1u64)),
        )
        .await?
        .expect_successful_receipt()
        .await?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if create_blob.hits_async().await > 0 && check_finality.hits_async().await > 0 {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("bitcoin da publication mocks were not hit in time");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert!(loadwallet.hits_async().await > 0);
    assert!(getaddressesbylabel.hits_async().await > 0);
    assert!(getnewaddress.hits_async().await > 0);
    assert!(create_blob.hits_async().await > 0);
    assert!(check_finality.hits_async().await > 0);

    Ok(())
}
