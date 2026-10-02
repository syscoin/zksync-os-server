//! Only newly generated, source-registered Anvil component replay records are
//! recovered here. Ordinary node startup validates their computed block hashes;
//! no executed state, private database or canonical fixture is substituted.
use crate::config::fixture_backend::{
    FileIdentity, ValidatedFixtureInventory, component::ComponentReplayArchive,
};
use alloy::primitives::B256;
use anyhow::{Context, ensure};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
use zksync_os_replay_archive::{
    DEFAULT_DECRYPT_CONCURRENCY, FileSystemReplayArchiveReader, ReplayArchiveStorageReader,
    recover_replay_records_to_rocksdb_with_optional_decryption,
};
use zksync_os_storage::db::BlockReplayStorage;
use zksync_os_storage_api::{ReadReplay, ReplayRecord};

fn private_directory(path: &Path) -> anyhow::Result<()> {
    std::fs::DirBuilder::new().mode(0o700).create(path)?;
    Ok(())
}

fn existing_directory(path: &Path) -> anyhow::Result<()> {
    ensure!(
        path.is_absolute() && path.canonicalize()? == path,
        "canonical replay directory required"
    );
    ensure!(
        std::fs::symlink_metadata(path)?.is_dir(),
        "replay directory required"
    );
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    output.write_all(bytes)?;
    output.sync_all()?;
    Ok(())
}

fn verify_record(bytes: &[u8], chain_id: u64, block_number: u64) -> anyhow::Result<()> {
    // WAL reads supply chain_id from the opener rather than persisting it; verify
    // the actual Noop archive record before recovery instead of trusting that read.
    let record: ReplayRecord =
        serde_json::from_slice(bytes).context("public Noop replay record")?;
    ensure!(
        record.block_context.chain_id == chain_id
            && record.block_context.block_number == block_number,
        "archive record chain/block mismatch"
    );
    Ok(())
}

fn recovery_relative_path(relative: &str) -> anyhow::Result<std::path::PathBuf> {
    let parts: Vec<_> = relative.split('/').collect();
    ensure!(parts.len() == 3, "recovery object layout");
    // The public package uses unprefixed hashes. Existing recovery's downloaded
    // layout uses canonical 0x-prefixed hashes; construct that exact API input.
    Ok(Path::new(parts[0])
        .join(format!("0x{}", parts[1]))
        .join(parts[2]))
}

/// Called only after the freshly generated node and archive writer have drained.
/// The stopped WAL supplies the canonical anchor, not a pre-shutdown RPC tip.
pub async fn export_replay(
    archive_root: &Path,
    stopped_db: &Path,
    chain_id: u64,
    package_root: &Path,
) -> anyhow::Result<serde_json::Value> {
    ensure!(
        std::env::var("SYSCOIN_ANVIL_COMPONENT_ONLY").as_deref() == Ok("31337"),
        "component scope required"
    );
    ensure!(
        [57001, 6565, 6566].contains(&chain_id),
        "component chain required"
    );
    for path in [archive_root, stopped_db, package_root] {
        existing_directory(path)?;
    }
    ensure!(
        !package_root.starts_with(archive_root)
            && !archive_root.starts_with(package_root)
            && !package_root.starts_with(stopped_db)
            && !stopped_db.starts_with(package_root),
        "component export inputs and package must be disjoint"
    );
    let wal_path = stopped_db.join("block_replay_wal");
    existing_directory(&wal_path)?;
    ensure!(
        wal_path.join("CURRENT").is_file(),
        "existing stopped WAL required"
    );
    let wal = BlockReplayStorage::new_without_genesis(&wal_path, chain_id);
    ensure!(
        wal.get_canonical_block_hash(0).is_some(),
        "stopped WAL genesis missing"
    );
    let anchor_number = wal.latest_record();
    let anchor_hash = wal
        .get_canonical_block_hash(anchor_number)
        .context("canonical stopped WAL anchor missing")?;
    ensure!(
        anchor_number < 4096,
        "finite component replay head required"
    );
    let reader = FileSystemReplayArchiveReader::new(archive_root.to_path_buf());
    let page = reader.list_keys_page(None).await?;
    ensure!(
        page.next_page_token.is_none(),
        "filesystem replay page expected"
    );
    let mut selected = BTreeMap::new();
    for key in page.keys {
        if key.block_number <= anchor_number
            && wal.get_canonical_block_hash(key.block_number) == Some(key.block_hash)
        {
            ensure!(
                selected.insert(key.block_number, key).is_none(),
                "duplicate canonical archive object"
            );
        }
    }
    ensure!(
        selected.len() == anchor_number as usize + 1,
        "writer-drained canonical archive is incomplete"
    );
    // RocksDBInner drops synchronously and cancels background work with wait=true.
    // A global instance-count wait would deadlock on supporting Gateway nodes.
    drop(wal);
    let replay_root = package_root.join("replay");
    if !replay_root.exists() {
        private_directory(&replay_root)?;
    }
    existing_directory(&replay_root)?;
    let chain_root = replay_root.join(chain_id.to_string());
    private_directory(&chain_root)?;
    let mut objects = Vec::new();
    let mut total_bytes = 0usize;
    for (number, key) in selected {
        let source = archive_root.join(key.object_path());
        ensure!(
            source.canonicalize()? == source && std::fs::symlink_metadata(&source)?.is_file(),
            "canonical archive object required"
        );
        let bytes = reader.fetch_object(&key).await?;
        ensure!(
            !bytes.is_empty() && bytes.len() <= 256 * 1024 * 1024,
            "bounded replay object required"
        );
        total_bytes = total_bytes
            .checked_add(bytes.len())
            .context("replay size overflow")?;
        ensure!(
            total_bytes <= 2 * 1024 * 1024 * 1024,
            "bounded replay archive required"
        );
        verify_record(&bytes, chain_id, number)?;
        let hash = format!("{:x}", key.block_hash);
        let block_root = chain_root.join(number.to_string());
        private_directory(&block_root)?;
        let hash_root = block_root.join(&hash);
        private_directory(&hash_root)?;
        let path = format!("replay/{chain_id}/{number}/{hash}/{}", key.session);
        write_new(&package_root.join(&path), &bytes)?;
        objects.push(serde_json::json!({"path":path, "size":bytes.len(), "sha256":format!("{:x}", Sha256::digest(&bytes))}));
    }
    let result = serde_json::json!({"schema":"syscoin-v32-component-replay-archive-v1", "chain_id":chain_id,
        "anchor_block_number":anchor_number, "anchor_block_hash":format!("{anchor_hash:#x}"), "objects":objects});
    let mut bytes = serde_json::to_vec_pretty(&result)?;
    bytes.push(b'\n');
    write_new(&chain_root.join("manifest.json"), &bytes)?;
    Ok(result)
}

/// Source-registered descriptors bind every object byte and the canonical anchor.
/// Recovery writes only a fresh WAL; ordinary startup reconstructs all other DBs.
pub async fn recover_replay(
    root: &Path,
    archive: &ComponentReplayArchive,
    destination: &Path,
) -> anyhow::Result<()> {
    existing_directory(root)?;
    archive.validate(root).map_err(anyhow::Error::msg)?;
    ensure!(
        destination.is_absolute()
            && destination
                .parent()
                .context("fresh DB parent")?
                .canonicalize()?
                == destination.parent().unwrap(),
        "canonical fresh DB parent required"
    );
    ensure!(
        !destination.starts_with(root) && !root.starts_with(destination),
        "fixture inputs and recovered database must be disjoint"
    );
    ensure!(
        !destination.exists() && !destination.is_symlink(),
        "component replay destination must be absent"
    );
    let staging = tempfile::Builder::new()
        .prefix("component-replay-")
        .tempdir_in(destination.parent().unwrap())?;
    for object in &archive.objects {
        let source = object.verify(root).map_err(anyhow::Error::msg)?;
        let bytes = std::fs::read(source)?;
        object.verify_bytes(&bytes).map_err(anyhow::Error::msg)?;
        let relative = object
            .path
            .strip_prefix(&format!("replay/{}/", archive.chain_id))
            .context("chain archive path")?;
        let number = relative
            .split('/')
            .next()
            .context("archive number")?
            .parse()?;
        verify_record(&bytes, archive.chain_id, number)?;
        let target = staging.path().join(recovery_relative_path(relative)?);
        std::fs::create_dir_all(target.parent().context("recovery object parent")?)?;
        write_new(&target, &bytes)?;
    }
    private_directory(destination)?;
    let anchor: B256 = archive.anchor_block_hash.parse()?;
    let recovered = recover_replay_records_to_rocksdb_with_optional_decryption(
        staging.path(),
        &destination.join("block_replay_wal"),
        archive.anchor_block_number,
        anchor,
        None,
        DEFAULT_DECRYPT_CONCURRENCY,
    )
    .await?;
    ensure!(
        recovered == archive.anchor_block_number as usize + 1,
        "canonical replay count mismatch"
    );
    // Recovery's local WAL is synchronously dropped before its future returns.
    let wal = BlockReplayStorage::new_without_genesis(
        &destination.join("block_replay_wal"),
        archive.chain_id,
    );
    ensure!(
        wal.latest_record() == archive.anchor_block_number
            && wal.get_canonical_block_hash(archive.anchor_block_number) == Some(anchor),
        "recovered canonical WAL anchor mismatch"
    );
    drop(wal);
    Ok(())
}

pub async fn recover_registered(
    root: &Path,
    chain_id: u64,
    destination: &Path,
) -> anyhow::Result<()> {
    let hash = crate::config::fixture_backend::component::registered_hash("v32.0")
        .map_err(anyhow::Error::msg)?;
    let inventory = crate::config::fixture_backend::component::load(root, "v32.0", &hash)
        .map_err(anyhow::Error::msg)?;
    let ValidatedFixtureInventory::AnvilComponentOnly(descriptor) = inventory else {
        anyhow::bail!("component inventory required");
    };
    let archive = descriptor
        .replay_archives
        .iter()
        .find(|archive| archive.chain_id == chain_id)
        .context("component chain replay missing")?;
    let marker = descriptor
        .database_identity_bytes(root, chain_id)
        .map_err(anyhow::Error::msg)?;
    recover_replay(root, archive, destination).await?;
    // The seed's real deployment marker belongs beside the fresh recovered WAL;
    // normal startup rederives and checks the full active deployment identity.
    write_new(
        &destination.join(crate::config::fixture_backend::component::DATABASE_IDENTITY_FILE_NAME),
        &marker,
    )
}

pub async fn recover_manifest(
    root: &Path,
    manifest: &Path,
    expected_sha: &str,
    destination: &Path,
) -> anyhow::Result<()> {
    ensure!(
        std::env::var("SYSCOIN_ANVIL_COMPONENT_ONLY").as_deref() == Ok("31337"),
        "component generation scope required"
    );
    existing_directory(root)?;
    let relative = manifest
        .strip_prefix(root)?
        .to_str()
        .context("manifest path type")?;
    let identity = FileIdentity {
        path: relative.to_owned(),
        size: std::fs::symlink_metadata(manifest)?.len(),
        sha256: expected_sha.to_owned(),
    };
    let path = identity.verify(root).map_err(anyhow::Error::msg)?;
    ensure!(
        identity.size <= 1024 * 1024,
        "bounded replay manifest required"
    );
    let bytes = std::fs::read(path)?;
    identity.verify_bytes(&bytes).map_err(anyhow::Error::msg)?;
    let archive: ComponentReplayArchive = serde_json::from_slice(&bytes)?;
    recover_replay(root, &archive, destination).await
}

/// Generation configs outside this namespace require the explicit recover-replay
/// command; only a source-registered packaged config is restored automatically.
pub fn registered_root_for_config(path: &Path) -> Option<&Path> {
    let directory = path.parent()?;
    let root = directory.parent()?;
    let name = path.file_name()?.to_str()?;
    let recognized = (directory.file_name()? == "default" && name == "config.yaml")
        || (directory.file_name()? == "multi_chain"
            && ["chain_57001.yaml", "chain_6565.yaml", "chain_6566.yaml"].contains(&name));
    (recognized
        && root.file_name()? == "v32.0"
        && root.parent()?.file_name()? == "anvil-component-only")
        .then_some(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restored_seed_marker_is_create_only_private_and_byte_preserving() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("database_identity.json");
        let bytes = b"original public seed marker bytes\n";
        write_new(&path, bytes).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(write_new(&path, b"replacement").is_err());
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn only_registered_namespace_configs_trigger_initial_restore() {
        for config in [
            "default/config.yaml",
            "multi_chain/chain_57001.yaml",
            "multi_chain/chain_6565.yaml",
            "multi_chain/chain_6566.yaml",
        ] {
            let path = Path::new("/repo/local-chains/anvil-component-only/v32.0").join(config);
            assert_eq!(
                registered_root_for_config(&path),
                Some(Path::new("/repo/local-chains/anvil-component-only/v32.0"))
            );
        }
        for config in [
            "/work/generated-config.json",
            "/repo/local-chains/v32.0/default/config.yaml",
            "/repo/local-chains/anvil-component-only/v31.0/default/config.yaml",
            "/repo/local-chains/anvil-component-only/v32.0/multi_chain/chain_1.yaml",
        ] {
            assert!(registered_root_for_config(Path::new(config)).is_none());
        }
    }

    #[test]
    fn recovery_layout_matches_existing_download_api() {
        let hash = "ab".repeat(32);
        let session = format!("1-{}-node", "cd".repeat(16));
        assert_eq!(
            recovery_relative_path(&format!("0/{hash}/{session}")).unwrap(),
            Path::new("0").join(format!("0x{hash}")).join(session)
        );
        assert!(recovery_relative_path("0/hash").is_err());
        assert!(verify_record(b"{}", 57001, 0).is_err());
    }
}
