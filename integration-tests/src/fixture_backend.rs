//! SYSCOIN: Shared build/runtime fixture inventory boundary, not a snapshot restorer.
//! A valid inventory is never permission to start Core/NEVM or use Anvil APIs.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

pub const REGENERATION_MARKER: &str = "CANONICAL_V8_REGENERATION_REQUIRED";
pub const DESCRIPTOR_FILE: &str = "l1-backend.json";
const DESCRIPTOR_SCHEMA: u32 = 1;
const MAX_DESCRIPTOR_BYTES: u64 = 1024 * 1024;
const ROOT_CHAIN_ID: u64 = 31337;
const GATEWAY_CHAIN_ID: u64 = 57_001;
const EDGE_CHAIN_IDS: [u64; 2] = [6565, 6566];

pub type FixtureResult<T> = Result<T, String>;

/// Test purpose is separate from the protocol identity. Component fixtures never
/// satisfy the canonical Core/NEVM release gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureScope {
    CanonicalSyscoin,
    AnvilComponentOnly,
}

impl FixtureScope {
    pub const fn integration_default() -> Self {
        if cfg!(feature = "prover-tests") {
            Self::CanonicalSyscoin
        } else {
            Self::AnvilComponentOnly
        }
    }

    pub fn protocol_dir(self, workspace: &Path, version: &str) -> FixtureResult<PathBuf> {
        check_version(version)?;
        let base = workspace.join("local-chains");
        match self {
            Self::CanonicalSyscoin => Ok(base.join(version)),
            Self::AnvilComponentOnly if version == "v32.0" => {
                Ok(base.join("anvil-component-only").join(version))
            }
            Self::AnvilComponentOnly => Err("current component scope requires V32/V8".into()),
        }
    }
}

#[path = "fixture_component.rs"]
pub mod component;

// These are the two historical compressed-Anvil layouts before the V32 reset.
// Never extend this by numerical comparison or treat V32 as a legacy version.
const LEGACY_ANVIL_VERSIONS: &[&str] = &["v30.2", "v31.0"];

// SYSCOIN: Historical Anvil names remain506; the real V32 guest/host topology
// requires57001. This chooses a path only, never authorizes an absent fixture.
pub fn gateway_chain_id(version: &str) -> FixtureResult<u64> {
    check_version(version)?;
    Ok(if version == "v32.0" {
        GATEWAY_CHAIN_ID
    } else {
        506
    })
}

// Promotion must add an independently reviewed descriptor hash here. A sidecar
// beside an untrusted descriptor, or an environment override, is not a trust root.
const TRUSTED_DESCRIPTORS: &[(&str, &str)] = &[];

pub fn trusted_descriptor_hash(protocol_version: &str) -> Option<&'static str> {
    TRUSTED_DESCRIPTORS
        .iter()
        .find_map(|(version, hash)| (*version == protocol_version).then_some(*hash))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileIdentity {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ToolRole {
    Syscoind,
    SyscoinCli,
    SyscoinGeth,
    Server,
    FriProver,
    SnarkProver,
    RestoreSupervisor,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotRole {
    CoreConsensus,
    NevmDatabase,
    GatewayDatabase,
    Edge6565Database,
    Edge6566Database,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleArtifact<R> {
    pub role: R,
    pub file: FileIdentity,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainIds {
    pub root: u64,
    pub gateway: u64,
    pub edges: [u64; 2],
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FinalityPolicy {
    Confirmations { required_confirmations: u32 },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreNevmInventory {
    pub chain_ids: ChainIds,
    pub finality: FinalityPolicy,
    pub source_identities: FileIdentity,
    pub proof_identities: FileIdentity,
    pub deployment_record: FileIdentity,
    pub clean_boundary: FileIdentity,
    /// Exact bytes only; interpreting/restoring this schema is deliberately unsupported.
    pub snapshot_manifest: FileIdentity,
    pub snapshots: Vec<RoleArtifact<SnapshotRole>>,
    pub tools: Vec<RoleArtifact<ToolRole>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackendDescriptor {
    AnvilComponent {
        compressed_state: FileIdentity,
        decompressed_state: FileIdentity,
    },
    SyscoinCoreNevm {
        inventory: Box<CoreNevmInventory>,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureDescriptor {
    pub schema_version: u32,
    pub protocol_version: String,
    pub backend: BackendDescriptor,
}

#[derive(Debug, Clone)]
pub enum ValidatedFixtureInventory {
    /// Historical component-only compatibility; this is not released V32 evidence.
    LegacyAnvilComponent {
        compressed_state: PathBuf,
    },
    Descriptor(FixtureDescriptor),
    /// Explicit current component lane, not a canonical Syscoin fixture.
    AnvilComponentOnly(Box<component::ComponentDescriptor>),
}

pub fn check_regeneration_marker(root: &Path) -> FixtureResult<()> {
    if regeneration_marker_present(root)? {
        Err(format!("fixture is blocked by {REGENERATION_MARKER}"))
    } else {
        Ok(())
    }
}

pub fn regeneration_marker_present(root: &Path) -> FixtureResult<bool> {
    match fs::symlink_metadata(root.join(REGENERATION_MARKER)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("cannot check fixture regeneration marker: {error}")),
    }
}

fn check_anvil_output_paths(root: &Path) -> FixtureResult<()> {
    for name in ["l1-state.json", "l1-state.json.sha256"] {
        match fs::symlink_metadata(root.join(name)) {
            Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
                return Err("Anvil output must be a regular non-symlink path".into());
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("Anvil output metadata: {error}"));
            }
            _ => {}
        }
    }
    Ok(())
}

fn check_version(version: &str) -> FixtureResult<()> {
    let valid = version.strip_prefix('v').is_some_and(|numbers| {
        let parts: Vec<_> = numbers.split('.').collect();
        parts.len() == 2
            && parts
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    });
    if !valid {
        Err("invalid fixture protocol version".into())
    } else if version != "v32.0" && !LEGACY_ANVIL_VERSIONS.contains(&version) {
        Err("unknown fixture protocol version".into())
    } else {
        Ok(())
    }
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && digest.bytes().any(|b| b != b'0')
}

/// Check every component before opening a file; archives are not extracted here.
fn regular_file(root: &Path, relative: &str) -> FixtureResult<PathBuf> {
    let path = Path::new(relative);
    if relative.is_empty()
        || path.is_absolute()
        || relative.contains('\\')
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || relative
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == "..")
    {
        return Err("artifact path must be a normalized relative path".into());
    }
    let root_meta = fs::symlink_metadata(root).map_err(|e| format!("fixture directory: {e}"))?;
    if !root_meta.is_dir() || root_meta.file_type().is_symlink() {
        return Err("fixture directory must not be a symlink".into());
    }
    let mut current = root.to_path_buf();
    for component in path.components() {
        current.push(component);
        let metadata =
            fs::symlink_metadata(&current).map_err(|e| format!("artifact metadata: {e}"))?;
        if metadata.file_type().is_symlink() {
            return Err("artifact path contains a symlink".into());
        }
    }
    if !fs::metadata(&current).map_err(|e| e.to_string())?.is_file() {
        return Err("artifact must be a regular file".into());
    }
    Ok(current)
}

fn digest_file(path: &Path) -> FixtureResult<(u64, String)> {
    let mut file = fs::File::open(path).map_err(|e| format!("artifact open: {e}"))?;
    let mut buffer = [0; 64 * 1024];
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|e| format!("artifact read: {e}"))?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .ok_or("artifact size overflow")?;
        digest.update(&buffer[..count]);
    }
    Ok((size, format!("{:x}", digest.finalize())))
}

impl FileIdentity {
    fn validate_identity(&self) -> FixtureResult<()> {
        if self.size == 0 || !valid_digest(&self.sha256) {
            return Err("invalid artifact size/hash".into());
        }
        Ok(())
    }

    pub fn verify(&self, root: &Path) -> FixtureResult<PathBuf> {
        self.validate_identity()?;
        let path = regular_file(root, &self.path)?;
        let (size, hash) = digest_file(&path)?;
        if size != self.size || hash != self.sha256 {
            return Err(format!("artifact size/hash mismatch: {}", self.path));
        }
        Ok(path)
    }

    pub fn verify_bytes(&self, bytes: &[u8]) -> FixtureResult<()> {
        self.validate_identity()?;
        if bytes.len() as u64 != self.size || format!("{:x}", Sha256::digest(bytes)) != self.sha256
        {
            return Err("decompressed Anvil state size/hash mismatch".into());
        }
        Ok(())
    }
}

impl CoreNevmInventory {
    fn verify(&self, root: &Path) -> FixtureResult<()> {
        if self.chain_ids.root != ROOT_CHAIN_ID
            || self.chain_ids.gateway != GATEWAY_CHAIN_ID
            || self.chain_ids.edges != EDGE_CHAIN_IDS
        {
            return Err("real fixture must use the reviewed private chain IDs".into());
        }
        if !matches!(
            self.finality,
            FinalityPolicy::Confirmations {
                required_confirmations: 5
            }
        ) {
            return Err("real fixture requires explicit five-confirmation local policy".into());
        }
        let expected_tools = BTreeSet::from([
            ToolRole::Syscoind,
            ToolRole::SyscoinCli,
            ToolRole::SyscoinGeth,
            ToolRole::Server,
            ToolRole::FriProver,
            ToolRole::SnarkProver,
            ToolRole::RestoreSupervisor,
        ]);
        let expected_snapshots = BTreeSet::from([
            SnapshotRole::CoreConsensus,
            SnapshotRole::NevmDatabase,
            SnapshotRole::GatewayDatabase,
            SnapshotRole::Edge6565Database,
            SnapshotRole::Edge6566Database,
        ]);
        if self.tools.len() != expected_tools.len()
            || self
                .tools
                .iter()
                .map(|entry| entry.role)
                .collect::<BTreeSet<_>>()
                != expected_tools
            || self.snapshots.len() != expected_snapshots.len()
            || self
                .snapshots
                .iter()
                .map(|entry| entry.role)
                .collect::<BTreeSet<_>>()
                != expected_snapshots
        {
            return Err("missing, duplicate or unexpected tool/snapshot role".into());
        }
        let mut paths = BTreeSet::new();
        for artifact in [
            &self.source_identities,
            &self.proof_identities,
            &self.deployment_record,
            &self.clean_boundary,
            &self.snapshot_manifest,
        ]
        .into_iter()
        .chain(self.tools.iter().map(|entry| &entry.file))
        .chain(self.snapshots.iter().map(|entry| &entry.file))
        {
            if artifact.path == DESCRIPTOR_FILE
                || artifact.path == REGENERATION_MARKER
                || !paths.insert(&artifact.path)
            {
                return Err("duplicate or reserved artifact path".into());
            }
            artifact.verify(root)?;
        }
        Ok(())
    }
}

/// Inventory validation only. `trusted_hash` must come from outside the fixture.
pub fn load_fixture_inventory(
    root: &Path,
    version: &str,
    trusted_hash: Option<&str>,
) -> FixtureResult<ValidatedFixtureInventory> {
    check_regeneration_marker(root)?;
    check_version(version)?;
    match fs::symlink_metadata(root.join(DESCRIPTOR_FILE)) {
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && LEGACY_ANVIL_VERSIONS.contains(&version) =>
        {
            check_anvil_output_paths(root)?;
            return Ok(ValidatedFixtureInventory::LegacyAnvilComponent {
                compressed_state: regular_file(root, "l1-state.json.gz")?,
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err("missing required typed backend descriptor".into());
        }
        Err(error) => return Err(format!("backend descriptor metadata: {error}")),
        Ok(_) => {}
    }
    let trusted_hash = trusted_hash
        .filter(|hash| valid_digest(hash))
        .ok_or("missing trusted backend descriptor registry entry")?;
    let path = regular_file(root, DESCRIPTOR_FILE)?;
    if fs::metadata(&path).map_err(|e| e.to_string())?.len() > MAX_DESCRIPTOR_BYTES {
        return Err("backend descriptor exceeds size limit".into());
    }
    let bytes = fs::read(path).map_err(|e| format!("backend descriptor read: {e}"))?;
    if format!("{:x}", Sha256::digest(&bytes)) != trusted_hash {
        return Err("backend descriptor trusted hash mismatch".into());
    }
    let descriptor: FixtureDescriptor = serde_json::from_slice(&bytes)
        .map_err(|e| format!("invalid typed backend descriptor: {e}"))?;
    if descriptor.schema_version != DESCRIPTOR_SCHEMA || descriptor.protocol_version != version {
        return Err("backend descriptor schema/protocol mismatch".into());
    }
    match &descriptor.backend {
        BackendDescriptor::AnvilComponent {
            compressed_state,
            decompressed_state,
        } => {
            if version == "v32.0" {
                return Err(
                    "V32 canonical fixture cannot use the component-only Anvil backend".into(),
                );
            }
            check_anvil_output_paths(root)?;
            if compressed_state.path != "l1-state.json.gz"
                || decompressed_state.path != "l1-state.json"
            {
                return Err("Anvil component requires explicit Anvil state paths".into());
            }
            compressed_state.verify(root)?;
            decompressed_state.validate_identity()?;
        }
        BackendDescriptor::SyscoinCoreNevm { inventory } => inventory.verify(root)?,
    }
    Ok(ValidatedFixtureInventory::Descriptor(descriptor))
}

impl ValidatedFixtureInventory {
    pub fn anvil_state(&self, root: &Path) -> FixtureResult<(PathBuf, Option<&FileIdentity>)> {
        match self {
            Self::LegacyAnvilComponent { compressed_state } => Ok((compressed_state.clone(), None)),
            Self::AnvilComponentOnly(descriptor) => Ok((
                descriptor.compressed_state.verify(root)?,
                Some(&descriptor.decompressed_state),
            )),
            Self::Descriptor(FixtureDescriptor { backend: BackendDescriptor::AnvilComponent {
                compressed_state, decompressed_state }, .. }) =>
                Ok((compressed_state.verify(root)?, Some(decompressed_state))),
            Self::Descriptor(_) => Err("Core/NEVM is not Anvil state; real snapshot restore/runtime adapter is not implemented".into()),
        }
    }

    pub fn core_nevm_inventory(&self) -> FixtureResult<&CoreNevmInventory> {
        match self {
            Self::Descriptor(FixtureDescriptor {
                backend: BackendDescriptor::SyscoinCoreNevm { inventory },
                ..
            }) => Ok(inventory),
            _ => Err("component Anvil state is not a real Core/NEVM snapshot inventory".into()),
        }
    }
}

#[cfg(test)]
#[path = "fixture_backend_tests.rs"]
mod tests;
