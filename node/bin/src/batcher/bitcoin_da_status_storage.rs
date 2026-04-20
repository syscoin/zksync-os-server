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
}
