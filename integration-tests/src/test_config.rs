use crate::config::{ChainLayout, load_chain_config};
use crate::{AnvilL1, BATCH_VERIFICATION_ADDRESSES, BATCH_VERIFICATION_KEYS};
use alloy::primitives::{Address, keccak256};
use httpmock::Method::POST;
use httpmock::{HttpMockRequest, HttpMockResponse, MockServer};
use serde_json::{Value, json};
use smart_config::value::SecretString;
use std::fmt;
use std::net::Ipv4Addr;
use std::time::Duration;
use zksync_os_server::config::{Config, ProviderConfig};
use zksync_os_types::PubdataMode;

pub(crate) const TEST_PROVIDER_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub(crate) struct BitcoinDaMock {
    server: MockServer,
}

impl fmt::Debug for BitcoinDaMock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BitcoinDaMock")
            .field("base_url", &self.server.base_url())
            .finish()
    }
}

pub(crate) fn disable_prover_input_generation(config: &mut Config) {
    if config.prover_api_config.fake_fri_provers.enabled
        && config.prover_api_config.fake_snark_provers.enabled
    {
        config.prover_input_generator_config.enable_input_generation = false;
    }
}

pub(crate) async fn build_node_config(
    l1: &AnvilL1,
    chain_layout: ChainLayout<'static>,
) -> anyhow::Result<Config> {
    let mut config = load_chain_config(chain_layout).await;
    config.l1_provider_config =
        ProviderConfig::new(l1.address.clone(), TEST_PROVIDER_POLL_INTERVAL);
    if let Some(gateway_provider_config) = &mut config.gateway_provider_config {
        gateway_provider_config.rpc_poll_interval = TEST_PROVIDER_POLL_INTERVAL;
    }
    config.sequencer_config.fee_collector_address = Address::random();
    config.rpc_config.send_raw_transaction_sync_timeout = Duration::from_secs(10);
    // SYSCOIN: integration tests intentionally exercise debug tracing on local RPC.
    config.rpc_config.enable_debug_namespace = true;
    config.prover_api_config.fake_fri_provers.enabled = true;
    config.prover_api_config.fake_snark_provers.enabled = true;
    config.batch_verification_config.server_enabled = false;
    config.batch_verification_config.client_enabled = false;
    config.batch_verification_config.threshold = 1;
    config.batch_verification_config.accepted_signers = BATCH_VERIFICATION_ADDRESSES.clone();
    config.batch_verification_config.request_timeout = Duration::from_millis(500);
    config.batch_verification_config.retry_delay = Duration::from_secs(1);
    config.batch_verification_config.total_timeout = Duration::from_secs(300);
    config.batch_verification_config.signing_key = BATCH_VERIFICATION_KEYS[0].into();
    config.status_server_config.enabled = true;
    config.network_config.enabled = true;
    config.network_config.address = Ipv4Addr::LOCALHOST;
    config.network_config.interface = None;
    config.network_config.boot_nodes.clear();
    Ok(config)
}

pub(crate) async fn maybe_start_bitcoin_da_mock(config: &mut Config) -> Option<BitcoinDaMock> {
    if config.batcher_config.bitcoin_da_rpc_url.is_some()
        || !matches!(
            config.l1_sender_config.pubdata_mode,
            Some(PubdataMode::Blobs | PubdataMode::RelayedL2Calldata)
        )
    {
        return None;
    }

    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(POST);
            then.respond_with(|req: &HttpMockRequest| {
                HttpMockResponse::builder()
                    .status(200)
                    .header("content-type", "application/json")
                    .body(handle_bitcoin_da_rpc(&req.body_string()))
                    .build()
            });
        })
        .await;

    let server_url = server.base_url();
    config.batcher_config.bitcoin_da_rpc_url = Some(server_url.clone());
    config.batcher_config.bitcoin_da_rpc_user = Some(SecretString::new("user".into()));
    config.batcher_config.bitcoin_da_rpc_password = Some(SecretString::new("password".into()));
    config.batcher_config.bitcoin_da_poda_url = server_url;
    config.batcher_config.bitcoin_da_wallet_name = "zksync-os".into();
    config.batcher_config.bitcoin_da_address_label = "zksync-os-batcher".into();

    Some(BitcoinDaMock { server })
}

fn handle_bitcoin_da_rpc(body: &str) -> String {
    let request: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let response = if let Some(calls) = request.as_array() {
        Value::Array(calls.iter().map(handle_bitcoin_da_call).collect())
    } else {
        handle_bitcoin_da_call(&request)
    };
    response.to_string()
}

fn handle_bitcoin_da_call(call: &Value) -> Value {
    let id = call.get("id").cloned().unwrap_or(Value::Null);
    let method = call
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = call
        .get("params")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    let result = match method {
        "loadwallet" => json!(true),
        "getaddressesbylabel" => json!({}),
        "getnewaddress" => json!("sys-mock-address"),
        "estimatesmartfee" => json!({"feerate": 0.00001, "blocks": 6}),
        "getmempoolinfo" => json!({"mempoolminfee": 0.00002, "minrelaytxfee": 0.000015}),
        "getblockcount" => json!(110),
        "syscoincreatenevmblob" => {
            let data = params.first().and_then(Value::as_str).unwrap_or_default();
            let data = data.strip_prefix("0x").unwrap_or(data);
            let bytes = alloy::hex::decode(data).unwrap_or_default();
            json!({"versionhash": format!("0x{}", alloy::hex::encode(keccak256(bytes)))})
        }
        "getnevmblobdata" => {
            let version_hash = params.first().and_then(Value::as_str).unwrap_or_default();
            json!({
                "versionhash": version_hash,
                "txid": "sys-mock-txid",
                "mtp": 12345,
                "datasize": 32,
                "height": 100,
                "chainlock": true
            })
        }
        _ => Value::Null,
    };

    json!({"jsonrpc": "2.0", "id": id, "result": result, "error": null})
}
