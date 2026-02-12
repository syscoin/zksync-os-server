use cargo_metadata::{MetadataCommand, PackageId};
use url::Url;

fn parse_git_tag(package_id: &PackageId) -> anyhow::Result<String> {
    let url = Url::parse(&package_id.to_string())?;
    let mut query_pairs = url.query_pairs();
    let (_, tag) = query_pairs
        .find(|(key, _)| key == "tag")
        .ok_or_else(|| anyhow::anyhow!("missing tag in git url `{url}`"))?;
    Ok(tag.to_string())
}

pub fn execution_version_from_tag(tag: &str) -> String {
    match tag {
        "v0.0.27-interface-v0.0.13" => String::from("V3"),
        "v0.1.1-interface-v0.0.13" => String::from("V4"),
        "v0.2.7-interface-v0.0.13" => String::from("V5"),
        "v0.2.7-simulation-only-interface-v0.0.13" => String::from("V5_SIMULATION"),
        "dev-20260211-3" => String::from("V6"),
        _ => panic!("Unsupported ZKsync OS execution version: {tag}"),
    }
}

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let metadata = MetadataCommand::new().exec().unwrap();

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

        println!("tag: {tag}");

        let dir = format!("{manifest_dir}/apps/{tag}");
        std::fs::create_dir_all(&dir).expect("failed to create directory");
        for variant in [
            "multiblock_batch",
            "singleblock_batch",
            "singleblock_batch_logging_enabled",
        ] {
            let url = format!(
                "https://github.com/matter-labs/zksync-os/releases/download/{tag}/{variant}.bin"
            );
            let path = format!("{dir}/{variant}.bin");
            if std::fs::exists(&path).expect("failed to check file existence") {
                continue;
            }
            let resp = reqwest::blocking::get(url).expect("failed to download");
            let body = resp.bytes().expect("failed to read response body").to_vec();
            std::fs::write(path, body).expect("failed to write file");
        }

        let execution_version = execution_version_from_tag(&tag);
        println!("cargo:rustc-env=ZKSYNC_OS_{execution_version}_SOURCE_PATH={dir}");
    }
}
