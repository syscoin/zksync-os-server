"""Check hosted server release boundaries without publishing or fixture restore."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/release-bins.yml"
PR_CHECK = ROOT / ".github/workflows/release-build-check.yml"
ACTION = ROOT / ".github/actions/build-release-server/action.yml"


def step_shell(text, name):
    block = text.split(f"      - name: {name}\n", 1)[1]
    block = block.split("      - name:", 1)[0]
    body = block.split("        run: |\n", 1)[1]
    lines = []
    for line in body.splitlines():
        if line and not line.startswith("          "):
            break
        lines.append(line[10:] if line else "")
    return "\n".join(lines)


class HostedReleaseWorkflowTests(unittest.TestCase):
    def setUp(self):
        self.workflow = WORKFLOW.read_text()
        self.identity = step_shell(self.workflow, "Validate requested release identity")
        self.preflight = step_shell(self.workflow, "Verify tag and fixture readiness")
        self.repo = tempfile.TemporaryDirectory()
        self.addCleanup(self.repo.cleanup)
        self.repo_path = Path(self.repo.name)
        subprocess.run(["git", "init", "-q", str(self.repo_path)], check=True)
        subprocess.run(["git", "-C", str(self.repo_path), "-c", "user.name=Test", "-c",
                        "user.email=test@example.invalid", "commit", "--allow-empty", "-qm", "initial"], check=True)
        self.sha = subprocess.check_output(["git", "-C", str(self.repo_path), "rev-parse", "HEAD"], text=True).strip()
        subprocess.run(["git", "-C", str(self.repo_path), "tag", "v1"], check=True)
        (self.repo_path / "scripts").mkdir()
        (self.repo_path / "scripts/apply-era-contracts-syscoin-release.py").write_text(
            "import sys\nassert sys.argv[1:] == ['--check-canonical-fixture']\n")

    def run_guard(self, script, tag="v1", sha=None):
        env = {**os.environ, "RELEASE_TAG": tag, "RELEASE_SOURCE_SHA": sha or self.sha,
               "GITHUB_OUTPUT": str(self.repo_path / "output"),
               "GITHUB_STEP_SUMMARY": str(self.repo_path / "summary")}
        return subprocess.run(["bash", "-c", script], cwd=self.repo_path, env=env,
                              text=True, capture_output=True)

    def test_release_input_validation_rejects_ambiguous_or_shell_tags(self):
        self.assertEqual(self.run_guard(self.identity).returncode, 0)
        for invalid in ("../v1", "v1;echo", "-v1", "v1/other", "v1\ncmd"):
            self.assertNotEqual(self.run_guard(self.identity, tag=invalid).returncode, 0, invalid)
        self.assertNotEqual(self.run_guard(self.identity, sha="a" * 39).returncode, 0)
        self.assertNotEqual(self.run_guard(self.identity, sha="g" * 40).returncode, 0)

    def test_preflight_requires_both_checkout_and_tag_to_match(self):
        self.assertEqual(self.run_guard(self.preflight).returncode, 0)
        self.assertNotEqual(self.run_guard(self.preflight, sha="a" * 40).returncode, 0)
        subprocess.run(["git", "-C", str(self.repo_path), "-c", "user.name=Test", "-c",
                        "user.email=test@example.invalid", "commit", "--allow-empty", "-qm", "new"], check=True)
        self.assertNotEqual(self.run_guard(self.preflight).returncode, 0)

    def test_current_marker_fails_the_real_fixture_check_without_consuming_it(self):
        marker = ROOT / "local-chains/v32.0/CANONICAL_V8_REGENERATION_REQUIRED"
        if not marker.exists():
            self.skipTest("canonical fixture regeneration marker has been removed")
        result = subprocess.run(["python3", "scripts/apply-era-contracts-syscoin-release.py",
                                 "--check-canonical-fixture"], cwd=ROOT, text=True, capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("canonical fixture regeneration marker is present", result.stderr)

    def test_marker_omits_fixture_but_keeps_binary_release_available(self):
        marker = self.repo_path / "local-chains/v32.0/CANONICAL_V8_REGENERATION_REQUIRED"
        marker.parent.mkdir(parents=True)
        marker.write_text("pending")
        checker = self.repo_path / "scripts/apply-era-contracts-syscoin-release.py"
        checker.write_text("raise RuntimeError('must not consume pending fixture')\n")
        self.assertEqual(self.run_guard(self.preflight).returncode, 0)
        self.assertIn("ready=false", (self.repo_path / "output").read_text())
        self.assertIn("block-production smoke and fixture archive are omitted", (self.repo_path / "summary").read_text())
        smoke = self.workflow.split("      - name: Check block production with certified canonical fixture\n", 1)[1]
        self.assertIn("if: ${{ needs.preflight.outputs.fixture_ready == 'true' }}", smoke.split("        env:", 1)[0])
        archive = self.workflow.split("      - name: Archive certified local chains\n", 1)[1]
        self.assertIn("if: ${{ needs.preflight.outputs.fixture_ready == 'true' }}", archive.split("        run:", 1)[0])
        self.assertIn("block-production smoke was not run", self.workflow)
        self.assertNotIn("--clobber", self.workflow)

    def test_fixture_free_config_smoke_uses_empty_working_directory(self):
        for path in (WORKFLOW, PR_CHECK):
            source = path.read_text()
            self.assertIn('smoke_dir="$(mktemp -d ', source)
            self.assertIn('cd "${smoke_dir}"', source)
            self.assertIn('config help > config-help.txt', source)
            self.assertIn('test -s config-help.txt', source)
            self.assertIn('--version', source)
            self.assertIn('ldd ', source)
            self.assertIn('otool -L', source)

    def test_release_inventory_rejects_missing_or_extra_files(self):
        script = step_shell(self.workflow, "Validate complete release inventory")
        dist = self.repo_path / "dist"
        dist.mkdir()
        names = (
            "zksync-os-server-v1-x86_64-unknown-linux-gnu.tar.gz",
            "zksync-os-server-v1-aarch64-unknown-linux-gnu.tar.gz",
            "zksync-os-server-v1-universal-apple-darwin.tar.gz",
            "local-chains.tar.gz",
        )
        env = {**os.environ, "TARGET_TAG": "v1", "FIXTURE_READY": "true"}
        def run():
            return subprocess.run(["bash", "-c", script], cwd=self.repo_path, env=env,
                                  text=True, capture_output=True).returncode
        for name in names:
            (dist / name).write_bytes(b"archive")
        self.assertEqual(run(), 0)
        (dist / names[0]).unlink()
        self.assertNotEqual(run(), 0)
        (dist / names[0]).write_bytes(b"archive")
        (dist / "unexpected.tar.gz").write_bytes(b"archive")
        self.assertNotEqual(run(), 0)
        (dist / "unexpected.tar.gz").unlink()
        (dist / names[0]).unlink()
        (dist / names[0]).symlink_to(dist / names[1])
        self.assertNotEqual(run(), 0)
        (dist / names[0]).unlink()
        (dist / names[0]).write_bytes(b"archive")
        (dist / "local-chains.tar.gz").unlink()
        env["FIXTURE_READY"] = "false"
        self.assertEqual(run(), 0)
        (dist / "local-chains.tar.gz").write_bytes(b"archive")
        self.assertNotEqual(run(), 0)

    def test_all_release_targets_use_standard_hosted_runners(self):
        block = self.workflow.split("  build-bins:\n", 1)[1].split("  test-bins:\n", 1)[0]
        for runner, target in (("ubuntu-22.04", "x86_64-unknown-linux-gnu"),
                               ("ubuntu-22.04-arm", "aarch64-unknown-linux-gnu"),
                               ("macos-15-intel", "x86_64-apple-darwin"),
                               ("macos-15", "aarch64-apple-darwin")):
            self.assertIn(f"runner: {runner}\n            target: {target}", block)
        self.assertNotIn("matterlabs-ci", self.workflow)
        self.assertIn("uses: ./.release-tooling/.github/actions/build-release-server", block)
        self.assertIn("uses: ./.github/actions/build-release-server", PR_CHECK.read_text())
        self.assertIn("scripts/cargo-with-patched-zksync-os.sh", ACTION.read_text())
        self.assertIn("build --locked --profile maxperf", ACTION.read_text())


if __name__ == "__main__":
    unittest.main()
