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
