use cargo_metadata::MetadataCommand;
use sha2::{Digest, Sha256};
use std::{fmt::Write as _, path::Path};

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn require_source_sha256(root: &Path, relative_path: &str, expected: &str) -> anyhow::Result<()> {
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

fn verify_syscoin_source(manifest: &Path) -> anyhow::Result<()> {
    let source_root = manifest.parent().and_then(Path::parent).ok_or_else(|| {
        anyhow::anyhow!(
            "invalid forward_system manifest path: {}",
            manifest.display()
        )
    })?;

    // SYSCOIN: These exact files bind native execution to the audited final-v0.4 guest source.
    require_source_sha256(
        source_root,
        "basic_bootloader/src/bootloader/transaction_flow/zk/syscoin_edge_da.rs",
        "383259d3edeb24c56dfc9d8ee6fb5e814673a712a44cabcbd1c86338b2791899",
    )?;
    require_source_sha256(
        source_root,
        "basic_bootloader/src/bootloader/block_flow/zk/post_tx_op/da_commitment_generator/blob_commitment_generator/mod.rs",
        "843481be1d01a40dc6b92814ecacaa16fc3b87dc721c93ea0b47a1bc3ec82e1f",
    )?;
    require_source_sha256(
        source_root,
        "basic_system/src/system_functions/slh_dsa_sha2_128_24_verify.rs",
        "459d8443f9675ab15e8eea39f39c3549a4f13df5ddec0cdeb9f66d101c095067",
    )?;
    require_source_sha256(
        source_root,
        "callable_oracles/src/blob_data_id/mod.rs",
        "e4c64345e49c1c0d578628f68428c519b46736f424f9d8c38ae9ce5bee9dee73",
    )?;
    require_source_sha256(
        source_root,
        "forward_system/src/run/mod.rs",
        "2b17e4a417e29e34048c197d9a8e78c809cadf9562d730f2ac209f5f29249827",
    )?;
    Ok(())
}

fn main() {
    let metadata = MetadataCommand::new()
        .exec()
        .expect("failed to read Cargo metadata");
    let forward_systems: Vec<_> = metadata
        .packages
        .iter()
        .filter(|package| package.name.as_str() == "forward_system")
        .collect();

    assert_eq!(
        forward_systems.len(),
        1,
        "expected one canonical forward_system source, found {}",
        forward_systems.len()
    );
    verify_syscoin_source(forward_systems[0].manifest_path.as_std_path()).unwrap_or_else(|err| {
        panic!(
            "zksync-os execution source is not the audited Syscoin final-v0.4 postimage: {err}. Use scripts/cargo-with-patched-zksync-os.sh instead of plain Cargo"
        )
    });
}
