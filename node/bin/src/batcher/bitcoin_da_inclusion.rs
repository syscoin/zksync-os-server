//! Native journal entries locate candidate Core blocks; only canonical Core receipts establish inclusion.
use crate::config::BatcherConfig;
use alloy::eips::BlockId;
use alloy::hex;
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, B256, Bytes, U64};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use anyhow::{Context, Result, bail, ensure};
use secrecy::ExposeSecret;
use serde_json::{Value, json};
use std::collections::{BTreeSet, VecDeque};
use std::time::Duration;
use tokio::sync::Mutex;
use zksync_os_provider::NodeProvider;

const MAX_CORE_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_COMMAND_HASHES: usize = 64;
const MAX_LOCATORS: usize = 256;
const PAGE_BLOCKS: u64 = 1024;
const MAX_PAGES: usize = 50;
const RETAINED_HEIGHT_DISTANCE: u64 = 50_000;
const EXPECTED_CHAIN_ID: u64 = 5700;
const DATA_HASH_PRECOMPILE: Address = Address::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x63,
]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Head {
    number: u64,
    hash: B256,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Locator {
    data_hash: B256,
    nevm_number: u64,
    nevm_hash: B256,
    sys_hash: B256,
}

#[derive(Default)]
struct SearchState {
    command: Vec<B256>,
    wanted: BTreeSet<B256>,
    head: Option<Head>,
    before: Option<u64>,
    pages: usize,
    exhausted: bool,
    locators: VecDeque<Locator>,
}

impl SearchState {
    fn restart(&mut self, head: Head) {
        self.head = Some(head);
        self.before = None;
        self.pages = 0;
        self.exhausted = false;
    }

    fn remember(&mut self, locator: Locator) {
        if !self.locators.contains(&locator) {
            if self.locators.len() == MAX_LOCATORS {
                self.locators.pop_front();
            }
            self.locators.push_back(locator);
        }
    }

    fn reject(&mut self, locator: Locator) {
        self.locators.retain(|entry| *entry != locator);
        // A page exposes only its newest locator per hash. Resume below a rejected locator
        // so an older valid publication in that same page is not silently skipped.
        if self.pages > 0 && locator.nevm_number > 0 {
            let retry_before = locator.nevm_number - 1;
            if self.before.is_none_or(|before| retry_before > before) {
                self.before = Some(retry_before);
                self.exhausted = false;
            }
        }
    }
}

struct CoreReadClient {
    http: reqwest::Client,
    url: reqwest::Url,
    user: String,
    password: String,
}

impl CoreReadClient {
    fn new(config: &BatcherConfig) -> Result<Self> {
        let url = reqwest::Url::parse(
            config
                .bitcoin_da_rpc_url
                .as_deref()
                .context("Core RPC URL missing")?,
        )
        .map_err(|_| anyhow::anyhow!("invalid Core RPC URL"))?;
        ensure!(
            matches!(url.scheme(), "http" | "https"),
            "invalid Core RPC URL scheme"
        );
        ensure!(
            !url.path_segments()
                .is_some_and(|mut parts| parts.any(|part| part == "wallet")),
            "Core inclusion reads cannot use a wallet URL"
        );
        let http = reqwest::Client::builder()
            .timeout(config.bitcoin_da_request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("failed to construct Core read client")?;
        Ok(Self {
            http,
            url,
            user: config
                .bitcoin_da_rpc_user
                .as_ref()
                .context("Core RPC user missing")?
                .expose_secret()
                .to_owned(),
            password: config
                .bitcoin_da_rpc_password
                .as_ref()
                .context("Core RPC password missing")?
                .expose_secret()
                .to_owned(),
        })
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        ensure!(
            matches!(
                method,
                "getblock" | "getblockheader" | "getblockhash" | "getblockchaininfo"
            ),
            "unsupported Core inclusion read"
        );
        let mut response = self
            .http
            .post(self.url.clone())
            .basic_auth(&self.user, Some(&self.password))
            .json(&json!({"jsonrpc":"2.0", "id":1, "method":method, "params":params}))
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Core inclusion request failed"))?;
        ensure!(
            response.status().is_success(),
            "Core inclusion HTTP request failed"
        );
        ensure!(
            response
                .content_length()
                .is_none_or(|length| length <= MAX_CORE_RESPONSE_BYTES as u64),
            "Core inclusion response exceeds bound"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("Core inclusion response read failed"))?
        {
            ensure!(
                bytes
                    .len()
                    .checked_add(chunk.len())
                    .is_some_and(|size| size <= MAX_CORE_RESPONSE_BYTES),
                "Core inclusion response exceeds bound"
            );
            bytes.extend_from_slice(&chunk);
        }
        let response: Value =
            serde_json::from_slice(&bytes).context("malformed Core inclusion response")?;
        ensure!(
            response.is_object() && response["id"] == 1 && response["error"].is_null(),
            "Core inclusion RPC failed"
        );
        response
            .get("result")
            .cloned()
            .context("Core inclusion RPC result missing")
    }
}

pub(super) struct CanonicalInclusionResolver {
    provider: NodeProvider,
    core: CoreReadClient,
    timeout: Duration,
    state: Mutex<SearchState>,
}

impl CanonicalInclusionResolver {
    pub(super) fn new(provider: NodeProvider, config: &BatcherConfig) -> Result<Self> {
        ensure!(
            !config.bitcoin_da_request_timeout.is_zero(),
            "Core inclusion timeout is zero"
        );
        Ok(Self {
            provider,
            core: CoreReadClient::new(config)?,
            timeout: config.bitcoin_da_request_timeout,
            state: Mutex::new(SearchState::default()),
        })
    }

    pub(super) async fn prepare_command(&self, hashes: &BTreeSet<String>) -> Result<()> {
        let mut command = BTreeSet::new();
        for hash in hashes {
            let encoded = hash.strip_prefix("0x").unwrap_or(hash);
            command.insert(core_hash(&json!(encoded), "requested data hash")?);
        }
        let command: Vec<_> = command.into_iter().collect();
        let mut state = self.state.lock().await;
        if state.command != command {
            state.wanted = command.iter().take(MAX_COMMAND_HASHES).copied().collect();
            state.command = command;
            state.head = None;
            state.before = None;
            state.pages = 0;
            state.exhausted = false;
        }
        Ok(())
    }

    pub(super) async fn resolve(&self, hash: B256) -> Result<bool> {
        tokio::time::timeout(self.timeout, self.resolve_inner(hash))
            .await
            .context("canonical Core inclusion lookup timed out")?
    }

    async fn nevm_head(&self, block: Value) -> Result<Head> {
        let value: Value = self
            .provider
            .raw_request("eth_getBlockByNumber".into(), (block, false))
            .await
            .context("NEVM canonical header lookup failed")?;
        Ok(Head {
            number: quantity(&value["number"], "NEVM block number")?,
            hash: eth_hash(&value["hash"], "NEVM block hash")?,
        })
    }

    async fn current_membership(&self, hash: B256) -> Result<bool> {
        let request = TransactionRequest::default()
            .with_to(DATA_HASH_PRECOMPILE)
            .with_input(Bytes::copy_from_slice(hash.as_slice()));
        let output = self
            .provider
            .call(request)
            .block(BlockId::latest())
            .await
            .context("NEVM current membership lookup failed")?;
        ensure!(
            output.is_empty() || output.as_ref() == hash.as_slice(),
            "NEVM current membership returned malformed data"
        );
        Ok(!output.is_empty())
    }

    async fn validate_locator(&self, locator: Locator) -> Result<bool> {
        let nevm = self
            .nevm_head(json!(U64::from(locator.nevm_number)))
            .await?;
        if nevm.number != locator.nevm_number || nevm.hash != locator.nevm_hash {
            return Ok(false);
        }
        let block = self
            .core
            .call("getblock", json!([hex::encode(locator.sys_hash), 2]))
            .await?;
        let inclusion = match validate_core_inclusion(&block, locator.sys_hash, locator.data_hash) {
            Ok(inclusion) => inclusion,
            Err(_) => return Ok(false),
        };
        let canonical = self
            .core
            .call("getblockhash", json!([inclusion.height]))
            .await?;
        if core_hash(&canonical, "canonical block hash")? != locator.sys_hash {
            return Ok(false);
        }
        let header = self
            .core
            .call(
                "getblockheader",
                json!([hex::encode(locator.sys_hash), true]),
            )
            .await?;
        let header = match validate_core_header(&header, locator.sys_hash) {
            Ok(header) => header,
            Err(_) => return Ok(false),
        };
        if header.height != inclusion.height {
            return Ok(false);
        }
        if !self.current_membership(locator.data_hash).await? {
            return Ok(false);
        }
        // Recheck both canonical mappings after all content reads; locators never cache finality.
        let canonical = self
            .core
            .call("getblockhash", json!([inclusion.height]))
            .await?;
        let nevm = self
            .nevm_head(json!(U64::from(locator.nevm_number)))
            .await?;
        Ok(
            core_hash(&canonical, "canonical block hash")? == locator.sys_hash
                && nevm.number == locator.nevm_number
                && nevm.hash == locator.nevm_hash,
        )
    }

    async fn resolve_inner(&self, hash: B256) -> Result<bool> {
        let mut state = self.state.lock().await;
        ensure!(hash != B256::ZERO, "Core inclusion data hash is zero");
        if !state.wanted.contains(&hash) {
            // A commit can combine several per-batch hash bounds. Page each group together,
            // without making the RPC's 64-hash limit a new limit on valid commit commands.
            state.wanted = state
                .command
                .chunks(MAX_COMMAND_HASHES)
                .find(|group| group.contains(&hash))
                .map(|group| group.iter().copied().collect())
                .unwrap_or_else(|| BTreeSet::from([hash]));
            state.head = None;
        }
        ensure!(
            self.provider.get_chain_id().await? == EXPECTED_CHAIN_ID,
            "NEVM inclusion provider is not Tanenbaum"
        );
        let chain = self.core.call("getblockchaininfo", json!([])).await?;
        ensure!(
            chain["chain"].as_str() == Some("test"),
            "Core inclusion provider is not testnet"
        );
        let current_head = self.nevm_head(json!("latest")).await?;
        if state.head != Some(current_head) {
            state.restart(current_head);
        }
        let mut attempted = BTreeSet::new();
        loop {
            let candidates: Vec<_> = state
                .locators
                .iter()
                .copied()
                .filter(|locator| {
                    locator.data_hash == hash
                        && !attempted.contains(&(
                            locator.nevm_number,
                            locator.nevm_hash,
                            locator.sys_hash,
                        ))
                })
                .collect();
            for locator in candidates {
                attempted.insert((locator.nevm_number, locator.nevm_hash, locator.sys_hash));
                if locator.nevm_number > current_head.number {
                    state.locators.retain(|entry| *entry != locator);
                    continue;
                }
                if self.validate_locator(locator).await? {
                    return Ok(true);
                }
                state.reject(locator);
            }
            if state.exhausted || state.pages >= MAX_PAGES {
                return Ok(false);
            }
            let page: Value = self
                .provider
                .raw_request(
                    "eth_getDataHashInclusions".into(),
                    (
                        state.wanted.iter().copied().collect::<Vec<_>>(),
                        state.before.map(U64::from),
                        U64::from(PAGE_BLOCKS),
                    ),
                )
                .await
                .context("NEVM inclusion journal lookup unavailable")?;
            let parsed = validate_page(&page, current_head, state.before, &state.wanted)?;
            // The response's claimed anchor must still be a canonical NEVM block.
            ensure!(
                self.nevm_head(json!(U64::from(current_head.number)))
                    .await?
                    == current_head,
                "NEVM inclusion scan anchor changed"
            );
            state.pages += 1;
            state.before = parsed.next_before;
            state.exhausted = parsed.exhausted;
            for locator in parsed.locators {
                state.remember(locator);
            }
        }
    }
}

struct Page {
    locators: Vec<Locator>,
    next_before: Option<u64>,
    exhausted: bool,
}

fn quantity(value: &Value, field: &str) -> Result<u64> {
    let value = value
        .as_str()
        .with_context(|| format!("{field} is not a hex quantity"))?;
    let digits = value
        .strip_prefix("0x")
        .with_context(|| format!("{field} is not a hex quantity"))?;
    ensure!(
        !digits.is_empty() && (digits.len() == 1 || !digits.starts_with('0')),
        "{field} is not a canonical hex quantity"
    );
    u64::from_str_radix(digits, 16).with_context(|| format!("{field} exceeds a u64 quantity"))
}

fn eth_hash(value: &Value, field: &str) -> Result<B256> {
    let value = value
        .as_str()
        .with_context(|| format!("{field} is not a hash"))?;
    let encoded = value
        .strip_prefix("0x")
        .with_context(|| format!("{field} is not a prefixed hash"))?;
    core_hash(&json!(encoded), field)
}

fn validate_page(
    value: &Value,
    head: Head,
    before: Option<u64>,
    wanted: &BTreeSet<B256>,
) -> Result<Page> {
    ensure!(value.is_object(), "NEVM inclusion page is not an object");
    ensure!(
        quantity(&value["chainId"], "journal chain ID")? == EXPECTED_CHAIN_ID,
        "NEVM inclusion journal is not Tanenbaum"
    );
    ensure!(
        quantity(&value["headNumber"], "journal head number")? == head.number
            && eth_hash(&value["headHash"], "journal head hash")? == head.hash,
        "NEVM inclusion journal anchor mismatch"
    );
    let exhausted = value["exhausted"]
        .as_bool()
        .context("journal exhausted flag is not boolean")?;
    let inclusions = value["inclusions"]
        .as_array()
        .context("journal inclusions is not an array")?;
    ensure!(
        inclusions.len() <= wanted.len(),
        "too many journal inclusion candidates"
    );
    let floor = head.number.saturating_sub(RETAINED_HEIGHT_DISTANCE).max(1);
    let from = before.unwrap_or(head.number);
    let optional_quantity = |name| -> Result<Option<u64>> {
        value
            .get(name)
            .filter(|field| !field.is_null())
            .map(|field| quantity(field, name))
            .transpose()
    };
    let next_before = optional_quantity("nextBefore")?;
    if from < floor {
        ensure!(
            inclusions.is_empty()
                && exhausted
                && next_before.is_none()
                && optional_quantity("searchedFrom")?.is_none()
                && optional_quantity("searchedTo")?.is_none(),
            "invalid empty journal scan"
        );
        return Ok(Page {
            locators: Vec::new(),
            next_before,
            exhausted,
        });
    }
    ensure!(from <= head.number, "journal cursor is ahead of its head");
    let to = from.saturating_sub(PAGE_BLOCKS - 1).max(floor);
    ensure!(
        optional_quantity("searchedFrom")? == Some(to)
            && optional_quantity("searchedTo")? == Some(from),
        "journal scan range mismatch"
    );
    let expected_next = if to > floor { Some(to - 1) } else { None };
    ensure!(
        next_before == expected_next && exhausted == expected_next.is_none(),
        "journal pagination does not advance within its bound"
    );
    let mut locators = Vec::new();
    let mut seen = BTreeSet::new();
    let mut previous_number = from;
    for entry in inclusions {
        ensure!(entry.is_object(), "journal candidate is not an object");
        let locator = Locator {
            data_hash: eth_hash(&entry["dataHash"], "candidate data hash")?,
            nevm_number: quantity(&entry["nevmBlockNumber"], "candidate NEVM number")?,
            nevm_hash: eth_hash(&entry["nevmBlockHash"], "candidate NEVM hash")?,
            sys_hash: eth_hash(&entry["syscoinBlockHash"], "candidate Core hash")?,
        };
        ensure!(
            wanted.contains(&locator.data_hash) && seen.insert(locator.data_hash),
            "unexpected or duplicate journal candidate"
        );
        ensure!(
            (to..=from).contains(&locator.nevm_number) && locator.nevm_number <= previous_number,
            "journal candidate height is outside descending scan range"
        );
        previous_number = locator.nevm_number;
        locators.push(locator);
    }
    Ok(Page {
        locators,
        next_before,
        exhausted,
    })
}

#[cfg(test)]
mod resolver_tests {
    use super::*;
    use crate::batcher::bitcoin_da_native_evidence::test_support::mocked_provider;
    use alloy::rpc::json_rpc::ErrorPayload;
    use alloy::transports::mock::Asserter;
    use axum::{Json, Router, routing::post};
    use std::sync::{Arc, Mutex as StdMutex};

    const HASH: B256 = B256::repeat_byte(0x22);
    const SYS_HASH: B256 = B256::repeat_byte(0x11);
    const NEVM_HASH: B256 = B256::repeat_byte(0x44);

    fn head_json() -> Value {
        json!({"number":"0x1","hash":NEVM_HASH})
    }
    fn candidate() -> Value {
        json!({"dataHash":HASH,"nevmBlockNumber":"0x1","nevmBlockHash":NEVM_HASH,"syscoinBlockHash":SYS_HASH})
    }
    fn page_json() -> Value {
        json!({"chainId":"0x1644","headNumber":"0x1","headHash":NEVM_HASH,
            "searchedFrom":"0x1","searchedTo":"0x1","inclusions":[candidate()],"exhausted":true})
    }
    fn empty_page() -> Value {
        json!({"chainId":"0x1644","headNumber":"0x1","headHash":NEVM_HASH,"inclusions":[],"exhausted":true})
    }
    fn core_block(confirmations: i64) -> Value {
        let script = [vec![0x6a, 34, 33, 1], HASH.as_slice().to_vec()].concat();
        json!({"hash":hex::encode(SYS_HASH),"height":123,"confirmations":confirmations,
            "tx":[{"txid":"33".repeat(32),"version":137,"vout":[{"n":0,"scriptPubKey":{"hex":hex::encode(script)}}]}]})
    }
    fn config(url: String) -> BatcherConfig {
        BatcherConfig {
            bitcoin_da_rpc_url: Some(url),
            bitcoin_da_rpc_user: Some("test-user".into()),
            bitcoin_da_rpc_password: Some("test-password".into()),
            bitcoin_da_request_timeout: Duration::from_secs(2),
            ..Default::default()
        }
    }
    async fn core_mock(
        responses: Vec<Value>,
    ) -> (
        String,
        tokio::task::JoinHandle<()>,
        Arc<StdMutex<Vec<String>>>,
    ) {
        let responses = Arc::new(StdMutex::new(VecDeque::from(responses)));
        let calls = Arc::new(StdMutex::new(Vec::new()));
        let recorded = calls.clone();
        let app = Router::new().route(
            "/",
            post(move |Json(request): Json<Value>| {
                let responses = responses.clone();
                let recorded = recorded.clone();
                async move {
                    recorded
                        .lock()
                        .unwrap()
                        .push(request["method"].as_str().unwrap().to_owned());
                    let result = responses
                        .lock()
                        .unwrap()
                        .pop_front()
                        .expect("unexpected Core request");
                    Json(json!({"jsonrpc":"2.0","id":1,"result":result,"error":null}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, task, calls)
    }
    fn push_initial_lookup(asserter: &Asserter) {
        asserter.push_success(&U64::from(EXPECTED_CHAIN_ID));
        asserter.push_success(&head_json());
        asserter.push_success(&page_json());
        asserter.push_success(&head_json());
        asserter.push_success(&head_json());
    }

    #[test]
    fn page_rejects_wrong_chain_anchor_range_candidate_and_pagination() {
        let head = Head {
            number: 1,
            hash: NEVM_HASH,
        };
        let wanted = BTreeSet::from([HASH]);
        assert!(validate_page(&page_json(), head, None, &wanted).is_ok());
        for (field, value) in [
            ("chainId", json!("0x1")),
            ("headHash", json!(SYS_HASH)),
            ("headNumber", json!("0x2")),
            ("searchedTo", json!("0x0")),
            ("exhausted", json!(false)),
            ("nextBefore", json!("0x0")),
            ("inclusions", json!({})),
        ] {
            let mut page = page_json();
            page[field] = value;
            assert!(
                validate_page(&page, head, None, &wanted).is_err(),
                "accepted {field}"
            );
        }
        for (field, value) in [
            ("dataHash", json!(SYS_HASH)),
            ("nevmBlockNumber", json!("0x2")),
            ("nevmBlockHash", json!("0x00")),
            ("syscoinBlockHash", json!(B256::ZERO)),
        ] {
            let mut page = page_json();
            page["inclusions"][0][field] = value;
            assert!(
                validate_page(&page, head, None, &wanted).is_err(),
                "accepted candidate {field}"
            );
        }
        let mut page = page_json();
        page["inclusions"].as_array_mut().unwrap().push(candidate());
        assert!(validate_page(&page, head, None, &wanted).is_err());
        assert!(validate_page(&empty_page(), head, Some(0), &wanted).is_ok());
        assert!(validate_page(&empty_page(), head, None, &wanted).is_err());
    }

    #[test]
    fn page_requires_exact_lower_upper_bounds_and_cursor_progress() {
        let head = Head {
            number: 2049,
            hash: NEVM_HASH,
        };
        let wanted = BTreeSet::from([HASH]);
        let mut page = json!({"chainId":"0x1644","headNumber":"0x801","headHash":NEVM_HASH,
            "searchedFrom":"0x402","searchedTo":"0x801","inclusions":[],
            "nextBefore":"0x401","exhausted":false});
        assert!(validate_page(&page, head, None, &wanted).is_ok());
        page["searchedFrom"] = json!("0x801");
        page["searchedTo"] = json!("0x402");
        assert!(validate_page(&page, head, None, &wanted).is_err());
        page["searchedFrom"] = json!("0x402");
        page["searchedTo"] = json!("0x801");
        page["nextBefore"] = json!("0x801");
        assert!(validate_page(&page, head, None, &wanted).is_err());
    }

    #[test]
    fn locator_cache_is_bounded_and_rejection_reopens_hidden_same_page_candidates() {
        let mut state = SearchState::default();
        for height in 1..=300 {
            state.remember(Locator {
                data_hash: HASH,
                nevm_number: height,
                nevm_hash: NEVM_HASH,
                sys_hash: SYS_HASH,
            });
        }
        assert_eq!(state.locators.len(), MAX_LOCATORS);
        assert_eq!(state.locators.front().unwrap().nevm_number, 45);
        state.before = Some(10);
        state.pages = 1;
        state.exhausted = true;
        state.reject(Locator {
            data_hash: HASH,
            nevm_number: 299,
            nevm_hash: NEVM_HASH,
            sys_hash: SYS_HASH,
        });
        assert_eq!(state.before, Some(298));
        assert!(!state.exhausted);
    }

    #[tokio::test]
    async fn large_commands_use_bounded_groups_and_unknown_requests_can_still_resolve() {
        let asserter = Asserter::new();
        let provider = mocked_provider(&asserter).await;
        let resolver =
            CanonicalInclusionResolver::new(provider, &config("http://127.0.0.1:1".into()))
                .unwrap();
        let hashes: BTreeSet<_> = (1u8..=70)
            .map(|value| B256::repeat_byte(value).to_string())
            .collect();
        resolver.prepare_command(&hashes).await.unwrap();
        {
            let state = resolver.state.lock().await;
            assert_eq!(state.command.len(), 70);
            assert_eq!(state.wanted.len(), MAX_COMMAND_HASHES);
        }
        // Fail the first network check after group selection, without any Core request.
        asserter.push_success(&U64::from(1));
        assert!(resolver.resolve(B256::repeat_byte(70)).await.is_err());
        assert_eq!(resolver.state.lock().await.wanted.len(), 6);
        asserter.push_success(&U64::from(1));
        assert!(resolver.resolve(B256::repeat_byte(255)).await.is_err());
        assert_eq!(
            resolver.state.lock().await.wanted,
            BTreeSet::from([B256::repeat_byte(255)])
        );
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test]
    async fn canonical_inclusion_is_rechecked_after_cached_success() {
        let asserter = Asserter::new();
        let provider = mocked_provider(&asserter).await;
        let (url, server, calls) = core_mock(vec![
            json!({"chain":"test"}),
            core_block(1),
            json!(hex::encode(SYS_HASH)),
            core_block(1),
            json!(hex::encode(SYS_HASH)),
            json!({"chain":"test"}),
            core_block(0),
        ])
        .await;
        let resolver = CanonicalInclusionResolver::new(provider, &config(url)).unwrap();
        resolver
            .prepare_command(&BTreeSet::from([HASH.to_string()]))
            .await
            .unwrap();
        push_initial_lookup(&asserter);
        asserter.push_success(&Bytes::copy_from_slice(HASH.as_slice()));
        asserter.push_success(&head_json());
        assert!(resolver.resolve(HASH).await.unwrap());
        asserter.push_success(&U64::from(EXPECTED_CHAIN_ID));
        asserter.push_success(&head_json());
        asserter.push_success(&head_json());
        asserter.push_success(&empty_page());
        asserter.push_success(&head_json());
        assert!(!resolver.resolve(HASH).await.unwrap());
        assert_eq!(
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|method| *method == "getblock")
                .count(),
            2
        );
        assert!(asserter.read_q().is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn canonical_receipt_cannot_replace_current_native_membership() {
        let asserter = Asserter::new();
        let provider = mocked_provider(&asserter).await;
        let (url, server, _) = core_mock(vec![
            json!({"chain":"test"}),
            core_block(1),
            json!(hex::encode(SYS_HASH)),
            core_block(1),
        ])
        .await;
        let resolver = CanonicalInclusionResolver::new(provider, &config(url)).unwrap();
        resolver
            .prepare_command(&BTreeSet::from([HASH.to_string()]))
            .await
            .unwrap();
        push_initial_lookup(&asserter);
        asserter.push_success(&Bytes::new());
        asserter.push_success(&empty_page());
        asserter.push_success(&head_json());
        assert!(!resolver.resolve(HASH).await.unwrap());
        assert!(asserter.read_q().is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn unavailable_journal_and_wrong_core_chain_fail_closed() {
        for wrong_chain in [false, true] {
            let asserter = Asserter::new();
            let provider = mocked_provider(&asserter).await;
            let (url, server, _) =
                core_mock(vec![json!({"chain":if wrong_chain {"main"} else {"test"}})]).await;
            let resolver = CanonicalInclusionResolver::new(provider, &config(url)).unwrap();
            resolver
                .prepare_command(&BTreeSet::from([HASH.to_string()]))
                .await
                .unwrap();
            asserter.push_success(&U64::from(EXPECTED_CHAIN_ID));
            if !wrong_chain {
                asserter.push_success(&head_json());
                asserter.push_failure(ErrorPayload::method_not_found());
            }
            assert!(resolver.resolve(HASH).await.is_err());
            assert!(asserter.read_q().is_empty());
            server.abort();
        }
    }

    #[tokio::test]
    async fn core_transport_refuses_wallet_methods_and_oversized_bodies() {
        let app = Router::new().route(
            "/",
            post(|| async { "x".repeat(MAX_CORE_RESPONSE_BYTES + 1) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = CoreReadClient::new(&config(url.clone())).unwrap();
        assert!(
            client
                .call("getblock", json!([hex::encode(SYS_HASH), 2]))
                .await
                .is_err()
        );
        assert!(client.call("sendrawtransaction", json!([])).await.is_err());
        assert!(CoreReadClient::new(&config(format!("{url}wallet/test"))).is_err());
        server.abort();
    }
}

const NEVM_DATA_TX_VERSION: u64 = 137;
const MAX_CORE_SCRIPT_BYTES: usize = 10_000;
const WITNESS_COMMITMENT_HEADER: [u8; 4] = [0xaa, 0x21, 0xa9, 0xed];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CoreHeader {
    pub(crate) height: u64,
    pub(crate) sys_hash: B256,
    pub(crate) confirmations: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CoreInclusion {
    pub(crate) height: u64,
    pub(crate) sys_hash: B256,
    pub(crate) txid: B256,
}

fn core_hash(value: &Value, field: &str) -> Result<B256> {
    let encoded = value
        .as_str()
        .with_context(|| format!("Core {field} is not a string"))?;
    // Core emits bare display-order hex; do not silently accept prefixes or variable lengths.
    ensure!(encoded.len() == 64, "Core {field} is not a 32-byte hash");
    let bytes = hex::decode(encoded).with_context(|| format!("Core {field} is not hex"))?;
    let hash = B256::from_slice(&bytes);
    ensure!(hash != B256::ZERO, "Core {field} is zero");
    Ok(hash)
}

pub(crate) fn validate_core_header(block: &Value, expected_sys_hash: B256) -> Result<CoreHeader> {
    ensure!(block.is_object(), "Core block is not an object");
    let sys_hash = core_hash(&block["hash"], "block hash")?;
    ensure!(
        sys_hash == expected_sys_hash,
        "Core block hash does not match candidate"
    );
    let height = block["height"]
        .as_u64()
        .context("Core block height is not a nonnegative integer")?;
    let confirmations = block["confirmations"]
        .as_u64()
        .context("Core block confirmations are not a nonnegative integer")?;
    ensure!(confirmations >= 1, "Core block is not confirmed");
    Ok(CoreHeader {
        height,
        sys_hash,
        confirmations,
    })
}

// Mirrors CScript::GetOp's push lengths, without accepting non-push opcodes or truncated tails.
// Nonminimal push encodings are allowed by Core's GetSyscoinData.
fn read_push<'a>(script: &'a [u8], position: &mut usize) -> Result<&'a [u8]> {
    let opcode = *script.get(*position).context("missing Core data push")?;
    *position += 1;
    let length_bytes = match opcode {
        0x00..=0x4b => 0,
        0x4c => 1,
        0x4d => 2,
        0x4e => 4,
        _ => bail!("Core data script contains a non-push opcode"),
    };
    let length = if length_bytes == 0 {
        usize::from(opcode)
    } else {
        let end = position
            .checked_add(length_bytes)
            .context("Core push length overflow")?;
        let encoded = script
            .get(*position..end)
            .context("truncated Core push length")?;
        *position = end;
        let mut length = [0u8; 4];
        length[..length_bytes].copy_from_slice(encoded);
        usize::try_from(u32::from_le_bytes(length)).context("Core push length exceeds usize")?
    };
    let end = position
        .checked_add(length)
        .context("Core push length overflow")?;
    let payload = script
        .get(*position..end)
        .context("truncated Core data push")?;
    *position = end;
    Ok(payload)
}

fn data_hash_from_script(script: &[u8]) -> Result<B256> {
    ensure!(
        script.len() <= MAX_CORE_SCRIPT_BYTES,
        "Core data script is oversized"
    );
    ensure!(
        script.first() == Some(&0x6a),
        "Core data output does not start with OP_RETURN"
    );
    let mut position = 1;
    let first = read_push(script, &mut position)?;
    let payload = if first.len() >= 36 && first.starts_with(&WITNESS_COMMITMENT_HEADER) {
        read_push(script, &mut position)?
    } else {
        first
    };
    ensure!(
        position == script.len(),
        "Core data script has trailing bytes"
    );
    // d738769 CNEVMData::Ser writes CompactSize(33), then type 1 and exactly 32 digest bytes.
    // Legacy 32-byte hashes and noncanonical CompactSize encodings are intentionally excluded.
    ensure!(
        payload.len() == 34 && payload[..2] == [33, 1],
        "Core data is not one serialized Blake2s version-1 digest"
    );
    Ok(B256::from_slice(&payload[2..]))
}

pub(crate) fn validate_core_inclusion(
    block: &Value,
    expected_sys_hash: B256,
    expected_data_hash: B256,
) -> Result<CoreInclusion> {
    let header = validate_core_header(block, expected_sys_hash)?;
    let transactions = block["tx"]
        .as_array()
        .context("Core block tx is not an array")?;
    let mut matching_txid = None;
    for transaction in transactions {
        ensure!(transaction.is_object(), "Core transaction is not an object");
        let txid = core_hash(&transaction["txid"], "transaction txid")?;
        let version = transaction["version"]
            .as_u64()
            .context("Core transaction version is not an unsigned integer")?;
        ensure!(
            version <= u64::from(u32::MAX),
            "Core transaction version exceeds u32"
        );
        let outputs = transaction["vout"]
            .as_array()
            .context("Core transaction vout is not an array")?;
        let mut first_unspendable = None;
        for (index, output) in outputs.iter().enumerate() {
            ensure!(
                output.is_object(),
                "Core transaction output is not an object"
            );
            ensure!(
                output["n"].as_u64() == Some(index as u64),
                "Core transaction output index is malformed"
            );
            let script_object = &output["scriptPubKey"];
            ensure!(
                script_object.is_object(),
                "Core scriptPubKey is not an object"
            );
            let encoded = script_object["hex"]
                .as_str()
                .context("Core script hex is not a string")?;
            ensure!(
                !encoded.starts_with("0x") && !encoded.starts_with("0X"),
                "Core script hex must be bare"
            );
            ensure!(
                encoded.len().is_multiple_of(2),
                "Core script hex has an odd length"
            );
            let script = hex::decode(encoded).context("Core script is not bare hex")?;
            // GetSyscoinDataOutput uses the first IsUnspendable output, not any later matching decoy.
            if first_unspendable.is_none()
                && (script.first() == Some(&0x6a) || script.len() > MAX_CORE_SCRIPT_BYTES)
            {
                first_unspendable = Some(script);
            }
        }
        if version == NEVM_DATA_TX_VERSION {
            if let Some(script) = first_unspendable {
                // Other NEVM publications in the same block may carry different supported hashes.
                // They are not evidence for this requested digest.
                if data_hash_from_script(&script).is_ok_and(|hash| hash == expected_data_hash) {
                    matching_txid.get_or_insert(txid);
                }
            }
        }
    }
    let txid =
        matching_txid.context("Core block contains no matching version-1 NEVM data transaction")?;
    Ok(CoreInclusion {
        height: header.height,
        sys_hash: header.sys_hash,
        txid,
    })
}

#[cfg(test)]
mod receipt_tests {
    use super::*;
    use serde_json::json;

    fn sys_hash() -> B256 {
        B256::repeat_byte(0x11)
    }
    fn data_hash() -> B256 {
        B256::repeat_byte(0x22)
    }
    fn txid() -> B256 {
        B256::repeat_byte(0x33)
    }
    fn hash_hex(hash: B256) -> String {
        hex::encode(hash)
    }
    fn payload() -> Vec<u8> {
        [vec![33, 1], data_hash().as_slice().to_vec()].concat()
    }
    fn ordinary_script() -> Vec<u8> {
        [vec![0x6a, 34], payload()].concat()
    }
    fn output(script: &[u8], n: u64) -> Value {
        json!({"n":n,"value":0,"scriptPubKey":{"hex":hex::encode(script),"type":"nulldata"}})
    }
    fn block_with_script(script: &[u8]) -> Value {
        json!({"hash":hash_hex(sys_hash()),"height":123,"confirmations":1,
            "tx":[{"txid":hash_hex(txid()),"version":137,"vout":[output(script,0)]}]})
    }
    fn valid_block() -> Value {
        block_with_script(&ordinary_script())
    }
    fn check(block: &Value) -> Result<CoreInclusion> {
        validate_core_inclusion(block, sys_hash(), data_hash())
    }

    #[test]
    fn accepts_exact_core_version_one_publication() {
        assert_eq!(
            check(&valid_block()).unwrap(),
            CoreInclusion {
                height: 123,
                sys_hash: sys_hash(),
                txid: txid()
            }
        );
    }

    #[test]
    fn accepts_core_witness_prefix_and_nonminimal_push() {
        let witness = [WITNESS_COMMITMENT_HEADER.to_vec(), vec![0x77; 32]].concat();
        let script = [vec![0x6a, 36], witness, vec![0x4c, 34], payload()].concat();
        let mut block = block_with_script(&script);
        block["confirmations"] = json!(7);
        block["tx"][0]["vout"]
            .as_array_mut()
            .unwrap()
            .insert(0, output(&[0x51], 0));
        block["tx"][0]["vout"][1]["n"] = json!(1);
        assert_eq!(check(&block).unwrap().txid, txid());
    }

    #[test]
    fn rejects_unconfirmed_or_malformed_header_fields() {
        for field in ["height", "confirmations"] {
            for value in [
                Value::Null,
                json!(-1),
                json!(1.5),
                json!("1"),
                json!(true),
                json!([]),
            ] {
                let mut block = valid_block();
                block[field] = value;
                assert!(
                    check(&block).is_err(),
                    "accepted malformed {field}: {block}"
                );
            }
        }
        let mut block = valid_block();
        block["confirmations"] = json!(0);
        assert!(check(&block).is_err());
        for value in [
            Value::Null,
            json!(7),
            json!("11"),
            json!("zz".repeat(32)),
            json!("0x".to_owned() + &hash_hex(sys_hash())),
            json!(hash_hex(data_hash())),
            json!("00".repeat(32)),
        ] {
            let mut block = valid_block();
            block["hash"] = value;
            assert!(check(&block).is_err());
        }
        assert!(check(&Value::Null).is_err());
    }

    #[test]
    fn rejects_malformed_transaction_and_output_fields() {
        for value in [Value::Null, json!("txid"), json!({})] {
            let mut block = valid_block();
            block["tx"] = value;
            assert!(check(&block).is_err());
        }
        for value in [Value::Null, json!("txid"), json!([])] {
            let mut block = valid_block();
            block["tx"][0] = value;
            assert!(check(&block).is_err());
        }
        for field in ["txid", "version", "vout"] {
            let mut block = valid_block();
            block["tx"][0][field] = Value::Null;
            assert!(check(&block).is_err());
        }
        for value in [
            json!("00".repeat(32)),
            json!("33"),
            json!("gg".repeat(32)),
            json!(true),
        ] {
            let mut block = valid_block();
            block["tx"][0]["txid"] = value;
            assert!(check(&block).is_err());
        }
        for value in [json!(-1), json!(137.0), json!("137"), json!(4294967296u64)] {
            let mut block = valid_block();
            block["tx"][0]["version"] = value;
            assert!(check(&block).is_err());
        }
        for field in ["n", "scriptPubKey"] {
            let mut block = valid_block();
            block["tx"][0]["vout"][0][field] = Value::Null;
            assert!(check(&block).is_err());
        }
        for value in [
            Value::Null,
            json!(false),
            json!("6a2"),
            json!("6axx"),
            json!("0x6a"),
        ] {
            let mut block = valid_block();
            block["tx"][0]["vout"][0]["scriptPubKey"]["hex"] = value;
            assert!(check(&block).is_err());
        }
        let mut block = valid_block();
        block["tx"][0]["vout"][0]["n"] = json!(1);
        assert!(check(&block).is_err());
    }

    #[test]
    fn rejects_wrong_version_hash_type_digest_and_legacy_payload() {
        let mut block = valid_block();
        block["tx"][0]["version"] = json!(2);
        assert!(check(&block).is_err());
        let mut script = ordinary_script();
        script[3] = 2;
        assert!(check(&block_with_script(&script)).is_err());
        let mut script = ordinary_script();
        script[4] ^= 1;
        assert!(check(&block_with_script(&script)).is_err());
        let legacy = [vec![0x6a, 33, 32], data_hash().as_slice().to_vec()].concat();
        assert!(check(&block_with_script(&legacy)).is_err());
        let unversioned = [vec![0x6a, 32], data_hash().as_slice().to_vec()].concat();
        assert!(check(&block_with_script(&unversioned)).is_err());
        let noncanonical_compact = [
            vec![0x6a, 36, 0xfd, 33, 0, 1],
            data_hash().as_slice().to_vec(),
        ]
        .concat();
        assert!(check(&block_with_script(&noncanonical_compact)).is_err());
    }

    #[test]
    fn rejects_wrong_script_truncated_pushes_and_every_trailing_form() {
        let mut script = ordinary_script();
        script[0] = 0x51;
        assert!(check(&block_with_script(&script)).is_err());
        for script in [
            vec![0x6a],
            vec![0x6a, 0x76],
            vec![0x6a, 0x4c],
            vec![0x6a, 0x4d, 34],
            vec![0x6a, 0x4e, 34, 0, 0],
            vec![0x6a, 34, 33, 1],
        ] {
            assert!(check(&block_with_script(&script)).is_err());
        }
        for suffix in [vec![0], vec![0x51], vec![0x4c], vec![1], vec![1, 0]] {
            let script = [ordinary_script(), suffix].concat();
            assert!(check(&block_with_script(&script)).is_err());
        }
        let mut script = ordinary_script();
        script[1] = 35;
        script.push(0);
        assert!(check(&block_with_script(&script)).is_err());
    }

    #[test]
    fn refuses_matching_later_output_after_first_unspendable_decoy() {
        for decoy in [vec![0x6a, 1, 0], vec![0x51; MAX_CORE_SCRIPT_BYTES + 1]] {
            let mut block = valid_block();
            block["tx"][0]["vout"] = json!([output(&decoy, 0), output(&ordinary_script(), 1)]);
            assert!(check(&block).is_err());
        }
    }

    #[test]
    fn does_not_return_before_later_malformed_transaction_is_checked() {
        let mut block = valid_block();
        block["tx"]
            .as_array_mut()
            .unwrap()
            .push(json!("not an object"));
        assert!(check(&block).is_err());
    }
}
