//! Opt-in, same-host wallet coordination. This is not a replacement for Core mempool policy.
use crate::config::BatcherConfig;
use alloy::hex;
use anyhow::Context;
use bitcoin_da_client::{MAX_BLOB_SIZE, RealRpcClient, RpcClient, SyscoinError};
use blake2::{Blake2s256, Digest};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MAX_JOURNAL_BYTES: u64 = 16 * 1024;
const MAX_WALLET_RECEIPT_BYTES: usize = 8 * 1024 * 1024;

// The pinned client logs generic response bodies. gettransaction includes raw transaction
// hex even with verbose=false; bound that one read and return only unlogged receipt metadata.
pub(crate) struct PublicationRpc {
    inner: RealRpcClient,
    http: reqwest::Client,
    wallet_url: String,
    user: String,
    password: String,
}

#[async_trait::async_trait]
impl RpcClient for PublicationRpc {
    async fn call(&self, method: &str, params: &[Value]) -> Result<Value, SyscoinError> {
        self.inner.call(method, params).await
    }

    async fn call_wallet(&self, method: &str, params: &[Value]) -> Result<Value, SyscoinError> {
        if method != "gettransaction" {
            return self.inner.call_wallet(method, params).await;
        }
        let mut response = self
            .http
            .post(&self.wallet_url)
            .basic_auth(&self.user, Some(&self.password))
            .json(&json!({"jsonrpc":"2.0", "id":1, "method":method, "params":params}))
            .send()
            .await?;
        if !response.status().is_success() {
            return Err("wallet receipt RPC failed".into());
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_WALLET_RECEIPT_BYTES as u64)
        {
            return Err("wallet receipt response exceeds bound".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes
                .len()
                .checked_add(chunk.len())
                .is_none_or(|size| size > MAX_WALLET_RECEIPT_BYTES)
            {
                return Err("wallet receipt response exceeds bound".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let response: Value = serde_json::from_slice(&bytes)?;
        if response["id"] != 1 || !response["error"].is_null() {
            return Err("invalid wallet receipt RPC result".into());
        }
        let result = response["result"]
            .as_object()
            .ok_or("wallet receipt result missing")?;
        let mut metadata = serde_json::Map::new();
        for name in ["txid", "confirmations", "blockhash", "blockheight"] {
            if let Some(value) = result.get(name) {
                metadata.insert(name.to_owned(), value.clone());
            }
        }
        Ok(Value::Object(metadata))
    }

    async fn get_balance(
        &self,
        account: Option<&str>,
        include_watchonly: Option<bool>,
    ) -> Result<f64, SyscoinError> {
        self.inner.get_balance(account, include_watchonly).await
    }

    async fn http_get(&self, url: &str) -> Result<Vec<u8>, SyscoinError> {
        self.inner.http_get(url).await
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WalletPolicy {
    version: u32,
    wallet_identity: String,
    window: usize,
    count_limit: u64,
    vbytes_limit: u64,
    vbytes_reserve: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockAnchor {
    height: u64,
    hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingPublication {
    version_hash: String,
    // None is a durable global fence, including cancellation before receiving the txid.
    txid: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    policy: WalletPolicy,
    genesis: String,
    mined_anchor: Option<BlockAnchor>,
    pending: Vec<PendingPublication>,
}

pub(crate) struct BitcoinDaPublicationQueue<R = PublicationRpc> {
    directory: PathBuf,
    policy: WalletPolicy,
    rpc: R,
}

pub(crate) struct NormalPublicationTurn(File);

pub(crate) struct PublicationGuard<'a, R> {
    queue: &'a BitcoinDaPublicationQueue<R>,
    _wallet_lock: File,
    _recovery_turn: Option<File>,
    journal: Journal,
    poisoned: bool,
}

fn hash32(value: &str) -> anyhow::Result<String> {
    let bytes = hex::decode(value.strip_prefix("0x").unwrap_or(value))
        .context("invalid publication hash")?;
    anyhow::ensure!(bytes.len() == 32, "invalid publication hash length");
    Ok(hex::encode(bytes))
}

fn private_file(path: &Path) -> anyhow::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        if let Ok(metadata) = std::fs::symlink_metadata(path) {
            anyhow::ensure!(
                metadata.permissions().mode() & 0o077 == 0,
                "publication coordinator files must be private"
            );
        }
    }
    let file = options.open(path)?;
    anyhow::ensure!(file.metadata()?.is_file(), "invalid publication lock file");
    Ok(file)
}

fn private_directory(directory: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        directory.is_absolute(),
        "publication directory must be absolute"
    );
    // Reject symlink aliases: every cooperating writer must lock the very same files.
    for parent in directory.ancestors() {
        if let Ok(metadata) = std::fs::symlink_metadata(parent) {
            anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "publication directory contains a symlink"
            );
        }
    }
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    if !directory.exists() {
        builder.create(directory)?;
        File::open(
            directory
                .parent()
                .context("publication directory has no parent")?,
        )?
        .sync_all()?;
    }
    let metadata = std::fs::symlink_metadata(directory)?;
    anyhow::ensure!(metadata.is_dir(), "invalid publication directory");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        anyhow::ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "publication directory must be private (0700)"
        );
    }
    Ok(())
}

impl BitcoinDaPublicationQueue {
    pub(crate) fn from_config(config: &BatcherConfig) -> anyhow::Result<Option<Self>> {
        anyhow::ensure!(
            (1..=4).contains(&config.bitcoin_da_publication_window),
            "publication window must be between one and four"
        );
        if config.bitcoin_da_publication_window == 1 {
            anyhow::ensure!(
                config.bitcoin_da_publication_state_path.is_none(),
                "configured shared publication state cannot be bypassed with legacy window one"
            );
            return Ok(None);
        }
        let directory = config
            .bitcoin_da_publication_state_path
            .clone()
            .context("bounded publication requires a shared state directory")?;
        private_directory(&directory)?;
        let rpc_url = config
            .bitcoin_da_rpc_url
            .as_deref()
            .context("missing Bitcoin DA RPC URL")?;
        let mut endpoint = url::Url::parse(rpc_url).context("invalid Bitcoin DA RPC URL")?;
        let _ = endpoint.set_username("");
        let _ = endpoint.set_password(None);
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        let identity = hex::encode(Blake2s256::digest(format!(
            "{}\n{}",
            endpoint, config.bitcoin_da_wallet_name
        )));
        let rpc = RealRpcClient::new(
            rpc_url,
            config
                .bitcoin_da_rpc_user
                .as_ref()
                .context("missing Bitcoin DA RPC user")?
                .expose_secret(),
            config
                .bitcoin_da_rpc_password
                .as_ref()
                .context("missing Bitcoin DA RPC password")?
                .expose_secret(),
            Some(config.bitcoin_da_request_timeout),
            &config.bitcoin_da_wallet_name,
        )
        .map_err(|_| anyhow::anyhow!("failed to initialize coordinated Bitcoin DA RPC"))?;
        let rpc = PublicationRpc {
            inner: rpc,
            http: reqwest::Client::builder()
                .timeout(config.bitcoin_da_request_timeout)
                .build()?,
            wallet_url: format!(
                "{}/wallet/{}",
                rpc_url.trim_end_matches('/'),
                config.bitcoin_da_wallet_name
            ),
            user: config
                .bitcoin_da_rpc_user
                .as_ref()
                .context("missing Bitcoin DA RPC user")?
                .expose_secret()
                .to_owned(),
            password: config
                .bitcoin_da_rpc_password
                .as_ref()
                .context("missing Bitcoin DA RPC password")?
                .expose_secret()
                .to_owned(),
        };
        let policy = WalletPolicy {
            version: 1,
            wallet_identity: identity,
            window: config.bitcoin_da_publication_window,
            count_limit: config.bitcoin_da_publication_mempool_count_limit,
            vbytes_limit: config.bitcoin_da_publication_mempool_vbytes_limit,
            vbytes_reserve: config.bitcoin_da_publication_vbytes_reserve,
        };
        anyhow::ensure!(
            policy.count_limit > policy.window as u64
                && policy.vbytes_reserve > 0
                && policy.vbytes_limit > policy.vbytes_reserve,
            "invalid advisory publication headroom budgets"
        );
        Ok(Some(Self {
            directory,
            policy,
            rpc,
        }))
    }
}

impl<R: RpcClient + Sync> BitcoinDaPublicationQueue<R> {
    /// Normal publishers retain a shared priority lock while waiting for capacity. Recovery
    /// requires an exclusive try-lock, so it cannot repeatedly take the next available slot.
    pub(crate) async fn normal_turn(&self) -> anyhow::Result<NormalPublicationTurn> {
        let path = self.directory.join("normal.lock");
        loop {
            let file = private_file(&path)?;
            match file.try_lock_shared() {
                Ok(()) => return Ok(NormalPublicationTurn(file)),
                Err(TryLockError::WouldBlock) => {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    pub(crate) async fn try_acquire(
        &self,
        normal_turn: Option<&NormalPublicationTurn>,
    ) -> anyhow::Result<Option<PublicationGuard<'_, R>>> {
        let recovery = normal_turn.is_none();
        if let Some(turn) = normal_turn {
            let _ = turn.0.metadata()?;
        }
        let directory = self.directory.clone();
        let locks = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let recovery_turn = if recovery {
                let file = private_file(&directory.join("normal.lock"))?;
                match file.try_lock() {
                    Ok(()) => Some(file),
                    Err(TryLockError::WouldBlock) => return Ok(None),
                    Err(error) => return Err(error.into()),
                }
            } else {
                None
            };
            let file = private_file(&directory.join("wallet.lock"))?;
            match file.try_lock() {
                Ok(()) => {}
                Err(TryLockError::WouldBlock) => return Ok(None),
                Err(error) => return Err(error.into()),
            }
            Ok(Some((file, recovery_turn)))
        })
        .await??;
        let Some((wallet_lock, recovery_turn)) = locks else {
            return Ok(None);
        };
        let genesis = self
            .rpc
            .call("getblockhash", &[json!(0)])
            .await
            .map_err(|_| anyhow::anyhow!("cannot read publication chain identity"))?;
        let genesis = hash32(genesis.as_str().context("invalid publication genesis")?)?;
        let chain = self
            .rpc
            .call("getblockchaininfo", &[])
            .await
            .map_err(|_| anyhow::anyhow!("cannot read publication chain readiness"))?;
        let initial_download = chain
            .get("initialblockdownload")
            .and_then(Value::as_bool)
            .context("publication chain readiness metadata is missing")?;
        if initial_download {
            return Ok(None);
        }
        let stored_journal = self.load()?;
        if stored_journal.is_none() {
            let info = self.rpc.call("getmempoolinfo", &[]).await.map_err(|_| {
                anyhow::anyhow!("cannot verify empty mempool for coordinator initialization")
            })?;
            anyhow::ensure!(
                info.get("loaded").and_then(Value::as_bool) == Some(true)
                    && info.get("size").and_then(Value::as_u64) == Some(0),
                "new publication coordinator requires a fully loaded, empty Core mempool; wait for existing publications to mine"
            );
        }
        let journal = stored_journal.unwrap_or_else(|| Journal {
            policy: self.policy.clone(),
            genesis: genesis.clone(),
            mined_anchor: None,
            pending: vec![],
        });
        anyhow::ensure!(
            journal.policy == self.policy && journal.genesis == genesis,
            "shared publication wallet/configuration mismatch"
        );
        anyhow::ensure!(
            journal.pending.len() <= self.policy.window,
            "invalid publication journal window"
        );
        let mut guard = PublicationGuard {
            queue: self,
            _wallet_lock: wallet_lock,
            _recovery_turn: recovery_turn,
            journal,
            poisoned: false,
        };
        guard.reconcile().await?;
        anyhow::ensure!(
            guard
                .journal
                .pending
                .iter()
                .all(|entry| entry.txid.is_some()),
            "Bitcoin DA publication outcome is unknown; all wallet writers are fenced pending operator investigation"
        );
        if guard.journal.pending.len() >= self.policy.window || !guard.headroom().await? {
            return Ok(None);
        }
        // Also creates the identity header before a claim can consume an attempt.
        guard.save()?;
        Ok(Some(guard))
    }

    fn load(&self) -> anyhow::Result<Option<Journal>> {
        let path = self.directory.join("journal.json");
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                anyhow::ensure!(metadata.is_file(), "invalid publication journal file type")
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let mut file = options.open(path)?;
        let metadata = file.metadata()?;
        anyhow::ensure!(
            metadata.is_file() && metadata.len() <= MAX_JOURNAL_BYTES,
            "invalid publication journal size"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            anyhow::ensure!(
                metadata.permissions().mode() & 0o077 == 0,
                "publication journal must be private"
            );
        }
        let mut bytes = vec![];
        Read::by_ref(&mut file)
            .take(MAX_JOURNAL_BYTES + 1)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() as u64 <= MAX_JOURNAL_BYTES,
            "publication journal exceeds bound"
        );
        let journal: Journal = serde_json::from_slice(&bytes)?;
        for entry in &journal.pending {
            anyhow::ensure!(
                hash32(&entry.version_hash)? == entry.version_hash,
                "noncanonical publication hash"
            );
            if let Some(txid) = &entry.txid {
                anyhow::ensure!(hash32(txid)? == *txid, "noncanonical publication txid");
            }
        }
        if let Some(anchor) = &journal.mined_anchor {
            hash32(&anchor.hash)?;
        }
        Ok(Some(journal))
    }
}

#[cfg(test)]
#[path = "bitcoin_da_publication_queue/tests.rs"]
mod tests;

impl<R: RpcClient + Sync> PublicationGuard<'_, R> {
    fn save(&self) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(&self.journal)?;
        anyhow::ensure!(
            bytes.len() as u64 <= MAX_JOURNAL_BYTES,
            "publication journal exceeds bound"
        );
        let mut temporary = tempfile::NamedTempFile::new_in(&self.queue.directory)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist(self.queue.directory.join("journal.json"))?;
        File::open(&self.queue.directory)?.sync_all()?;
        Ok(())
    }

    async fn canonical_anchor(&self, anchor: &BlockAnchor) -> anyhow::Result<()> {
        let actual = self
            .queue
            .rpc
            .call("getblockhash", &[json!(anchor.height)])
            .await
            .map_err(|_| anyhow::anyhow!("cannot validate publication mined anchor"))?;
        anyhow::ensure!(
            actual
                .as_str()
                .is_some_and(|actual| actual.eq_ignore_ascii_case(&anchor.hash)),
            "publication funding chain reorganized; wallet writers are fenced pending investigation"
        );
        Ok(())
    }

    async fn reconcile(&mut self) -> anyhow::Result<()> {
        let previous_anchor = self.journal.mined_anchor.clone();
        if let Some(anchor) = &self.journal.mined_anchor {
            self.canonical_anchor(anchor).await?;
        }
        let mut retained = vec![];
        let mut released_anchors = Vec::with_capacity(self.journal.pending.len());
        let mut anchor = self.journal.mined_anchor.clone();
        for entry in &self.journal.pending {
            let Some(txid) = &entry.txid else {
                retained.push(entry.clone());
                continue;
            };
            // Most polls need only small public mempool metadata. A missing/error result
            // cannot release capacity: fall back to the exact, bounded wallet receipt.
            if self
                .queue
                .rpc
                .call("getmempoolentry", &[json!(txid)])
                .await
                .ok()
                .is_some_and(|metadata| {
                    metadata
                        .get("vsize")
                        .and_then(Value::as_u64)
                        .is_some_and(|size| size > 0)
                })
            {
                retained.push(entry.clone());
                continue;
            }
            // Blob metadata can prune while the wallet's confirmed transaction history
            // survives. Publication already bound this txid to authenticated blob bytes.
            let metadata = self
                .queue
                .rpc
                .call_wallet("gettransaction", &[json!(txid), json!(false), json!(false)])
                .await
                .map_err(|_| {
                    anyhow::anyhow!("cannot reconcile known publication wallet receipt")
                })?;
            anyhow::ensure!(
                metadata
                    .get("txid")
                    .and_then(Value::as_str)
                    .is_some_and(|actual| actual.eq_ignore_ascii_case(txid)),
                "wallet publication receipt txid mismatch"
            );
            let confirmations = metadata
                .get("confirmations")
                .and_then(Value::as_i64)
                .context("wallet publication receipt omitted confirmations")?;
            let mined = if confirmations > 0 {
                Some(BlockAnchor {
                    height: metadata
                        .get("blockheight")
                        .and_then(Value::as_u64)
                        .context("mined wallet publication receipt omitted height")?,
                    hash: hash32(
                        metadata
                            .get("blockhash")
                            .and_then(Value::as_str)
                            .context("mined wallet publication receipt omitted block hash")?,
                    )?,
                })
            } else {
                None
            };
            if let Some(mined) = mined {
                self.canonical_anchor(&mined).await?;
                released_anchors.push(mined.clone());
                if anchor
                    .as_ref()
                    .is_none_or(|previous| mined.height > previous.height)
                {
                    anchor = Some(mined)
                }
            } else {
                retained.push(entry.clone());
            }
        }
        // Independently funded lower transactions can reorg between receipt reads while a
        // higher transaction remains canonical. Recheck every candidate (at most the shared
        // window), not only the highest anchor; these RPC reads are not an atomic snapshot.
        for released_anchor in &released_anchors {
            self.canonical_anchor(released_anchor).await?;
        }
        if let Some(previous_anchor) = &previous_anchor {
            self.canonical_anchor(previous_anchor).await?;
        }
        self.journal.pending = retained;
        self.journal.mined_anchor = anchor;
        self.save()
    }

    async fn headroom(&self) -> anyhow::Result<bool> {
        let info = self
            .queue
            .rpc
            .call("getmempoolinfo", &[])
            .await
            .map_err(|_| anyhow::anyhow!("cannot read publication mempool headroom"))?;
        let count = info
            .get("size")
            .and_then(Value::as_u64)
            .context("invalid mempool count")?;
        let loaded = info
            .get("loaded")
            .and_then(Value::as_bool)
            .context("mempool readiness metadata is missing")?;
        let bytes = info
            .get("bytes")
            .and_then(Value::as_u64)
            .context("invalid mempool vbytes")?;
        Ok(loaded
            && count
                .checked_add(1)
                .is_some_and(|next| next <= self.queue.policy.count_limit)
            && bytes
                .checked_add(self.queue.policy.vbytes_reserve)
                .is_some_and(|next| next <= self.queue.policy.vbytes_limit))
    }

    pub(crate) fn contains(&self, version_hash: &str) -> anyhow::Result<bool> {
        let hash = hash32(version_hash)?;
        Ok(self
            .journal
            .pending
            .iter()
            .any(|entry| entry.version_hash == hash))
    }

    pub(crate) async fn publish(
        &mut self,
        blob: &[u8],
        expected_hash: &str,
        overwrite: bool,
    ) -> anyhow::Result<String> {
        let expected_hash = hash32(expected_hash)?;
        anyhow::ensure!(
            !blob.is_empty()
                && blob.len() <= MAX_BLOB_SIZE
                && hex::encode(Blake2s256::digest(blob)) == expected_hash,
            "publication bytes do not match the committed bounded hash"
        );
        anyhow::ensure!(
            !self.poisoned
                && self
                    .journal
                    .pending
                    .iter()
                    .all(|entry| entry.txid.is_some())
                && !self.contains(&expected_hash)?
                && self.journal.pending.len() < self.queue.policy.window,
            "publication fenced, already pending, or window full"
        );
        // Must be durable before the first possible wallet side effect. On cancellation,
        // timeout, malformed result, or failed result save this entry remains a global fence.
        self.poisoned = true;
        self.journal.pending.push(PendingPublication {
            version_hash: expected_hash.clone(),
            txid: None,
        });
        self.save()?;
        let result = self.queue.rpc.call_wallet("syscoincreatenevmblob", &[json!(hex::encode(blob)), json!(overwrite), json!("blake2s")]).await
            .map_err(|_| anyhow::anyhow!("coordinated Bitcoin DA wallet RPC failed; outcome is unknown and all writers remain fenced"))?;
        let returned_hash = hash32(
            result
                .get("versionhash")
                .and_then(Value::as_str)
                .context("publication result omitted hash; writers remain fenced")?,
        )?;
        anyhow::ensure!(
            returned_hash == expected_hash,
            "publication hash mismatch; writers remain fenced"
        );
        if let Some(txid) = result.get("txid").and_then(Value::as_str) {
            let txid = hash32(txid)?;
            self.journal
                .pending
                .last_mut()
                .expect("reserved publication exists")
                .txid = Some(txid);
        } else {
            // Core's non-overwrite idempotent branch returns only versionhash. A forced
            // publication never has that no-broadcast success shape.
            anyhow::ensure!(
                !overwrite && result.as_object().is_some_and(|object| object.len() == 1),
                "publication result omitted txid; writers remain fenced"
            );
            self.journal.pending.pop();
        }
        self.save()?;
        self.poisoned = false;
        Ok(expected_hash)
    }
}
