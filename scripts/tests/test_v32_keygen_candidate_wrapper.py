"""No-build checks for the candidate keygen's pinned proving source graph."""

import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import textwrap
from unittest import mock
import tempfile
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "candidate_wrapper", ROOT / "scripts/keygen/prepare-candidate-wrapper.py")
CANDIDATE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CANDIDATE)
UPSTREAM_AIRBENDER = "https://github.com/matter-labs/zksync-airbender"
UPSTREAM_LOCK_SOURCE = ("git+" + UPSTREAM_AIRBENDER
                        + "?tag=v0.6.0-rc.2#03454c7a41053a4b88bb421e97fb9efe893a92f5")


class CandidateWrapperTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.wrapper = self.root / "zkos-wrapper"
        self.crypto = self.root / "zksync-crypto"
        self.wrapper.mkdir()
        self.crypto.mkdir()
        self.crypto_source = ("git+https://github.com/matter-labs/zksync-crypto.git"
                              "?branch=oh_for_wrapper#bf2797e4ca13475bf797aa43e085389cdd6732f9")
        self.crypto_pins = {
            "upstream_url": "https://github.com/matter-labs/zksync-crypto.git",
            "upstream_lock_source": self.crypto_source,
            "upstream_packages": {name: "0.32.10" for name in
                                  CANDIDATE.WRAPPER_CRYPTO_PACKAGES |
                                  {"zksync_solidity_vk_codegen"}},
        }
        self.crypto_paths = {**CANDIDATE.CRYPTO_PATHS,
                             "zksync_solidity_vk_codegen": "crates/vk_codegen"}
        manifest = "[workspace]\n[workspace.dependencies]\n"
        for name in sorted(CANDIDATE.AIRBENDER_DEPS):
            manifest += (name + ' = { git = "' + CANDIDATE.FORK
                         + '", rev = "' + CANDIDATE.FORK_COMMIT + '" }\n')
        (self.wrapper / "Cargo.toml").write_text(manifest)
        lock = "version = 4\n"
        for index in range(44):
            lock += (f'\n[[package]]\nname = "air-{index}"\nversion = "0.1.0"\n'
                     + 'source = "' + CANDIDATE.NEW_SOURCE + '"\n')
        for name in sorted(CANDIDATE.WRAPPER_CRYPTO_PACKAGES):
            lock += (f'\n[[package]]\nname = "{name}"\nversion = "0.32.10"\n'
                     + 'source = "' + self.crypto_source + '"\n')
        lock += ('\n[[package]]\nname = "unrelated"\nversion = "1.0.0"\n'
                 'source = "registry+https://github.com/rust-lang/crates.io-index"\n')
        (self.wrapper / "Cargo.lock").write_text(lock)
        self.original = tomllib.loads(lock)

    def pin(self, tree="reviewed-wrapper", head=None, dirty="",
            crypto_head=None, crypto_tree="reviewed-crypto", crypto_dirty=""):
        def git(directory, *args):
            if directory == self.crypto:
                return {("rev-parse", "HEAD"): crypto_head or CANDIDATE.CRYPTO_COMMIT,
                        ("rev-parse", "HEAD^{tree}"): crypto_tree,
                        ("status", "--porcelain", "--untracked-files=all"): crypto_dirty}[args]
            return {("rev-parse", "HEAD"): head or CANDIDATE.WRAPPER_COMMIT,
                    ("rev-parse", "HEAD^{tree}"): tree,
                    ("status", "--porcelain", "--untracked-files=all"): dirty}[args]

        with mock.patch.object(CANDIDATE, "git", side_effect=git):
            CANDIDATE.pin(self.wrapper, self.crypto, self.crypto_pins,
                          self.crypto_paths, "reviewed-wrapper", "reviewed-crypto")

    def test_keeps_pinned_forks_and_exact_crypto_lock_closure(self):
        self.pin()
        manifest = tomllib.loads((self.wrapper / "Cargo.toml").read_text())
        for name in CANDIDATE.AIRBENDER_DEPS:
            dependency = manifest["workspace"]["dependencies"][name]
            self.assertEqual(dependency["git"], CANDIDATE.FORK)
            self.assertEqual(dependency["rev"], CANDIDATE.FORK_COMMIT)
            self.assertNotIn("tag", dependency)
        crypto_patch = manifest["patch"][self.crypto_pins["upstream_url"]]
        self.assertEqual(set(crypto_patch), CANDIDATE.WRAPPER_CRYPTO_PACKAGES)
        for name, patch in crypto_patch.items():
            self.assertEqual(patch, {"path": str(self.crypto / self.crypto_paths[name])})
        packages = tomllib.loads((self.wrapper / "Cargo.lock").read_text())["package"]
        self.assertEqual(len(packages), len(self.original["package"]))
        for before, after in zip(self.original["package"], packages):
            expected = before.copy()
            if expected.get("source") == self.crypto_source:
                del expected["source"]
            self.assertEqual(after, expected)
        self.assertNotIn("wrapper_patched_tree", CANDIDATE.SOURCE_IDENTITY)
        self.assertEqual(CANDIDATE.SOURCE_IDENTITY["wrapper_repository"],
                         "https://github.com/syscoin/zkos-wrapper.git")
        self.assertEqual(CANDIDATE.SOURCE_IDENTITY["wrapper_commit"],
                         "816a3598c60fac37d02cf1dcd1bc48b2e2c75de1")
        self.assertEqual(CANDIDATE.SOURCE_IDENTITY["wrapper_tree"],
                         "a1e9a551b9588b1dfab5c5088554fc69770c0e85")

    def test_rejects_upstream_airbender_manifest_or_lock(self):
        manifest_path = self.wrapper / "Cargo.toml"
        lock_path = self.wrapper / "Cargo.lock"
        for path, old, new in (
            (manifest_path, CANDIDATE.FORK, UPSTREAM_AIRBENDER),
            (lock_path, CANDIDATE.NEW_SOURCE, UPSTREAM_LOCK_SOURCE),
        ):
            with self.subTest(path=path):
                original = path.read_text()
                path.write_text(original.replace(old, new, 1))
                with self.assertRaises(ValueError):
                    self.pin()
                path.write_text(original)

    def test_rejects_missing_crypto_package_without_mutating_inputs(self):
        path = self.wrapper / "Cargo.lock"
        original = path.read_text()
        path.write_text(original.replace('name = "boojum"', 'name = "unknown"'))
        with self.assertRaisesRegex(ValueError, "unexpected wrapper proving lock graph"):
            self.pin()
        self.assertEqual((self.wrapper / "Cargo.toml").read_text().count(CANDIDATE.FORK), 7)

    def test_rejects_unreviewed_wrapper_fork_tree_without_mutating_inputs(self):
        with self.assertRaisesRegex(ValueError, "wrapper fork source changed"):
            self.pin(tree="different-tree")
        self.assertEqual((self.wrapper / "Cargo.lock").read_text().count(CANDIDATE.NEW_SOURCE), 44)

    def test_rejects_unreviewed_wrapper_fork_revision_or_dirty_source(self):
        for inputs in ({"head": "0" * 40}, {"dirty": " M wrapper/src/lib.rs\n"}):
            with self.subTest(inputs=inputs):
                with self.assertRaisesRegex(ValueError, "wrapper"):
                    self.pin(**inputs)
                self.assertNotIn("[patch.", (self.wrapper / "Cargo.toml").read_text())

    def test_rejects_unreviewed_crypto_fork_before_mutating_inputs(self):
        for inputs in ({"crypto_head": "0" * 40},
                       {"crypto_tree": "different-tree"},
                       {"crypto_dirty": " M crates/boojum/src/lib.rs\n"}):
            with self.subTest(inputs=inputs):
                with self.assertRaisesRegex(ValueError, "crypto fork source changed"):
                    self.pin(**inputs)
                self.assertNotIn("[patch.", (self.wrapper / "Cargo.toml").read_text())
                self.assertEqual((self.wrapper / "Cargo.lock").read_text().count(
                    self.crypto_source), 10)

    def downloaded_closures(self):
        self.pin()
        attest = self.root / "fresh-attest"
        a, b = (attest / lane / "proving-source-closure" for lane in ("a", "b"))
        CANDIDATE.export_source_closure(self.wrapper, self.crypto, a)
        producer_b = self.root / "producer-b"
        wrapper_b, crypto_b = producer_b / "zkos-wrapper", producer_b / "zksync-crypto"
        wrapper_b.mkdir(parents=True)
        crypto_b.mkdir()
        (wrapper_b / "Cargo.toml").write_text(
            (self.wrapper / "Cargo.toml").read_text().replace(str(self.crypto), str(crypto_b)))
        shutil.copyfile(self.wrapper / "Cargo.lock", wrapper_b / "Cargo.lock")
        CANDIDATE.export_source_closure(wrapper_b, crypto_b, b)
        # Model the real job boundary: only uploaded files survive in a fresh attest job.
        shutil.rmtree(self.wrapper)
        shutil.rmtree(self.crypto)
        shutil.rmtree(producer_b)
        (attest / "final").mkdir()
        return attest, a, b

    def test_fresh_attest_job_reconciles_only_downloaded_files(self):
        attest, a, b = self.downloaded_closures()
        workflow = (ROOT / ".github/workflows/syscoin-v32-v8-keygen.yml").read_text()
        block = workflow.split("      - name: Reconcile transferred candidate proving sources", 1)[1]
        block = block.split("      - name:", 1)[0]
        script = textwrap.dedent(block.split("        run: |\n", 1)[1])
        result = subprocess.run(["bash", "-c", script], cwd=attest, capture_output=True,
                                text=True, env={**os.environ, "WORK_DIR": str(attest),
                                                "GITHUB_WORKSPACE": str(ROOT),
                                                "PYTHONDONTWRITEBYTECODE": "1"}, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        record = json.loads((attest / "final/proving-source-closure.json").read_text())
        self.assertEqual(record["sources"], CANDIDATE.SOURCE_IDENTITY)
        self.assertNotEqual(record["manifest_sha256_by_lane"]["a"],
                            record["manifest_sha256_by_lane"]["b"])
        for lane, directory in (("a", a), ("b", b)):
            self.assertEqual(record["manifest_sha256_by_lane"][lane],
                             CANDIDATE.digest((directory / "Cargo.toml").read_bytes()))
        self.assertFalse((attest / "candidate-proving-sources").exists())
        self.assertFalse((attest / "prover-tooling").exists())

    def test_missing_transferred_input_fails_without_final_record(self):
        attest, a, b = self.downloaded_closures()
        (b / "Cargo.lock").unlink()
        output = attest / "final/proving-source-closure.json"
        with self.assertRaises(FileNotFoundError):
            CANDIDATE.reconcile_source_closures(a, b, output)
        self.assertFalse(output.exists())

    def test_tampered_transferred_manifest_or_lock_fails_without_final_record(self):
        attest, a, b = self.downloaded_closures()
        output = attest / "final/proving-source-closure.json"
        for name in ("Cargo.toml", "Cargo.lock"):
            with self.subTest(name=name):
                path = b / name
                original = path.read_bytes()
                path.write_bytes(original + b"\n# tampered\n")
                with self.assertRaisesRegex(ValueError, "hash mismatch"):
                    CANDIDATE.reconcile_source_closures(a, b, output)
                self.assertFalse(output.exists())
                path.write_bytes(original)

    def test_unexpected_transferred_source_identity_is_rejected(self):
        attest, a, b = self.downloaded_closures()
        path = b / "source-closure.json"
        original = json.loads(path.read_text())
        for name in CANDIDATE.SOURCE_IDENTITY:
            with self.subTest(name=name):
                record = {**original, "sources": {**original["sources"], name: "unreviewed"}}
                path.write_text(json.dumps(record))
                output = attest / "final/proving-source-closure.json"
                with self.assertRaisesRegex(ValueError, "source identity"):
                    CANDIDATE.reconcile_source_closures(a, b, output)
                self.assertFalse(output.exists())

    def test_self_consistent_but_different_normalized_graph_is_rejected(self):
        attest, a, b = self.downloaded_closures()
        path = b / "Cargo.toml"
        path.write_text(path.read_text() + "\n[profile.release]\nlto = true\n")
        metadata = b / "source-closure.json"
        record = json.loads(metadata.read_text())
        record["manifest_sha256"] = CANDIDATE.digest(path.read_bytes())
        record["normalized_manifest_sha256"] = CANDIDATE.digest(
            CANDIDATE.normalized_manifest(path.read_bytes(), record["prepared_crypto_root"]))
        metadata.write_text(json.dumps(record))
        with self.assertRaisesRegex(ValueError, "graphs differ"):
            CANDIDATE.reconcile_source_closures(a, b, attest / "final/proving-source-closure.json")

    def test_normalization_does_not_hide_crypto_path_or_airbender_pin_changes(self):
        _, _, b = self.downloaded_closures()
        record = json.loads((b / "source-closure.json").read_text())
        original = (b / "Cargo.toml").read_bytes()
        for raw in (original.replace(b"crates/boojum", b"crates/unreviewed"),
                    original.replace(CANDIDATE.FORK_COMMIT.encode(), b"0" * 40)):
            with self.subTest(raw=raw[:30]):
                with self.assertRaises(ValueError):
                    CANDIDATE.normalized_manifest(raw, record["prepared_crypto_root"])

    def test_final_attestation_reads_only_reconciled_source_artifact(self):
        workflow = (ROOT / ".github/workflows/syscoin-v32-v8-keygen.yml").read_text()
        block = workflow.split("      - name: Produce fail-closed server candidate", 1)[1]
        block = block.split("      - name:", 1)[0]
        self.assertNotIn("${WORK_DIR}/candidate-proving-sources", block)
        self.assertNotIn("${WORK_DIR}/prover-tooling", block)
        self.assertIn('--slurpfile candidate_sources "${source_closure}"', block)
        for field in ("repository", "commit", "tree"):
            self.assertIn(f"candidate_wrapper_{field}: $candidate_sources[0].sources.wrapper_{field}", block)
        self.assertNotIn("wrapper_patched_tree", block)
        self.assertIn("candidate_wrapper_manifest_sha256_by_lane:", block)
        for field in ("repository", "commit", "tree"):
            self.assertIn(f"common_crypto_{field}: $candidate_sources[0].sources.common_crypto_{field}", block)
        self.assertNotIn("common_crypto_patched_tree", block)
        self.assertEqual(CANDIDATE.SOURCE_IDENTITY["common_crypto_repository"],
                         "https://github.com/syscoin/zksync-crypto.git")
        self.assertEqual(CANDIDATE.SOURCE_IDENTITY["common_crypto_commit"],
                         "5eafa1f914519d88a13c577a9e8cc5c7db1dfc77")
        self.assertEqual(CANDIDATE.SOURCE_IDENTITY["common_crypto_tree"],
                         "c52391d0df82126d27e5c76810e9df85c61875d0")


if __name__ == "__main__":
    unittest.main()
