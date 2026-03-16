use crate::dyn_wallet_provider::EthDynProvider;
use crate::AnvilL1;
use alloy::providers::{Provider, ProviderBuilder, WalletProvider};
use anyhow::Context;
use backon::{ConstantBuilder, Retryable};
use reqwest::{
    Client, StatusCode,
    header::{AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT},
};
use semver::Version;
use serde::Deserialize;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use zksync_os_server::default_protocol_version::PROTOCOL_VERSION;

use super::{CURRENT_BIN_ENV, RELEASE_DOWNLOAD_DIR, REPO};

#[derive(Debug, Clone, Copy)]
pub(crate) enum ReleaseSelector {
    PreviousPatch,
    PreviousMinor,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
}

pub(crate) struct ReleaseBundle {
    pub(crate) binary_path: PathBuf,
    pub(crate) fixture: ReleaseFixture,
}

pub(crate) struct ReleaseFixture {
    root: PathBuf,
    protocol_version: String,
}

impl ReleaseFixture {
    pub(crate) fn default_config_path(&self) -> PathBuf {
        self.root
            .join(&self.protocol_version)
            .join("default")
            .join("config.yaml")
    }

    pub(crate) fn l1_state_bytes(&self) -> anyhow::Result<Vec<u8>> {
        let path = self.root.join(&self.protocol_version).join("l1-state.json");
        if path.exists() {
            return std::fs::read(&path)
                .with_context(|| format!("failed to read release L1 state {}", path.display()));
        }

        let gz_path = self.root.join(&self.protocol_version).join("l1-state.json.gz");
        let file = File::open(&gz_path)
            .with_context(|| format!("failed to open release L1 state {}", gz_path.display()))?;
        let mut decoder = flate2::read::GzDecoder::new(file);
        let mut bytes = Vec::new();
        decoder
            .read_to_end(&mut bytes)
            .with_context(|| format!("failed to decompress {}", gz_path.display()))?;
        Ok(bytes)
    }
}

pub(crate) async fn resolve_current_server_binary() -> anyhow::Result<PathBuf> {
    if let Ok(path) = std::env::var(CURRENT_BIN_ENV) {
        return Ok(PathBuf::from(path));
    }

    let workspace = workspace_dir();
    let debug_bin = workspace.join("target/debug/zksync-os-server");
    if debug_bin.exists() {
        return Ok(debug_bin);
    }
    let release_bin = workspace.join("target/release/zksync-os-server");
    if release_bin.exists() {
        return Ok(release_bin);
    }

    let status = Command::new("cargo")
        .arg("build")
        .arg("--bin")
        .arg("zksync-os-server")
        .current_dir(workspace)
        .status()
        .await
        .context("failed to invoke cargo build for zksync-os-server")?;
    anyhow::ensure!(status.success(), "cargo build --bin zksync-os-server failed");
    Ok(debug_bin)
}

pub(crate) async fn download_server_release(
    selector: ReleaseSelector,
) -> anyhow::Result<Option<ReleaseBundle>> {
    let current_version = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let releases = fetch_releases().await?;
    let selected = match selector {
        ReleaseSelector::PreviousPatch => releases
            .into_iter()
            .filter_map(parse_release_version)
            .filter(|version| {
                version.major == current_version.major
                    && version.minor == current_version.minor
                    && version.patch < current_version.patch
            })
            .max(),
        ReleaseSelector::PreviousMinor => releases
            .into_iter()
            .filter_map(parse_release_version)
            .filter(|version| {
                version.major == current_version.major
                    && version.minor + 1 == current_version.minor
            })
            .max(),
    };

    let Some(version) = selected else {
        return Ok(None);
    };
    let tag = format!("v{version}");
    download_and_unpack_server_release(&tag).await.map(Some)
}

pub(crate) fn workspace_dir() -> PathBuf {
    std::env::var("WORKSPACE_DIR")
        .expect("WORKSPACE_DIR environment variable is not set")
        .into()
}

pub(crate) fn config_workspace_dir(config_path: &Path) -> anyhow::Result<PathBuf> {
    for ancestor in config_path.ancestors() {
        if ancestor.file_name().and_then(|name| name.to_str()) == Some("local-chains") {
            return ancestor
                .parent()
                .map(Path::to_path_buf)
                .context("local-chains directory has no parent");
        }
    }
    anyhow::bail!(
        "failed to derive workspace directory from config path {}",
        config_path.display()
    )
}

pub(crate) async fn start_anvil_from_state(l1_state: Vec<u8>) -> anyhow::Result<AnvilL1> {
    let tempdir = tempfile::tempdir()?;
    let l1_state_path = tempdir.path().join("l1-state.json");
    std::fs::write(&l1_state_path, &l1_state)
        .context("failed to write release L1 state to temporary state file")?;

    let locked_port = crate::utils::LockedPort::acquire_unused().await?;
    let address = format!("http://localhost:{}", locked_port.port);

    let provider = ProviderBuilder::new().connect_anvil_with_wallet_and_config(|anvil| {
        anvil
            .port(locked_port.port)
            .chain_id(31337)
            .arg("--load-state")
            .arg(l1_state_path)
    })?;

    let wallet = provider.wallet().clone();
    let addr = address.clone();
    (|| async {
        provider.clone().get_chain_id().await?;
        anyhow::Ok(())
    })
    .retry(
        ConstantBuilder::default()
            .with_delay(Duration::from_millis(200))
            .with_max_times(50),
    )
    .notify(|err: &anyhow::Error, dur: Duration| {
        tracing::info!(%err, ?dur, addr, "retrying connection to historical L1 node");
    })
    .await?;

    Ok(AnvilL1 {
        address,
        provider: EthDynProvider::new(provider),
        wallet,
        _tempdir: Arc::new(tempdir),
    })
}

fn parse_release_version(release: GithubRelease) -> Option<Version> {
    if release.draft || release.prerelease {
        return None;
    }
    release.tag_name.strip_prefix('v')?.parse().ok()
}

async fn fetch_releases() -> anyhow::Result<Vec<GithubRelease>> {
    let client = github_client()?;
    let response = client
        .get(format!("https://api.github.com/repos/{REPO}/releases?per_page=100"))
        .send()
        .await?
        .error_for_status()?;
    Ok(response.json().await?)
}

async fn download_and_unpack_server_release(tag: &str) -> anyhow::Result<ReleaseBundle> {
    let asset_name = release_asset_name(tag);
    let cache_dir = workspace_dir().join(RELEASE_DOWNLOAD_DIR);
    std::fs::create_dir_all(&cache_dir)?;
    let binary_path = cache_dir.join(asset_name.trim_end_matches(".tar.gz"));
    let fixture = download_and_unpack_release_fixtures(tag, &cache_dir).await?;
    let archive_path = cache_dir.join(&asset_name);
    let lock_path = cache_dir.join(format!("{asset_name}.lock"));
    let lock_file = File::create(&lock_path)?;
    fs2::FileExt::lock_exclusive(&lock_file)?;

    if !archive_path.exists() {
        let client = github_client()?;
        let url = format!("https://github.com/{REPO}/releases/download/{tag}/{asset_name}");
        let response = download_with_retry(&client, &url).await?;
        let body = response.bytes().await?;
        std::fs::write(&archive_path, &body)?;
    }

    if !binary_path.exists() {
        let extract_dir =
            cache_dir.join(format!("{}-extract", asset_name.trim_end_matches(".tar.gz")));
        if extract_dir.exists() {
            std::fs::remove_dir_all(&extract_dir)?;
        }
        std::fs::create_dir_all(&extract_dir)?;
        let archive_path_clone = archive_path.clone();
        let extract_dir_clone = extract_dir.clone();
        tokio::task::spawn_blocking(move || {
            let file = std::fs::File::open(&archive_path_clone)
                .expect("release archive exists and is readable");
            tar::Archive::new(flate2::read::GzDecoder::new(file))
                .unpack(&extract_dir_clone)
                .unwrap();
        })
        .await
        .expect("server release extraction task panicked");

        let extracted = find_server_binary(&extract_dir).with_context(|| {
            format!(
                "failed to locate server binary after unpacking {}",
                archive_path.display()
            )
        })?;
        std::fs::copy(&extracted, &binary_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut perms = std::fs::metadata(&binary_path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&binary_path, perms)?;
        }
    }

    drop(lock_file);
    Ok(ReleaseBundle { binary_path, fixture })
}

async fn download_and_unpack_release_fixtures(
    tag: &str,
    cache_dir: &Path,
) -> anyhow::Result<ReleaseFixture> {
    let archive_path = cache_dir.join(format!("local-chains-{tag}.tar.gz"));
    let extract_dir = cache_dir.join(format!("local-chains-{tag}"));
    let lock_path = cache_dir.join(format!("local-chains-{tag}.lock"));
    let lock_file = File::create(&lock_path)?;
    fs2::FileExt::lock_exclusive(&lock_file)?;

    if !archive_path.exists() {
        let client = github_client()?;
        let url = format!("https://github.com/{REPO}/releases/download/{tag}/local-chains.tar.gz");
        let response = download_with_retry(&client, &url).await?;
        let body = response.bytes().await?;
        std::fs::write(&archive_path, &body)?;
    }

    if !extract_dir.exists() {
        std::fs::create_dir_all(&extract_dir)?;
        let archive_path_clone = archive_path.clone();
        let extract_dir_clone = extract_dir.clone();
        tokio::task::spawn_blocking(move || {
            let file = std::fs::File::open(&archive_path_clone)
                .expect("release local-chains archive exists and is readable");
            tar::Archive::new(flate2::read::GzDecoder::new(file))
                .unpack(&extract_dir_clone)
                .unwrap();
        })
        .await
        .expect("local-chains extraction task panicked");
    }

    drop(lock_file);
    let protocol_version = detect_protocol_version(&extract_dir)?;
    Ok(ReleaseFixture {
        root: extract_dir.join("local-chains"),
        protocol_version,
    })
}

fn detect_protocol_version(extract_dir: &Path) -> anyhow::Result<String> {
    let local_chains_dir = extract_dir.join("local-chains");
    if local_chains_dir.join(PROTOCOL_VERSION).exists() {
        return Ok(PROTOCOL_VERSION.to_owned());
    }
    let mut candidates = std::fs::read_dir(&local_chains_dir)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with('v'))
        .collect::<Vec<_>>();
    candidates.sort();
    candidates
        .pop()
        .context("failed to detect protocol version in downloaded local-chains")
}

fn release_asset_name(tag: &str) -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    match (os, arch) {
        ("linux", "x86_64") => format!("zksync-os-server-{tag}-x86_64-unknown-linux-gnu.tar.gz"),
        ("linux", "aarch64") => {
            format!("zksync-os-server-{tag}-aarch64-unknown-linux-gnu.tar.gz")
        }
        ("macos", _) => format!("zksync-os-server-{tag}-universal-apple-darwin.tar.gz"),
        _ => panic!("unsupported platform for server release download: {os}-{arch}"),
    }
}

fn find_server_binary(dir: &Path) -> anyhow::Result<PathBuf> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            if let Ok(found) = find_server_binary(&path) {
                return Ok(found);
            }
            continue;
        }

        if path.file_name().and_then(|name| name.to_str()) == Some("zksync-os-server") {
            return Ok(path);
        }
    }
    anyhow::bail!("zksync-os-server binary not found in {}", dir.display())
}

fn github_client() -> anyhow::Result<Client> {
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static("zksync-os-version-restart-tests/1.0"),
    );

    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        let bearer = format!("Bearer {}", token.trim());
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&bearer)?);
    }

    Ok(Client::builder().default_headers(headers).build()?)
}

async fn download_with_retry(client: &Client, url: &str) -> anyhow::Result<reqwest::Response> {
    const MAX_ATTEMPTS: usize = 5;
    for attempt in 1..=MAX_ATTEMPTS {
        let response = client.get(url).send().await;
        match response {
            Ok(response) if response.status().is_success() => return Ok(response),
            Ok(response) if retryable_status(response.status()) && attempt < MAX_ATTEMPTS => {
                tokio::time::sleep(Duration::from_millis(500 * attempt as u64)).await;
            }
            Ok(response) => {
                return Err(anyhow::anyhow!(
                    "failed to download {url}: HTTP {}",
                    response.status()
                ));
            }
            Err(err) if attempt < MAX_ATTEMPTS => {
                tracing::warn!(%err, attempt, "retrying release download");
                tokio::time::sleep(Duration::from_millis(500 * attempt as u64)).await;
            }
            Err(err) => return Err(err.into()),
        }
    }
    unreachable!()
}

fn retryable_status(status: StatusCode) -> bool {
    status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS
}
