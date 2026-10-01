//! SYSCOIN: Explicit, test-only witness export; never a canonical fixture or release attestation.

use super::*;
use anyhow::{bail, ensure};
use serde::Deserialize;
use sha2::Sha256;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use zksync_os_genesis::{GenesisInput, GenesisInputSource, build_genesis};

const SOURCE_PINS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/_patched-zksync-os-workspace.sh"
));

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportConfig {
    schema_version: u32,
    genesis_path: PathBuf,
    genesis_sha256: String,
    app_bin_path: PathBuf,
    app_bin_sha256: String,
    app_text_path: PathBuf,
    app_text_sha256: String,
    guest_source_tree: String,
    expected_security100_program_commitment: String,
    chain_id: u64,
    sl_chain_id: u64,
    pubdata_mode: String,
    compact_da_commit_target: String,
    output_dir: PathBuf,
}

#[derive(Debug)]
struct GenesisSnapshot(GenesisInput);

#[async_trait::async_trait]
impl GenesisInputSource for GenesisSnapshot {
    async fn genesis_input(&self) -> anyhow::Result<GenesisInput> {
        // Build from the exact hashed snapshot, not a second read of a mutable input path.
        Ok(self.0.clone())
    }
}

fn sha256(bytes: &[u8]) -> String {
    alloy::hex::encode(Sha256::digest(bytes))
}

fn require_hex(value: &str, digits: usize) -> anyhow::Result<()> {
    ensure!(
        value.len() == digits
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            && value.bytes().any(|byte| byte != b'0'),
        "expected nonzero lowercase {digits}-digit hex value"
    );
    Ok(())
}

fn reviewed_source_tree() -> anyhow::Result<&'static str> {
    let prefix = "SYSCOIN_EXPECTED_ZKSYNC_OS_PATCHED_TREE=\"";
    let values: Vec<_> = SOURCE_PINS
        .lines()
        .filter_map(|line| {
            line.strip_prefix(prefix)
                .and_then(|value| value.strip_suffix('"'))
        })
        .collect();
    ensure!(values.len() == 1, "ambiguous compiled source-tree pin");
    require_hex(values[0], 40)?;
    Ok(values[0])
}

fn repo_root() -> anyhow::Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .context("resolve test build workspace")
}

fn external_path(path: &Path, must_exist: bool) -> anyhow::Result<PathBuf> {
    ensure!(path.is_absolute(), "all witness paths must be absolute");
    let mut partial = PathBuf::new();
    let mut previous = None;
    for component in path.components() {
        match component {
            Component::RootDir => partial.push(component.as_os_str()),
            Component::Normal(name) => {
                ensure!(
                    !(previous == Some(std::ffi::OsStr::new("local-chains")) && name == "v32.0"),
                    "canonical v32.0 fixture paths are forbidden"
                );
                previous = Some(name);
                partial.push(name);
                match fs::symlink_metadata(&partial) {
                    Ok(metadata) => ensure!(
                        !metadata.file_type().is_symlink(),
                        "symlink paths are forbidden"
                    ),
                    Err(error)
                        if error.kind() == std::io::ErrorKind::NotFound
                            && !must_exist
                            && partial == path => {}
                    Err(error) => return Err(error.into()),
                }
            }
            _ => bail!("parent/relative path components are forbidden"),
        }
    }
    ensure!(
        !path.starts_with(repo_root()?),
        "witness paths must be outside the build checkout"
    );
    if let Some(source_root) = option_env!("ZKSYNC_OS_SERVER_PATH") {
        ensure!(
            !path.starts_with(Path::new(source_root).canonicalize()?),
            "witness paths must be outside the original server checkout"
        );
    }
    Ok(path.to_owned())
}

fn read_bound_file(path: &Path, expected_sha256: &str) -> anyhow::Result<Vec<u8>> {
    require_hex(expected_sha256, 64)?;
    let path = external_path(path, true)?;
    ensure!(
        fs::metadata(&path)?.is_file(),
        "input must be a regular file"
    );
    let bytes = fs::read(&path)?;
    ensure!(
        sha256(&bytes) == expected_sha256,
        "input SHA-256 mismatch for {}",
        path.display()
    );
    Ok(bytes)
}

impl ExportConfig {
    fn validate(&self) -> anyhow::Result<(PubdataMode, Address)> {
        ensure!(self.schema_version == 1, "unsupported export config schema");
        ensure!(
            self.chain_id != 0 && self.sl_chain_id != 0 && self.chain_id != self.sl_chain_id,
            "chain and settlement IDs must be nonzero and distinct"
        );
        zksync_os_native_pig::chain_config(self.chain_id)?;
        ensure!(
            self.guest_source_tree == reviewed_source_tree()?,
            "guest source tree differs from compiled wrapper pin"
        );
        require_hex(
            self.expected_security100_program_commitment
                .strip_prefix("0x")
                .context("program commitment must have 0x prefix")?,
            64,
        )?;
        for digest in [
            &self.genesis_sha256,
            &self.app_bin_sha256,
            &self.app_text_sha256,
        ] {
            require_hex(digest, 64)?;
        }
        require_hex(
            self.compact_da_commit_target
                .strip_prefix("0x")
                .context("compact target must have 0x prefix")?,
            40,
        )?;
        let target = Address::from_str(&self.compact_da_commit_target)?;
        ensure!(
            target == SYSCOIN_COMPACT_EDGE_DA_COMMIT_TARGET,
            "compact target differs from compiled guest binding"
        );
        let mode = match self.pubdata_mode.as_str() {
            "Blobs" => PubdataMode::Blobs,
            "RelayedL2Calldata" => PubdataMode::RelayedL2Calldata,
            _ => bail!("only Blobs and RelayedL2Calldata are supported"),
        };
        for path in [&self.genesis_path, &self.app_bin_path, &self.app_text_path] {
            external_path(path, true)?;
        }
        ensure!(
            self.genesis_path
                .extension()
                .is_some_and(|extension| extension == "json"),
            "final genesis must be JSON"
        );
        ensure!(
            self.app_bin_path
                .extension()
                .is_some_and(|extension| extension == "bin")
                && self.app_bin_path.with_extension("text") == self.app_text_path,
            "app .bin and .text must be paired siblings"
        );
        external_path(&self.output_dir, false)?;
        ensure!(
            !self.output_dir.try_exists()?,
            "output directory must be fresh; existing output is never overwritten"
        );
        Ok((mode, target))
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// A genuine one-block SetSLChainId smoke witness, not a throughput workload or release gate.
/// See docs/src/guides/v32_offline_witness.md. No test default may select deployment inputs.
#[tokio::test]
#[ignore = "opt-in native witness smoke export requires externally hash-bound final genesis and guest"]
async fn dump_v8_simplest_batch_prover_input() -> anyhow::Result<()> {
    export_native_batches(1).await
}

/// Two distinct contiguous batches for real aggregation: setup, then an empty block.
/// Empty means no transactions, not a fabricated state transition or copied proof.
#[tokio::test]
#[ignore = "opt-in sequential witness export requires externally hash-bound final genesis and guest"]
async fn dump_v8_two_batch_prover_inputs() -> anyhow::Result<()> {
    export_native_batches(2).await
}

async fn export_native_batches(batch_count: u64) -> anyhow::Result<()> {
    ensure!(
        matches!(batch_count, 1 | 2),
        "unsupported smoke batch count"
    );
    let config_path = PathBuf::from(
        std::env::var("V8_PROVER_INPUT_CONFIG")
            .context("set V8_PROVER_INPUT_CONFIG to an external absolute config path")?,
    );
    let config_hash = std::env::var("V8_PROVER_INPUT_CONFIG_SHA256")
        .context("set V8_PROVER_INPUT_CONFIG_SHA256 to the reviewed config digest")?;
    let config_bytes = read_bound_file(&config_path, &config_hash)?;
    let config: ExportConfig = serde_json::from_slice(&config_bytes)?;
    let (mode, target) = config.validate()?;
    read_bound_file(&config.app_bin_path, &config.app_bin_sha256)?;
    read_bound_file(&config.app_text_path, &config.app_text_sha256)?;
    let genesis_bytes = read_bound_file(&config.genesis_path, &config.genesis_sha256)?;
    let genesis = GenesisSnapshot(serde_json::from_slice(&genesis_bytes)?);
    ensure!(
        genesis.0.genesis_root != B256::ZERO,
        "final genesis root must be nonzero"
    );
    let protocol = ProtocolSemanticVersion::new(0, 32, 0);
    let genesis_state = build_genesis(&genesis, config.chain_id, &protocol).await?;
    let mut read_state = MemoryStateHistory::from_genesis_state(&genesis_state);
    read_state.block_range = 0..=0;
    let tree_dir = tempfile::tempdir()?;
    let tree = genesis_tree(&genesis_state, tree_dir.path());
    let mut previous_commitment = genesis_stored_batch_info(&genesis_state, &tree).state_commitment;
    let mut block_hashes = BlockHashes::default().push(genesis_state.header.hash());
    let mut sequence = Vec::new();
    let mut public_input_bytes = Vec::new();
    for batch_number in 1..=batch_count {
        let mut record = empty_replay_record(protocol.clone());
        record.block_context.chain_id = config.chain_id;
        record.block_context.block_number = batch_number;
        record.block_context.timestamp = batch_number;
        record.block_context.block_hashes = block_hashes;
        record.previous_block_timestamp = batch_number - 1;
        record.transactions = if batch_number == 1 {
            vec![ZkTransaction::from(SystemTxEnvelope::set_sl_chain_id(
                config.sl_chain_id,
                u64::MAX,
            ))]
        } else {
            vec![]
        };
        let block = executed_tree_block_from_record(&tree, &read_state, &genesis_state, record);
        let native = generate_batch_run(
            &[NativeBatchBlock {
                replay_record: &block.record,
                tree_data: &block.tree,
                block_output: &block.output,
            }],
            &read_state,
            tree.clone(),
            mode,
            target,
        )?;
        ensure!(
            native.previous_state_commitment == previous_commitment,
            "native batch state continuity mismatch"
        );
        let batch_info = native.build_batch_info(
            batch_number,
            batch_number,
            batch_number,
            mode,
            &protocol,
            config.chain_id,
            config.sl_chain_id,
        )?;
        ensure!(!native.prover_input.is_empty(), "native witness is empty");
        let bytes: Vec<_> = native
            .prover_input
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        let hex: String = native
            .prover_input
            .iter()
            .map(|word| format!("{word:08x}"))
            .collect();
        let registers: Vec<_> = native
            .batch_public_input_hash
            .as_slice()
            .chunks_exact(4)
            .map(|bytes| u32::from_le_bytes(bytes.try_into().expect("four-byte chunk")))
            .collect();
        let manifest = json!({
            "schema_version": 1,
            "app_bin_sha256": config.app_bin_sha256,
            "app_text_sha256": config.app_text_sha256,
            "prover_input_sha256": sha256(&bytes),
            "prover_input_words": native.prover_input.len(),
            "batch_id": batch_number,
            "expected_public_input_hash": native.batch_public_input_hash,
            "expected_security100_program_commitment": config.expected_security100_program_commitment,
            "context": {
                "purpose": if batch_count == 1 {
                    "single-SetSLChainId genuine proof smoke; not representative throughput"
                } else {
                    "two contiguous real native batches (SetSLChainId then empty block); aggregation smoke, not representative throughput"
                },
                "release_attestation": false,
                "config_sha256": config_hash,
                "guest_source_tree": config.guest_source_tree,
                "guest_binding": "reviewed wrapper source tree and compiled compact target; app hashes checked, program commitment supplied for independent prover verification",
                "genesis_sha256": config.genesis_sha256,
                "genesis_root": genesis_state.expected_genesis_root,
                "chain_id": config.chain_id,
                "sl_chain_id": config.sl_chain_id,
                "protocol_version": "v32.0",
                "execution_version": 7,
                "proving_version": 8,
                "security_bits": 100,
                "pubdata_mode": config.pubdata_mode,
                "compact_da_commit_target": target,
                "first_block_number": batch_number,
                "last_block_number": batch_number,
                "system_transaction_count": block.record.transactions.len(),
                "state_before": native.previous_state_commitment,
                "state_after": native.new_state_commitment,
                "chain_config_hash": zksync_os_native_pig::chain_config_hash(config.chain_id)?,
                "batch_output_hash": batch_info.batch_output_hash(),
                "expected_public_input_registers_le": registers,
                "da_commitment": native.da_commitment
            }
        });
        // Directory creation reserves a fresh run; on failure retain partial files for inspection.
        if batch_number == 1 {
            external_path(&config.output_dir, false)?;
            fs::create_dir(&config.output_dir)?;
        }
        let output_dir = if batch_count == 1 {
            config.output_dir.clone()
        } else {
            let directory = config.output_dir.join(format!("batch-{batch_number}"));
            fs::create_dir(&directory)?;
            directory
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
        write_new(&output_dir.join("v8_simplest_prover_input.le.bin"), &bytes)?;
        write_new(
            &output_dir.join("v8_simplest_prover_input.hex"),
            hex.as_bytes(),
        )?;
        write_new(&output_dir.join("manifest.json"), &manifest_bytes)?;
        fs::File::open(&output_dir)?.sync_all()?;
        sequence.push(json!({
            "batch_id": batch_number,
            "manifest_sha256": sha256(&manifest_bytes),
            "prover_input_sha256": sha256(&bytes),
            "public_input_hash": native.batch_public_input_hash,
            "state_before": native.previous_state_commitment,
            "state_after": native.new_state_commitment,
        }));
        public_input_bytes.extend_from_slice(native.batch_public_input_hash.as_slice());
        // Carry only the actual executed writes/preimages into the next native snapshot.
        // The Merkle tree's committed version was advanced by executed_tree_block_from_record.
        for write in &block.output.storage_writes {
            Arc::make_mut(&mut read_state.view.storage).insert(write.key, write.value);
        }
        for (hash, preimage) in &block.output.published_preimages {
            Arc::make_mut(&mut read_state.view.preimages).insert(*hash, preimage.clone());
        }
        read_state.block_range = batch_number..=batch_number;
        previous_commitment = native.new_state_commitment;
        block_hashes = block_hashes.push(block.output.header.hash());
        println!(
            "Exported genuine native/Merkle smoke witness to {}",
            output_dir.display()
        );
    }
    if batch_count == 2 {
        let manifest = json!({
            "schema_version": 1,
            "release_attestation": false,
            "purpose": "two contiguous native batches for real FRI aggregation smoke",
            "config_sha256": config_hash,
            "batches": sequence,
            "expected_combined_public_input_hash": keccak256(&public_input_bytes),
            "combined_hash_convention": "Keccak256(concatenated ordered 32-byte batch public-input hashes)",
        });
        write_new(
            &config.output_dir.join("sequence.json"),
            &serde_json::to_vec_pretty(&manifest)?,
        )?;
        fs::File::open(&config.output_dir)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod config_tests {
    use super::*;

    fn fixture(root: &Path) -> Value {
        for (name, bytes) in [
            ("genesis.json", b"{}".as_slice()),
            ("app.bin", b"bin"),
            ("app.text", b"text"),
        ] {
            fs::write(root.join(name), bytes).unwrap();
        }
        json!({
            "schema_version": 1,
            "genesis_path": root.join("genesis.json"), "genesis_sha256": sha256(b"{}"),
            "app_bin_path": root.join("app.bin"), "app_bin_sha256": sha256(b"bin"),
            "app_text_path": root.join("app.text"), "app_text_sha256": sha256(b"text"),
            "guest_source_tree": reviewed_source_tree().unwrap(),
            "expected_security100_program_commitment": format!("0x{}", "1".repeat(64)),
            "chain_id": 57057, "sl_chain_id": 57001, "pubdata_mode": "RelayedL2Calldata",
            "compact_da_commit_target": format!("{:#x}", SYSCOIN_COMPACT_EDGE_DA_COMMIT_TARGET),
            "output_dir": root.join("fresh-output")
        })
    }

    #[test]
    fn explicit_config_accepts_only_supported_bound_inputs() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let value = fixture(&root);
        let config: ExportConfig = serde_json::from_value(value.clone()).unwrap();
        config.validate().unwrap();
        for (field, invalid) in [
            ("schema_version", json!(2)),
            ("chain_id", json!(0)),
            ("sl_chain_id", json!(57057)),
            ("pubdata_mode", json!("Validium")),
            ("guest_source_tree", json!("f".repeat(40))),
            ("compact_da_commit_target", json!(Address::ZERO)),
            (
                "compact_da_commit_target",
                json!(format!("0x{}", "11".repeat(20))),
            ),
            ("genesis_sha256", json!("0".repeat(64))),
            ("expected_security100_program_commitment", json!("0x00")),
            ("genesis_path", json!("relative.json")),
            ("output_dir", json!(root)),
            ("app_text_path", json!(root.join("genesis.json"))),
        ] {
            let mut bad = value.clone();
            bad[field] = invalid;
            let config: ExportConfig = serde_json::from_value(bad).unwrap();
            assert!(config.validate().is_err(), "accepted invalid {field}");
        }
        let mut unknown = value;
        unknown["allow_canonical_fixture"] = json!(true);
        assert!(serde_json::from_value::<ExportConfig>(unknown).is_err());
    }

    #[test]
    fn missing_duplicate_and_out_of_range_parameters_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let value = fixture(&root);
        for field in value.as_object().unwrap().keys() {
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<ExportConfig>(missing).is_err(),
                "accepted missing {field}"
            );
        }
        let encoded = serde_json::to_string(&value).unwrap();
        let duplicate = encoded.replacen('{', "{\"chain_id\":1,", 1);
        assert!(serde_json::from_str::<ExportConfig>(&duplicate).is_err());
        for invalid in [json!(-1), json!(1.5), json!("57057")] {
            let mut bad = value.clone();
            bad["chain_id"] = invalid;
            assert!(serde_json::from_value::<ExportConfig>(bad).is_err());
        }
    }

    #[test]
    fn config_and_artifact_hashes_are_checked_without_overwrite() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let path = root.join("input");
        write_new(&path, b"original").unwrap();
        assert_eq!(
            read_bound_file(&path, &sha256(b"original")).unwrap(),
            b"original"
        );
        assert!(read_bound_file(&path, &sha256(b"other")).is_err());
        assert!(write_new(&path, b"replacement").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert!(external_path(&root.join("local-chains/v32.0/genesis.json"), false).is_err());
        assert!(external_path(&root.join("../outside"), false).is_err());
        assert!(external_path(&repo_root().unwrap().join("Cargo.toml"), true).is_err());
        if let Some(source_root) = option_env!("ZKSYNC_OS_SERVER_PATH") {
            assert!(external_path(&Path::new(source_root).join("Cargo.toml"), true).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_inputs_and_output_parents_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::write(root.join("input"), b"input").unwrap();
        std::os::unix::fs::symlink(root.join("input"), root.join("alias")).unwrap();
        std::os::unix::fs::symlink(&root, root.join("parent-alias")).unwrap();
        assert!(read_bound_file(&root.join("alias"), &sha256(b"input")).is_err());
        assert!(external_path(&root.join("parent-alias/output"), false).is_err());
    }
}
