"""No-download regressions for filtered, exact-pinned workflow source checkouts."""

from pathlib import Path
import re
import shlex
import unittest


WORKFLOW = Path(__file__).resolve().parents[2] / ".github/workflows/syscoin-v32-v8-keygen.yml"
SOURCES = (
    ("${ZKSYNC_OS_REPOSITORY}", "${source_dir}", "${ZKSYNC_OS_BASE_COMMIT}"),
    ("${AIRBENDER_REPOSITORY}", "${WORK_DIR}/airbender", "${AIRBENDER_COMMIT}"),
    ("${PROVER_TOOLING_REPOSITORY}", "${WORK_DIR}/prover-tooling", "${PROVER_TOOLING_COMMIT}"),
    ("${WRAPPER_REPOSITORY}", "${WORK_DIR}/wrapper", "${WRAPPER_COMMIT}"),
    ("${PROVER_REPOSITORY}", "${WORK_DIR}/prover", "${PROVER_COMMIT}"),
    ("${ERA_REPOSITORY}", "${era_dir}", "${ERA_BASE_COMMIT}"),
)


class V32KeygenSourceFetchTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = WORKFLOW.read_text()
        cls.clones = re.findall(r"^          (git clone [^\n]+)$", cls.workflow, re.MULTILINE)

    def test_all_six_sources_skip_unrelated_blobs_and_default_checkout(self):
        self.assertEqual(len(self.clones), len(SOURCES))
        for command, (repository, destination, _) in zip(self.clones, SOURCES):
            with self.subTest(repository=repository):
                self.assertEqual(shlex.split(command), [
                    "git", "clone", "--filter=blob:none", "--no-checkout", "--no-tags",
                    repository, destination,
                ])

    def test_filtered_checkouts_retain_full_ancestry(self):
        for command in self.clones:
            with self.subTest(command=command):
                self.assertNotRegex(command, r"--(?:depth|shallow-since|shallow-exclude|single-branch)\b")

    def test_each_source_still_checks_out_and_authenticates_its_exact_commit(self):
        for command, (_, destination, commit) in zip(self.clones, SOURCES):
            with self.subTest(destination=destination):
                checkout = f'git -C "{destination}" checkout --detach "{commit}"'
                identity = f'[[ "$(git -C "{destination}" rev-parse HEAD)" == "{commit}" ]]'
                start = self.workflow.index(command) + len(command)
                end = self.workflow.find("\n      - name:", start)
                block = self.workflow[start:end if end >= 0 else None]
                self.assertIn(checkout, block)
                self.assertIn(identity, block)
                self.assertLess(block.index(checkout), block.index(identity))

    def test_existing_reviewed_tree_and_era_submodule_checks_remain(self):
        for destination, tree in (
            ("${source_dir}", "${ZKSYNC_OS_BASE_TREE}"),
            ("${WORK_DIR}/airbender", "${AIRBENDER_TREE}"),
            ("${WORK_DIR}/prover-tooling", "${PROVER_TOOLING_TREE}"),
            ("${era_dir}", "${ERA_BASE_TREE}"),
        ):
            with self.subTest(destination=destination):
                self.assertIn(
                    f'[[ "$(git -C "{destination}" rev-parse \'HEAD^{{tree}}\')" == "{tree}" ]]',
                    self.workflow,
                )
        self.assertIn('git -C "${era_dir}" submodule update --init', self.workflow)
        self.assertIn('"${ERA_NESTED_COMMIT}" ]]', self.workflow)

    def test_ci_explicitly_runs_the_fetch_regressions(self):
        ci = WORKFLOW.with_name("ci.yml").read_text()
        self.assertIn("scripts.tests.test_v32_keygen_source_fetch \\\n", ci)


if __name__ == "__main__":
    unittest.main()
