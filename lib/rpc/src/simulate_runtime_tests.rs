use super::*;
use alloy::eips::BlockId;
use alloy::network::TransactionBuilder;
use alloy::rpc::types::state::AccountOverride;
use std::{collections::HashSet, num::NonZeroU32, time::Duration};
use zksync_os_interface::traits::{PreimageSource, ReadStorage};
use zksync_os_storage_api::*;

#[derive(Clone, Debug)]
struct MemoryState;
impl ReadStorage for MemoryState {
    fn read(&mut self, _: B256) -> Option<B256> {
        None
    }
}
impl PreimageSource for MemoryState {
    fn get_preimage(&mut self, _: B256) -> Option<Vec<u8>> {
        None
    }
}
#[derive(Clone, Debug)]
struct SimulationStorage;
impl ReadStateHistory for SimulationStorage {
    fn state_view_at(&self, _: u64) -> StateResult<impl ViewState> {
        Ok(MemoryState)
    }
    fn block_range_available(&self) -> std::ops::RangeInclusive<u64> {
        0..=0
    }
}
impl ReadRpcStorage for SimulationStorage {
    fn repository(&self) -> &dyn ReadRepository {
        panic!("unused repository")
    }
    fn block_subscriptions(&self) -> &dyn notifications::SubscribeToBlocks {
        panic!("unused subscription")
    }
    fn replay_storage(&self) -> &dyn ReadReplay {
        self
    }
    fn finality(&self) -> &dyn ReadFinality {
        panic!("unused finality")
    }
    fn batch(&self) -> &dyn ReadBatch {
        panic!("unused batch")
    }
    fn tree(&self) -> &dyn zksync_os_merkle_tree_api::MerkleTreeProver {
        panic!("unused tree")
    }
    fn get_block_by_id(&self, _: BlockId) -> RepositoryResult<Option<RepositoryBlock>> {
        panic!("unused block")
    }
}
impl ReadReplay for SimulationStorage {
    fn get_context(&self, _: u64) -> Option<BlockContext> {
        Some(context())
    }
    fn get_original_context(&self, _: u64) -> Option<BlockContext> {
        Some(context())
    }
    fn get_replay_record_identity(&self, _: u64) -> Option<B256> {
        Some(B256::ZERO)
    }
    fn get_replay_record_by_key(&self, _: u64, _: Option<Vec<u8>>) -> Option<ReplayRecord> {
        panic!("unused full replay")
    }
    fn get_canonical_block_hash(&self, _: u64) -> Option<B256> {
        Some(B256::repeat_byte(0x44))
    }
    fn latest_record(&self) -> u64 {
        0
    }
}
fn context() -> BlockContext {
    let mut hashes = BlockHashes::default();
    hashes.0[255] = U256::from_be_bytes(B256::repeat_byte(0x44).0);
    BlockContext {
        chain_id: 57057,
        block_number: 1,
        block_hashes: hashes,
        timestamp: 1,
        eip1559_basefee: U256::from(3),
        native_price: U256::from(1),
        pubdata_price: U256::ZERO,
        blob_fee: U256::ONE,
        gas_limit: 30_000_000,
        pubdata_limit: 1_000_000,
        execution_version: zksync_os_types::ExecutionVersion::V7 as u32,
        ..Default::default()
    }
}
fn config() -> crate::config::RpcConfig {
    crate::config::RpcConfig {
        address: "127.0.0.1:0".into(),
        eth_call_gas: 30_000_000,
        block_timestamp_offset_seconds: 0,
        js_tracer_timeout: Duration::from_secs(1),
        js_tracer_max_memory_bytes: 0,
        enable_custom_js_tracers: false,
        call_tracer_max_capture_bytes: 1_000_000,
        call_tracer_max_frames: 1000,
        eth_simulate_block_gas_limit: 30_000_000,
        eth_simulate_gas_limit: NonZeroU64::new(30_000_000).unwrap(),
        max_connections: 1,
        max_concurrent_blocking_rpcs: 1,
        max_imt_reconstruction_leaves: NonZeroU64::new(1).unwrap(),
        max_subscriptions_per_connection: 1,
        max_request_size: 1,
        max_response_size: 1,
        max_batch_size: 10,
        parallel_batches: false,
        max_blocks_per_filter: 1,
        max_logs_per_response: 1,
        max_active_filters: NonZeroU32::new(1).unwrap(),
        max_filter_terms: NonZeroU32::new(1).unwrap(),
        stale_filter_ttl: Duration::from_secs(1),
        l2_signer_blacklist: HashSet::new(),
        l2_tx_blacklist: HashSet::new(),
        send_raw_transaction_sync_timeout: Duration::from_secs(1),
        gas_price_scale_factor: 1.0,
        estimate_gas_pubdata_price_factor: 1.0,
        enable_debug_namespace: false,
        enable_txpool_namespace: false,
        edge_da_admission: None,
        rate_limits: crate::config::RateLimits::None,
        method_filter: HashSet::new(),
    }
}
fn transfer(nonce: u64) -> TransactionRequest {
    TransactionRequest::default()
        .from(Address::repeat_byte(0x11))
        .to(Address::repeat_byte(0x22))
        .value(U256::ONE)
        .nonce(nonce)
        .with_chain_id(57057)
        // Small synthetic gas prices need a larger gas allowance to cover native units.
        .with_gas_limit(10_000_000)
        .max_fee_per_gas(8)
        .max_priority_fee_per_gas(1)
}

fn payload(calls: Vec<TransactionRequest>, funded: bool) -> SimulatePayload {
    let state_overrides = funded.then(|| {
        StateOverride::from_iter([(
            Address::repeat_byte(0x11),
            AccountOverride {
                balance: Some(U256::from(1u64 << 32)),
                ..Default::default()
            },
        )])
    });
    SimulatePayload {
        block_state_calls: vec![SimBlock {
            calls,
            state_overrides,
            ..Default::default()
        }],
        validation: true,
        return_full_transactions: true,
        trace_transfers: false,
    }
}

fn execute(
    handler: &EthCallHandler<SimulationStorage>,
    payload: SimulatePayload,
) -> serde_json::Value {
    serde_json::to_value(
        handler
            .simulate_v1_impl(payload, Some(BlockId::pending()))
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn exact_fee_cap_survives_native_simulation_and_response() {
    let (_sender, context_rx) = tokio::sync::watch::channel(Some(context()));
    let handler = EthCallHandler::new(config(), SimulationStorage, 57057, context_rx, None);
    let value = execute(&handler, payload(vec![transfer(0)], true));
    assert_eq!(value[0]["calls"][0]["status"], "0x1");
    assert_eq!(value[0]["baseFeePerGas"], "0x3");
    assert_eq!(value[0]["parentHash"], B256::repeat_byte(0x44).to_string());
    assert_eq!(value[0]["number"], "0x1");
    let tx = &value[0]["transactions"][0];
    assert_eq!(tx["from"], Address::repeat_byte(0x11).to_string());
    assert_eq!(tx["to"], Address::repeat_byte(0x22).to_string());
    assert_eq!(tx["maxFeePerGas"], "0x8");
    assert_eq!(tx["maxPriorityFeePerGas"], "0x1");
    assert_eq!(tx["gasPrice"], "0x4");
    assert_eq!(tx["gas"], "0x989680");
    assert_eq!(tx["nonce"], "0x0");
    assert_eq!(tx["value"], "0x1");
    assert_eq!(tx["chainId"], "0xdee1");
    assert_eq!(tx["type"], "0x2");
    assert_eq!(tx["accessList"], serde_json::json!([]));
}

#[test]
fn native_simulation_requires_the_preceding_nonce_execution() {
    let (_sender, context_rx) = tokio::sync::watch::channel(Some(context()));
    let handler = EthCallHandler::new(config(), SimulationStorage, 57057, context_rx, None);
    let missing = execute(&handler, payload(vec![transfer(1)], true));
    assert_eq!(missing[0]["calls"][0]["status"], "0x0");
    let included = execute(&handler, payload(vec![transfer(0), transfer(1)], true));
    assert_eq!(included[0]["calls"][0]["status"], "0x1");
    assert_eq!(included[0]["calls"][1]["status"], "0x1");
    assert_eq!(included[0]["transactions"][1]["nonce"], "0x1");
}

#[test]
fn native_simulation_does_not_create_unsigned_sender_funding() {
    let (_sender, context_rx) = tokio::sync::watch::channel(Some(context()));
    let handler = EthCallHandler::new(config(), SimulationStorage, 57057, context_rx, None);
    let blocks = execute(&handler, payload(vec![transfer(0)], false));
    assert_eq!(blocks[0]["calls"][0]["status"], "0x0");
    assert_eq!(blocks[0]["calls"][0]["error"]["code"], -38014);
}

#[tokio::test]
async fn native_simulation_observes_write_policy_and_resets_between_calls() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use zksync_os_tx_validators::policy_client::{
        Component, Config, DEFAULT_POLICY_TRACE_LIMITS, PolicyClient,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (expected_path, allow) in [
            ("/admit", false),
            ("/admit", true),
            ("/judge", false),
            ("/admit", true),
            ("/judge", true),
        ] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0; 2048];
            let start = loop {
                let size = stream.read(&mut chunk).await.unwrap();
                assert_ne!(size, 0);
                bytes.extend_from_slice(&chunk[..size]);
                if let Some(i) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let headers = std::str::from_utf8(&bytes[..start]).unwrap();
            assert!(headers.starts_with(&format!("POST {expected_path} ")));
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|s| s.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            while bytes.len() - start < length {
                let size = stream.read(&mut chunk).await.unwrap();
                assert_ne!(size, 0);
                bytes.extend_from_slice(&chunk[..size]);
            }
            let request: serde_json::Value =
                serde_json::from_slice(&bytes[start..start + length]).unwrap();
            assert_eq!(request["accessType"], "write");
            requests.push(request);
            let body = serde_json::json!({"allow":allow}).to_string();
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).as_bytes()).await.unwrap();
        }
        requests
    });
    let from = Address::repeat_byte(0x11);
    let client = PolicyClient::new(Config {
        url: format!("http://{address}"),
        component: Component::Rpc,
        request_timeout: Duration::from_secs(3),
        protocol_version: "1".into(),
        expected_protocol_version: None,
        bypass_from: HashSet::from([from]),
        trace_limits: DEFAULT_POLICY_TRACE_LIMITS,
        auth_token: Some("native-test".into()),
    })
    .unwrap();
    let results = tokio::task::spawn_blocking(move || {
        let (_sender, context_rx) = tokio::sync::watch::channel(Some(context()));
        let handler =
            EthCallHandler::new(config(), SimulationStorage, 57057, context_rx, Some(client));
        (0..3)
            .map(|_| execute(&handler, payload(vec![transfer(0)], true)))
            .collect::<Vec<_>>()
    })
    .await
    .unwrap();
    assert_eq!(results[0][0]["calls"][0]["status"], "0x0");
    assert_eq!(results[1][0]["calls"][0]["status"], "0x0");
    assert_eq!(results[2][0]["calls"][0]["status"], "0x1");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), service)
            .await
            .unwrap()
            .unwrap()
            .len(),
        5
    );
}
