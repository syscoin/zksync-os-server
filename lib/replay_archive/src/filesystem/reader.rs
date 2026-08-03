use crate::{
    ReplayArchiveKey, ReplayArchiveKeyPage, ReplayArchiveStorageReader, format_block_hash,
};
use alloy::primitives::{BlockHash, BlockNumber};
use anyhow::Context as _;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::str::FromStr as _;

/// File-system implementation of [`ReplayArchiveStorageReader`].
///
/// Lists the flat layout (`<root>/<block_number>/<block_hash>`).
#[derive(Debug, Clone)]
pub struct FileSystemReplayArchiveReader {
    root_path: PathBuf,
}

impl FileSystemReplayArchiveReader {
    pub fn new(root_path: PathBuf) -> Self {
        Self { root_path }
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    fn object_path(&self, key: &ReplayArchiveKey) -> PathBuf {
        self.root_path
            .join(key.block_number.to_string())
            .join(format_block_hash(key.block_hash))
    }

    async fn list_block_dir_objects(
        &self,
        block_dir: &Path,
        block_number: BlockNumber,
        keys: &mut Vec<ReplayArchiveKey>,
    ) -> anyhow::Result<()> {
        let mut object_entries = tokio::fs::read_dir(block_dir).await.with_context(|| {
            format!(
                "failed to read replay archive block directory {}",
                block_dir.display()
            )
        })?;
        while let Some(object_entry) = object_entries.next_entry().await.with_context(|| {
            format!(
                "failed to read replay archive object entry {}",
                block_dir.display()
            )
        })? {
            if !object_entry.file_type().await?.is_file() {
                continue;
            }
            let file_name = object_entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            // Interrupted-write leftovers accompany data files in the flat layout; only plain
            // block hash names are archive objects.
            let Ok(block_hash) = BlockHash::from_str(file_name) else {
                if !file_name.contains(".partial") {
                    tracing::warn!(
                        path = %object_entry.path().display(),
                        "Skipping replay archive entry that is not a block hash"
                    );
                }
                continue;
            };
            keys.push(ReplayArchiveKey::new(block_number, block_hash));
        }
        Ok(())
    }
}

#[async_trait]
impl ReplayArchiveStorageReader for FileSystemReplayArchiveReader {
    // The local filesystem backend does not paginate: the first page contains every key.
    async fn list_keys_page(
        &self,
        page_token: Option<String>,
    ) -> anyhow::Result<ReplayArchiveKeyPage> {
        anyhow::ensure!(
            page_token.is_none(),
            "filesystem replay archive reader returns a single page"
        );
        let mut keys = Vec::new();
        let mut root_entries = tokio::fs::read_dir(&self.root_path)
            .await
            .with_context(|| {
                format!(
                    "failed to read replay archive root {}",
                    self.root_path.display()
                )
            })?;

        while let Some(root_entry) = root_entries.next_entry().await.with_context(|| {
            format!(
                "failed to read replay archive root entry {}",
                self.root_path.display()
            )
        })? {
            if !root_entry.file_type().await?.is_dir() {
                continue;
            }
            let dir_name = root_entry.file_name();
            let Some(dir_name) = dir_name.to_str() else {
                continue;
            };

            let block_number = parse_block_number_entry(&block_entry)?;
            let mut object_entries =
                tokio::fs::read_dir(block_entry.path())
                    .await
                    .with_context(|| {
                        format!(
                            "failed to read replay archive block directory {}",
                            block_entry.path().display()
                        )
                    })?;
            while let Some(object_entry) = object_entries.next_entry().await.with_context(|| {
                format!(
                    "failed to read replay archive object entry {}",
                    block_entry.path().display()
                )
            })? {
                let object_metadata = object_entry.metadata().await.with_context(|| {
                    format!(
                        "failed to read replay archive object metadata {}",
                        object_entry.path().display()
                    )
                })?;
                if !object_metadata.is_file() {
                    continue;
                }
                // SYSCOIN: atomic archive publication uses hidden temp files before final publish.
                if is_temporary_archive_object(&object_entry) {
                    continue;
                }

                let block_hash = parse_block_hash_entry(&object_entry)?;
                let key = ReplayArchiveKey::new(session.clone(), block_number, block_hash);
                let bytes = tokio::fs::read(object_entry.path())
                    .await
                    .with_context(|| {
                        format!(
                            "failed to read replay archive object {}",
                            object_entry.path().display()
                        )
                    })?;
                if sender
                    .send(Ok(ReplayArchiveObject { key, bytes }))
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
        }

        Ok(ReplayArchiveKeyPage {
            keys,
            next_page_token: None,
        })
    }

    async fn fetch_object(&self, key: &ReplayArchiveKey) -> anyhow::Result<Vec<u8>> {
        let path = self.object_path(key);
        tokio::fs::read(&path)
            .await
            .with_context(|| format!("failed to read replay archive object {}", path.display()))
    }
}

fn is_temporary_archive_object(entry: &tokio::fs::DirEntry) -> bool {
    let file_name = entry.file_name();
    let file_name = file_name.to_string_lossy();
    file_name.starts_with('.') && file_name.ends_with(".tmp")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ReplayArchiveKey, format_block_hash};

    #[tokio::test]
    async fn list_objects_ignores_temporary_archive_files() {
        let tempdir = tempfile::tempdir().unwrap();
        let session = ReplayArchiveSession::new(42, "node-a").unwrap();
        let block_number = 7;
        let block_hash = BlockHash::with_last_byte(1);
        let session_path = tempdir.path().join(session.folder_name());
        let block_path = session_path.join(block_number.to_string());
        tokio::fs::create_dir_all(&block_path).await.unwrap();
        tokio::fs::write(
            block_path.join(format!(".{}.999.1.tmp", format_block_hash(block_hash))),
            b"partial",
        )
        .await
        .unwrap();
        tokio::fs::write(block_path.join(format_block_hash(block_hash)), b"complete")
            .await
            .unwrap();

        let reader = FileSystemReplayArchiveReader::new(tempdir.path().to_path_buf());
        let objects = reader
            .list_objects()
            .await
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<anyhow::Result<Vec<_>>>()
            .unwrap();

        assert_eq!(objects.len(), 1);
        assert_eq!(
            objects[0].key,
            ReplayArchiveKey::new(session, block_number, block_hash)
        );
        assert_eq!(objects[0].bytes, b"complete");
    }
}

