use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;
use std::str::from_utf8;

#[allow(dead_code)]
#[path = "src/fixture_backend.rs"]
mod fixture_backend;

/// Authenticate a bounded decode before replacing the generated cache file.
fn decode_component_state(
    compressed: &[u8],
    identity: &fixture_backend::FileIdentity,
    output: &Path,
) -> std::io::Result<()> {
    let invalid = || std::io::Error::other("decoded Anvil component state identity mismatch");
    if identity.size == 0 || identity.size > 2 * 1024 * 1024 * 1024 {
        return Err(invalid());
    }
    let mut decoder = zstd::stream::read::Decoder::new(compressed)?;
    // The measured package uses --long=27; do not permit a larger decode window.
    decoder.window_log_max(27)?;
    let mut decoder = decoder.take(identity.size + 1);
    let mut temporary = tempfile::NamedTempFile::new_in(output.parent().unwrap())?;
    let mut buffer = [0; 64 * 1024];
    let mut size = 0_u64;
    let mut digest = Sha256::new();
    loop {
        let count = decoder.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        if size > identity.size {
            return Err(invalid());
        }
        digest.update(&buffer[..count]);
        temporary.write_all(&buffer[..count])?;
    }
    if size != identity.size || format!("{:x}", digest.finalize()) != identity.sha256 {
        return Err(invalid());
    }
    temporary.as_file().sync_all()?;
    temporary.persist(output).map_err(|error| error.error)?;
    Ok(())
}

/// Decode authenticated component zstd states and historical gzip states once
/// at build time rather than in every test process. The sidecar tracks the
/// compressed input; registered cached outputs are also authenticated directly.
fn decompress_l1_states() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let workspace_root = Path::new(&manifest_dir).parent().unwrap();
    let local_chains = workspace_root.join("local-chains");

    // Re-run when a version directory is added/removed or compressed state changes.
    println!("cargo::rerun-if-changed={}", local_chains.display());

    let Ok(entries) = std::fs::read_dir(&local_chains) else {
        return;
    };

    let mut fixtures = Vec::new();
    for entry in entries.flatten() {
        if entry.file_name() == "anvil-component-only" || !entry.path().is_dir() {
            continue;
        }
        fixtures.push((
            entry.path(),
            entry.file_name(),
            fixture_backend::FixtureScope::CanonicalSyscoin,
        ));
    }
    // Explicit current component namespace; do not infer a protocol from its parent name.
    let component = local_chains.join("anvil-component-only/v32.0");
    if component.exists() {
        fixtures.push((
            component,
            "v32.0".into(),
            fixture_backend::FixtureScope::AnvilComponentOnly,
        ));
    }

    for (root, version, scope) in fixtures {
        // SYSCOIN: The marker takes precedence over versions, descriptors and inventories.
        if fixture_backend::regeneration_marker_present(&root)
            .unwrap_or_else(|error| panic!("cannot inspect fixture marker: {error}"))
        {
            println!(
                "cargo::warning=skipping blocked local-chain fixture at {}",
                root.display(),
            );
            continue;
        }

        if !root.join("versions.yaml").is_file() {
            // Ignore materializations without a declared protocol fixture.
            continue;
        }
        let version = version.to_str().expect("fixture version must be UTF-8");
        let inventory = match scope {
            fixture_backend::FixtureScope::CanonicalSyscoin => {
                fixture_backend::load_fixture_inventory(
                    &root,
                    version,
                    fixture_backend::trusted_descriptor_hash(version),
                )
            }
            fixture_backend::FixtureScope::AnvilComponentOnly => {
                fixture_backend::component::registered_hash(version)
                    .and_then(|hash| fixture_backend::component::load(&root, version, &hash))
            }
        }
        .unwrap_or_else(|e| panic!("invalid fixture {}: {e}", root.display()));
        if inventory.core_nevm_inventory().is_ok() {
            // Validation is not extraction: real snapshots are never passed to GzDecoder.
            println!(
                "cargo::warning=Core/NEVM inventory validated; runtime restore is unsupported"
            );
            continue;
        }
        let (gz_path, decoded_identity) = inventory
            .anvil_state(&root)
            .unwrap_or_else(|e| panic!("invalid Anvil component fixture: {e}"));

        let compressed = std::fs::read(&gz_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", gz_path.display()));

        let hash = Sha256::digest(&compressed);
        let hex_hash = format!("{hash:x}");

        // Both registered .zst and legacy .gz strip to the same decoded filename.
        let json_path = gz_path.with_extension("");
        let hash_path = json_path.with_extension("json.sha256");

        // Skip decompression if the output exists and the hash file matches.
        if json_path.is_file()
            && let Ok(existing_hash) = std::fs::read_to_string(&hash_path)
            && existing_hash.trim() == hex_hash
        {
            // A cached output is not authenticated by the compressed-input sidecar.
            if let Some(identity) = decoded_identity {
                identity
                    .verify(&root)
                    .expect("cached Anvil state identity mismatch");
            }
            continue;
        }

        if scope == fixture_backend::FixtureScope::AnvilComponentOnly {
            let identity = decoded_identity.expect("component state requires decoded identity");
            decode_component_state(&compressed, identity, &json_path)
                .unwrap_or_else(|e| panic!("failed to decode {}: {e}", gz_path.display()));
            std::fs::write(&hash_path, &hex_hash)
                .unwrap_or_else(|e| panic!("failed to write {}: {e}", hash_path.display()));
            continue;
        }

        let mut decoder = GzDecoder::new(compressed.as_slice());
        let mut decoded = Vec::new();
        decoder
            .read_to_end(&mut decoded)
            .unwrap_or_else(|e| panic!("failed to decompress {}: {e}", gz_path.display()));
        if let Some(identity) = decoded_identity {
            identity
                .verify_bytes(&decoded)
                .expect("decoded Anvil state identity mismatch");
        }

        std::fs::write(&json_path, &decoded)
            .unwrap_or_else(|e| panic!("failed to write {}: {e}", json_path.display()));
        std::fs::write(&hash_path, &hex_hash)
            .unwrap_or_else(|e| panic!("failed to write {}: {e}", hash_path.display()));
    }
}

fn main() {
    decompress_l1_states();

    // Rerun build script when test contracts change.
    println!("cargo::rerun-if-changed=test-contracts/src");
    println!("cargo::rerun-if-changed=test-contracts/foundry.toml");

    // Check that `forge` is installed and is executable
    let Ok(status) = Command::new("forge").arg("--version").status() else {
        println!("cargo::warning=`forge` not found, skipping build script");
        println!("cargo::warning=visit https://getfoundry.sh/ for installation instructions");
        return;
    };
    if !status.success() {
        println!("cargo::warning=could not run `forge --version`, skipping build script");
        println!("cargo::warning=make sure your foundry installation is working correctly");
        return;
    }

    match Command::new("forge")
        .arg("build")
        .arg("--root")
        .arg("test-contracts")
        // SYSCOIN: The Rust crate embeds artifacts from src/ only. Solidity
        // tests have deployment-only Era deps and run separately with Forge.
        .arg("src")
        .output()
    {
        Ok(output) if output.status.success() => {
            // Success, do nothing
        }
        Ok(output) => {
            println!("cargo::error=`forge build` failed, see stdout/stderr below");
            println!("cargo::error=stdout={}", from_utf8(&output.stdout).unwrap());
            println!("cargo::error=stderr={}", from_utf8(&output.stderr).unwrap());
        }
        Err(err) => {
            println!("cargo::error=could not run `forge build`: {err}");
        }
    }
}
#[cfg(test)]
mod component_decode_tests {
    use super::*;

    fn identity(bytes: &[u8]) -> fixture_backend::FileIdentity {
        serde_json::from_value(
            serde_json::json!({"path": "l1-state.json", "size": bytes.len(),
            "sha256": format!("{:x}", Sha256::digest(bytes))}),
        )
        .unwrap()
    }

    #[test]
    fn component_zstd_decode_preserves_exact_authenticated_bytes() {
        let bytes = b"{\"block\":{\"timestamp\":\"1\"},\"historical_states\":[]}\n";
        let compressed = zstd::stream::encode_all(bytes.as_slice(), 3).unwrap();
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("l1-state.json");
        decode_component_state(&compressed, &identity(bytes), &output).unwrap();
        assert_eq!(std::fs::read(output).unwrap(), bytes);
    }

    #[test]
    fn component_zstd_decode_never_publishes_wrong_size_hash_or_unbounded_state() {
        let bytes = b"unchanged unit-only state";
        let compressed = zstd::stream::encode_all(bytes.as_slice(), 3).unwrap();
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("l1-state.json");
        std::fs::write(&output, b"previous authenticated cache").unwrap();
        for (size, hash) in [
            (bytes.len() as u64 - 1, identity(bytes).sha256),
            (bytes.len() as u64 + 1, identity(bytes).sha256),
            (bytes.len() as u64, "ab".repeat(32)),
            (2 * 1024 * 1024 * 1024 + 1, identity(bytes).sha256),
        ] {
            let mut expected = identity(bytes);
            expected.size = size;
            expected.sha256 = hash;
            assert!(decode_component_state(&compressed, &expected, &output).is_err());
            assert_eq!(
                std::fs::read(&output).unwrap(),
                b"previous authenticated cache"
            );
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn component_zstd_decode_rejects_corrupt_and_truncated_frames() {
        let bytes = b"unit-only state";
        let compressed = zstd::stream::encode_all(bytes.as_slice(), 3).unwrap();
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("l1-state.json");
        for invalid in [&compressed[..compressed.len() - 1], b"not zstd".as_slice()] {
            assert!(decode_component_state(invalid, &identity(bytes), &output).is_err());
            assert!(!output.exists());
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
