use crate::{
    PublishedObjectIdentities, ReplayArchiveKey, ReplayArchiveKeyPage, ReplayArchiveSession,
    ReplayArchiveStorage, ReplayArchiveStorageReader, UPLOAD_TOKEN_METADATA_KEY,
    ensure_published_identity_matches,
};
use alloy::primitives::{BlockHash, BlockNumber};
use anyhow::Context as _;
use async_trait::async_trait;
use aws_config::{BehaviorVersion, ConfigLoader, Region, meta::region::RegionProviderChain};
use aws_runtime::env_config::file::{EnvConfigFileKind, EnvConfigFiles};
use aws_sdk_s3::{
    Client,
    error::{ProvideErrorMetadata as _, SdkError},
    operation::put_object::PutObjectError,
    primitives::ByteStream,
    types::ChecksumMode,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest as _, Sha256};
use std::path::PathBuf;

/// Object metadata key recording which node archived the object; forensic only.
const ARCHIVED_BY_METADATA_KEY: &str = "archived-by";
const SESSION_MARKER_FILE_NAME: &str = ".session";

/// Authentication mode for S3 replay archive access.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum S3ReplayArchiveAuthMode {
    /// Authentication via a credentials file at the specified path.
    AuthenticatedWithCredentialFile(PathBuf),
    /// Anonymous access. This is only useful for read-only recovery from public buckets.
    Anonymous,
}

/// S3 replay archive configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3ReplayArchiveConfig {
    /// Name or URL of the bucket.
    pub bucket_base_url: String,
    pub auth_mode: S3ReplayArchiveAuthMode,
    /// Allows overriding AWS S3 API endpoint, e.g. to use another S3-compatible store provider.
    pub endpoint: Option<String>,
    /// Allows specifying bucket region. If omitted, the SDK provider chain is used, falling back to `auto`.
    pub region: Option<String>,
}

impl S3ReplayArchiveConfig {
    pub fn with_credential_file(
        bucket_base_url: impl Into<String>,
        s3_credential_file_path: PathBuf,
    ) -> Self {
        Self {
            bucket_base_url: bucket_base_url.into(),
            auth_mode: S3ReplayArchiveAuthMode::AuthenticatedWithCredentialFile(
                s3_credential_file_path,
            ),
            endpoint: None,
            region: None,
        }
    }

    pub fn anonymous(bucket_base_url: impl Into<String>) -> Self {
        Self {
            bucket_base_url: bucket_base_url.into(),
            auth_mode: S3ReplayArchiveAuthMode::Anonymous,
            endpoint: None,
            region: None,
        }
    }
}

/// S3 implementation of [`ReplayArchiveStorage`].
#[derive(Debug, Clone)]
pub struct S3ReplayArchiveStorage {
    config: S3ReplayArchiveConfig,
    session: ReplayArchiveSession,
    client: Client,
    published_checksums: PublishedObjectIdentities<String>,
}

impl S3ReplayArchiveStorage {
    pub fn config(&self) -> &S3ReplayArchiveConfig {
        &self.config
    }

    pub fn session(&self) -> &ReplayArchiveSession {
        &self.session
    }

    fn object_key(&self, block_number: BlockNumber, block_hash: BlockHash) -> String {
        ReplayArchiveKey::new(self.session.clone(), block_number, block_hash).object_path()
    }

    fn session_marker_key(&self) -> String {
        format!("{}/{}", self.session, SESSION_MARKER_FILE_NAME)
    }

    async fn put_new_object(&self, key: &str, object: Vec<u8>) -> anyhow::Result<String> {
        let upload_token = crate::new_upload_token();
        let checksum = STANDARD.encode(Sha256::digest(&object));
        let result = self
            .client
            .put_object()
            .bucket(&self.config.bucket_base_url)
            .key(key)
            .metadata(ARCHIVED_BY_METADATA_KEY, self.session.node_id())
            .metadata(UPLOAD_TOKEN_METADATA_KEY, &upload_token)
            .checksum_sha256(&checksum)
            .if_none_match("*")
            .body(ByteStream::from(object))
            .send()
            .await;
        match result {
            Ok(output) => {
                ensure_published_identity_matches(
                    &format!("s3://{}/{}", self.config.bucket_base_url, key),
                    checksum.as_str(),
                    output.checksum_sha256(),
                )?;
                Ok(checksum)
            }
            // SYSCOIN: a conditional PUT may have succeeded before its response was lost. Accept
            // the retry conflict only when the stored unguessable token belongs to this request;
            // a first-writer object from any other request still fails closed.
            Err(err) if is_precondition_failed(&err) => {
                self.verify_upload(key, &upload_token, &checksum).await?;
                Ok(checksum)
            }
            Err(err) => Err(err).with_context(|| {
                format!(
                    "failed to create append-only replay archive S3 object s3://{}/{}",
                    self.config.bucket_base_url, key
                )
            }),
        }
    }

    async fn verify_upload(
        &self,
        key: &str,
        expected_token: &str,
        expected_checksum: &str,
    ) -> anyhow::Result<()> {
        let object = self
            .client
            .head_object()
            .bucket(&self.config.bucket_base_url)
            .key(key)
            .checksum_mode(ChecksumMode::Enabled)
            .send()
            .await
            .with_context(|| {
                format!(
                    "failed to verify replay archive S3 object after a conditional conflict at s3://{}/{}",
                    self.config.bucket_base_url, key
                )
            })?;
        let stored_token = object
            .metadata()
            .and_then(|metadata| metadata.get(UPLOAD_TOKEN_METADATA_KEY))
            .map(String::as_str);
        let location = format!("s3://{}/{}", self.config.bucket_base_url, key);
        crate::ensure_upload_token_matches(&location, expected_token, stored_token)?;
        ensure_published_identity_matches(&location, expected_checksum, object.checksum_sha256())
    }
}

fn is_precondition_failed(err: &SdkError<PutObjectError>) -> bool {
    err.as_service_error()
        .and_then(|err| err.code())
        .is_some_and(|code| code == "PreconditionFailed")
        || err
            .raw_response()
            .is_some_and(|response| response.status().as_u16() == 412)
}

#[async_trait]
impl ReplayArchiveStorage for S3ReplayArchiveStorage {
    type Config = S3ReplayArchiveConfig;

    async fn init(config: Self::Config, session: ReplayArchiveSession) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !config.bucket_base_url.is_empty(),
            "replay archive S3 bucket_base_url cannot be empty"
        );
        let client = create_s3_client(&config).await;
        let storage = Self {
            config,
            session,
            client,
            published_checksums: PublishedObjectIdentities::default(),
        };
        storage
            .put_new_object(&storage.session_marker_key(), Vec::new())
            .await
            .with_context(|| {
                format!(
                    "failed to create append-only replay archive S3 session {}",
                    storage.session
                )
            })?;
        Ok(storage)
    }

    async fn append_object(
        &self,
        block_number: BlockNumber,
        block_hash: BlockHash,
        object: Vec<u8>,
    ) -> anyhow::Result<()> {
        let checksum = self
            .put_new_object(&self.object_key(block_number, block_hash), object)
            .await?;
        self.published_checksums
            .record(block_number, block_hash, checksum)
    }

    async fn contains_object(
        &self,
        block_number: BlockNumber,
        block_hash: BlockHash,
    ) -> anyhow::Result<bool> {
        let Some(expected_checksum) = self.published_checksums.get(block_number, block_hash) else {
            return Ok(false);
        };
        let key = self.object_key(block_number, block_hash);
        match self
            .client
            .head_object()
            .bucket(&self.config.bucket_base_url)
            .key(&key)
            .checksum_mode(ChecksumMode::Enabled)
            .send()
            .await
        {
            Ok(object) => {
                // SYSCOIN: S3 validates this SHA-256 during PUT, so checking it here binds the
                // commit gate to the exact bytes this process successfully published.
                ensure_published_identity_matches(
                    &format!("s3://{}/{}", self.config.bucket_base_url, key),
                    expected_checksum.as_str(),
                    object.checksum_sha256(),
                )?;
                self.published_checksums.remove(block_number, block_hash)?;
                Ok(true)
            }
            Err(err) if matches!(err.as_service_error(), Some(err) if err.is_not_found()) => {
                anyhow::bail!(
                    "locally published replay archive object disappeared from s3://{}/{}",
                    self.config.bucket_base_url,
                    key
                )
            }
            Err(err) => Err(err).with_context(|| {
                format!(
                    "failed to check replay archive S3 object s3://{}/{}",
                    self.config.bucket_base_url, key
                )
            }),
        }
    }
}

/// S3 implementation of [`ReplayArchiveStorageReader`].
#[derive(Debug, Clone)]
pub struct S3ReplayArchiveReader {
    config: S3ReplayArchiveConfig,
    client: Client,
}

impl S3ReplayArchiveReader {
    pub async fn new(config: S3ReplayArchiveConfig) -> Self {
        let client = create_s3_client(&config).await;
        Self { config, client }
    }

    pub fn config(&self) -> &S3ReplayArchiveConfig {
        &self.config
    }
}

#[async_trait]
impl ReplayArchiveStorageReader for S3ReplayArchiveReader {
    async fn list_keys_page(
        &self,
        page_token: Option<String>,
    ) -> anyhow::Result<ReplayArchiveKeyPage> {
        let mut request = self
            .client
            .list_objects_v2()
            .bucket(&self.config.bucket_base_url);
        if let Some(token) = page_token {
            request = request.continuation_token(token);
        }
        let response = request.send().await.with_context(|| {
            format!(
                "failed to list replay archive S3 objects in s3://{}",
                self.config.bucket_base_url
            )
        })?;

        let mut keys = Vec::new();
        for object in response.contents() {
            let Some(object_key) = object.key() else {
                continue;
            };
            if let Some(key) = crate::parse_archive_object_key(object_key) {
                keys.push(key);
            }
        }
        Ok(ReplayArchiveKeyPage {
            keys,
            next_page_token: response.next_continuation_token().map(str::to_owned),
        })
    }

    async fn fetch_object(&self, key: &ReplayArchiveKey) -> anyhow::Result<Vec<u8>> {
        let object_key = key.object_path();
        let bytes = self
            .client
            .get_object()
            .bucket(&self.config.bucket_base_url)
            .key(&object_key)
            .send()
            .await
            .with_context(|| {
                format!(
                    "failed to read replay archive S3 object s3://{}/{}",
                    self.config.bucket_base_url, object_key
                )
            })?
            .body
            .collect()
            .await
            .with_context(|| {
                format!(
                    "failed to collect replay archive S3 object s3://{}/{}",
                    self.config.bucket_base_url, object_key
                )
            })?
            .into_bytes()
            .to_vec();
        Ok(bytes)
    }
}

async fn create_s3_client(config: &S3ReplayArchiveConfig) -> Client {
    let region_provider = RegionProviderChain::first_try(config.region.clone().map(Region::new))
        .or_default_provider()
        .or_else(Region::new("auto"));
    let mut sdk_config = get_client_config(config.auth_mode.clone()).region(region_provider);
    if let Some(endpoint) = config.endpoint.clone() {
        tracing::info!(%endpoint, "using S3 endpoint defined in replay archive config");
        sdk_config = sdk_config.endpoint_url(endpoint);
    }
    let sdk_config = sdk_config.load().await;
    Client::new(&sdk_config)
}

fn get_client_config(auth_mode: S3ReplayArchiveAuthMode) -> ConfigLoader {
    match auth_mode {
        S3ReplayArchiveAuthMode::AuthenticatedWithCredentialFile(path) => {
            let profile_files = EnvConfigFiles::builder()
                .with_file(EnvConfigFileKind::Credentials, path)
                .build();
            aws_config::defaults(BehaviorVersion::latest()).profile_files(profile_files)
        }
        S3ReplayArchiveAuthMode::Anonymous => {
            aws_config::defaults(BehaviorVersion::latest()).no_credentials()
        }
    }
}
