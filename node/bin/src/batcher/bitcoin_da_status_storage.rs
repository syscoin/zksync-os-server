use crate::config::BitcoinDaFinalityMode;
use alloy::hex;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
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

    pub async fn reserve_republication(
        &self,
        version_hash: &str,
        max_attempts: u32,
    ) -> anyhow::Result<u32> {
        let hash = hex::decode(version_hash.strip_prefix("0x").unwrap_or(version_hash))
            .context("invalid Bitcoin DA republication hash")?;
        anyhow::ensure!(
            hash.len() == 32,
            "invalid Bitcoin DA republication hash length"
        );
        let hash = hex::encode(hash);
        for attempt in 1..=max_attempts {
            // Reservation names cannot match batch-receipt cleanup. A lost wallet response or
            // restart must not release a slot that may already have paid for publication.
            let path = self.base_dir.join(format!("republish_{hash}_{attempt}"));
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            let file = match options.open(path).await {
                Ok(file) => file,
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err).context("reserve Bitcoin DA republication"),
            };
            file.sync_all().await?;
            fs::File::open(&self.base_dir).await?.sync_all().await?;
            // The status directory itself can have been created during this startup.
            let parent = self
                .base_dir
                .parent()
                .filter(|path| !path.as_os_str().is_empty());
            fs::File::open(parent.unwrap_or(Path::new(".")))
                .await?
                .sync_all()
                .await?;
            return Ok(attempt);
        }
        anyhow::bail!(
            "Bitcoin DA republication limit exhausted for {hash}: {max_attempts} attempts; operator intervention required"
        )
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
