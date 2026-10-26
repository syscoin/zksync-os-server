"""Exercise the release gate with controlled staged workspace inputs."""
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("release_inputs", ROOT / "scripts/check-server-release-inputs.py")
RELEASE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RELEASE)


class ReleaseInputsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        self.write("Cargo.toml", '[workspace]\nmembers = ["one"]\n[workspace.package]\nversion = "0.1.0"\n')
        self.write("one/Cargo.toml", '[package]\nname = "one"\nversion.workspace = true\n')
        self.write(".github/release-please/manifest.json", '{".": "0.1.0"}\n')
        self.write("Cargo.lock", 'version = 4\n[[package]]\nname = "one"\nversion = "0.1.0"\n')
        self.write("scripts/security/native-linux-review.json", '{}\n')
        subprocess.run(["git", "-C", str(self.root), "add", "."], check=True)
        self.update_policy()

    def write(self, name, data):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(data)

    def update_policy(self):
        policy = {
            "schema": 1,
            "target": RELEASE.NATIVE_AUDIT.TARGET,
            "lock_sha256": hashlib.sha256((self.root / "Cargo.lock").read_bytes()).hexdigest(),
            "source_sha256": RELEASE.NATIVE_AUDIT.source_fingerprint(self.root),
        }
        self.write("scripts/security/native-linux-review.json", json.dumps(policy))

    def test_valid_inputs(self):
        RELEASE.validate(self.root)

    def test_release_please_version_bump_without_lock_fails(self):
        self.write("Cargo.toml", '[workspace]\nmembers = ["one"]\n[workspace.package]\nversion = "0.2.0"\n')
        self.write(".github/release-please/manifest.json", '{".": "0.2.0"}\n')
        with self.assertRaisesRegex(ValueError, "Cargo.lock workspace package version"):
            RELEASE.validate(self.root)

    def test_manifest_must_match_workspace(self):
        self.write(".github/release-please/manifest.json", '{".": "0.2.0"}\n')
        with self.assertRaisesRegex(ValueError, "manifest and workspace"):
            RELEASE.validate(self.root)

    def test_unreviewed_lock_change_fails(self):
        with (self.root / "Cargo.lock").open("a") as output:
            output.write("\n# changed\n")
        with self.assertRaisesRegex(ValueError, "lock review is stale"):
            RELEASE.validate(self.root)

    def test_unreviewed_source_change_fails(self):
        with (self.root / "one/Cargo.toml").open("a") as output:
            output.write("\n# changed\n")
        with self.assertRaisesRegex(ValueError, "source review is stale"):
            RELEASE.validate(self.root)

    def test_fresh_release_inputs_need_review_refresh(self):
        self.write("Cargo.toml", '[workspace]\nmembers = ["one"]\n[workspace.package]\nversion = "0.2.0"\n')
        self.write(".github/release-please/manifest.json", '{".": "0.2.0"}\n')
        self.write("Cargo.lock", 'version = 4\n[[package]]\nname = "one"\nversion = "0.2.0"\n')
        with self.assertRaisesRegex(ValueError, "lock review is stale"):
            RELEASE.validate(self.root)
        # Simulate an explicit review of the changed source and lock. Actual
        # release policy also revalidates the feature graph and advisories.
        self.update_policy()
        RELEASE.validate(self.root)

    def test_release_fixture_is_blocked_when_missing_or_marked(self):
        self.assertFalse(RELEASE.fixture_ready(self.root))
        self.write("local-chains/v32.0/l1-state.json.gz", "test")
        self.write("local-chains/v32.0/default/config.yaml", "test")
        self.write("local-chains/v32.0/default/genesis.json", "test")
        self.assertTrue(RELEASE.fixture_ready(self.root))
        self.write("local-chains/v32.0/CANONICAL_V8_REGENERATION_REQUIRED", "blocked")
        self.assertFalse(RELEASE.fixture_ready(self.root))

    def test_workflow_checks_fixture_before_tagging_and_binds_release_sha(self):
        workflow = (ROOT / ".github/workflows/release-please.yml").read_text()
        self.assertLess(workflow.index("Check canonical release fixture readiness"),
                        workflow.index("Prepare release"))
        self.assertIn("skip-github-release: ${{ steps.fixture.outputs.skip }}", workflow)
        self.assertIn("target-branch: main", workflow)
        self.assertIn("if: ${{ github.ref == 'refs/heads/main' }}", workflow)
        self.assertEqual(workflow.count("source_sha: ${{ needs.release-please.outputs.source_sha }}"), 2)


if __name__ == "__main__":
    unittest.main()
