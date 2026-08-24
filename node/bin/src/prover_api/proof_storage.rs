use crate::config::ProofStorageConfig;
use crate::prover_api::fri_job_manager::FailedFriProof;
use crate::prover_api::metrics::{PROOF_STORAGE_METRICS, ProofStorageMethod};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::Metadata;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::Mutex;
use zksync_os_batch_types::batcher_model::{FriProof, SignedBatchEnvelope};
use zksync_os_pipeline::HasBlockRangeEnd;

// SYSCOIN: Unique same-directory transaction files make accepted-proof publication atomic while
// retaining stale crash artifacts for explicit startup quarantine.
static STORAGE_TRANSACTION_COUNTER: AtomicU64 = AtomicU64::new(0);
const STORAGE_TRANSACTION_PREFIX: &str = ".syscoin-proof-txn-";
const STORAGE_TRANSACTION_SUFFIX: &str = ".tmp";

/// Persists FRI proofs to disk together with the batch if proof is successful
#[derive(Clone, Debug)]
pub struct ProofStorage {
    batches_with_proof: Arc<Mutex<BoundedFileStorage>>,
    // SYSCOIN: Pending accepted proofs are durable and capacity-protected until handoff.
    pending_batches_with_proof: Arc<Mutex<HashMap<String, u64>>>,
    // SYSCOIN: Recovered pending keys are replayed exactly once after restart.
    recovered_pending_batches_with_proof: Arc<Mutex<HashSet<String>>>,
    // SYSCOIN: Disambiguates pending writes created in the same clock tick.
    pending_key_counter: Arc<AtomicU64>,
    failed: Arc<Mutex<BoundedFileStorage>>,
}

// SYSCOIN: Couples a proven batch with its durable pending-file lease.
#[derive(Debug)]
pub struct ProvenBatch {
    pub batch: SignedBatchEnvelope<FriProof>,
    pub pending_proof_key: Option<PendingBatchProofKey>,
}

impl ProvenBatch {
    pub fn new(batch: SignedBatchEnvelope<FriProof>) -> Self {
        Self {
            batch,
            pending_proof_key: None,
        }
    }

    pub fn pending(
        batch: SignedBatchEnvelope<FriProof>,
        pending_proof_key: PendingBatchProofKey,
    ) -> Self {
        Self {
            batch,
            pending_proof_key: Some(pending_proof_key),
        }
    }
}

impl HasBlockRangeEnd for ProvenBatch {
    fn block_number(&self) -> u64 {
        self.batch.block_number()
    }

    fn block_timestamp(&self) -> Option<u64> {
        self.batch.block_timestamp()
    }

    fn batch_number(&self) -> Option<u64> {
        Some(self.batch.batch_number())
    }
}

impl ProofStorage {
    pub async fn new(config: ProofStorageConfig) -> anyhow::Result<Self> {
        let fri_batches_path = config.path.join("fri_batches");
        // SYSCOIN: Discover and protect accepted-but-unforwarded proofs before capacity cleanup.
        let pending_keys = discover_pending_batch_proof_keys(&fri_batches_path).await?;
        let pending_protected_keys: HashSet<_> = pending_keys
            .iter()
            .map(|key| key.as_str().to_string())
            .collect();
        let pending_batches_with_proof = pending_protected_keys
            .iter()
            .map(|key| (key.clone(), 1))
            .collect();
        tracing::info!(
            path = config.path.display().to_string(),
            batch_with_proof_capacity = config.batch_with_proof_capacity.0,
            failed_capacity = config.failed_capacity.0,
            pending_accepted_proofs = pending_keys.len(),
            "Initializing proof storage"
        );
        Ok(Self {
            batches_with_proof: Arc::new(Mutex::new(
                BoundedFileStorage::new_protected(
                    fri_batches_path,
                    config.batch_with_proof_capacity.0,
                    &pending_protected_keys,
                )
                .await?,
            )),
            pending_batches_with_proof: Arc::new(Mutex::new(pending_batches_with_proof)),
            recovered_pending_batches_with_proof: Arc::new(Mutex::new(pending_protected_keys)),
            pending_key_counter: Arc::new(AtomicU64::new(0)),
            failed: Arc::new(Mutex::new(
                BoundedFileStorage::new(
                    config.path.join("failed_proofs"),
                    config.failed_capacity.0,
                )
                .await?,
            )),
        })
    }

    #[cfg(test)]
    pub(crate) async fn pending_batch_proof_count_for_test(&self) -> usize {
        self.pending_batches_with_proof.lock().await.len()
    }

    /// Persist a BatchWithProof. Overwrites any existing entry for the same batch.
    pub async fn save_batch_with_proof(&self, batch: &StoredBatch) -> anyhow::Result<()> {
        let latency =
            PROOF_STORAGE_METRICS.latency[&ProofStorageMethod::SaveBatchWithProof].start();

        let key = format!("batch_{}.json", batch.batch_number());
        // SYSCOIN: Canonical writes must not evict durable pending proof leases.
        let pending = self.pending_batches_with_proof.lock().await;
        let protected_keys: HashSet<_> = pending.keys().cloned().collect();
        let result = self
            .batches_with_proof
            .lock()
            .await
            .store_protected(&key, batch, &protected_keys)
            .await;
        latency.observe();
        let usage = result?;

        PROOF_STORAGE_METRICS.disk_usage[&ProofStorageMethod::SaveBatchWithProof].set(usage);
        Ok(())
    }

    /// SYSCOIN: Promote a pending proof file to its canonical batch key.
    ///
    /// Unlike [`Self::save_batch_with_proof`], this is a required durable handoff; it renames
    /// the already-written pending file instead of requiring temporary capacity for a second copy.
    pub async fn promote_pending_batch_with_proof(
        &self,
        key: &PendingBatchProofKey,
    ) -> anyhow::Result<()> {
        let latency =
            PROOF_STORAGE_METRICS.latency[&ProofStorageMethod::SaveBatchWithProof].start();

        let result = self
            .batches_with_proof
            .lock()
            .await
            .promote(key.as_str(), &format!("batch_{}.json", key.batch_number()))
            .await;
        latency.observe();
        let usage = result?;

        PROOF_STORAGE_METRICS.disk_usage[&ProofStorageMethod::SaveBatchWithProof].set(usage);
        Ok(())
    }

    /// SYSCOIN: Persist a batch with proof that has been accepted by the FRI API but not yet forwarded.
    ///
    /// Pending proofs are protected from capacity eviction until [`Self::release_pending_batch_with_proof`]
    /// is called. Returning `Ok(())` from this method means the proof was actually written and remains
    /// loadable by the forwarder unless it is externally deleted or the filesystem fails.
    pub async fn save_pending_batch_with_proof(
        &self,
        batch: &StoredBatch,
    ) -> anyhow::Result<PendingBatchProofKey> {
        let latency =
            PROOF_STORAGE_METRICS.latency[&ProofStorageMethod::SaveBatchWithProof].start();

        let key = PendingBatchProofKey::new(
            batch.batch_number(),
            self.pending_key_counter.fetch_add(1, Ordering::Relaxed),
        )?;
        let mut pending = self.pending_batches_with_proof.lock().await;
        // SYSCOIN: Reference-count concurrent handoffs that share this durable pending key.
        *pending.entry(key.as_str().to_string()).or_insert(0) += 1;
        let protected_keys: HashSet<_> = pending.keys().cloned().collect();

        let result = self
            .batches_with_proof
            .lock()
            .await
            .store_protected(key.as_str(), batch, &protected_keys)
            .await;

        if result.is_err() {
            // SYSCOIN: Roll back the pending lease when the durable write fails.
            decrement_pending_proof(&mut pending, key.as_str());
        }

        latency.observe();
        let usage = result?;

        PROOF_STORAGE_METRICS.disk_usage[&ProofStorageMethod::SaveBatchWithProof].set(usage);
        Ok(key)
    }

    // SYSCOIN: Release and remove a durable pending proof after successful handoff.
    pub async fn release_pending_batch_with_proof(&self, key: &PendingBatchProofKey) {
        let mut pending = self.pending_batches_with_proof.lock().await;
        let Some(reference_count) = pending.get_mut(key.as_str()) else {
            return;
        };

        if *reference_count > 1 {
            *reference_count -= 1;
            return;
        }

        match self
            .batches_with_proof
            .lock()
            .await
            .remove(key.as_str())
            .await
        {
            Ok(()) => {
                pending.remove(key.as_str());
                self.recovered_pending_batches_with_proof
                    .lock()
                    .await
                    .remove(key.as_str());
            }
            Err(err) => {
                tracing::warn!(
                    ?err,
                    pending_proof_key = key.as_str(),
                    "failed to remove released pending proof; keeping it protected"
                );
            }
        }
    }

    // SYSCOIN: Quarantine a corrupt pending proof so restart recovery cannot loop on it.
    pub async fn quarantine_pending_batch_with_proof(&self, key: &PendingBatchProofKey) {
        let mut pending = self.pending_batches_with_proof.lock().await;
        let Some(reference_count) = pending.get_mut(key.as_str()) else {
            return;
        };

        if *reference_count > 1 {
            *reference_count -= 1;
            return;
        }

        match self
            .batches_with_proof
            .lock()
            .await
            .quarantine(key.as_str())
            .await
        {
            Ok(Some(quarantine_key)) => {
                pending.remove(key.as_str());
                self.recovered_pending_batches_with_proof
                    .lock()
                    .await
                    .remove(key.as_str());
                tracing::error!(
                    pending_proof_key = key.as_str(),
                    quarantine_key,
                    "quarantined unloadable pending proof"
                );
            }
            Ok(None) => {
                pending.remove(key.as_str());
                self.recovered_pending_batches_with_proof
                    .lock()
                    .await
                    .remove(key.as_str());
                tracing::error!(
                    pending_proof_key = key.as_str(),
                    "pending proof was missing during quarantine"
                );
            }
            Err(err) => {
                tracing::error!(
                    ?err,
                    pending_proof_key = key.as_str(),
                    "failed to quarantine unloadable pending proof; keeping it protected"
                );
            }
        }
    }

    // SYSCOIN: Return the startup snapshot of durable pending proofs in canonical order.
    pub async fn recovered_pending_batch_proof_keys(&self) -> Vec<PendingBatchProofKey> {
        let recovered = self.recovered_pending_batches_with_proof.lock().await;
        let mut keys: Vec<_> = recovered
            .iter()
            .filter_map(|key| PendingBatchProofKey::parse(key.clone()))
            .collect();
        keys.sort_by_key(|key| (key.batch_number, key.key.clone()));
        keys
    }

    // SYSCOIN: Mark a recovered pending key consumed without changing its file lease yet.
    pub async fn remove_recovered_pending_batch_proof_key(&self, key: &PendingBatchProofKey) {
        self.recovered_pending_batches_with_proof
            .lock()
            .await
            .remove(key.as_str());
    }

    // SYSCOIN: Load an accepted proof by its durable pending lease key.
    pub async fn get_pending_batch_with_proof(
        &self,
        key: &PendingBatchProofKey,
    ) -> anyhow::Result<Option<SignedBatchEnvelope<FriProof>>> {
        let latency = PROOF_STORAGE_METRICS.latency[&ProofStorageMethod::GetBatchWithProof].start();

        let result = self
            .batches_with_proof
            .lock()
            .await
            .load::<StoredBatch>(key.as_str())
            .await
            .map(|o| o.map(|o| o.batch_envelope()));

        latency.observe();
        result
    }

    /// Loads a BatchWithProof for `batch_number`, if present
    pub async fn get_batch_with_proof(
        &self,
        batch_num: u64,
    ) -> anyhow::Result<Option<SignedBatchEnvelope<FriProof>>> {
        Ok(self
            .get_batch_with_proof_and_age(batch_num)
            .await?
            .map(|(batch, _)| batch))
    }

    /// SYSCOIN: Loads a canonical FRI proof with time elapsed since durable acceptance.
    ///
    /// Accepted proofs are first written under a pending key and then renamed to their canonical
    /// key. Renaming preserves the file modification time, so this timestamp survives process
    /// restart without changing the existing on-disk JSON format.
    pub async fn get_batch_with_proof_and_age(
        &self,
        batch_num: u64,
    ) -> anyhow::Result<Option<(SignedBatchEnvelope<FriProof>, Duration)>> {
        let latency = PROOF_STORAGE_METRICS.latency[&ProofStorageMethod::GetBatchWithProof].start();

        let key = format!("batch_{batch_num}.json");
        let result = self
            .batches_with_proof
            .lock()
            .await
            .load_with_modified_time::<StoredBatch>(&key)
            .await
            .map(|stored| {
                stored.map(|(stored, modified_at)| {
                    let accepted_age = SystemTime::now()
                        .duration_since(modified_at)
                        .unwrap_or(Duration::ZERO);
                    (stored.batch_envelope(), accepted_age)
                })
            });

        latency.observe();
        result
    }

    /// Save a failed FRI proof for debugging.
    pub async fn save_failed_proof(&self, proof: &FailedFriProof) -> anyhow::Result<()> {
        let latency = PROOF_STORAGE_METRICS.latency[&ProofStorageMethod::SaveFailed].start();

        let key = format!("failed_{}.json", proof.batch_number);
        let result = self.failed.lock().await.store(&key, proof).await;
        latency.observe();
        let usage = result?;

        PROOF_STORAGE_METRICS.disk_usage[&ProofStorageMethod::SaveFailed].set(usage);
        Ok(())
    }

    /// Get the failed proof for a given batch number.
    /// Returns None if no failed proof exists for this batch.
    pub async fn get_failed_proof(&self, batch_num: u64) -> anyhow::Result<Option<FailedFriProof>> {
        let latency = PROOF_STORAGE_METRICS.latency[&ProofStorageMethod::GetFailed].start();

        let key = format!("failed_{batch_num}.json");
        let result = self.failed.lock().await.load(&key).await;

        latency.observe();
        result
    }
}

// SYSCOIN: Decrement a durable pending-key lease and report when its file can be removed.
fn decrement_pending_proof(pending: &mut HashMap<String, u64>, key: &str) -> bool {
    if let Some(count) = pending.get_mut(key) {
        *count -= 1;
        if *count == 0 {
            pending.remove(key);
            return true;
        }
    }
    false
}

// SYSCOIN: Opaque durable lease for an accepted FRI proof awaiting pipeline handoff.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct PendingBatchProofKey {
    key: String,
    batch_number: u64,
}

impl PendingBatchProofKey {
    fn new(batch_number: u64, sequence: u64) -> anyhow::Result<Self> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        Ok(Self {
            key: format!("batch_{batch_number}_pending_{now}_{sequence}.json"),
            batch_number,
        })
    }

    fn parse(key: String) -> Option<Self> {
        let suffix_stripped = key.strip_suffix(".json")?;
        let batch_number = suffix_stripped
            .strip_prefix("batch_")?
            .split_once("_pending_")?
            .0
            .parse()
            .ok()?;
        Some(Self { key, batch_number })
    }

    pub fn batch_number(&self) -> u64 {
        self.batch_number
    }

    fn as_str(&self) -> &str {
        &self.key
    }
}

// SYSCOIN: Recover pending proof leases left by a process interruption.
async fn discover_pending_batch_proof_keys(
    base_dir: &std::path::Path,
) -> anyhow::Result<Vec<PendingBatchProofKey>> {
    let mut keys = Vec::new();
    if !fs::try_exists(base_dir).await? {
        return Ok(keys);
    }

    let mut entries = fs::read_dir(base_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        if !entry.metadata().await?.is_file() {
            continue;
        }
        if let Ok(filename) = entry.file_name().into_string()
            && let Some(key) = PendingBatchProofKey::parse(filename)
        {
            keys.push(key);
        }
    }

    keys.sort_by_key(|key| (key.batch_number, key.key.clone()));
    Ok(keys)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(transparent)]
/// SYSCOIN: Fresh V32 proof storage has one canonical envelope without legacy enum variants.
pub struct StoredBatch(pub SignedBatchEnvelope<FriProof>);

impl StoredBatch {
    pub fn batch_number(&self) -> u64 {
        self.0.batch_number()
    }

    pub fn batch_envelope(self) -> SignedBatchEnvelope<FriProof> {
        self.0
    }
}

/// Storage for data blobs that
/// automatically removes old files to keep disk usage within capacity_bytes
/// Keys are expected to be file names.
/// In case of overwrite old value will be preserved under a different name (see handle_duplicate)
/// Expected use case for this data is debugging.
/// The only way to access overwritten entries is directly from disk.
/// Currently, the key is batch number. Overwrites could happen in these 2 cases:
/// * server restart -- we do not store block ranges for the batches, so they could change
/// * batch revert
#[derive(Clone, Debug)]
struct BoundedFileStorage {
    base_dir: PathBuf,
    capacity_bytes: u64,
    current_size: u64,
    /// Files ordered by eviction priority (oldest first). New files are pushed to the back;
    /// eviction pops from the front.
    ///
    /// A key may appear more than once when a file has been overwritten: the original queue
    /// entry becomes outdated (the file was renamed away) while the renamed file and the new
    /// file each add their own entry. Outdated entries must be skipped during eviction — see
    /// `outdated_count`.
    remove_queue: VecDeque<(String, Metadata)>,
    /// Counts outdated entries in `remove_queue` for each key.
    ///
    /// Each time a key is overwritten, `handle_duplicate` renames the existing file and
    /// increments this counter. The original queue entry (still carrying the old key) becomes
    /// outdated: the file it pointed to no longer exists under that name. During eviction,
    /// `enforce_capacity` decrements the counter and skips the entry instead of trying to
    /// delete it, preventing accidental deletion of the current version of the file.
    outdated_count: HashMap<String, u64>,
}

impl BoundedFileStorage {
    async fn new(base_dir: PathBuf, capacity_bytes: u64) -> anyhow::Result<Self> {
        Self::new_protected(base_dir, capacity_bytes, &HashSet::new()).await
    }

    // SYSCOIN: Initialize bounded storage while excluding active pending leases from eviction.
    async fn new_protected(
        base_dir: PathBuf,
        capacity_bytes: u64,
        protected_keys: &HashSet<String>,
    ) -> anyhow::Result<Self> {
        // Create the directory if it doesn't exist already
        fs::create_dir_all(&base_dir).await?;
        // SYSCOIN: Proof files may contain unpublished execution data. Restrict a freshly created
        // or pre-existing storage directory before scanning/recovering transactional artifacts.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&base_dir, std::fs::Permissions::from_mode(0o700)).await?;
        }
        // SYSCOIN: A crash before atomic publication can leave a partial or fully-synced temp.
        // Its generic payload type is unavailable here, so fail safely by quarantining it rather
        // than treating it as accepted proof state; the retained prover lease can replay it.
        quarantine_stale_storage_transactions(&base_dir).await?;
        // List all files sorted by timestamp (descending)
        let mut entries = fs::read_dir(&base_dir).await?;
        let mut files = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let meta = entry.metadata().await?;
            if meta.is_file() {
                match entry.file_name().into_string() {
                    Ok(filename) => files.push((filename, meta)),
                    Err(filename) => tracing::warn!(
                        "Unrelated file detected in {} ({}): the name cannot be represented using a String",
                        base_dir.display(),
                        filename.to_string_lossy(),
                    ),
                }
            }
        }
        files.sort_by_cached_key(|(_, meta)| meta.modified().unwrap_or(SystemTime::UNIX_EPOCH));

        let current_size: u64 = files.iter().map(|(_, meta)| meta.len()).sum();
        let mut storage = Self {
            base_dir,
            capacity_bytes,
            current_size,
            remove_queue: files.into_iter().collect(),
            outdated_count: HashMap::new(),
        };

        if current_size > capacity_bytes {
            tracing::warn!(
                current_size,
                capacity_bytes,
                "On startup, more data is used than expected"
            );
            storage.enforce_capacity(0, protected_keys).await?;
        }

        Ok(storage)
    }

    /// Stores serialized value as a file named `key` (should be a valid file name)
    /// Previous `value` for `key` is preserved under a different name, with a recent timestamp
    /// removes old files to enforce capacity constraints and
    /// returns disk usage
    async fn store<T: Serialize>(&mut self, key: &str, value: &T) -> anyhow::Result<u64> {
        self.store_internal(key, value, &HashSet::new(), false)
            .await
    }

    // SYSCOIN: Store a required value without evicting any active pending lease.
    async fn store_protected<T: Serialize>(
        &mut self,
        key: &str,
        value: &T,
        protected_keys: &HashSet<String>,
    ) -> anyhow::Result<u64> {
        self.store_internal(key, value, protected_keys, true).await
    }

    async fn store_internal<T: Serialize>(
        &mut self,
        key: &str,
        value: &T,
        protected_keys: &HashSet<String>,
        require_write: bool,
    ) -> anyhow::Result<u64> {
        fs::create_dir_all(&self.base_dir).await?;

        let data = serde_json::to_vec(value)?;
        let count = data.len() as u64;
        if count > self.capacity_bytes {
            if require_write {
                anyhow::bail!(
                    "entry {key} size {count} exceeds storage capacity {}",
                    self.capacity_bytes
                );
            }
            tracing::warn!(
                data_len = data.len(),
                capacity = self.capacity_bytes,
                "Entry size is larger than the limit. Not saving.",
            );
            return Ok(self.current_size);
        }

        let path = self.base_dir.join(key);
        if require_write && fs::try_exists(&path).await? && fs::read(&path).await? == data {
            // SYSCOIN: GaplessCommitter can replay an already-canonical proof while the pipeline catches
            // up after restart. Rewriting identical bytes would refresh the file mtime that is
            // also the durable SNARK aggregation-age clock.
            tracing::info!(key, "Skipping identical proof storage replay");
            return Ok(self.current_size);
        }

        if require_write && path.is_file() {
            return self
                .overwrite_existing_required(key, data, count, protected_keys)
                .await;
        }
        if !require_write && protected_keys.contains(key) && path.is_file() {
            tracing::warn!(
                key,
                "Skipping best-effort overwrite of protected proof storage entry"
            );
            return Ok(self.current_size);
        }

        self.handle_duplicate(key).await?;
        // This could still remove the duplicate if there is not enough space for it
        self.enforce_capacity(count, protected_keys).await?;
        if self.current_size + count > self.capacity_bytes {
            if require_write {
                anyhow::bail!(
                    "not enough storage capacity for {key}; remaining files are protected"
                );
            }
            tracing::warn!(
                data_len = data.len(),
                capacity = self.capacity_bytes,
                "Not enough capacity after preserving protected entries. Not saving.",
            );
            return Ok(self.current_size);
        }
        self.write_file(key, data).await?;
        Ok(self.current_size)
    }

    // SYSCOIN: Replace a required canonical value atomically while preserving capacity accounting.
    async fn overwrite_existing_required(
        &mut self,
        key: &str,
        data: Vec<u8>,
        count: u64,
        protected_keys: &HashSet<String>,
    ) -> anyhow::Result<u64> {
        let path = self.base_dir.join(key);
        let old_meta = fs::metadata(&path).await?;
        let old_len = old_meta.len();
        let extra_size = count.saturating_sub(old_len);

        let mut protected_keys = protected_keys.clone();
        protected_keys.insert(key.to_string());

        self.enforce_capacity(extra_size, &protected_keys).await?;
        anyhow::ensure!(
            self.current_size + extra_size <= self.capacity_bytes,
            "not enough storage capacity for {key}; remaining files are protected"
        );

        // SYSCOIN: File fsync precedes atomic replacement. Update in-memory accounting immediately
        // after publication, then require parent fsync before reporting success.
        self.durable_atomic_write(&path, &data).await?;

        self.current_size = self.current_size - old_len + count;
        *self.outdated_count.entry(key.to_string()).or_insert(0) += 1;
        let meta = fs::metadata(&path).await?;
        self.remove_queue.push_back((key.to_string(), meta));
        sync_storage_directory(&self.base_dir).await?;
        Ok(self.current_size)
    }

    async fn load<T: DeserializeOwned>(&self, key: &str) -> anyhow::Result<Option<T>> {
        Ok(self
            .load_with_modified_time(key)
            .await?
            .map(|(value, _)| value))
    }

    async fn load_with_modified_time<T: DeserializeOwned>(
        &self,
        key: &str,
    ) -> anyhow::Result<Option<(T, SystemTime)>> {
        let path = self.base_dir.join(key);
        if !fs::try_exists(&path).await? {
            return Ok(None);
        }

        let data = fs::read(&path).await?;
        let modified_at = fs::metadata(path).await?.modified()?;
        let decoded = serde_json::from_slice(&data)?;
        Ok(Some((decoded, modified_at)))
    }

    // SYSCOIN: Remove a durable file and retain stale queue accounting for lazy cleanup.
    async fn remove(&mut self, key: &str) -> anyhow::Result<()> {
        let path = self.base_dir.join(key);
        if !fs::try_exists(&path).await? {
            return Ok(());
        }

        let meta = fs::metadata(&path).await?;
        fs::remove_file(path).await?;
        self.current_size = self.current_size.saturating_sub(meta.len());
        *self.outdated_count.entry(key.to_string()).or_insert(0) += 1;
        // SYSCOIN: Keep process accounting truthful even if directory fsync fails and forces the
        // caller to restart/recover; success still requires the durable unlink boundary.
        sync_storage_directory(&self.base_dir).await?;
        Ok(())
    }

    // SYSCOIN: Atomically promote a pending proof to its canonical batch key.
    async fn promote(&mut self, from_key: &str, to_key: &str) -> anyhow::Result<u64> {
        let from_path = self.base_dir.join(from_key);
        anyhow::ensure!(
            fs::try_exists(&from_path).await?,
            "pending proof {from_key} is missing"
        );

        if from_key != to_key {
            let to_path = self.base_dir.join(to_key);
            if fs::try_exists(&to_path).await?
                && fs::read(&from_path).await? == fs::read(&to_path).await?
            {
                // SYSCOIN: Leave the pending file for `release_pending_batch_with_proof()` to remove. The
                // canonical file (and therefore its original acceptance timestamp) stays intact.
                tracing::info!(
                    from_key,
                    to_key,
                    "Skipping promotion of proof identical to canonical storage"
                );
                return Ok(self.current_size);
            }
            self.remove(to_key).await?;
        }

        let to_path = self.base_dir.join(to_key);
        fs::rename(&from_path, &to_path).await?;
        *self.outdated_count.entry(from_key.to_string()).or_insert(0) += 1;
        let meta = fs::metadata(&to_path).await?;
        self.remove_queue.push_back((to_key.to_string(), meta));
        // SYSCOIN: Persist the pending-to-canonical namespace transition before downstream ack;
        // accounting is already reconciled if this fsync itself fails.
        sync_storage_directory(&self.base_dir).await?;
        Ok(self.current_size)
    }

    // SYSCOIN: Move an unreadable pending proof aside for operator inspection.
    async fn quarantine(&mut self, key: &str) -> anyhow::Result<Option<String>> {
        let path = self.base_dir.join(key);
        if !fs::try_exists(&path).await? {
            return Ok(None);
        }

        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let quarantine_key = format!("{key}.quarantined_{now}");
        let quarantine_path = self.base_dir.join(&quarantine_key);
        fs::rename(&path, &quarantine_path).await?;
        *self.outdated_count.entry(key.to_string()).or_insert(0) += 1;
        let meta = fs::metadata(&quarantine_path).await?;
        self.remove_queue.push_back((quarantine_key.clone(), meta));
        // SYSCOIN: Make quarantine durable after reconciling the already-visible rename.
        sync_storage_directory(&self.base_dir).await?;
        Ok(Some(quarantine_key))
    }

    /// Delete old files to make space for the new file
    async fn enforce_capacity(
        &mut self,
        new_file_size: u64,
        protected_keys: &HashSet<String>,
    ) -> anyhow::Result<()> {
        // Delete old files to satisfy capacity constraints
        while self.current_size + new_file_size > self.capacity_bytes
            && !self.remove_queue.is_empty()
        {
            // SYSCOIN: Skip protected pending leases while reclaiming bounded storage capacity.
            let mut removed_any = false;
            let entries_to_scan = self.remove_queue.len();
            for _ in 0..entries_to_scan {
                let (key, meta) = self.remove_queue.pop_front().unwrap();
                // This queue entry is outdated: the file was renamed away by a later overwrite.
                // Skip it without touching the filesystem and decrement the counter.
                // The renamed file is tracked separately under its new name.
                if let Some(outdated) = self.outdated_count.get_mut(&key)
                    && *outdated > 0
                {
                    *outdated -= 1;
                    continue;
                }
                if protected_keys.contains(&key) {
                    self.remove_queue.push_back((key, meta));
                    continue;
                }

                fs::remove_file(self.base_dir.join(key)).await?;
                self.current_size -= meta.len();
                removed_any = true;
                // SYSCOIN: Persist every capacity-eviction unlink after reflecting it in memory;
                // a directory-fsync failure is returned without leaving live accounting stale.
                sync_storage_directory(&self.base_dir).await?;

                if self.current_size + new_file_size <= self.capacity_bytes {
                    break;
                }
            }

            if !removed_any {
                break;
            }
        }

        if self.remove_queue.is_empty() && self.current_size > 0 {
            tracing::warn!(
                current_size = self.current_size,
                "current_size is not maintained correctly"
            );
        }

        Ok(())
    }
    /// If a file named `key` already exists, renames it to `key.overwritten_{timestamp}`
    /// and appends the renamed entry to the back of the queue so it is eventually evicted.
    async fn handle_duplicate(&mut self, key: &str) -> anyhow::Result<()> {
        let path = self.base_dir.join(key);
        if path.is_file() {
            tracing::info!("Storing old version of {}", key);

            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let new_key = &format!("{key}.overwritten_{now}");
            let new_path = self.base_dir.join(new_key);
            // The original queue entry for `key` becomes outdated: the file it pointed to
            // no longer exists under that name. Increment the counter so that
            // `enforce_capacity` knows to skip that entry rather than deleting the
            // newly-written file.
            // Rename and add to the back of the queue
            fs::rename(path, new_path.clone()).await?;
            *self.outdated_count.entry(key.to_string()).or_insert(0) += 1;
            let meta = fs::metadata(&new_path).await?;
            self.remove_queue.push_back((new_key.to_string(), meta));
            // SYSCOIN: Preserve the duplicate after reconciling its visible queue identity.
            sync_storage_directory(&self.base_dir).await?;
        }
        Ok(())
    }

    /// Write file to disk and add an entry to remove_queue
    async fn write_file(&mut self, key: &str, data: Vec<u8>) -> anyhow::Result<()> {
        let path = self.base_dir.join(key);
        let len = data.len() as u64;
        // SYSCOIN: Required accepted proofs and best-effort diagnostics share one crash-safe
        // publication primitive; callers decide whether a write failure is terminal or retryable.
        self.durable_atomic_write(&path, &data).await?;
        self.current_size += len;
        let meta = fs::metadata(&path).await?;
        self.remove_queue.push_back((key.to_string(), meta));
        sync_storage_directory(&self.base_dir).await?;
        Ok(())
    }

    // SYSCOIN: Publish one owner-only file via same-directory temp -> file fsync -> atomic rename.
    // The caller updates capacity/queue accounting before performing the required directory fsync.
    async fn durable_atomic_write(
        &self,
        path: &std::path::Path,
        data: &[u8],
    ) -> anyhow::Result<()> {
        let transaction_id = STORAGE_TRANSACTION_COUNTER.fetch_add(1, Ordering::Relaxed);
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let temporary_path = self.base_dir.join(format!(
            "{STORAGE_TRANSACTION_PREFIX}{}-{now}-{transaction_id}{STORAGE_TRANSACTION_SUFFIX}",
            std::process::id()
        ));

        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary_path).await?;
        if let Err(error) = file.write_all(data).await {
            drop(file);
            let _ = fs::remove_file(&temporary_path).await;
            let _ = sync_storage_directory(&self.base_dir).await;
            return Err(error.into());
        }
        if let Err(error) = file.sync_all().await {
            drop(file);
            let _ = fs::remove_file(&temporary_path).await;
            let _ = sync_storage_directory(&self.base_dir).await;
            return Err(error.into());
        }
        drop(file);

        if let Err(error) = fs::rename(&temporary_path, path).await {
            let _ = fs::remove_file(&temporary_path).await;
            let _ = sync_storage_directory(&self.base_dir).await;
            return Err(error.into());
        }
        Ok(())
    }
}

// SYSCOIN: Linux directory fsync is the durability boundary for rename/unlink operations.
async fn sync_storage_directory(directory: &std::path::Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(directory).await?.sync_all().await?;
    }
    #[cfg(not(unix))]
    let _ = directory;
    Ok(())
}

// SYSCOIN: Transaction temps are never silently accepted after a crash. Quarantine each one with
// a durable rename; a client-owned envelope or reconstructed FRI job remains the recovery source.
async fn quarantine_stale_storage_transactions(directory: &std::path::Path) -> anyhow::Result<()> {
    let mut entries = fs::read_dir(directory).await?;
    while let Some(entry) = entries.next_entry().await? {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(STORAGE_TRANSACTION_PREFIX)
            || !name.ends_with(STORAGE_TRANSACTION_SUFFIX)
        {
            continue;
        }
        let source = entry.path();
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let destination = directory.join(format!("{name}.quarantined_{now}"));
        fs::rename(source, destination).await?;
        sync_storage_directory(directory).await?;
    }
    Ok(())
}

// Since this data isn't used by the node itself, I added some tests
#[cfg(test)]
mod tests {
    use super::*;
    use crate::prover_api::test_util::{
        create_test_batch_envelope_with_data, mark_test_batch_as_interop_bundle,
    };
    use tempfile::TempDir;
    use zksync_os_types::ProtocolSemanticVersion;

    // SYSCOIN: A crash-surviving transaction temp is never mistaken for accepted proof state.
    #[tokio::test]
    async fn stale_transaction_temp_is_durably_quarantined_on_restart() -> anyhow::Result<()> {
        let dir = TempDir::new()?;
        let temporary_name =
            format!("{STORAGE_TRANSACTION_PREFIX}crash{STORAGE_TRANSACTION_SUFFIX}");
        fs::write(dir.path().join(&temporary_name), b"partial proof").await?;

        let storage = BoundedFileStorage::new(dir.path().to_owned(), 1024).await?;
        assert!(
            storage
                .load::<serde_json::Value>(&temporary_name)
                .await?
                .is_none()
        );

        let mut entries = fs::read_dir(dir.path()).await?;
        let mut quarantined = false;
        while let Some(entry) = entries.next_entry().await? {
            quarantined |= entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(&format!("{temporary_name}.quarantined_")));
        }
        assert!(quarantined, "stale transaction temp was not quarantined");
        Ok(())
    }

    #[tokio::test]
    async fn canonical_proof_age_survives_pending_promotion_and_restart() -> anyhow::Result<()> {
        let dir = TempDir::new()?;
        let config = ProofStorageConfig {
            path: dir.path().to_owned(),
            ..ProofStorageConfig::default()
        };
        let storage = ProofStorage::new(config.clone()).await?;
        let mut batch = create_test_batch_envelope_with_data(
            1,
            ProtocolSemanticVersion::canonical_genesis_version(),
            FriProof::Fake,
        );
        mark_test_batch_as_interop_bundle(&mut batch);
        let stored_batch = StoredBatch(batch);

        let pending_key = storage.save_pending_batch_with_proof(&stored_batch).await?;
        storage
            .promote_pending_batch_with_proof(&pending_key)
            .await?;
        storage.release_pending_batch_with_proof(&pending_key).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(storage);

        let restarted_storage = ProofStorage::new(config).await?;
        let (batch, accepted_age) = restarted_storage
            .get_batch_with_proof_and_age(1)
            .await?
            .expect("promoted proof must survive restart");
        assert_eq!(batch.batch_number(), 1);
        assert_eq!(batch.batch.logs.len(), 1);
        assert_eq!(batch.batch.messages, vec![vec![0x01, 0x12, 0x34]]);
        assert!(
            accepted_age >= Duration::from_millis(25),
            "acceptance age was reset on restart: {accepted_age:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn identical_replays_preserve_canonical_proof_mtime() -> anyhow::Result<()> {
        let dir = TempDir::new()?;
        let config = ProofStorageConfig {
            path: dir.path().to_owned(),
            ..ProofStorageConfig::default()
        };
        let storage = ProofStorage::new(config).await?;
        let stored_batch = StoredBatch(create_test_batch_envelope_with_data(
            1,
            ProtocolSemanticVersion::canonical_genesis_version(),
            FriProof::Fake,
        ));
        let canonical_path = dir.path().join("fri_batches/batch_1.json");

        storage.save_batch_with_proof(&stored_batch).await?;
        let deliberately_old_mtime = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        std::fs::File::options()
            .write(true)
            .open(&canonical_path)?
            .set_times(std::fs::FileTimes::new().set_modified(deliberately_old_mtime))?;
        let original_mtime = fs::metadata(&canonical_path).await?.modified()?;

        // GaplessCommitter takes this path when replaying a canonical proof without a pending
        // acceptance file.
        storage.save_batch_with_proof(&stored_batch).await?;
        assert_eq!(
            fs::metadata(&canonical_path).await?.modified()?,
            original_mtime
        );

        // It takes this path when replaying a recovered pending proof for the same batch.
        let pending_key = storage.save_pending_batch_with_proof(&stored_batch).await?;
        storage
            .promote_pending_batch_with_proof(&pending_key)
            .await?;
        storage.release_pending_batch_with_proof(&pending_key).await;
        assert_eq!(
            fs::metadata(&canonical_path).await?.modified()?,
            original_mtime
        );
        Ok(())
    }

    // Make sure files are being removed as expected
    #[tokio::test]
    async fn test_bounded_storage_capacity() -> anyhow::Result<()> {
        const LIMIT: u64 = 20000;
        let dir = TempDir::new()?;
        let path = dir.path().to_owned();
        let mut storage = BoundedFileStorage::new(path, LIMIT).await?;

        // Many small files
        let num_iter = 2000;
        for i in 0..num_iter {
            let key: String = i.to_string();
            let val = "a".repeat((LIMIT / num_iter) as usize);
            storage.store(&key, &val).await?;
            assert_eq!(storage.load::<String>(key.as_str()).await?, Some(val));
            if i >= num_iter {
                assert!(
                    storage
                        .load::<String>(&(i - num_iter + 1).to_string())
                        .await?
                        .is_some()
                );
                assert!(
                    storage
                        .load::<String>(&(i - num_iter).to_string())
                        .await?
                        .is_none()
                );
            }
        }

        // Large files
        let big_str = "a".repeat((LIMIT * 2 / 3) as usize);
        storage.store("key", &big_str).await?;
        // This removes most entries but not all
        assert!(
            storage
                .load::<String>(&(num_iter / 2).to_string())
                .await?
                .is_none()
        );
        assert!(
            storage
                .load::<String>(&(num_iter - 1).to_string())
                .await?
                .is_some()
        );
        // This should remove all the old entries
        storage.store("key2", &big_str).await?;
        assert!(storage.load::<String>("key").await?.is_none());
        // Files larger than limit won't be stored
        let very_big = "a".repeat((2 * LIMIT) as usize);
        storage.store("key", &very_big).await?;
        assert!(storage.load::<String>("key").await?.is_none());

        Ok(())
    }

    #[tokio::test]
    async fn test_bounded_storage_protected_entries_are_not_evicted() -> anyhow::Result<()> {
        const LIMIT: u64 = 600;
        let dir = TempDir::new()?;
        let path = dir.path().to_owned();
        let mut storage = BoundedFileStorage::new(path, LIMIT).await?;

        let protected_value = "p".repeat(200);
        let evictable_value = "e".repeat(200);
        storage.store("protected", &protected_value).await?;
        storage.store("evictable", &evictable_value).await?;

        let protected_keys = HashSet::from(["protected".to_string()]);
        let new_value = "n".repeat(200);
        storage
            .store_protected("new", &new_value, &protected_keys)
            .await?;

        assert_eq!(
            storage.load::<String>("protected").await?,
            Some(protected_value)
        );
        assert!(storage.load::<String>("evictable").await?.is_none());
        assert_eq!(storage.load::<String>("new").await?, Some(new_value));
        let too_large = "x".repeat((LIMIT * 2) as usize);
        assert!(
            storage
                .store_protected("too_large", &too_large, &protected_keys)
                .await
                .is_err()
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_protected_overwrite_failure_preserves_existing_entry() -> anyhow::Result<()> {
        const LIMIT: u64 = 700;
        let dir = TempDir::new()?;
        let path = dir.path().to_owned();
        let mut storage = BoundedFileStorage::new(path, LIMIT).await?;

        let old_value = "old".repeat(50);
        let other_value = "other".repeat(50);
        storage.store("batch", &old_value).await?;
        storage.store("other", &other_value).await?;

        let protected_keys = HashSet::from(["other".to_string()]);
        let too_large_replacement = "replacement".repeat(50);
        assert!(
            storage
                .store_protected("batch", &too_large_replacement, &protected_keys)
                .await
                .is_err()
        );

        assert_eq!(storage.load::<String>("batch").await?, Some(old_value));
        assert_eq!(storage.load::<String>("other").await?, Some(other_value));

        Ok(())
    }

    #[tokio::test]
    async fn test_bounded_storage_overwrites() -> anyhow::Result<()> {
        const LIMIT: u64 = 1 << 20;
        let dir = TempDir::new()?;
        let path = dir.path().to_owned();
        let mut storage = BoundedFileStorage::new(path, LIMIT).await?;
        // overrides in case of large strings
        let big_str_a = "a".repeat((LIMIT * 2 / 3) as usize);
        storage.store("key", &big_str_a).await?;
        assert_eq!(storage.load("key").await?, Some(big_str_a));
        let big_str_b = "b".repeat((LIMIT * 2 / 3) as usize);
        storage.store("key", &big_str_b).await?;
        assert_eq!(storage.load("key").await?, Some(big_str_b));
        Ok(())
    }

    #[tokio::test]
    async fn test_bounded_storage_overwrite_cleanup() -> anyhow::Result<()> {
        const LIMIT: u64 = 506;
        let dir = TempDir::new()?;
        let path = dir.path().to_owned();
        let mut storage = BoundedFileStorage::new(path, LIMIT).await?;

        let str1 = "a".repeat(100);
        let str2 = "ab".repeat(100);
        storage.store("0", &str2).await?;
        storage.store("1", &str2).await?;
        storage.store("0", &str1).await?;
        // TODO: handle acse when overwrite is the same value
        storage.store("0", &str2).await?;
        assert_eq!(storage.load::<String>("1").await?, None);
        storage.store("1", &str2).await?;
        // Duplicate was removed here
        assert!(storage.load::<String>("0").await?.is_some());
        assert!(storage.load::<String>("1").await?.is_some());

        Ok(())
    }
}
