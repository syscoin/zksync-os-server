use crate::{
    ReplayArchiveSession, ReplayArchiveStorage, ReplayArchiver, ReplayRecordArchiver,
    format_block_hash,
};
use alloy::primitives::{BlockHash, BlockNumber};
use anyhow::Context as _;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::AsyncWriteExt as _;

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);
const TEMP_FILE_CREATE_ATTEMPTS: usize = 1024;

/// File-system implementation of [`ReplayArchiveStorage`].
///
/// The archive root is backend configuration. The session is created as a direct child of that root,
/// and objects are written to `<root>/<session>/<block_number>/<block_hash>`.
#[derive(Debug, Clone)]
pub struct FileSystemReplayArchiveStorage {
    root_path: PathBuf,
    session: ReplayArchiveSession,
}

impl FileSystemReplayArchiveStorage {
    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    pub fn session(&self) -> &ReplayArchiveSession {
        &self.session
    }

    fn session_path(&self) -> PathBuf {
        self.root_path.join(self.session.folder_name())
    }

    fn block_dir_path(&self, block_number: BlockNumber) -> PathBuf {
        self.session_path().join(block_number.to_string())
    }

    fn object_path(&self, block_number: BlockNumber, block_hash: BlockHash) -> PathBuf {
        self.block_dir_path(block_number)
            .join(format_block_hash(block_hash))
    }

    fn temporary_object_path(&self, block_number: BlockNumber, block_hash: BlockHash) -> PathBuf {
        let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        self.block_dir_path(block_number).join(format!(
            ".{}.{}.{}.tmp",
            format_block_hash(block_hash),
            std::process::id(),
            counter
        ))
    }

    async fn create_temporary_object(
        &self,
        block_number: BlockNumber,
        block_hash: BlockHash,
    ) -> anyhow::Result<(PathBuf, tokio::fs::File)> {
        let candidate_paths = (0..TEMP_FILE_CREATE_ATTEMPTS)
            .map(|_| self.temporary_object_path(block_number, block_hash));
        create_temporary_object_from_candidates(candidate_paths).await
    }
}

async fn create_temporary_object_from_candidates<I>(
    candidate_paths: I,
) -> anyhow::Result<(PathBuf, tokio::fs::File)>
where
    I: IntoIterator<Item = PathBuf>,
{
    let mut attempts = 0;
    for temporary_object_path in candidate_paths {
        attempts += 1;
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_object_path)
            .await
        {
            Ok(file) => return Ok((temporary_object_path, file)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "failed to create temporary replay archive object {}",
                        temporary_object_path.display()
                    )
                });
            }
        }
    }

    anyhow::bail!("failed to create temporary replay archive object after {attempts} attempts");
}

async fn sync_directory(path: &Path) -> anyhow::Result<()> {
    let dir = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("failed to open directory {} for sync", path.display()))?;
    dir.sync_all()
        .await
        .with_context(|| format!("failed to sync directory {}", path.display()))
}

#[async_trait]
impl ReplayArchiveStorage for FileSystemReplayArchiveStorage {
    type Config = PathBuf;

    async fn init(root_path: Self::Config, session: ReplayArchiveSession) -> anyhow::Result<Self> {
        tokio::fs::create_dir_all(&root_path)
            .await
            .with_context(|| {
                format!(
                    "failed to create replay archive root {}",
                    root_path.display()
                )
            })?;

        let session_path = root_path.join(session.folder_name());
        tokio::fs::create_dir(&session_path)
            .await
            .with_context(|| {
                format!(
                    "failed to create append-only replay archive session {}",
                    session_path.display()
                )
            })?;

        Ok(Self { root_path, session })
    }

    async fn append_object(
        &self,
        block_number: BlockNumber,
        block_hash: BlockHash,
        object: Vec<u8>,
    ) -> anyhow::Result<()> {
        let block_dir_path = self.block_dir_path(block_number);
        tokio::fs::create_dir_all(&block_dir_path)
            .await
            .with_context(|| {
                format!(
                    "failed to create replay archive block directory {}",
                    block_dir_path.display()
                )
            })?;
        sync_directory(&self.session_path()).await?;

        // SYSCOIN: avoid letting the commit gate observe a final archive path until
        // the object is fully written, synced, and published without overwriting.
        let object_path = self.object_path(block_number, block_hash);
        let (temporary_object_path, mut file) = self
            .create_temporary_object(block_number, block_hash)
            .await?;

        let publish_result: anyhow::Result<()> = async {
            file.write_all(&object).await.with_context(|| {
                format!(
                    "failed to write replay archive object {}",
                    temporary_object_path.display()
                )
            })?;
            file.flush().await.with_context(|| {
                format!(
                    "failed to flush replay archive object {}",
                    temporary_object_path.display()
                )
            })?;
            file.sync_all().await.with_context(|| {
                format!(
                    "failed to sync replay archive object {}",
                    temporary_object_path.display()
                )
            })?;
            drop(file);

            tokio::fs::hard_link(&temporary_object_path, &object_path)
                .await
                .with_context(|| {
                    format!(
                        "failed to publish append-only replay archive object {}",
                        object_path.display()
                    )
                })?;
            sync_directory(&block_dir_path).await?;

            if let Err(err) = tokio::fs::remove_file(&temporary_object_path).await {
                tracing::warn!(
                    temporary_object_path = %temporary_object_path.display(),
                    "failed to remove temporary replay archive object after publish: {err}"
                );
            } else if let Err(err) = sync_directory(&block_dir_path).await {
                tracing::warn!(
                    block_dir_path = %block_dir_path.display(),
                    "failed to sync replay archive directory after temporary object cleanup: {err}"
                );
            }
            Ok(())
        }
        .await;

        if publish_result.is_err() {
            let _ = tokio::fs::remove_file(&temporary_object_path).await;
        }
        publish_result?;
        Ok(())
    }

    async fn contains_object(
        &self,
        block_number: BlockNumber,
        block_hash: BlockHash,
    ) -> anyhow::Result<bool> {
        let object_path = self.object_path(block_number, block_hash);
        match tokio::fs::metadata(&object_path).await {
            Ok(metadata) => Ok(metadata.is_file()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(err) => Err(err).with_context(|| {
                format!(
                    "failed to read replay archive object metadata {}",
                    object_path.display()
                )
            }),
        }
    }
}

/// File-system replay archiver that stores plaintext JSON replay records.
#[derive(Debug, Clone)]
pub struct FileSystemReplayArchiver {
    inner: ReplayRecordArchiver<FileSystemReplayArchiveStorage>,
}

impl FileSystemReplayArchiver {
    pub fn new(storage: FileSystemReplayArchiveStorage) -> Self {
        Self {
            inner: ReplayRecordArchiver::new(storage),
        }
    }

    pub async fn init(root_path: PathBuf, session: ReplayArchiveSession) -> anyhow::Result<Self> {
        let storage = FileSystemReplayArchiveStorage::init(root_path, session).await?;
        Ok(Self::new(storage))
    }
}

#[async_trait]
impl ReplayArchiver for FileSystemReplayArchiver {
    async fn append_replay_record(
        &self,
        block_hash: BlockHash,
        replay_record: zksync_os_storage_api::ReplayRecord,
    ) -> anyhow::Result<()> {
        self.inner
            .append_replay_record(block_hash, replay_record)
            .await
    }

    async fn contains_replay_record(
        &self,
        block_number: BlockNumber,
        block_hash: BlockHash,
    ) -> anyhow::Result<bool> {
        self.inner
            .contains_replay_record(block_number, block_hash)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn contains_object_ignores_temporary_archive_files() {
        let tempdir = tempfile::tempdir().unwrap();
        let session = ReplayArchiveSession::new(42, "node-a").unwrap();
        let storage = FileSystemReplayArchiveStorage::init(tempdir.path().to_path_buf(), session)
            .await
            .unwrap();
        let block_number = 7;
        let block_hash = BlockHash::with_last_byte(1);
        let block_dir_path = storage.block_dir_path(block_number);
        tokio::fs::create_dir_all(&block_dir_path).await.unwrap();

        let temporary_object_path = storage.temporary_object_path(block_number, block_hash);
        tokio::fs::write(&temporary_object_path, b"partial")
            .await
            .unwrap();

        assert!(
            !storage
                .contains_object(block_number, block_hash)
                .await
                .unwrap()
        );
        assert!(!storage.object_path(block_number, block_hash).exists());
    }

    #[tokio::test]
    async fn create_temporary_object_retries_stale_temporary_file_collision() {
        let tempdir = tempfile::tempdir().unwrap();
        let stale_path = tempdir.path().join(".stale.tmp");
        let fresh_path = tempdir.path().join(".fresh.tmp");
        tokio::fs::write(&stale_path, b"stale").await.unwrap();

        let (created_path, file) =
            create_temporary_object_from_candidates([stale_path.clone(), fresh_path.clone()])
                .await
                .unwrap();
        drop(file);

        assert_eq!(created_path, fresh_path);
        assert_eq!(tokio::fs::read(stale_path).await.unwrap(), b"stale");
        assert!(created_path.exists());
    }

    #[tokio::test]
    async fn append_object_publishes_complete_final_path() {
        let tempdir = tempfile::tempdir().unwrap();
        let session = ReplayArchiveSession::new(42, "node-a").unwrap();
        let storage = FileSystemReplayArchiveStorage::init(tempdir.path().to_path_buf(), session)
            .await
            .unwrap();
        let block_number = 7;
        let block_hash = BlockHash::with_last_byte(1);
        let object = b"complete replay object".to_vec();

        storage
            .append_object(block_number, block_hash, object.clone())
            .await
            .unwrap();

        assert!(
            storage
                .contains_object(block_number, block_hash)
                .await
                .unwrap()
        );
        assert_eq!(
            tokio::fs::read(storage.object_path(block_number, block_hash))
                .await
                .unwrap(),
            object
        );
        let mut entries = tokio::fs::read_dir(storage.block_dir_path(block_number))
            .await
            .unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            assert!(
                !entry.file_name().to_string_lossy().ends_with(".tmp"),
                "temporary archive file was left behind: {:?}",
                entry.path()
            );
        }
    }
}
