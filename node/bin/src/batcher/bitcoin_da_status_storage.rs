use crate::config::BitcoinDaFinalityMode;
use alloy::hex;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::fs;

#[derive(Clone, Debug)]
pub struct BitcoinDaStatusStorage {
    base_dir: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct BitcoinDaBatchStatus {
    pub expected_hashes: Vec<String>,
    pub published_hashes: Vec<String>,
    pub finalized: bool,
    #[serde(default)]
    pub finality_policy: Option<BitcoinDaFinalityPolicy>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BitcoinDaFinalityPolicy {
    pub mode: BitcoinDaFinalityMode,
    pub confirmations: u64,
}

#[derive(Debug)]
pub struct BitcoinDaRepublicationClaim {
    pub attempt: u32,
    _lock: std::fs::File,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BitcoinDaRecoveryClock {
    version: u32,
    first_observed_unix_ms: u64,
    last_attempt_unix_ms: Option<u64>,
}

impl BitcoinDaRecoveryClock {
    fn validate(&self, now_unix_ms: u64) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.version == 1,
            "unsupported Bitcoin DA recovery clock version"
        );
        anyhow::ensure!(
            self.first_observed_unix_ms > 0 && self.first_observed_unix_ms <= now_unix_ms,
            "invalid or backwards Bitcoin DA recovery clock"
        );
        if let Some(last_attempt) = self.last_attempt_unix_ms {
            anyhow::ensure!(
                last_attempt >= self.first_observed_unix_ms && last_attempt <= now_unix_ms,
                "invalid or backwards Bitcoin DA recovery attempt clock"
            );
        }
        Ok(())
    }

    fn due(&self, now_unix_ms: u64, initial_delay: Duration, cooldown: Duration) -> bool {
        let (start, delay) = match self.last_attempt_unix_ms {
            Some(attempt) => (attempt, cooldown),
            None => (self.first_observed_unix_ms, initial_delay),
        };
        u128::from(now_unix_ms - start) >= delay.as_millis()
    }
}

fn normalized_republication_hash(version_hash: &str) -> anyhow::Result<String> {
    let hash = hex::decode(version_hash.strip_prefix("0x").unwrap_or(version_hash))
        .context("invalid Bitcoin DA republication hash")?;
    anyhow::ensure!(
        hash.len() == 32,
        "invalid Bitcoin DA republication hash length"
    );
    Ok(hex::encode(hash))
}

fn recovery_now_unix_ms() -> anyhow::Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("Bitcoin DA recovery clock is before UNIX epoch")?
        .as_millis()
        .try_into()
        .context("Bitcoin DA recovery clock exceeds u64 milliseconds")
}

impl BitcoinDaStatusStorage {
    pub fn new(base_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        let base_dir = base_dir.as_ref().to_owned();
        std::fs::create_dir_all(&base_dir)?;
        Ok(Self { base_dir })
    }

    fn path_for(&self, batch_number: u64) -> PathBuf {
        self.base_dir.join(format!("batch_{batch_number}.json"))
    }

    fn tmp_path_for(&self, batch_number: u64) -> PathBuf {
        self.base_dir.join(format!("batch_{batch_number}.json.tmp"))
    }

    pub async fn load(&self, batch_number: u64) -> anyhow::Result<Option<BitcoinDaBatchStatus>> {
        let path = self.path_for(batch_number);
        if !fs::try_exists(&path).await? {
            return Ok(None);
        }
        let bytes = fs::read(path).await?;
        Ok(Some(serde_json::from_slice(&bytes)?))
    }

    pub async fn save(
        &self,
        batch_number: u64,
        status: &BitcoinDaBatchStatus,
    ) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(status)?;
        let tmp_path = self.tmp_path_for(batch_number);
        let path = self.path_for(batch_number);
        fs::write(&tmp_path, bytes).await?;
        fs::rename(&tmp_path, &path).await?;
        Ok(())
    }

    #[cfg(test)]
    pub async fn reserve_republication(
        &self,
        version_hash: &str,
        max_attempts: u32,
    ) -> anyhow::Result<u32> {
        let storage = self.clone();
        let hash = normalized_republication_hash(version_hash)?;
        tokio::task::spawn_blocking(move || storage.reserve_republication_sync(&hash, max_attempts))
            .await?
    }

    fn reserve_republication_sync(&self, hash: &str, max_attempts: u32) -> anyhow::Result<u32> {
        for attempt in 1..=max_attempts {
            // Reservation names cannot match batch-receipt cleanup. A lost wallet response or
            // restart must not release a slot that may already have paid for publication.
            let path = self.base_dir.join(format!("republish_{hash}_{attempt}"));
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let file = match options.open(path) {
                Ok(file) => file,
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err).context("reserve Bitcoin DA republication"),
            };
            file.sync_all()?;
            self.sync_recovery_directory()?;
            return Ok(attempt);
        }
        anyhow::bail!(
            "Bitcoin DA republication limit exhausted for {hash}: {max_attempts} attempts; operator intervention required"
        )
    }

    pub async fn observe_recovery(&self, version_hash: &str) -> anyhow::Result<()> {
        self.observe_recovery_with_clock(version_hash, recovery_now_unix_ms)
            .await
    }

    async fn observe_recovery_with_clock(
        &self,
        version_hash: &str,
        clock: impl FnOnce() -> anyhow::Result<u64> + Send + 'static,
    ) -> anyhow::Result<()> {
        let storage = self.clone();
        let hash = normalized_republication_hash(version_hash)?;
        tokio::task::spawn_blocking(move || {
            let _lock = storage.recovery_lock(&hash)?;
            storage.load_or_create_recovery_clock(&hash, clock()?)?;
            Ok(())
        })
        .await?
    }

    pub async fn recovery_due(
        &self,
        version_hash: &str,
        initial_delay: Duration,
        cooldown: Duration,
    ) -> anyhow::Result<bool> {
        self.recovery_due_with_clock(version_hash, recovery_now_unix_ms, initial_delay, cooldown)
            .await
    }

    async fn recovery_due_with_clock(
        &self,
        version_hash: &str,
        clock: impl FnOnce() -> anyhow::Result<u64> + Send + 'static,
        initial_delay: Duration,
        cooldown: Duration,
    ) -> anyhow::Result<bool> {
        let storage = self.clone();
        let hash = normalized_republication_hash(version_hash)?;
        tokio::task::spawn_blocking(move || {
            let _lock = storage.recovery_lock(&hash)?;
            let now_unix_ms = clock()?;
            let clock = storage.load_or_create_recovery_clock(&hash, now_unix_ms)?;
            Ok(clock.due(now_unix_ms, initial_delay, cooldown))
        })
        .await?
    }

    pub async fn claim_republication(
        &self,
        version_hash: &str,
        initial_delay: Duration,
        cooldown: Duration,
        max_attempts: u32,
    ) -> anyhow::Result<Option<BitcoinDaRepublicationClaim>> {
        self.claim_republication_with_clock(
            version_hash,
            recovery_now_unix_ms,
            initial_delay,
            cooldown,
            max_attempts,
        )
        .await
    }

    async fn claim_republication_with_clock(
        &self,
        version_hash: &str,
        clock: impl FnOnce() -> anyhow::Result<u64> + Send + 'static,
        initial_delay: Duration,
        cooldown: Duration,
        max_attempts: u32,
    ) -> anyhow::Result<Option<BitcoinDaRepublicationClaim>> {
        anyhow::ensure!(
            max_attempts == 0 || !cooldown.is_zero(),
            "Bitcoin DA republication cooldown must be positive"
        );
        let storage = self.clone();
        let hash = normalized_republication_hash(version_hash)?;
        tokio::task::spawn_blocking(move || {
            let lock = storage.recovery_lock(&hash)?;
            // Read after acquiring the lock: an enqueued waiter must not mistake the
            // previous claimant's later timestamp for a backwards host clock.
            let now_unix_ms = clock()?;
            let mut clock = storage.load_or_create_recovery_clock(&hash, now_unix_ms)?;
            if !clock.due(now_unix_ms, initial_delay, cooldown) {
                return Ok(None);
            }
            // A restart or lost wallet response cannot reset this cooldown. Save it before
            // reserving the fee budget, and keep both operations under the same OS lock.
            clock.last_attempt_unix_ms = Some(now_unix_ms);
            storage.save_recovery_clock(&hash, &clock)?;
            let attempt = storage.reserve_republication_sync(&hash, max_attempts)?;
            Ok(Some(BitcoinDaRepublicationClaim {
                attempt,
                _lock: lock,
            }))
        })
        .await?
    }

    #[cfg(test)]
    pub(super) async fn observe_recovery_at(
        &self,
        version_hash: &str,
        now_unix_ms: u64,
    ) -> anyhow::Result<()> {
        self.observe_recovery_with_clock(version_hash, move || Ok(now_unix_ms))
            .await
    }

    #[cfg(test)]
    async fn recovery_due_at(
        &self,
        version_hash: &str,
        now_unix_ms: u64,
        initial_delay: Duration,
        cooldown: Duration,
    ) -> anyhow::Result<bool> {
        self.recovery_due_with_clock(
            version_hash,
            move || Ok(now_unix_ms),
            initial_delay,
            cooldown,
        )
        .await
    }

    #[cfg(test)]
    async fn claim_republication_at(
        &self,
        version_hash: &str,
        now_unix_ms: u64,
        initial_delay: Duration,
        cooldown: Duration,
        max_attempts: u32,
    ) -> anyhow::Result<Option<BitcoinDaRepublicationClaim>> {
        self.claim_republication_with_clock(
            version_hash,
            move || Ok(now_unix_ms),
            initial_delay,
            cooldown,
            max_attempts,
        )
        .await
    }

    #[cfg(test)]
    async fn claim_republication_number(
        &self,
        version_hash: &str,
        now_unix_ms: u64,
        initial_delay: Duration,
        cooldown: Duration,
        max_attempts: u32,
    ) -> anyhow::Result<Option<u32>> {
        Ok(self
            .claim_republication_at(
                version_hash,
                now_unix_ms,
                initial_delay,
                cooldown,
                max_attempts,
            )
            .await?
            .map(|claim| claim.attempt))
    }

    fn recovery_lock(&self, hash: &str) -> anyhow::Result<std::fs::File> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(self.base_dir.join(format!("recovery_{hash}.lock")))?;
        anyhow::ensure!(
            file.metadata()?.is_file(),
            "invalid Bitcoin DA recovery lock"
        );
        file.lock().context("lock Bitcoin DA recovery clock")?;
        Ok(file)
    }

    fn load_or_create_recovery_clock(
        &self,
        hash: &str,
        now_unix_ms: u64,
    ) -> anyhow::Result<BitcoinDaRecoveryClock> {
        let path = self.base_dir.join(format!("recovery_{hash}.json"));
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = match options.open(path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                // Older versions recorded durable fee reservations but no clock. Treat any
                // such reservation as a recent ambiguous attempt, never as a free retry.
                let prefix = format!("republish_{hash}_");
                let mut has_previous_attempt = false;
                for entry in std::fs::read_dir(&self.base_dir)? {
                    if entry?.file_name().to_string_lossy().starts_with(&prefix) {
                        has_previous_attempt = true;
                        break;
                    }
                }
                let clock = BitcoinDaRecoveryClock {
                    version: 1,
                    first_observed_unix_ms: now_unix_ms,
                    last_attempt_unix_ms: has_previous_attempt.then_some(now_unix_ms),
                };
                clock.validate(now_unix_ms)?;
                self.save_recovery_clock(hash, &clock)?;
                return Ok(clock);
            }
            Err(err) => return Err(err).context("read Bitcoin DA recovery clock"),
        };
        let metadata = file.metadata()?;
        anyhow::ensure!(
            metadata.is_file() && metadata.len() <= 1024,
            "invalid Bitcoin DA recovery clock file"
        );
        let mut bytes = Vec::new();
        file.take(1025).read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len() <= 1024, "oversized Bitcoin DA recovery clock");
        let clock: BitcoinDaRecoveryClock =
            serde_json::from_slice(&bytes).context("corrupt Bitcoin DA recovery clock")?;
        clock.validate(now_unix_ms)?;
        Ok(clock)
    }

    fn save_recovery_clock(
        &self,
        hash: &str,
        clock: &BitcoinDaRecoveryClock,
    ) -> anyhow::Result<()> {
        let mut tmp = tempfile::NamedTempFile::new_in(&self.base_dir)?;
        tmp.write_all(&serde_json::to_vec(clock)?)?;
        tmp.as_file().sync_all()?;
        tmp.persist(self.base_dir.join(format!("recovery_{hash}.json")))?;
        self.sync_recovery_directory()
    }

    fn sync_recovery_directory(&self) -> anyhow::Result<()> {
        std::fs::File::open(&self.base_dir)?.sync_all()?;
        let parent = self
            .base_dir
            .parent()
            .filter(|path| !path.as_os_str().is_empty());
        std::fs::File::open(parent.unwrap_or(Path::new(".")))?.sync_all()?;
        Ok(())
    }

    pub async fn delete(&self, batch_number: u64) -> anyhow::Result<()> {
        let path = self.path_for(batch_number);
        if fs::try_exists(&path).await? {
            fs::remove_file(path).await?;
        }
        Ok(())
    }

    pub async fn delete_through(&self, last_committed_batch: u64) -> anyhow::Result<()> {
        let mut entries = fs::read_dir(&self.base_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(batch_number) = parse_batch_number(&name) else {
                continue;
            };
            if batch_number <= last_committed_batch {
                fs::remove_file(entry.path()).await?;
            }
        }
        Ok(())
    }
}

fn parse_batch_number(name: &str) -> Option<u64> {
    name.strip_prefix("batch_")
        .and_then(|value| value.strip_suffix(".json"))
        .and_then(|value| value.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "508c5e8c327c14e2e1a72ba34eeb452f37458b209ed63a294d999b4c86675982";

    #[tokio::test]
    async fn enqueued_observe_due_and_claim_read_clock_after_prior_wallet_claim_releases() {
        use std::sync::{
            Arc,
            atomic::{AtomicU64, AtomicUsize, Ordering},
        };
        for operation in 0..3 {
            let root = tempfile::tempdir().unwrap();
            let storage = BitcoinDaStatusStorage::new(root.path()).unwrap();
            storage.observe_recovery_at(HASH, 1_000).await.unwrap();
            let first = storage
                .claim_republication_at(HASH, 1_001, Duration::ZERO, Duration::from_millis(10), 3)
                .await
                .unwrap()
                .unwrap();
            let now = Arc::new(AtomicU64::new(1_000));
            let reads = Arc::new(AtomicUsize::new(0));
            let clock = {
                let now = now.clone();
                let reads = reads.clone();
                move || {
                    reads.fetch_add(1, Ordering::SeqCst);
                    Ok(now.load(Ordering::SeqCst))
                }
            };
            let waiting = async {
                match operation {
                    0 => storage.observe_recovery_with_clock(HASH, clock).await,
                    1 => storage
                        .recovery_due_with_clock(
                            HASH,
                            clock,
                            Duration::ZERO,
                            Duration::from_millis(10),
                        )
                        .await
                        .map(|_| ()),
                    _ => storage
                        .claim_republication_with_clock(
                            HASH,
                            clock,
                            Duration::ZERO,
                            Duration::from_millis(10),
                            3,
                        )
                        .await
                        .map(|_| ()),
                }
            };
            tokio::pin!(waiting);
            assert!(futures::poll!(&mut waiting).is_pending());
            assert_eq!(reads.load(Ordering::SeqCst), 0);
            now.store(1_002, Ordering::SeqCst);
            drop(first);
            waiting.await.unwrap();
            assert_eq!(reads.load(Ordering::SeqCst), 1);
            let rollback = || Ok(1_000);
            let result = match operation {
                0 => storage.observe_recovery_with_clock(HASH, rollback).await,
                1 => storage
                    .recovery_due_with_clock(
                        HASH,
                        rollback,
                        Duration::ZERO,
                        Duration::from_millis(10),
                    )
                    .await
                    .map(|_| ()),
                _ => storage
                    .claim_republication_with_clock(
                        HASH,
                        rollback,
                        Duration::ZERO,
                        Duration::from_millis(10),
                        3,
                    )
                    .await
                    .map(|_| ()),
            };
            assert!(result.unwrap_err().to_string().contains("backwards"));
            assert_eq!(storage.reserve_republication(HASH, 3).await.unwrap(), 2);
        }
    }

    #[tokio::test]
    async fn queued_claim_starts_full_cooldown_at_lock_acquisition_not_enqueue_time() {
        use std::sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        };
        let root = tempfile::tempdir().unwrap();
        let storage = BitcoinDaStatusStorage::new(root.path()).unwrap();
        storage.observe_recovery_at(HASH, 1_000).await.unwrap();
        let first = storage
            .claim_republication_at(HASH, 1_001, Duration::ZERO, Duration::from_millis(10), 3)
            .await
            .unwrap()
            .unwrap();
        let now = Arc::new(AtomicU64::new(1_002));
        let later = now.clone();
        let waiting = storage.claim_republication_with_clock(
            HASH,
            move || Ok(later.load(Ordering::SeqCst)),
            Duration::ZERO,
            Duration::from_millis(10),
            3,
        );
        tokio::pin!(waiting);
        assert!(futures::poll!(&mut waiting).is_pending());
        now.store(1_100, Ordering::SeqCst);
        drop(first);
        let second = waiting.await.unwrap().unwrap();
        assert_eq!(second.attempt, 2);
        drop(second);
        assert!(
            !storage
                .recovery_due_at(HASH, 1_109, Duration::ZERO, Duration::from_millis(10))
                .await
                .unwrap()
        );
        assert!(
            storage
                .recovery_due_at(HASH, 1_110, Duration::ZERO, Duration::from_millis(10))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn recovery_clock_ages_across_restart_and_receipt_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let storage = BitcoinDaStatusStorage::new(root.path()).unwrap();
        storage.observe_recovery_at(HASH, 1_000).await.unwrap();
        storage.observe_recovery_at(HASH, 1_050).await.unwrap();
        storage
            .save(1, &BitcoinDaBatchStatus::default())
            .await
            .unwrap();
        storage.delete(1).await.unwrap();
        storage
            .save(2, &BitcoinDaBatchStatus::default())
            .await
            .unwrap();
        storage.delete_through(2).await.unwrap();
        let reopened = BitcoinDaStatusStorage::new(root.path()).unwrap();
        assert!(
            !reopened
                .recovery_due_at(
                    HASH,
                    1_099,
                    Duration::from_millis(100),
                    Duration::from_secs(90)
                )
                .await
                .unwrap()
        );
        assert!(
            reopened
                .recovery_due_at(
                    HASH,
                    1_100,
                    Duration::from_millis(100),
                    Duration::from_secs(90)
                )
                .await
                .unwrap()
        );
        assert_eq!(
            reopened
                .claim_republication_number(
                    HASH,
                    1_100,
                    Duration::from_millis(100),
                    Duration::from_secs(90),
                    3
                )
                .await
                .unwrap(),
            Some(1)
        );
        let reopened = BitcoinDaStatusStorage::new(root.path()).unwrap();
        assert!(
            !reopened
                .recovery_due_at(
                    HASH,
                    91_099,
                    Duration::from_millis(100),
                    Duration::from_secs(90)
                )
                .await
                .unwrap()
        );
        assert_eq!(
            reopened
                .claim_republication_number(
                    HASH,
                    91_100,
                    Duration::from_millis(100),
                    Duration::from_secs(90),
                    3
                )
                .await
                .unwrap(),
            Some(2)
        );
    }

    #[tokio::test]
    async fn missing_delay_does_not_shorten_unconfirmed_or_post_attempt_cooldown() {
        let root = tempfile::tempdir().unwrap();
        let storage = BitcoinDaStatusStorage::new(root.path()).unwrap();
        storage.observe_recovery_at(HASH, 1_000).await.unwrap();
        let missing = Duration::from_secs(300);
        let normal = Duration::from_secs(5_400);
        assert!(
            storage
                .recovery_due_at(HASH, 301_000, missing, normal)
                .await
                .unwrap()
        );
        assert!(
            !storage
                .recovery_due_at(HASH, 301_000, normal, normal)
                .await
                .unwrap()
        );
        assert_eq!(
            storage
                .claim_republication_number(HASH, 301_000, missing, normal, 3)
                .await
                .unwrap(),
            Some(1)
        );
        assert!(
            !storage
                .recovery_due_at(HASH, 601_000, missing, normal)
                .await
                .unwrap()
        );
        assert!(
            storage
                .recovery_due_at(HASH, 5_701_000, missing, normal)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn concurrent_aliases_only_claim_one_due_attempt() {
        let root = tempfile::tempdir().unwrap();
        let storage = BitcoinDaStatusStorage::new(root.path()).unwrap();
        storage.observe_recovery_at(HASH, 1_000).await.unwrap();
        let alias = format!("0x{}", HASH.to_uppercase());
        let (a, b, c) = tokio::join!(
            storage.claim_republication_number(
                HASH,
                1_100,
                Duration::from_millis(100),
                Duration::from_secs(90),
                3
            ),
            storage.claim_republication_number(
                &alias,
                1_100,
                Duration::from_millis(100),
                Duration::from_secs(90),
                3
            ),
            storage.claim_republication_number(
                HASH,
                1_100,
                Duration::from_millis(100),
                Duration::from_secs(90),
                3
            ),
        );
        let claimed = [a.unwrap(), b.unwrap(), c.unwrap()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(claimed, vec![1]);
        assert_eq!(storage.reserve_republication(HASH, 3).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn wallet_claim_holds_os_lock_even_after_a_short_cooldown() {
        let root = tempfile::tempdir().unwrap();
        let storage = BitcoinDaStatusStorage::new(root.path()).unwrap();
        storage.observe_recovery_at(HASH, 1_000).await.unwrap();
        let first = storage
            .claim_republication_at(
                HASH,
                1_100,
                Duration::from_millis(100),
                Duration::from_millis(1),
                3,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.attempt, 1);
        let alias = format!("0x{}", HASH.to_uppercase());
        let second = storage.claim_republication_at(
            &alias,
            1_101,
            Duration::from_millis(100),
            Duration::from_millis(1),
            3,
        );
        tokio::pin!(second);
        assert!(futures::poll!(&mut second).is_pending());
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.path().join(format!("recovery_{HASH}.lock")))
            .unwrap();
        assert!(
            file.try_lock().is_err(),
            "wallet claim must retain its OS lock"
        );
        drop(first);
        assert_eq!(second.await.unwrap().unwrap().attempt, 2);
    }

    #[tokio::test]
    async fn legacy_attempts_get_conservative_cooldown_and_keep_the_budget() {
        let root = tempfile::tempdir().unwrap();
        let storage = BitcoinDaStatusStorage::new(root.path()).unwrap();
        assert_eq!(storage.reserve_republication(HASH, 2).await.unwrap(), 1);
        assert_eq!(
            storage
                .claim_republication_number(
                    HASH,
                    1_000,
                    Duration::from_millis(10),
                    Duration::from_millis(100),
                    2
                )
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            storage
                .claim_republication_number(
                    HASH,
                    1_100,
                    Duration::from_millis(10),
                    Duration::from_millis(100),
                    2
                )
                .await
                .unwrap(),
            Some(2)
        );
        assert!(
            storage
                .claim_republication_number(
                    HASH,
                    1_200,
                    Duration::from_millis(10),
                    Duration::from_millis(100),
                    2
                )
                .await
                .unwrap_err()
                .to_string()
                .contains("limit exhausted")
        );
    }

    #[tokio::test]
    async fn recovery_corruption_backwards_clock_and_storage_failure_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let storage = BitcoinDaStatusStorage::new(root.path()).unwrap();
        assert!(storage.observe_recovery_at(HASH, 0).await.is_err());
        storage.observe_recovery_at(HASH, 1_000).await.unwrap();
        assert!(
            storage
                .claim_republication_number(HASH, 999, Duration::ZERO, Duration::from_secs(90), 3)
                .await
                .is_err()
        );
        let path = root.path().join(format!("recovery_{HASH}.json"));
        for bytes in [
            b"corrupt".as_slice(),
            br#"{"version":2,"first_observed_unix_ms":1000,"last_attempt_unix_ms":null}"#,
            br#"{"version":1,"first_observed_unix_ms":1000,"last_attempt_unix_ms":999}"#,
            br#"{"version":1,"first_observed_unix_ms":1000,"last_attempt_unix_ms":2000}"#,
            br#"{"version":1,"first_observed_unix_ms":1000,"last_attempt_unix_ms":null,"unexpected":true}"#,
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert!(storage.claim_republication_number(HASH, 1_100, Duration::ZERO, Duration::from_secs(90), 3).await.is_err());
        }
        assert!(!root.path().join(format!("republish_{HASH}_1")).exists());
        let broken = root.path().join("not-a-directory");
        std::fs::write(&broken, b"file").unwrap();
        let storage = BitcoinDaStatusStorage { base_dir: broken };
        assert!(
            storage
                .claim_republication_number(HASH, 1_100, Duration::ZERO, Duration::from_secs(90), 3)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn current_command_observations_age_together() {
        let root = tempfile::tempdir().unwrap();
        let storage = BitcoinDaStatusStorage::new(root.path()).unwrap();
        let second = "608c5e8c327c14e2e1a72ba34eeb452f37458b209ed63a294d999b4c86675982";
        for hash in [HASH, second] {
            storage.observe_recovery_at(hash, 1_000).await.unwrap();
        }
        let delay = Duration::from_millis(100);
        assert_eq!(
            storage
                .claim_republication_number(HASH, 1_100, delay, delay, 3)
                .await
                .unwrap(),
            Some(1)
        );
        assert_eq!(
            storage
                .claim_republication_number(second, 1_100, delay, delay, 3)
                .await
                .unwrap(),
            Some(1)
        );
    }

    #[tokio::test]
    async fn republication_budget_survives_aliases_restart_and_receipt_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let storage = BitcoinDaStatusStorage::new(root.path()).unwrap();
        assert_eq!(storage.reserve_republication(HASH, 2).await.unwrap(), 1);
        storage
            .save(1, &BitcoinDaBatchStatus::default())
            .await
            .unwrap();
        storage.delete(1).await.unwrap();
        storage
            .save(2, &BitcoinDaBatchStatus::default())
            .await
            .unwrap();
        storage.delete_through(2).await.unwrap();
        let reopened = BitcoinDaStatusStorage::new(root.path()).unwrap();
        assert_eq!(
            reopened
                .reserve_republication(&format!("0x{}", HASH.to_uppercase()), 2)
                .await
                .unwrap(),
            2
        );
        assert!(
            reopened
                .reserve_republication(HASH, 2)
                .await
                .unwrap_err()
                .to_string()
                .contains("limit exhausted")
        );
    }

    #[tokio::test]
    async fn concurrent_republication_reservations_cannot_share_a_slot() {
        let root = tempfile::tempdir().unwrap();
        let storage = BitcoinDaStatusStorage::new(root.path()).unwrap();
        let (a, b, c) = tokio::join!(
            storage.reserve_republication(HASH, 2),
            storage.reserve_republication(HASH, 2),
            storage.reserve_republication(HASH, 2),
        );
        let mut slots = [a, b, c]
            .into_iter()
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        slots.sort();
        assert_eq!(slots, vec![1, 2]);
    }

    #[tokio::test]
    async fn disabled_invalid_and_unwritable_republication_budgets_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let storage = BitcoinDaStatusStorage::new(root.path()).unwrap();
        assert!(storage.reserve_republication(HASH, 0).await.is_err());
        assert!(
            storage
                .reserve_republication("../invalid", 2)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        let invalid_storage = BitcoinDaStatusStorage {
            base_dir: root.path().join("not-a-directory"),
        };
        fs::write(&invalid_storage.base_dir, b"file").await.unwrap();
        assert!(
            invalid_storage
                .reserve_republication(HASH, 2)
                .await
                .is_err()
        );
    }
}
