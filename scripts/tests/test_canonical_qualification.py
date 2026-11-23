import importlib.util
import hashlib
from pathlib import Path
import re
import subprocess
import unittest
from unittest.mock import Mock, patch


ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".github/workflows"
SPEC = importlib.util.spec_from_file_location(
    "check_canonical_qualification", ROOT / ".github/scripts/check-canonical-qualification.py"
)
GATE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GATE)

CHECKED = "1" * 40
OTHER = "2" * 40
TREE = "4" * 40
DESCRIPTOR = "5" * 64
ACCOUNT = 1234
CPU, CUDA = GATE.CONTEXTS.values()
DESCRIPTION = f"v1 tree={TREE} descriptor={DESCRIPTOR}"


def status(sha=CHECKED, **overrides):
    return {
        "creator": {"id": ACCOUNT},
        "context": CPU,
        "state": "success",
        "url": f"{GATE.API}/statuses/{sha}",
        "description": DESCRIPTION,
        **overrides,
    }


class StatusTests(unittest.TestCase):
    def verify(self, pages, context=CPU):
        GATE.verify(CHECKED, TREE, DESCRIPTOR, ACCOUNT, context,
                    lambda sha, page: pages.get((sha, page), []))

    def test_spoofed_creator_other_lane_and_other_commit_are_ignored(self):
        absent = [
            {(CHECKED, 1): [status(creator={"id": ACCOUNT + 1, "login": "claimed-qualifier"})]},
            {(CHECKED, 1): [status(creator={"id": str(ACCOUNT)})]},
            {(CHECKED, 1): [status(creator=None)]},
            {(CHECKED, 1): [status(context=CUDA)]},
            {(OTHER, 1): [status(sha=OTHER)]},
        ]
        for pages in absent:
            with self.subTest(pages=pages), self.assertRaisesRegex(ValueError, "missing trusted"):
                self.verify(pages)

    def test_newer_untrusted_failure_cannot_revoke_trusted_success(self):
        self.verify({(CHECKED, 1): [status(creator={"id": ACCOUNT + 1}, state="failure"), status()]})

    def test_newest_trusted_status_must_match_even_with_older_success(self):
        invalid = [
            status(state="pending"),
            status(state="failure"),
            status(context=CPU.upper()),
            status(sha=OTHER),
            status(description=DESCRIPTION.replace(TREE, "6" * 40)),
            status(description=DESCRIPTION.replace(DESCRIPTOR, "7" * 64)),
        ]
        for row in invalid:
            with self.subTest(row=row), self.assertRaisesRegex(ValueError, "latest trusted"):
                self.verify({(CHECKED, 1): [row, status()]})

    def test_newest_trusted_status_can_be_on_later_page(self):
        self.verify({(CHECKED, 1): [status(context="unrelated")] * 100,
                     (CHECKED, 2): [status()]})

    def test_exhausted_search_fails_closed(self):
        pages = {(CHECKED, page): [status(context="unrelated")] * 100 for page in range(1, 11)}
        with self.assertRaisesRegex(ValueError, "search exhausted"):
            self.verify(pages)

    def test_configured_creator_is_required(self):
        with patch("sys.argv", ["gate", "--source-sha", CHECKED]), patch.dict(
            "os.environ", {}, clear=True
        ):
            with self.assertRaisesRegex(ValueError, "approved numeric"):
                GATE.main()


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        identity = {("rev-parse", "HEAD"): CHECKED, ("diff", "--quiet", "HEAD", "--"): "",
                    ("rev-parse", "HEAD^{tree}"): TREE}
        self.git = patch.object(GATE, "git", side_effect=lambda *argv: identity[argv]).start()
        patch.object(GATE, "descriptor_hash", return_value=DESCRIPTOR).start()
        self.addCleanup(patch.stopall)

    def test_exact_release_requires_both_lanes_from_one_response(self):
        status_pages = Mock(return_value=[status(), status(context=CUDA)])
        with patch("builtins.print") as report:
            GATE.verify_release(CHECKED, ACCOUNT, status_pages)
        report.assert_called_once()
        status_pages.assert_called_once_with(CHECKED, 1)

    def test_one_lane_cannot_qualify_release(self):
        for lane in (CPU, CUDA):
            with self.subTest(lane=lane), self.assertRaisesRegex(ValueError, "missing trusted"):
                GATE.verify_release(CHECKED, ACCOUNT, lambda *_: [status(context=lane)])

    def test_qualifier_rejects_different_or_dirty_checkout_before_network(self):
        network = Mock()
        with self.assertRaisesRegex(ValueError, "exact release commit"):
            GATE.verify_release(OTHER, ACCOUNT, network)
        self.git.side_effect = [CHECKED, subprocess.CalledProcessError(1, "git diff")]
        with self.assertRaises(subprocess.CalledProcessError):
            GATE.verify_release(CHECKED, ACCOUNT, network)
        network.assert_not_called()

    def test_binding_failure_precedes_network(self):
        with patch.object(GATE, "descriptor_hash", side_effect=ValueError("pending fixture")):
            network = Mock()
            with self.assertRaisesRegex(ValueError, "pending fixture"):
                GATE.verify_release(CHECKED, ACCOUNT, network)
            network.assert_not_called()


class DescriptorTests(unittest.TestCase):
    def test_release_helper_is_read_as_source_data(self):
        app = b"bound application source"
        app_hash = hashlib.sha256(app).hexdigest()
        binding = {
            "descriptor_sha256": DESCRIPTOR,
            "versions_sha256": "6" * 64,
            "identity_sha256": dict.fromkeys(GATE.FIXTURE_IDENTITIES, "7" * 64),
        }
        files = dict.fromkeys(GATE.APP_SOURCE_PATHS, app)
        files[GATE.RELEASE_HELPER] = (
            "raise RuntimeError('release helpers must never run on the publication runner')\n"
            f"CANONICAL_BINDING = {binding!r}\n"
            f"APP_SOURCES = {dict.fromkeys(GATE.APP_SOURCE_PATHS, app_hash)!r}\n"
        ).encode()
        files["integration-tests/src/fixture_backend.rs"] = (
            f'const TRUSTED_DESCRIPTORS: &[(&str, &str)] = &[("v32.0", "{DESCRIPTOR}")];'
        ).encode()
        with patch.object(GATE, "blob", side_effect=files.__getitem__), patch.object(GATE, "git", return_value=""):
            self.assertEqual(GATE.descriptor_hash(), DESCRIPTOR)
            files[next(iter(GATE.APP_SOURCE_PATHS))] += b"modified"
            with self.assertRaisesRegex(ValueError, "source SHA-256 mismatch"):
                GATE.descriptor_hash()


class WorkflowTests(unittest.TestCase):
    def publishing_steps(self, workflow, name):
        text = (WORKFLOWS / workflow).read_text().split("\njobs:\n", 1)[1]
        job = re.search(rf"(?ms)^  {re.escape(name)}:\n(.*?)(?=^  [\w-]+:\n|\Z)", text)
        self.assertIsNotNone(job, f"{workflow} must keep the {name} job")
        fields, *steps = job[1].split("\n      - name: ")
        self.assertRegex(fields, r"(?m)^      statuses: read$")
        self.assertNotIn("continue-on-error:", fields)
        return steps

    def live_check_before(self, steps, mutation, checker, *, versioned=False, versioned_write=False):
        writes = [i for i, step in enumerate(steps) if mutation in step]
        self.assertEqual(len(writes), 1, mutation)
        index = writes[0]
        self.assertGreater(index, 0, mutation)
        write = steps[index]
        versioned_condition = "if: ${{ inputs.tag_name || github.ref_type == 'tag' }}"
        if versioned_write:
            self.assertIn(versioned_condition, write)
        else:
            self.assertNotIn("\n        if:", write)
        self.assertNotIn("continue-on-error:", write)
        gate = steps[index - 1]
        self.assertIn(f'python3 -I -B {checker}' + ' --source-sha "${RELEASE_SOURCE_SHA}"', gate)
        self.assertIn("GITHUB_TOKEN: ${{ github.token }}", gate)
        self.assertIn("SYSCOIN_CANONICAL_QUALIFIER_ID: ${{ vars.SYSCOIN_CANONICAL_QUALIFIER_ID }}", gate)
        self.assertNotIn("continue-on-error:", gate)
        if versioned:
            self.assertIn(versioned_condition, gate)
        else:
            self.assertNotIn("\n        if:", gate)
        return gate

    def trusted_checkout(self, steps, path):
        checkouts = [s for s in steps if "ref: ${{ job.workflow_sha }}" in s
                     and (path is None or f"\n          path: {path}\n" in s)]
        self.assertEqual(len(checkouts), 1)
        self.assertIn("uses: actions/checkout@", checkouts[0])
        self.assertIn("repository: ${{ job.workflow_repository }}", checkouts[0])
        return checkouts[0]

    def test_release_ref_and_metadata_publication_recheck_exact_attestations(self):
        steps = self.publishing_steps("release-please.yml", "publish")
        checkout = self.trusted_checkout(steps, "release-tooling")
        self.assertIn(".github/scripts/check-canonical-qualification.py", checkout)
        gate = self.live_check_before(
            steps, "scripts/release/prepare-qualified-release.py publish",
            "../release-tooling/.github/scripts/check-canonical-qualification.py",
        )
        self.assertIn("working-directory: release-source", gate)
        sha = "${{ needs.release-please.outputs.source_sha }}"
        self.assertIn(f"RELEASE_SOURCE_SHA: {sha}", gate)
        self.assertTrue(any(f"ref: {sha}" in step and "path: release-source" in step for step in steps))
        self.assertTrue(any(f"SELECTED_SOURCE_SHA: {sha}" in step for step in steps))

    def test_binary_publication_and_recovery_recheck_exact_attestations(self):
        for workflow, job, tooling_path, checker, write in (
            ("release-bins.yml", "publish-bins", ".release-publication-tooling",
             ".release-publication-tooling/.github/scripts/check-canonical-qualification.py",
             "scripts/release/publish-release-assets.py"),
            ("release-bins.yml", "publish-bins", ".release-publication-tooling",
             ".release-publication-tooling/.github/scripts/check-canonical-qualification.py",
             'gh release edit "${RELEASE_TAG}"'),
            ("resume-release-bins.yml", "resume", None,
             "../.github/scripts/check-canonical-qualification.py",
             "scripts/release/resume-release-bins.py publish staged-release"),
        ):
            with self.subTest(workflow=workflow, write=write):
                steps = self.publishing_steps(workflow, job)
                self.trusted_checkout(steps, tooling_path)
                gate = self.live_check_before(steps, write, checker)
                self.assertIn("RELEASE_SOURCE_SHA: ${{ inputs.source_sha }}", (WORKFLOWS / workflow).read_text())
                preceding = "\n".join(steps[:steps.index(gate)])
                self.assertIn("ref: refs/tags/${{ inputs.tag }}", preceding)
                self.assertIn('^{commit}', preceding)
                self.assertIn('== "${RELEASE_SOURCE_SHA}"', preceding)
                if tooling_path is None:
                    self.assertIn("path: release-source", preceding)
                    self.assertIn("working-directory: release-source", gate)

    def test_pr_main_and_stress_keep_compilation_without_private_status_checks(self):
        ci = (WORKFLOWS / "ci.yml").read_text()
        self.assertIn("  pull_request:", ci)
        self.assertIn("     - main", ci)
        self.assertIn("     - 'ci/stress/**'", ci)
        self.assertIn("nextest archive --locked --release", ci)
        self.assertIn("        build-prover-tests,", ci)
        for private in ("release-qualification.yml", "SYSCOIN_CANONICAL_QUALIFIER_ID",
                        "canonical-restore-smoke", "        prover-tests,"):
            self.assertNotIn(private, ci)

    def test_manual_route_and_binary_release_use_same_exact_read_only_gate(self):
        qualification = (WORKFLOWS / "release-qualification.yml").read_text()
        self.assertIn("  workflow_dispatch:", qualification)
        self.assertIn("  workflow_call:", qualification)
        self.assertIn("  statuses: read", qualification)
        self.assertNotIn("write", qualification)
        self.assertIn(' --source-sha "${RELEASE_SOURCE_SHA}"', qualification)
        for route, prereq in (("release-bins.yml", "  preflight:\n"),
                              ("resume-release-bins.yml", "  resume:\n")):
            workflow = (WORKFLOWS / route).read_text()
            self.assertIn("uses: ./.github/workflows/release-qualification.yml", workflow)
            self.assertIn("source_sha: ${{ inputs.source_sha }}", workflow)
            self.assertIn("needs: qualify", workflow.split(prereq, 1)[1])

    def test_versioned_docker_qualification_precedes_publication_and_keeps_main_latest(self):
        docker = (WORKFLOWS / "docker.yml").read_text()
        build_job = docker.split("\n  build-images:\n", 1)[1].split("\n  publish-image:\n", 1)[0]
        self.assertNotIn("write", build_job)
        self.assertNotIn("docker/login-action@", build_job)
        self.assertNotIn("secrets.", build_job)
        self.assertIn("actions/upload-artifact@", build_job)
        builds = [step for step in build_job.split("\n      - name: ") if "uses: docker/build-push-action@" in step]
        self.assertEqual(len(builds), 1)
        build = builds[0]
        self.assertIn("push: false", build)
        self.assertIn("outputs: type=oci,tar=false,dest=${{ runner.temp }}/server-image", build)
        self.assertEqual(
            [line.strip() for line in build.splitlines() if line.strip().startswith("tags:")],
            ["tags: ${{ needs.resolve-source.outputs.image_name }}:${{ needs.resolve-source.outputs.version }}"],
        )
        self.assertNotIn("provenance:", build)

        steps = self.publishing_steps("docker.yml", "publish-image")
        self.trusted_checkout(steps, "qualification-tooling")
        self.assertTrue(any("artifact-ids: ${{ needs.build-images.outputs.artifact_id }}" in step for step in steps))
        self.assertFalse(any("uses: docker/build-push-action@" in step for step in steps))
        checker = "../qualification-tooling/.github/scripts/check-canonical-qualification.py"
        push = self.live_check_before(
            steps, "skopeo copy --all --preserve-digests", checker, versioned=True,
        )
        attest = self.live_check_before(steps, "push-to-registry: true", checker, versioned=True)
        for gate in (push, attest):
            self.assertIn("RELEASE_SOURCE_SHA: ${{ needs.resolve-source.outputs.source_sha }}", gate)
            self.assertIn("working-directory: release-source", gate)
        publish = next(step for step in steps if "skopeo copy --all --preserve-digests" in step)
        self.assertIn('publish_tag "${VERSION}"', publish)
        self.assertRegex(publish, r'if \[\[ "\$\{PUBLISH_LATEST\}" == true \]\]; then\s+publish_tag latest\s+fi')
        self.assertIn("github.event_name == 'push' && github.ref == 'refs/heads/main' && inputs.tag_name == ''",
                      publish)


if __name__ == "__main__":
    unittest.main()
