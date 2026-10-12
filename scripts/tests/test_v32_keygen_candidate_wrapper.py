"""No-build checks for the candidate keygen's patched proving source graph."""

import importlib.util
from pathlib import Path
from unittest import mock
import tempfile
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "candidate_wrapper", ROOT / "scripts/keygen/prepare-candidate-wrapper.py")
CANDIDATE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CANDIDATE)


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
        self.crypto_paths = {name: "crates/" + name for name in
                             self.crypto_pins["upstream_packages"]}
        manifest = "[workspace]\n[workspace.dependencies]\n"
        for name in sorted(CANDIDATE.AIRBENDER_DEPS):
            manifest += (name + ' = { git = "' + CANDIDATE.UPSTREAM
                         + '", tag = "v0.6.0-rc.2" }\n')
        (self.wrapper / "Cargo.toml").write_text(manifest)
        lock = "version = 4\n"
        for index in range(44):
            lock += (f'\n[[package]]\nname = "air-{index}"\nversion = "0.1.0"\n'
                     + 'source = "' + CANDIDATE.OLD_SOURCE + '"\n')
        for name in sorted(CANDIDATE.WRAPPER_CRYPTO_PACKAGES):
            lock += (f'\n[[package]]\nname = "{name}"\nversion = "0.32.10"\n'
                     + 'source = "' + self.crypto_source + '"\n')
        lock += ('\n[[package]]\nname = "unrelated"\nversion = "1.0.0"\n'
                 'source = "registry+https://github.com/rust-lang/crates.io-index"\n')
        (self.wrapper / "Cargo.lock").write_text(lock)
        self.original = tomllib.loads(lock)

    def pin(self, tree="reviewed-wrapper"):
        def git(_directory, *args):
            return {("rev-parse", "HEAD"): CANDIDATE.WRAPPER_COMMIT,
                    ("write-tree",): tree}[args]

        with mock.patch.object(CANDIDATE, "git", side_effect=git):
            CANDIDATE.pin(self.wrapper, self.crypto, self.crypto_pins,
                          self.crypto_paths, "reviewed-wrapper", "reviewed-wrapper")

    def test_pins_fork_and_exact_patched_crypto_lock_closure(self):
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
            if expected.get("source") == CANDIDATE.OLD_SOURCE:
                expected["source"] = CANDIDATE.NEW_SOURCE
            elif expected.get("source") == self.crypto_source:
                del expected["source"]
            self.assertEqual(after, expected)

    def test_rejects_missing_crypto_package_without_mutating_inputs(self):
        path = self.wrapper / "Cargo.lock"
        original = path.read_text()
        path.write_text(original.replace('name = "boojum"', 'name = "unknown"'))
        with self.assertRaisesRegex(ValueError, "unexpected wrapper proving lock graph"):
            self.pin()
        self.assertEqual((self.wrapper / "Cargo.toml").read_text().count(CANDIDATE.FORK), 0)

    def test_rejects_unreviewed_patched_tree_without_mutating_inputs(self):
        with self.assertRaisesRegex(ValueError, "patch tree changed"):
            self.pin(tree="different-tree")
        self.assertEqual((self.wrapper / "Cargo.lock").read_text().count(CANDIDATE.NEW_SOURCE), 0)


if __name__ == "__main__":
    unittest.main()
