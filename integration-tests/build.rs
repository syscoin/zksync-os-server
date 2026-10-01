use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::str::from_utf8;

#[allow(dead_code)]
#[path = "src/fixture_backend.rs"]
mod fixture_backend;

/// Decompress `l1-state.json.gz` files at build time so every test process can
/// read the plain JSON without paying the ~70 MB decompression cost at runtime.
///
/// A `.sha256` sidecar file stores the hash of the `.gz` input; the
/// decompressed output is only regenerated when the hash changes.
fn decompress_l1_states() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let workspace_root = Path::new(&manifest_dir).parent().unwrap();
    let local_chains = workspace_root.join("local-chains");

    // Re-run when a version directory is added/removed or any .gz file changes.
    println!("cargo::rerun-if-changed={}", local_chains.display());

    let Ok(entries) = std::fs::read_dir(&local_chains) else {
        return;
    };

    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }

        // SYSCOIN: The marker takes precedence over versions, descriptors and inventories.
        if fixture_backend::regeneration_marker_present(&entry.path())
            .unwrap_or_else(|error| panic!("cannot inspect fixture marker: {error}"))
        {
            println!(
                "cargo::warning=skipping blocked local-chain fixture at {}",
                entry.path().display(),
            );
            continue;
        }

        if !entry.path().join("versions.yaml").is_file() {
            // Ignore materializations without a declared protocol fixture.
            continue;
        }
        let version = entry.file_name();
        let version = version.to_str().expect("fixture version must be UTF-8");
        let inventory = fixture_backend::load_fixture_inventory(
            &entry.path(),
            version,
            fixture_backend::trusted_descriptor_hash(version),
        )
        .unwrap_or_else(|e| panic!("invalid fixture {}: {e}", entry.path().display()));
        if inventory.core_nevm_inventory().is_ok() {
            // Validation is not extraction: real snapshots are never passed to GzDecoder.
            println!(
                "cargo::warning=Core/NEVM inventory validated; runtime restore is unsupported"
            );
            continue;
        }
        let (gz_path, decoded_identity) = inventory
            .anvil_state(&entry.path())
            .unwrap_or_else(|e| panic!("invalid Anvil component fixture: {e}"));

        let compressed = std::fs::read(&gz_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", gz_path.display()));

        let hash = Sha256::digest(&compressed);
        let hex_hash = format!("{hash:x}");

        // l1-state.json.gz → l1-state.json (with_extension strips last extension)
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
                    .verify(&entry.path())
                    .expect("cached Anvil state identity mismatch");
            }
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
