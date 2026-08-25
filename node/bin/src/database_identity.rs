use alloy::primitives::{Address, B256};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

pub(crate) const DATABASE_IDENTITY_FILE_NAME: &str = "database_identity.json";
const DATABASE_IDENTITY_SCHEMA_VERSION: u32 = 1;
const MAX_DATABASE_IDENTITY_BYTES: u64 = 16 * 1024;

/// Identity shared by every database below one node storage root.
///
/// SYSCOIN: V31 testnet state is intentionally not migrated into the fresh V32 deployment lane.
/// Bind the complete storage root before opening any child RocksDB so a same-chain reset cannot
/// silently combine state from different deployments.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DatabaseIdentity {
    schema_version: u32,
    protocol_version: String,
    l1_chain_id: u64,
    l1_genesis_block_hash: B256,
    l2_chain_id: u64,
    diamond_proxy_l1: Address,
    l2_genesis_block_hash: B256,
}

impl DatabaseIdentity {
    pub(crate) fn new(
        protocol_version: &str,
        l1_chain_id: u64,
        l1_genesis_block_hash: B256,
        l2_chain_id: u64,
        diamond_proxy_l1: Address,
        l2_genesis_block_hash: B256,
    ) -> Self {
        Self {
            schema_version: DATABASE_IDENTITY_SCHEMA_VERSION,
            protocol_version: protocol_version.to_owned(),
            l1_chain_id,
            l1_genesis_block_hash,
            l2_chain_id,
            diamond_proxy_l1,
            l2_genesis_block_hash,
        }
    }
}

/// Creates the root identity on a fresh directory or verifies an existing identity exactly.
pub(crate) fn ensure_database_identity(
    database_root: &Path,
    expected: &DatabaseIdentity,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(database_root)
        .with_context(|| format!("failed to create database root {}", database_root.display()))?;
    let identity_path = database_root.join(DATABASE_IDENTITY_FILE_NAME);

    if identity_path.try_exists().with_context(|| {
        format!(
            "failed to inspect database identity {}",
            identity_path.display()
        )
    })? {
        let stored = read_database_identity(&identity_path)?;
        anyhow::ensure!(
            stored == *expected,
            "database identity mismatch at {}; reset or move the complete pre-V32 node database directory before startup\nexpected: {expected:?}\nstored: {stored:?}",
            identity_path.display()
        );
        return Ok(());
    }

    // SYSCOIN: A missing marker is valid only before any child database exists. Filesystem
    // replacement by the trusted host/operator is outside this marker's deployment-mismatch role.
    let mut entries = std::fs::read_dir(database_root)
        .with_context(|| format!("failed to read database root {}", database_root.display()))?;
    if let Some(entry) = entries.next().transpose().with_context(|| {
        format!(
            "failed to inspect an entry under database root {}",
            database_root.display()
        )
    })? {
        anyhow::bail!(
            "unmarked database root {} is not empty (found {}); reset or move the complete pre-V32 node database directory before startup",
            database_root.display(),
            entry.path().display()
        );
    }

    create_database_identity(&identity_path, expected)
}

fn read_database_identity(path: &Path) -> anyhow::Result<DatabaseIdentity> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect database identity {}", path.display()))?;
    anyhow::ensure!(
        metadata.file_type().is_file(),
        "database identity {} is not a regular file",
        path.display()
    );

    let file = File::open(path)
        .with_context(|| format!("failed to open database identity {}", path.display()))?;
    // SYSCOIN: Read one sentinel byte past the bound so a malformed local marker cannot drive an
    // unbounded startup allocation. A partial marker intentionally fails parsing and stays closed.
    let mut bytes = Vec::new();
    file.take(MAX_DATABASE_IDENTITY_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read database identity {}", path.display()))?;
    anyhow::ensure!(
        bytes.len() <= MAX_DATABASE_IDENTITY_BYTES as usize,
        "database identity {} exceeds the {}-byte limit",
        path.display(),
        MAX_DATABASE_IDENTITY_BYTES
    );

    serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse database identity {}", path.display()))
}

fn create_database_identity(path: &Path, expected: &DatabaseIdentity) -> anyhow::Result<()> {
    let mut bytes =
        serde_json::to_vec_pretty(expected).context("failed to serialize database identity")?;
    bytes.push(b'\n');
    anyhow::ensure!(
        bytes.len() <= MAX_DATABASE_IDENTITY_BYTES as usize,
        "serialized database identity exceeds the {MAX_DATABASE_IDENTITY_BYTES}-byte limit"
    );

    // SYSCOIN: Direct create-new publication is sufficient under exclusive node ownership. It
    // cannot overwrite an existing identity; an interrupted partial write fails closed next boot.
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to create database identity {}", path.display()))?;
    file.write_all(&bytes)
        .with_context(|| format!("failed to write database identity {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync database identity {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        DATABASE_IDENTITY_FILE_NAME, DatabaseIdentity, MAX_DATABASE_IDENTITY_BYTES,
        ensure_database_identity,
    };
    use alloy::primitives::{Address, B256};

    fn identity() -> DatabaseIdentity {
        DatabaseIdentity::new(
            "v32.0",
            1,
            B256::repeat_byte(0x11),
            57,
            Address::repeat_byte(0x22),
            B256::repeat_byte(0x33),
        )
    }

    #[test]
    fn binds_fresh_root_and_reopens_exact_identity() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("node");
        let expected = identity();

        ensure_database_identity(&root, &expected).unwrap();
        ensure_database_identity(&root, &expected).unwrap();

        assert!(root.join(DATABASE_IDENTITY_FILE_NAME).is_file());
    }

    #[test]
    fn rejects_nonempty_unmarked_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("legacy-state"), b"v31").unwrap();

        let err = ensure_database_identity(dir.path(), &identity()).unwrap_err();
        assert!(err.to_string().contains("unmarked database root"));
        assert!(!dir.path().join(DATABASE_IDENTITY_FILE_NAME).exists());
    }

    #[test]
    fn rejects_a_different_deployment_without_overwriting_marker() {
        let dir = tempfile::tempdir().unwrap();
        let expected = identity();
        ensure_database_identity(dir.path(), &expected).unwrap();
        let marker = dir.path().join(DATABASE_IDENTITY_FILE_NAME);
        let original = std::fs::read(&marker).unwrap();

        let mut other = expected.clone();
        other.l2_genesis_block_hash = B256::repeat_byte(0x44);
        let err = ensure_database_identity(dir.path(), &other).unwrap_err();

        assert!(err.to_string().contains("database identity mismatch"));
        assert_eq!(std::fs::read(marker).unwrap(), original);
    }

    #[test]
    fn rejects_truncated_existing_marker() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(DATABASE_IDENTITY_FILE_NAME), b"{").unwrap();

        let err = ensure_database_identity(dir.path(), &identity()).unwrap_err();
        assert!(
            err.to_string()
                .contains("failed to parse database identity")
        );
    }

    #[test]
    fn rejects_oversized_identity_before_unbounded_read() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(DATABASE_IDENTITY_FILE_NAME);
        let file = std::fs::File::create(marker).unwrap();
        file.set_len(MAX_DATABASE_IDENTITY_BYTES + 1).unwrap();

        let err = ensure_database_identity(dir.path(), &identity()).unwrap_err();
        assert!(err.to_string().contains("exceeds the 16384-byte limit"));
    }
}
