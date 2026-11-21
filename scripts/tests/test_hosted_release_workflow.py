"""Check hosted server release boundaries without publishing or fixture restore."""

import os
import json
from pathlib import Path
import subprocess
import tarfile
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
        (self.repo_path / "scripts").mkdir()
        self.checker = self.repo_path / "scripts/apply-era-contracts-syscoin-release.py"
        self.checker.write_text(
            'import argparse\n'
            'parser = argparse.ArgumentParser()\n'
            'mode = parser.add_mutually_exclusive_group(required=True)\n'
            'mode.add_argument("--check-canonical-fixture-binding", action="store_true")\n'
            'if not parser.parse_args().check_canonical_fixture_binding:\n'
            '    raise RuntimeError("must not read the private fixture")\n')
        self.commit_source("initial")

    def commit_source(self, message):
        subprocess.run(["git", "add", "scripts", "local-chains"] if (self.repo_path / "local-chains").exists()
                       else ["git", "add", "scripts"], cwd=self.repo_path, check=True)
        subprocess.run(["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                        "commit", "-qm", message], cwd=self.repo_path, check=True)
        self.sha = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.repo_path, text=True).strip()
        subprocess.run(["git", "tag", "-f", "v1"], cwd=self.repo_path, check=True, stdout=subprocess.PIPE)

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

    def test_current_source_policy_matches_checked_out_marker(self):
        marker = ROOT / "local-chains/v32.0/CANONICAL_V8_REGENERATION_REQUIRED"
        result = subprocess.run(["python3", "scripts/apply-era-contracts-syscoin-release.py",
                                 "--check-canonical-fixture-binding"], cwd=ROOT, text=True, capture_output=True)
        if os.path.lexists(marker):
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("canonical fixture regeneration marker is present", result.stderr)
        else:
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout)["fixture_mode"], "private")

    def test_bound_private_source_disables_public_fixture_output(self):
        result = self.run_guard(self.preflight)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.repo_path / "output").read_text(), "ready=false\nmode=private\n")
        self.assertIn("do not open or package it", (self.repo_path / "summary").read_text())
        archive = self.workflow.split("      - name: Archive certified local chains\n", 1)[1].split(
            "      - name:", 1)[0]
        self.assertIn("git archive --format=tar.gz", archive)

    def test_invalid_private_binding_never_uses_legacy_fallback(self):
        with self.checker.open("a") as output:
            output.write('raise SystemExit("invalid binding")\n')
        self.commit_source("invalid binding")
        result = self.run_guard(self.preflight)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("invalid binding", result.stderr)
        self.assertFalse((self.repo_path / "output").exists())

    def test_legacy_requires_existing_checker_and_tracked_anvil_state(self):
        self.checker.write_text(
            "import os,sys\n"
            "assert sys.argv[1:] == ['--check-canonical-fixture']\n"
            "assert 'SYSCOIN_V32_CANONICAL_FIXTURE_DIR' not in os.environ\n")
        fixture = self.repo_path / "local-chains/v32.0"
        fixture.mkdir(parents=True)
        (fixture / "l1-state.json.gz").write_bytes(b"test-only")
        self.commit_source("legacy exact tagged tooling")
        result = self.run_guard(self.preflight)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.repo_path / "output").read_text(), "ready=true\nmode=legacy-public\n")
        (fixture / "untracked-capture").write_bytes(b"synthetic fixture sentinel")
        (self.repo_path / "dist").mkdir()
        command = self.workflow.split("      - name: Archive certified local chains\n", 1)[1].split(
            "        run: ", 1)[1].splitlines()[0]
        env = {**os.environ, "RELEASE_SOURCE_SHA": self.sha, "LOCAL_CHAINS_DIR": "local-chains"}
        subprocess.run(["bash", "-e", "-c", command], cwd=self.repo_path, env=env, check=True)
        with tarfile.open(self.repo_path / "dist/local-chains.tar.gz") as archive:
            self.assertIn("local-chains/v32.0/l1-state.json.gz", archive.getnames())
            self.assertNotIn("local-chains/v32.0/untracked-capture", archive.getnames())
        (fixture / "untracked-capture").unlink()
        (fixture / "l1-backend.json").write_text("{}")
        self.assertNotEqual(self.run_guard(self.preflight).returncode, 0)
        (fixture / "l1-backend.json").unlink()
        (fixture / "l1-state.json.gz").unlink()
        self.commit_source("missing legacy state")
        self.assertNotEqual(self.run_guard(self.preflight).returncode, 0)

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

    def test_publication_uses_successfully_staged_attested_archives(self):
        staging = self.workflow.split("  release-bins:\n", 1)[1].split("  publish-bins:\n", 1)[0]
        publishing = self.workflow.split("  publish-bins:\n", 1)[1]
        self.assertIn("needs: [preflight, build-bins, test-bins]", staging)
        self.assertIn("needs: [preflight, release-bins]", publishing)
        self.assertLess(staging.index("      - name: Validate complete release inventory"),
                        staging.index("      - name: Binaries attestation"))
        self.assertLess(staging.index("      - name: Binaries attestation"),
                        staging.index("      - name: Stage complete release archives"))
        self.assertLess(staging.index("      - name: Stage complete release archives"),
                        staging.index("      - name: Upload staged release archives"))
        self.assertNotIn("      - name: Upload immutable release artifacts", staging)
        artifact = "name: server-release-${{ inputs.tag }}-${{ inputs.source_sha }}"
        self.assertIn(artifact, staging)
        self.assertIn(artifact, publishing)
        self.assertLess(publishing.index("      - name: Download staged release archives"),
                        publishing.index("      - name: Validate staged release identity"))
        self.assertLess(publishing.index("      - name: Validate staged release identity"),
                        publishing.index("      - name: Upload immutable release artifacts"))
        self.assertIn("${{ needs.release-bins.outputs.attestation_url }}", publishing)

    def test_publication_rejects_wrong_staged_identity_and_inventory(self):
        script = step_shell(self.workflow, "Validate staged release identity")
        tag = "v1"
        sha = "a" * 40
        url = "https://github.com/example/attestation/1"
        env = {**os.environ, "RELEASE_TAG": tag, "RELEASE_SOURCE_SHA": sha,
               "FIXTURE_READY": "false", "GITHUB_RUN_ID": "123", "ATTESTATION_URL": url}
        names = (
            "zksync-os-server-v1-x86_64-unknown-linux-gnu.tar.gz",
            "zksync-os-server-v1-aarch64-unknown-linux-gnu.tar.gz",
            "zksync-os-server-v1-universal-apple-darwin.tar.gz",
        )

        def run(metadata_change=None, remove=None, extra=None, url_value=url, symlink=False):
            with tempfile.TemporaryDirectory(dir=self.repo_path) as directory:
                root = Path(directory)
                staged = root / "staged-release"
                dist = staged / "dist"
                dist.mkdir(parents=True)
                for name in names:
                    (dist / name).write_bytes(b"archive")
                if remove:
                    (dist / remove).unlink()
                if extra:
                    (dist / extra).write_bytes(b"unexpected")
                if symlink:
                    (dist / names[0]).unlink()
                    (dist / names[0]).symlink_to(dist / names[1])
                metadata = {"tag": tag, "source_sha": sha, "fixture_ready": "false", "run_id": "123"}
                metadata.update(metadata_change or {})
                (staged / "metadata.json").write_text(json.dumps(metadata))
                (staged / "attestation-url.txt").write_text(url_value + "\n")
                result = subprocess.run(["bash", "-c", script], cwd=root, env=env,
                                        text=True, capture_output=True)
                return result.returncode

        self.assertEqual(run(), 0)
        for key, value in (("tag", "v2"), ("source_sha", "b" * 40),
                           ("fixture_ready", "true"), ("run_id", "124")):
            self.assertNotEqual(run(metadata_change={key: value}), 0, key)
        self.assertNotEqual(run(url_value="https://example.invalid"), 0)
        self.assertNotEqual(run(remove=names[0]), 0)
        self.assertNotEqual(run(extra="unexpected.tar.gz"), 0)
        self.assertNotEqual(run(symlink=True), 0)

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
