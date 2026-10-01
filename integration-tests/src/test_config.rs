use crate::config::{ChainLayout, load_chain_config};
use crate::{AnvilL1, BATCH_VERIFICATION_ADDRESSES, BATCH_VERIFICATION_KEYS};
use alloy::primitives::Address;
use blake2::{Blake2s256, Digest};
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

/// SYSCOIN: Configures the node to commit batches on L1 without starting either proof stage.
/// Commitment precedes proving, so the intentionally idle pipeline keeps batches committed but
/// unproved without violating the V32 rule that rejects partial fake-prover topologies.
pub fn make_commit_only_config(config: &mut Config) {
    config.prover_api_config.fake_fri_provers.enabled = false;
    config.prover_api_config.fake_snark_provers.enabled = false;
}

/// Runs the full settlement pipeline so batches commit, prove, and execute on L1.
pub fn make_full_pipeline_config(config: &mut Config) {
    config.prover_api_config.fake_fri_provers.enabled = true;
    config.prover_api_config.fake_fri_provers.compute_time = Duration::from_millis(200);
    config.prover_api_config.fake_fri_provers.min_age = Duration::ZERO;
    config.prover_api_config.fake_snark_provers.enabled = true;
    config.prover_api_config.fake_snark_provers.max_batch_age = Duration::ZERO;
}

pub(crate) async fn build_node_config(
    l1: &AnvilL1,
    chain_layout: ChainLayout<'static>,
    with_proofs: bool,
) -> anyhow::Result<Config> {
    let mut config = load_chain_config(chain_layout).await;
    config.l1_provider_config =
        ProviderConfig::new(l1.address.clone(), TEST_PROVIDER_POLL_INTERVAL);
    if let Some(gateway_provider_config) = &mut config.gateway_provider_config {
        gateway_provider_config.rpc_poll_interval = TEST_PROVIDER_POLL_INTERVAL;
    }
    // The L1 senders poll receipts on their own cadence (1s default) — keep tests fast
    // against anvil's 0.25s blocks.
    config.l1_sender_config.poll_interval = TEST_PROVIDER_POLL_INTERVAL;
    config.sequencer_config.fee_collector_address = Address::random();
    config.sequencer_config.block_timestamp_offset_seconds = l1.timestamp_offset_seconds;
    config.rpc_config.send_raw_transaction_sync_timeout = Duration::from_secs(10);
    // SYSCOIN: integration tests intentionally exercise debug tracing on local RPC.
    config.rpc_config.enable_debug_namespace = true;
    config.prover_api_config.fake_fri_provers.enabled = !with_proofs;
    config.prover_api_config.fake_snark_provers.enabled = !with_proofs;
    config.batch_verification_config.server_enabled = false;
    config.batch_verification_config.client_enabled = false;
    config.batch_verification_config.threshold = 1;
    config.batch_verification_config.accepted_signers = BATCH_VERIFICATION_ADDRESSES.clone();
    config.batch_verification_config.request_timeout = Duration::from_millis(500);
    config.batch_verification_config.retry_delay = Duration::from_secs(1);
    config.batch_verification_config.signing_key = BATCH_VERIFICATION_KEYS[0].into();
    config.status_server_config.enabled = true;
    config.network_config.enabled = true;
    config.network_config.address = Ipv4Addr::LOCALHOST;
    config.network_config.interface = None;
    config.network_config.boot_nodes.clear();
    if chain_layout.fixture_scope() == crate::config::FixtureScope::AnvilComponentOnly {
        // bind_runtime_config replaces this default with this test's tempdir.
        // Tests may still override the encryption before launch.
        config.replay_archive_config = zksync_os_server::config::ReplayArchiveConfig::FileSystem {
            root_path: "component-test-replay".into(),
            encryption: zksync_os_server::config::ReplayArchiveEncryptionConfig::Noop,
        };
    }
    Ok(config)
}

pub(crate) fn maybe_start_bitcoin_da_mock(config: &mut Config) -> Option<BitcoinDaMock> {
    let may_use_syscoin_da = matches!(
        config.l1_sender_config.pubdata_mode,
        Some(PubdataMode::Blobs | PubdataMode::RelayedL2Calldata)
    ) || config.gateway_provider_config.is_some();
    if config.batcher_config.bitcoin_da_rpc_url.is_some() || !may_use_syscoin_da {
        return None;
    }

    // SYSCOIN: Keep mock creation synchronous so restart futures remain `Send` and can be
    // supervised with `tokio::spawn`; httpmock's async builder retains `Rc` state across await.
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST);
        then.respond_with(|req: &HttpMockRequest| {
            HttpMockResponse::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(handle_bitcoin_da_rpc(&req.body_string()))
                .build()
        });
    });

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
            // SYSCOIN: The committed blob ID is Blake2s over the exact encoded chunk.
            json!({"versionhash": format!("0x{}", alloy::hex::encode(Blake2s256::digest(bytes)))})
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

/// Fresh localhost fixture generation only. Reuses this module's ordinary
/// component DA mock and the normal node implementation; it is not a Core,
/// consensus, DA-finality or proof qualification process.
pub async fn run_component_fixture_bootstrap(
    config_path: std::path::PathBuf,
    deadline_unix: u64,
) -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    use reth_tasks::{RuntimeBuilder, RuntimeConfig, TokioConfig};
    use smart_config::{ConfigRepository, ConfigSources};
    use tokio::runtime::Handle;
    use tokio::signal::unix::{SignalKind, signal};
    use zksync_os_server::config::{
        ConfigValidate, build_external_config, load_config_file_sources,
    };

    ensure!(
        std::env::var("SYSCOIN_ANVIL_COMPONENT_ONLY").as_deref() == Ok("31337"),
        "component generation scope is required"
    );
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock before epoch")
            .as_secs()
    };
    let started = now();
    ensure!(
        deadline_unix > started && deadline_unix - started <= 3600,
        "finite generation deadline"
    );
    let config_path = config_path
        .canonicalize()
        .context("component config path")?;
    let registered_root = crate::component_replay::registered_root_for_config(&config_path)
        .map(std::path::Path::to_path_buf);
    let schema = Config::schema();
    let mut sources = ConfigSources::default();
    load_config_file_sources(&mut sources, &[config_path]);
    let mut config = build_external_config(ConfigRepository::new(&schema).with_all(sources)).await;
    let localhost_rpc = |value: &str| -> bool {
        reqwest::Url::parse(value).is_ok_and(|url| {
            url.scheme() == "http"
                && url.host_str() == Some("127.0.0.1")
                && url.port().is_some_and(|port| port > 1024)
                && url.username().is_empty()
                && url.password().is_none()
                && url.path() == "/"
                && url.query().is_none()
                && url.fragment().is_none()
        })
    };
    ensure!(
        localhost_rpc(&config.l1_provider_config.rpc_url)
            && config
                .gateway_provider_config
                .as_ref()
                .is_none_or(|c| localhost_rpc(&c.rpc_url))
            && config
                .rpc_config
                .address
                .parse::<std::net::SocketAddr>()
                .is_ok_and(|a| a.ip().is_loopback() && a.port() > 1024)
            && config.prover_api_config.fake_fri_provers.enabled
            && config.prover_api_config.fake_snark_provers.enabled
            && !config.prover_api_config.enabled,
        "only explicit localhost component mock topology is allowed"
    );
    config.network_config.enabled = false;
    let _mock =
        maybe_start_bitcoin_da_mock(&mut config).context("component DA mock is required")?;
    // The production validator is not bypassed. Only the existing test mock
    // supplies its component DA endpoint before normal validation and startup.
    config
        .validate()
        .await
        .context("component config validation")?;
    if let Some(root) = registered_root {
        ensure!(
            config.general_config.node_role.is_main()
                && config.general_config.ephemeral_state.is_none(),
            "fresh component main node required"
        );
        let hash = crate::config::fixture_backend::component::registered_hash("v32.0")
            .map_err(anyhow::Error::msg)?;
        let inventory = crate::config::fixture_backend::component::load(&root, "v32.0", &hash)
            .map_err(anyhow::Error::msg)?;
        let (_, state_identity) = inventory.anvil_state(&root).map_err(anyhow::Error::msg)?;
        let state_identity = state_identity.context("component L1 state identity")?;
        let state_path = state_identity.verify(&root).map_err(anyhow::Error::msg)?;
        let state_bytes = std::fs::read(state_path)?;
        state_identity
            .verify_bytes(&state_bytes)
            .map_err(anyhow::Error::msg)?;
        let timestamp = crate::l1_state_timestamp(&state_bytes)?;
        drop(state_bytes);
        // Match AnvilL1's fixture clock; new batches must not jump ahead of
        // the smoke test's explicitly timestamped Anvil settlement layer.
        config.sequencer_config.block_timestamp_offset_seconds =
            i64::try_from(timestamp)?.saturating_sub(i64::try_from(now())?);
        let destination = &config.general_config.rocks_db_path;
        config.general_config.rocks_db_path = destination
            .parent()
            .context("component DB parent")?
            .canonicalize()?
            .join(destination.file_name().context("component DB name")?);
        crate::component_replay::recover_registered(
            &root,
            config
                .genesis_config
                .chain_id
                .context("component chain ID")?,
            &config.general_config.rocks_db_path,
        )
        .await?;
        config.replay_archive_config = zksync_os_server::config::ReplayArchiveConfig::FileSystem {
            root_path: config.general_config.rocks_db_path.join("replay_archive"),
            encryption: zksync_os_server::config::ReplayArchiveEncryptionConfig::Noop,
        };
    }
    let runtime = RuntimeBuilder::new(
        RuntimeConfig::default().with_tokio(TokioConfig::existing_handle(Handle::current())),
    )
    .build()
    .context("component runtime")?;
    let mut term = signal(SignalKind::terminate())?;
    let booted = tokio::select! {
        ports = zksync_os_server::run(&runtime, config) => {
            println!("{}", json!({"scope": "AnvilComponentOnly", "rpc_port": ports.rpc}));
            true
        },
        _ = term.recv() => false,
        _ = tokio::time::sleep(Duration::from_secs(deadline_unix.saturating_sub(now()))) => false,
    };
    if booted {
        let task_manager = runtime
            .take_task_manager_handle()
            .context("component task manager")?;
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = term.recv() => {},
            result = task_manager => {
                result.context("component task manager join")?.context("component critical task failed")?;
            },
            _ = tokio::time::sleep(Duration::from_secs(deadline_unix.saturating_sub(now()))) => {},
        }
    }
    let stopped = tokio::task::spawn_blocking(move || {
        runtime.graceful_shutdown_with_timeout(Duration::from_secs(15))
    })
    .await?;
    ensure!(stopped, "component node did not stop cleanly");
    ensure!(booted, "component bootstrap stopped before RPC startup");
    Ok(())
}
