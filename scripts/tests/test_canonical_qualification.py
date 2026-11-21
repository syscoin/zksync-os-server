import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "check_canonical_qualification", ROOT / ".github/scripts/check-canonical-qualification.py"
)
GATE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GATE)

CHECKED = "1" * 40
HEAD = "2" * 40
BASE = "3" * 40
TREE = "4" * 40
DESCRIPTOR = "5" * 64
ACCOUNT = 1234
CONTEXT = GATE.CONTEXTS["cpu"]
DESCRIPTION = f"v1 tree={TREE} descriptor={DESCRIPTOR}"


def status(sha=CHECKED, **overrides):
    return {
        "creator": {"id": ACCOUNT},
        "context": CONTEXT,
        "state": "success",
        "url": f"{GATE.API}/statuses/{sha}",
        "description": DESCRIPTION,
        **overrides,
    }


class StatusTests(unittest.TestCase):
    def verify(self, pages, candidates=(CHECKED, HEAD), context=CONTEXT):
        return GATE.verify(candidates, TREE, DESCRIPTOR, ACCOUNT, context,
                           lambda sha, page: pages.get((sha, page), []))

    def test_trusted_success_for_each_lane(self):
        for lane in GATE.CONTEXTS.values():
            self.assertEqual(self.verify({(CHECKED, 1): [status(context=lane)]}, context=lane),
                             CHECKED)

    def test_spoofed_creator_and_other_lane_are_ignored(self):
        absent = [
            status(creator={"id": ACCOUNT + 1, "login": "claimed-qualifier"}),
            status(creator={"id": str(ACCOUNT)}),
            status(creator=None),
            status(context=GATE.CONTEXTS["cuda"]),
        ]
        for row in absent:
            with self.subTest(row=row), self.assertRaisesRegex(ValueError, "missing trusted"):
                self.verify({(CHECKED, 1): [row]})

    def test_newer_untrusted_failure_cannot_revoke_trusted_success(self):
        self.assertEqual(self.verify({(CHECKED, 1): [
            status(creator={"id": ACCOUNT + 1}, state="failure"), status()
        ]}), CHECKED)

    def test_newest_trusted_status_must_match_even_with_older_success(self):
        invalid = [
            status(state="pending"),
            status(state="failure"),
            status(context=CONTEXT.upper()),
            status(sha=HEAD),
            status(description=DESCRIPTION.replace(TREE, "6" * 40)),
            status(description=DESCRIPTION.replace(DESCRIPTOR, "7" * 64)),
        ]
        for row in invalid:
            with self.subTest(row=row), self.assertRaisesRegex(ValueError, "latest trusted"):
                self.verify({(CHECKED, 1): [row, status()], (HEAD, 1): [status(sha=HEAD)]})

    def test_newest_trusted_status_can_be_on_later_page(self):
        pages = {
            (CHECKED, 1): [status(context="unrelated")] * 100,
            (CHECKED, 2): [status()],
        }
        self.assertEqual(self.verify(pages), CHECKED)

    def test_exhausted_search_fails_closed(self):
        pages = {(CHECKED, page): [status(context="unrelated")] * 100
                 for page in range(1, 11)}
        with self.assertRaisesRegex(ValueError, "search exhausted"):
            self.verify(pages)

    def test_verified_same_tree_candidate_can_supply_missing_status(self):
        pages = {(HEAD, 1): [status(sha=HEAD)]}
        self.assertEqual(self.verify(pages), HEAD)
        with self.assertRaisesRegex(ValueError, "missing trusted"):
            self.verify(pages, candidates=(CHECKED,))

    def test_configured_creator_is_required(self):
        with patch("sys.argv", ["gate", "cpu"]), patch.dict("os.environ", {}, clear=True):
            with self.assertRaisesRegex(ValueError, "approved numeric"):
                GATE.main()


class CommitTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        repo = tempfile.TemporaryDirectory()
        cls.addClassCleanup(repo.cleanup)

        def git(*args, **kwargs):
            return subprocess.check_output(["git", "-C", repo.name, *args],
                                           text=True, **kwargs).strip()

        git("init", "--quiet")
        git("config", "user.name", "Qualification Test")
        git("config", "user.email", "qualification@example.com")
        cls.base_tree = git("mktree", input="")
        blob = git("hash-object", "-w", "--stdin", input="tested bytes")
        cls.head_tree = git("mktree", input=f"100644 blob {blob}\treviewed.txt\n")
        cls.base = git("commit-tree", cls.base_tree, "-m", "base")
        cls.head = git("commit-tree", cls.head_tree, "-p", cls.base, "-m", "PR")
        cls.same = git("commit-tree", cls.head_tree, "-p", cls.base, "-p", cls.head,
                       "-m", "no composition changes")
        cls.different = git("commit-tree", cls.base_tree, "-p", cls.base, "-p", cls.head,
                            "-m", "composition changes")
        cls.git = staticmethod(git)

    def candidates(self, name, ref, event, commit):
        with patch.object(GATE, "git", self.git):
            return GATE.candidate_commits(name, ref, event, commit,
                                          self.git("rev-parse", f"{commit}^{{tree}}"))

    def test_pr_checkout_candidates_compare_git_trees(self):
        event = {"pull_request": {"head": {"sha": self.head}}}
        self.assertEqual(self.candidates("pull_request", "refs/pull/1/merge", event, self.same),
                         (self.same, self.head))
        self.assertEqual(self.candidates("pull_request", "refs/pull/1/merge", event, self.different),
                         (self.different,))

    def test_pr_checkout_rejects_wrong_event_head(self):
        event = {"pull_request": {"head": {"sha": self.base}}}
        with self.assertRaisesRegex(ValueError, "PR event's merge commit"):
            self.candidates("pull_request", "refs/pull/1/merge", event, self.same)

    def test_main_reuses_only_same_tree_second_parent_of_single_merge(self):
        event = {"before": self.base}
        self.assertEqual(self.candidates("push", "refs/heads/main", event, self.same),
                         (self.same, self.head))
        self.assertEqual(self.candidates("push", "refs/heads/main", event, self.different),
                         (self.different,))
        self.assertEqual(self.candidates("push", "refs/heads/main", event, self.head),
                         (self.head,))
        self.assertEqual(self.candidates("push", "refs/heads/main", {"before": BASE}, self.same),
                         (self.same,))

    def test_stress_push_uses_only_checked_commit(self):
        self.assertEqual(self.candidates("push", "refs/heads/ci/stress/example",
                                         {"before": self.base}, self.same), (self.same,))


if __name__ == "__main__":
    unittest.main()
