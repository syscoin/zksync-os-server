//! Current V32/V8 Anvil component-test inventory. This is deliberately not the
//! canonical fixture loader and cannot certify Core consensus, NEVM DA or proofs.
use super::{FileIdentity, FixtureResult, ValidatedFixtureInventory, regular_file};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::io::Read;
use std::{fs, path::Path};

pub const DESCRIPTOR_FILE: &str = "anvil-component.json";
pub const DATABASE_IDENTITY_FILE_NAME: &str = "database_identity.json";
const MAX_DATABASE_IDENTITY_BYTES: u64 = 16 * 1024;
pub const GENESIS_SHA256: &str = "5adf0dd1b618911d51c335e983c0c71cc1c74fc7db37161bf76a4b51e5055a95";
pub const VK_HASH: &str = "0xc1ab3d6506620ad299672c2c2530e8732ac7bae55cdb9d8cf1fa12355b7388fe";
/// Public, insecure Anvil development account 0, not an operator wallet output.
pub const PUBLIC_ANVIL_REVERTER_KEY: &str =
    "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
pub const PUBLIC_ANVIL_REVERTER_ADDRESS: &str = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";

pub const PUBLIC_FILES: &[&str] = &[
    "default/config.yaml",
    "default/genesis.json",
    "deployment.json",
    "multi_chain/chain_6565.yaml",
    "multi_chain/chain_6566.yaml",
    "multi_chain/chain_57001.yaml",
    "proof-identities.json",
    "replay/57001/database_identity.json",
    "replay/6565/database_identity.json",
    "replay/6566/database_identity.json",
    "source-identities.json",
    "test-signers.json",
    "versions.yaml",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    schema: String,
    protocol_version: String,
    descriptor_sha256: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentDescriptor {
    schema_version: u32,
    scope: String,
    protocol_version: String,
    execution_version: u32,
    proving_version: u32,
    security_bits: u32,
    verification_key_hash: String,
    root_chain_id: u64,
    default_chain_id: u64,
    gateway_chain_id: u64,
    edge_chain_ids: [u64; 2],
    reverter_address: String,
    pub compressed_state: FileIdentity,
    pub decompressed_state: FileIdentity,
    files: Vec<FileIdentity>,
    pub replay_archives: Vec<ComponentReplayArchive>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComponentDatabaseIdentity {
    schema_version: u32,
    protocol_version: String,
    l1_chain_id: u64,
    l1_genesis_block_hash: String,
    l2_chain_id: u64,
    diamond_proxy_l1: String,
    l2_genesis_block_hash: String,
}

impl ComponentDescriptor {
    /// Return the original seed node's authenticated public marker, not a
    /// reconstructed identity. Normal startup still compares it with live L1.
    pub fn database_identity_bytes(&self, root: &Path, chain_id: u64) -> FixtureResult<Vec<u8>> {
        if ![57001, 6565, 6566].contains(&chain_id) {
            return Err("unsupported component database chain".into());
        }
        let relative = format!("replay/{chain_id}/{DATABASE_IDENTITY_FILE_NAME}");
        let file = self
            .files
            .iter()
            .find(|file| file.path == relative)
            .ok_or("component database identity missing")?;
        if file.size > MAX_DATABASE_IDENTITY_BYTES {
            return Err("component database identity exceeds size limit".into());
        }
        let path = file.verify(root)?;
        let mut bytes = Vec::new();
        fs::File::open(path)
            .map_err(|error| error.to_string())?
            .take(MAX_DATABASE_IDENTITY_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        file.verify_bytes(&bytes)?;
        let identity: ComponentDatabaseIdentity =
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        let valid_hash = |hash: &str| hash.strip_prefix("0x").is_some_and(super::valid_digest);
        let valid_address = identity
            .diamond_proxy_l1
            .strip_prefix("0x")
            .is_some_and(|address| {
                address.len() == 40
                    && address
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    && address.bytes().any(|byte| byte != b'0')
            });
        if identity.schema_version != 1
            || identity.protocol_version != "v32.0"
            || identity.l1_chain_id != 31337
            || identity.l2_chain_id != chain_id
            || !valid_hash(&identity.l1_genesis_block_hash)
            || !valid_hash(&identity.l2_genesis_block_hash)
            || !valid_address
        {
            return Err("invalid component database deployment identity".into());
        }
        Ok(bytes)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentReplayArchive {
    pub schema: String,
    pub chain_id: u64,
    pub anchor_block_number: u64,
    pub anchor_block_hash: String,
    pub objects: Vec<FileIdentity>,
}

impl ComponentReplayArchive {
    pub fn validate(&self, root: &Path) -> FixtureResult<()> {
        if self.schema != "syscoin-v32-component-replay-archive-v1"
            || ![57001, 6565, 6566].contains(&self.chain_id)
            || self.anchor_block_number >= 4096
            || !super::valid_digest(self.anchor_block_hash.strip_prefix("0x").unwrap_or(""))
            || self.objects.len() != self.anchor_block_number as usize + 1
        {
            return Err("invalid component replay archive identity/count".into());
        }
        let prefix = format!("replay/{}/", self.chain_id);
        let mut numbers = BTreeSet::new();
        let mut total_bytes = 0u64;
        for (index, object) in self.objects.iter().enumerate() {
            let suffix = object
                .path
                .strip_prefix(&prefix)
                .ok_or("component replay object is outside its chain namespace")?;
            let parts: Vec<_> = suffix.split('/').collect();
            if parts.len() != 3 || object.size > 256 * 1024 * 1024 {
                return Err("invalid component replay object layout/size".into());
            }
            let number: u64 = parts[0]
                .parse()
                .map_err(|_| "invalid replay block number")?;
            if number.to_string() != parts[0]
                || number != index as u64
                || number > self.anchor_block_number
                || !numbers.insert(number)
                || !super::valid_digest(parts[1])
                || (number == self.anchor_block_number
                    && format!("0x{}", parts[1]) != self.anchor_block_hash)
            {
                return Err("component replay block/anchor mismatch".into());
            }
            let session: Vec<_> = parts[2].splitn(3, '-').collect();
            if session.len() != 3
                || session[0]
                    .parse::<u64>()
                    .ok()
                    .is_none_or(|value| value.to_string() != session[0])
                || session[1].len() != 32
                || !session[1]
                    .bytes()
                    .all(|value| value.is_ascii_digit() || (b'a'..=b'f').contains(&value))
                || session[2].is_empty()
                || matches!(session[2], "." | "..")
            {
                return Err("invalid component replay session".into());
            }
            object.verify(root)?;
            total_bytes = total_bytes
                .checked_add(object.size)
                .ok_or("component replay byte count overflow")?;
            if total_bytes > 2 * 1024 * 1024 * 1024 {
                return Err("component replay archive exceeds size limit".into());
            }
        }
        Ok(())
    }
}

/// The source-controlled registration is outside the generated fixture. Null is
/// an unissued registration, not permission to trust a sidecar or environment.
pub fn registered_hash(version: &str) -> FixtureResult<String> {
    let registration: Registration = serde_json::from_str(include_str!(
        "../../scripts/fixtures/v32-component-registration.json"
    ))
    .map_err(|_| "invalid component registration")?;
    if version != "v32.0"
        || registration.schema != "syscoin-v32-anvil-component-registration-v1"
        || registration.protocol_version != version
    {
        return Err("component registration protocol/schema mismatch".into());
    }
    registration
        .descriptor_sha256
        .filter(|value| super::valid_digest(value))
        .ok_or_else(|| {
            "current Anvil component fixture has not been generated and registered".into()
        })
}

pub fn load(
    root: &Path,
    version: &str,
    trusted_hash: &str,
) -> FixtureResult<ValidatedFixtureInventory> {
    super::check_regeneration_marker(root)?;
    if version != "v32.0" || !super::valid_digest(trusted_hash) {
        return Err("current component scope requires a trusted V32/V8 descriptor".into());
    }
    let path = regular_file(root, DESCRIPTOR_FILE)?;
    if fs::metadata(&path).map_err(|e| e.to_string())?.len() > super::MAX_DESCRIPTOR_BYTES {
        return Err("component descriptor exceeds size limit".into());
    }
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    if format!("{:x}", Sha256::digest(&bytes)) != trusted_hash {
        return Err("component descriptor trusted hash mismatch".into());
    }
    let descriptor: ComponentDescriptor =
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if descriptor.schema_version != 1
        || descriptor.scope != "AnvilComponentOnly"
        || descriptor.protocol_version != version
        || descriptor.execution_version != 7
        || descriptor.proving_version != 8
        || descriptor.security_bits != 100
        || descriptor.verification_key_hash != VK_HASH
        || descriptor.root_chain_id != 31337
        || descriptor.gateway_chain_id != 57001
        || descriptor.default_chain_id != descriptor.gateway_chain_id
        || descriptor.edge_chain_ids != [6565, 6566]
        || descriptor.reverter_address != PUBLIC_ANVIL_REVERTER_ADDRESS
    {
        return Err("component fixture is not the reviewed V32/V8 test identity".into());
    }
    if descriptor.compressed_state.path != "l1-state.json.zst"
        || descriptor.decompressed_state.path != "l1-state.json"
        || descriptor.compressed_state.size > 64 * 1024 * 1024
        || descriptor.decompressed_state.size > 2 * 1024 * 1024 * 1024
    {
        return Err("component requires explicit Anvil state paths".into());
    }
    super::check_anvil_output_paths(root)?;
    descriptor.compressed_state.verify(root)?;
    descriptor.decompressed_state.validate_identity()?;
    let expected: BTreeSet<_> = PUBLIC_FILES.iter().copied().collect();
    let actual: BTreeSet<_> = descriptor
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    if descriptor.files.len() != expected.len() || actual != expected {
        return Err("component public artifact roles missing, duplicate or unexpected".into());
    }
    for chain_id in [57001, 6565, 6566] {
        descriptor.database_identity_bytes(root, chain_id)?;
    }
    for file in &descriptor.files {
        file.verify(root)?;
        if file.path == "default/genesis.json" && file.sha256 != GENESIS_SHA256 {
            return Err("component genesis differs from the accepted current genesis".into());
        }
    }
    let chains: BTreeSet<_> = descriptor
        .replay_archives
        .iter()
        .map(|archive| archive.chain_id)
        .collect();
    if descriptor.replay_archives.len() != 3
        || chains != BTreeSet::from([57001, 6565, 6566])
        || descriptor
            .replay_archives
            .iter()
            .map(|archive| archive.objects.len())
            .sum::<usize>()
            > 4096
        || descriptor
            .replay_archives
            .iter()
            .flat_map(|archive| &archive.objects)
            .try_fold(0u64, |total, object| total.checked_add(object.size))
            .is_none_or(|total| total > 2 * 1024 * 1024 * 1024)
    {
        return Err("component requires three finite physical chain replay archives".into());
    }
    for archive in &descriptor.replay_archives {
        archive.validate(root)?;
    }
    Ok(ValidatedFixtureInventory::AnvilComponentOnly(Box::new(
        descriptor,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(chain_id: u64) -> serde_json::Value {
        serde_json::json!({"schema_version": 1, "protocol_version": "v32.0",
            "l1_chain_id": 31337, "l1_genesis_block_hash": format!("0x{}", "ab".repeat(32)),
            "l2_chain_id": chain_id, "diamond_proxy_l1": format!("0x{}", "cd".repeat(20)),
            "l2_genesis_block_hash": format!("0x{}", "ef".repeat(32))})
    }

    fn descriptor_with_marker(root: &Path, chain_id: u64, bytes: &[u8]) -> ComponentDescriptor {
        let relative = format!("replay/{chain_id}/{DATABASE_IDENTITY_FILE_NAME}");
        let path = root.join(&relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        let state =
            serde_json::json!({"path": "l1-state.json", "size": 1, "sha256": "ab".repeat(32)});
        serde_json::from_value(serde_json::json!({"schema_version": 1, "scope": "AnvilComponentOnly",
            "protocol_version": "v32.0", "execution_version": 7, "proving_version": 8,
            "security_bits": 100, "verification_key_hash": VK_HASH, "root_chain_id": 31337,
            "default_chain_id": 57001, "gateway_chain_id": 57001, "edge_chain_ids": [6565, 6566],
            "reverter_address": PUBLIC_ANVIL_REVERTER_ADDRESS,
            "compressed_state": state, "decompressed_state": state,
            "files": [{"path": relative, "size": bytes.len(), "sha256": format!("{:x}", Sha256::digest(bytes))}],
            "replay_archives": []})).unwrap()
    }

    #[test]
    fn seed_database_markers_are_exact_authenticated_chain_specific_bytes() {
        assert_eq!(PUBLIC_FILES.len(), 13);
        let root = tempfile::tempdir().unwrap();
        for chain_id in [57001, 6565, 6566] {
            let mut bytes = serde_json::to_vec_pretty(&marker(chain_id)).unwrap();
            bytes.push(b'\n');
            let descriptor = descriptor_with_marker(root.path(), chain_id, &bytes);
            assert_eq!(
                descriptor
                    .database_identity_bytes(root.path(), chain_id)
                    .unwrap(),
                bytes
            );
            assert!(
                descriptor
                    .database_identity_bytes(root.path(), 12345)
                    .is_err()
            );
            assert!(
                descriptor
                    .database_identity_bytes(root.path(), 31337)
                    .is_err()
            );
        }
    }

    #[test]
    fn seed_database_marker_rejects_wrong_identity_and_unbounded_or_changed_bytes() {
        let root = tempfile::tempdir().unwrap();
        for (field, value) in [
            ("schema_version", serde_json::json!(2)),
            ("protocol_version", serde_json::json!("v31.0")),
            ("l1_chain_id", serde_json::json!(1)),
            ("l2_chain_id", serde_json::json!(6565)),
            ("l2_chain_id", serde_json::json!(true)),
            ("l1_genesis_block_hash", serde_json::json!("0x00")),
            (
                "l2_genesis_block_hash",
                serde_json::json!(format!("0x{}", "00".repeat(32))),
            ),
            (
                "diamond_proxy_l1",
                serde_json::json!(format!("0x{}", "00".repeat(20))),
            ),
            ("unexpected", serde_json::json!("private-field")),
        ] {
            let mut invalid = marker(57001);
            invalid[field] = value;
            let bytes = serde_json::to_vec(&invalid).unwrap();
            let descriptor = descriptor_with_marker(root.path(), 57001, &bytes);
            assert!(
                descriptor
                    .database_identity_bytes(root.path(), 57001)
                    .is_err(),
                "{field}"
            );
        }
        let descriptor = descriptor_with_marker(
            root.path(),
            57001,
            &vec![b' '; MAX_DATABASE_IDENTITY_BYTES as usize + 1],
        );
        assert!(
            descriptor
                .database_identity_bytes(root.path(), 57001)
                .is_err()
        );
        let bytes = serde_json::to_vec(&marker(57001)).unwrap();
        let descriptor = descriptor_with_marker(root.path(), 57001, &bytes);
        fs::write(
            root.path().join("replay/57001/database_identity.json"),
            b"changed",
        )
        .unwrap();
        assert!(
            descriptor
                .database_identity_bytes(root.path(), 57001)
                .is_err()
        );
    }

    #[test]
    fn default_view_requires_the_same_physical_gateway_identity() {
        let identity = serde_json::json!({"path": "l1-state.json", "size": 1,
            "sha256": "ab".repeat(32)});
        let mut descriptor = serde_json::json!({
            "schema_version": 1, "scope": "AnvilComponentOnly", "protocol_version": "v32.0",
            "execution_version": 7, "proving_version": 8, "security_bits": 100,
            "verification_key_hash": VK_HASH, "root_chain_id": 31337,
            "default_chain_id": 57001, "gateway_chain_id": 57001,
            "edge_chain_ids": [6565, 6566], "reverter_address": PUBLIC_ANVIL_REVERTER_ADDRESS,
            "compressed_state": identity, "decompressed_state": identity,
            "files": [], "replay_archives": []
        });
        let parsed: ComponentDescriptor = serde_json::from_value(descriptor.clone()).unwrap();
        assert_eq!(parsed.default_chain_id, parsed.gateway_chain_id);
        descriptor
            .as_object_mut()
            .unwrap()
            .remove("default_chain_id");
        assert!(serde_json::from_value::<ComponentDescriptor>(descriptor.clone()).is_err());
        descriptor["default_chain_id"] = serde_json::json!(12345);
        let root = tempfile::tempdir().unwrap();
        let bytes = serde_json::to_vec(&descriptor).unwrap();
        fs::write(root.path().join(DESCRIPTOR_FILE), &bytes).unwrap();
        let hash = format!("{:x}", Sha256::digest(bytes));
        assert_eq!(
            load(root.path(), "v32.0", &hash).unwrap_err(),
            "component fixture is not the reviewed V32/V8 test identity"
        );
    }

    fn archive(root: &Path) -> ComponentReplayArchive {
        let hash = "ab".repeat(32);
        let relative = format!("replay/57001/0/{hash}/1-{}-node", "ab".repeat(16));
        let path = root.join(&relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes = b"public replay fixture validation test";
        fs::write(path, bytes).unwrap();
        ComponentReplayArchive {
            schema: "syscoin-v32-component-replay-archive-v1".into(),
            chain_id: 57001,
            anchor_block_number: 0,
            anchor_block_hash: format!("0x{hash}"),
            objects: vec![FileIdentity {
                path: relative,
                size: bytes.len() as u64,
                sha256: format!("{:x}", Sha256::digest(bytes)),
            }],
        }
    }

    #[test]
    fn replay_manifest_requires_exact_finite_canonical_inventory() {
        let root = tempfile::tempdir().unwrap();
        let valid = archive(root.path());
        valid.validate(root.path()).unwrap();
        for invalid in [
            ComponentReplayArchive {
                chain_id: 31337,
                ..valid.clone()
            },
            ComponentReplayArchive {
                chain_id: 12345,
                ..valid.clone()
            },
            ComponentReplayArchive {
                anchor_block_number: 4096,
                ..valid.clone()
            },
            ComponentReplayArchive {
                anchor_block_hash: format!("0x{}", "cd".repeat(32)),
                ..valid.clone()
            },
            ComponentReplayArchive {
                objects: vec![],
                ..valid.clone()
            },
            ComponentReplayArchive {
                objects: vec![valid.objects[0].clone(); 2],
                ..valid.clone()
            },
        ] {
            assert!(invalid.validate(root.path()).is_err());
        }
    }

    #[test]
    fn replay_manifest_rejects_aliases_and_changed_public_bytes() {
        let root = tempfile::tempdir().unwrap();
        let valid = archive(root.path());
        for path in ["replay/6565/0/alias", "replay/57001/00/alias", "../outside"] {
            let mut invalid = valid.clone();
            invalid.objects[0].path = path.into();
            assert!(invalid.validate(root.path()).is_err());
        }
        fs::write(root.path().join(&valid.objects[0].path), b"changed").unwrap();
        assert!(valid.validate(root.path()).is_err());
    }

    #[test]
    fn replay_manifest_rejects_symlinked_seed_objects() {
        let root = tempfile::tempdir().unwrap();
        let valid = archive(root.path());
        let path = root.path().join(&valid.objects[0].path);
        let target = root.path().join("public-copy");
        fs::rename(&path, &target).unwrap();
        std::os::unix::fs::symlink(target, path).unwrap();
        assert!(valid.validate(root.path()).is_err());
    }
}
