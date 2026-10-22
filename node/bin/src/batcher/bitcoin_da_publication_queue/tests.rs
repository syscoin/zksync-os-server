use super::*;
use async_trait::async_trait;
use bitcoin_da_client::SyscoinError;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

const GENESIS: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const TXID: &str = "2222222222222222222222222222222222222222222222222222222222222222";
const OLD_TXID: &str = "3333333333333333333333333333333333333333333333333333333333333333";
const BLOCK_HASH: &str = "4444444444444444444444444444444444444444444444444444444444444444";

#[test]
fn default_legacy_path_is_off_and_invalid_or_bypassed_shared_config_is_refused() {
    let legacy = BatcherConfig {
        bitcoin_da_publication_window: 1,
        ..Default::default()
    };
    assert!(
        BitcoinDaPublicationQueue::from_config(&legacy)
            .unwrap()
            .is_none()
    );
    for window in [0, 2, 5] {
        let config = BatcherConfig {
            bitcoin_da_publication_window: window,
            ..Default::default()
        };
        assert!(BitcoinDaPublicationQueue::from_config(&config).is_err());
    }
    let bypass = BatcherConfig {
        bitcoin_da_publication_window: 1,
        bitcoin_da_publication_state_path: Some(PathBuf::from("/must-not-bypass-shared-state")),
        ..Default::default()
    };
    assert!(BitcoinDaPublicationQueue::from_config(&bypass).is_err());
}

#[test]
fn directory_creation_accepts_existing_private_directory_and_retains_type_checks() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary
        .path()
        .canonicalize()
        .unwrap()
        .join("shared-wallet");
    private_directory(&directory).unwrap();
    // The second full initializer must execute and accept mkdir's AlreadyExists path.
    private_directory(&directory).unwrap();
    let file = directory.parent().unwrap().join("not-a-directory");
    std::fs::write(&file, b"corrupt path").unwrap();
    assert!(private_directory(&file).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let alias = directory.parent().unwrap().join("symlink-alias");
        symlink(&directory, &alias).unwrap();
        assert!(private_directory(&alias).is_err());
        let dangling = directory.parent().unwrap().join("dangling-alias");
        symlink(
            directory.parent().unwrap().join("missing-target"),
            &dangling,
        )
        .unwrap();
        assert!(private_directory(&dangling).is_err());
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_directory(&directory).is_err());
    }
}

#[test]
fn simultaneous_first_publication_coordinators_accept_the_same_private_directory() {
    for round in 0..8 {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary
            .path()
            .canonicalize()
            .unwrap()
            .join("shared-wallet");
        let config = BatcherConfig {
            bitcoin_da_publication_window: 2,
            bitcoin_da_publication_state_path: Some(directory.clone()),
            bitcoin_da_rpc_url: Some("http://127.0.0.1:1".into()),
            bitcoin_da_rpc_user: Some("test-user".into()),
            bitcoin_da_rpc_password: Some("test-password".into()),
            bitcoin_da_request_timeout: std::time::Duration::from_secs(2),
            bitcoin_da_wallet_name: "shared-test-wallet".into(),
            bitcoin_da_publication_mempool_count_limit: 20,
            bitcoin_da_publication_mempool_vbytes_limit: 75_000,
            bitcoin_da_publication_vbytes_reserve: 25_000,
            ..Default::default()
        };
        let barrier = std::sync::Barrier::new(8);
        let results = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        BitcoinDaPublicationQueue::from_config(&config)
                            .map(|queue| assert!(queue.is_some()))
                            .map_err(|error| error.to_string())
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(
            results.iter().all(Result::is_ok),
            "concurrent initialization round {round}: {results:?}"
        );
        private_directory(&directory).unwrap();
    }
}

#[tokio::test]
async fn wallet_receipt_transport_drops_sensitive_fields_and_bounds_the_response() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for (status, response_body, claimed_length, succeeds) in [
        ("200 OK", json!({"id":1,"result":{"txid":TXID,"confirmations":1,"blockhash":BLOCK_HASH,"blockheight":7,"hex":"private raw transaction","amount":123,"details":[{"address":"private wallet address"}]}}).to_string(), None, true),
        ("200 OK", json!({"id":2,"result":{"txid":TXID}}).to_string(), None, false),
        ("200 OK", "{}".to_owned(), Some(MAX_WALLET_RECEIPT_BYTES+1), false),
        ("500 Internal Server Error", "do not expose wallet body".to_owned(), None, false),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            socket.read(&mut request).await.unwrap();
            let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}", claimed_length.unwrap_or(response_body.len()));
            let _ = socket.write_all(response.as_bytes()).await;
        });
        let rpc = PublicationRpc {
            inner: RealRpcClient::new(&url, "user", "password", Some(std::time::Duration::from_secs(2)), "wallet").unwrap(),
            http: reqwest::Client::builder().timeout(std::time::Duration::from_secs(2)).build().unwrap(),
            wallet_url: format!("{url}/wallet/wallet"), user:"user".into(), password:"password".into(),
        };
        let result = rpc.call_wallet("gettransaction", &[json!(TXID), json!(false), json!(false)]).await;
        if succeeds {
            assert_eq!(result.unwrap(), json!({"txid":TXID,"confirmations":1,"blockhash":BLOCK_HASH,"blockheight":7}));
        } else { assert!(result.is_err()); }
        server.await.unwrap();
    }
}

enum WalletAction {
    Reply(Value),
    BlockResultSave(Value),
    Error,
    Wait {
        entered: Arc<Notify>,
        release: Arc<Notify>,
    },
}

#[derive(Clone)]
struct ReceiptFork {
    before_txid: String,
    lower_height: u64,
    lower_replacement: String,
    higher_height: u64,
    higher_hash: String,
}

struct FakeState {
    genesis: String,
    chain: Value,
    blocks: BTreeMap<u64, String>,
    block_reads: Vec<(u64, String)>,
    fork_before_receipt: Option<ReceiptFork>,
    receipts: BTreeMap<String, Value>,
    mempool_entries: BTreeMap<String, Value>,
    mempool: Value,
    actions: VecDeque<WalletAction>,
    wallet_calls: usize,
    receipt_reads: usize,
    sending_seen_by_wallet: Vec<Journal>,
}

#[derive(Clone)]
struct FakeRpc {
    directory: PathBuf,
    state: Arc<Mutex<FakeState>>,
}

#[async_trait]
impl RpcClient for FakeRpc {
    async fn call(&self, method: &str, params: &[Value]) -> Result<Value, SyscoinError> {
        let mut state = self.state.lock().unwrap();
        match method {
            "getblockhash" => {
                let height = params[0].as_u64().ok_or("invalid fake block height")?;
                let hash = if height == 0 {
                    state.genesis.clone()
                } else {
                    state
                        .blocks
                        .get(&height)
                        .cloned()
                        .ok_or("unknown fake block")?
                };
                state.block_reads.push((height, hash.clone()));
                Ok(json!(hash))
            }
            "getnevmblobdata" => Err("blob metadata has been pruned".into()),
            "getblockchaininfo" => Ok(state.chain.clone()),
            "getmempoolinfo" => Ok(state.mempool.clone()),
            "getmempoolentry" => state
                .mempool_entries
                .get(params[0].as_str().ok_or("invalid fake mempool txid")?)
                .cloned()
                .ok_or_else(|| "transaction is not in the fake mempool".into()),
            _ => Err(format!("unexpected fake RPC {method}").into()),
        }
    }

    async fn call_wallet(&self, method: &str, params: &[Value]) -> Result<Value, SyscoinError> {
        if method == "gettransaction" {
            assert_eq!(params.len(), 3);
            assert_eq!(params[1], json!(false));
            assert_eq!(params[2], json!(false));
            let txid = params[0].as_str().ok_or("invalid fake receipt txid")?;
            let mut state = self.state.lock().unwrap();
            state.receipt_reads += 1;
            if let Some(fork) = state.fork_before_receipt.clone() {
                if fork.before_txid == txid {
                    state
                        .blocks
                        .insert(fork.lower_height, fork.lower_replacement);
                    state
                        .blocks
                        .insert(fork.higher_height, fork.higher_hash.clone());
                    for receipt in state.receipts.values_mut() {
                        if receipt.get("blockheight").and_then(Value::as_u64)
                            == Some(fork.lower_height)
                        {
                            receipt["confirmations"] = json!(0);
                            let receipt = receipt.as_object_mut().unwrap();
                            receipt.remove("blockheight");
                            receipt.remove("blockhash");
                        }
                    }
                    state.receipts.insert(
                        txid.to_owned(),
                        json!({
                            "txid": txid, "confirmations": 1,
                            "blockheight": fork.higher_height, "blockhash": fork.higher_hash,
                        }),
                    );
                    state.fork_before_receipt = None;
                }
            }
            return Ok(state
                .receipts
                .get(txid)
                .cloned()
                .unwrap_or_else(|| json!({"txid": txid, "confirmations": 0})));
        }
        assert_eq!(method, "syscoincreatenevmblob");
        assert_eq!(params.len(), 3);
        assert_eq!(params[2], json!("blake2s"));
        let blob = hex::decode(params[0].as_str().ok_or("missing publication data")?)?;
        let version_hash = blob_hash(&blob);
        let action = {
            let mut state = self.state.lock().unwrap();
            state.wallet_calls += 1;
            let bytes = std::fs::read(self.directory.join("journal.json"))?;
            state
                .sending_seen_by_wallet
                .push(serde_json::from_slice(&bytes)?);
            state.actions.pop_front().unwrap_or_else(|| {
                WalletAction::Reply(json!({"versionhash": version_hash, "txid": TXID}))
            })
        };
        match action {
            WalletAction::Reply(reply) => Ok(reply),
            WalletAction::BlockResultSave(reply) => {
                std::fs::rename(
                    self.directory.join("journal.json"),
                    self.directory.join("saved-sending.json"),
                )?;
                std::fs::create_dir(self.directory.join("journal.json"))?;
                Ok(reply)
            }
            WalletAction::Error => Err("lost wallet response".into()),
            WalletAction::Wait { entered, release } => {
                entered.notify_one();
                release.notified().await;
                Ok(json!({"versionhash": version_hash, "txid": TXID}))
            }
        }
    }

    async fn get_balance(
        &self,
        _account: Option<&str>,
        _include_watchonly: Option<bool>,
    ) -> Result<f64, SyscoinError> {
        Err("balance must not be queried by publication coordination".into())
    }

    async fn http_get(&self, _url: &str) -> Result<Vec<u8>, SyscoinError> {
        Err("publication coordination must not fetch HTTP data".into())
    }
}

fn blob_hash(blob: &[u8]) -> String {
    hex::encode(Blake2s256::digest(blob))
}

fn fixture(window: usize) -> (tempfile::TempDir, BitcoinDaPublicationQueue<FakeRpc>) {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().canonicalize().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    private_directory(&directory).unwrap();
    let rpc = FakeRpc {
        directory: directory.clone(),
        state: Arc::new(Mutex::new(FakeState {
            genesis: GENESIS.to_owned(),
            chain: json!({"initialblockdownload": false}),
            blocks: BTreeMap::from([(7, BLOCK_HASH.to_owned())]),
            block_reads: vec![],
            fork_before_receipt: None,
            receipts: BTreeMap::new(),
            mempool_entries: BTreeMap::new(),
            mempool: json!({"loaded": true, "size": 0, "bytes": 0}),
            actions: VecDeque::new(),
            wallet_calls: 0,
            receipt_reads: 0,
            sending_seen_by_wallet: vec![],
        })),
    };
    let queue = BitcoinDaPublicationQueue {
        directory,
        policy: WalletPolicy {
            version: 1,
            wallet_identity: "test-wallet".to_owned(),
            window,
            count_limit: 25,
            vbytes_limit: 101_000,
            vbytes_reserve: 25_000,
        },
        rpc,
    };
    (temporary, queue)
}

fn reopened(queue: &BitcoinDaPublicationQueue<FakeRpc>) -> BitcoinDaPublicationQueue<FakeRpc> {
    BitcoinDaPublicationQueue {
        directory: queue.directory.clone(),
        policy: queue.policy.clone(),
        rpc: queue.rpc.clone(),
    }
}

fn journal(queue: &BitcoinDaPublicationQueue<FakeRpc>) -> Journal {
    serde_json::from_slice(&std::fs::read(queue.directory.join("journal.json")).unwrap()).unwrap()
}

async fn acquire_error(queue: &BitcoinDaPublicationQueue<FakeRpc>) -> String {
    match queue.try_acquire(None).await {
        Err(error) => error.to_string(),
        Ok(_) => panic!("expected acquisition to fail closed"),
    }
}

async fn submit(queue: &BitcoinDaPublicationQueue<FakeRpc>, blob: &[u8]) {
    let mut guard = queue.try_acquire(None).await.unwrap().unwrap();
    assert_eq!(
        guard.publish(blob, &blob_hash(blob), true).await.unwrap(),
        blob_hash(blob)
    );
}

#[tokio::test]
async fn publication_reserves_durable_unknown_entry_before_wallet_rpc() {
    let (_temporary, queue) = fixture(2);
    submit(&queue, b"durable publication").await;
    let state = queue.rpc.state.lock().unwrap();
    assert_eq!(state.wallet_calls, 1);
    assert_eq!(state.sending_seen_by_wallet.len(), 1);
    let before = &state.sending_seen_by_wallet[0];
    assert_eq!(before.policy, queue.policy);
    assert_eq!(before.genesis, GENESIS);
    assert_eq!(before.pending.len(), 1);
    assert_eq!(
        before.pending[0].version_hash,
        blob_hash(b"durable publication")
    );
    assert!(before.pending[0].txid.is_none());
    drop(state);
    assert_eq!(journal(&queue).pending[0].txid.as_deref(), Some(TXID));
}

#[tokio::test]
async fn ambiguous_wallet_error_fences_fresh_queue_without_another_rpc() {
    let (_temporary, queue) = fixture(2);
    queue
        .rpc
        .state
        .lock()
        .unwrap()
        .actions
        .push_back(WalletAction::Error);
    let mut guard = queue.try_acquire(None).await.unwrap().unwrap();
    assert!(
        guard
            .publish(b"ambiguous", &blob_hash(b"ambiguous"), true)
            .await
            .is_err()
    );
    assert!(
        guard
            .publish(b"another", &blob_hash(b"another"), true)
            .await
            .is_err()
    );
    assert_eq!(queue.rpc.state.lock().unwrap().wallet_calls, 1);
    drop(guard);
    queue.rpc.state.lock().unwrap().receipts.insert(
        TXID.to_owned(),
        json!({
            "txid": TXID, "confirmations": 1, "blockheight": 7, "blockhash": BLOCK_HASH,
        }),
    );
    let fresh = reopened(&queue);
    assert!(acquire_error(&fresh).await.contains("outcome is unknown"));
    assert_eq!(queue.rpc.state.lock().unwrap().wallet_calls, 1);
    assert!(journal(&fresh).pending[0].txid.is_none());
}

#[tokio::test]
async fn malformed_forced_results_retain_global_fence_after_restart() {
    let blob = b"malformed response";
    let expected = blob_hash(blob);
    for reply in [
        json!({"txid": TXID}),
        json!({"versionhash": OLD_TXID, "txid": TXID}),
        json!({"versionhash": expected}),
        json!({"versionhash": expected, "txid": "not-a-txid"}),
        json!({"versionhash": expected, "txid": null}),
    ] {
        let (_temporary, queue) = fixture(2);
        queue
            .rpc
            .state
            .lock()
            .unwrap()
            .actions
            .push_back(WalletAction::Reply(reply));
        let mut guard = queue.try_acquire(None).await.unwrap().unwrap();
        assert!(guard.publish(blob, &expected, true).await.is_err());
        drop(guard);
        assert!(
            acquire_error(&reopened(&queue))
                .await
                .contains("outcome is unknown")
        );
        assert_eq!(queue.rpc.state.lock().unwrap().wallet_calls, 1);
    }
}

#[tokio::test]
async fn cancelled_publication_retains_global_fence_after_restart() {
    let (_temporary, queue) = fixture(2);
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    queue
        .rpc
        .state
        .lock()
        .unwrap()
        .actions
        .push_back(WalletAction::Wait {
            entered: entered.clone(),
            release,
        });
    let worker = reopened(&queue);
    let publication = tokio::spawn(async move {
        let normal = worker.normal_turn().await.unwrap();
        let mut guard = worker.try_acquire(Some(&normal)).await.unwrap().unwrap();
        guard
            .publish(b"cancelled", &blob_hash(b"cancelled"), true)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let other_normal = queue.normal_turn().await.unwrap();
    assert!(
        queue
            .try_acquire(Some(&other_normal))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(queue.rpc.state.lock().unwrap().wallet_calls, 1);
    drop(other_normal);
    publication.abort();
    assert!(publication.await.unwrap_err().is_cancelled());
    assert!(
        acquire_error(&reopened(&queue))
            .await
            .contains("outcome is unknown")
    );
    assert_eq!(queue.rpc.state.lock().unwrap().wallet_calls, 1);
    assert!(journal(&queue).pending[0].txid.is_none());
}

#[tokio::test]
async fn failed_journal_reservation_never_calls_wallet() {
    let (_temporary, queue) = fixture(2);
    let mut guard = queue.try_acquire(None).await.unwrap().unwrap();
    let target = queue.directory.join("journal.json");
    std::fs::remove_file(&target).unwrap();
    std::fs::create_dir(&target).unwrap();
    assert!(
        guard
            .publish(b"unsaved", &blob_hash(b"unsaved"), true)
            .await
            .is_err()
    );
    assert_eq!(queue.rpc.state.lock().unwrap().wallet_calls, 0);
    drop(guard);
    assert!(reopened(&queue).try_acquire(None).await.is_err());
}

#[tokio::test]
async fn two_unconfirmed_submissions_fill_shared_window() {
    let (_temporary, queue) = fixture(2);
    submit(&queue, b"first").await;
    queue
        .rpc
        .state
        .lock()
        .unwrap()
        .actions
        .push_back(WalletAction::Reply(json!({
            "versionhash": blob_hash(b"second"), "txid": OLD_TXID,
        })));
    submit(&reopened(&queue), b"second").await;
    assert!(reopened(&queue).try_acquire(None).await.unwrap().is_none());
    assert_eq!(queue.rpc.state.lock().unwrap().wallet_calls, 2);
    assert_eq!(journal(&queue).pending.len(), 2);
}

#[tokio::test]
async fn fork_between_mined_receipts_cannot_release_an_earlier_noncanonical_publication() {
    let (_temporary, queue) = fixture(2);
    submit(&queue, b"independent lower publication").await;
    queue
        .rpc
        .state
        .lock()
        .unwrap()
        .actions
        .push_back(WalletAction::Reply(json!({
            "versionhash": blob_hash(b"independent higher publication"), "txid": OLD_TXID,
        })));
    submit(&queue, b"independent higher publication").await;
    let original_journal = std::fs::read(queue.directory.join("journal.json")).unwrap();
    let higher_hash = "55".repeat(32);
    {
        let mut state = queue.rpc.state.lock().unwrap();
        state.receipts.insert(
            TXID.to_owned(),
            json!({
                "txid": TXID, "confirmations": 1, "blockheight": 7, "blockhash": BLOCK_HASH,
            }),
        );
        state.receipts.insert(
            OLD_TXID.to_owned(),
            json!({
                "txid": OLD_TXID, "confirmations": 0,
            }),
        );
        assert!(
            !state.blocks.contains_key(&8),
            "before the fork the tip is seven and B is unconfirmed"
        );
        state.block_reads.clear();
        // A is canonical when its receipt is checked. Before B's receipt is returned,
        // switch to a fork where higher B is mined but independently funded lower A is not.
        state.fork_before_receipt = Some(ReceiptFork {
            before_txid: OLD_TXID.to_owned(),
            lower_height: 7,
            lower_replacement: OLD_TXID.to_owned(),
            higher_height: 8,
            higher_hash: higher_hash.clone(),
        });
    }
    let fresh = reopened(&queue);
    match fresh.try_acquire(None).await {
        Err(error) => assert!(error.to_string().contains("reorganized")),
        Ok(_) => {
            let incorrectly_released = journal(&queue);
            assert!(incorrectly_released.pending.is_empty());
            assert_eq!(
                incorrectly_released.mined_anchor,
                Some(BlockAnchor {
                    height: 8,
                    hash: higher_hash.clone()
                })
            );
            assert_eq!(
                queue.rpc.state.lock().unwrap().receipts[TXID]["confirmations"],
                0
            );
            panic!(
                "reconciliation released both pending entries although the lower publication is now unconfirmed"
            );
        }
    }
    assert_eq!(
        std::fs::read(queue.directory.join("journal.json")).unwrap(),
        original_journal
    );
    let state = queue.rpc.state.lock().unwrap();
    assert_eq!(
        state.wallet_calls, 2,
        "reconciliation must not issue another publication"
    );
    assert_eq!(
        state
            .block_reads
            .iter()
            .filter(|(height, _)| *height != 0)
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            (7, BLOCK_HASH.to_owned()),
            (8, higher_hash),
            (7, OLD_TXID.to_owned()),
        ]
    );
    assert_eq!(journal(&queue).pending.len(), 2);
    assert!(journal(&queue).mined_anchor.is_none());
}

#[tokio::test]
async fn duplicate_hash_and_alias_are_suppressed_without_wallet_call() {
    let (_temporary, queue) = fixture(2);
    let blob = b"same hash";
    let expected = blob_hash(blob);
    submit(&queue, blob).await;
    let mut guard = queue.try_acquire(None).await.unwrap().unwrap();
    let alias = format!("0x{}", expected.to_uppercase());
    assert!(guard.contains(&alias).unwrap());
    assert!(guard.publish(blob, &alias, true).await.is_err());
    assert_eq!(queue.rpc.state.lock().unwrap().wallet_calls, 1);
    assert_eq!(journal(&queue).pending.len(), 1);
}

#[tokio::test]
async fn wrong_wallet_receipt_txid_does_not_release_publication() {
    let (_temporary, queue) = fixture(2);
    let blob = b"republished hash";
    submit(&queue, blob).await;
    queue.rpc.state.lock().unwrap().receipts.insert(
        TXID.to_owned(),
        json!({
            "txid": OLD_TXID, "confirmations": 1, "blockheight": 7, "blockhash": BLOCK_HASH,
        }),
    );
    assert!(acquire_error(&queue).await.contains("txid mismatch"));
    assert_eq!(journal(&queue).pending.len(), 1);
    assert!(journal(&queue).mined_anchor.is_none());
}

#[tokio::test]
async fn pruned_blob_metadata_matching_wallet_receipt_releases_and_reorg_fences() {
    let (_temporary, queue) = fixture(2);
    let blob = b"mined publication";
    let expected = blob_hash(blob);
    submit(&queue, blob).await;
    assert!(
        queue
            .rpc
            .call("getnevmblobdata", &[json!(expected), json!(false)])
            .await
            .is_err()
    );
    queue.rpc.state.lock().unwrap().receipts.insert(
        TXID.to_owned(),
        json!({
            "txid": TXID, "confirmations": 1, "blockheight": 7, "blockhash": BLOCK_HASH,
        }),
    );
    let guard = queue.try_acquire(None).await.unwrap().unwrap();
    assert!(!guard.contains(&expected).unwrap());
    let saved = journal(&queue);
    assert!(saved.pending.is_empty());
    assert_eq!(
        saved.mined_anchor,
        Some(BlockAnchor {
            height: 7,
            hash: BLOCK_HASH.to_owned()
        })
    );
    drop(guard);
    queue
        .rpc
        .state
        .lock()
        .unwrap()
        .blocks
        .insert(7, OLD_TXID.to_owned());
    assert!(
        acquire_error(&reopened(&queue))
            .await
            .contains("reorganized")
    );
    assert_eq!(queue.rpc.state.lock().unwrap().wallet_calls, 1);
    assert_eq!(journal(&queue).mined_anchor, saved.mined_anchor);
}

#[tokio::test]
async fn mismatched_receipt_txid_or_noncanonical_mined_block_never_releases_slot() {
    for (receipt_txid, block_hash) in [(OLD_TXID, BLOCK_HASH), (TXID, OLD_TXID)] {
        let (_temporary, queue) = fixture(2);
        submit(&queue, b"exact").await;
        queue.rpc.state.lock().unwrap().receipts.insert(
            TXID.to_owned(),
            json!({
                "txid": receipt_txid, "confirmations": 1, "blockheight": 7, "blockhash": block_hash,
            }),
        );
        if block_hash == BLOCK_HASH {
            assert!(acquire_error(&queue).await.contains("txid mismatch"));
        } else {
            assert!(acquire_error(&queue).await.contains("reorganized"));
        }
        assert_eq!(journal(&queue).pending.len(), 1);
    }
}

#[tokio::test]
async fn negative_unconfirmed_or_malformed_wallet_receipts_retain_known_slot() {
    for receipt in [
        json!({"txid": TXID, "confirmations": -1, "blockheight": 7, "blockhash": BLOCK_HASH}),
        json!({"txid": TXID, "confirmations": 0, "blockheight": 7, "blockhash": BLOCK_HASH}),
        json!({"txid": TXID, "confirmations": "1", "blockheight": 7, "blockhash": BLOCK_HASH}),
        json!({"confirmations": 1, "blockheight": 7, "blockhash": BLOCK_HASH}),
        json!({"txid": TXID, "confirmations": 1, "blockhash": BLOCK_HASH}),
        json!({"txid": TXID, "confirmations": 1, "blockheight": "7", "blockhash": BLOCK_HASH}),
        json!({"txid": TXID, "confirmations": 1, "blockheight": 7, "blockhash": "not-a-hash"}),
    ] {
        let (_temporary, queue) = fixture(2);
        let expected = blob_hash(b"receipt validation");
        submit(&queue, b"receipt validation").await;
        let nonmined = receipt
            .get("confirmations")
            .and_then(Value::as_i64)
            .is_some_and(|count| count <= 0);
        queue
            .rpc
            .state
            .lock()
            .unwrap()
            .receipts
            .insert(TXID.to_owned(), receipt);
        if nonmined {
            let guard = queue.try_acquire(None).await.unwrap().unwrap();
            assert!(guard.contains(&expected).unwrap());
        } else {
            assert!(queue.try_acquire(None).await.is_err());
        }
        let saved = journal(&queue);
        assert_eq!(saved.pending.len(), 1);
        assert!(saved.mined_anchor.is_none());
        assert_eq!(queue.rpc.state.lock().unwrap().wallet_calls, 1);
    }
}

#[tokio::test]
async fn positive_mempool_prefilter_retains_slot_without_wallet_receipt_read() {
    let (_temporary, queue) = fixture(2);
    let expected = blob_hash(b"cheap mempool poll");
    submit(&queue, b"cheap mempool poll").await;
    {
        let mut state = queue.rpc.state.lock().unwrap();
        state
            .mempool_entries
            .insert(TXID.to_owned(), json!({"vsize": 149}));
        state.receipts.insert(
            TXID.to_owned(),
            json!({"invalid": "unavailable wallet receipt"}),
        );
    }
    let guard = queue.try_acquire(None).await.unwrap().unwrap();
    assert!(guard.contains(&expected).unwrap());
    assert_eq!(journal(&queue).pending.len(), 1);
    let state = queue.rpc.state.lock().unwrap();
    assert_eq!(state.receipt_reads, 0);
    assert_eq!(state.wallet_calls, 1);
}

#[tokio::test]
async fn configured_count_and_size_budgets_backpressure_before_publication() {
    for (count, bytes, permitted) in [
        (24, 76_000, true),
        (25, 0, false),
        (0, 76_001, false),
        (u64::MAX, 0, false),
        (0, u64::MAX, false),
    ] {
        let (_temporary, queue) = fixture(2);
        drop(queue.try_acquire(None).await.unwrap().unwrap());
        queue.rpc.state.lock().unwrap().mempool =
            json!({"loaded": true, "size": count, "bytes": bytes});
        let acquired = queue.try_acquire(None).await.unwrap();
        assert_eq!(
            acquired.is_some(),
            permitted,
            "count={count}, bytes={bytes}"
        );
        assert_eq!(queue.rpc.state.lock().unwrap().wallet_calls, 0);
        assert!(journal(&queue).pending.is_empty());
    }
}

#[tokio::test]
async fn malformed_headroom_and_changed_identity_or_configuration_fail_closed() {
    for mempool in [
        json!({}),
        json!({"loaded": true, "size": -1, "bytes": 0}),
        json!({"loaded": true, "size": 0, "bytes": "0"}),
    ] {
        let (_temporary, queue) = fixture(2);
        drop(queue.try_acquire(None).await.unwrap().unwrap());
        queue.rpc.state.lock().unwrap().mempool = mempool;
        assert!(queue.try_acquire(None).await.is_err());
        assert_eq!(queue.rpc.state.lock().unwrap().wallet_calls, 0);
    }
    for change in 0..4 {
        let (_temporary, queue) = fixture(2);
        drop(queue.try_acquire(None).await.unwrap().unwrap());
        let original = std::fs::read(queue.directory.join("journal.json")).unwrap();
        let mut fresh = reopened(&queue);
        match change {
            0 => fresh.policy.wallet_identity = "different-wallet".to_owned(),
            1 => fresh.policy.window = 3,
            2 => fresh.policy.vbytes_reserve += 1,
            _ => fresh.rpc.state.lock().unwrap().genesis = OLD_TXID.to_owned(),
        }
        assert!(acquire_error(&fresh).await.contains("mismatch"));
        assert_eq!(
            std::fs::read(queue.directory.join("journal.json")).unwrap(),
            original
        );
        assert_eq!(queue.rpc.state.lock().unwrap().wallet_calls, 0);
    }
}

#[tokio::test]
async fn waiting_normal_publisher_has_priority_over_recovery() {
    let (_temporary, queue) = fixture(2);
    let normal = queue.normal_turn().await.unwrap();
    assert!(queue.try_acquire(None).await.unwrap().is_none());
    drop(queue.try_acquire(Some(&normal)).await.unwrap().unwrap());
    assert!(queue.try_acquire(None).await.unwrap().is_none());
    drop(normal);
    assert!(queue.try_acquire(None).await.unwrap().is_some());
    assert_eq!(queue.rpc.state.lock().unwrap().wallet_calls, 0);
}

#[tokio::test]
async fn invalid_blob_never_reserves_or_calls_wallet_and_normal_idempotency_releases() {
    let (_temporary, queue) = fixture(2);
    let mut guard = queue.try_acquire(None).await.unwrap().unwrap();
    assert!(guard.publish(b"", &blob_hash(b""), true).await.is_err());
    assert!(
        guard
            .publish(b"wrong", &blob_hash(b"expected"), true)
            .await
            .is_err()
    );
    assert!(journal(&queue).pending.is_empty());
    assert_eq!(queue.rpc.state.lock().unwrap().wallet_calls, 0);
    let expected = blob_hash(b"already exists");
    queue
        .rpc
        .state
        .lock()
        .unwrap()
        .actions
        .push_back(WalletAction::Reply(json!({"versionhash": expected})));
    guard
        .publish(b"already exists", &expected, false)
        .await
        .unwrap();
    assert!(journal(&queue).pending.is_empty());
    drop(guard);
    assert!(reopened(&queue).try_acquire(None).await.unwrap().is_some());
}

#[tokio::test]
async fn corrupt_and_oversized_journals_fail_closed_without_wallet_calls() {
    for bytes in [
        b"invalid json".to_vec(),
        vec![b' '; MAX_JOURNAL_BYTES as usize + 1],
    ] {
        let (_temporary, queue) = fixture(2);
        drop(private_file(&queue.directory.join("journal.json")).unwrap());
        std::fs::write(queue.directory.join("journal.json"), bytes).unwrap();
        assert!(queue.try_acquire(None).await.is_err());
        assert_eq!(queue.rpc.state.lock().unwrap().wallet_calls, 0);
    }
}

#[tokio::test]
async fn chain_and_mempool_readiness_gate_bootstrap_and_existing_journals() {
    for initialized in [false, true] {
        let (_temporary, queue) = fixture(2);
        if initialized {
            drop(queue.try_acquire(None).await.unwrap().unwrap());
        }
        queue.rpc.state.lock().unwrap().chain = json!({"initialblockdownload": true});
        assert!(queue.try_acquire(None).await.unwrap().is_none());
        queue.rpc.state.lock().unwrap().chain = json!({});
        assert!(queue.try_acquire(None).await.is_err());
        queue.rpc.state.lock().unwrap().chain = json!({"initialblockdownload": false});
        queue.rpc.state.lock().unwrap().mempool = json!({"loaded": false, "size": 0, "bytes": 0});
        if initialized {
            assert!(queue.try_acquire(None).await.unwrap().is_none());
        } else {
            assert!(acquire_error(&queue).await.contains("fully loaded, empty"));
        }
        queue.rpc.state.lock().unwrap().mempool = json!({"size": 0, "bytes": 0});
        assert!(queue.try_acquire(None).await.is_err());
        assert_eq!(queue.rpc.state.lock().unwrap().wallet_calls, 0);
    }
    let (_temporary, queue) = fixture(2);
    queue.rpc.state.lock().unwrap().mempool = json!({"loaded": true, "size": 1, "bytes": 149});
    assert!(acquire_error(&queue).await.contains("fully loaded, empty"));
    assert!(!queue.directory.join("journal.json").exists());
}

#[tokio::test]
async fn failed_result_save_poisoned_guard_cannot_start_another_wallet_rpc() {
    let (_temporary, queue) = fixture(2);
    let expected = blob_hash(b"accepted but result unsaved");
    queue
        .rpc
        .state
        .lock()
        .unwrap()
        .actions
        .push_back(WalletAction::BlockResultSave(json!({
            "versionhash": expected, "txid": TXID,
        })));
    let mut guard = queue.try_acquire(None).await.unwrap().unwrap();
    assert!(
        guard
            .publish(b"accepted but result unsaved", &expected, true)
            .await
            .is_err()
    );
    assert!(
        guard
            .publish(b"must not publish", &blob_hash(b"must not publish"), true)
            .await
            .is_err()
    );
    assert_eq!(queue.rpc.state.lock().unwrap().wallet_calls, 1);
    drop(guard);
    std::fs::remove_dir(queue.directory.join("journal.json")).unwrap();
    std::fs::rename(
        queue.directory.join("saved-sending.json"),
        queue.directory.join("journal.json"),
    )
    .unwrap();
    assert!(
        acquire_error(&reopened(&queue))
            .await
            .contains("outcome is unknown")
    );
    assert!(journal(&queue).pending[0].txid.is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_and_fifo_journals_are_rejected_without_reading_or_wallet_calls() {
    use std::os::unix::ffi::OsStrExt;
    for fifo in [false, true] {
        let (_temporary, queue) = fixture(2);
        let path = queue.directory.join("journal.json");
        if fifo {
            let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            // SAFETY: CString remains alive for this call and contains a valid terminated path.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        } else {
            let target = queue.directory.join("target");
            std::fs::write(&target, b"must not read").unwrap();
            std::os::unix::fs::symlink(&target, &path).unwrap();
        }
        assert!(queue.try_acquire(None).await.is_err());
        assert_eq!(queue.rpc.state.lock().unwrap().wallet_calls, 0);
    }
}
