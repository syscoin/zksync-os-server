use cargo_metadata::{MetadataCommand, PackageId};
use reqwest::StatusCode;
use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::path::Path;
use url::Url;

struct BinarySourceConfig {
    proving_version: &'static str,
    download_tag: &'static str,
}

fn parse_git_reference(package_id: &PackageId) -> anyhow::Result<String> {
    let url = Url::parse(&package_id.to_string())?;
    let mut query_pairs = url.query_pairs();
    let (_, reference) = query_pairs
        .find(|(key, _)| key == "tag" || key == "branch" || key == "rev")
        .ok_or_else(|| anyhow::anyhow!("missing tag, branch or rev in git url `{url}`"))?;
    Ok(reference.to_string())
}

// Remove entries as the corresponding proving lanes leave the support window.
fn binary_source_config(reference: &str) -> Option<BinarySourceConfig> {
    match reference {
        // The V6 VK was generated from the original v0.2.5 binaries; 0.2.x rebuild tags
        // produce different ones.
        "v0.2.10-interface-v0.1.3-2026-02-10" => Some(BinarySourceConfig {
            proving_version: "V6",
            download_tag: "v0.2.5",
        }),
        "v0.3.2-interface-v0.1.3" => Some(BinarySourceConfig {
            proving_version: "V7",
            download_tag: "v0.3.2-interface-v0.1.3",
        }),
        _ => None,
    }
}

fn require_patched_source_text(
    root: &Path,
    relative_path: &str,
    needle: &str,
) -> anyhow::Result<()> {
    let path = root.join(relative_path);
    println!("cargo:rerun-if-changed={}", path.display());
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_file() {
        anyhow::bail!(
            "patched zksync-os source is not a regular file: {}",
            path.display()
        );
    }
    let text = std::fs::read_to_string(&path)?;
    if !text.contains(needle) {
        anyhow::bail!(
            "patched zksync-os sentinel is missing from {}",
            path.display()
        );
    }
    Ok(())
}

fn require_patched_source_sha256(
    root: &Path,
    relative_path: &str,
    expected: &str,
) -> anyhow::Result<()> {
    let path = root.join(relative_path);
    println!("cargo:rerun-if-changed={}", path.display());
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_file() {
        anyhow::bail!(
            "patched zksync-os source is not a regular file: {}",
            path.display()
        );
    }
    let actual = sha256_hex(&std::fs::read(&path)?);
    if actual != expected {
        anyhow::bail!(
            "patched zksync-os source SHA-256 mismatch for {}: expected {expected}, got {actual}",
            path.display()
        );
    }
    Ok(())
}

fn verify_syscoin_execution_source(package_manifest: &Path) -> anyhow::Result<()> {
    let package_dir = package_manifest.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "forward_system manifest has no parent directory: {}",
            package_manifest.display()
        )
    })?;
    let source_root = package_dir.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "forward_system package has no zksync-os source root: {}",
            package_dir.display()
        )
    })?;

    // A released V7 guest contains the Syscoin 0x101 implementation. Native execution for the
    // V7 proving lane must come from the same patched source or simulation and proving can
    // disagree. Cargo cannot apply an external patch during dependency resolution, so unsupported
    // plain builds fail closed and direct callers use the checked-in patched-workspace launcher.
    require_patched_source_text(
        source_root,
        "forward_system/Cargo.toml",
        "system_hooks/slh_dsa_precompile",
    )?;
    require_patched_source_text(
        source_root,
        "basic_system/src/system_functions/slh_dsa_sha2_128_24_verify.rs",
        "compress256(state, core::slice::from_ref(&block));",
    )?;
    require_patched_source_text(
        source_root,
        "evm_interpreter/src/precompile_addresses.rs",
        "SLH_DSA_SHA2_128_24_VERIFY_HOOK_ADDRESS_LOW",
    )?;
    require_patched_source_sha256(
        source_root,
        "basic_bootloader/src/bootloader/transaction_flow/zk/syscoin_edge_da.rs",
        "1eb8dc0da30570626a860968140c41663b9a40077f2c420665196b7506d7a7cb",
    )?;
    Ok(())
}

const DOWNLOAD_MAX_ATTEMPTS: usize = 5;
const DOWNLOAD_TIMEOUT_SECS: u64 = 60;
const DOWNLOAD_BASE_BACKOFF_MS: u64 = 500;
const APP_VARIANTS: [&str; 3] = [
    "multiblock_batch",
    "singleblock_batch",
    "singleblock_batch_logging_enabled",
];

fn expected_syscoin_app_sha256(tag: &str, variant: &str) -> anyhow::Result<&'static str> {
    // SYSCOIN: keep the patched V7 VM app release assets pinned to exact bytes.
    match (tag, variant) {
        ("v0.2.5", "multiblock_batch") => {
            Ok("f8612c0c43719549d233a16efb95984109ea7ce543b102ffaf572c9496cebf22")
        }
        ("v0.2.5", "singleblock_batch") => {
            Ok("c7f375b6086814033e1de5ada8a4b0cfb3a1a71f9cb25de824ced247178d23e0")
        }
        ("v0.2.5", "singleblock_batch_logging_enabled") => {
            Ok("055ed473eb0af6797c9dda7ef7551aa7bb8907761be9c8726046c1959eeb6e4d")
        }
        ("v0.3.2-interface-v0.1.3", "multiblock_batch") => {
            Ok("1487dd6070b75f43f433499f3ab2910e23dfacc24319bb09c1ed43375483e7b5")
        }
        ("v0.3.2-interface-v0.1.3", "singleblock_batch") => {
            Ok("097ca3c97ddf5c3985f2d97dfdc05354329ed137b219566847acda9417d02a87")
        }
        ("v0.3.2-interface-v0.1.3", "singleblock_batch_logging_enabled") => {
            Ok("4e7dbf72ae7edd7b1f6b555da787ad61f993c3757f0d8c654586b557a2c0417d")
        }
        _ => anyhow::bail!("missing expected SHA-256 for Syscoin zksync-os app {tag}/{variant}"),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn verify_syscoin_app_sha256(tag: &str, variant: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let expected = expected_syscoin_app_sha256(tag, variant)?;
    let actual = sha256_hex(bytes);
    if actual != expected {
        anyhow::bail!(
            "SHA-256 mismatch for Syscoin zksync-os app {tag}/{variant}: expected {expected}, got {actual}"
        );
    }
    Ok(())
}

fn verify_syscoin_app_file(tag: &str, variant: &str, path: &str) -> anyhow::Result<()> {
    let bytes = std::fs::read(path)?;
    verify_syscoin_app_sha256(tag, variant, &bytes)
}

fn is_retryable_status(status: StatusCode) -> bool {
    status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS
}

fn new_http_client() -> anyhow::Result<Client> {
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static("zksync-os-build-script/1.0"),
    );

    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        let bearer = format!("Bearer {}", token.trim());
        match HeaderValue::from_str(&bearer) {
            Ok(value) => {
                headers.insert(AUTHORIZATION, value);
            }
            Err(err) => {
                println!("cargo:warning=Ignoring invalid GITHUB_TOKEN format: {err}");
            }
        }
    }

    Ok(Client::builder()
        .default_headers(headers)
        .timeout(std::time::Duration::from_secs(DOWNLOAD_TIMEOUT_SECS))
        .build()?)
}

fn download_with_retry(
    client: &Client,
    url: &str,
    path: &str,
    tag: &str,
    variant: &str,
) -> anyhow::Result<()> {
    for attempt in 1..=DOWNLOAD_MAX_ATTEMPTS {
        let response = client.get(url).send();
        match response {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    let body = response.bytes()?;
                    verify_syscoin_app_sha256(tag, variant, body.as_ref())?;
                    std::fs::write(path, body.as_ref())?;
                    return Ok(());
                }

                if is_retryable_status(status) && attempt < DOWNLOAD_MAX_ATTEMPTS {
                    let delay_ms = DOWNLOAD_BASE_BACKOFF_MS * attempt as u64;
                    println!(
                        "cargo:warning=download attempt {attempt}/{DOWNLOAD_MAX_ATTEMPTS} failed with status {status} for {url}; retrying in {delay_ms}ms"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    continue;
                }

                anyhow::bail!("download failed with status {status} for {url}");
            }
            Err(err) => {
                if attempt < DOWNLOAD_MAX_ATTEMPTS {
                    let delay_ms = DOWNLOAD_BASE_BACKOFF_MS * attempt as u64;
                    println!(
                        "cargo:warning=download attempt {attempt}/{DOWNLOAD_MAX_ATTEMPTS} failed for {url}: {err}; retrying in {delay_ms}ms"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    continue;
                }

                anyhow::bail!("download request failed for {url}: {err}");
            }
        }
    }
    unreachable!("loop always returns on success or final attempt");
}

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let metadata = MetadataCommand::new().exec().unwrap();
    let mut configured_v7_sources = 0;

    // Find forward_system crate and expose its path to the directory containing `app*.bin` files.
    for package in &metadata.packages {
        if package.name.as_str() != "forward_system" {
            continue;
        }
        let Ok(reference) = parse_git_reference(&package.id) else {
            continue;
        };

        if let Some(config) = binary_source_config(&reference) {
            if config.proving_version == "V7" {
                configured_v7_sources += 1;
                verify_syscoin_execution_source(package.manifest_path.as_std_path())
                    .unwrap_or_else(|err| {
                        panic!(
                            "V7 zksync-os execution source is not Syscoin-patched: {err}. Use run_local.sh or scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh instead of plain Cargo for builds that include multivm"
                        )
                    });
            }
            let client = new_http_client().expect("failed to create HTTP client");
            let dir = format!("{manifest_dir}/apps/{}", config.download_tag);
            std::fs::create_dir_all(&dir).expect("failed to create directory");
            for variant in APP_VARIANTS {
                // SYSCOIN: app binaries are published as hash-pinned release assets;
                // this URL is artifact hosting, not an execution-source dependency.
                // Verify exact bytes before embedding them with include_bytes!.
                let url = format!(
                    "https://github.com/syscoin/zksync-os/releases/download/{}/{variant}.bin",
                    config.download_tag
                );
                let path = format!("{dir}/{variant}.bin");
                if std::fs::exists(&path).expect("failed to check file existence") {
                    if let Err(err) = verify_syscoin_app_file(config.download_tag, variant, &path) {
                        println!(
                            "cargo:warning=removing cached Syscoin zksync-os app with invalid SHA-256 at {path}: {err}"
                        );
                        std::fs::remove_file(&path).expect("failed to remove invalid app binary");
                    } else {
                        continue;
                    }
                }
                download_with_retry(&client, &url, &path, config.download_tag, variant)
                    .expect("failed to download");
            }

            println!(
                "cargo:rustc-env=ZKSYNC_OS_{}_SOURCE_PATH={dir}",
                config.proving_version
            );
            continue;
        }
    }

    assert_eq!(
        configured_v7_sources, 1,
        "expected exactly one V7 forward_system source, found {configured_v7_sources}"
    );
}
