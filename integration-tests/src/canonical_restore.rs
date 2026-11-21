//! Owns a private canonical Core/NEVM restore shared by the CPU and GPU qualification tests.

use crate::config::{ChainLayout, FixtureScope, fixture_backend, load_config_from_paths};
use crate::l1_helpers::fetch_l1_state;
use crate::test_config::TEST_PROVIDER_POLL_INTERVAL;
use crate::{L1_CHAIN_ID, TestL1, Tester, spawn_prover_child, wait_for_gateway_readiness};
use alloy::eips::BlockNumberOrTag;
use alloy::network::EthereumWallet;
use alloy::primitives::{Address, B256, U256, address, hex};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, ensure};
use blake2::{Blake2s256, Digest};
use serde::Deserialize;
use serde_json::{Value, json};
use smart_config::value::SecretString;
use std::fs::{File, symlink_metadata};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

use tokio::time::{Instant, sleep, timeout};
use zksync_os_alloy_ext::provider::ZksyncApi;
use zksync_os_provider::NodeProvider;
use zksync_os_server::config::{
    BitcoinDaFinalityMode, ConfigValidate, ExternalPriceApiClientConfig, ProviderConfig,
    ReplayArchiveConfig,
};
use zksync_os_server::default_protocol_version::PROTOCOL_VERSION_V32_0;
use zksync_os_types::{ProvingVersion, PubdataMode};

const GATEWAY_ID: u64 = 57_001;
const EDGE_IDS: [u64; 2] = [6565, 6566];
const REQUIRED_DA_CONFIRMATIONS: u64 = 5;
const DA_PRECOMPILE: Address = address!("0000000000000000000000000000000000000063");
const CORE_COOKIE: &str = "core/regtest/.cookie";
// Public development account zero. Sign only after checking the owned, isolated regtest's ID.
const NEVM_FUNDER_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(600);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ready {
    schema: String,
    descriptor_sha256: String,
    nevm_rpc_url: String,
    core_rpc_url: String,
    core_cookie_file: String,
    core_wallet_name: String,
    nevm_latest_timestamp: u64,
    roles: RestoredRoles,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoredRoles {
    gateway: Frontier,
    #[serde(rename = "edge-6565")]
    edge_6565: Frontier,
    #[serde(rename = "edge-6566")]
    edge_6566: Frontier,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Frontier {
    block_number: u64,
    block_hash: B256,
    batch_number: u64,
}

struct Supervisor {
    child: Option<Child>,
    dir: Arc<TempDir>,
}

impl Supervisor {
    fn spawn(fixture_dir: &Path, supervisor: &Path, digest: &str) -> anyhow::Result<Self> {
        let private_configs = std::env::var_os("SYSCOIN_CANONICAL_PRIVATE_CONFIG_DIR")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .context(
                "SYSCOIN_CANONICAL_PRIVATE_CONFIG_DIR must name the private config directory",
            )?;
        let dir = Arc::new(tempfile::tempdir()?);
        let child = Command::new(supervisor)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .arg("--fixture-dir")
            .arg(fixture_dir)
            .arg("--descriptor-sha256")
            .arg(digest)
            .arg("--run-dir")
            .arg(dir.path())
            .arg("--private-config-dir")
            .arg(private_configs)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .context("failed to start the authenticated canonical restore supervisor")?;
        Ok(Self {
            child: Some(child),
            dir,
        })
    }

    async fn ready(&mut self) -> anyhow::Result<Ready> {
        let path = self.dir.path().join("ready.json");
        timeout(STARTUP_TIMEOUT, async {
            loop {
                self.ensure_alive()?;
                if path.try_exists()? {
                    return serde_json::from_slice(&read_small_file(&path, 16 * 1024)?)
                        .context("invalid canonical restore readiness record");
                }
                sleep(POLL_INTERVAL).await;
            }
        })
        .await
        .context("canonical restore supervisor readiness timed out")?
    }

    fn ensure_alive(&mut self) -> anyhow::Result<()> {
        if let Some(status) = self
            .child
            .as_mut()
            .context("restore supervisor has already stopped")?
            .try_wait()?
        {
            anyhow::bail!("canonical restore supervisor exited before shutdown: {status}");
        }
        Ok(())
    }

    fn begin_mining(&mut self) -> anyhow::Result<()> {
        self.child
            .as_mut()
            .and_then(|child| child.stdin.as_mut())
            .context("restore supervisor mining control pipe is closed")?
            .write_all(b"mine\n")
            .context("failed to enable private paired mining")
    }

    async fn shutdown(mut self) -> anyhow::Result<()> {
        let child = self.child.as_mut().context("missing restore supervisor")?;
        // EOF waits for Core and NEVM to stop in order. Killing the supervisor first could
        // interrupt child teardown before the restored state has closed.
        drop(child.stdin.take());
        let status = timeout(SHUTDOWN_TIMEOUT, async {
            loop {
                if let Some(status) = child.try_wait()? {
                    return Ok::<_, std::io::Error>(status);
                }
                sleep(POLL_INTERVAL).await;
            }
        })
        .await
        .context("canonical restore supervisor did not finish shutdown")??;
        self.child.take();
        ensure!(
            status.success(),
            "canonical supervisor shutdown failed: {status}"
        );
        Ok(())
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            drop(child.stdin.take());
            let dir = Arc::clone(&self.dir);
            // Keep the restored state present during best-effort teardown on unwind.
            std::thread::spawn(move || {
                let _dir = dir;
                let _ = child.wait();
            });
        }
    }
}

struct CoreRpc {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    user: String,
    password: String,
    wallet_name: String,
}

impl CoreRpc {
    fn from_ready(ready: &Ready, run: &Path) -> anyhow::Result<Self> {
        let endpoint = owned_rpc_url(&ready.core_rpc_url)?;
        ensure!(
            ready.core_cookie_file == CORE_COOKIE
                && !ready.core_wallet_name.is_empty()
                && ready.core_wallet_name.len() <= 128
                && !matches!(ready.core_wallet_name.as_str(), "." | "..")
                && ready
                    .core_wallet_name
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')),
            "invalid private Core cookie or wallet path"
        );
        let cookie = read_small_file(&run.join(CORE_COOKIE), 1024)?;
        let cookie =
            std::str::from_utf8(&cookie).context("invalid private Core cookie encoding")?;
        let (user, password) = cookie
            .trim_end_matches(['\r', '\n'])
            .split_once(':')
            .context("invalid private Core cookie format")?;
        ensure!(
            user == "__cookie__"
                && !password.is_empty()
                && password.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid private Core cookie credentials"
        );
        Ok(Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(10))
                .build()?,
            endpoint,
            user: user.to_owned(),
            password: password.to_owned(),
            wallet_name: ready.core_wallet_name.clone(),
        })
    }

    async fn call(&self, wallet: bool, method: &str, params: Value) -> anyhow::Result<Value> {
        let mut endpoint = self.endpoint.clone();
        if wallet {
            endpoint.set_path(&format!("/wallet/{}", self.wallet_name));
        }
        #[derive(Deserialize)]
        struct Response {
            id: String,
            result: Value,
            error: Option<Value>,
        }
        let body = serde_json::to_vec(&json!({
            "jsonrpc": "1.0", "id": "canonical-restore", "method": method, "params": params,
        }))?;
        let response = self
            .client
            .post(endpoint)
            .basic_auth(&self.user, Some(&self.password))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .with_context(|| format!("owned Core RPC {method} transport failed"))?;
        let status = response.status();
        let body: Response = response
            .json()
            .await
            .with_context(|| format!("owned Core RPC {method} returned an invalid body"))?;
        ensure!(
            body.id == "canonical-restore",
            "owned Core RPC returned the wrong request ID"
        );
        if method == "getnevmblobdata"
            && body.error.as_ref().is_some_and(|error| {
                error["code"].as_i64() == Some(-8)
                    && error["message"].as_str().is_some_and(|message| {
                        message.starts_with("Could not find blob information for versionhash ")
                    })
            })
        {
            // The wallet can return success before the version hash is visible to the DA DB.
            return Ok(Value::Null);
        }
        ensure!(
            status.is_success() && body.error.is_none(),
            "owned Core RPC {method} returned an error"
        );
        Ok(body.result)
    }
}

fn read_small_file(path: &Path, max_bytes: u64) -> anyhow::Result<Vec<u8>> {
    ensure!(
        symlink_metadata(path)?.file_type().is_file(),
        "canonical runtime record must be a regular file: {}",
        path.display()
    );
    let mut bytes = Vec::new();
    File::open(path)?
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= max_bytes,
        "canonical runtime record exceeds size limit"
    );
    Ok(bytes)
}

fn owned_rpc_url(value: &str) -> anyhow::Result<reqwest::Url> {
    let leaf = value
        .strip_prefix("http://127.0.0.1:")
        .context("canonical RPC must use literal IPv4 loopback HTTP")?;
    let port = leaf.parse::<u16>().context("invalid owned RPC port")?;
    ensure!(
        port != 0 && leaf == port.to_string(),
        "noncanonical owned RPC port"
    );
    Ok(reqwest::Url::parse(value)?)
}

async fn connect_root(ready: &Ready, dir: Arc<TempDir>) -> anyhow::Result<TestL1> {
    let endpoint = owned_rpc_url(&ready.nevm_rpc_url)?;
    ensure!(
        ready.nevm_rpc_url != ready.core_rpc_url,
        "Core and NEVM listeners must be distinct"
    );
    let signer: PrivateKeySigner = NEVM_FUNDER_KEY.parse()?;
    let wallet = EthereumWallet::new(signer);
    let provider = timeout(Duration::from_secs(30), async {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()?;
        let provider = ProviderBuilder::new()
            .wallet(wallet.clone())
            .connect_reqwest(client, endpoint);
        ensure!(
            provider.get_chain_id().await? == L1_CHAIN_ID,
            "wrong owned NEVM chain"
        );
        let block = provider
            .get_block_by_number(BlockNumberOrTag::Latest)
            .await?
            .context("owned NEVM did not return its restored head")?;
        ensure!(
            block.header.timestamp == ready.nevm_latest_timestamp,
            "owned NEVM head differs from the supervisor's stopped-boundary observation"
        );
        ensure!(
            provider
                .get_balance(wallet.default_signer().address())
                .await?
                > U256::ZERO,
            "the private NEVM fixture funder has no balance"
        );
        Ok::<_, anyhow::Error>(NodeProvider::new(provider).await?)
    })
    .await
    .context("timeout verifying the owned NEVM endpoint")??;
    Ok(TestL1 {
        address: ready.nevm_rpc_url.clone(),
        provider,
        wallet,
        // Private NEVM starts mining on the wall clock once the restored head is authenticated.
        timestamp_offset_seconds: 0,
        _tempdir: dir,
    })
}

/// The exact restored network. Both qualification tests boot and check all three rollup roles.
pub struct CanonicalTestStack {
    gateway: Option<Tester>,
    edges: Vec<Tester>,
    supervisor: Supervisor,
    core: CoreRpc,
}

impl CanonicalTestStack {
    pub async fn start() -> anyhow::Result<Self> {
        let layout = ChainLayout::Gateway {
            protocol_version: PROTOCOL_VERSION_V32_0,
            fixture_scope: FixtureScope::CanonicalSyscoin,
        };
        let fixture_dir = layout.protocol_dir().canonicalize()?;
        fixture_backend::check_regeneration_marker(&fixture_dir).map_err(anyhow::Error::msg)?;
        let digest = fixture_backend::trusted_descriptor_hash(PROTOCOL_VERSION_V32_0)
            .context("the canonical V32 fixture has no reviewed descriptor pin")?;
        let inventory = fixture_backend::load_fixture_inventory(
            &fixture_dir,
            PROTOCOL_VERSION_V32_0,
            Some(digest),
        )
        .map_err(anyhow::Error::msg)?;
        let supervisor_artifact = &inventory
            .core_nevm_inventory()
            .map_err(anyhow::Error::msg)?
            .tools
            .iter()
            .find(|tool| tool.role == fixture_backend::ToolRole::RestoreSupervisor)
            .context("canonical restore supervisor slot is missing")?
            .file;
        let supervisor_path = supervisor_artifact
            .verify(&fixture_dir)
            .map_err(anyhow::Error::msg)?;
        let mut supervisor = Supervisor::spawn(&fixture_dir, &supervisor_path, digest)?;
        let ready = supervisor.ready().await?;
        ensure!(
            ready.schema == "syscoin-v32-canonical-restore-ready-v1"
                && ready.descriptor_sha256 == digest,
            "canonical restore readiness schema or digest mismatch"
        );
        let core = CoreRpc::from_ready(&ready, supervisor.dir.path())?;
        let wallet: Value = core.call(true, "getwalletinfo", json!([])).await?;
        ensure!(
            wallet["walletname"].as_str() == Some(core.wallet_name.as_str()),
            "wrong owned Core wallet"
        );
        let l1 = connect_root(&ready, Arc::clone(&supervisor.dir)).await?;
        supervisor.begin_mining()?;

        let mut stack = Self {
            gateway: None,
            edges: Vec::new(),
            supervisor,
            core,
        };
        let startup = timeout(STARTUP_TIMEOUT, stack.start_nodes(&l1, ready.roles))
            .await
            .context("restored Gateway and edge startup timed out")
            .and_then(|result| result);
        if let Err(error) = startup {
            if let Err(cleanup) = stack.shutdown().await {
                return Err(error.context(format!("restore cleanup also failed: {cleanup:#}")));
            }
            return Err(error);
        }
        Ok(stack)
    }

    pub fn gateway(&self) -> &Tester {
        self.gateway.as_ref().expect("canonical Gateway is running")
    }

    pub fn ensure_running(&mut self) -> anyhow::Result<()> {
        self.supervisor.ensure_alive()
    }

    pub fn start_gateway_worker(&self) -> anyhow::Result<tokio::process::Child> {
        let gateway = self.gateway();
        let url = gateway
            .prover_api_url()
            .context("canonical Gateway prover API missing")?;
        spawn_prover_child(gateway, &[url], None)
    }

    async fn start_nodes(&mut self, l1: &TestL1, roles: RestoredRoles) -> anyhow::Result<()> {
        let gateway = self.start_node(l1, "gateway", GATEWAY_ID, None).await?;
        self.gateway = Some(gateway);
        verify_frontier(self.gateway(), GATEWAY_ID, L1_CHAIN_ID, &roles.gateway).await?;
        let gateway_rpc = self.gateway().l2_rpc_url().to_owned();
        for (index, (role, frontier)) in [
            ("edge-6565", roles.edge_6565),
            ("edge-6566", roles.edge_6566),
        ]
        .into_iter()
        .enumerate()
        {
            let chain_id = EDGE_IDS[index];
            let edge = self
                .start_node(l1, role, chain_id, Some((&gateway_rpc, index)))
                .await?;
            self.edges.push(edge);
            verify_frontier(
                self.edges.last().expect("just started the edge"),
                chain_id,
                GATEWAY_ID,
                &frontier,
            )
            .await?;
        }
        let gateway = self.gateway();
        ensure!(
            gateway
                .l2_provider
                .get_balance(gateway.l2_wallet.default_signer().address())
                .await?
                > U256::ZERO,
            "canonical Gateway probe wallet was not funded at capture"
        );
        self.ensure_running()
    }

    async fn start_node(
        &self,
        l1: &TestL1,
        role: &str,
        chain_id: u64,
        gateway: Option<(&str, usize)>,
    ) -> anyhow::Result<Tester> {
        let role_dir = self.supervisor.dir.path().join(role);
        let data_dir = role_dir.join("rocksdb");
        ensure!(
            symlink_metadata(data_dir.join("database_identity.json"))?
                .file_type()
                .is_file(),
            "canonical {role} database has no captured deployment identity"
        );
        let mut config = load_config_from_paths(&[role_dir.join("config.json")]).await;
        let expected_pubdata = if gateway.is_some() {
            PubdataMode::RelayedL2Calldata
        } else {
            PubdataMode::Blobs
        };
        ensure!(
            config.general_config.node_role.is_main()
                && config.general_config.gateway_chain_id == GATEWAY_ID
                && config.genesis_config.chain_id == Some(chain_id)
                && config.l1_sender_config.pubdata_mode == Some(expected_pubdata)
                && config.batcher_config.enabled
                && config.prover_api_config.enabled
                && config.status_server_config.enabled
                && matches!(
                    &config.external_price_api_client_config,
                    Some(ExternalPriceApiClientConfig::Forced { .. })
                )
                && !config.prover_api_config.fake_fri_provers.enabled
                && !config.prover_api_config.fake_snark_provers.enabled
                && !config.general_config.ephemeral
                && config.general_config.ephemeral_state.is_none(),
            "canonical {role} config changes the reviewed role, DA, or production proving policy"
        );
        config.general_config.rocks_db_path = data_dir;
        config.genesis_config.genesis_input_path = Some(role_dir.join("genesis.json"));
        match &mut config.replay_archive_config {
            ReplayArchiveConfig::FileSystem { root_path, .. } => {
                *root_path = role_dir.join("replay_archive");
            }
            ReplayArchiveConfig::Noop => {}
            _ => anyhow::bail!("canonical {role} must use private local replay storage"),
        }
        config.prover_api_config.proof_storage.path = role_dir.join("proof_storage");
        let l1_provider = ProviderConfig::new(&l1.address, TEST_PROVIDER_POLL_INTERVAL);
        config.l1_provider_config = l1_provider.clone();
        config.l1_archive_provider_config = Some(l1_provider);
        config.gateway_provider_config =
            gateway.map(|(rpc, _)| ProviderConfig::new(rpc, TEST_PROVIDER_POLL_INTERVAL));
        config.l1_sender_config.poll_interval = TEST_PROVIDER_POLL_INTERVAL;
        config.network_config.boot_nodes.clear();
        config.service_publication_config.enabled = false;
        config.sequencer_config.block_timestamp_offset_seconds = l1.timestamp_offset_seconds;
        config.batcher_config.bitcoin_da_rpc_url = Some(self.core.endpoint.to_string());
        config.batcher_config.bitcoin_da_rpc_user =
            Some(SecretString::new(self.core.user.clone().into()));
        config.batcher_config.bitcoin_da_rpc_password =
            Some(SecretString::new(self.core.password.clone().into()));
        config.batcher_config.bitcoin_da_wallet_name = self.core.wallet_name.clone();
        // The owned fixture validates native DA. Keep the fallback local and unable to serve
        // PoDA requests instead of fetching from a public index for a private signing domain.
        config.batcher_config.bitcoin_da_poda_url = self.core.endpoint.to_string();
        config.batcher_config.bitcoin_da_finality_mode = BitcoinDaFinalityMode::Confirmations;
        config.batcher_config.bitcoin_da_finality_confirmations = REQUIRED_DA_CONFIRMATIONS;
        config.batch_verification_config.server_enabled = false;
        config.batch_verification_config.client_enabled = false;
        config.batch_verification_config.max_batch_diff_to_upstream = None;
        if gateway.is_none() {
            config.prover_api_config.auth_user = None;
            config.prover_api_config.auth_password = None;
            config.prover_api_config.max_fris_per_snark = 2;
            config.prover_api_config.target_fris_per_snark = 2;
            config.prover_api_config.fri_job_timeout = Duration::from_secs(48 * 3600);
            config.prover_api_config.snark_job_timeout = Duration::from_secs(48 * 3600);
            config.batcher_config.batch_timeout = Duration::from_millis(100);
        }
        Tester::bind_runtime_endpoints(l1, &mut config);
        config.rpc_config.address = "127.0.0.1:0".to_owned();
        config.status_server_config.address = "127.0.0.1:0".to_owned();
        config
            .validate()
            .await
            .with_context(|| format!("invalid canonical {role} runtime config"))?;
        let protocol_version = PROTOCOL_VERSION_V32_0;
        let fixture_scope = FixtureScope::CanonicalSyscoin;
        let layout = if let Some((gateway_rpc, index)) = gateway {
            wait_for_gateway_readiness(l1, gateway_rpc, &config).await?;
            ChainLayout::GatewayChain {
                protocol_version,
                fixture_scope,
                chain_index: index,
            }
        } else {
            ChainLayout::Gateway {
                protocol_version,
                fixture_scope,
            }
        };
        // Do not deposit/fund automatically. The restored state must be verified before probes.
        Tester::launch_node_inner(
            l1.clone(),
            config,
            Arc::clone(&self.supervisor.dir),
            layout,
            None,
            false,
            None,
        )
        .await
    }

    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        let mut result = Ok(());
        for node in self.edges.drain(..).rev().chain(self.gateway.take()) {
            if let Err(error) = node.shutdown().await {
                result = Err(error);
            }
        }
        let backend_result = self.supervisor.shutdown().await;
        result.and(backend_result)
    }

    /// Sample fresh Gateway batch publication and confirm it on both native protocols.
    pub async fn verify_gateway_da(&self, batch: u64) -> anyhow::Result<()> {
        let published_path = self
            .gateway()
            .config()
            .general_config
            .rocks_db_path
            .join("bitcoin_da_status")
            .join(format!("batch_{batch}.json"));
        #[derive(Deserialize)]
        struct BatchDaRecord {
            expected_hashes: Vec<String>,
            published_hashes: Vec<String>,
        }
        let record: BatchDaRecord =
            serde_json::from_slice(&read_small_file(&published_path, 64 * 1024)?)?;
        ensure!(
            !record.expected_hashes.is_empty() && record.expected_hashes == record.published_hashes,
            "canonical Gateway probe has no complete native DA publication record"
        );
        for hash in record.published_hashes {
            let expected = hex::decode(&hash)?;
            ensure!(expected.len() == 32, "invalid canonical DA version hash");
            loop {
                let metadata: Value = self
                    .core
                    .call(false, "getnevmblobdata", json!([hash, true]))
                    .await?;
                let height = metadata["height"].as_u64().unwrap_or_default();
                if height > 0 {
                    let tip = self
                        .core
                        .call(false, "getblockcount", json!([]))
                        .await?
                        .as_u64()
                        .context("owned Core returned an invalid block height")?;
                    if tip
                        .checked_sub(height)
                        .is_some_and(|delta| delta >= REQUIRED_DA_CONFIRMATIONS - 1)
                    {
                        let block = self
                            .core
                            .call(false, "getblockhash", json!([height]))
                            .await?;
                        let block_hash = block
                            .as_str()
                            .context("owned Core returned an invalid block hash")?;
                        ensure!(
                            metadata["blockhash"].as_str() == Some(block_hash),
                            "native DA blob block is not canonical"
                        );
                        let payload = hex::decode(
                            metadata["data"]
                                .as_str()
                                .context("native DA payload missing")?,
                        )?;
                        ensure!(
                            Blake2s256::digest(payload)[..] == expected,
                            "native DA payload does not match its version hash"
                        );
                        let geth: String = self
                            .gateway()
                            .l1_provider()
                            .client()
                            .request(
                                "eth_call",
                                (
                                    json!({"to": DA_PRECOMPILE, "data": format!("0x{hash}")}),
                                    "latest",
                                ),
                            )
                            .await?;
                        ensure!(
                            geth == format!("0x{hash}"),
                            "owned NEVM did not expose the confirmed native DA blob"
                        );
                        break;
                    }
                }
                sleep(POLL_INTERVAL).await;
            }
        }
        Ok(())
    }
}

async fn verify_frontier(
    tester: &Tester,
    chain_id: u64,
    settlement_id: u64,
    frontier: &Frontier,
) -> anyhow::Result<()> {
    ensure!(
        tester.l2_provider.get_chain_id().await? == chain_id,
        "restored rollup RPC returned the wrong chain ID"
    );
    let block = tester
        .l2_provider
        .get_block_by_number(frontier.block_number.into())
        .await?
        .context("authenticated stopped-boundary block is absent after restore")?;
    ensure!(
        block.header.hash == frontier.block_hash
            && tester.l2_zk_provider.get_batch_number().await? >= frontier.batch_number,
        "restored chain {chain_id} disagrees with the authenticated stopped boundary"
    );
    let state = fetch_l1_state(tester).await?;
    ensure!(
        state.l1_chain_id == L1_CHAIN_ID && state.sl_chain_id == settlement_id,
        "restored chain {chain_id} has the wrong settlement route"
    );
    let block_id = state.sl_block_number.into();
    let verifier = state.diamond_proxy_sl.get_verifier(block_id).await?;
    let (testnet_verifier, vk) = state
        .diamond_proxy_sl
        .get_zksync_os_verifier_mode(verifier, block_id)
        .await?;
    let expected_vk = ProvingVersion::V8.vk_hash().parse::<B256>()?;
    ensure!(
        verifier != Address::ZERO && !testnet_verifier && vk == expected_vk && vk != B256::ZERO,
        "restored chain {chain_id} does not select the compiled production V8 verifier"
    );
    Ok(())
}
