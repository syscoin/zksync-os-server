use super::*;
use serde_json::{Value, json};
use tempfile::TempDir;

fn artifact(root: &Path, name: &str) -> Value {
    // Unit-only bytes; no generated descriptor or snapshot is published by these tests.
    let bytes = format!("unit-test-only:{name}");
    fs::write(root.join(name), &bytes).unwrap();
    json!({"path": name, "size": bytes.len(), "sha256": format!("{:x}", Sha256::digest(bytes.as_bytes()))})
}

fn real_descriptor(root: &Path) -> Value {
    let tools: Vec<_> = [
        "syscoind",
        "syscoin_cli",
        "syscoin_geth",
        "server",
        "fri_prover",
        "snark_prover",
        "restore_supervisor",
    ]
    .into_iter()
    .map(|role| json!({"role": role, "file": artifact(root, &format!("tool-{role}"))}))
    .collect();
    let snapshots: Vec<_> = [
        "core_consensus",
        "nevm_database",
        "gateway_database",
        "edge6565_database",
        "edge6566_database",
    ]
    .into_iter()
    .map(|role| json!({"role": role, "file": artifact(root, &format!("snapshot-{role}"))}))
    .collect();
    json!({"schema_version": 1, "protocol_version": "v32.0", "backend": {
        "kind": "syscoin_core_nevm", "inventory": {
            "chain_ids": {"root": 31337, "gateway": 57001, "edges": [6565,6566]},
            "finality": {"kind": "confirmations", "required_confirmations": 5},
            "source_identities": artifact(root, "source-identities.json"),
            "proof_identities": artifact(root, "proof-identities.json"),
            "deployment_record": artifact(root, "deployment-record.json"),
            "clean_boundary": artifact(root, "clean-boundary.json"),
            "snapshot_manifest": artifact(root, "snapshot-manifest.json"),
            "tools": tools, "snapshots": snapshots
        }
    }})
}

fn write_descriptor(root: &Path, value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap();
    fs::write(root.join(DESCRIPTOR_FILE), &bytes).unwrap();
    format!("{:x}", Sha256::digest(bytes))
}

fn load(root: &Path, value: &Value) -> FixtureResult<ValidatedFixtureInventory> {
    let hash = write_descriptor(root, value);
    load_fixture_inventory(root, "v32.0", Some(&hash))
}

#[test]
fn marker_precedes_version_descriptor_and_trust() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join(REGENERATION_MARKER), "blocked").unwrap();
    fs::write(root.path().join(DESCRIPTOR_FILE), "not json").unwrap();
    let error = load_fixture_inventory(root.path(), "../unknown", None).unwrap_err();
    assert!(error.contains(REGENERATION_MARKER));
}

#[test]
fn missing_descriptor_and_unknown_version_fail_closed() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("l1-state.json.gz"), "not an escape hatch").unwrap();
    assert!(
        load_fixture_inventory(root.path(), "v32.0", None)
            .unwrap_err()
            .contains("missing required")
    );
    assert!(
        load_fixture_inventory(root.path(), "v999.0", None)
            .unwrap_err()
            .contains("unknown fixture")
    );
    assert!(
        load_fixture_inventory(root.path(), "../v31.0", None)
            .unwrap_err()
            .contains("invalid fixture")
    );
}

#[test]
fn actual_legacy_mapping_is_explicit_and_component_only() {
    let root = TempDir::new().unwrap();
    fs::write(
        root.path().join("l1-state.json.gz"),
        "legacy component test bytes",
    )
    .unwrap();
    for version in ["v30.2", "v31.0"] {
        assert_eq!(gateway_chain_id(version).unwrap(), 506);
        let inventory = load_fixture_inventory(root.path(), version, None).unwrap();
        assert!(matches!(
            inventory,
            ValidatedFixtureInventory::LegacyAnvilComponent { .. }
        ));
        assert!(inventory.anvil_state(root.path()).is_ok());
        assert!(inventory.core_nevm_inventory().is_err());
    }
}

#[test]
fn v32_gateway_matches_guest_and_legacy506_is_rejected() {
    assert_eq!(gateway_chain_id("v32.0").unwrap(), 57_001);
    for unknown in ["v29.0", "v32.1", "v999.0", "../v32.0"] {
        assert!(gateway_chain_id(unknown).is_err());
    }
    let root = TempDir::new().unwrap();
    let mut descriptor = real_descriptor(root.path());
    let inventory = load(root.path(), &descriptor).unwrap();
    assert_eq!(
        inventory.core_nevm_inventory().unwrap().chain_ids.gateway,
        57_001
    );
    assert!(inventory.anvil_state(root.path()).is_err());
    descriptor["backend"]["inventory"]["chain_ids"]["gateway"] = json!(506);
    assert!(
        load(root.path(), &descriptor)
            .unwrap_err()
            .contains("reviewed private chain IDs")
    );
    assert!(trusted_descriptor_hash("v32.0").is_none());
}

#[test]
fn missing_registry_and_modified_descriptor_rejected() {
    let root = TempDir::new().unwrap();
    let mut descriptor = real_descriptor(root.path());
    let hash = write_descriptor(root.path(), &descriptor);
    assert!(trusted_descriptor_hash("v32.0").is_none());
    assert!(
        load_fixture_inventory(root.path(), "v32.0", None)
            .unwrap_err()
            .contains("registry")
    );
    descriptor["schema_version"] = json!(2);
    write_descriptor(root.path(), &descriptor);
    assert!(
        load_fixture_inventory(root.path(), "v32.0", Some(&hash))
            .unwrap_err()
            .contains("trusted hash")
    );
}

#[test]
fn real_inventory_validates_without_anvil_or_restore_fallback() {
    let root = TempDir::new().unwrap();
    let inventory = load(root.path(), &real_descriptor(root.path())).unwrap();
    assert_eq!(inventory.core_nevm_inventory().unwrap().snapshots.len(), 5);
    assert!(
        inventory
            .anvil_state(root.path())
            .unwrap_err()
            .contains("not implemented")
    );
    assert!(!root.path().join("l1-state.json").exists());
    assert!(!root.path().join("l1-state.json.gz").exists());
}

#[test]
fn schema_protocol_tag_and_unknown_fields_rejected() {
    let root = TempDir::new().unwrap();
    let descriptor = real_descriptor(root.path());
    for (pointer, value) in [
        ("/schema_version", json!(2)),
        ("/protocol_version", json!("v31.0")),
        ("/backend/kind", json!("anvil")),
        ("/backend/inventory/chain_ids/root", json!(5700)),
        (
            "/backend/inventory/finality/required_confirmations",
            json!(0),
        ),
    ] {
        let mut invalid = descriptor.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        assert!(load(root.path(), &invalid).is_err(), "accepted {pointer}");
    }
    let mut invalid = descriptor;
    invalid["backend"]["inventory"]["allow_fake_proofs"] = json!(true);
    assert!(load(root.path(), &invalid).is_err());
    invalid["backend"].as_object_mut().unwrap().remove("kind");
    assert!(load(root.path(), &invalid).is_err());
}

#[test]
fn missing_duplicate_and_unknown_roles_rejected() {
    let root = TempDir::new().unwrap();
    let descriptor = real_descriptor(root.path());
    for collection in ["tools", "snapshots"] {
        let mut missing = descriptor.clone();
        missing["backend"]["inventory"][collection]
            .as_array_mut()
            .unwrap()
            .pop();
        assert!(load(root.path(), &missing).unwrap_err().contains("role"));
        let mut duplicate = descriptor.clone();
        duplicate["backend"]["inventory"][collection][1]["role"] =
            duplicate["backend"]["inventory"][collection][0]["role"].clone();
        assert!(load(root.path(), &duplicate).unwrap_err().contains("role"));
        let mut unknown = descriptor.clone();
        unknown["backend"]["inventory"][collection][0]["role"] = json!("unreviewed");
        assert!(load(root.path(), &unknown).is_err());
    }
}

#[test]
fn inventory_size_hash_and_duplicate_paths_rejected() {
    let root = TempDir::new().unwrap();
    let descriptor = real_descriptor(root.path());
    for (field, value) in [
        ("size", json!(1)),
        ("sha256", json!("a".repeat(64))),
        ("sha256", json!("0".repeat(64))),
        ("sha256", json!("A".repeat(64))),
    ] {
        let mut invalid = descriptor.clone();
        invalid["backend"]["inventory"]["tools"][0]["file"][field] = value;
        assert!(load(root.path(), &invalid).is_err());
    }
    let mut duplicate = descriptor;
    duplicate["backend"]["inventory"]["tools"][1]["file"] =
        duplicate["backend"]["inventory"]["tools"][0]["file"].clone();
    assert!(
        load(root.path(), &duplicate)
            .unwrap_err()
            .contains("duplicate")
    );
}

#[test]
fn traversal_absolute_and_ambiguous_paths_rejected() {
    let root = TempDir::new().unwrap();
    let descriptor = real_descriptor(root.path());
    for path in [
        "../secret",
        "/etc/passwd",
        "nested/../secret",
        "./tool-syscoind",
        "x//y",
        "x\\y",
    ] {
        let mut invalid = descriptor.clone();
        invalid["backend"]["inventory"]["tools"][0]["file"]["path"] = json!(path);
        assert!(
            load(root.path(), &invalid)
                .unwrap_err()
                .contains("normalized relative")
        );
    }
}

#[cfg(unix)]
#[test]
fn symlinked_artifact_directory_descriptor_and_outputs_rejected() {
    use std::os::unix::fs::symlink;
    let root = TempDir::new().unwrap();
    let descriptor = real_descriptor(root.path());
    fs::remove_file(root.path().join("tool-syscoind")).unwrap();
    symlink("tool-syscoin_cli", root.path().join("tool-syscoind")).unwrap();
    assert!(
        load(root.path(), &descriptor)
            .unwrap_err()
            .contains("symlink")
    );
    let hash = write_descriptor(root.path(), &descriptor);
    fs::rename(
        root.path().join(DESCRIPTOR_FILE),
        root.path().join("descriptor-target"),
    )
    .unwrap();
    symlink("descriptor-target", root.path().join(DESCRIPTOR_FILE)).unwrap();
    assert!(
        load_fixture_inventory(root.path(), "v32.0", Some(&hash))
            .unwrap_err()
            .contains("symlink")
    );
    let legacy = TempDir::new().unwrap();
    fs::write(legacy.path().join("l1-state.json.gz"), "legacy").unwrap();
    symlink("l1-state.json.gz", legacy.path().join("l1-state.json")).unwrap();
    assert!(
        load_fixture_inventory(legacy.path(), "v31.0", None)
            .unwrap_err()
            .contains("symlink")
    );
    let link = root.path().join("linked-root");
    symlink(legacy.path(), &link).unwrap();
    assert!(load_fixture_inventory(&link, "v31.0", None).is_err());
}

#[test]
fn typed_anvil_is_legacy_component_only_and_checks_decoded_identity() {
    let root = TempDir::new().unwrap();
    let mut descriptor = json!({"schema_version": 1, "protocol_version": "v31.0",
        "backend": {"kind": "anvil_component",
            "compressed_state": artifact(root.path(), "l1-state.json.gz"),
            "decompressed_state": artifact(root.path(), "l1-state.json")}});
    let hash = write_descriptor(root.path(), &descriptor);
    let inventory = load_fixture_inventory(root.path(), "v31.0", Some(&hash)).unwrap();
    let (_, decoded) = inventory.anvil_state(root.path()).unwrap();
    decoded.unwrap().verify(root.path()).unwrap();
    assert!(decoded.unwrap().verify_bytes(b"tampered").is_err());
    descriptor["protocol_version"] = json!("v32.0");
    assert!(
        load(root.path(), &descriptor)
            .unwrap_err()
            .contains("component-only")
    );
}
