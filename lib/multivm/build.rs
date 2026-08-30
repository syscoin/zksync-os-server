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
        "b2c21b485a3460598f3c26bcdc6f6dcd9fb7e7b7ffb6419b56a968b529aa0c3c",
    )?;
    require_source_sha256(
        source_root,
        "basic_bootloader/src/bootloader/transaction_flow/zk/syscoin_gas_tank.rs",
        "7ba8d21c59b244c090be3cda6e01581d652a79c930ff0a488172e1212b74f188",
    )?;
    require_source_sha256(
        source_root,
        "basic_bootloader/src/bootloader/block_flow/zk/post_tx_op/da_commitment_generator/mod.rs",
        "cbf166eea82af6c2fc5d0570095630987498dc75bce935cb7b3e05077a8f1863",
    )?;
    require_source_sha256(
        source_root,
        "basic_bootloader/src/bootloader/block_flow/zk/post_tx_op/da_commitment_generator/blob_commitment_generator/mod.rs",
        "8fff7414159aff9ea8fe8513e57b6cc6f31aa5ae15943066aae224f1dcff3d26",
    )?;
    require_source_sha256(
        source_root,
        "basic_bootloader/src/bootloader/block_flow/zk/post_tx_op/da_commitment_generator/syscoin_commitment_generator.rs",
        "39be17a6fb165137e175271758514de959c0812e1579275a6f5d4d3a386a421c",
    )?;
    require_source_sha256(
        source_root,
        "basic_system/src/system_functions/slh_dsa_sha2_128_24_verify.rs",
        "929738ac17af40fa260313ed0a8ce09e396ebede3b10f32a3dd7701928078b84",
    )?;
    require_source_sha256(
        source_root,
        "forward_system/src/run/mod.rs",
        "b7980e0634eef1808edb4c804de0d598ab7baea7bec2620fc4bc2adf71d88af7",
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
