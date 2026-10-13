"""Synthetic fixtures and optional read-only bundle checks; no activation or build."""
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import types
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("era_release", ROOT / "scripts/apply-era-contracts-syscoin-release.py")
M = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(M)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def put(path, raw):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(raw)
    return {"path": path.name, "size": len(raw), "sha256": sha(raw)}


class ActivationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.server = Path(self.tmp.name).resolve()
        self.fixture = self.server / "local-chains/v32.0"
        self.fixture.mkdir(parents=True)
        self.inv = {"chain_ids": {"root": 31337, "gateway": 57001, "edges": [6565, 6566]},
                    "finality": {"kind": "confirmations", "required_confirmations": 5}}
        identities = {}
        for name in M.IDENTITIES:
            self.inv[name] = put(self.fixture / (name + ".json"), (name + "\n").encode())
            identities[name] = self.inv[name]["sha256"]
        for key, roles in (("snapshots", M.SNAPSHOTS), ("tools", M.TOOLS)):
            self.inv[key] = [{"role": role, "file": put(self.fixture / (role + ".bin"), role.encode())}
                             for role in sorted(roles)]
        self.data = {"schema_version": 1, "protocol_version": "v32.0",
                     "backend": {"kind": "syscoin_core_nevm", "inventory": self.inv}}
        versions = put(self.fixture / "versions.yaml", b"synthetic: test-only\n")
        self.binding = {"descriptor_sha256": "", "versions_sha256": versions["sha256"],
                        "identity_sha256": identities}
        self.refresh()
        self.app = {"lib/types/src/protocol/proving_version.rs": b"synthetic final identity"}
        for rel, content in self.app.items():
            put(self.server / rel, content)
        self.addCleanup(patch.stopall)
        patch.object(M, "CANONICAL_BINDING", self.binding).start()
        patch.object(M, "APP_SOURCES", {k: sha(v) for k, v in self.app.items()}).start()

    def refresh(self):
        raw = json.dumps(self.data).encode()
        put(self.fixture / "l1-backend.json", raw)
        self.binding["descriptor_sha256"] = sha(raw)
        text = 'const TRUSTED_DESCRIPTORS: &[(&str, &str)] = &[("v32.0", "' + sha(raw) + '")];'
        put(self.server / "integration-tests/src/fixture_backend.rs", text.encode())

    def test_complete_synthetic_binding(self):
        self.assertEqual(M.check_activation(self.server), self.binding["descriptor_sha256"])

    def test_marker_first_even_dangling_symlink(self):
        marker = self.fixture / "CANONICAL_V8_REGENERATION_REQUIRED"
        marker.symlink_to("not-present")
        with patch.object(M, "CANONICAL_BINDING", None), self.assertRaisesRegex(ValueError, "marker"):
            M.check_activation(self.server)

    def test_removed_marker_does_not_activate_binding(self):
        with patch.object(M, "CANONICAL_BINDING", None), self.assertRaisesRegex(ValueError, "not activated"):
            M.check_activation(self.server)

    def test_empty_or_different_registry_rejected(self):
        for body in ("", '("v32.0", "' + "0" * 64 + '")'):
            put(self.server / "integration-tests/src/fixture_backend.rs",
                ('const TRUSTED_DESCRIPTORS: &[(&str, &str)] = &[' + body + '];').encode())
            with self.assertRaisesRegex(ValueError, "consumer registry"):
                M.check_activation(self.server)

    def test_duplicate_registry_entry_rejected(self):
        path = self.server / "integration-tests/src/fixture_backend.rs"
        text = path.read_text().replace(")];", '), ("v32.0", "' + self.binding["descriptor_sha256"] + '")];')
        path.write_text(text)
        with self.assertRaisesRegex(ValueError, "consumer registry"):
            M.check_activation(self.server)

    def test_descriptor_tamper(self):
        with (self.fixture / "l1-backend.json").open("ab") as output:
            output.write(b" ")
        with self.assertRaisesRegex(ValueError, "SHA-256"):
            M.check_activation(self.server)

    def test_snapshot_tamper_and_absence(self):
        path = self.fixture / self.inv["snapshots"][0]["file"]["path"]
        raw = path.read_bytes()
        path.write_bytes(b"x" * len(raw))
        with self.assertRaisesRegex(ValueError, "SHA-256"):
            M.check_activation(self.server)
        path.unlink()
        with self.assertRaises(OSError):
            M.check_activation(self.server)

    def test_foreign_topology_and_anvil(self):
        self.inv["chain_ids"]["gateway"] = 506
        self.refresh()
        with self.assertRaisesRegex(ValueError, "topology"):
            M.check_activation(self.server)
        self.inv["chain_ids"]["gateway"] = 57001
        self.data["backend"]["kind"] = "anvil_component"
        self.refresh()
        with self.assertRaisesRegex(ValueError, "Core/NEVM"):
            M.check_activation(self.server)

    def test_missing_role_duplicate_path_and_identity_drift(self):
        original = copy.deepcopy(self.inv)
        self.inv["tools"].pop()
        self.refresh()
        with self.assertRaisesRegex(ValueError, "role inventory"):
            M.check_activation(self.server)
        self.inv.clear(); self.inv.update(copy.deepcopy(original))
        self.inv["tools"][0]["file"] = self.inv["tools"][1]["file"]
        self.refresh()
        with self.assertRaisesRegex(ValueError, "duplicate fixture"):
            M.check_activation(self.server)
        self.inv.clear(); self.inv.update(original)
        self.inv["proof_identities"]["sha256"] = "0" * 64
        self.refresh()
        with self.assertRaisesRegex(ValueError, "identity record"):
            M.check_activation(self.server)

    def test_path_traversal_and_symlink(self):
        row = self.inv["tools"][0]["file"]
        original = row["path"]
        row["path"] = "../outside"
        self.refresh()
        with self.assertRaisesRegex(ValueError, "relative path"):
            M.check_activation(self.server)
        row["path"] = original
        path = self.fixture / original
        raw = path.read_bytes(); path.unlink()
        put(self.server / "outside", raw)
        path.symlink_to(self.server / "outside")
        self.refresh()
        with self.assertRaisesRegex(ValueError, "symlink"):
            M.check_activation(self.server)

    def test_stale_server_identity(self):
        put(self.server / next(iter(self.app)), b"stock or zero identity")
        with self.assertRaisesRegex(ValueError, "SHA-256"):
            M.check_activation(self.server)


class BundleAndLauncherTests(unittest.TestCase):
    def test_canonical_query_candidate_records_only_completed_gates(self):
        module, manifest = M.load_bundle()
        generation = manifest["contract_generation_provenance"]
        evidence = manifest["proof_qualification"]
        self.assertEqual(generation["candidate_tree"], module.CANDIDATE)
        self.assertEqual(generation["previous_candidate_tree"], "d194956535ddeded60829706e9301e9adbe6cb5b")
        self.assertEqual(generation["inventory_rows"], 278)
        self.assertEqual(generation["unchanged_inventory_rows"], 276)
        self.assertEqual(set(generation["changed_inventory_rows"]), module.ROWS)
        self.assertTrue(generation["full_contract_hash_recomputation_passed"])
        self.assertTrue(generation["normal_l1_evm_and_eravm_build_passed"])
        self.assertEqual(evidence["verification_key_hash"], manifest["verification_key_hash"])
        self.assertEqual(evidence["crypto_commit"], "5eafa1f914519d88a13c577a9e8cc5c7db1dfc77")
        self.assertEqual(evidence["crypto_tree"], "c52391d0df82126d27e5c76810e9df85c61875d0")
        self.assertEqual(evidence["native_wrapper_stages_verified"], 2)
        self.assertEqual(evidence["independent_cpu_verifications_passed"], 2)
        self.assertTrue(evidence["retained_risc_stage_reused"])
        self.assertTrue(evidence["real_proof_verified"])
        self.assertTrue(evidence["real_evm_verified"])
        self.assertEqual(evidence["real_evm_controls_passed"], 2)
        self.assertEqual(evidence["estimated_verifier_gas"], 414006)
        self.assertEqual(evidence["result_sha256"]["compression_snark"],
                         "2bd6abd4d98715ffaa34727f30f4d8400e41a99118c51e5048c8c20723caa89e")
        self.assertEqual(evidence["result_sha256"]["evm"],
                         "e4b0de9143c620f13a2fc3b57128272b84afe899707e2253ae389a16ff49ac55")
        self.assertEqual(evidence["status"],
                         "completed_offline_compression_snark_normal_evm_cpu_keys_loaders_and_gpu_builds_release_pending")
        self.assertFalse(evidence["independent_cpu_keygen_pending"])
        self.assertTrue(evidence["cpu_gpu_setup_keys_byte_identical"])
        cpu_keys = evidence["independent_cpu_keygen"]
        self.assertEqual(cpu_keys["exit_code"], 0)
        self.assertTrue(cpu_keys["all_three_keys_match_byte_for_byte"])
        self.assertTrue(cpu_keys["risc_key_unchanged"])
        self.assertEqual(cpu_keys["risc_vk_sha256"], evidence["retained_risc_vk_sha256"])
        self.assertEqual(cpu_keys["compression_vk_sha256"], evidence["native_compression_vk_sha256"])
        self.assertEqual(cpu_keys["snark_vk_sha256"], evidence["native_snark_vk_sha256"])
        self.assertEqual(evidence["result_sha256"]["root_cpu_keygen_acceptance"],
                         "6811744f568d88de692213dcb8f7d15d4e8bbea0c6d907d52418632fe69025cc")
        self.assertEqual(evidence["result_sha256"]["root_cpu_loaders_acceptance"],
                         "6ac85a26a0596e143c4cf2d579b7dce03d611ecdc9a693b0c472607a614fe3f5")
        self.assertEqual(evidence["result_sha256"]["cpu_loaders_build_attestation"],
                         "378a51f1bc9e5c0f9a80f42ce3ef8e8afa124ac221072542bde9d5b17ada5ceb")
        self.assertEqual(evidence["result_sha256"]["cpu_loaders_log"],
                         "ebe0620310557c51524790423735d7144a39ce7a694c82c7973e28f67847544c")
        cpu_loaders = evidence["normal_cpu_release_loaders"]
        self.assertEqual(cpu_loaders["source_commit"], "875975949ba3b856faaf5b0c092b8b81762c947a")
        self.assertEqual(cpu_loaders["source_tree"], "ac0a431ca1d4738b07a23bad1e9f7fd4274a0066")
        self.assertEqual(cpu_loaders["exit_code"], 0)
        self.assertEqual(cpu_loaders["passed_tests"],
                         {"protocol_version": 4, "fri": 12, "snark": 54, "total": 70})
        for key in ("failed_tests", "ignored_tests", "proofs_run", "workers_started"):
            self.assertEqual(cpu_loaders[key], 0)
        for key in ("normal_locked_prepared_sources", "inputs_reverified", "bundled_fri_loaded",
                    "bundled_commitment_loaded"):
            self.assertTrue(cpu_loaders[key])
        gpu_build = evidence["normal_gpu_worker_build"]
        self.assertEqual(gpu_build["source_commit"], cpu_loaders["source_commit"])
        self.assertEqual(gpu_build["source_tree"], cpu_loaders["source_tree"])
        self.assertEqual(gpu_build["archive_sha256"], cpu_loaders["archive_sha256"])
        self.assertEqual(gpu_build["exit_code"], 0)
        self.assertEqual(gpu_build["binaries_sha256"], {
            "zksync-os-prover-service": "4324e19759a4dd777df29926303eb51593fe989f40d1deba2e164d6445da5823",
            "zksync_os_snark_prover": "9d9b8f7b6d7b8cfb348e09c669e9d52bdb9cb65210e3a9cd74890f968f8dc0ac",
        })
        self.assertEqual(gpu_build["attested_file_count"], 53)
        for key in ("attested_application_and_tooling_inputs_match", "normal_locked_prepared_sources",
                    "inputs_reverified", "gpu_memory_overlay_retained"):
            self.assertTrue(gpu_build[key])
        self.assertEqual(gpu_build["proofs_run"], 0)
        self.assertEqual(gpu_build["workers_started"], 0)
        self.assertEqual(evidence["result_sha256"]["root_gpu_workers_acceptance"],
                         "2de3251f37e191c71a8faaf1e5d8a4f7d1baa04f1b27d8f8a3cce7bee386c533")
        self.assertEqual(evidence["result_sha256"]["gpu_workers_build_attestation"],
                         "05080505a1f09c21d43457f4b33db80de850fd4a38738234ef311e293afb9da4")
        self.assertFalse(evidence["normal_gpu_worker_build_pending"])
        self.assertTrue(evidence["normal_gpu_worker_build_passed"])
        self.assertTrue(evidence["normal_producer_build_loader_qualification_completed"])
        # Full release qualification/promotion is separate from completed build/loader checks.
        self.assertTrue(evidence["normal_producer_release_pending"])
        for key in ("live_submission_used", "deployment_attested", "release_promoted"):
            self.assertFalse(evidence[key])

    def test_copied_bundle_and_unactivated_source(self):
        module, manifest = M.load_bundle()
        self.assertEqual(module.BASE, "8fb7c29a4e3174335c6480b23f57822e054f9d5f")
        self.assertEqual(module.SOURCE, "0cb850a5c77436f17b49e747a1117dac5ee90888")
        self.assertEqual(module.CANDIDATE, "39aba041f59da6733f1562f2dee74ef39fc0c024")
        self.assertEqual(manifest["candidate_tree"], module.CANDIDATE)
        self.assertEqual(manifest["verification_key_hash"],
                         "0x22b4a58c40ab11839bfc79c96957f24711776f3e1992fa3cf14c1c3b898a2153")
        self.assertEqual(len(manifest["paths"]), 4)
        overlay = (M.BUNDLE / "generated-verifier-overlay.patch").read_bytes()
        self.assertEqual(len(overlay), manifest["overlay_size"])
        self.assertEqual(sha(overlay), manifest["overlay_sha256"])
        self.assertEqual(manifest["overlay_sha256"], module.OVERLAY_SHA)
        self.assertIsNone(M.CANONICAL_BINDING)
        self.assertEqual(sha((ROOT / "scripts/apply-era-contracts-syscoin-patch.sh").read_bytes()),
                         module.SOURCE_APPLICATOR_SHA)

    def test_current_contract_generation_binds_actual_normal_profile_postimages(self):
        module, manifest = M.load_bundle()
        paths = manifest["paths"]
        self.assertEqual(paths["AllContractsHashes.json"], {
            "size": 160049, "sha256": "6642cde552fa4b3391a122b92399a7d16c1b46c939143977d7959c8206b18cd4"})
        for rel in ("l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierPlonk.sol",
                    "tools/verifier-gen/data/ZKsyncOSVerifierPlonk.sol"):
            self.assertEqual(paths[rel], {
                "size": 95216, "sha256": "a51be347eb47aa8a3bba353c3f452b8990e0b326da2129cff1c7da7c39fac238"})
        self.assertEqual(paths["tools/verifier-gen/data/ZKsyncOS_plonk_scheduler_key.json"], {
            "size": 8073, "sha256": "668ee187e12fe8c1639018872c5dd7080a96d02475377a29f42cd308ca5d0da2"})
        evidence = manifest["historical_pre_canonical_query_contract_generation_provenance"]
        self.assertEqual(evidence["status"], "completed_gateway_priority_setter_inventory_refresh")
        self.assertEqual(evidence["reviewed_source_tree"], module.SOURCE)
        self.assertEqual(evidence["candidate_tree"], "d194956535ddeded60829706e9301e9adbe6cb5b")
        self.assertNotEqual(evidence["verification_key_hash"], manifest["verification_key_hash"])
        self.assertEqual(evidence["inventory_rows"], 278)
        self.assertEqual(evidence["unchanged_inventory_rows"], 277)
        self.assertEqual(evidence["changed_inventory_rows"], ["l1-contracts/AdminFacet"])
        for flag in ("full_contract_hash_recomputation_passed", "contract_hash_check_passed",
                     "normal_l1_evm_and_eravm_build_passed", "admin_abi_unchanged",
                     "generated_verifier_overlay_unchanged"):
            self.assertTrue(evidence[flag])
        self.assertFalse(evidence["new_proof_or_key_generation_performed"])
        self.assertFalse(evidence["deployed"])
        self.assertEqual(evidence["admin_evm_runtime_bytes"], 22482)
        self.assertEqual(evidence["tests_passed"], 28)
        self.assertEqual(evidence["tests_failed"], 0)
        self.assertEqual(evidence["new_gateway_regressions"], 3)
        self.assertEqual(evidence["log_sha256"],
                         "a78608670bca544c0654dbb567131a10a9c3c4f64754eb2718b3f30610afc70d")
        previous = manifest["historical_pre_gateway_priority_setter_contract_generation_provenance"]
        self.assertEqual(previous["status"], "completed_normal_profile_incremental_full_inventory_qualification")
        self.assertEqual(previous["baseline_inventory_rows_reproduced"], 278)
        self.assertEqual(previous["unchanged_inventory_rows"], 276)
        self.assertEqual(set(previous["changed_inventory_rows"]), module.ROWS)
        self.assertEqual(previous["compiler"]["evm_version"], "prague")
        self.assertEqual(previous["compiler"]["optimizer_runs"], 9999999)
        self.assertEqual(previous["candidate_tree"], evidence["previous_candidate_tree"])
        self.assertEqual(previous["result_sha256"],
                         "c283de5f73e332f43cc125cb27b92d2b5640cff64a9a1b215b687462c82e0af1")
        historical = manifest["historical_pre_optimized_crypto_contract_generation_provenance"]
        self.assertEqual(historical["status"], "completed_priority_admission_inventory_refresh")
        self.assertEqual(historical["result_sha256"],
                         "60e4344a40e31bef505e5de663062cd4b86f90371f65d61deb8bcfc28125b2e3")
        self.assertFalse(historical["full_contract_hash_recomputation_passed"])
        self.assertNotEqual(historical["verification_key_hash"], manifest["verification_key_hash"])
        self.assertFalse(manifest["independent_host_reproduction"])

    def test_preceding_offline_proof_is_not_current_or_live_acceptance(self):
        _, manifest = M.load_bundle()
        evidence = manifest["historical_pre_canonical_query_proof_qualification"]
        self.assertEqual(evidence["status"], "completed_offline_minimal_input_native_and_normal_profile_evm_qualification")
        self.assertNotEqual(evidence["verification_key_hash"], manifest["verification_key_hash"])
        self.assertEqual(evidence["program_commitment"],
                         "0xff43db6564b43491d6c25a613270e7802a4fca2bc67ad621e5cb08dd57d22975")
        self.assertEqual(evidence["plonk_source_sha256"],
                         "5abc85e5e33751defd4228c2f8775dfba540e5820b430325a7cb410f83e89feb")
        self.assertTrue(evidence["real_proof_verified"])
        self.assertTrue(evidence["real_evm_verified"])
        self.assertEqual(evidence["native_wrapper_stages_verified"], 3)
        self.assertEqual(evidence["independent_cpu_verifications_passed"], 3)
        self.assertTrue(evidence["cpu_gpu_setup_keys_byte_identical"])
        self.assertEqual(evidence["real_evm_controls_passed"], 2)
        self.assertEqual(evidence["estimated_verifier_gas"], 406832)
        self.assertEqual(evidence["input_batch_range"], {"from": 1, "to": 2})
        self.assertEqual(evidence["local_evm_original_publications"], 1)
        self.assertTrue(evidence["immediate_null_receipt_failure_preserved"])
        self.assertTrue(evidence["completion_used_read_only_views"])
        self.assertEqual(evidence["result_sha256"]["evm"],
                         "7716000dc165773feb665ddd673dc5c3ab7e588b48009ba750ded73a241a513d")
        for flag in ("live_submission_used", "deployment_attested", "release_promoted"):
            self.assertFalse(evidence[flag])
        self.assertIsNone(M.CANONICAL_BINDING)
        self.assertFalse(manifest["canonical_fixture_activated"])
        self.assertFalse(manifest["deployed"])
        historical = manifest["historical_2ac3_pending_proof_qualification"]
        self.assertEqual(historical["status"], "pending_fresh_canonical_proof_qualification")
        self.assertFalse(historical["real_proof_verified"])
        self.assertFalse(historical["real_evm_verified"])
        self.assertEqual(historical["result_sha256"], {})
        self.assertNotEqual(historical["verification_key_hash"], evidence["verification_key_hash"])

    def test_historical_d5bc_proof_evidence_retains_results_without_qualifying_current_key(self):
        _, manifest = M.load_bundle()
        evidence = manifest["historical_d5bc_proof_qualification"]
        self.assertEqual(evidence["status"], "completed_offline_proof_and_evm_qualification")
        self.assertEqual(evidence["verification_key_hash"],
                         "0xd5bc91a7af04425e93a92ad4e29f4f9ab62210087b5dea105d6bb579f1218139")
        self.assertNotEqual(evidence["verification_key_hash"], manifest["verification_key_hash"])
        self.assertEqual(evidence["program_commitment"],
                         "0x05c969ad8fcf8870cbb064c2947101ae27a5152c64467dcd7641f880485131de")
        self.assertEqual(evidence["plonk_source_sha256"],
                         "233a2e781431c132591431911442e3f0bccef95dfa813c57931f229d6c619efe")
        self.assertNotEqual(evidence["plonk_source_sha256"], manifest["paths"][
            "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierPlonk.sol"]["sha256"])
        self.assertTrue(evidence["real_proof_verified"])
        self.assertTrue(evidence["real_evm_verified"])
        self.assertEqual(evidence["native_wrapper_stages_verified"], 3)
        self.assertEqual(evidence["real_evm_controls_passed"], 10)
        self.assertEqual(set(evidence["result_sha256"]),
                         {"fri20", "fri21", "combine", "snark", "export", "evm"})
        self.assertEqual(evidence["native_server_boundary"]["owning_tests_passed"], 17)
        self.assertTrue(evidence["native_server_boundary"]["wrong_batch_and_trailing_bytes_rejected"])
        self.assertFalse(evidence["native_server_boundary"]["server_http_endpoint_exercised"])
        for flag in ("live_submission_used", "deployment_attested", "release_promoted"):
            self.assertFalse(evidence[flag])
        self.assertFalse(evidence["cross_host_guest_byte_reproduction"]["passed"])
        self.assertEqual(evidence["cross_host_guest_byte_reproduction"]["remaining_guest_pairs_not_compared"], 3)
        self.assertIsNone(M.CANONICAL_BINDING)

    def test_historical_d5bc_contract_generation_remains_distinct(self):
        _, manifest = M.load_bundle()
        evidence = manifest["historical_d5bc_contract_generation_provenance"]
        self.assertEqual(evidence["status"], "completed_contract_generation_only")
        self.assertEqual(evidence["verification_key_hash"],
                         "0xd5bc91a7af04425e93a92ad4e29f4f9ab62210087b5dea105d6bb579f1218139")
        self.assertNotEqual(evidence["verification_key_hash"], manifest["verification_key_hash"])
        self.assertEqual(evidence["result_sha256"],
                         "cbebf4ff7c9db6cd7da428784e3328f63c2b022e91b0dfb17d69976215fee1d3")
        self.assertEqual(evidence["keygen_result_sha256"],
                         "c17c12458443fb53681661476ace88b255c3f6dec34e4d817441290d786eccc2")

    def test_complete_historical_evidence_objects_are_preserved(self):
        _, manifest = M.load_bundle()
        expected = {
            "historical_pre_canonical_query_contract_generation_provenance":
                "c5b5fba5d407a1d476f1214ffdf7e9784337692abf65ea395437ea34a1175feb",
            "historical_pre_canonical_query_proof_qualification":
                "876dedfad2862a868ebc458532095b152f444ef63e7635641eb2889cf8b322f5",
            "historical_pre_paused_guard_contract_generation_provenance":
                "a2a2870562b7199332a4be3ad3e088d3a346775413afeb58783cba9c61517fd4",
            "historical_d5bc_contract_generation_provenance":
                "d94bc9dd31b6f0c148572f60c089e6466a9a34f0ef596e032faacf5da362df17",
            "historical_d5bc_proof_qualification":
                "392861e6d71268fb0c7f62ac1c01d120d8721ead54b85c5eb2dd4b82f321959d",
            "historical_crypto_validation_provenance":
                "d22a9b4ff31cbbe08ca2c8833a5226e528411d9830699de60b3ed805b9eb40de",
        }
        for key, digest in expected.items():
            with self.subTest(key=key):
                raw = json.dumps(manifest[key], sort_keys=True, separators=(",", ":")).encode()
                self.assertEqual(sha(raw), digest)

    def test_current_generated_verifier_authenticates_current_app_sources(self):
        self.assertEqual(M.APP_SOURCES, {
            "lib/types/src/protocol/proving_version.rs":
                "b1396be72c197077e4167686399caeee73f40d4f020be9da01bad48919574a63",
            "node/bin/src/prover_api/fri_proof_verifier.rs":
                "1ec3df17b7e4c3e89e53a9e8829415e75ad06894e22072eba7288c0681dc8077",
        })
        M.check_app_sources(ROOT)
        with tempfile.TemporaryDirectory() as temporary:
            server = Path(temporary).resolve()
            for relative in M.APP_SOURCES:
                put(server / relative, (ROOT / relative).read_bytes())
            relative = next(iter(M.APP_SOURCES))
            put(server / relative, (server / relative).read_bytes() + b"// stale identity\n")
            with self.assertRaisesRegex(ValueError, "input SHA-256 mismatch"):
                M.check_app_sources(server)

    def test_historical_crypto_evidence_does_not_claim_current_key_or_tree(self):
        module, manifest = M.load_bundle()
        evidence = manifest["historical_crypto_validation_provenance"]
        self.assertEqual(evidence["verification_key_hash"],
                         "0xc1ab3d6506620ad299672c2c2530e8732ac7bae55cdb9d8cf1fa12355b7388fe")
        self.assertNotEqual(evidence["verification_key_hash"], manifest["verification_key_hash"])
        self.assertEqual(evidence["reviewed_source_tree"], "3eefa0f127d1deff365ebffcf489b183cde0e756")
        self.assertEqual(evidence["candidate_tree"], "9b4ff94d1ff647cc00aeb0c3b81dbb922646b946")
        self.assertNotEqual(evidence["reviewed_source_tree"], module.SOURCE)
        self.assertNotEqual(evidence["candidate_tree"], module.CANDIDATE)
        self.assertEqual(evidence["subsequent_generated_tree"], "ff5565cd22b61259d6f886e9a0130f788bbdb11c")
        historical = {
            "cancun_artifact_sha256": "e460fffcbac0e4aba61ff19e19d3c7c970b6996b08bc708eba0368082ce60e1f",
            "cancun_creation_keccak256": "0x71bce5bbc0e436538cc293b76bf39d8dffceb47c5acbf43a32940d86c575cca3",
            "cancun_runtime_keccak256": "0x489376a19005518c7e60943cc58060afb684d16831eed41558ec876fc2705473",
            "cancun_archive_sha256": "ec039222c0ffd5e7dcd6906e2708e648cae355ffbe8c0ed68d0a689e30d48610",
            "native_build_evidence_archive_sha256": "bf1bb141a0c6c54e512319de3291535dd8aaccf15f41eb402b3ed48bda0cf66a",
            "real_serialized_proof_sha256": "ee9c95301ae9f6de0756308e318b37ea8280dcc911e097d4a3fe8abc7a24c8f7",
            "real_evm_tests": 10,
            "real_evm_verified_record_sha256": "7b9e216483593fbb28c6675ca4f5c6a171e38f539c9d6d772fafa0a904611d6a",
        }
        for key, value in historical.items():
            self.assertNotIn(key, manifest)
            self.assertEqual(evidence[key], value)
        self.assertFalse(manifest["canonical_fixture_activated"])
        self.assertFalse(manifest["deployed"])

    @unittest.skipUnless(os.environ.get("ERA_GENERATED_RELEASE_TEST_ROOT"),
                         "set ERA_GENERATED_RELEASE_TEST_ROOT for real read-only tree reconstruction")
    def test_real_bundle_reconstructs_exact_new_tree_without_checkout_mutation(self):
        module, _ = M.load_bundle()
        era = Path(os.environ["ERA_GENERATED_RELEASE_TEST_ROOT"])
        env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        env["GIT_OPTIONAL_LOCKS"] = "0"
        def git(*args):
            return subprocess.check_output(["git", "-C", str(era), *args], env=env)
        index = Path(git("rev-parse", "--git-path", "index").decode().strip())
        if not index.is_absolute():
            index = era / index
        index_before = index.read_bytes()
        worktree_before = git("diff", "--binary")
        result = module.check_bundle(era,
            ROOT / "scripts/patches/era-contracts-syscoin.patch",
            ROOT / "scripts/apply-era-contracts-syscoin-patch.sh",
            M.BUNDLE / "generated-verifier-overlay.patch")
        self.assertEqual(result["status"], "exact_overlay_bundle_validated")
        self.assertEqual(result["source_tree"], "0cb850a5c77436f17b49e747a1117dac5ee90888")
        self.assertEqual(result["candidate_tree"], "39aba041f59da6733f1562f2dee74ef39fc0c024")
        self.assertEqual(result["generated_paths"], sorted(module.PATHS))
        self.assertFalse(result["checkout_mutated"])
        self.assertFalse(result["canonical_fixture_activated"])
        self.assertFalse(result["deployed"])
        self.assertEqual(index.read_bytes(), index_before)
        self.assertEqual(git("diff", "--binary"), worktree_before)

    def test_explicit_fixture_check_marker_first_and_environment_cannot_override(self):
        env = dict(os.environ, CANONICAL_BINDING="approved", PROVER_MODE="gpu")
        result = subprocess.run(["python3", "-B", str(ROOT / "scripts/apply-era-contracts-syscoin-release.py"),
                                 "--check-canonical-fixture"], env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("regeneration marker", result.stderr)
        self.assertNotIn("git", result.stderr)

    def test_source_assertion_does_not_consume_fixture(self):
        result = subprocess.run(["python3", "-B", str(ROOT / "scripts/apply-era-contracts-syscoin-release.py"),
                                 "--assert-applied", "/nonexistent-era"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertNotIn("regeneration marker", result.stderr)

    def test_generated_manifest_tamper_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = Path(directory).resolve()
            for name in ("check-release-overlay.py", "generated-verifier-manifest.json"):
                put(bundle / name, (M.BUNDLE / name).read_bytes())
            manifest = bundle / "generated-verifier-manifest.json"
            manifest.write_text(manifest.read_text().replace(M.VK, "0x" + "0" * 64))
            with patch.object(M, "BUNDLE", bundle), self.assertRaisesRegex(ValueError, "SHA-256"):
                M.load_bundle()

    def dispatch(self, mode="gpu", mock="false", override="", assert_only=False):
        env = dict(os.environ, COMMON=str(ROOT / "scripts/gateway-launch/_common.sh"),
                   ZKSYNC_ERA_PATH="/fixture-era", ZKSYNC_OS_SERVER_PATH=str(ROOT),
                   PROTOCOL_VERSION="v32.0", PROVER_MODE=mode,
                   GATEWAY_PROVER_MODE=override or mode, EDGE_PROVER_MODE=mode,
                   SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER=mock, L1_NETWORK="localhost", L1_CHAIN_ID="31337")
        probe = 'source "$COMMON"; bash() { printf "SOURCE %s\\n" "$*"; }; python3() { printf "RELEASE %s\\n" "$*"; }; '
        probe += "gl_assert_era_contracts_syscoin_postimage" if assert_only else "gl_ensure_era_contracts_syscoin_postimage"
        return subprocess.run(["bash", "-c", probe], env=env, capture_output=True, text=True)

    def test_existing_mock_route_and_real_route(self):
        source = self.dispatch("no-proofs", "true")
        self.assertEqual(source.returncode, 0, source.stderr)
        self.assertIn("SOURCE", source.stdout)
        release = self.dispatch(assert_only=True)
        self.assertEqual(release.returncode, 0, release.stderr)
        self.assertIn("RELEASE -B", release.stdout)
        self.assertIn("--assert-applied /fixture-era/contracts", release.stdout)
        self.assertNotIn("--check-bundle", release.stdout)

    def test_mixed_modes_cannot_select_either_lane(self):
        for args in (("no-proofs", "true", "gpu"), ("gpu", "true", ""), ("no-proofs", "false", "")):
            result = self.dispatch(*args)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn("SOURCE", result.stdout)
            self.assertNotIn("RELEASE", result.stdout)

    STANDALONE_ENTRYPOINTS = (
        "gateway-ecosystem-create.sh",
        "gateway-chain-init.sh",
        "gateway-convert-settlement.sh",
        "edge-chain-create-init.sh",
        "edge-chain-migrate-to-gateway.sh",
        "zksys-l2-bootstrap.sh",
    )

    def standalone_dispatch(self, script, mode="gpu", mock="false", gateway=None,
                            edge=None, network="localhost", chain_id="auto",
                            normalize_first=False, args=()):
        launcher_dir = ROOT / "scripts/gateway-launch"
        source = (launcher_dir / script).read_text()
        common_source = 'source "${SCRIPT_DIR}/_common.sh"\n'
        start = source.index(common_source) + len(common_source)
        dispatch = ("gl_ensure_era_contracts_syscoin_postimage" if script == "zksys-l2-bootstrap.sh"
                    else "gl_ensure_zkstack_cli_release_current")
        end = source.index("\n" + dispatch + "\n", start) + len(dispatch) + 2
        entrypoint = source[start:end]
        with tempfile.TemporaryDirectory() as directory:
            gateway_dir = Path(directory).resolve()
            config = gateway_dir / "chains/zksys/ZkStack.yaml"
            put(config, json.dumps({
                "name": "zksys", "chain_id": 57057,
                "prover_version": "NoProofs" if (edge or mode).lower() == "no-proofs" else "Gpu",
                "l1_batch_commit_data_generator_mode": "Rollup", "vm_option": "ZKSyncOsVM",
                "evm_emulator": False,
                "base_token": {"address": "0x0000000000000000000000000000000000000001",
                               "nominator": 1, "denominator": 1},
                "l1_network": (network or "localhost").lower(),
            }).encode())
            config.chmod(0o600)
            config.parent.chmod(0o700)
            # The fixture uses JSON, a YAML subset; keep this test independent of PyYAML.
            put(gateway_dir / "yaml.py", b"from json import loads as safe_load\n")
            env = {name: os.environ[name] for name in ("HOME", "PATH", "TMPDIR") if name in os.environ}
            env.update(COMMON=str(launcher_dir / "_common.sh"), SCRIPT_DIR=str(launcher_dir),
                       ZKSYNC_ERA_PATH="/fixture-era", ZKSYNC_OS_SERVER_PATH=str(ROOT),
                       PROTOCOL_VERSION="v32.0", PROVER_MODE=mode,
                       SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER=mock, GATEWAY_DIR=str(gateway_dir),
                       GATEWAY_CREATE2_FACTORY_SALT="normalization-test-salt",
                       L1_RPC_URL="http://fixture.invalid", ZKSYS_L2_RPC_URL="http://edge.invalid",
                       ZKSYS_L2_TOKEN_ADMIN_ADDRESS="0x0000000000000000000000000000000000000001",
                       ZKSYS_ISSUER_START_TIME="10000", PYTHONDONTWRITEBYTECODE="1",
                       PYTHONPATH=str(gateway_dir))
            for name, value in (("GATEWAY_PROVER_MODE", gateway), ("EDGE_PROVER_MODE", edge),
                                ("L1_NETWORK", network)):
                if value is not None:
                    env[name] = value
            if chain_id == "auto":
                chain_id = {"localhost": "31337", "tanenbaum": "5700", "mainnet": "57"}.get(
                    (network or "localhost").lower(), "31337")
            if chain_id is not None:
                env["L1_CHAIN_ID"] = chain_id
            # Retain each actual prefix, including bootstrap's earlier network and
            # config checks. Only external Git, source/build, lock and RPC I/O are
            # replaced; normalization, dispatch and CLI preparation remain real.
            probe = 'set -euo pipefail\nsource "$COMMON"\n'
            probe += r"""gl_assert_zksync_era_sha() { :; }
gl_assert_contracts_sha() { :; }
gl_zkstack_cli_release_stamp_matches() { return 1; }
gl_build_zkstack_cli_release() { printf 'BUILD\n'; }
gl_acquire_gateway_launch_lock() { printf 'LOCK\n'; }
gl_non_l1_cast() {
  [ "$*" = 'chain-id --rpc-url http://edge.invalid' ] || return 90
  printf '57057\n'
}
bash() {
  case "${1:-}" in
    "$ZKSYNC_OS_SERVER_PATH/scripts/apply-era-contracts-syscoin-patch.sh")
      printf 'SOURCE %s\n' "$*" ;;
    "$ZKSYNC_OS_SERVER_PATH/scripts/apply-zksync-era-syscoin-patch.sh")
      printf 'ERA %s\n' "$*" ;;
    *) printf 'unexpected bash command\n' >&2; return 91 ;;
  esac
}
python3() {
  if [ "${1:-}" = -B ] && [ "${2:-}" = "$ZKSYNC_OS_SERVER_PATH/scripts/apply-era-contracts-syscoin-release.py" ]; then
    printf 'RELEASE %s\n' "$*"
  else
    command python3 "$@"
  fi
}
"""
            if normalize_first:
                probe += 'gl_normalize_canonical_deployment_inputs\n'
            probe += entrypoint
            probe += ('printf "MODES %s:%s:%s:%s\\n" "$PROVER_MODE" "$GATEWAY_PROVER_MODE" '
                      '"$EDGE_PROVER_MODE" "$SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER"\n'
                      'printf "NETWORK %s\\n" "$L1_NETWORK"\n')
            return subprocess.run(["bash", "-c", probe, "standalone-dispatch", *args],
                                  env=env, capture_output=True, text=True)

    def assert_dispatch_route(self, result, script, mock=False, network="localhost"):
        self.assertEqual(result.returncode, 0, result.stderr)
        if mock:
            self.assertIn("SOURCE ", result.stdout)
            self.assertNotIn("RELEASE ", result.stdout)
            self.assertIn("MODES no-proofs:no-proofs:no-proofs:true", result.stdout)
        else:
            self.assertIn("RELEASE -B", result.stdout)
            self.assertNotIn("SOURCE ", result.stdout)
            self.assertIn("MODES gpu:gpu:gpu:false", result.stdout)
        self.assertIn("NETWORK " + network, result.stdout)
        if script != "zksys-l2-bootstrap.sh":
            self.assertIn("ERA ", result.stdout)
            self.assertIn("BUILD", result.stdout)

    def assert_no_source_dispatch(self, result):
        self.assertNotEqual(result.returncode, 0)
        for event in ("SOURCE ", "RELEASE ", "ERA ", "BUILD"):
            self.assertNotIn(event, result.stdout)

    def test_standalone_entrypoints_accept_case_variants_before_postimage(self):
        for script in self.STANDALONE_ENTRYPOINTS:
            for mode, mock, gateway, edge in (("GPU", "FALSE", "GPU", "GPU"),
                                              ("GpU", "FaLsE", "gPu", "gpU"),
                                              ("gpu", "false", "gpu", "gpu"),
                                              ("GPU", "FALSE", None, None)):
                with self.subTest(script=script, mode=mode, mock=mock, gateway=gateway):
                    result = self.standalone_dispatch(script, mode, mock, gateway, edge)
                    self.assert_dispatch_route(result, script)

    def test_standalone_entrypoints_preserve_already_normalized_launch_path(self):
        for script in self.STANDALONE_ENTRYPOINTS:
            with self.subTest(script=script):
                result = self.standalone_dispatch(script, "GpU", "FaLsE", "gPu", "gpU",
                                                  normalize_first=True)
                self.assert_dispatch_route(result, script)

    def test_standalone_entrypoints_preserve_mock_modes_and_network_case(self):
        for script in self.STANDALONE_ENTRYPOINTS:
            for network in ("localhost", "LOCALHOST", "TaNeNbAuM"):
                with self.subTest(script=script, network=network):
                    result = self.standalone_dispatch(script, "NO-PROOFS", "TrUe", "No-Proofs",
                                                      network=network)
                    self.assert_dispatch_route(result, script, mock=True, network=network.lower())

    def test_ecosystem_keeps_default_network_and_optional_real_chain_id(self):
        script = "gateway-ecosystem-create.sh"
        real = self.standalone_dispatch(script, "GPU", "FALSE", network=None, chain_id=None)
        self.assert_dispatch_route(real, script)
        mock = self.standalone_dispatch(script, "NO-PROOFS", "TRUE", "NO-PROOFS", network=None)
        self.assert_dispatch_route(mock, script, mock=True)

    def test_standalone_entrypoints_reject_invalid_and_mixed_modes_before_source(self):
        invalid = (("CPU", "FALSE", None, None),
                   ("GPU", "FALSE", "CPU", None),
                   ("GPU", "FALSE", None, "CPU"),
                   ("GPU", "maybe", None, None),
                   ("GPU", "TRUE", None, None),
                   ("NO-PROOFS", "FALSE", "NO-PROOFS", None),
                   ("GPU", "FALSE", "No-Proofs", "GPU"),
                   ("NO-PROOFS", "TRUE", "NO-PROOFS", "GPU"))
        for script in self.STANDALONE_ENTRYPOINTS:
            for args in invalid:
                with self.subTest(script=script, args=args):
                    self.assert_no_source_dispatch(self.standalone_dispatch(script, *args))

    def test_standalone_entrypoints_reject_invalid_mock_network_before_source(self):
        for script in self.STANDALONE_ENTRYPOINTS:
            for network, chain_id in (("unknown", "31337"), ("LOCALHOST", "57"), ("Tanenbaum", "31337")):
                with self.subTest(script=script, network=network, chain_id=chain_id):
                    result = self.standalone_dispatch(script, "NO-PROOFS", "TRUE", "NO-PROOFS",
                                                      network=network, chain_id=chain_id)
                    self.assert_no_source_dispatch(result)

    def test_standalone_network_validation_precedes_source_refresh(self):
        for script in self.STANDALONE_ENTRYPOINTS[1:]:
            for network, chain_id in (("unknown", "31337"), ("localhost", "57"), (None, "31337")):
                with self.subTest(script=script, network=network, chain_id=chain_id):
                    result = self.standalone_dispatch(script, network=network, chain_id=chain_id)
                    self.assert_no_source_dispatch(result)

    def test_standalone_mainnet_real_route_and_mock_guards_remain_intact(self):
        for script in self.STANDALONE_ENTRYPOINTS:
            with self.subTest(script=script, route="real"):
                real = self.standalone_dispatch(script, "GPU", "FALSE", network="MaInNeT")
                self.assert_dispatch_route(real, script, network="mainnet")
            for mock in ("TRUE", "FALSE"):
                with self.subTest(script=script, mock=mock):
                    result = self.standalone_dispatch(script, "NO-PROOFS", mock, "NO-PROOFS",
                                                      network="MAINNET")
                    self.assert_no_source_dispatch(result)

    def test_migration_post_finalize_lock_precedes_source_refresh(self):
        script = "edge-chain-migrate-to-gateway.sh"
        result = self.standalone_dispatch(script, "GPU", "FALSE", args=("--resume-post-finalize",))
        self.assert_dispatch_route(result, script)
        self.assertLess(result.stdout.index("LOCK"), result.stdout.index("RELEASE -B"))
        self.assertLess(result.stdout.index("LOCK"), result.stdout.index("ERA "))

    def test_fingerprint_covers_exact_new_inputs(self):
        common = (ROOT / "scripts/gateway-launch/_common.sh").read_text()
        fn = common.split("gl_zkstack_cli_release_fingerprint() {", 1)[1].split("\nPY\n}", 1)[0]
        self.assertIn('"era_contracts_postimage_lane": sys.argv[4]', fn)
        self.assertIn("contracts_lane=pending-mock-source", fn)
        for rel in ("scripts/apply-era-contracts-syscoin-release.py",
                    "scripts/releases/era-v32/check-release-overlay.py",
                    "scripts/releases/era-v32/generated-verifier-overlay.patch",
                    "scripts/releases/era-v32/generated-verifier-manifest.json"):
            self.assertIn('"' + rel + '"', fn)


class TemporaryGitEngineTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.repo = self.root / "era"; self.repo.mkdir()
        self.server = self.root / "server"; self.server.mkdir()
        self.bundle = self.server / "bundle"; self.bundle.mkdir()
        self.git("init", "-q")
        self.git("config", "user.email", "test@example.invalid")
        self.git("config", "user.name", "Synthetic Test")
        put(self.repo / ".gitignore", b"*.sol\n")
        put(self.repo / "source.txt", b"upstream\n")
        self.git("add", "."); self.git("commit", "-qm", "synthetic base")
        base = self.git("rev-parse", "HEAD").strip()
        self.index_before = (self.repo / ".git/index").read_bytes()
        put(self.repo / "source.txt", b"reviewed source\n")
        source_patch = self.git("diff", "--binary")
        self.git("add", "source.txt")
        source_tree = self.git("write-tree").strip()
        put(self.repo / "generated.sol", b"exact generated public code\n")
        self.git("add", "-f", "generated.sol")
        candidate = self.git("write-tree").strip()
        overlay_patch = self.git("diff", "--cached", source_tree)
        self.git("read-tree", "HEAD")
        put(self.repo / "source.txt", b"upstream\n")
        (self.repo / "generated.sol").unlink()
        self.index_before = (self.repo / ".git/index").read_bytes()
        put(self.server / "scripts/patches/era-contracts-syscoin.patch", source_patch.encode())
        put(self.bundle / "generated-verifier-overlay.patch", overlay_patch.encode())
        self.overlay = types.SimpleNamespace(BASE=base, SOURCE=source_tree, CANDIDATE=candidate,
            PATHS={"generated.sol"}, check_bundle=lambda *args: None)
        self.manifest = {"paths": {"generated.sol": {"size": 28,
            "sha256": sha(b"exact generated public code\n")}}}
        self.manifest["paths"]["generated.sol"]["size"] = len(b"exact generated public code\n")
        self.addCleanup(patch.stopall)
        patch.object(M, "SERVER", self.server).start()
        patch.object(M, "BUNDLE", self.bundle).start()
        patch.object(M, "PREIMAGES", {"generated.sol": None}).start()
        patch.object(M, "load_bundle", return_value=(self.overlay, self.manifest)).start()
        self.app_path = self.server / "lib/types/src/protocol/proving_version.rs"
        put(self.app_path, b"synthetic reviewed identity\n")
        patch.object(M, "APP_SOURCES", {
            "lib/types/src/protocol/proving_version.rs": sha(self.app_path.read_bytes())}).start()
        put(self.server / "local-chains/v32.0/CANONICAL_V8_REGENERATION_REQUIRED", b"blocked\n")
        patch.object(M, "CANONICAL_BINDING", None).start()
        patch.object(M, "require_dependencies").start()

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.repo), *args], stderr=subprocess.PIPE).decode()

    def test_base_apply_exact_and_idempotent_real_index_unchanged(self):
        first = M.run(self.repo)
        self.assertTrue(first["target_mutated"])
        self.assertEqual(first["tree"], self.overlay.CANDIDATE)
        self.assertFalse(first["canonical_fixture_authorized"])
        asserted = M.run(self.repo, assert_applied=True)
        self.assertFalse(asserted["target_mutated"])
        self.assertFalse(asserted["canonical_fixture_authorized"])
        with self.assertRaisesRegex(ValueError, "regeneration marker"):
            M.check_activation(self.server)
        self.assertEqual((self.repo / ".git/index").read_bytes(), self.index_before)

    def test_changed_app_identity_rejected_before_source_mutation(self):
        self.app_path.write_bytes(b"stock or stale VK identity\n")
        with self.assertRaisesRegex(ValueError, "SHA-256"):
            M.run(self.repo)
        self.assertEqual((self.repo / "source.txt").read_bytes(), b"upstream\n")
        self.assertFalse((self.repo / "generated.sol").exists())

    def test_source_apply_and_check_only_no_mutation(self):
        put(self.repo / "source.txt", b"reviewed source\n")
        before = (self.repo / "source.txt").read_bytes()
        checked = M.run(self.repo, check_bundle_only=True)
        self.assertEqual(checked["tree"], self.overlay.SOURCE)
        self.assertFalse(checked["canonical_launch_authorized"])
        self.assertFalse((self.repo / "generated.sol").exists())
        self.assertEqual((self.repo / "source.txt").read_bytes(), before)
        self.assertEqual(M.run(self.repo)["tree"], self.overlay.CANDIDATE)

    def test_assertion_refuses_to_apply(self):
        with self.assertRaisesRegex(ValueError, "assert-applied"):
            M.run(self.repo, assert_applied=True)
        self.assertEqual((self.repo / "source.txt").read_bytes(), b"upstream\n")

    def test_partial_unrelated_staged_and_symlink_rejected(self):
        put(self.repo / "source.txt", b"unexpected edit\n")
        with self.assertRaisesRegex(ValueError, "postimage"):
            M.run(self.repo)
        put(self.repo / "source.txt", b"upstream\n")
        put(self.repo / "unexpected.txt", b"unrelated\n")
        with self.assertRaisesRegex(ValueError, "unrelated"):
            M.run(self.repo)
        (self.repo / "unexpected.txt").unlink()
        put(self.repo / "source.txt", b"reviewed source\n")
        self.git("add", "source.txt")
        with self.assertRaisesRegex(ValueError, "staged"):
            M.run(self.repo)
        self.git("read-tree", "HEAD")
        (self.repo / "generated.sol").symlink_to(self.repo / "source.txt")
        with self.assertRaisesRegex(ValueError, "symlink"):
            M.run(self.repo)


if __name__ == "__main__":
    unittest.main()
