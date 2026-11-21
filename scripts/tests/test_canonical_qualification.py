import importlib.util
from pathlib import Path
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


class WorkflowTests(unittest.TestCase):
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
        stage = docker.split("Verify canonical qualification for a versioned release", 1)[1]
        self.assertIn("if: ${{ inputs.tag_name || github.ref_type == 'tag' }}", stage)
        self.assertIn("RELEASE_SOURCE_SHA: ${{ steps.source.outputs.source_sha }}", stage)
        self.assertLess(docker.index("Verify canonical qualification"),
                        docker.index("Login to GitHub Container Registry"))
        self.assertIn("type=raw,value=latest,enable=", docker)
        self.assertIn("github.event_name == 'push' && github.ref == 'refs/heads/main' && inputs.tag_name == ''",
                      docker)


if __name__ == "__main__":
    unittest.main()
