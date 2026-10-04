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
    def test_copied_bundle_and_unactivated_source(self):
        module, manifest = M.load_bundle()
        self.assertEqual(module.BASE, "8fb7c29a4e3174335c6480b23f57822e054f9d5f")
        self.assertEqual(module.SOURCE, "264d98e758c3a032942dfb08ee7d87a3f46288b4")
        self.assertEqual(module.CANDIDATE, "117b5f2d1ad82de6a073142d45bd46a5218f493e")
        self.assertEqual(manifest["candidate_tree"], module.CANDIDATE)
        self.assertEqual(manifest["verification_key_hash"],
                         "0xd5bc91a7af04425e93a92ad4e29f4f9ab62210087b5dea105d6bb579f1218139")
        self.assertEqual(len(manifest["paths"]), 4)
        overlay = (M.BUNDLE / "generated-verifier-overlay.patch").read_bytes()
        self.assertEqual(len(overlay), manifest["overlay_size"])
        self.assertEqual(sha(overlay), manifest["overlay_sha256"])
        self.assertEqual(manifest["overlay_sha256"], module.OVERLAY_SHA)
        self.assertIsNone(M.CANONICAL_BINDING)
        self.assertEqual(sha((ROOT / "scripts/apply-era-contracts-syscoin-patch.sh").read_bytes()),
                         module.SOURCE_APPLICATOR_SHA)

    def test_new_contract_generation_binds_actual_postimages_without_proof_claim(self):
        module, manifest = M.load_bundle()
        paths = manifest["paths"]
        self.assertEqual(paths["AllContractsHashes.json"], {
            "size": 160049, "sha256": "fe6060331c9ffacc1bf26d52a6e0851c09b593a0723faf1bda7a7811821ba91d"})
        for rel in ("l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierPlonk.sol",
                    "tools/verifier-gen/data/ZKsyncOSVerifierPlonk.sol"):
            self.assertEqual(paths[rel], {
                "size": 95216, "sha256": "233a2e781431c132591431911442e3f0bccef95dfa813c57931f229d6c619efe"})
        self.assertEqual(paths["tools/verifier-gen/data/ZKsyncOS_plonk_scheduler_key.json"], {
            "size": 8072, "sha256": "3dffa1e43ee043d708934ecc70ceedbfe4c9aff3ace3c871848de9ff61ab0379"})
        evidence = manifest["contract_generation_provenance"]
        self.assertEqual(evidence["status"], "completed_contract_generation_only")
        self.assertEqual(evidence["reviewed_source_tree"], module.SOURCE)
        self.assertEqual(evidence["verification_key_hash"], manifest["verification_key_hash"])
        self.assertEqual(evidence["result_sha256"],
                         "cbebf4ff7c9db6cd7da428784e3328f63c2b022e91b0dfb17d69976215fee1d3")
        self.assertEqual(evidence["keygen_result_sha256"],
                         "c17c12458443fb53681661476ace88b255c3f6dec34e4d817441290d786eccc2")
        self.assertEqual(evidence["plonk_generator_tests_passed"], 4)
        self.assertFalse(manifest["independent_host_reproduction"])

    def test_offline_proof_evidence_binds_current_verifier_without_activation(self):
        _, manifest = M.load_bundle()
        evidence = manifest["proof_qualification"]
        self.assertEqual(evidence["status"], "completed_offline_proof_and_evm_qualification")
        self.assertEqual(evidence["verification_key_hash"], manifest["verification_key_hash"])
        self.assertEqual(evidence["plonk_source_sha256"], manifest["paths"][
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
        self.assertEqual(result["source_tree"], "264d98e758c3a032942dfb08ee7d87a3f46288b4")
        self.assertEqual(result["candidate_tree"], "117b5f2d1ad82de6a073142d45bd46a5218f493e")
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
