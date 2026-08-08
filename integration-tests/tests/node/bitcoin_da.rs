// SYSCOIN: end-to-end Bitcoin DA publication/finality regression test.
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use blake2::{Blake2s256, Digest};
use httpmock::Method::POST;
use httpmock::{HttpMockRequest, HttpMockResponse, MockServer};
use serde_json::{Value, json};
use smart_config::value::SecretString;
use std::time::Duration;
// SYSCOIN: Bitcoin-DA Gateway coverage stays on the v31 production topology until V8 exposes the
// compact edge-DA inputs required by our settlement contract.
use zksync_os_integration_tests::CURRENT_TO_GATEWAY;
use zksync_os_integration_tests::assert_traits::ReceiptAssert;
use zksync_os_server::config::BitcoinDaFinalityMode;
use zksync_os_types::PubdataMode;

fn create_blob_response(req: &HttpMockRequest) -> HttpMockResponse {
    let request: Value = serde_json::from_str(&req.body_string()).unwrap();
    let data = request["params"][0].as_str().unwrap();
    let bytes = alloy::hex::decode(data.strip_prefix("0x").unwrap_or(data)).unwrap();
    let version_hash = format!("0x{}", alloy::hex::encode(Blake2s256::digest(bytes)));
    HttpMockResponse::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(
            json!({
                "result": {"versionhash": version_hash},
                "error": null,
                "id": request["id"]
            })
            .to_string(),
        )
        .build()
}

fn blob_data_response(req: &HttpMockRequest) -> HttpMockResponse {
    let request: Value = serde_json::from_str(&req.body_string()).unwrap();
    HttpMockResponse::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(
            json!({
                "result": {
                    "versionhash": request["params"][0],
                    "txid": "abc123",
                    "mtp": 12345,
                    "datasize": 32,
                    "height": 100
                },
                "error": null,
                "id": request["id"]
            })
            .to_string(),
        )
        .build()
}

#[tokio::test]
async fn publishes_bitcoin_da_blob_for_gateway_settling_chain() -> anyhow::Result<()> {
    let server = MockServer::start_async().await;

    let loadwallet = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/")
                .body_matches(r#""method"\s*:\s*"loadwallet""#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({"result": true, "error": null, "id": 1}));
        })
        .await;
    let getaddressesbylabel = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/wallet/zksync-os")
                .body_matches(r#""method"\s*:\s*"getaddressesbylabel""#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({"result": {}, "error": null, "id": 1}));
        })
        .await;
    let getnewaddress = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/wallet/zksync-os")
                .body_matches(r#""method"\s*:\s*"getnewaddress""#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({"result": "sys-mock-address", "error": null, "id": 1}));
        })
        .await;
    let estimate_smart_fee = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/")
                .body_matches(r#""method"\s*:\s*"estimatesmartfee""#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(
                    json!({"result": {"feerate": 0.00001, "blocks": 6}, "error": null, "id": 1}),
                );
        })
        .await;
    let get_mempool_info = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/")
                .body_matches(r#""method"\s*:\s*"getmempoolinfo""#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({"result": {"mempoolminfee": 0.00002, "minrelaytxfee": 0.000015}, "error": null, "id": 1}));
        })
        .await;
    let create_blob = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/wallet/zksync-os")
                .body_matches(r#""method"\s*:\s*"syscoincreatenevmblob""#);
            then.respond_with(create_blob_response);
        })
        .await;
    let server_url = server.base_url();
    // SYSCOIN: Exercise the same v31 Gateway path deployed on testnet.
    let env = CURRENT_TO_GATEWAY.environment().await?;
    let mut config = env.default_config().await?;
    config.sequencer_config.block_time = Duration::from_millis(50);
    config.l1_sender_config.pubdata_mode = Some(PubdataMode::Blobs);
    config.batcher_config.batch_timeout = Duration::from_millis(100);
    config.batcher_config.bitcoin_da_rpc_url = Some(server_url.clone());
    config.batcher_config.bitcoin_da_rpc_user = Some(SecretString::new("user".into()));
    config.batcher_config.bitcoin_da_rpc_password = Some(SecretString::new("password".into()));
    config.batcher_config.bitcoin_da_poda_url = server_url;
    config.batcher_config.bitcoin_da_wallet_name = "zksync-os".into();
    config.batcher_config.bitcoin_da_address_label = "zksync-os-batcher".into();
    config.batcher_config.bitcoin_da_request_timeout = Duration::from_secs(2);
    config.batcher_config.bitcoin_da_finality_poll_interval = Duration::from_millis(20);
    config.batcher_config.bitcoin_da_finality_timeout = Duration::from_secs(5);
    let tester = env.launch(config).await?;

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

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if create_blob.calls_async().await > 0 {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("bitcoin da publication mocks were not hit in time");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert!(loadwallet.calls_async().await > 0);
    assert!(getaddressesbylabel.calls_async().await > 0);
    assert!(getnewaddress.calls_async().await > 0);
    assert!(estimate_smart_fee.calls_async().await > 0);
    assert!(get_mempool_info.calls_async().await > 0);
    assert!(create_blob.calls_async().await > 0);

    Ok(())
}

#[tokio::test]
async fn publishes_bitcoin_da_blob_with_confirmation_based_finality() -> anyhow::Result<()> {
    let server = MockServer::start_async().await;

    let loadwallet = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/")
                .body_matches(r#""method"\s*:\s*"loadwallet""#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({"result": true, "error": null, "id": 1}));
        })
        .await;
    let getaddressesbylabel = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/wallet/zksync-os")
                .body_matches(r#""method"\s*:\s*"getaddressesbylabel""#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({"result": {}, "error": null, "id": 1}));
        })
        .await;
    let getnewaddress = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/wallet/zksync-os")
                .body_matches(r#""method"\s*:\s*"getnewaddress""#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({"result": "sys-mock-address", "error": null, "id": 1}));
        })
        .await;
    let estimate_smart_fee = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/")
                .body_matches(r#""method"\s*:\s*"estimatesmartfee""#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(
                    json!({"result": {"feerate": 0.00001, "blocks": 6}, "error": null, "id": 1}),
                );
        })
        .await;
    let get_mempool_info = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/")
                .body_matches(r#""method"\s*:\s*"getmempoolinfo""#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({"result": {"mempoolminfee": 0.00002, "minrelaytxfee": 0.000015}, "error": null, "id": 1}));
        })
        .await;
    let create_blob = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/wallet/zksync-os")
                .body_matches(r#""method"\s*:\s*"syscoincreatenevmblob""#);
            then.respond_with(create_blob_response);
        })
        .await;
    let get_blob_data = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/")
                .body_matches(r#""method"\s*:\s*"getnevmblobdata""#);
            then.respond_with(blob_data_response);
        })
        .await;
    let get_block_count = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/")
                .body_matches(r#""method"\s*:\s*"getblockcount""#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({"result": 104, "error": null, "id": 1}));
        })
        .await;

    let server_url = server.base_url();
    let gateway_server_url = server_url.clone();
    // SYSCOIN: Confirmation-based Bitcoin DA finality must remain covered on production v31.
    let env = CURRENT_TO_GATEWAY
        .environment_with_gateway_config(move |config| {
            config.l1_sender_config.pubdata_mode = Some(PubdataMode::Blobs);
            config.batcher_config.bitcoin_da_rpc_url = Some(gateway_server_url.clone());
            config.batcher_config.bitcoin_da_rpc_user = Some(SecretString::new("user".into()));
            config.batcher_config.bitcoin_da_rpc_password =
                Some(SecretString::new("password".into()));
            config.batcher_config.bitcoin_da_poda_url = gateway_server_url;
            config.batcher_config.bitcoin_da_wallet_name = "zksync-os".into();
            config.batcher_config.bitcoin_da_address_label = "zksync-os-batcher".into();
            config.batcher_config.bitcoin_da_request_timeout = Duration::from_secs(2);
            config.batcher_config.bitcoin_da_finality_poll_interval = Duration::from_millis(20);
            config.batcher_config.bitcoin_da_finality_mode = BitcoinDaFinalityMode::Confirmations;
            config.batcher_config.bitcoin_da_finality_confirmations = 5;
            config.batcher_config.bitcoin_da_finality_timeout = Duration::from_secs(5);
        })
        .await?;
    let publication_calls_before_child = create_blob.calls_async().await;
    let blob_data_calls_before_child = get_blob_data.calls_async().await;
    let block_count_calls_before_child = get_block_count.calls_async().await;
    let mut config = env.default_config().await?;
    config.sequencer_config.block_time = Duration::from_millis(50);
    config.l1_sender_config.pubdata_mode = Some(PubdataMode::Blobs);
    config.batcher_config.batch_timeout = Duration::from_millis(100);
    config.batcher_config.bitcoin_da_rpc_url = Some(server_url.clone());
    config.batcher_config.bitcoin_da_rpc_user = Some(SecretString::new("user".into()));
    config.batcher_config.bitcoin_da_rpc_password = Some(SecretString::new("password".into()));
    config.batcher_config.bitcoin_da_poda_url = server_url;
    config.batcher_config.bitcoin_da_wallet_name = "zksync-os".into();
    config.batcher_config.bitcoin_da_address_label = "zksync-os-batcher".into();
    config.batcher_config.bitcoin_da_request_timeout = Duration::from_secs(2);
    config.batcher_config.bitcoin_da_finality_poll_interval = Duration::from_millis(20);
    config.batcher_config.bitcoin_da_finality_mode = BitcoinDaFinalityMode::Confirmations;
    config.batcher_config.bitcoin_da_finality_confirmations = 5;
    config.batcher_config.bitcoin_da_finality_timeout = Duration::from_secs(5);
    let tester = env.launch(config).await?;

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

    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        if create_blob.calls_async().await > publication_calls_before_child
            && get_blob_data.calls_async().await > blob_data_calls_before_child
            && get_block_count.calls_async().await > block_count_calls_before_child
        {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("bitcoin da confirmation-based mocks were not hit in time");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert!(loadwallet.calls_async().await > 0);
    assert!(getaddressesbylabel.calls_async().await > 0);
    assert!(getnewaddress.calls_async().await > 0);
    assert!(estimate_smart_fee.calls_async().await > 0);
    assert!(get_mempool_info.calls_async().await > 0);
    assert!(create_blob.calls_async().await > 0);
    assert!(get_blob_data.calls_async().await > 0);
    assert!(get_block_count.calls_async().await > 0);

    Ok(())
}
