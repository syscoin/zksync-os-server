use cargo_metadata::{MetadataCommand, PackageId};
use reqwest::StatusCode;
use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use url::Url;

fn parse_git_tag(package_id: &PackageId) -> anyhow::Result<String> {
    let url = Url::parse(&package_id.to_string())?;
    let mut query_pairs = url.query_pairs();
    let (_, tag) = query_pairs
        .find(|(key, _)| key == "tag")
        .ok_or_else(|| anyhow::anyhow!("missing tag in git url `{url}`"))?;
    Ok(tag.to_string())
}

fn proving_version_from_tag(tag: &str) -> Option<String> {
    match tag {
        "v0.2.9-interface-v0.0.15" => Some(String::from("V6")),
        "v0.3.0" => Some(String::from("V7")),
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
        ("v0.3.0", "multiblock_batch") => {
            Ok("1601bd3fd5d21835be2dbc53e1285a1642b78272db8221e072d77c03a6a28856")
        }
        ("v0.3.0", "singleblock_batch") => {
            Ok("42e496faed31216297ecba60c77476afb86594c9db47efe9ed7f85fa32fa9516")
        }
        ("v0.3.0", "singleblock_batch_logging_enabled") => {
            Ok("eb273d998817b69fe791026018a73a2fd94b17d1558627f58c7f34f2e85d7ebc")
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
        let tag = match parse_git_tag(&package.id) {
            Ok(tag) => tag,
            Err(err) => {
                println!("cargo::error=failed to parse forward_system's git tag: {err}");
                return;
            }
        };

        if let Some(proving_version) = proving_version_from_tag(&tag) {
            // TEMPORARY HACK for V6!!!
            // We've updated interface and rust toolchain for corresponding zksync-os version and it caused a change in binaries.
            // We need to use original V6 binaries from zksync-os v0.2.5.
            // Should be removed as soon as we can get rig of proving V6.
            let tag = if proving_version == "V6" {
                "v0.2.5".to_owned()
            } else {
                tag
            };

            let dir = format!("{manifest_dir}/apps/{tag}");
            std::fs::create_dir_all(&dir).expect("failed to create directory");
            for variant in APP_VARIANTS {
                // SYSCOIN: app binaries are hosted in the Syscoin zksync-os fork;
                // verify exact bytes before embedding them with include_bytes!.
                let url = format!(
                    "https://github.com/syscoin/zksync-os/releases/download/{tag}/{variant}.bin"
                );
                let path = format!("{dir}/{variant}.bin");
                if std::fs::exists(&path).expect("failed to check file existence") {
                    if let Err(err) = verify_syscoin_app_file(&tag, variant, &path) {
                        println!(
                            "cargo:warning=removing cached Syscoin zksync-os app with invalid SHA-256 at {path}: {err}"
                        );
                        std::fs::remove_file(&path).expect("failed to remove invalid app binary");
                    } else {
                        continue;
                    }
                }
                download_with_retry(&client, &url, &path, &tag, variant)
                    .expect("failed to download");
            }

            println!("cargo:rustc-env=ZKSYNC_OS_{proving_version}_SOURCE_PATH={dir}");
            continue;
        }
    }
}
