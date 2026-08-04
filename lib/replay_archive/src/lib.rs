use alloy::primitives::{BlockHash, BlockNumber};
use async_trait::async_trait;
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use zksync_os_storage_api::ReplayRecord;

mod age_encrypted;
mod component;
mod filesystem;
mod gate_component;
#[cfg(feature = "gcp")]
mod gcs;
mod init;
#[cfg(feature = "gcp")]
mod kms;
mod metrics;
mod reader;
mod recovery;
mod replay_record;
mod s3;
mod write_replay;

pub use age_encrypted::{AgeEncryptedReplayArchiver, ArchiveRecipient};
pub use component::{ReplayArchiveComponent, ReplayArchiveRecord, ReplayArchiveSender};
pub use filesystem::{
    FileSystemReplayArchiveReader, FileSystemReplayArchiveStorage, FileSystemReplayArchiver,
};
pub use gate_component::ReplayArchiveGateComponent;
#[cfg(feature = "gcp")]
pub use gcs::{
    GcsReplayArchiveAuthMode, GcsReplayArchiveConfig, GcsReplayArchiveReader,
    GcsReplayArchiveStorage,
};
pub use init::{
    InitializedReplayArchive, ReplayArchiveConfig, ReplayArchiveEncryptionConfig,
    init_replay_archive,
};
#[cfg(feature = "gcp")]
pub use kms::{GcpKmsClient, GcpKmsConfig, GcpKmsIdentity, GcpKmsRecipient};
pub use reader::{ReplayArchiveKeyPage, ReplayArchiveStorageReader};
pub use recovery::{
    ArchiveIdentity, DEFAULT_DECRYPT_CONCURRENCY, DEFAULT_DOWNLOAD_CONCURRENCY,
    download_all_replay_archive_objects, parse_age_x25519_identity, read_age_x25519_identity,
    recover_replay_records_to_rocksdb, recover_replay_records_to_rocksdb_with_optional_decryption,
};
pub use replay_record::ReplayRecordArchiver;
pub use s3::{
    S3ReplayArchiveAuthMode, S3ReplayArchiveConfig, S3ReplayArchiveReader, S3ReplayArchiveStorage,
};
pub use write_replay::ReplayArchivingWriteReplay;

pub const REPLAY_ARCHIVE_QUEUE_SIZE: usize = 128;

// SYSCOIN: Conditional cloud-upload retries carry an unpredictable token so a retry conflict can
// be distinguished from an object created by another archive writer without weakening fail-closed
// session ownership.
pub(crate) const UPLOAD_TOKEN_METADATA_KEY: &str = "upload-token";

fn new_upload_token() -> String {
    alloy::hex::encode(rand::random::<[u8; 32]>())
}

fn ensure_upload_token_matches(
    location: &str,
    expected_token: &str,
    stored_token: Option<&str>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        stored_token == Some(expected_token),
        "append-only replay archive object already exists with a different upload token at {location}"
    );
    Ok(())
}

/// Process-local identities returned by successful backend writes.
///
/// SYSCOIN: A session prefix only prevents first-writer collisions. The archive gate also needs a
/// backend-authenticated identity for the exact object this process published so an overwrite by
/// another credentialed writer cannot satisfy the gate.
#[derive(Debug)]
struct PublishedObjectIdentities<Identity> {
    inner: Arc<RwLock<HashMap<(BlockNumber, BlockHash), Identity>>>,
}

impl<Identity> Default for PublishedObjectIdentities<Identity> {
    fn default() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl<Identity> Clone for PublishedObjectIdentities<Identity> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<Identity> PublishedObjectIdentities<Identity>
where
    Identity: Clone,
{
    fn record(
        &self,
        block_number: BlockNumber,
        block_hash: BlockHash,
        identity: Identity,
    ) -> anyhow::Result<()> {
        let mut identities = self
            .inner
            .write()
            .expect("published replay archive identity lock is poisoned");
        match identities.entry((block_number, block_hash)) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(identity);
                Ok(())
            }
            std::collections::hash_map::Entry::Occupied(_) => anyhow::bail!(
                "replay archive object identity was already recorded for block #{block_number}, {block_hash}"
            ),
        }
    }

    fn get(&self, block_number: BlockNumber, block_hash: BlockHash) -> Option<Identity> {
        self.inner
            .read()
            .expect("published replay archive identity lock is poisoned")
            .get(&(block_number, block_hash))
            .cloned()
    }

    fn remove(&self, block_number: BlockNumber, block_hash: BlockHash) -> anyhow::Result<()> {
        // SYSCOIN: The gate consumes each block once; discard its receipt after verification so a
        // long-lived sequencer retains only the uncommitted backlog.
        let removed = self
            .inner
            .write()
            .expect("published replay archive identity lock is poisoned")
            .remove(&(block_number, block_hash));
        anyhow::ensure!(
            removed.is_some(),
            "locally published replay archive identity disappeared for block #{block_number}, {block_hash}"
        );
        Ok(())
    }
}

fn ensure_published_identity_matches<Identity>(
    location: &str,
    expected: &Identity,
    stored: Option<&Identity>,
) -> anyhow::Result<()>
where
    Identity: PartialEq + ?Sized,
{
    anyhow::ensure!(
        stored == Some(expected),
        "replay archive object no longer matches the locally published object at {location}"
    );
    Ok(())
}

/// Replay archive layout:
///
/// ```text
/// <timestamp_millis>-<random_nonce>-<node_id>/<block_number>/<block_hash>
/// ```
///
/// SYSCOIN: Each node process owns an append-only session. This prevents another archive writer
/// from making this node's archive gate succeed with bytes the node did not write.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReplayArchiveSession {
    timestamp_millis: u64,
    nonce: String,
    node_id: String,
}

impl ReplayArchiveSession {
    pub fn new(
        timestamp_millis: u64,
        node_id: impl Into<String>,
    ) -> Result<Self, InvalidReplayArchiveSession> {
        let nonce = alloy::hex::encode(rand::random::<[u8; 16]>());
        Self::from_parts(timestamp_millis, nonce, node_id)
    }

    fn from_parts(
        timestamp_millis: u64,
        nonce: impl Into<String>,
        node_id: impl Into<String>,
    ) -> Result<Self, InvalidReplayArchiveSession> {
        let nonce = nonce.into();
        let node_id = node_id.into();
        validate_nonce(&nonce)?;
        validate_node_id(&node_id)?;
        Ok(Self {
            timestamp_millis,
            nonce,
            node_id,
        })
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn timestamp_millis(&self) -> u64 {
        self.timestamp_millis
    }

    pub fn folder_name(&self) -> String {
        format!("{}-{}-{}", self.timestamp_millis, self.nonce, self.node_id)
    }
}

impl fmt::Display for ReplayArchiveSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.folder_name())
    }
}

impl FromStr for ReplayArchiveSession {
    type Err = InvalidReplayArchiveSession;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut parts = value.splitn(3, '-');
        let timestamp_millis = parts
            .next()
            .ok_or(InvalidReplayArchiveSession::MissingTimestamp)?;
        let nonce = parts
            .next()
            .ok_or(InvalidReplayArchiveSession::MissingNonce)?;
        let node_id = parts
            .next()
            .ok_or(InvalidReplayArchiveSession::MissingNodeId)?;
        let timestamp_millis = timestamp_millis
            .parse()
            .map_err(|_| InvalidReplayArchiveSession::InvalidTimestamp)?;
        let session = Self::from_parts(timestamp_millis, nonce, node_id)?;
        // SYSCOIN: Recovery must never normalize a listed path into a different fetch key.
        if session.folder_name() != value {
            return Err(InvalidReplayArchiveSession::NonCanonical);
        }
        Ok(session)
    }
}

/// Full storage key for a single replay record object.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ReplayArchiveKey {
    pub session: ReplayArchiveSession,
    pub block_number: BlockNumber,
    pub block_hash: BlockHash,
}

impl ReplayArchiveKey {
    pub fn new(
        session: ReplayArchiveSession,
        block_number: BlockNumber,
        block_hash: BlockHash,
    ) -> Self {
        Self {
            session,
            block_number,
            block_hash,
        }
    }

    pub fn object_path(&self) -> String {
        format!(
            "{}/{}/{}",
            self.session,
            self.block_number,
            format_block_hash(self.block_hash)
        )
    }
}

impl fmt::Display for ReplayArchiveKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.object_path())
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InvalidReplayArchiveSession {
    #[error("replay archive node id cannot be empty")]
    EmptyNodeId,
    #[error("replay archive node id cannot contain path separators")]
    NodeIdContainsPathSeparator,
    #[error("replay archive session name must start with <timestamp_millis>-")]
    MissingTimestamp,
    #[error("replay archive session name must contain a random nonce")]
    MissingNonce,
    #[error("replay archive session name must contain a node id")]
    MissingNodeId,
    #[error("replay archive session timestamp must be an unsigned integer")]
    InvalidTimestamp,
    #[error("replay archive session nonce must be 32 lowercase hexadecimal characters")]
    InvalidNonce,
    #[error("replay archive session name is not canonically encoded")]
    NonCanonical,
}

fn validate_nonce(nonce: &str) -> Result<(), InvalidReplayArchiveSession> {
    if nonce.len() != 32
        || !nonce
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(InvalidReplayArchiveSession::InvalidNonce);
    }
    Ok(())
}

fn validate_node_id(node_id: &str) -> Result<(), InvalidReplayArchiveSession> {
    if node_id.is_empty() {
        return Err(InvalidReplayArchiveSession::EmptyNodeId);
    }
    if node_id.contains('/') || node_id.contains('\\') {
        return Err(InvalidReplayArchiveSession::NodeIdContainsPathSeparator);
    }
    Ok(())
}

fn format_block_hash(block_hash: BlockHash) -> String {
    alloy::hex::encode_prefixed(block_hash.0)
}

// SYSCOIN: Listed paths must round-trip exactly. Normalizing a listed component could make a
// reader fetch a different object than the one whose name passed validation.
pub(crate) fn parse_canonical_block_number(value: &str) -> Option<BlockNumber> {
    let block_number = value.parse::<BlockNumber>().ok()?;
    (block_number.to_string() == value).then_some(block_number)
}

pub(crate) fn parse_canonical_block_hash(value: &str) -> Option<BlockHash> {
    let block_hash = value.parse::<BlockHash>().ok()?;
    (format_block_hash(block_hash) == value).then_some(block_hash)
}

/// Parses a session-scoped object-store key back into a [`ReplayArchiveKey`].
///
/// Keys that do not match `<session>/<block_number>/<block_hash>` are logged and skipped instead of
/// failing the listing because a shared bucket may hold foreign objects.
pub(crate) fn parse_archive_object_key(object_key: &str) -> Option<ReplayArchiveKey> {
    let parts = object_key.split('/').collect::<Vec<_>>();
    if matches!(parts.as_slice(), [session, ".session"] if session.parse::<ReplayArchiveSession>().is_ok())
    {
        return None;
    }
    let key = match parts.as_slice() {
        [session, block_number, block_hash] => match (
            session.parse::<ReplayArchiveSession>(),
            parse_canonical_block_number(block_number),
            parse_canonical_block_hash(block_hash),
        ) {
            (Ok(session), Some(block_number), Some(block_hash)) => {
                Some(ReplayArchiveKey::new(session, block_number, block_hash))
            }
            _ => None,
        },
        _ => None,
    };
    if key.is_none() {
        tracing::warn!(
            object_key,
            "Skipping object key that is not a replay archive record"
        );
    }
    key
}

/// Session-bound byte storage using the `<session>/<block_number>/<block_hash>` layout.
///
/// SYSCOIN: Implementations must create a fresh session and fail if either the session or an object
/// in the session already exists. Treating an existing object as success would let another writer
/// make the local archive gate accept unverified bytes.
#[async_trait]
pub trait ReplayArchiveStorage: Sized + Send + Sync + 'static {
    /// Backend-specific configuration needed to create the storage.
    type Config: Send;

    /// Initializes storage bound to a newly created session.
    async fn init(config: Self::Config, session: ReplayArchiveSession) -> anyhow::Result<Self>;

    /// Appends `object` at `<session>/<block_number>/<block_hash>` without overwriting.
    async fn append_object(
        &self,
        block_number: BlockNumber,
        block_hash: BlockHash,
        object: Vec<u8>,
    ) -> anyhow::Result<()>;

    /// Verifies the exact object successfully published by this process is still stored.
    ///
    /// Returns `false` until the local append succeeds. A successful verification consumes the
    /// process-local identity; the single archive gate must verify each block exactly once.
    async fn contains_object(
        &self,
        block_number: BlockNumber,
        block_hash: BlockHash,
    ) -> anyhow::Result<bool>;
}

/// Archive for replay records in one writer-owned session.
#[async_trait]
pub trait ReplayArchiver: Send + Sync + 'static {
    /// Appends `replay_record` to this writer's archive session.
    async fn append_replay_record(
        &self,
        block_hash: BlockHash,
        replay_record: ReplayRecord,
    ) -> anyhow::Result<()>;

    /// Verifies this writer's exact locally published object and consumes its local identity.
    async fn contains_replay_record(
        &self,
        block_number: BlockNumber,
        block_hash: BlockHash,
    ) -> anyhow::Result<bool>;
}

#[async_trait]
impl<T> ReplayArchiver for Arc<T>
where
    T: ReplayArchiver + ?Sized,
{
    async fn append_replay_record(
        &self,
        block_hash: BlockHash,
        replay_record: ReplayRecord,
    ) -> anyhow::Result<()> {
        self.as_ref()
            .append_replay_record(block_hash, replay_record)
            .await
    }

    async fn contains_replay_record(
        &self,
        block_number: BlockNumber,
        block_hash: BlockHash,
    ) -> anyhow::Result<bool> {
        self.as_ref()
            .contains_replay_record(block_number, block_hash)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay_record::encode_replay_record;
    use alloy::primitives::B256;
    use zksync_os_storage_api::ReplayRecord;

    #[test]
    fn archive_key_roundtrips_session_layout() {
        let session = ReplayArchiveSession::new(42, "node-a").unwrap();
        let session_name = session.folder_name();
        let key = ReplayArchiveKey::new(session, 7, B256::ZERO);

        assert_eq!(
            key.object_path(),
            format!(
                "{session_name}/7/0x0000000000000000000000000000000000000000000000000000000000000000"
            )
        );
        assert_eq!(parse_archive_object_key(&key.object_path()), Some(key));
    }

    #[test]
    fn sessions_are_unique_and_reject_noncanonical_names() {
        let first = ReplayArchiveSession::new(42, "node-a").unwrap();
        let second = ReplayArchiveSession::new(42, "node-a").unwrap();
        assert_ne!(first, second);

        let canonical = first.folder_name();
        assert_eq!(canonical.parse::<ReplayArchiveSession>().unwrap(), first);
        assert!(
            format!("0{canonical}")
                .parse::<ReplayArchiveSession>()
                .is_err()
        );

        let mut uppercase_nonce = canonical.clone();
        let nonce_start = uppercase_nonce.find('-').unwrap() + 1;
        uppercase_nonce.replace_range(nonce_start..nonce_start + 1, "A");
        assert!(uppercase_nonce.parse::<ReplayArchiveSession>().is_err());
    }

    #[test]
    fn archive_object_key_parser_skips_foreign_keys() {
        // A shared bucket may hold unrelated objects; they must be skipped, not abort listing.
        assert!(parse_archive_object_key("logs/2026/app.txt").is_none());
        assert!(
            parse_archive_object_key(
                "prefix/7/0x0000000000000000000000000000000000000000000000000000000000000001"
            )
            .is_none()
        );
        assert!(parse_archive_object_key("42-node-a/7/not-a-hash").is_none());
        assert!(
            parse_archive_object_key(&format!(
                "{}/.session",
                ReplayArchiveSession::new(42, "node-a")
                    .unwrap()
                    .folder_name()
            ))
            .is_none()
        );
        assert!(parse_archive_object_key("42-node-a/not-a-number/0x00").is_none());
        assert!(parse_archive_object_key("7/not-a-hash").is_none());
        assert!(parse_archive_object_key("single-segment").is_none());
    }

    #[test]
    fn archive_object_key_parser_rejects_noncanonical_components() {
        let session = ReplayArchiveSession::new(42, "node-a")
            .unwrap()
            .folder_name();
        let block_hash = B256::with_last_byte(0xab);
        let canonical_hash = format_block_hash(block_hash);
        let uppercase_hash = format!("0x{}", canonical_hash[2..].to_uppercase());
        assert_eq!("0007".parse::<BlockNumber>().unwrap(), 7);
        assert_eq!(uppercase_hash.parse::<BlockHash>().unwrap(), block_hash);

        assert!(parse_archive_object_key(&format!("{session}/0007/{canonical_hash}")).is_none());
        assert!(parse_archive_object_key(&format!("{session}/7/{uppercase_hash}")).is_none());
    }

    #[test]
    fn upload_token_distinguishes_ambiguous_retry_from_foreign_object() {
        ensure_upload_token_matches("archive/key", "ours", Some("ours")).unwrap();
        ensure_upload_token_matches("archive/key", "ours", Some("theirs")).unwrap_err();
        ensure_upload_token_matches("archive/key", "ours", None).unwrap_err();
    }

    #[test]
    fn gate_identity_rejects_replaced_object() {
        ensure_published_identity_matches("archive/key", &7, Some(&7)).unwrap();
        ensure_published_identity_matches("archive/key", &7, Some(&8)).unwrap_err();
        ensure_published_identity_matches("archive/key", &7, None).unwrap_err();
    }

    #[tokio::test]
    async fn filesystem_writers_keep_independent_session_copies() {
        let tempdir = tempfile::tempdir().unwrap();
        let first_session = ReplayArchiveSession::new(42, "node-a").unwrap();
        let second_session = ReplayArchiveSession::new(43, "node-b").unwrap();
        let first_storage = FileSystemReplayArchiveStorage::init(
            tempdir.path().to_path_buf(),
            first_session.clone(),
        )
        .await
        .unwrap();
        let second_storage = FileSystemReplayArchiveStorage::init(
            tempdir.path().to_path_buf(),
            second_session.clone(),
        )
        .await
        .unwrap();
        let block_hash = B256::with_last_byte(1);

        let first = first_storage.append_object(7, block_hash, b"first payload".to_vec());
        let second = second_storage.append_object(7, block_hash, b"second payload".to_vec());
        let (first, second) = tokio::join!(first, second);
        first.unwrap();
        second.unwrap();

        let reader = FileSystemReplayArchiveReader::new(tempdir.path().to_path_buf());
        let first = reader
            .fetch_object(&ReplayArchiveKey::new(first_session, 7, block_hash))
            .await
            .unwrap();
        let second = reader
            .fetch_object(&ReplayArchiveKey::new(second_session, 7, block_hash))
            .await
            .unwrap();
        assert_eq!(first, b"first payload");
        assert_eq!(second, b"second payload");
    }

    #[tokio::test]
    async fn filesystem_archive_rejects_existing_session_object() {
        let tempdir = tempfile::tempdir().unwrap();
        let session = ReplayArchiveSession::new(42, "node-a").unwrap();
        let storage =
            FileSystemReplayArchiveStorage::init(tempdir.path().to_path_buf(), session.clone())
                .await
                .unwrap();
        let archive = FileSystemReplayArchiver::new(storage);
        let block_hash = B256::with_last_byte(1);
        let replay_record = test_replay_record(7);

        archive
            .append_replay_record(block_hash, replay_record.clone())
            .await
            .unwrap();

        let mut other_record = replay_record.clone();
        other_record.block_output_hash = B256::with_last_byte(2);
        archive
            .append_replay_record(block_hash, other_record)
            .await
            .unwrap_err();

        let reader = FileSystemReplayArchiveReader::new(tempdir.path().to_path_buf());
        let stored = reader
            .fetch_object(&ReplayArchiveKey::new(session, 7, block_hash))
            .await
            .unwrap();
        let stored: ReplayRecord = serde_json::from_slice(&stored).unwrap();
        assert_eq!(stored, replay_record);
    }

    #[test]
    fn age_encrypted_archive_encrypts_replay_record_for_recipient() {
        let identity = age::x25519::Identity::generate();
        let recipient = identity.to_public();
        let archive = AgeEncryptedReplayArchiver::new((), ArchiveRecipient::X25519(recipient));
        let replay_record = test_replay_record(7);

        let encrypted = archive.encrypt_replay_record(&replay_record).unwrap();
        let encoded = encode_replay_record(&replay_record);

        assert_ne!(encrypted, encoded);
        let decrypted = age::decrypt(&identity, encrypted.as_slice()).unwrap();
        assert_eq!(decrypted, encoded);
    }

    fn test_replay_record(block_number: u64) -> ReplayRecord {
        ReplayRecord {
            block_context: zksync_os_storage_api::BlockContext {
                block_number,
                ..Default::default()
            },
            transactions: vec![],
            previous_block_timestamp: 0,
            node_version: "0.0.0".parse().unwrap(),
            protocol_version: "0.29.1".parse().unwrap(),
            block_output_hash: B256::ZERO,
            force_preimages: vec![],
            canonical_upgrade_tx_hash: B256::ZERO,
            starting_cursors: Default::default(),
        }
    }
}
