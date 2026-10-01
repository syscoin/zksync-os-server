"""Temporary-fixture tests only: no canonical activation, compilation or deployment."""
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
        self.assertEqual(module.CANDIDATE, "ff5565cd22b61259d6f886e9a0130f788bbdb11c")
        self.assertEqual(len(manifest["paths"]), 4)
        self.assertIsNone(M.CANONICAL_BINDING)
        self.assertEqual(sha((ROOT / "scripts/apply-era-contracts-syscoin-patch.sh").read_bytes()),
                         module.SOURCE_APPLICATOR_SHA)

    def test_historical_crypto_evidence_does_not_claim_current_deployment_tree(self):
        module, manifest = M.load_bundle()
        evidence = manifest["crypto_validation_provenance"]
        self.assertEqual(evidence["reviewed_source_tree"], "3eefa0f127d1deff365ebffcf489b183cde0e756")
        self.assertEqual(evidence["candidate_tree"], "9b4ff94d1ff647cc00aeb0c3b81dbb922646b946")
        self.assertNotEqual(evidence["reviewed_source_tree"], module.SOURCE)
        self.assertNotEqual(evidence["candidate_tree"], module.CANDIDATE)
        self.assertFalse(manifest["canonical_fixture_activated"])
        self.assertFalse(manifest["deployed"])

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

    def standalone_bootstrap_dispatch(self, mode="gpu", mock="false", gateway=None,
                                      edge=None, network="localhost", normalize_first=False):
        bootstrap = (ROOT / "scripts/gateway-launch/zksys-l2-bootstrap.sh").read_text()
        start = bootstrap.index("\ngl_resolve_required_source_pins\n") + 1
        end = bootstrap.index("\ngl_require L1_RPC_URL\n", start)
        entrypoint = bootstrap[start:end]
        env = dict(os.environ, COMMON=str(ROOT / "scripts/gateway-launch/_common.sh"),
                   ZKSYNC_ERA_PATH="/fixture-era", ZKSYNC_OS_SERVER_PATH=str(ROOT),
                   PROTOCOL_VERSION="v32.0", PROVER_MODE=mode,
                   GATEWAY_PROVER_MODE=mode if gateway is None else gateway,
                   EDGE_PROVER_MODE=mode if edge is None else edge,
                   SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER=mock, L1_NETWORK=network,
                   L1_CHAIN_ID="57" if network == "mainnet" else "31337")
        for name in ("USE_DUMMY_MESSAGE_ROOT", "GATEWAY_COMMIT_MODE",
                     "GATEWAY_L2_DA_COMMITMENT_SCHEME_VALUE", "GATEWAY_L2_DA_COMMITMENT_SCHEME",
                     "EDGE_GATEWAY_COMMITTER_WALLET_NAME", "ZKSYS_ZK_TOKEN_ASSET_ID", "ZK_TOKEN_ASSET_ID"):
            env.pop(name, None)
        # Execute the actual standalone ordering with only Git/source writes
        # stubbed. Both the real normalizer and lane-selection gate stay intact.
        probe = 'set -euo pipefail\nsource "$COMMON"\n'
        probe += ('gl_resolve_required_source_pins() { :; }\n'
                  'gl_assert_zksync_era_sha() { :; }\n'
                  'gl_assert_contracts_sha() { :; }\n'
                  'bash() { printf "SOURCE %s\\n" "$*"; }\n'
                  'python3() { printf "RELEASE %s\\n" "$*"; }\n')
        if normalize_first:
            probe += 'gl_normalize_canonical_deployment_inputs\n'
        probe += entrypoint + '\nprintf "MODES %s:%s:%s:%s\\n" "$PROVER_MODE" "$GATEWAY_PROVER_MODE" "$EDGE_PROVER_MODE" "$SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER"\n'
        return subprocess.run(["bash", "-c", probe], env=env, capture_output=True, text=True)

    def test_standalone_bootstrap_accepts_case_variants_before_postimage(self):
        for mode, mock, gateway, edge in (("GPU", "FALSE", "GPU", "GPU"),
                                          ("GpU", "FaLsE", "gPu", "gpU"),
                                          ("gpu", "false", "gpu", "gpu")):
            with self.subTest(mode=mode, mock=mock):
                result = self.standalone_bootstrap_dispatch(mode, mock, gateway, edge)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("RELEASE -B", result.stdout)
                self.assertIn("MODES gpu:gpu:gpu:false", result.stdout)
                self.assertNotIn("SOURCE", result.stdout)

    def test_standalone_bootstrap_preserves_already_normalized_launch_path(self):
        result = self.standalone_bootstrap_dispatch("GpU", "FaLsE", "gPu", "gpU", normalize_first=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("RELEASE -B", result.stdout)
        self.assertIn("MODES gpu:gpu:gpu:false", result.stdout)

    def test_standalone_bootstrap_preserves_mock_lane_and_rejects_invalid_modes(self):
        mock = self.standalone_bootstrap_dispatch("NO-PROOFS", "TRUE")
        self.assertEqual(mock.returncode, 0, mock.stderr)
        self.assertIn("SOURCE", mock.stdout)
        self.assertNotIn("RELEASE", mock.stdout)
        self.assertIn("MODES no-proofs:no-proofs:no-proofs:true", mock.stdout)
        for args in (("CPU", "FALSE", None, None, "localhost"),
                     ("GPU", "TRUE", None, None, "localhost"),
                     ("NO-PROOFS", "FALSE", None, None, "localhost"),
                     ("GPU", "FALSE", "No-Proofs", "GPU", "localhost"),
                     ("NO-PROOFS", "TRUE", None, None, "mainnet")):
            with self.subTest(args=args):
                result = self.standalone_bootstrap_dispatch(*args)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("SOURCE", result.stdout)
                self.assertNotIn("RELEASE", result.stdout)

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
