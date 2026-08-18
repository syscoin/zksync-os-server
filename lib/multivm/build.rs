use cargo_metadata::{MetadataCommand, PackageId};
use reqwest::StatusCode;
use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
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
        "v0.3.2-interface-v0.1.3" => Some(BinarySourceConfig {
            proving_version: "V7",
            download_tag: "v0.3.2-interface-v0.1.3",
        }),
        _ => None,
    }
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
    // SYSCOIN: keep fork-specific VM app release assets pinned to exact bytes.
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
    let client = new_http_client().expect("failed to create HTTP client");

    // Find forward_system crate and expose its path to the directory containing `app*.bin` files.
    for package in &metadata.packages {
        if package.name.as_str() != "forward_system" {
            continue;
        }
        let Ok(reference) = parse_git_reference(&package.id) else {
            continue;
        };

        if let Some(config) = binary_source_config(&reference) {
            let dir = format!("{manifest_dir}/apps/{}", config.download_tag);
            std::fs::create_dir_all(&dir).expect("failed to create directory");
            for variant in APP_VARIANTS {
                // SYSCOIN: app binaries are published as hash-pinned release assets;
                // verify exact bytes before embedding them with include_bytes!.
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
}
